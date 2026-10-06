#!/usr/bin/env python3
"""Verify a Markraft store archive without the checkout or Rust toolchain."""

import argparse
import json
import os
from pathlib import Path, PurePosixPath
import plistlib
import re
import subprocess
import sys


BUNDLE_ID = "app.markraft.mac"
ARCHITECTURES = {"arm64", "x86_64"}
ENTITLEMENTS = (
    "com.apple.security.app-sandbox",
    "com.apple.security.files.user-selected.read-write",
    "com.apple.security.files.bookmarks.app-scope",
    "com.apple.security.network.client",
    "com.apple.security.print",
)


class VerificationError(Exception):
    """An archive does not meet the store build contract."""


def require(condition, message):
    if not condition:
        raise VerificationError(message)


def run(command):
    result = subprocess.run(command, capture_output=True, text=True, check=False)
    if result.returncode:
        raise VerificationError(
            "{} failed ({}): {}".format(
                " ".join(map(str, command)),
                result.returncode,
                (result.stderr + result.stdout).strip(),
            )
        )
    return result.stdout


def read_plist(path):
    with path.open("rb") as stream:
        value = plistlib.load(stream)
    require(isinstance(value, dict), "Expected a plist dictionary: {}".format(path))
    return value


def contained_path(root, relative):
    require(isinstance(relative, str) and relative, "Missing relative bundle path")
    path = PurePosixPath(relative)
    require(not path.is_absolute() and ".." not in path.parts, "Unsafe bundle path: " + relative)
    resolved = (root / relative).resolve()
    require(root.resolve() in resolved.parents, "Bundle path escapes its directory: " + relative)
    return resolved


def require_file(path):
    require(path.is_file() and path.stat().st_size > 0, "Missing or empty file: {}".format(path))


def validate_metadata(properties, app_info, build_number):
    require(re.fullmatch(r"[1-9][0-9]*", build_number), "Build number must be a positive integer")
    for info in (properties, app_info):
        require(info.get("CFBundleIdentifier") == BUNDLE_ID, "Incorrect bundle identifier")
        require(info.get("CFBundleVersion") == build_number, "Incorrect archive or app build number")
    version = app_info.get("CFBundleShortVersionString", "")
    # Store release versions use three numeric components, without prerelease suffixes.
    require(isinstance(version, str) and re.fullmatch(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", version),
            "Invalid store release version: {!r}".format(version))
    require(properties.get("CFBundleShortVersionString") == version, "Archive and app versions differ")
    require(set(properties.get("Architectures", [])) == ARCHITECTURES, "Archive metadata must include arm64 and x86_64")
    require(app_info.get("ITSAppUsesNonExemptEncryption") is False,
            "ITSAppUsesNonExemptEncryption must be the boolean false for the current TestFlight scope")
    return version


def validate_entitlements(entitlements):
    require(isinstance(entitlements, dict), "Missing entitlement dictionary")
    missing = [key for key in ENTITLEMENTS if entitlements.get(key) is not True]
    require(not missing, "Missing sandbox entitlements: " + ", ".join(missing))


def validate_architectures(output):
    require(set(output.split()) == ARCHITECTURES, "Executable must contain arm64 and x86_64: " + output.strip())


def validate_libraries(output):
    require("sparkle" not in output.lower(), "Store archive links Sparkle")
    libraries = []
    for line in output.splitlines():
        if not line.startswith(("\t", " ")):
            continue
        match = re.fullmatch(r"\s+(.+?) \(compatibility version .+\)", line)
        require(match is not None, "Unrecognized otool dependency: " + line)
        library = match.group(1)
        if library.startswith("/"):
            normalized = os.path.normpath(library)
            require(normalized.startswith(("/System/Library/", "/usr/lib/")),
                    "Non-system absolute library path: " + library)
        else:
            require(library.startswith(("@rpath/", "@loader_path/", "@executable_path/")),
                    "Unrecognized library path: " + library)
        libraries.append(library)
    require(libraries, "No linked libraries found")
    return sorted(set(libraries))


def parse_uuids(output):
    uuids = {}
    for line in output.splitlines():
        if not line.strip():
            continue
        match = re.fullmatch(r"UUID: ([0-9A-Fa-f]{8}(?:-[0-9A-Fa-f]{4}){3}-[0-9A-Fa-f]{12}) \(([^)]+)\) .+", line)
        require(match is not None, "Unrecognized UUID output: " + line)
        uuid, architecture = match.groups()
        require(architecture not in uuids, "Duplicate UUID architecture: " + architecture)
        uuids[architecture] = uuid.upper()
    require(set(uuids) == ARCHITECTURES, "UUIDs must include arm64 and x86_64")
    return uuids


def validate_uuid_pair(binary_output, dsym_output):
    binary_uuids = parse_uuids(binary_output)
    require(binary_uuids == parse_uuids(dsym_output), "Executable and dSYM UUIDs differ")
    return binary_uuids


def validate_statistics(output, architecture):
    statistics = json.loads(output)
    require(isinstance(statistics, dict), "Invalid dSYM statistics")
    require(str(statistics.get("file", "")).endswith("({})".format(architecture)),
            "dSYM statistics report the wrong architecture")
    counts = {}
    for key in ("#functions with location", "#line entries"):
        value = statistics.get(key)
        require(type(value) is int and value > 0, "dSYM {} has no {}".format(architecture, key))
        counts[key.removeprefix("#")] = value
    return counts


def verify_archive(archive, build_number, runner=run):
    archive = archive.resolve()
    archive_info = read_plist(archive / "Info.plist")
    properties = archive_info.get("ApplicationProperties")
    require(isinstance(properties, dict), "Archive lacks ApplicationProperties")
    app = contained_path(archive / "Products", properties.get("ApplicationPath"))
    require(app.suffix == ".app" and app.parent.name == "Applications", "Invalid archive ApplicationPath")
    app_info = read_plist(app / "Contents/Info.plist")
    version = validate_metadata(properties, app_info, build_number)
    executable_name = app_info.get("CFBundleExecutable")
    require(isinstance(executable_name, str) and Path(executable_name).name == executable_name,
            "Invalid CFBundleExecutable")
    executable = contained_path(app / "Contents/MacOS", executable_name)
    require_file(executable)
    require(os.access(executable, os.X_OK), "App binary is not executable")
    resources = app / "Contents/Resources"
    require_file(resources / "Assets.car")
    icon = app_info.get("CFBundleIconFile")
    require(isinstance(icon, str) and icon, "Missing CFBundleIconFile")
    require_file(contained_path(resources, icon if icon.endswith(".icns") else icon + ".icns"))
    validate_architectures(runner(["xcrun", "lipo", "-archs", str(executable)]))
    runner(["/usr/bin/codesign", "--verify", "--deep", "--strict", str(app)])
    for architecture in sorted(ARCHITECTURES):
        entitlements = runner(["/usr/bin/codesign", "--display", "--entitlements", "-", "--xml", "--arch", architecture, str(app)])
        validate_entitlements(plistlib.loads(entitlements.encode("utf-8")))
    libraries = validate_libraries(runner(["xcrun", "otool", "-L", str(executable)]))
    dsym = archive / "dSYMs" / (app.name + ".dSYM")
    require_file(dsym / "Contents/Resources/DWARF" / executable_name)
    uuids = validate_uuid_pair(
        runner(["xcrun", "dwarfdump", "--uuid", str(executable)]),
        runner(["xcrun", "dwarfdump", "--uuid", str(dsym)]),
    )
    symbols = {}
    for architecture in sorted(ARCHITECTURES):
        output = runner(["xcrun", "dwarfdump", "--arch=" + architecture, "--statistics", str(dsym)])
        symbols[architecture] = validate_statistics(output, architecture)
    return {
        "archive": str(archive), "bundle_id": BUNDLE_ID, "version": version,
        "build_number": build_number, "architectures": sorted(ARCHITECTURES),
        "signature": "verified", "sandbox_entitlements": "verified",
        "uses_non_exempt_encryption": False,
        "linked_library_count": len(libraries), "uuids": uuids, "symbols": symbols,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive", required=True, type=Path)
    parser.add_argument("--build-number", required=True)
    arguments = parser.parse_args()
    try:
        summary = verify_archive(arguments.archive, arguments.build_number)
    except (VerificationError, OSError, ValueError, plistlib.InvalidFileException) as error:
        print("Archive verification failed: {}".format(error), file=sys.stderr)
        return 1
    print(json.dumps(summary, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
