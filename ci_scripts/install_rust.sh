#!/bin/bash
set -euo pipefail

if [[ "${CI_XCODE_CLOUD:-}" != "TRUE" ]]; then
  echo "Run this installer only in an Xcode Cloud checkout." >&2
  exit 1
fi

: "${CARGO_HOME:?Source cloud_environment.sh first}"
: "${RUSTUP_HOME:?Source cloud_environment.sh first}"
RUST_TOOLCHAIN="$(sed -nE 's/^channel = "([0-9]+\.[0-9]+\.[0-9]+)"$/\1/p' rust-toolchain.toml)"
if [[ ! "$RUST_TOOLCHAIN" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "rust-toolchain.toml must pin one stable Rust version." >&2
  exit 1
fi

if [[ ! -x "$CARGO_HOME/bin/rustup" ]]; then
  RUSTUP_VERSION=1.28.2
  case "$(uname -m)" in
    arm64)
      RUSTUP_HOST=aarch64-apple-darwin
      RUSTUP_SHA256=20ef5516c31b1ac2290084199ba77dbbcaa1406c45c1d978ca68558ef5964ef5
      ;;
    x86_64)
      RUSTUP_HOST=x86_64-apple-darwin
      RUSTUP_SHA256=9c331076f62b4d0edeae63d9d1c9442d5fe39b37b05025ec8d41c5ed35486496
      ;;
    *) echo "Unsupported macOS build host." >&2; exit 1 ;;
  esac
  INSTALL_DIRECTORY="$(mktemp -d)"
  trap 'rm -rf "$INSTALL_DIRECTORY"' EXIT
  curl --proto '=https' --tlsv1.2 --fail --silent --show-error --location --retry 3 \
    "https://static.rust-lang.org/rustup/archive/$RUSTUP_VERSION/$RUSTUP_HOST/rustup-init" \
    -o "$INSTALL_DIRECTORY/rustup-init"
  printf '%s  %s\n' "$RUSTUP_SHA256" "$INSTALL_DIRECTORY/rustup-init" | shasum -a 256 -c -
  chmod +x "$INSTALL_DIRECTORY/rustup-init"
  "$INSTALL_DIRECTORY/rustup-init" -y --no-modify-path --default-toolchain none --profile minimal
fi

# Pin the compiler separately from the installer. Repeat this in pre-build so
# a different stage environment can recover missing tools or targets.
"$CARGO_HOME/bin/rustup" set auto-self-update disable
"$CARGO_HOME/bin/rustup" toolchain install "$RUST_TOOLCHAIN" --profile minimal \
  --component rustfmt --component clippy \
  --target aarch64-apple-darwin --target x86_64-apple-darwin
"$CARGO_HOME/bin/rustc" --version
"$CARGO_HOME/bin/cargo" --version
