#!/usr/bin/env bash
# Regenerates the screenshots used by the README, the docs and the site.
#
# The UI tests (crates/skiff/tests/ui.rs) drive the real app, headless,
# against the `demo` test cluster and write what they draw to
# target/shots/. This script runs them and copies a chosen few to
# docs/img/ and site/img/ under stable names.
#
#   hack/cluster-up.sh          # once: the test clusters and the fake Prometheus
#   hack/screenshots.sh         # run the tests, then copy
#   hack/screenshots.sh --copy  # only copy what is already in target/shots/
#
# Needs a Vulkan driver (mesa-vulkan-drivers on a machine without a GPU).
# The copies are reduced to a 256-colour palette (about a quarter of the
# size, no visible difference on flat UI) with pngquant, or with python3
# and Pillow where that is not installed; with neither they are copied as
# they are.
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
shots="$repo/target/shots"
kubeconfig="${SKIFF_KUBECONFIG:-$repo/hack/kubeconfig}"

# test screenshot (target/shots/<name>.png) -> published name
picks=(
  "88_overview_alerts:overview"
  "89_overview_alerts_light:overview-light"
  "03_pod_detail:pod-details"
  "80_pod_metrics:metrics"
  "13_logs_wrapped_timestamps:logs"
  "28_shell:shell"
  "06_palette:palette"
  "50_settings_appearance:settings"
)

# The site also shows details of some screenshots, so that the text in
# them stays readable next to a paragraph:
#   test screenshot -> site/img/<name>.png, cut to left,top,right,bottom
crops=(
  "03_pod_detail:crop-pod-details:560,44,1440,684"
  "80_pod_metrics:crop-metrics:600,44,1440,884"
  "13_logs_wrapped_timestamps:crop-logs:214,432,916,900"
  "28_shell:crop-shell:214,432,916,900"
  "06_palette:crop-palette:214,60,1044,613"
  "50_settings_appearance:crop-settings:331,151,1109,670"
)

# Writes $1 to $2 with a 256-colour palette.
shrink() {
  if command -v pngquant >/dev/null 2>&1; then
    pngquant --force --strip --quality 70-95 --output "$2" -- "$1" && return 0
  fi
  if python3 -c 'import PIL' >/dev/null 2>&1; then
    python3 - "$1" "$2" <<'PY' && return 0
import sys
from PIL import Image
image = Image.open(sys.argv[1]).convert("RGB")
# (The k-means pass keeps small coloured details, such as a red icon.)
image = image.quantize(colors=256, method=Image.Quantize.MEDIANCUT, kmeans=2, dither=Image.Dither.NONE)
image.save(sys.argv[2], "PNG", optimize=True)
PY
  fi
  cp "$1" "$2"
}

if [ "${1:-}" != "--copy" ]; then
  if ! KUBECONFIG="$kubeconfig" kubectl --context demo get --raw /readyz >/dev/null 2>&1; then
    echo "The demo cluster is not answering. Start it with hack/cluster-up.sh." >&2
    exit 1
  fi
  # (The tests skip themselves when there is no cluster, which would leave
  # old screenshots in place unnoticed: hence the check above.)
  (cd "$repo" && SKIFF_TEST_KUBECONFIG="$kubeconfig" cargo test -p skiff --test ui)
fi

mkdir -p "$repo/docs/img" "$repo/site/img"
for pick in "${picks[@]}"; do
  from="$shots/${pick%%:*}.png"
  name="${pick##*:}.png"
  if [ ! -f "$from" ]; then
    echo "missing $from (did the UI tests run against the demo cluster?)" >&2
    exit 1
  fi
  shrink "$from" "$repo/docs/img/$name"
  cp "$repo/docs/img/$name" "$repo/site/img/$name"
  printf '%-22s <- %s (%s bytes)\n' "$name" "${pick%%:*}" "$(wc -c <"$repo/docs/img/$name")"
done

if ! python3 -c 'import PIL' >/dev/null 2>&1; then
  echo "python3 with Pillow is needed to cut the details shown on the site." >&2
  exit 1
fi
for crop in "${crops[@]}"; do
  IFS=: read -r from name box <<<"$crop"
  if [ ! -f "$shots/$from.png" ]; then
    echo "missing $shots/$from.png" >&2
    exit 1
  fi
  python3 - "$shots/$from.png" "$repo/site/img/$name.png" "$box" <<'PY'
import sys
from PIL import Image
box = tuple(int(n) for n in sys.argv[3].split(","))
Image.open(sys.argv[1]).convert("RGB").crop(box).save(sys.argv[2], "PNG")
PY
  shrink "$repo/site/img/$name.png" "$repo/site/img/$name.png.tmp"
  mv "$repo/site/img/$name.png.tmp" "$repo/site/img/$name.png"
  printf '%-22s <- %s, cut to %s (%s bytes)\n' "$name.png" "$from" "$box" "$(wc -c <"$repo/site/img/$name.png")"
done

# The site shows the app icon too (full-canvas PNGs made by hack/icons.sh).
cp "$repo/crates/skiff/assets/icons/skiff-64.png" "$repo/site/img/icon-64.png"
cp "$repo/crates/skiff/assets/icons/skiff-256.png" "$repo/site/img/icon-256.png"
