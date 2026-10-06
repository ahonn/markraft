#!/bin/bash
set -euo pipefail

cd "${SRCROOT:?Run this script from the Markraft Xcode target}"
if [[ "${CI_XCODE_CLOUD:-}" == "TRUE" ]]; then
  source "$SRCROOT/ci_scripts/cloud_environment.sh"
else
  export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
  # The Xcode 27 strip tool can corrupt Rust proc-macro libraries. Preserve the
  # existing caller flags and disable stripping for the host build of xtask too.
  export RUSTFLAGS="${RUSTFLAGS:-} -C strip=none"
fi
export CARGO_TARGET_DIR="$SRCROOT/target"
python3 scripts/prepare-dependencies.py
cargo xtask xcode-build
