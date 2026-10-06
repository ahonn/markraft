#!/bin/bash
set -euo pipefail

SCRIPT_DIRECTORY="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIRECTORY/cloud_environment.sh"
bash "$SCRIPT_DIRECTORY/install_rust.sh"
python3 scripts/prepare-dependencies.py

xcodebuild -version
git rev-parse HEAD
cargo fmt --all -- --check
python3 -m unittest discover -s "$SCRIPT_DIRECTORY/tests" -v
# Check every workspace crate with the store channel selected. Do not download
# Sparkle just to run the direct-distribution gate in a store-only workflow.
cargo clippy --workspace --all-targets --locked \
  --no-default-features --features markraft-app/mac-app-store -- -D warnings
# GPUI 0.3.6 test teardown retains visual contexts and their file handles.
# Cloud shells can start at 256, even when the hard limit permits more.
MARKRAFT_TEST_FILE_LIMIT="$(ulimit -Sn)"
if [[ "$MARKRAFT_TEST_FILE_LIMIT" != "unlimited" && "$MARKRAFT_TEST_FILE_LIMIT" -lt 4096 ]]; then
  ulimit -Sn 4096 || {
    echo "GUI tests require an open-file soft limit of at least 4096." >&2
    exit 1
  }
fi
echo "Running tests with 4 threads; open-file soft limit: $(ulimit -Sn)"
cargo test --workspace --locked \
  --no-default-features --features markraft-app/mac-app-store -- --test-threads=4
