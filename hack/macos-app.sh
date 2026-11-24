#!/usr/bin/env bash
# Builds Skiff on a Mac and wraps it in an application bundle.
#
#   hack/macos-app.sh               builds target/release/Skiff.app for this Mac
#   hack/macos-app.sh --install     and copies it to /Applications
#   hack/macos-app.sh --dmg         and makes a disk image of it next to it
#   hack/macos-app.sh --zip         and makes the zip the in-app updater downloads
#   hack/macos-app.sh --universal   one app for Apple Silicon and Intel Macs
#   hack/macos-app.sh --target aarch64-apple-darwin | x86_64-apple-darwin
#   hack/macos-app.sh --build-only  only compiles (with --universal/--target)
#   hack/macos-app.sh --no-build    bundles and signs what --build-only made
#
# The last two exist for the release workflow: it compiles first, and only
# then puts the signing keys on the machine, so that no dependency's build
# script ever runs within reach of them. --build-only reads no signing
# variable; give it SKIFF_TEAM_ID if the result is going to be signed.
#
# Needs Rust (https://rustup.rs) and the Xcode command line tools
# (xcode-select --install). Nothing else.
#
# Without any of the variables below the app gets an ad-hoc signature: fine
# for the Mac it was built on, but a downloaded copy has to be allowed once
# under System Settings > Privacy & Security. The release workflow sets them
# (packaging/README.md says where each comes from):
#
#   MACOS_SIGN_IDENTITY   "Developer ID Application: Name (TEAMID)". Signs with
#                         the hardened runtime, the entitlements in
#                         packaging/macos/Skiff.entitlements and a timestamp.
#   MACOS_KEYCHAIN        optional keychain file that holds that identity
#   APPLE_API_KEY_PATH    App Store Connect API key (.p8 file), with
#   APPLE_API_KEY_ID      its key id and
#   APPLE_API_ISSUER_ID   its issuer id: notarizes and staples the app and
#                         the disk image (only together with the identity).
#   SKIFF_TEAM_ID         Apple Team ID compiled into the app, which then only
#                         accepts updates signed by that team. Taken from the
#                         identity's "(TEAMID)" when not set. With --no-build
#                         the team that --build-only compiled in is used, and
#                         must be the one the app is then signed by.
#   SKIFF_BUILD           build number (git rev-list --count HEAD); 0 locally
#   SKIFF_UPDATE_REPO     owner/repo the updater asks, when not the default
#   MACOSX_DEPLOYMENT_TARGET   oldest macOS the app runs on (default 11.0)
set -euo pipefail

install=0
dmg=0
zip=0
universal=0
compile=1
package=1
target=""
while [ $# -gt 0 ]; do
  case "$1" in
    --install) install=1 ;;
    --dmg) dmg=1 ;;
    --zip) zip=1 ;;
    --universal) universal=1 ;;
    --build-only) package=0 ;;
    --no-build) compile=0 ;;
    --target)
      shift
      target="${1:-}"
      ;;
    --target=*) target="${1#--target=}" ;;
    *)
      echo "unknown option: $1 (known: --install, --dmg, --zip, --universal, --target <triple>, --build-only, --no-build)" >&2
      exit 2
      ;;
  esac
  shift
done

if [ "$(uname -s)" != Darwin ]; then
  echo "This script is for macOS; on Linux run: hack/linux-package.sh" >&2
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
if [ "$universal" = 1 ] && [ -n "$target" ]; then
  echo "--universal and --target exclude each other" >&2
  exit 2
fi
if [ "$compile" = 0 ] && [ "$package" = 0 ]; then
  echo "--build-only and --no-build exclude each other" >&2
  exit 2
fi
if [ "$package" = 0 ] && [ "$install$dmg$zip" != 000 ]; then
  echo "--build-only makes no app: leave out --install, --dmg and --zip" >&2
  exit 2
fi
case "$target" in
  "" | aarch64-apple-darwin | x86_64-apple-darwin) ;;
  *)
    echo "unknown target: $target (known: aarch64-apple-darwin, x86_64-apple-darwin)" >&2
    exit 2
    ;;
esac

# The oldest macOS the binary may be started on. Must be exported for the
# compiler and linker, and is written into the Info.plist below.
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-11.0}"

identity="${MACOS_SIGN_IDENTITY:-}"
notarize=0
if [ "$package" = 0 ]; then
  # --build-only: nothing is signed here, so the team cannot be read from an
  # identity; it is compiled in exactly as given (or not at all).
  identity=""
  if [ -z "${SKIFF_TEAM_ID:-}" ]; then
    unset SKIFF_TEAM_ID
  elif [[ "$SKIFF_TEAM_ID" =~ ^[A-Z0-9]{10}$ ]]; then
    export SKIFF_TEAM_ID
  else
    echo "SKIFF_TEAM_ID is not an Apple Team ID (10 capital letters and digits): $SKIFF_TEAM_ID" >&2
    exit 1
  fi
elif [ -n "$identity" ]; then
  # The updater of a signed build accepts only updates signed by the same
  # team, so the Team ID has to be known at compile time.
  # (Not with --no-build: the team is compiled in already.)
  if [ "$compile" = 1 ]; then
    if [ -z "${SKIFF_TEAM_ID:-}" ]; then
      SKIFF_TEAM_ID=$(printf '%s\n' "$identity" | sed -n 's/.*(\([A-Z0-9]\{10\}\))[[:space:]]*$/\1/p')
    fi
    if [ -z "${SKIFF_TEAM_ID:-}" ]; then
      echo "MACOS_SIGN_IDENTITY does not end in \"(TEAMID)\"; set SKIFF_TEAM_ID to the Apple Team ID." >&2
      exit 1
    fi
    export SKIFF_TEAM_ID
  fi
  if [ -n "${APPLE_API_KEY_PATH:-}" ] && [ -n "${APPLE_API_KEY_ID:-}" ] && [ -n "${APPLE_API_ISSUER_ID:-}" ]; then
    if [ ! -f "$APPLE_API_KEY_PATH" ]; then
      echo "APPLE_API_KEY_PATH does not exist: $APPLE_API_KEY_PATH" >&2
      exit 1
    fi
    notarize=1
  fi
else
  # An ad-hoc build must not claim a team: its updater could then never
  # accept anything.
  unset SKIFF_TEAM_ID
fi

# ---- build ---------------------------------------------------------------

build_target() {
  if command -v rustup >/dev/null 2>&1; then
    rustup target add "$1" >/dev/null
  fi
  cargo build --release --locked -p skiff --target "$1"
}

if [ "$universal" = 1 ]; then
  binary=target/release/skiff-universal
  archs="arm64 x86_64"
  name=universal
elif [ -n "$target" ]; then
  binary="target/$target/release/skiff"
  case "$target" in
    aarch64-*) archs=arm64 ;;
    *) archs=x86_64 ;;
  esac
  name=$archs
else
  binary=target/release/skiff
  archs=$(uname -m)
  name=$archs
fi
# Next to the binary: the Team ID it was compiled with (an empty file when
# none), for a later --no-build run to check against.
team_file="$binary.team"

if [ "$compile" = 1 ]; then
  rm -f "$team_file"
  if [ "$universal" = 1 ]; then
    build_target aarch64-apple-darwin
    build_target x86_64-apple-darwin
    mkdir -p target/release
    lipo -create -output "$binary" \
      target/aarch64-apple-darwin/release/skiff target/x86_64-apple-darwin/release/skiff
  elif [ -n "$target" ]; then
    build_target "$target"
  else
    cargo build --release --locked -p skiff
  fi
  compiled_team="${SKIFF_TEAM_ID:-}"
  printf '%s\n' "$compiled_team" > "$team_file"
else
  if [ ! -f "$binary" ] || [ ! -f "$team_file" ]; then
    echo "--no-build: $binary is not there; run this with --build-only (and the same --universal/--target) first." >&2
    exit 1
  fi
  compiled_team=$(cat "$team_file")
fi

if [ "$package" = 0 ]; then
  echo "Built $binary ($archs), compiled for ${compiled_team:+team }${compiled_team:-no team (to be signed ad hoc)}"
  exit 0
fi

# What is compiled in and what signs must agree, or the updater's check of
# who signed an update is off (no team compiled in) or can never pass.
if [ -n "$identity" ] && [ -z "$compiled_team" ]; then
  echo "$binary was compiled without a Team ID but is to be signed with: $identity" >&2
  echo "Compile it with SKIFF_TEAM_ID set to that identity's team." >&2
  exit 1
fi
if [ -z "$identity" ] && [ -n "$compiled_team" ]; then
  echo "$binary was compiled for team '$compiled_team' but there is no MACOS_SIGN_IDENTITY to sign it with." >&2
  exit 1
fi

# ---- bundle --------------------------------------------------------------

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
# Set by the release workflow (git rev-list --count HEAD); 0 for a local build.
build="${SKIFF_BUILD:-0}"
app=target/release/Skiff.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp LICENSE THIRD-PARTY-NOTICES.md "$app/Contents/Resources/"
cp "$binary" "$app/Contents/MacOS/Skiff"

sed -e "s/@VERSION@/${version}/g" -e "s/@BUILD@/${build}/g" \
  -e "s/@MIN_MACOS@/${MACOSX_DEPLOYMENT_TARGET}/g" \
  packaging/macos/Info.plist.in > "$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist" >/dev/null

icon=crates/skiff/assets/icons/Skiff.icns
if [ -f "$icon" ]; then
  cp "$icon" "$app/Contents/Resources/Skiff.icns"
else
  echo "warning: $icon is missing (hack/icons.sh makes it); the app gets the generic icon" >&2
fi

# ---- sign ----------------------------------------------------------------

keychain_args=()
if [ -n "${MACOS_KEYCHAIN:-}" ]; then
  keychain_args=(--keychain "$MACOS_KEYCHAIN")
fi

if [ -n "$identity" ]; then
  # (The bundle holds one executable and no nested code, so no --deep.)
  codesign --force --options runtime --timestamp \
    --entitlements packaging/macos/Skiff.entitlements \
    ${keychain_args[@]+"${keychain_args[@]}"} --sign "$identity" "$app"
  codesign --verify --deep --strict --verbose=2 "$app"
  signed_team=$(codesign -dv --verbose=4 "$app" 2>&1 | sed -n 's/^TeamIdentifier=//p')
  if [ "$signed_team" != "$compiled_team" ]; then
    echo "The app is signed by team '$signed_team' but was compiled for team '$compiled_team'." >&2
    exit 1
  fi
  echo "Signed $app with: $identity"
else
  # An ad-hoc signature (no Apple developer account involved). Apple Silicon
  # refuses to run code that has none; this is not notarization, so a copy
  # that was downloaded still has to be allowed once on first launch.
  codesign --force --sign - "$app"
  codesign --verify --deep --strict "$app"
  echo "Signed $app ad hoc (no MACOS_SIGN_IDENTITY)"
fi

# Sends a zip or disk image to Apple's notary service and waits for the
# verdict. Fails, printing Apple's log, unless it is "Accepted".
notarize_file() {
  local out id
  out=$(xcrun notarytool submit "$1" --key "$APPLE_API_KEY_PATH" --key-id "$APPLE_API_KEY_ID" \
    --issuer "$APPLE_API_ISSUER_ID" --wait 2>&1) || {
    printf '%s\n' "$out" >&2
    return 1
  }
  printf '%s\n' "$out"
  if ! printf '%s\n' "$out" | grep -q 'status: Accepted'; then
    id=$(printf '%s\n' "$out" | sed -n 's/^ *id: //p' | head -1)
    if [ -n "$id" ]; then
      xcrun notarytool log "$id" --key "$APPLE_API_KEY_PATH" --key-id "$APPLE_API_KEY_ID" \
        --issuer "$APPLE_API_ISSUER_ID" >&2 || true
    fi
    echo "Apple did not accept $1 for notarization." >&2
    return 1
  fi
}

if [ "$notarize" = 1 ]; then
  # The service takes an archive, not a folder; the ticket it issues is
  # then stapled to the app itself, so that Gatekeeper can check it offline.
  submission=$(mktemp -d)
  ditto -c -k --keepParent "$app" "$submission/Skiff.zip"
  notarize_file "$submission/Skiff.zip"
  rm -rf "$submission"
  xcrun stapler staple "$app"
  xcrun stapler validate "$app"
  spctl --assess --type execute --verbose=2 "$app"
  echo "Notarized and stapled $app"
elif [ -n "$identity" ]; then
  echo "warning: signed but NOT notarized (APPLE_API_KEY_PATH, APPLE_API_KEY_ID, APPLE_API_ISSUER_ID are not all set)" >&2
fi

echo "Built $app ($archs)"

# ---- disk image ----------------------------------------------------------

if [ "$dmg" = 1 ]; then
  image="target/release/Skiff-macos-${name}.dmg"
  stage=$(mktemp -d)
  # (ditto, not cp -R: it keeps the signature's extended attributes and
  # the stapled ticket intact.)
  ditto "$app" "$stage/Skiff.app"
  ln -s /Applications "$stage/Applications"
  rm -f "$image"
  hdiutil create -volname Skiff -srcfolder "$stage" -ov -format UDZO "$image" >/dev/null
  rm -rf "$stage"
  if [ -n "$identity" ]; then
    codesign --force --timestamp ${keychain_args[@]+"${keychain_args[@]}"} --sign "$identity" "$image"
  fi
  if [ "$notarize" = 1 ]; then
    notarize_file "$image"
    xcrun stapler staple "$image"
    xcrun stapler validate "$image"
    spctl --assess --type open --context context:primary-signature --verbose=2 "$image"
  fi
  echo "Built $image"
fi

# ---- updater zips --------------------------------------------------------

# What the in-app updater downloads: Skiff-macos-<arch>-build-<N>.zip, where
# <arch> is the architecture of the running copy. Made after stapling (a zip
# cannot be stapled, the app inside it can). A universal app is published
# under both names. ditto keeps the signature.
if [ "$zip" = 1 ]; then
  first=""
  for arch in $archs; do
    archive="target/release/Skiff-macos-${arch}-build-${build}.zip"
    rm -f "$archive"
    if [ -z "$first" ]; then
      ditto -c -k --keepParent "$app" "$archive"
      first=$archive
    else
      cp "$first" "$archive"
    fi
    echo "Built $archive"
  done
fi

if [ "$install" = 1 ]; then
  rm -rf /Applications/Skiff.app
  ditto "$app" /Applications/Skiff.app
  echo "Installed /Applications/Skiff.app"
elif [ "$dmg" = 0 ] && [ "$zip" = 0 ]; then
  echo "Run it with:  open $app      (or copy it to /Applications)"
fi
