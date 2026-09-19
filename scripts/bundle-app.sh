#!/bin/bash
# cargo-bundle creates the app; this wrapper supplies Sparkle and release metadata.
set -euo pipefail
cd "$(dirname "$0")/.."
export CARGO_TARGET_DIR="$PWD/target"
export SPARKLE_FRAMEWORK_PATH="$PWD/target/sparkle"
export MACOSX_DEPLOYMENT_TARGET=13.0
profile=debug
universal=false
mock_updates=false
for arg in "$@"; do
  case "$arg" in
    --release) profile=release ;;
    --universal) universal=true ;;
    --mock-updates) mock_updates=true ;;
    *) echo 'Usage: scripts/bundle-app.sh [--release] [--universal] [--mock-updates]' >&2; exit 2 ;;
  esac
done
if [[ "$(cargo bundle --version)" != 'cargo-bundle v0.11.0' ]]; then
  echo 'Install cargo-bundle: cargo install cargo-bundle --version 0.11.0 --locked' >&2
  exit 1
fi
bash scripts/download-sparkle.sh
swift scripts/app-icon.swift assets/icon/Markraft.png target/Markraft.iconset
iconutil -c icns target/Markraft.iconset -o target/Markraft.icns
build_args=(--profile dev)
[[ "$profile" == debug ]] || build_args=(--release)
if "$mock_updates"; then
  build_args+=(--features updater-mock)
fi
if "$universal"; then
  for arch in aarch64 x86_64; do
    cargo build --locked -p markraft-app "${build_args[@]}" --target "$arch-apple-darwin"
  done
  CARGO_BUNDLE_SKIP_BUILD=1 cargo bundle -p markraft-app --format osx "${build_args[@]}" --target aarch64-apple-darwin
  bundle="target/universal-apple-darwin/$profile/bundle/osx/Markraft Notes.app"
  rm -rf "$bundle"
  ditto "target/aarch64-apple-darwin/$profile/bundle/osx/Markraft Notes.app" "$bundle"
  lipo -create "target/aarch64-apple-darwin/$profile/markraft-app" \
    "target/x86_64-apple-darwin/$profile/markraft-app" -output "$bundle/Contents/MacOS/markraft-app"
else
  cargo build --locked -p markraft-app "${build_args[@]}"
  CARGO_BUNDLE_SKIP_BUILD=1 cargo bundle -p markraft-app --format osx "${build_args[@]}"
  bundle="target/$profile/bundle/osx/Markraft Notes.app"
fi
if "$mock_updates"; then
  python3 scripts/configure-bundle.py "$bundle/Contents/Info.plist" --mock-updates
else
  python3 scripts/configure-bundle.py "$bundle/Contents/Info.plist"
fi
bash scripts/sign-app.sh "$bundle" "${MARKRAFT_SIGN_IDENTITY:--}"
plutil -lint "$bundle/Contents/Info.plist"
printf 'Built %s (not installed).\n' "$bundle"
