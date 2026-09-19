#!/bin/bash
# Build distributable artifacts locally or in CI; publication is a separate step.
set -euo pipefail
cd "$(dirname "$0")/.."
: "${MARKRAFT_SIGN_IDENTITY:?Set a Developer ID Application signing identity}"
: "${SPARKLE_PUBLIC_KEY:?Set the public Ed25519 key}"
: "${SPARKLE_PRIVATE_KEY_FILE:?Set the path to the exported Sparkle private key}"
: "${APPLE_ID:?Set the notarization Apple ID}"
: "${APPLE_TEAM_ID:?Set the notarization team ID}"
: "${APPLE_APP_SPECIFIC_PASSWORD:?Set the notarization app-specific password}"
if [[ "$MARKRAFT_SIGN_IDENTITY" != 'Developer ID Application:'* ]]; then
  echo 'Distribution requires a Developer ID Application identity.' >&2; exit 1
fi
version=$(cargo metadata --no-deps --format-version 1 --locked | python3 -c \
  'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "markraft-app"))')
if [[ "${1:-}" != "v$version" ]]; then
  echo "Release tag must equal v$version (the Cargo workspace version)." >&2; exit 1
fi
bash scripts/bundle-app.sh --release --universal
app='target/universal-apple-darwin/release/bundle/osx/Markraft Notes.app'
mkdir -p target/release-artifacts
archive="target/release-artifacts/Markraft-$version-universal.zip"
notarization_archive="target/release-artifacts/notarization.zip"
ditto -c -k --sequesterRsrc --keepParent "$app" "$notarization_archive"
xcrun notarytool submit "$notarization_archive" --wait \
  --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" --password "$APPLE_APP_SPECIFIC_PASSWORD"
xcrun stapler staple "$app"
xcrun stapler validate "$app"
codesign --verify --deep --strict "$app"
spctl --assess --type execute --verbose "$app"
# Stapling changes the bundle. Only sign the final, re-created archive for Sparkle.
ditto -c -k --sequesterRsrc --keepParent "$app" "$archive"
signature=$(target/sparkle/sparkle-bin/sign_update --ed-key-file "$SPARKLE_PRIVATE_KEY_FILE" -p "$archive")
swift scripts/verify-update.swift "$archive" "$SPARKLE_PUBLIC_KEY" "$signature"
python3 scripts/appcast.py --archive "$archive" --version "$version" \
  --repository ahonn/markraft --signature "$signature" --output target/release-artifacts/appcast.xml
(cd target/release-artifacts && shasum -a 256 "Markraft-$version-universal.zip" appcast.xml > SHA256SUMS)
printf 'Release artifacts ready in target/release-artifacts for %s.\n' "$1"
