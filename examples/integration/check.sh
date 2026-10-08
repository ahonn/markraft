#!/bin/sh
set -eu

example_directory=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repository_directory=$(CDPATH= cd -- "$example_directory/../.." && pwd)
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$repository_directory/target}"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}"

python3 "$repository_directory/scripts/prepare-dependencies.py"
cargo fmt --manifest-path "$example_directory/Cargo.toml" --all -- --check
cargo build --manifest-path "$example_directory/Cargo.toml" --workspace --locked
cargo test --manifest-path "$example_directory/Cargo.toml" --package markraft-sqlite-example --locked
cargo run --manifest-path "$example_directory/Cargo.toml" --package markraft-notes-consumer --locked
cargo run --manifest-path "$example_directory/Cargo.toml" --package markraft-notes-consumer --locked -- --storage sqlite
python3 - "$example_directory/Cargo.toml" <<'PY'
import subprocess
import sys

manifest = sys.argv[1]
tree = subprocess.check_output(
    [
        "cargo", "tree", "--manifest-path", manifest,
        "--package", "markraft-notes-consumer", "--edges", "normal",
        "--prefix", "none", "--format", "{p}", "--locked",
    ],
    text=True,
)
for line in tree.splitlines():
    package = line.split()[0]
    if "gpui" in package or package in {"markraft-app", "markraft-workspace"}:
        raise SystemExit(f"Unexpected GUI dependency in the notes consumer: {line}")
print("The notes consumer has no GPUI, workspace, or application dependency.")
PY
