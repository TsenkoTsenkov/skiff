#!/usr/bin/env bash
# Records and renders the demo animation at the top of the README and the site.
#
#   hack/cluster-up.sh             # once: the test clusters and the fake Prometheus
#   hack/demo/render.sh            # record the frames, render, copy to docs/demo/ and site/img/
#   hack/demo/render.sh --render   # render again from the frames already in target/demo/frames/
#
# See hack/demo/README.md.
set -euo pipefail

repo="$(cd "$(dirname "$0")/../.." && pwd)"
kubeconfig="${SKIFF_KUBECONFIG:-$repo/hack/kubeconfig}"

if [ "${1:-}" != "--render" ]; then
  if ! KUBECONFIG="$kubeconfig" kubectl --context demo get --raw /readyz >/dev/null 2>&1; then
    echo "The demo cluster is not answering. Start it with hack/cluster-up.sh." >&2
    exit 1
  fi
  (cd "$repo" && SKIFF_TEST_KUBECONFIG="$kubeconfig" cargo test -p skiff --test demo -- --ignored --nocapture)
fi

python3 "$repo/hack/demo/render.py" --frames "$repo/target/demo/frames" --out "$repo/target/demo" --sheet "$repo/target/demo/contact-sheet.png"

mkdir -p "$repo/docs/demo" "$repo/site/img"
for f in skiff-demo.gif skiff-demo.mp4; do
  cp "$repo/target/demo/$f" "$repo/docs/demo/$f"
  cp "$repo/target/demo/$f" "$repo/site/img/$f"
done
echo "copied to docs/demo/ and site/img/; look at target/demo/contact-sheet.png before committing"
