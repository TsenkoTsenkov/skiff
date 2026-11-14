#!/usr/bin/env bash
# Builds Skiff on a Mac and wraps it in an application bundle.
#
#   hack/macos-app.sh             builds target/release/Skiff.app
#   hack/macos-app.sh --install   and copies it to /Applications
#
# Needs Rust (https://rustup.rs) and the Xcode command line tools
# (xcode-select --install). Nothing else.
set -euo pipefail

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
app=target/release/Skiff.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/skiff "$app/Contents/MacOS/skiff"

# An app started from the Finder or the Dock does not get the shell's PATH,
# and the credential helpers a kubeconfig calls (aws, gke-gcloud-auth-plugin,
# kubelogin, ...) usually live only there. A login shell knows it.
cat > "$app/Contents/MacOS/Skiff" <<'LAUNCHER'
#!/bin/zsh -l
export PATH="$PATH:/opt/homebrew/bin:/usr/local/bin"
exec "${0:A:h}/skiff" "$@"
LAUNCHER
chmod +x "$app/Contents/MacOS/Skiff"

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
  <key>CFBundleVersion</key><string>${version}</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
</dict>
</plist>
PLIST

echo "Built $app"
if [ "${1:-}" = "--install" ]; then
  rm -rf /Applications/Skiff.app
  cp -R "$app" /Applications/Skiff.app
  echo "Installed /Applications/Skiff.app"
else
  echo "Run it with:  open $app      (or copy it to /Applications)"
fi
