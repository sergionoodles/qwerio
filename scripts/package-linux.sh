#!/usr/bin/env bash
# Packages a release build as an AppImage: Qwerio-<version>-x86_64.AppImage.
# The AppImage relies on the host for the graphics stack (Vulkan, Wayland,
# EGL, X11/xkbcommon), as GPU drivers cannot be bundled.
#
# Usage: scripts/package-linux.sh <path to qwerio binary> <output directory>
set -euo pipefail

readonly APPLICATION_ID="dev.qwerio.Qwerio"
readonly APPIMAGETOOL_VERSION="1.9.1"
readonly APPIMAGETOOL_SHA256="ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0"
readonly ICON_SIZES=(16 24 32 48 64 128 256 512)

if [[ $# -ne 2 ]]; then
  echo "usage: $0 <qwerio binary> <output directory>" >&2
  exit 2
fi

root="$(cd "$(dirname "$0")/.." && pwd)"
binary="$1"
output="$2"
arch="$(uname -m)"

if [[ "$arch" != "x86_64" ]]; then
  echo "AppImages are built on x86_64 only (this machine is $arch)" >&2
  exit 1
fi

version="$(cargo metadata --manifest-path "$root/Cargo.toml" --no-deps --format-version 1 \
  | jq -r '.packages[] | select(.name == "qwerio-desktop") | .version')"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

appdir="$work/Qwerio.AppDir"
share="$appdir/usr/share"
install -Dm755 "$binary" "$appdir/usr/bin/qwerio"
install -Dm644 "$root/packaging/linux/$APPLICATION_ID.desktop" "$share/applications/$APPLICATION_ID.desktop"
install -Dm644 "$root/packaging/linux/$APPLICATION_ID.metainfo.xml" "$share/metainfo/$APPLICATION_ID.appdata.xml"

for size in "${ICON_SIZES[@]}"; do
  install -Dm644 "$root/packaging/icons/qwerio-$size.png" \
    "$share/icons/hicolor/${size}x${size}/apps/$APPLICATION_ID.png"
done

# appimagetool expects the desktop entry, icon, and AppRun at the AppDir root.
cp "$share/applications/$APPLICATION_ID.desktop" "$appdir/"
cp "$root/packaging/icons/qwerio-256.png" "$appdir/$APPLICATION_ID.png"
ln -s "$APPLICATION_ID.png" "$appdir/.DirIcon"
install -Dm755 "$root/packaging/linux/AppRun" "$appdir/AppRun"

appimagetool="$work/appimagetool"
curl --fail --location --silent --show-error --output "$appimagetool" \
  "https://github.com/AppImage/appimagetool/releases/download/$APPIMAGETOOL_VERSION/appimagetool-x86_64.AppImage"
echo "$APPIMAGETOOL_SHA256  $appimagetool" | sha256sum --check --quiet
chmod +x "$appimagetool"

mkdir -p "$output"
# Extract-and-run avoids needing FUSE on build machines. appimagetool's
# AppStream check fails on warnings (no homepage yet), so it is skipped.
ARCH="$arch" APPIMAGE_EXTRACT_AND_RUN=1 "$appimagetool" --no-appstream \
  "$appdir" "$output/Qwerio-$version-$arch.AppImage"
