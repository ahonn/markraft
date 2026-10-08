#!/bin/sh
# Compare the public interface of the embeddable crates with the latest release.
# A change that breaks a host's use of that interface fails, unless a change file
# in .changeset declares the next release `major`.
#
# Usage: scripts/check-public-api.sh [baseline revision]
set -eu

repository=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repository"

baseline=${1:-}
if [ -z "$baseline" ]; then
    baseline=$(git describe --tags --abbrev=0 --match 'v[0-9]*' 2>/dev/null || true)
fi
if [ -z "$baseline" ]; then
    # A shallow checkout has no tags. Take the newest release from the remote.
    baseline=$(git ls-remote --tags --refs origin 'v[0-9]*' \
        | sed 's#.*refs/tags/##' | sort -V | tail -n 1)
    if [ -z "$baseline" ]; then
        echo "There is no release to compare with."
        exit 0
    fi
    git fetch --quiet --depth=1 origin "refs/tags/$baseline:refs/tags/$baseline"
fi

release_type=minor
if grep -qsE '^default:[[:space:]]*major' .changeset/*.md; then
    release_type=major
fi

work=$(mktemp -d)
cleanup() {
    git worktree remove --force "$work/baseline" >/dev/null 2>&1 || true
    rm -rf "$work"
}
trap cleanup EXIT
git worktree add --quiet --detach "$work/baseline" "$baseline"

prepared=false
status=0
# check <crate> [feature flags]: only the features that a host can rely on.
check() {
    crate=$1
    shift
    if [ ! -f "$work/baseline/crates/$crate/Cargo.toml" ]; then
        echo "$crate is not in $baseline: nothing to compare."
        return 0
    fi
    if [ "$prepared" = false ]; then
        (cd "$work/baseline" && python3 scripts/prepare-dependencies.py)
        prepared=true
    fi
    cargo semver-checks --package "$crate" --baseline-root "$work/baseline" \
        --release-type "$release_type" --only-explicit-features "$@" || status=1
}

check markraft-notes --features conformance
check markraft-workspace
exit "$status"
