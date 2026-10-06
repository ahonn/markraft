#!/usr/bin/env python3
"""Reject release tags that do not match the workspace marketing version."""

import argparse
import os
from pathlib import Path
import re
import sys
from typing import Optional


VERSION_PATTERN = r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)"
RELEASE_TAG = re.compile(r"v(" + VERSION_PATTERN + r")")
WORKSPACE_HEADER = re.compile(r"\[workspace\.package\]\s*(?:#.*)?")
VERSION_ASSIGNMENT = re.compile(r"version\s*=\s*([\"'])([^\"']+)\1\s*(?:#.*)?")


def workspace_version(manifest: Path) -> str:
    """Read the repository's literal workspace.package.version without TOML dependencies.

    This deliberately accepts only the simple, single-line version declaration
    used by our manifest. Fail closed if that declaration changes shape.
    """
    in_workspace_package = False
    versions = []
    for raw_line in manifest.read_text(encoding="utf-8").splitlines():
        line = raw_line.strip()
        if line.startswith("["):
            in_workspace_package = WORKSPACE_HEADER.fullmatch(line) is not None
        elif in_workspace_package:
            match = VERSION_ASSIGNMENT.fullmatch(line)
            if match:
                versions.append(match.group(2))
    if len(versions) != 1 or re.fullmatch(VERSION_PATTERN, versions[0]) is None:
        raise ValueError("Cargo.toml must declare one literal X.Y.Z workspace.package.version")
    return versions[0]


def verify_release_tag(manifest: Path, tag: Optional[str]) -> Optional[str]:
    if tag is None:
        return None
    match = RELEASE_TAG.fullmatch(tag)
    if match is None:
        raise ValueError("CI_TAG must be a stable release tag in vX.Y.Z format")
    version = workspace_version(manifest)
    if match.group(1) != version:
        raise ValueError("CI_TAG version does not match workspace.package.version " + version)
    return version


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, required=True)
    arguments = parser.parse_args()
    try:
        version = verify_release_tag(arguments.manifest, os.environ.get("CI_TAG"))
    except (OSError, ValueError) as error:
        print("Release tag validation failed: " + str(error), file=sys.stderr)
        return 1
    if version is None:
        print("No CI_TAG supplied; allowing a manual or branch build.")
    else:
        print("Verified release tag v" + version + " against Cargo.toml.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
