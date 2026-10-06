#!/usr/bin/env python3
"""Package a local binary for isolated App Sandbox experiments, never distribution."""

import argparse
import hashlib
import json
import pathlib
import plistlib
import shutil
import subprocess


ROOT = pathlib.Path(__file__).resolve().parents[1]
BUNDLE_ID = "app.markraft.masvalidation"
PRIVATE_SELECTORS = (
    "_opaqueRectForWindowMoveWhenInTitlebar",
    "_windowRestorationOptions",
    "_zoomFill:",
    "_windowResizeNorthWestSouthEastCursor",
    "_windowResizeNorthEastSouthWestCursor",
    "NSWindowRestorationOptions",
    "NSWindowRestoresWorkspaceAtLaunch",
    "CAChameleonLayer",
)


def run(*args):
    return subprocess.check_output(args, text=True, stderr=subprocess.STDOUT)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("label", help="A new experiment directory name")
    parser.add_argument("--binary", type=pathlib.Path, default=ROOT / "target/debug/markraft-app")
    args = parser.parse_args()
    if pathlib.Path(args.label).name != args.label or args.label in (".", ".."):
        parser.error("label must be a single directory name")
    binary = args.binary.resolve(strict=True)
    linked = run("otool", "-L", str(binary))
    if "Sparkle.framework" in linked:
        parser.error("binary links Sparkle; build with --no-default-features --features mac-app-store")
    metadata = json.loads(run("cargo", "metadata", "--locked", "--no-deps",
                              "--format-version", "1", "--manifest-path", str(ROOT / "Cargo.toml")))
    version = next(p["version"] for p in metadata["packages"] if p["name"] == "markraft-app")
    output = ROOT / "target/mas-validation" / args.label
    output.mkdir(parents=True, exist_ok=False)
    app = output / "Markraft MAS Validation.app"
    contents = app / "Contents"
    (contents / "MacOS").mkdir(parents=True)
    shutil.copy2(binary, contents / "MacOS/markraft-app")
    info = {
        "CFBundleIdentifier": BUNDLE_ID,
        "CFBundleExecutable": "markraft-app",
        "CFBundleName": "Markraft MAS Validation",
        "CFBundleDisplayName": "Markraft MAS Validation",
        "CFBundlePackageType": "APPL",
        "CFBundleVersion": "1",
        "CFBundleShortVersionString": version,
        "LSMinimumSystemVersion": "13.0",
        "LSUIElement": True,
        "NSHighResolutionCapable": True,
    }
    (contents / "Info.plist").write_bytes(plistlib.dumps(info))
    entitlements = output / "sandbox.entitlements"
    shutil.copy2(ROOT / "crates/markraft-app/entitlements/mac-app-store.plist", entitlements)
    run("codesign", "--force", "--sign", "-", "--entitlements", str(entitlements), str(app))
    verification = run("codesign", "--verify", "--deep", "--strict", str(app))
    signed_entitlements = run("codesign", "-d", "--entitlements", "-", str(app))
    linked = run("otool", "-L", str(contents / "MacOS/markraft-app"))
    strings = run("strings", "-a", str(contents / "MacOS/markraft-app"))
    report = {
        "bundle": str(app),
        "bundle_id": BUNDLE_ID,
        "source_binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "private_selectors_present": [s for s in PRIVATE_SELECTORS if s in strings],
        "linked_libraries": linked,
        "signed_entitlements": signed_entitlements,
        "codesign_verify": verification or "passed",
        "distribution_ready": False,
    }
    (output / "binary-report.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
