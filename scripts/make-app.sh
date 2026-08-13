#!/usr/bin/env bash
# Packages the release binary as macOS Playmate.app and creates a distributable
# DMG containing an Applications shortcut.
# Usage: scripts/make-app.sh [binary path] [output directory]
# Default: scripts/make-app.sh target/release/Playmate target/bundle
set -euo pipefail
cd "$(dirname "$0")/.."

BIN="${1:-target/release/Playmate}"
OUT="${2:-target/bundle}"
[ -f "$BIN" ] || { echo "binary not found: $BIN (run cargo build --release first)" >&2; exit 1; }

VERSION=$(cargo metadata --no-deps --format-version 1 \
  | python3 -c "import json,sys; d=json.load(sys.stdin); print(next(p['version'] for p in d['packages'] if p['name']=='playmate-app'))")

APP="$OUT/Playmate.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/Playmate"
cp assets/icon/Playmate.icns "$APP/Contents/Resources/Playmate.icns"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>            <string>Playmate</string>
    <key>CFBundleDisplayName</key>     <string>Playmate</string>
    <key>CFBundleIdentifier</key>      <string>io.github.zlx2019.playmate</string>
    <key>CFBundleExecutable</key>      <string>Playmate</string>
    <key>CFBundleIconFile</key>        <string>Playmate</string>
    <key>CFBundlePackageType</key>     <string>APPL</string>
    <key>CFBundleShortVersionString</key> <string>${VERSION}</string>
    <key>CFBundleVersion</key>         <string>${VERSION}</string>
    <key>LSMinimumSystemVersion</key>  <string>11.0</string>
    <key>LSApplicationCategoryType</key> <string>public.app-category.games</string>
    <key>NSHighResolutionCapable</key> <true/>
</dict>
</plist>
PLIST

# Ad-hoc signing, equivalent to Tauri's signingIdentity = "-".
# Gatekeeper may report an unsigned bundle as damaged on Apple Silicon.
# Ad-hoc signing changes this to an unidentified-developer warning that users
# can bypass through the context menu's Open action.
codesign --force --sign - "$APP"

# Build the DMG from a staging directory containing the app and an Applications symlink.
STAGE=$(mktemp -d)
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
DMG="$OUT/Playmate-${VERSION}.dmg"
rm -f "$DMG"
hdiutil create -volname "Playmate" -srcfolder "$STAGE" -ov -format UDZO "$DMG" >/dev/null
rm -rf "$STAGE"

echo "generated: $APP"
echo "generated: $DMG"
