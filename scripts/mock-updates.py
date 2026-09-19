#!/usr/bin/env python3
"""Prepare isolated Sparkle update fixtures and serve them on loopback only."""

import argparse
import base64
import functools
import http.server
import json
from pathlib import Path
import plistlib
import shutil
import subprocess
import sys
import xml.etree.ElementTree as ET

from appcast import generate_appcast, sparkle

ROOT = Path(__file__).resolve().parent.parent
WORK = ROOT / "target/mock-updates"
APP_NAME = "Markraft Update Test.app"
BUNDLE_ID = "dev.markraft.update-test"
INSTALL = Path.home() / "Applications" / APP_NAME
SETTINGS = Path.home() / "Library/Application Support/Markraft Update Test/settings.json"
SCENARIOS = ("valid", "invalid-signature", "no-update")
SWIFT_SIGNER = r'''
import CryptoKit
import Foundation

let arguments = CommandLine.arguments
let keyURL = URL(fileURLWithPath: arguments[2])
if arguments[1] == "generate" {
    let key = Curve25519.Signing.PrivateKey()
    try key.rawRepresentation.write(to: keyURL, options: .atomic)
    try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: keyURL.path)
    print(key.publicKey.rawRepresentation.base64EncodedString())
} else {
    let key = try Curve25519.Signing.PrivateKey(rawRepresentation: Data(contentsOf: keyURL))
    let archive = try Data(contentsOf: URL(fileURLWithPath: arguments[3]))
    print(try key.signature(for: archive).base64EncodedString())
}
'''


def run(*arguments):
    subprocess.run([str(arg) for arg in arguments], cwd=ROOT, check=True)


def local_appcast(archive, version, signature, port):
    """Use the release generator, replacing only its public URLs and labels."""
    document = ET.fromstring(generate_appcast(archive, version, "ahonn/markraft", signature))
    base = f"http://127.0.0.1:{port}"
    channel = document.find("channel")
    channel.find("title").text = "Markraft Local Update Test"
    channel.find("link").text = base
    item = channel.find("item")
    item.find("title").text = f"Markraft Update Test {version}"
    item.find("link").text = f"{base}/release-notes.html"
    item.find(sparkle("fullReleaseNotesLink")).text = f"{base}/release-notes.html"
    item.find("enclosure").set("url", f"{base}/{archive.name}")
    ET.indent(document, space="  ")
    return ET.tostring(document, encoding="utf-8", xml_declaration=True) + b"\n"


def copy_app(source, destination, version, public_key, port):
    run("ditto", source, destination)
    info_path = destination / "Contents/Info.plist"
    with info_path.open("rb") as handle:
        info = plistlib.load(handle)
    info.update({
        "CFBundleIdentifier": BUNDLE_ID,
        "CFBundleName": "Markraft Update Test",
        "CFBundleDisplayName": "Markraft Update Test",
        "CFBundleVersion": version,
        "CFBundleShortVersionString": version,
        "SUFeedURL": f"http://127.0.0.1:{port}/appcast.xml",
        "SUPublicEDKey": public_key,
        "SUEnableAutomaticChecks": False,
        "SUAutomaticallyUpdate": False,
        "SUScheduledCheckInterval": 86400,
        "NSAppTransportSecurity": {"NSAllowsLocalNetworking": True},
    })
    with info_path.open("wb") as handle:
        plistlib.dump(info, handle)
    run("bash", ROOT / "scripts/sign-app.sh", destination, "-")


def prepare(args):
    # Check collisions before building, removing fixtures, or generating new keys.
    if INSTALL.exists() or INSTALL.is_symlink():
        if not args.reset:
            raise ValueError(f"{INSTALL} already exists; use --reset to explicitly replace this test app")
        with (INSTALL / "Contents/Info.plist").open("rb") as handle:
            if plistlib.load(handle).get("CFBundleIdentifier") != BUNDLE_ID:
                raise ValueError("refusing to replace an app without the isolated test bundle identifier")
    if SETTINGS.exists() and not args.reset:
        raise ValueError(f"{SETTINGS} already exists; use --reset to explicitly replace test settings")
    if WORK.exists() and not args.reset:
        raise ValueError(f"{WORK} already exists; use serve to reuse it or prepare --reset to replace it")
    if not args.skip_build:
        run("bash", ROOT / "scripts/bundle-app.sh", "--mock-updates")
    source = ROOT / "target/debug/bundle/osx/Markraft Notes.app"
    with (source / "Contents/Info.plist").open("rb") as handle:
        if plistlib.load(handle).get("MarkraftMockUpdates") is not True:
            raise ValueError("source app lacks MarkraftMockUpdates=true; build with --mock-updates")
    if WORK.exists():
        shutil.rmtree(WORK)
    WORK.mkdir(parents=True, mode=0o700)
    signer = WORK / "sign.swift"
    signer.write_text(SWIFT_SIGNER)
    key = WORK / "private-key.bin"
    public_key = subprocess.check_output(["swift", str(signer), "generate", str(key)], text=True).strip()
    public = WORK / "public"
    public.mkdir()
    archives = {}
    for version in ("0.1.0", "0.1.1"):
        app = WORK / version / APP_NAME
        app.parent.mkdir()
        copy_app(source, app, version, public_key, args.port)
        archive = public / f"Markraft-Update-Test-{version}.zip"
        run("ditto", "-c", "-k", "--sequesterRsrc", "--keepParent", app, archive)
        signature = subprocess.check_output(["swift", str(signer), "sign", str(key), str(archive)], text=True).strip()
        archives[version] = (archive, signature)
    archive, signature = archives["0.1.1"]
    (public / "valid.xml").write_bytes(local_appcast(archive, "0.1.1", signature, args.port))
    invalid = bytearray(base64.b64decode(signature))
    invalid[0] ^= 1
    (public / "invalid-signature.xml").write_bytes(local_appcast(
        archive, "0.1.1", base64.b64encode(invalid).decode("ascii"), args.port))
    archive, signature = archives["0.1.0"]
    (public / "no-update.xml").write_bytes(local_appcast(archive, "0.1.0", signature, args.port))
    (public / "release-notes.html").write_text(
        "<!doctype html><html><body><h1>Local Sparkle Update Test</h1>"
        "<p>Isolated test release 0.1.1. Notes should survive installation and relaunch.</p></body></html>\n")
    (WORK / "scenario.txt").write_text(args.scenario + "\n")
    (WORK / "config.json").write_text(json.dumps({"port": args.port, "public_key": public_key}, indent=2) + "\n")
    (WORK / "notes").mkdir()
    SETTINGS.parent.mkdir(parents=True, exist_ok=True)
    SETTINGS.write_text(json.dumps({"notes_folder": str(WORK / "notes")}, indent=2) + "\n")
    INSTALL.parent.mkdir(parents=True, exist_ok=True)
    if INSTALL.is_symlink():
        INSTALL.unlink()
    elif INSTALL.exists():
        shutil.rmtree(INSTALL)
    run("ditto", WORK / "0.1.0" / APP_NAME, INSTALL)
    print(f"Installed baseline: {INSTALL}")
    print(f"Feed: http://127.0.0.1:{args.port}/appcast.xml")
    print(f"Next: python3 scripts/mock-updates.py serve --port {args.port}")
    print(f"Change live scenario: write one of {', '.join(SCENARIOS)} to {WORK / 'scenario.txt'}")


class Handler(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        if self.path.split("?", 1)[0] == "/appcast.xml":
            scenario = (WORK / "scenario.txt").read_text().strip()
            if scenario not in SCENARIOS:
                self.send_error(500, "Invalid local test scenario")
                return
            self.path = f"/{scenario}.xml"
        super().do_GET()

    def end_headers(self):
        self.send_header("Cache-Control", "no-store")
        super().end_headers()

    def log_message(self, format, *args):
        message = f"{self.log_date_time_string()} {self.address_string()} {format % args}\n"
        with (WORK / "access.log").open("a") as handle:
            handle.write(message)
        sys.stderr.write(message)


def serve(args):
    config = json.loads((WORK / "config.json").read_text())
    if args.port != config["port"]:
        raise ValueError(f"fixtures use port {config['port']}; prepare again to change it")
    handler = functools.partial(Handler, directory=str(WORK / "public"))
    with http.server.ThreadingHTTPServer(("127.0.0.1", args.port), handler) as server:
        print(f"Serving local appcast on http://127.0.0.1:{args.port}/appcast.xml", flush=True)
        server.serve_forever()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    prepare_parser = commands.add_parser("prepare", help="build, sign, package, and install isolated baseline")
    prepare_parser.add_argument("--skip-build", action="store_true")
    prepare_parser.add_argument("--reset", action="store_true", help="replace existing test fixtures and isolated test app")
    prepare_parser.add_argument("--scenario", choices=SCENARIOS, default="valid")
    serve_parser = commands.add_parser("serve", help="serve prepared fixtures until interrupted")
    for command in (prepare_parser, serve_parser):
        command.add_argument("--port", type=int, default=8765)
    args = parser.parse_args()
    if not 1 <= args.port <= 65535:
        parser.error("port must be between 1 and 65535")
    try:
        (prepare if args.command == "prepare" else serve)(args)
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"error: {error}\n")
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
