#!/bin/sh
# Build a local bundle. Signing is opt-in through MARKRAFT_SIGN_IDENTITY.
set -eu
cd "$(dirname "$0")/.."
profile=debug
case "${1:-}" in
  "") cargo build --locked -p markraft-app ;;
  --release) profile=release; cargo build --locked --release -p markraft-app ;;
  *) printf '%s\n' 'Usage: scripts/bundle-app.sh [--release]' >&2; exit 2 ;;
esac
bundle=target/MarkraftNotes.app
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources" target/Markraft.iconset
cp "target/$profile/markraft-app" "$bundle/Contents/MacOS/markraft-app"
swift scripts/app-icon.swift assets/icon/Markraft.png target/Markraft.iconset
iconutil -c icns target/Markraft.iconset -o "$bundle/Contents/Resources/Markraft.icns"
cat > "$bundle/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>markraft-app</string>
<key>CFBundleIdentifier</key><string>dev.markraft.app</string>
<key>CFBundleName</key><string>Markraft Notes</string>
<key>CFBundleDisplayName</key><string>Markraft Notes</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleVersion</key><string>1</string>
<key>CFBundleShortVersionString</key><string>0.1.0</string>
<key>CFBundleIconFile</key><string>Markraft</string>
<key>LSMinimumSystemVersion</key><string>13.0</string>
<key>LSUIElement</key><true/>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
if [ -n "${MARKRAFT_SIGN_IDENTITY:-}" ]; then
  codesign --force --options runtime --timestamp --sign "$MARKRAFT_SIGN_IDENTITY" "$bundle"
  codesign --verify --strict "$bundle"
fi
plutil -lint "$bundle/Contents/Info.plist"
printf '%s\n' "Built $bundle ($profile; not installed)."
