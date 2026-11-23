#!/usr/bin/env bash
# Quick health + counts for both clusters and helper daemons.
source "$(dirname "$0")/lib.sh"
for d in logsim pf-target fake-prometheus churn; do daemon_running $d && echo "daemon $d: running (pid $(cat "$PID_DIR/$d.pid"))" || echo "daemon $d: NOT running"; done
for c in demo scale; do
  if cluster_running $c; then
    echo "== $c: $(kc $c config view --minify -o jsonpath='{.clusters[0].cluster.server}')"
    printf '  nodes=%s (Ready=%s) pods=%s (Running=%s) namespaces=%s deployments=%s configmaps=%s secrets=%s services=%s\n' \
      "$(kc $c get nodes --no-headers | wc -l)" "$(kc $c get nodes --no-headers | grep -c ' Ready')" \
      "$(kc $c get pods -A --no-headers | wc -l)" "$(kc $c get pods -A --no-headers --field-selector=status.phase=Running | wc -l)" \
      "$(kc $c get ns --no-headers | wc -l)" "$(kc $c get deploy -A --no-headers | wc -l)" \
      "$(kc $c get cm -A --no-headers | wc -l)" "$(kc $c get secrets -A --no-headers | wc -l)" "$(kc $c get svc -A --no-headers | wc -l)"
  else echo "== $c: NOT running"; fi
done
