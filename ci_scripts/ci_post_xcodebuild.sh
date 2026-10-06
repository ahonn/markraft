#!/bin/bash
set -euo pipefail

if [[ "${CI_XCODE_CLOUD:-}" != "TRUE" ]]; then
  echo "Run this script only in Xcode Cloud." >&2
  exit 1
fi
# Cloud calls this hook even after a failed build. Preserve the original failure
# instead of replacing it with an unrelated missing-archive error.
if [[ "${CI_XCODEBUILD_ACTION:-}" != "archive" || "${CI_XCODEBUILD_EXIT_CODE:-1}" != "0" ]]; then
  echo "Skipping archive validation: this action did not produce a successful archive."
  exit 0
fi
: "${CI_ARCHIVE_PATH:?Xcode Cloud must provide the archive path}"
: "${CI_BUILD_NUMBER:?Xcode Cloud must provide the build number}"
SCRIPT_DIRECTORY="$(cd "$(dirname "$0")" && pwd)"
python3 "$SCRIPT_DIRECTORY/verify_archive.py" \
  --archive "$CI_ARCHIVE_PATH" --build-number "$CI_BUILD_NUMBER"
