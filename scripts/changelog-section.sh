#!/bin/bash
set -euo pipefail

# Print the body of one version's section in CHANGELOG.md, without its heading.
# Usage: bash scripts/changelog-section.sh <version>
# Exits non-zero when the changelog has no section for that version.
if [ "$#" -ne 1 ]; then
    echo "Usage: $0 <version>" >&2
    exit 1
fi

CHANGELOG="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/CHANGELOG.md"
[ -f "$CHANGELOG" ] || exit 1

awk -v version="$1" '
    /^## / {
        if (found) exit
        # Headings look like "## 0.1.1 (2026-09-26)".
        if ($2 == version) { found = 1; next }
    }
    found { print }
    END { exit found ? 0 : 1 }
' "$CHANGELOG"
