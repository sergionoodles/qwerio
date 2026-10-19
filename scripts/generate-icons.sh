#!/usr/bin/env bash
# Resizes the artwork in resources/ for Linux, macOS, and Windows packages.
# Small icons use the favicon's dark background to keep the white mark visible.
#
# Requires: ImageMagick 6 (convert) or 7 (magick).
#
# Usage: scripts/generate-icons.sh
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
icons="$root/packaging/icons"
sizes=(16 24 32 48 64 128 256 512 1024)

if command -v magick >/dev/null; then
  imagemagick=magick
elif command -v convert >/dev/null; then
  imagemagick=convert
else
  echo "missing required tool: ImageMagick (magick or convert)" >&2
  exit 1
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$icons"

# Remove the source's large transparent margins, then give each icon a
# consistent safe area instead of enlarging the original empty canvas.
"$imagemagick" "$root/resources/logo_2048.png" -trim +repage "$work/logo.png"

for size in "${sizes[@]}"; do
  if (( size <= 48 )); then
    "$imagemagick" "$root/resources/favicon.png" -filter Lanczos -resize "${size}x${size}" \
      -strip "PNG32:$icons/qwerio-$size.png"
    continue
  fi

  content_size=$((size * 4 / 5))
  "$imagemagick" "$work/logo.png" -filter Lanczos -resize "${content_size}x${content_size}" \
    -gravity center -background none -extent "${size}x${size}" \
    -strip "PNG32:$icons/qwerio-$size.png"
done

"$imagemagick" "$icons/qwerio-16.png" "$icons/qwerio-24.png" \
  "$icons/qwerio-32.png" "$icons/qwerio-48.png" "$icons/qwerio-64.png" \
  "$icons/qwerio-128.png" "$icons/qwerio-256.png" "$icons/qwerio.ico"
