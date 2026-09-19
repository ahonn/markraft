"""Finish cargo-bundle metadata before signing. No signing key means local mode."""
import base64
import os
from pathlib import Path
import plistlib
import re
import sys


def configure(plist, public_key, mock_updates=False):
    version = plist["CFBundleShortVersionString"]
    if not re.fullmatch(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", version):
        raise ValueError("App releases require a stable major.minor.patch version")
    # cargo-bundle uses a timestamp here; Sparkle must compare release versions.
    plist["CFBundleVersion"] = version
    plist["CFBundleDisplayName"] = "Markraft Notes"
    plist["LSUIElement"] = True
    plist["SUFeedURL"] = "https://github.com/ahonn/markraft/releases/latest/download/appcast.xml"
    # Native reminders are enabled; installation remains a user-initiated action.
    plist["SUEnableAutomaticChecks"] = True
    plist["SUAllowsAutomaticUpdates"] = False
    plist["SUAutomaticallyUpdate"] = False
    if public_key:
        if len(base64.b64decode(public_key, validate=True)) != 32:
            raise ValueError("SPARKLE_PUBLIC_KEY must be a base64 Ed25519 public key (32 bytes)")
        plist["SUPublicEDKey"] = public_key
    else:
        plist.pop("SUPublicEDKey", None)
    if mock_updates:
        plist["MarkraftMockUpdates"] = True
        plist["CFBundleIdentifier"] = "dev.markraft.update-test"
        plist["CFBundleName"] = "Markraft Update Test"
        plist["CFBundleDisplayName"] = "Markraft Update Test"
        plist["SUFeedURL"] = "http://127.0.0.1:8765/appcast.xml"
        plist["SUEnableAutomaticChecks"] = False
        plist["NSAppTransportSecurity"] = {"NSAllowsLocalNetworking": True}
    return plist


if __name__ == "__main__":
    path = Path(sys.argv[1])
    config = configure(plistlib.loads(path.read_bytes()), os.environ.get("SPARKLE_PUBLIC_KEY", ""), "--mock-updates" in sys.argv[2:])
    path.write_bytes(plistlib.dumps(config))
