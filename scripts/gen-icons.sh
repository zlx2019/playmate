#!/usr/bin/env bash
# Composes a native-style macOS icon plate around assets/logo.png
# (a 512x512 controller with a transparent background), then generates all
# application icon formats: icns, ico, and png.
# The plate follows Apple's icon conventions: a 1024 canvas, a centered
# 824x824 rounded square with system margins, and a subtle drop shadow.
# Requirements: magick (ImageMagick) and iconutil (included with macOS).
# Generated icons are committed; rerun this script only after changing the logo.
set -euo pipefail
cd "$(dirname "$0")/.."

SRC=assets/logo.png
OUT=assets/icon

for tool in magick iconutil; do
  command -v "$tool" >/dev/null || { echo "missing required tool: $tool" >&2; exit 1; }
done

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

# --- Compose the 1024 master with its icon plate ---
# 1. Shadow: a blurred black rounded rectangle matching the plate and offset downward.
magick -size 1024x1024 xc:none -fill 'rgba(0,0,0,0.35)' \
  -draw "roundrectangle 100,114,923,937,186,186" -blur 0x14 "$TMP/shadow.png"
# 2. Plate: a vertical white gradient for contrast with the dark controller,
#    masked to an 824x824 rounded square with an approximately 22.5% corner radius.
magick -size 824x824 "gradient:#FFFFFF-#EDEDF0" "$TMP/grad.png"
magick -size 824x824 xc:none -fill white \
  -draw "roundrectangle 0,0,823,823,186,186" "$TMP/mask.png"
magick "$TMP/grad.png" "$TMP/mask.png" -alpha set -compose DstIn -composite "$TMP/plate.png"
# 3. Stack the shadow, centered plate, and centered controller scaled to 70% of the plate width.
# A neutral-gray plate may be saved by magick as grayscale PNG, stripping color
# from later composites. Force truecolor-alpha output and explicitly use sRGB.
magick "$TMP/shadow.png" "$TMP/plate.png" -geometry +100+100 -composite \
  -define png:color-type=6 "$TMP/base.png"
magick "$SRC" -resize 580x580 "$TMP/pad.png"
magick "$TMP/base.png" -colorspace sRGB "$TMP/pad.png" -gravity center -composite \
  -define png:color-type=6 "$TMP/composed.png"

# --- macOS .icns with 1x and 2x iconset sizes ---
ICONSET="$TMP/Playmate.iconset"
mkdir -p "$ICONSET"
for size in 16 32 128 256 512; do
  magick "$TMP/composed.png" -resize "${size}x${size}" "$ICONSET/icon_${size}x${size}.png"
  double=$((size * 2))
  magick "$TMP/composed.png" -resize "${double}x${double}" "$ICONSET/icon_${size}x${size}@2x.png"
done
iconutil -c icns "$ICONSET" -o "$OUT/Playmate.icns"

# --- Windows .ico containing multiple PNG sizes with the same icon plate ---
for size in 16 24 32 48 64 128 256; do
  magick "$TMP/composed.png" -resize "${size}x${size}" "$TMP/ico_$size.png"
done
magick "$TMP/ico_256.png" "$TMP/ico_128.png" "$TMP/ico_64.png" "$TMP/ico_48.png" \
  "$TMP/ico_32.png" "$TMP/ico_24.png" "$TMP/ico_16.png" "$OUT/Playmate.ico"

# --- Linux PNG for the .desktop entry ---
magick "$TMP/composed.png" -resize 256x256 "$OUT/playmate-256.png"

echo "icons generated in $OUT/"
