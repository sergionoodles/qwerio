#!/usr/bin/env bash
# Packages release builds as Qwerio.app inside Qwerio-<version>-macos-universal.dmg.
# Pass one binary per architecture; they are merged into a universal binary.
#
# The app is ad-hoc signed only: Apple Silicon refuses to run unsigned code,
# but an ad-hoc signature carries no identity, so Gatekeeper still blocks the
# downloaded app until the user allows it (System Settings → Privacy &
# Security → Open Anyway).
#
# Usage: scripts/package-macos.sh <output directory> <qwerio binary>...
set -euo pipefail

# Keep in sync with MACOSX_DEPLOYMENT_TARGET in the release workflow.
readonly MINIMUM_SYSTEM_VERSION="11.0"
readonly DMG_ATTEMPTS=3

if [[ $# -lt 2 ]]; then
  echo "usage: $0 <output directory> <qwerio binary>..." >&2
  exit 2
fi

root="$(cd "$(dirname "$0")/.." && pwd)"
output="$1"
shift

version="$(cargo metadata --manifest-path "$root/Cargo.toml" --no-deps --format-version 1 \
  | jq -r '.packages[] | select(.name == "qwerio-desktop") | .version')"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

app="$work/staging/Qwerio.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
lipo -create "$@" -output "$app/Contents/MacOS/qwerio"
sed -e "s/@VERSION@/$version/g" -e "s/@MINIMUM_SYSTEM_VERSION@/$MINIMUM_SYSTEM_VERSION/g" \
  "$root/packaging/macos/Info.plist" > "$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist"

# iconutil names each size and its @2x (double-density) variant.
iconset="$work/Qwerio.iconset"
mkdir "$iconset"
for size in 16 32 128 256 512; do
  cp "$root/packaging/icons/qwerio-$size.png" "$iconset/icon_${size}x${size}.png"
  cp "$root/packaging/icons/qwerio-$((size * 2)).png" "$iconset/icon_${size}x${size}@2x.png"
done
iconutil --convert icns --output "$app/Contents/Resources/Qwerio.icns" "$iconset"

codesign --force --sign - --timestamp=none "$app"
codesign --verify --strict "$app"

ln -s /Applications "$work/staging/Applications"
mkdir -p "$output"
dmg="$output/Qwerio-$version-macos-universal.dmg"

# hdiutil occasionally fails with "Resource busy" on CI machines.
for attempt in $(seq "$DMG_ATTEMPTS"); do
  if hdiutil create -volname Qwerio -srcfolder "$work/staging" -fs HFS+ -format UDZO -ov "$dmg"; then
    exit 0
  fi
  echo "hdiutil failed (attempt $attempt of $DMG_ATTEMPTS)" >&2
  sleep 5
done

exit 1
