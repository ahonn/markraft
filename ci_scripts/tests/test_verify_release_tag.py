"""Release tags must identify the exact stable version being archived."""

import importlib.util
from pathlib import Path
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("verify_release_tag", ROOT / "ci_scripts/verify_release_tag.py")
VERIFY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(VERIFY)


class ReleaseTagTests(unittest.TestCase):
    def setUp(self):
        self.temporary_directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary_directory.cleanup)
        self.manifest = Path(self.temporary_directory.name) / "Cargo.toml"
        self.manifest.write_text('[workspace.package]\nversion = "0.1.7"\n', encoding="utf-8")

    def test_matching_stable_tag_accepts_zero_major_version(self):
        self.assertEqual(VERIFY.verify_release_tag(self.manifest, "v0.1.7"), "0.1.7")

    def test_manual_build_does_not_require_tag_or_read_manifest(self):
        self.manifest.unlink()
        self.assertIsNone(VERIFY.verify_release_tag(self.manifest, None))

    def test_invalid_tag_shapes_are_rejected(self):
        for tag in ("", "0.1.7", "v0.1", "v0.1.7-rc.1", "v0.1.7+build", "v00.1.7",
                    "v0.01.7", "v0.1.07", "refs/tags/v0.1.7", "v0.1.7\n", "v0.1.7;touch marker"):
            with self.subTest(tag=tag), self.assertRaisesRegex(ValueError, "stable release tag"):
                VERIFY.verify_release_tag(self.manifest, tag)

    def test_mismatched_tag_is_rejected(self):
        with self.assertRaisesRegex(ValueError, "does not match"):
            VERIFY.verify_release_tag(self.manifest, "v0.1.8")

    def test_only_workspace_package_version_is_used(self):
        self.manifest.write_text(
            '[package]\nversion = "9.0.0"\n\n'
            '[workspace.package] # Inherited app version\n'
            "version = '0.1.7' # Marketing version\n"
            '[workspace.dependencies]\nversion = "8.0.0"\n', encoding="utf-8",
        )
        self.assertEqual(VERIFY.verify_release_tag(self.manifest, "v0.1.7"), "0.1.7")

    def test_missing_duplicate_or_non_literal_workspace_version_is_rejected(self):
        manifests = (
            '[package]\nversion = "0.1.7"\n',
            '[workspace.package]\nversion = "0.1.7"\nversion = "0.1.7"\n',
            '[workspace.package]\nversion.workspace = true\n',
            '[workspace.package]\nversion = "0.1.7-beta.1"\n',
            '[workspace.package]\nversion = "00.1.7"\n',
        )
        for manifest in manifests:
            with self.subTest(manifest=manifest):
                self.manifest.write_text(manifest, encoding="utf-8")
                with self.assertRaisesRegex(ValueError, "one literal X.Y.Z"):
                    VERIFY.verify_release_tag(self.manifest, "v0.1.7")


if __name__ == "__main__":
    unittest.main()
