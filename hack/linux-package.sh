#!/usr/bin/env bash
# Builds Skiff for Linux and packages it three ways, into dist/:
#
#   skiff-linux-<arch>.tar.gz     bin/skiff plus the desktop entry and icons;
#                                 unpack into ~/.local or /usr/local with
#                                 tar -xzf ... --strip-components=1 -C ~/.local
#   skiff_<amd64|arm64>.deb       Debian/Ubuntu package (plain dpkg-deb)
#   Skiff-linux-<arch>.AppImage   one file that runs on any distribution
#
#   hack/linux-package.sh                      build for this machine, package all
#   hack/linux-package.sh --target <triple>    x86_64- or aarch64-unknown-linux-gnu
#   hack/linux-package.sh --binary <file>      package this skiff binary, do not build
#   hack/linux-package.sh --no-appimage        skip the AppImage
#   hack/linux-package.sh --no-deb             skip the .deb
#   hack/linux-package.sh --out <dir>          write there instead of dist/
#
# Needs Rust, tar and gzip; dpkg-deb for the .deb; for the AppImage
# appimagetool, which is downloaded (pinned release, checked against the
# SHA-256 below) into target/tools/ unless APPIMAGETOOL names one or it is
# on PATH, and the AppImage runtime (the small program at the front of every
# AppImage, which starts it), downloaded and checked the same way unless
# APPIMAGE_RUNTIME names one. No GPU, Wayland or X11 libraries are put into any package: the
# program loads the system's at run time, and bundled copies would not match
# the machine's drivers.
#
# Environment:
#   SKIFF_BUILD        build number (git rev-list --count HEAD); 0 locally
#   SKIFF_UPDATE_REPO  owner/repo the update check asks, when not the default
#   APPIMAGETOOL       path of an appimagetool to use
#   APPIMAGE_RUNTIME   path of an AppImage type-2 runtime to embed instead of
#                      the pinned one
#   SOURCE_DATE_EPOCH  timestamp for the files in the packages (default: the
#                      last commit's)
set -euo pipefail

app_id=io.github.tsenkotsenkov.skiff
appimagetool_version=1.9.1
appimagetool_sha256() {
  case "$1" in
    x86_64) echo ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0 ;;
    aarch64) echo f0837e7448a0c1e4e650a93bb3e85802546e60654ef287576f46c71c126a9158 ;;
  esac
}
# The runtime is the first code that runs when someone starts the AppImage.
# appimagetool would otherwise fetch whatever the "continuous" release of
# AppImage/type2-runtime holds that day, unchecked; this is a dated release
# of it.
appimage_runtime_version=20251108
appimage_runtime_sha256() {
  case "$1" in
    x86_64) echo 2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d ;;
    aarch64) echo 00cbdfcf917cc6c0ff6d3347d59e0ca1f7f45a6df1a428a0d6d8a78664d87444 ;;
  esac
}

# fetch_pinned <url> <sha256> <file>: downloads <url> to <file> unless the
# file is already there with that SHA-256; fails, leaving nothing behind,
# when the download does not have it.
fetch_pinned() {
  local url=$1 want=$2 file=$3
  if [ -f "$file" ] && echo "$want  $file" | sha256sum -c --status; then
    return 0
  fi
  mkdir -p "$(dirname "$file")"
  echo "Downloading $url"
  curl -fsSL --retry 3 -o "$file.part" "$url"
  if ! echo "$want  $file.part" | sha256sum -c --status; then
    echo "The download of $url does not have the expected SHA-256 ($want)." >&2
    rm -f "$file.part"
    return 1
  fi
  mv "$file.part" "$file"
}

target=""
binary=""
out=dist
appimage=1
deb=1
while [ $# -gt 0 ]; do
  case "$1" in
    --target)
      shift
      target="${1:-}"
      ;;
    --target=*) target="${1#--target=}" ;;
    --binary)
      shift
      binary="${1:-}"
      ;;
    --binary=*) binary="${1#--binary=}" ;;
    --out)
      shift
      out="${1:-}"
      ;;
    --out=*) out="${1#--out=}" ;;
    --no-appimage) appimage=0 ;;
    --no-deb) deb=0 ;;
    *)
      echo "unknown option: $1 (known: --target <triple>, --binary <file>, --out <dir>, --no-appimage, --no-deb)" >&2
      exit 2
      ;;
  esac
  shift
done

if [ "$(uname -s)" != Linux ]; then
  echo "This script is for Linux; on a Mac run: hack/macos-app.sh" >&2
  exit 1
fi
if [ -n "$binary" ]; then
  binary=$(realpath "$binary")
fi
cd "$(dirname "$0")/.."

host_arch=$(uname -m)
case "$target" in
  "") arch=$host_arch ;;
  x86_64-unknown-linux-gnu) arch=x86_64 ;;
  aarch64-unknown-linux-gnu) arch=aarch64 ;;
  *)
    echo "unknown target: $target (known: x86_64-unknown-linux-gnu, aarch64-unknown-linux-gnu)" >&2
    exit 2
    ;;
esac
case "$arch" in
  x86_64) deb_arch=amd64 ;;
  aarch64) deb_arch=arm64 ;;
  *)
    echo "no packages are made for $arch (only x86_64 and aarch64)" >&2
    exit 1
    ;;
esac

# ---- build ---------------------------------------------------------------

if [ -z "$binary" ]; then
  if [ -n "$target" ]; then
    cargo build --release --locked -p skiff --target "$target"
    binary="$PWD/target/$target/release/skiff"
  else
    cargo build --release --locked -p skiff
    binary="$PWD/target/release/skiff"
  fi
fi
if [ ! -f "$binary" ]; then
  echo "no such binary: $binary" >&2
  exit 1
fi

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
build="${SKIFF_BUILD:-0}"
if [ -z "${SOURCE_DATE_EPOCH:-}" ]; then
  SOURCE_DATE_EPOCH=$(git log -1 --format=%ct 2>/dev/null || date +%s)
fi
export SOURCE_DATE_EPOCH
release_date=$(date -u -d "@$SOURCE_DATE_EPOCH" +%Y-%m-%d)

mkdir -p "$out"
out=$(realpath "$out")
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# ---- the file tree every package shares -----------------------------------

# Fills <root> (a prefix such as /usr) with bin/skiff, the desktop entry,
# the AppStream metadata and the icons.
populate() {
  local root=$1 size
  install -Dm755 "$binary" "$root/bin/skiff"
  install -Dm644 "packaging/linux/$app_id.desktop" "$root/share/applications/$app_id.desktop"
  mkdir -p "$root/share/metainfo"
  sed "s|^@RELEASES@\$|    <release version=\"$version\" date=\"$release_date\"/>|" \
    "packaging/linux/$app_id.metainfo.xml" > "$root/share/metainfo/$app_id.metainfo.xml"
  chmod 644 "$root/share/metainfo/$app_id.metainfo.xml"
  for size in 16 32 48 64 128 256 512; do
    if [ -f "crates/skiff/assets/icons/skiff-$size.png" ]; then
      install -Dm644 "crates/skiff/assets/icons/skiff-$size.png" \
        "$root/share/icons/hicolor/${size}x${size}/apps/$app_id.png"
    else
      echo "warning: crates/skiff/assets/icons/skiff-$size.png is missing (hack/icons.sh makes it)" >&2
    fi
  done
  if [ -f crates/skiff/assets/icons/skiff.svg ]; then
    install -Dm644 crates/skiff/assets/icons/skiff.svg \
      "$root/share/icons/hicolor/scalable/apps/$app_id.svg"
  fi
}

# The binary must start, when this machine can run it at all.
if [ "$arch" = "$host_arch" ]; then
  "$binary" --version
fi
if command -v desktop-file-validate >/dev/null 2>&1; then
  desktop-file-validate "packaging/linux/$app_id.desktop"
fi

# ---- tar.gz --------------------------------------------------------------

tree="$work/tar/skiff-linux-$arch"
populate "$tree"
install -Dm644 LICENSE "$tree/share/doc/skiff/LICENSE"
install -Dm644 THIRD-PARTY-NOTICES.md "$tree/share/doc/skiff/THIRD-PARTY-NOTICES.md"
tarball="$out/skiff-linux-$arch.tar.gz"
# (Sorted, with fixed owner and time, so that the same source gives the same
# archive.)
tar -C "$work/tar" --sort=name --owner=0 --group=0 --numeric-owner \
  --mtime="@$SOURCE_DATE_EPOCH" -cf - "skiff-linux-$arch" | gzip -9n > "$tarball"
echo "Built $tarball"

# ---- .deb ----------------------------------------------------------------

if [ "$deb" = 1 ]; then
  if ! command -v dpkg-deb >/dev/null 2>&1; then
    echo "dpkg-deb is not installed; use --no-deb to skip the .deb" >&2
    exit 1
  fi
  pkg="$work/deb"
  populate "$pkg/usr"
  # Debian wants the licence as /usr/share/doc/<package>/copyright.
  mkdir -p "$pkg/usr/share/doc/skiff"
  {
    echo "Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/"
    echo "Upstream-Name: Skiff"
    echo "Source: https://github.com/TsenkoTsenkov/skiff"
    echo
    echo "Files: *"
    echo "Copyright: Skiff contributors"
    echo "License: Expat"
    sed -e '1,/^$/d' -e 's/^$/./' -e 's/^/ /' LICENSE
  } > "$pkg/usr/share/doc/skiff/copyright"
  chmod 644 "$pkg/usr/share/doc/skiff/copyright"

  # The oldest glibc the binary can run on: the newest symbol version it
  # asks for.
  glibc=""
  if command -v objdump >/dev/null 2>&1; then
    glibc=$(objdump -T "$binary" 2>/dev/null | grep -o 'GLIBC_[0-9][0-9.]*' | sed 's/GLIBC_//' | sort -Vu | tail -1)
  fi
  libc_dep="libc6"
  if [ -n "$glibc" ]; then
    libc_dep="libc6 (>= $glibc)"
  fi

  installed_kb=$(du -sk "$pkg/usr" | cut -f1)
  mkdir -p "$pkg/DEBIAN"
  # The build number is the Debian revision: it only ever grows, so every
  # newer build upgrades an older one whatever its channel.
  cat > "$pkg/DEBIAN/control" <<CONTROL
Package: skiff
Version: $version-$build
Architecture: $deb_arch
Maintainer: Tsenko Tsenkov <tsenkotsenkov@users.noreply.github.com>
Installed-Size: $installed_kb
Depends: $libc_dep, libgcc-s1, libxkbcommon0
Recommends: libvulkan1, libwayland-client0, libwayland-cursor0, libwayland-egl1, libx11-6, libx11-xcb1, libxcursor1, libxi6, libxkbcommon-x11-0, libegl1, libfontconfig1
Suggests: mesa-vulkan-drivers, kubectl
Section: utils
Priority: optional
Homepage: https://github.com/TsenkoTsenkov/skiff
Description: fast, native Kubernetes cluster browser
 Skiff is a desktop client for Kubernetes clusters: browse and edit
 resources, follow logs, open shells in containers and forward ports.
 It uses the kubeconfig as it is, including exec credential plugins.
 .
 It needs a Vulkan or OpenGL driver; without a GPU, install
 mesa-vulkan-drivers.
CONTROL
  find "$pkg" -exec touch -h -d "@$SOURCE_DATE_EPOCH" {} +
  debfile="$out/skiff_$deb_arch.deb"
  rm -f "$debfile"
  # (xz rather than the newer default zstd: older dpkg versions read it.)
  dpkg-deb --root-owner-group -Zxz --build "$pkg" "$debfile" >/dev/null
  dpkg-deb --info "$debfile" >/dev/null
  echo "Built $debfile"
fi

# ---- AppImage ------------------------------------------------------------

if [ "$appimage" = 1 ]; then
  if [ "$arch" != "$host_arch" ]; then
    echo "The AppImage is made on a machine of its own architecture ($arch); this is $host_arch. Use --no-appimage." >&2
    exit 1
  fi
  tool="${APPIMAGETOOL:-}"
  if [ -z "$tool" ] && command -v appimagetool >/dev/null 2>&1; then
    tool=$(command -v appimagetool)
  fi
  if [ -z "$tool" ]; then
    tool="$PWD/target/tools/appimagetool-$appimagetool_version-$arch.AppImage"
    fetch_pinned "https://github.com/AppImage/appimagetool/releases/download/$appimagetool_version/appimagetool-$arch.AppImage" \
      "$(appimagetool_sha256 "$arch")" "$tool"
    chmod +x "$tool"
  fi
  # Always handed to appimagetool, so that it never downloads one itself.
  runtime="${APPIMAGE_RUNTIME:-}"
  if [ -z "$runtime" ]; then
    runtime="$PWD/target/tools/appimage-runtime-$appimage_runtime_version-$arch"
    fetch_pinned "https://github.com/AppImage/type2-runtime/releases/download/$appimage_runtime_version/runtime-$arch" \
      "$(appimage_runtime_sha256 "$arch")" "$runtime"
  elif [ ! -f "$runtime" ]; then
    echo "APPIMAGE_RUNTIME does not exist: $runtime" >&2
    exit 1
  fi

  appdir="$work/Skiff.AppDir"
  populate "$appdir/usr"
  # What the AppImage format asks for at the top of the directory: the
  # program to start, the desktop entry and the icon.
  ln -s usr/bin/skiff "$appdir/AppRun"
  ln -s "usr/share/applications/$app_id.desktop" "$appdir/$app_id.desktop"
  ln -s "usr/share/icons/hicolor/256x256/apps/$app_id.png" "$appdir/$app_id.png"
  ln -s "usr/share/icons/hicolor/256x256/apps/$app_id.png" "$appdir/.DirIcon"

  image="$out/Skiff-linux-$arch.AppImage"
  rm -f "$image"
  # (--appimage-extract-and-run: appimagetool is an AppImage itself, and
  # build machines and containers have no FUSE to mount it with.)
  ARCH=$arch "$tool" --appimage-extract-and-run --no-appstream \
    --runtime-file "$runtime" "$appdir" "$image"
  chmod +x "$image"
  # The image must begin with the runtime chosen above. (Up to 16 bytes
  # differ: appimagetool writes the MD5 of the payload into the runtime's
  # .digest_md5 section.)
  differing=$(cmp -l -n "$(stat -c %s "$runtime")" "$runtime" "$image" | wc -l || true)
  if [ "$differing" -gt 16 ]; then
    echo "$image does not start with the AppImage runtime $runtime." >&2
    exit 1
  fi
  # The metadata is checked here rather than by appimagetool, whose check
  # depends on which appstream tools happen to be installed.
  if command -v appstreamcli >/dev/null 2>&1; then
    appstreamcli validate --no-net "$appdir/usr/share/metainfo/$app_id.metainfo.xml" ||
      echo "warning: appstreamcli has remarks about the metainfo file (above)" >&2
  fi
  "$image" --appimage-extract-and-run --version
  echo "Built $image"
fi
