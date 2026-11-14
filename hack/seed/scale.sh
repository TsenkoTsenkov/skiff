#!/usr/bin/env bash
# Seeds the "scale" cluster: 100 nodes, 20 namespaces, 320 Deployments (~5.4k pods), 3000 each ConfigMaps/Secrets/Services.
# Skipped when already seeded (marker ConfigMap) unless FORCE=1.
source "$(dirname "$0")/../lib.sh"; source "$HACK_DIR/seed/common.sh"
CTX=scale
S="$HACK_DIR/seed"
if [ "${FORCE:-0}" != 1 ] && kc $CTX -n kube-system get cm skiff-seeded >/dev/null 2>&1; then log "scale: already seeded (FORCE=1 to re-apply)"; exit 0; fi
log "scale: kwok stages"
python3 -I "$S/kwok/gen-sim-stages.py" > "$S/kwok/40-sim-pod-states.yaml"
apply_stages $CTX
GEN="$STATE_DIR/scale-seed"; rm -rf "$GEN"
python3 -I "$S/scale_gen.py" "$GEN"
log "scale: nodes (100)"
kc $CTX apply --server-side --force-conflicts --field-manager=skiff-seed -f "$GEN/00-nodes.yaml" >/dev/null
kc $CTX apply -f "$GEN/01-namespaces.yaml" >/dev/null
wait_for_nodes_ready $CTX 98 240      # nodes 5 and 6 are cordoned but still Ready
log "scale: ~9.5k objects + 320 deployments (parallel server-side apply)"
mv "$GEN/00-nodes.yaml" "$GEN/01-namespaces.yaml" "$STATE_DIR/" 2>/dev/null
apply_dir_parallel $CTX "$GEN" 4 || die "scale seeding failed"
kc $CTX -n kube-system create configmap skiff-seeded --from-literal=at="$(date -u +%FT%TZ)" >/dev/null
log "scale: seeded (pods will keep converging to Running for a minute or two)"
