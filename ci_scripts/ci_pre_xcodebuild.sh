#!/bin/bash
set -euo pipefail

SCRIPT_DIRECTORY="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIRECTORY/cloud_environment.sh"
bash "$SCRIPT_DIRECTORY/install_rust.sh"
python3 scripts/prepare-dependencies.py

# Xcode reads this include after the hook exits. The tracked project keeps its
# local default and never needs an in-place edit to project.pbxproj.
mkdir -p target/xcode-cloud
printf 'CURRENT_PROJECT_VERSION = %s\n' "$CI_BUILD_NUMBER" \
  > target/xcode-cloud/BuildNumber.xcconfig
echo "Prepared Markraft build $CI_BUILD_NUMBER."
