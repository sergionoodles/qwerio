#!/usr/bin/env bash
# Builds the official logo and the Linux and macOS app icons from
# resources/logo_2048.png: the white/orange mark on a Tokyo Night tile, so the
# icon stays readable on light and dark desktops alike.
#
# Outputs:
#   resources/logo_tile_2048.png  marketing master
#   resources/favicon.png         256px copy for the README and the web
#   packaging/icons/qwerio-*.png  packaged app icons
#
# Requires: ImageMagick 6 (convert) or 7 (magick).
#
# Usage: scripts/generate-icons.sh
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
icons="$root/packaging/icons"
sizes=(16 24 32 48 64 128 256 512 1024)

tile_color="#1A1B26"
tile_size=2048
# Matches the platform icon grids: a 1/16 transparent margin around a tile
# with ~22% corner radius, and the mark filling ~55% of the canvas.
tile_inset=128
tile_radius=400
mark_size=1120

if command -v magick >/dev/null; then
  imagemagick=magick
elif command -v convert >/dev/null; then
  imagemagick=convert
else
  echo "missing required tool: ImageMagick (magick or convert)" >&2
  exit 1
fi

mkdir -p "$icons"

tile_end=$((tile_size - tile_inset - 1))
"$imagemagick" -size "${tile_size}x${tile_size}" xc:none \
  -fill "$tile_color" \
  -draw "roundrectangle $tile_inset,$tile_inset $tile_end,$tile_end $tile_radius,$tile_radius" \
  \( "$root/resources/logo_2048.png" -trim +repage -filter Lanczos \
     -resize "${mark_size}x${mark_size}" \) \
  -gravity center -composite \
  -strip "PNG32:$root/resources/logo_tile_2048.png"

"$imagemagick" "$root/resources/logo_tile_2048.png" -filter Lanczos -resize 256x256 \
  -strip "PNG32:$root/resources/favicon.png"

for size in "${sizes[@]}"; do
  "$imagemagick" "$root/resources/logo_tile_2048.png" -filter Lanczos -resize "${size}x${size}" \
    -strip "PNG32:$icons/qwerio-$size.png"
done
