#!/usr/bin/env bash
# Shared helpers for the Skiff local test bed. Source this file; do not run it.
set -o pipefail

HACK_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export HACK_DIR
export KWOK_WORKDIR="${KWOK_WORKDIR:-$HACK_DIR/.state/kwok}"   # kwokctl cluster data (etcd, pki, logs, pids)
export KUBECONFIG="${SKIFF_KUBECONFIG:-$HACK_DIR/kubeconfig}"
BIN_DIR="$HACK_DIR/bin"
STATE_DIR="$HACK_DIR/.state"
PID_DIR="$STATE_DIR/pids"
SIM_DIR="$STATE_DIR/simlogs"
mkdir -p "$BIN_DIR" "$PID_DIR" "$SIM_DIR" "$KWOK_WORKDIR"

K8S_VERSION="${K8S_VERSION:-v1.37.2}"
ETCD_VERSION="${ETCD_VERSION:-v3.5.21}"
KWOK_VERSION="${KWOK_VERSION:-v0.8.0}"
METRICS_SERVER_VERSION="${METRICS_SERVER_VERSION:-v0.8.1}"

DEMO_PORT="${DEMO_PORT:-18080}"
SCALE_PORT="${SCALE_PORT:-18081}"
PF_TARGET_PORT="${PF_TARGET_PORT:-18090}"   # tiny HTTP server that fake pods port-forward to
PROM_PORT="${PROM_PORT:-18091}"             # fake Prometheus behind the demo cluster's monitoring/prometheus-operated Service

export PATH="/usr/local/bin:$BIN_DIR:$PATH"

log() { printf '\033[1;34m[skiff]\033[0m %s\n' "$*" >&2; }
die() { printf '\033[1;31m[skiff] ERROR:\033[0m %s\n' "$*" >&2; exit 1; }

# Quiet kwokctl (it logs JSON to stderr); show only errors.
kwokctl_q() {
  local f rc; f="$(mktemp)"
  kwokctl "$@" 2>"$f" </dev/null; rc=$?
  grep -E '"level":"(ERROR|WARN)"' "$f" >&2 || true
  [ $rc -ne 0 ] && tail -n 5 "$f" >&2
  rm -f "$f"; return $rc
}

ensure_tools() {
  local url
  if ! command -v kwokctl >/dev/null || ! command -v kwok >/dev/null; then
    log "go install kwok/kwokctl $KWOK_VERSION -> /usr/local/bin (via proxy.golang.org)"
    GOBIN=/usr/local/bin go install "sigs.k8s.io/kwok/cmd/kwokctl@$KWOK_VERSION" "sigs.k8s.io/kwok/cmd/kwok@$KWOK_VERSION" || die "go install kwok failed"
  fi
  if ! command -v kubectl >/dev/null; then
    log "downloading kubectl $K8S_VERSION"
    curl -fsSL -o /usr/local/bin/kubectl "https://dl.k8s.io/release/$K8S_VERSION/bin/linux/amd64/kubectl" && chmod +x /usr/local/bin/kubectl || die "kubectl download failed"
  fi
  local b
  for b in kube-apiserver kube-controller-manager kube-scheduler; do
    [ -x "$BIN_DIR/$b" ] && continue
    log "downloading $b $K8S_VERSION"
    curl -fsSL -o "$BIN_DIR/$b.tmp" "https://dl.k8s.io/release/$K8S_VERSION/bin/linux/amd64/$b" && chmod +x "$BIN_DIR/$b.tmp" && mv "$BIN_DIR/$b.tmp" "$BIN_DIR/$b" || die "$b download failed"
  done
  if [ ! -x "$BIN_DIR/etcd" ]; then
    log "downloading etcd $ETCD_VERSION (storage.googleapis.com)"
    curl -fsSL "https://storage.googleapis.com/etcd/$ETCD_VERSION/etcd-$ETCD_VERSION-linux-amd64.tar.gz" \
      | tar xz -C "$BIN_DIR" --strip-components=1 "etcd-$ETCD_VERSION-linux-amd64/etcd" || die "etcd download failed"
  fi
  if [ ! -x "$BIN_DIR/metrics-server" ]; then
    log "go install metrics-server $METRICS_SERVER_VERSION -> $BIN_DIR"
    GOBIN="$BIN_DIR" go install "sigs.k8s.io/metrics-server/cmd/metrics-server@$METRICS_SERVER_VERSION" || log "WARN: metrics-server build failed; metrics.k8s.io will be unavailable"
  fi
}

# kwokctl's cluster names are "demo"/"scale"; its kubeconfig context is kwok-<name>. We rename to <name>.
cluster_exists() { [ -d "$KWOK_WORKDIR/clusters/$1" ]; }
cluster_running() { kubectl --kubeconfig "$KUBECONFIG" --context "$1" --request-timeout=3s get --raw /readyz >/dev/null 2>&1; }

kc() { local ctx="$1"; shift; kubectl --kubeconfig "$KUBECONFIG" --context "$ctx" "$@"; }

create_cluster() {   # name port [extra kwokctl args...]
  local name="$1" port="$2"; shift 2
  if cluster_exists "$name"; then
    if cluster_running "$name"; then log "cluster $name already running"; return 0; fi
    log "starting existing cluster $name"
    kwokctl_q start cluster --name "$name" --wait 180s || die "start $name failed"
  else
    log "creating cluster $name (apiserver port $port)"
    local ms=()
    [ -x "$BIN_DIR/metrics-server" ] && ms=(--enable metrics-server --metrics-server-binary "$BIN_DIR/metrics-server")
    kwokctl_q create cluster --name "$name" --runtime binary --kubeconfig "$KUBECONFIG" \
      --kube-apiserver-binary "$BIN_DIR/kube-apiserver" \
      --kube-controller-manager-binary "$BIN_DIR/kube-controller-manager" \
      --kube-scheduler-binary "$BIN_DIR/kube-scheduler" \
      --etcd-binary "$BIN_DIR/etcd" \
      --kwok-controller-binary /usr/local/bin/kwok \
      --kube-apiserver-port "$port" "${ms[@]}" \
      --enable-crds Stage,Logs,ClusterLogs,Exec,ClusterExec,PortForward,ClusterPortForward,Attach,ClusterAttach,ResourceUsage,ClusterResourceUsage,Metric \
      --wait 180s "$@" || die "create $name failed"
  fi
  # export kubeconfig again (start does not rewrite it) and rename the context
  if ! kubectl --kubeconfig "$KUBECONFIG" config get-contexts "$name" >/dev/null 2>&1; then
    kubectl --kubeconfig "$KUBECONFIG" config rename-context "kwok-$name" "$name" >/dev/null 2>&1 || true
  fi
  local i
  for i in $(seq 1 60); do cluster_running "$name" && return 0; sleep 1; done
  die "cluster $name did not become ready"
}

# ---- tiny process supervisor for helper daemons (logsim, port-forward target, churn) ----
daemon_running() { local f="$PID_DIR/$1.pid"; [ -f "$f" ] && kill -0 "$(cat "$f")" 2>/dev/null; }
daemon_start() {   # name cmd...
  local name="$1"; shift
  if daemon_running "$name"; then log "$name already running (pid $(cat "$PID_DIR/$name.pid"))"; return 0; fi
  nohup setsid "$@" >>"$STATE_DIR/$name.out" 2>&1 < /dev/null &
  echo $! > "$PID_DIR/$name.pid"
  log "started $name (pid $!)"
}
daemon_stop() {
  local name="$1" f="$PID_DIR/$1.pid"
  if [ -f "$f" ]; then
    local pid; pid="$(cat "$f")"
    if kill -0 "$pid" 2>/dev/null; then
      pkill -TERM -P "$pid" 2>/dev/null || true
      kill -TERM "$pid" 2>/dev/null || true
      log "stopped $name (pid $pid)"
    fi
    rm -f "$f"
  fi
}

# ---- fake Prometheus for the demo cluster (charts and alerts in Skiff) ----
# The API server proxies to it through a selector-less Service, and refuses
# loopback addresses in Endpoints, so it listens on this host's own address.
prom_address() { python3 -I "$HACK_DIR/fake-prometheus.py" --print-address 2>/dev/null; }
prom_start() {
  local ip; ip="$(prom_address)"
  if [ -z "$ip" ]; then log "WARN: this host has no non-loopback IPv4 address; the demo cluster gets no Prometheus"; return 0; fi
  # Restart when the host's address changed since it was started.
  [ "$(cat "$STATE_DIR/fake-prometheus.addr" 2>/dev/null)" = "$ip" ] || daemon_stop fake-prometheus
  echo "$ip" > "$STATE_DIR/fake-prometheus.addr"
  daemon_start fake-prometheus python3 -I "$HACK_DIR/fake-prometheus.py" "$PROM_PORT" --bind "$ip" --kubeconfig "$KUBECONFIG" --context demo
}
# Points monitoring/prometheus-operated at the fake Prometheus (idempotent).
prom_seed() {   # ctx
  local ip now status; ip="$(cat "$STATE_DIR/fake-prometheus.addr" 2>/dev/null)"
  [ -n "$ip" ] || return 0
  # (kubectl warns that v1 Endpoints is deprecated; the service proxy still reads it.)
  sed -e "s|@PROM_IP@|$ip|g" -e "s|@PROM_PORT@|$PROM_PORT|g" "$HACK_DIR/seed/demo/96-prometheus.yaml.tpl" \
    | kc "$1" apply -f - 2>&1 >/dev/null | grep -v 'v1 Endpoints is deprecated' >&2
  # The proxy only forwards to a Pod whose status carries the endpoint's IP.
  now="$(date -u +%FT%TZ)"
  status='{"status":{"phase":"Running","podIP":"'$ip'","podIPs":[{"ip":"'$ip'"}],"hostIP":"'$ip'","startTime":"'$now'",
    "conditions":[{"type":"Ready","status":"True","lastTransitionTime":"'$now'"},{"type":"ContainersReady","status":"True","lastTransitionTime":"'$now'"}],
    "containerStatuses":[{"name":"prometheus","ready":true,"started":true,"restartCount":0,"image":"skiff.dev/hack/fake-prometheus:local","imageID":"","state":{"running":{"startedAt":"'$now'"}}}]}}'
  if [ "$(kc "$1" -n monitoring get pod fake-prometheus -o jsonpath='{.status.podIP}' 2>/dev/null)" != "$ip" ]; then
    kc "$1" -n monitoring patch pod fake-prometheus --subresource=status --type=merge -p "$status" >/dev/null \
      || log "WARN: could not set the fake Prometheus pod's status; charts will say Prometheus is not answering"
  fi
}
