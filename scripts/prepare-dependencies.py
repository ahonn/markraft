#!/usr/bin/env python3
"""Prepare the pinned GPUI macOS source and apply Markraft's public API patch."""

import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import shutil
import subprocess
import sys
import tarfile
import tempfile
import urllib.request


CRATE = "gpui-pre-macos"
VERSION = "0.3.6"
ARCHIVE_NAME = f"{CRATE}-{VERSION}.crate"
ARCHIVE_URL = f"https://static.crates.io/crates/{CRATE}/{ARCHIVE_NAME}"
ARCHIVE_SHA256 = "951d17e41a72067ad2d35c60e08a62e8ad7fa9400b41c802e26cfb459f6c05da"


def file_hash(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def tree_hash(directory):
    """Include paths and contents so edits, additions, and deletions invalidate the cache."""
    if not directory.is_dir() or directory.is_symlink():
        return None
    digest = hashlib.sha256()
    for path in sorted(directory.rglob("*")):
        if path.is_symlink() or not (path.is_file() or path.is_dir()):
            return None
        digest.update(path.relative_to(directory).as_posix().encode("utf-8") + b"\0")
        digest.update(str(path.stat().st_mode & 0o777).encode("ascii") + b"\0")
        digest.update((file_hash(path) if path.is_file() else "directory").encode("ascii") + b"\0")
    return digest.hexdigest()


def verify_archive(path):
    if file_hash(path) != ARCHIVE_SHA256:
        raise ValueError(f"Checksum mismatch for {ARCHIVE_NAME}")


def extract_archive(archive, destination):
    prefix = f"{CRATE}-{VERSION}"
    with tarfile.open(archive, "r:gz") as source:
        members = source.getmembers()
        for member in members:
            path = PurePosixPath(member.name)
            if (path.is_absolute() or ".." in path.parts or not path.parts
                    or path.parts[0] != prefix or not (member.isfile() or member.isdir())):
                raise ValueError(f"Unsafe archive member: {member.name}")
        source.extractall(destination, members=members)
    extracted = destination / prefix
    if not (extracted / "Cargo.toml").is_file():
        raise ValueError("GPUI archive does not contain Cargo.toml")
    return extracted


def prepare(root):
    patch = (root / "patches" / f"{CRATE}-{VERSION}.patch").read_bytes()
    patch_hash = hashlib.sha256(patch).hexdigest()
    directory = root / ".build" / "dependencies"
    directory.mkdir(parents=True, exist_ok=True)
    destination = directory / CRATE
    receipt = directory / f"{CRATE}.json"
    expected = {"archive_sha256": ARCHIVE_SHA256, "patch_sha256": patch_hash}
    try:
        saved = json.loads(receipt.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        saved = {}
    if (isinstance(saved, dict) and all(saved.get(key) == value for key, value in expected.items())
            and saved.get("tree_sha256") is not None
            and saved["tree_sha256"] == tree_hash(destination)):
        return destination

    # Stage beside the destination so publication stays on the same filesystem.
    with tempfile.TemporaryDirectory(prefix=f".{CRATE}-", dir=directory) as temporary:
        staging = Path(temporary)
        archive = directory / ARCHIVE_NAME
        if not archive.exists():
            downloaded = staging / ARCHIVE_NAME
            with urllib.request.urlopen(ARCHIVE_URL, timeout=60) as source, downloaded.open("wb") as output:
                shutil.copyfileobj(source, output)
            verify_archive(downloaded)
            os.replace(downloaded, archive)
        verify_archive(archive)
        extracted = extract_archive(archive, staging)

        # Ignore the enclosing checkout: patch paths are relative to the crate.
        environment = os.environ.copy()
        environment.pop("GIT_DIR", None)
        environment.pop("GIT_WORK_TREE", None)
        environment["GIT_CEILING_DIRECTORIES"] = str(staging.resolve())
        for options in (["--check"], []):
            subprocess.run(["git", "apply", *options, "-"], input=patch, cwd=extracted,
                           env=environment, check=True)
        expected["tree_sha256"] = tree_hash(extracted)
        prepared_receipt = staging / "receipt.json"
        prepared_receipt.write_text(json.dumps(expected, indent=2) + "\n", encoding="utf-8")

        # Keep the previous source until download, extraction, and patching succeed.
        backup = staging / "previous"
        if destination.exists() or destination.is_symlink():
            os.replace(destination, backup)
        try:
            os.replace(extracted, destination)
        except OSError:
            if backup.exists() or backup.is_symlink():
                os.replace(backup, destination)
            raise
        os.replace(prepared_receipt, receipt)
    return destination


def main():
    try:
        destination = prepare(Path(__file__).resolve().parents[1])
    except (OSError, ValueError, tarfile.TarError, subprocess.CalledProcessError) as error:
        print(f"Failed to prepare GPUI: {error}", file=sys.stderr)
        return 1
    print(f"Prepared {CRATE} {VERSION}: {destination}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
