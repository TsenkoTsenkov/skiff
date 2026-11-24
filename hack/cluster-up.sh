#!/usr/bin/env bash
# Brings up (or re-starts) both kwok clusters, seeds them, and starts the helper daemons.
# Idempotent: safe to re-run; existing clusters are reused, seeds are applied declaratively.
#   ./cluster-up.sh            everything
#   SKIP_SCALE=1 ./cluster-up.sh   demo only
#   RESEED=1 ./cluster-up.sh   force re-apply of the scale seed
set -euo pipefail
source "$(dirname "$0")/lib.sh"

ensure_tools

# helper daemons the demo cluster's simulated logs / port-forward depend on
daemon_start logsim "$HACK_DIR/logsim.sh"
daemon_start pf-target python3 -I "$HACK_DIR/pf-target.py" "$PF_TARGET_PORT"
prom_start   # fake Prometheus: metrics charts and firing alerts for the demo cluster
sleep 1   # logsim writes its initial history before the ClusterLogs are applied

create_cluster demo "$DEMO_PORT"
"$HACK_DIR/seed/demo.sh"

if [ "${SKIP_SCALE:-0}" != 1 ]; then
  create_cluster scale "$SCALE_PORT" --disable-qps-limits --extra-args kube-apiserver=service-cluster-ip-range=10.96.0.0/12
  FORCE="${RESEED:-0}" "$HACK_DIR/seed/scale.sh"
fi

"$HACK_DIR/churn.sh" start

kubectl --kubeconfig "$KUBECONFIG" config use-context demo >/dev/null
cat >&2 <<MSG

Skiff test bed is up.
  export KUBECONFIG=$KUBECONFIG
  contexts: demo  (https://127.0.0.1:$DEMO_PORT)   scale (https://127.0.0.1:$SCALE_PORT)
  try:      kubectl --context demo get pods -A     |  ./status.sh
MSG
