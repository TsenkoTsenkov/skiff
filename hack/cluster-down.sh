#!/usr/bin/env bash
# Stops the helper daemons and both clusters.
#   ./cluster-down.sh            stop everything, keep data (cluster-up.sh restarts them with data intact)
#   ./cluster-down.sh --delete   also delete the clusters and all seeded state (fresh start next time)
source "$(dirname "$0")/lib.sh"
"$HACK_DIR/churn.sh" stop
daemon_stop logsim
daemon_stop pf-target
for c in demo scale; do
  if cluster_exists "$c"; then
    if [ "${1:-}" = "--delete" ]; then
      log "deleting cluster $c"; kwokctl_q delete cluster --name "$c"
    else
      log "stopping cluster $c"; kwokctl_q stop cluster --name "$c"
    fi
  fi
done
if [ "${1:-}" = "--delete" ]; then rm -f "$KUBECONFIG"; rm -rf "$STATE_DIR/simlogs" "$STATE_DIR/scale-seed" "$STATE_DIR"/*.out "$STATE_DIR"/0*.yaml; fi
