#!/bin/bash
# Source this helper only in stages that have access to the checkout.
if [[ "${CI_XCODE_CLOUD:-}" != "TRUE" ]]; then
  echo "Run this script only in an Xcode Cloud checkout." >&2
  exit 1
fi
if [[ "${CI_PRODUCT_PLATFORM:-}" != "macOS" ]]; then
  echo "This workflow requires the macOS platform." >&2
  exit 1
fi
if [[ ! "${CI_BUILD_NUMBER:-}" =~ ^[1-9][0-9]*$ ]]; then
  echo "Xcode Cloud must provide a positive integer CI_BUILD_NUMBER." >&2
  exit 1
fi
: "${CI_PRIMARY_REPOSITORY_PATH:?Xcode Cloud must provide the repository path}"
MARKRAFT_REPOSITORY_DIR="$(cd "$CI_PRIMARY_REPOSITORY_PATH" && pwd)"
cd "$MARKRAFT_REPOSITORY_DIR"
test -f rust-toolchain.toml
test -f Markraft.xcodeproj/project.pbxproj
python3 "$MARKRAFT_REPOSITORY_DIR/ci_scripts/verify_release_tag.py" \
  --manifest "$MARKRAFT_REPOSITORY_DIR/Cargo.toml"

# Each stage reconstructs these paths instead of relying on shell exports from
# an earlier stage. The tools and cache remain inside the disposable checkout.
export CARGO_HOME="$MARKRAFT_REPOSITORY_DIR/target/xcode-cloud/tools/cargo"
export RUSTUP_HOME="$MARKRAFT_REPOSITORY_DIR/target/xcode-cloud/tools/rustup"
export CARGO_TARGET_DIR="$MARKRAFT_REPOSITORY_DIR/target"
export PATH="$CARGO_HOME/bin:$PATH"
export RUSTFLAGS="${RUSTFLAGS:-} -C strip=none"
