#!/bin/bash
# Sign nested code inside-out. Preserve Sparkle's XPC sandbox entitlements.
set -euo pipefail
app="${1:?Usage: sign-app.sh APP IDENTITY}"
identity="${2:?Specify a Developer ID identity, or - for local ad-hoc signing}"
framework="$app/Contents/Frameworks/Sparkle.framework"
options=(--force --sign "$identity")
if [[ "$identity" != - ]]; then
  options+=(--options runtime --timestamp)
fi
codesign "${options[@]}" --preserve-metadata=entitlements "$framework/Versions/B/XPCServices/Downloader.xpc"
codesign "${options[@]}" --preserve-metadata=entitlements "$framework/Versions/B/XPCServices/Installer.xpc"
codesign "${options[@]}" "$framework/Versions/B/Autoupdate"
codesign "${options[@]}" "$framework/Versions/B/Updater.app"
codesign "${options[@]}" "$framework"
codesign "${options[@]}" "$app"
codesign --verify --deep --strict "$app"
