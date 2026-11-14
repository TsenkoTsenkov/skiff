# Helpers for the seed scripts (sourced after ../lib.sh).
apply_dir_parallel() {   # ctx dir [jobs]  - server-side apply every yaml file in dir, in parallel
  local ctx="$1" dir="$2" jobs="${3:-6}"
  ls "$dir"/*.yaml | xargs -P "$jobs" -I{} sh -c \
    'kubectl --kubeconfig "$1" --context "$2" apply --server-side --force-conflicts --field-manager=skiff-seed -f "$3" >/dev/null || { echo "apply failed: $3" >&2; exit 255; }' _ "$KUBECONFIG" "$ctx" {}
}
apply_stages() {   # ctx
  kc "$1" apply -f "$HACK_DIR/seed/kwok/" >/dev/null
}
wait_for_nodes_ready() {  # ctx expected [timeout]
  local ctx="$1" want="$2" t="${3:-180}" i n
  for i in $(seq 1 "$t"); do
    n=$(kc "$ctx" get nodes --no-headers 2>/dev/null | awk '$2!="Unknown"' | wc -l)
    [ "$n" -ge "$want" ] && return 0; sleep 1
  done
  log "WARN: only $n/$want nodes Ready after ${t}s"
}
