"""Behavior checks for rejecting incomplete or mismatched store archives."""

import importlib.util
import json
from pathlib import Path
import plistlib
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).resolve().parents[1] / "verify_archive.py"
SPEC = importlib.util.spec_from_file_location("verify_archive", SCRIPT)
verifier = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(verifier)

UUIDS = (
    "UUID: 46C5C989-09FC-3C54-8155-4B4DB6F2ECFC (arm64) /binary\n"
    "UUID: 96193151-FAC2-31B9-8330-93610A0EF0EE (x86_64) /binary\n"
)
SANDBOX = {
    "com.apple.security.app-sandbox": True,
    "com.apple.security.files.user-selected.read-write": True,
    "com.apple.security.files.bookmarks.app-scope": True,
    "com.apple.security.network.client": True,
    "com.apple.security.print": True,
}


class ArchiveTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.archive = Path(self.temporary.name) / "Markraft.xcarchive"
        self.app = self.archive / "Products/Applications/Markraft.app"
        self.properties = {
            "ApplicationPath": "Applications/Markraft.app",
            "CFBundleIdentifier": "app.markraft.mac",
            "CFBundleShortVersionString": "0.1.7",
            "CFBundleVersion": "3",
            "Architectures": ["arm64", "x86_64"],
        }
        self.app_info = {
            "CFBundleIdentifier": "app.markraft.mac",
            "CFBundleShortVersionString": "0.1.7",
            "CFBundleVersion": "3",
            "CFBundleExecutable": "markraft-app",
            "CFBundleIconFile": "Markraft",
            "ITSAppUsesNonExemptEncryption": False,
        }
        for path in (
            self.app / "Contents/MacOS/markraft-app",
            self.app / "Contents/Resources/Assets.car",
            self.app / "Contents/Resources/Markraft.icns",
            self.archive / "dSYMs/Markraft.app.dSYM/Contents/Resources/DWARF/markraft-app",
        ):
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b"fixture")
        (self.app / "Contents/MacOS/markraft-app").chmod(0o755)
        self.write_metadata()
        self.architectures = "x86_64 arm64\n"
        self.entitlements = dict(SANDBOX)
        self.dsym_uuids = UUIDS
        self.statistics = {"#functions with location": 100, "#line entries": 200}
        self.libraries = "binary:\n\t/usr/lib/libSystem.B.dylib (compatibility version 1.0.0, current version 1.0.0)\n"
        self.commands = []

    def write_metadata(self):
        (self.archive / "Info.plist").write_bytes(plistlib.dumps({"ApplicationProperties": self.properties}))
        (self.app / "Contents/Info.plist").write_bytes(plistlib.dumps(self.app_info))

    def runner(self, command):
        self.commands.append(command)
        if command[1:3] == ["lipo", "-archs"]:
            return self.architectures
        if "--verify" in command:
            return ""
        if "--entitlements" in command:
            self.assertIn("--xml", command)
            return plistlib.dumps(self.entitlements).decode()
        if "otool" in command:
            return self.libraries
        if "--uuid" in command:
            return self.dsym_uuids if command[-1].endswith(".dSYM") else UUIDS
        if "--statistics" in command:
            architecture = command[2].split("=", 1)[1]
            return json.dumps(dict(self.statistics, file="/symbols({})".format(architecture)))
        self.fail("Unexpected command: " + repr(command))

    def verify(self):
        return verifier.verify_archive(self.archive, "3", self.runner)

    def test_accepts_valid_archive_with_signing_entitlements(self):
        self.entitlements["com.apple.developer.team-identifier"] = "C97A45J272"
        self.entitlements["com.apple.application-identifier"] = "C97A45J272.app.markraft.mac"
        result = self.verify()
        self.assertEqual(result["version"], "0.1.7")
        self.assertIs(result["uses_non_exempt_encryption"], False)
        self.assertEqual(set(result["symbols"]), {"arm64", "x86_64"})
        entitlement_commands = [command for command in self.commands if "--entitlements" in command]
        self.assertEqual(len(entitlement_commands), 2)

    def test_rejects_invalid_release_versions(self):
        for version in ("0.1", "01.1.7", "0.1.7-beta.1", "", "1.2.3\n"):
            with self.subTest(version=version):
                self.app_info["CFBundleShortVersionString"] = version
                self.properties["CFBundleShortVersionString"] = version
                self.write_metadata()
                with self.assertRaisesRegex(verifier.VerificationError, "release version"):
                    self.verify()

    def test_rejects_archive_version_mismatch(self):
        self.properties["CFBundleShortVersionString"] = "0.1.6"
        self.write_metadata()
        with self.assertRaisesRegex(verifier.VerificationError, "versions differ"):
            self.verify()

    def test_rejects_missing_encryption_declaration(self):
        self.app_info.pop("ITSAppUsesNonExemptEncryption")
        self.write_metadata()
        with self.assertRaisesRegex(verifier.VerificationError, "ITSAppUsesNonExemptEncryption"):
            self.verify()
        self.assertEqual(self.commands, [])

    def test_rejects_non_boolean_false_encryption_declaration(self):
        for value in (True, 0, 1, "false", "NO", ""):
            with self.subTest(value=value):
                self.app_info["ITSAppUsesNonExemptEncryption"] = value
                self.write_metadata()
                with self.assertRaisesRegex(verifier.VerificationError, "ITSAppUsesNonExemptEncryption"):
                    self.verify()
                self.assertEqual(self.commands, [])

    def test_rejects_stale_build_number(self):
        self.app_info["CFBundleVersion"] = "2"
        self.write_metadata()
        with self.assertRaisesRegex(verifier.VerificationError, "build number"):
            self.verify()

    def test_rejects_missing_binary_architecture_despite_complete_metadata(self):
        self.architectures = "arm64"
        with self.assertRaisesRegex(verifier.VerificationError, "Executable must contain"):
            self.verify()

    def test_rejects_missing_or_false_entitlement(self):
        for value in (None, False, 1, "true"):
            with self.subTest(value=value):
                if value is None:
                    self.entitlements.pop("com.apple.security.app-sandbox")
                else:
                    self.entitlements["com.apple.security.app-sandbox"] = value
                with self.assertRaisesRegex(verifier.VerificationError, "app-sandbox"):
                    self.verify()

    def test_rejects_uuid_assigned_to_wrong_architecture(self):
        self.dsym_uuids = UUIDS.replace("(arm64)", "(temporary)").replace("(x86_64)", "(arm64)").replace("(temporary)", "(x86_64)")
        with self.assertRaisesRegex(verifier.VerificationError, "UUIDs differ"):
            self.verify()

    def test_rejects_missing_dsym_architecture(self):
        self.dsym_uuids = UUIDS.splitlines()[0]
        with self.assertRaisesRegex(verifier.VerificationError, "UUIDs must include"):
            self.verify()

    def test_rejects_empty_symbols_even_with_matching_uuids(self):
        for key in ("#functions with location", "#line entries"):
            with self.subTest(key=key):
                previous = self.statistics[key]
                self.statistics[key] = 0
                with self.assertRaisesRegex(verifier.VerificationError, "has no"):
                    self.verify()
                self.statistics[key] = previous

    def test_rejects_missing_icon(self):
        (self.app / "Contents/Resources/Markraft.icns").unlink()
        with self.assertRaisesRegex(verifier.VerificationError, "Missing or empty file"):
            self.verify()

    def test_rejects_sparkle_and_local_library_paths(self):
        for library in ("@rpath/Sparkle.framework/Sparkle", "/opt/homebrew/lib/libssl.dylib", "/usr/lib/../../tmp/library.dylib"):
            with self.subTest(library=library):
                self.libraries = "binary:\n\t{} (compatibility version 1.0.0, current version 1.0.0)\n".format(library)
                with self.assertRaises(verifier.VerificationError):
                    self.verify()

    def test_rejects_archive_application_path_escape(self):
        self.properties["ApplicationPath"] = "../../Applications/Markraft.app"
        self.write_metadata()
        with self.assertRaisesRegex(verifier.VerificationError, "Unsafe bundle path"):
            self.verify()

    def test_command_failure_preserves_diagnostic(self):
        result = subprocess.CompletedProcess(["codesign"], 1, "", "invalid signature")
        with patch.object(verifier.subprocess, "run", return_value=result):
            with self.assertRaisesRegex(verifier.VerificationError, "invalid signature"):
                verifier.run(["codesign", "--verify", "/archive"])

    def test_cli_reports_invalid_archive_without_success_json(self):
        result = subprocess.run([sys.executable, str(SCRIPT), "--archive", str(self.archive), "--build-number", "2"], capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("build number", result.stderr)
        self.assertEqual(result.stdout, "")


if __name__ == "__main__":
    unittest.main()
