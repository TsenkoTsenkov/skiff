#!/usr/bin/env bash
# Keeps the demo cluster changing so watches/live views have something to show.
#   ./churn.sh start|stop|status|restart|run   (run = foreground)
# Every CHURN_INTERVAL seconds (default 3, +/- jitter) it does one random thing: scale a deployment, delete a pod,
# emit a Warning/Normal event, rollout-restart a deployment, run a short Job, edit a ConfigMap, flip a Database CR phase.
source "$(dirname "$0")/lib.sh"
CTX="${CHURN_CONTEXT:-demo}"
INTERVAL="${CHURN_INTERVAL:-3}"
K() { kc "$CTX" "$@"; }

# "namespace/deployment min max" - none of these are HPA-managed, so churn and HPA do not fight.
SCALABLE=("shop/cart 1 6" "payments/notifier 1 5" "team-a/api-dev 1 6" "team-b/demo-app 1 6"
          "data/schema-registry 1 4" "platform/ingress-nginx-controller 1 5" "monitoring/grafana 1 4" "kube-system/coredns 1 4")
POD_NS=(shop payments monitoring data platform team-a team-b batch incidents)
DB_CRS=("shop/orders-db" "shop/catalog-db" "data/events-store" "payments/ledger-db")
PHASES=(Running Running Running Degraded Provisioning Updating)
REASONS=("Warning BackOff Back-off restarting failed container" "Warning Unhealthy Readiness probe failed: connection refused"
         "Warning FailedMount Unable to attach or mount volumes: timed out waiting for the condition" "Normal Pulled Container image already present on machine"
         "Warning NodePressure Node under memory pressure" "Normal Scheduled Successfully assigned pod to node" "Warning FailedScheduling 0/10 nodes are available: insufficient cpu")
pick() { local a=("$@"); echo "${a[RANDOM % ${#a[@]}]}"; }

act_scale() {
  read -r target mn mx <<<"$(pick "${SCALABLE[@]}")"
  local ns="${target%/*}" d="${target#*/}" cur new
  cur=$(K -n "$ns" get deploy "$d" -o jsonpath='{.spec.replicas}' 2>/dev/null) || return
  new=$(( cur + (RANDOM % 2 ? 1 : -1) * (RANDOM % 2 + 1) )); [ $new -lt $mn ] && new=$((mn + 1)); [ $new -gt $mx ] && new=$((mx - 1))
  K -n "$ns" scale deploy "$d" --replicas="$new" >/dev/null 2>&1 && echo "scale $target $cur -> $new"
}
act_delete_pod() {
  local ns; ns=$(pick "${POD_NS[@]}")
  local p; p=$(K -n "$ns" get pods --no-headers -o custom-columns=:metadata.name --field-selector=status.phase=Running 2>/dev/null | shuf -n1)
  [ -n "$p" ] && K -n "$ns" delete pod "$p" --wait=false >/dev/null 2>&1 && echo "delete pod $ns/$p"
}
act_event() {
  local ns; ns=$(pick shop payments monitoring data team-a incidents)
  local p; p=$(K -n "$ns" get pods --no-headers -o custom-columns=:metadata.name 2>/dev/null | shuf -n1); [ -z "$p" ] && return
  read -r type reason msg <<<"$(pick "${REASONS[@]}")"
  local now; now=$(date -u +%FT%TZ)
  K create -f - >/dev/null 2>&1 <<YAML && echo "event $type $reason on $ns/$p"
apiVersion: v1
kind: Event
metadata: {generateName: churn-, namespace: $ns}
type: $type
reason: $reason
message: "$msg (churn)"
involvedObject: {kind: Pod, name: $p, namespace: $ns, apiVersion: v1}
source: {component: skiff-churn}
count: 1
firstTimestamp: "$now"
lastTimestamp: "$now"
YAML
}
act_restart() {
  read -r target _ <<<"$(pick "${SCALABLE[@]}")"
  K -n "${target%/*}" rollout restart deploy "${target#*/}" >/dev/null 2>&1 && echo "rollout restart $target"
}
act_job() {
  local n="churn-job-$(date +%s)"
  K -n batch create job "$n" --image=busybox:1.36 -- sh -c 'echo hi' >/dev/null 2>&1 && echo "create job $n"
  # keep only the 5 newest churn jobs
  K -n batch get jobs --no-headers -o custom-columns=:metadata.name --sort-by=.metadata.creationTimestamp 2>/dev/null | grep '^churn-job-' | head -n -5 | xargs -r -n1 -I{} kubectl --kubeconfig "$KUBECONFIG" --context "$CTX" -n batch delete job {} --wait=false >/dev/null 2>&1
}
act_configmap() {
  K -n shop patch cm feature-flags --type merge -p "{\"data\":{\"last-churn\":\"$(date -u +%T)\",\"rollout-percent\":\"$((RANDOM % 100))\"}}" >/dev/null 2>&1 && echo "patch cm shop/feature-flags"
}
act_db() {
  local t; t=$(pick "${DB_CRS[@]}"); local ph; ph=$(pick "${PHASES[@]}")
  local ready=false; [ "$ph" = Running ] && ready=true
  K -n "${t%/*}" patch databases.stack.skiff.dev "${t#*/}" --type merge -p "{\"status\":{\"phase\":\"$ph\",\"ready\":$ready}}" >/dev/null 2>&1 && echo "patch database $t -> $ph"
}

run_loop() {
  trap 'exit 0' TERM INT
  echo "$(date -u +%T) churn started on context $CTX, interval ${INTERVAL}s"
  while true; do
    r=$((RANDOM % 100))
    if   [ $r -lt 30 ]; then out=$(act_scale)
    elif [ $r -lt 50 ]; then out=$(act_delete_pod)
    elif [ $r -lt 70 ]; then out=$(act_event)
    elif [ $r -lt 78 ]; then out=$(act_restart)
    elif [ $r -lt 86 ]; then out=$(act_job)
    elif [ $r -lt 94 ]; then out=$(act_configmap)
    else out=$(act_db); fi
    [ -n "$out" ] && echo "$(date -u +%T) $out"
    sleep $(( INTERVAL + RANDOM % 3 ))
  done
}

case "${1:-status}" in
  run) run_loop ;;
  start) daemon_start churn "$0" run ;;
  stop) daemon_stop churn ;;
  restart) daemon_stop churn; sleep 1; daemon_start churn "$0" run ;;
  status) if daemon_running churn; then echo "churn running (pid $(cat "$PID_DIR/churn.pid")); tail $STATE_DIR/churn.out"; tail -n 8 "$STATE_DIR/churn.out" 2>/dev/null; else echo "churn not running"; exit 1; fi ;;
  *) echo "usage: $0 start|stop|status|restart|run"; exit 2 ;;
esac
