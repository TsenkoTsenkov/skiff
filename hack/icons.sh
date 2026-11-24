#!/usr/bin/env bash
# Regenerates every app icon from crates/skiff/assets/icons/skiff.svg.
#
#   hack/icons.sh
#
# Writes, next to the SVG (all of it is committed, so a build needs none of
# the tools below):
#   skiff-{16,32,48,64,128,256,512,1024}.png  the tile filling the canvas:
#                                             window/taskbar icon, Linux
#                                             hicolor icons, the web site
#   skiff.ico    Windows (16-256 px, same full-canvas artwork)
#   Skiff.icns   macOS: the tile inset to 824/1024 with transparent padding,
#                as Apple's icon grid asks for (the SVG as drawn)
#
# Needs rsvg-convert (librsvg2-bin / `brew install librsvg`) and python3
# with Pillow.
set -euo pipefail

dir="$(cd "$(dirname "$0")/.." && pwd)/crates/skiff/assets/icons"
svg="$dir/skiff.svg"

for tool in rsvg-convert python3; do
  command -v "$tool" >/dev/null || { echo "icons.sh: $tool is not installed" >&2; exit 1; }
done
python3 -c 'import PIL' 2>/dev/null || { echo "icons.sh: python3 needs Pillow (pip install pillow)" >&2; exit 1; }

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# The same artwork without the padding: the view box is the tile itself.
grep -q 'viewBox="0 0 1024 1024"' "$svg" || { echo "icons.sh: unexpected viewBox in $svg" >&2; exit 1; }
sed 's/viewBox="0 0 1024 1024"/viewBox="100 100 824 824"/' "$svg" > "$tmp/full.svg"

render() { # <svg> <size> <out>
  rsvg-convert -w "$2" -h "$2" "$1" -o "$3"
}

for size in 16 32 48 64 128 256 512 1024; do
  render "$tmp/full.svg" "$size" "$dir/skiff-$size.png"
done
# Sizes only the containers need. Each is rendered from the vector, never
# scaled from a bigger bitmap, so small sizes stay crisp.
render "$tmp/full.svg" 24 "$tmp/full-24.png"
for size in 16 32 64 128 256 512 1024; do
  render "$svg" "$size" "$tmp/padded-$size.png"
done

python3 - "$dir" "$tmp" <<'PY'
import sys
from PIL import Image

out, tmp = sys.argv[1], sys.argv[2]

def load(path):
    return Image.open(path).convert("RGBA")

# Windows: one .ico holding every size Explorer and the taskbar ask for.
# Stored as bitmaps, not PNG: older resource compilers and installer
# builders only read PNG entries at 256 px, if at all.
ico_sizes = [16, 24, 32, 48, 64, 128, 256]
ico = [load(f"{tmp}/full-24.png" if s == 24 else f"{out}/skiff-{s}.png") for s in ico_sizes]
ico[-1].save(f"{out}/skiff.ico", format="ICO", sizes=[(s, s) for s in ico_sizes], append_images=ico[:-1],
             bitmap_format="bmp")

# macOS: Pillow fills each slot of the .icns (16 ... 512@2x) from the
# appended image of exactly that size.
icns = [load(f"{tmp}/padded-{s}.png") for s in (16, 32, 64, 128, 256, 512, 1024)]
icns[-1].save(f"{out}/Skiff.icns", format="ICNS", append_images=icns[:-1])
PY

echo "icons written to $dir"
