#!/bin/sh
# Build pmux and install it as /Applications/pmux.app (Spotlight, Launchpad, Dock).
# Usage: tools/install-app.sh [dest-dir]   (default /Applications)
set -eu
cd "$(dirname "$0")/.."
DEST=${1:-/Applications}
APP="$DEST/pmux.app"
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

cargo build --release

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

# Icon: rendered only when tools/icon.swift changes (cached in target/), into an iconset packed as .icns.
# The file name carries the hash, so macOS's icon cache can't keep showing a previous icon.
HASH=$(shasum tools/icon.swift | cut -c1-8)
ICNS="target/pmux-icon-$HASH.icns"
if [ ! -f "$ICNS" ]; then
  mkdir "$TMP/pmux.iconset"
  swift tools/icon.swift "$TMP/icon.png" 1024
  for sz in 16 32 128 256 512; do
    sips -z $sz $sz "$TMP/icon.png" --out "$TMP/pmux.iconset/icon_${sz}x${sz}.png" >/dev/null
    sips -z $((sz * 2)) $((sz * 2)) "$TMP/icon.png" --out "$TMP/pmux.iconset/icon_${sz}x${sz}@2x.png" >/dev/null
  done
  iconutil -c icns "$TMP/pmux.iconset" -o "$ICNS"
fi

B="$TMP/pmux.app/Contents"
mkdir -p "$B/MacOS" "$B/Resources"
cp target/release/pmux "$B/MacOS/pmux"
cp "$ICNS" "$B/Resources/pmux-$HASH.icns"
cat > "$B/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>pmux</string>
  <key>CFBundleDisplayName</key><string>pmux</string>
  <key>CFBundleIdentifier</key><string>dev.pmux.app</string>
  <key>CFBundleExecutable</key><string>pmux</string>
  <key>CFBundleIconFile</key><string>pmux-$HASH</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
EOF
codesign --force --sign - "$TMP/pmux.app" 2>/dev/null

# Swap in place; a running pmux keeps its (unlinked) old binary until it exits.
rm -rf "$APP"
mv "$TMP/pmux.app" "$APP"
touch "$APP"
/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f "$APP"
mdimport "$APP" 2>/dev/null || true
echo "installed $APP"
