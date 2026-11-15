#!/usr/bin/env bash
# Builds Skiff on a Mac and wraps it in an application bundle.
#
#   hack/macos-app.sh             builds target/release/Skiff.app
#   hack/macos-app.sh --install   and copies it to /Applications
#   hack/macos-app.sh --dmg       and makes a disk image of it next to it
#
# Needs Rust (https://rustup.rs) and the Xcode command line tools
# (xcode-select --install). Nothing else.
set -euo pipefail

install=0
dmg=0
for arg in "$@"; do
  case "$arg" in
    --install) install=1 ;;
    --dmg) dmg=1 ;;
    *) echo "unknown option: $arg (known: --install, --dmg)" >&2; exit 2 ;;
  esac
done

if [ "$(uname -s)" != Darwin ]; then
  echo "This script is for macOS; on Linux run: cargo build --release" >&2
  exit 1
fi
cd "$(dirname "$0")/.."
if ! command -v cargo >/dev/null 2>&1; then
  echo "Rust is not installed. Install it from https://rustup.rs and run this again." >&2
  exit 1
fi
if ! xcode-select -p >/dev/null 2>&1; then
  echo "The Xcode command line tools are missing. Run: xcode-select --install" >&2
  exit 1
fi

cargo build --release

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
# Set by the release workflow (its run number); 0 for a local build.
build="${SKIFF_BUILD:-0}"
app=target/release/Skiff.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/skiff "$app/Contents/MacOS/Skiff"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Skiff</string>
  <key>CFBundleDisplayName</key><string>Skiff</string>
  <key>CFBundleIdentifier</key><string>dev.skiff.Skiff</string>
  <key>CFBundleExecutable</key><string>Skiff</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${version}</string>
  <key>CFBundleVersion</key><string>${build}</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
</dict>
</plist>
PLIST

# An ad-hoc signature (no Apple developer account involved). Apple Silicon
# refuses to run code that has none; this is not notarization, so a copy
# that was downloaded still has to be allowed once on first launch.
codesign --force --deep --sign - "$app" || echo "warning: could not sign $app" >&2

echo "Built $app"

if [ "$dmg" = 1 ]; then
  image="target/release/Skiff-${version}-$(uname -m).dmg"
  stage=$(mktemp -d)
  cp -R "$app" "$stage/Skiff.app"
  ln -s /Applications "$stage/Applications"
  rm -f "$image"
  hdiutil create -volname Skiff -srcfolder "$stage" -ov -format UDZO "$image" >/dev/null
  rm -rf "$stage"
  echo "Built $image"
fi

if [ "$install" = 1 ]; then
  rm -rf /Applications/Skiff.app
  cp -R "$app" /Applications/Skiff.app
  echo "Installed /Applications/Skiff.app"
elif [ "$dmg" = 0 ]; then
  echo "Run it with:  open $app      (or copy it to /Applications)"
fi
