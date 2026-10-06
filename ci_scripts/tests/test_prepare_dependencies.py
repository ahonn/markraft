"""The GPUI bootstrap must reproduce patched source and reject invalid inputs."""

import hashlib
import importlib.util
import io
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("prepare_dependencies", ROOT / "scripts/prepare-dependencies.py")
PREPARE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PREPARE)

VALID_PATCH = b"""diff --git a/src/window.rs b/src/window.rs
--- a/src/window.rs
+++ b/src/window.rs
@@ -1 +1 @@
-private_api
+public_api
"""


class PrepareDependenciesTests(unittest.TestCase):
    def setUp(self):
        self.temporary_directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary_directory.cleanup)
        self.root = Path(self.temporary_directory.name)
        (self.root / "patches").mkdir()
        self.patch_file = self.root / "patches/gpui-pre-macos-0.3.6.patch"
        self.patch_file.write_bytes(VALID_PATCH)
        self.directory = self.root / ".build/dependencies"
        self.directory.mkdir(parents=True)
        self.archive = self.directory / PREPARE.ARCHIVE_NAME
        self.destination = self.directory / PREPARE.CRATE
        self.write_archive()
        checksum = patch.object(PREPARE, "ARCHIVE_SHA256", hashlib.sha256(self.archive.read_bytes()).hexdigest())
        checksum.start()
        self.addCleanup(checksum.stop)
        self.download = patch.object(PREPARE.urllib.request, "urlopen", side_effect=AssertionError("Unexpected network access"))
        self.download.start()
        self.addCleanup(self.download.stop)

    def write_archive(self, additional=None):
        files = {"gpui-pre-macos-0.3.6/Cargo.toml": b'[package]\nname = "gpui-pre-macos"\n',
                 "gpui-pre-macos-0.3.6/src/window.rs": b"private_api\n"}
        if additional:
            files.update(additional)
        with tarfile.open(self.archive, "w:gz") as archive:
            for name, contents in files.items():
                member = tarfile.TarInfo(name)
                member.size = len(contents)
                archive.addfile(member, io.BytesIO(contents))

    def test_applies_patch_and_reuses_unchanged_source(self):
        self.assertEqual(PREPARE.prepare(self.root), self.destination)
        source = self.destination / "src/window.rs"
        self.assertEqual(source.read_bytes(), b"public_api\n")
        original_stat = source.stat()
        with patch.object(PREPARE, "extract_archive", side_effect=AssertionError("Cache was not reused")):
            PREPARE.prepare(self.root)
        self.assertEqual(source.stat().st_mtime_ns, original_stat.st_mtime_ns)

    def test_patch_paths_ignore_enclosing_git_checkout(self):
        subprocess.run(["git", "init", "-q", str(self.root)], check=True)
        (self.root / "src").mkdir()
        unrelated = self.root / "src/window.rs"
        unrelated.write_bytes(b"private_api\n")
        PREPARE.prepare(self.root)
        self.assertEqual((self.destination / "src/window.rs").read_bytes(), b"public_api\n")
        self.assertEqual(unrelated.read_bytes(), b"private_api\n")

    def test_download_is_verified_and_cached(self):
        contents = self.archive.read_bytes()
        self.archive.unlink()
        with patch.object(PREPARE.urllib.request, "urlopen", return_value=io.BytesIO(contents)) as download:
            PREPARE.prepare(self.root)
        download.assert_called_once_with(PREPARE.ARCHIVE_URL, timeout=60)
        self.assertEqual(self.archive.read_bytes(), contents)
        self.assertEqual((self.destination / "src/window.rs").read_bytes(), b"public_api\n")

    def test_bad_download_does_not_cache_or_publish_source(self):
        self.archive.unlink()
        with patch.object(PREPARE.urllib.request, "urlopen", return_value=io.BytesIO(b"corrupt download")):
            with self.assertRaisesRegex(ValueError, "Checksum mismatch"):
                PREPARE.prepare(self.root)
        self.assertFalse(self.archive.exists())
        self.assertFalse(self.destination.exists())

    def test_bad_checksum_does_not_publish_source(self):
        self.archive.write_bytes(b"corrupt archive")
        with self.assertRaisesRegex(ValueError, "Checksum mismatch"):
            PREPARE.prepare(self.root)
        self.assertFalse(self.destination.exists())

    def test_invalid_patch_preserves_previously_prepared_source(self):
        PREPARE.prepare(self.root)
        before = PREPARE.tree_hash(self.destination)
        self.patch_file.write_bytes(VALID_PATCH.replace(b"-private_api", b"-missing_api"))
        with self.assertRaises(subprocess.CalledProcessError):
            PREPARE.prepare(self.root)
        self.assertEqual(PREPARE.tree_hash(self.destination), before)

    def test_changed_patch_rebuilds_source(self):
        PREPARE.prepare(self.root)
        self.patch_file.write_bytes(VALID_PATCH.replace(b"+public_api", b"+another_public_api"))
        PREPARE.prepare(self.root)
        self.assertEqual((self.destination / "src/window.rs").read_bytes(), b"another_public_api\n")

    def test_modified_deleted_and_extra_source_files_trigger_rebuild(self):
        PREPARE.prepare(self.root)
        source = self.destination / "src/window.rs"
        source.write_bytes(b"tampered\n")
        PREPARE.prepare(self.root)
        self.assertEqual(source.read_bytes(), b"public_api\n")
        source.unlink()
        PREPARE.prepare(self.root)
        self.assertEqual(source.read_bytes(), b"public_api\n")
        extra = self.destination / "extra.rs"
        extra.write_bytes(b"unexpected\n")
        PREPARE.prepare(self.root)
        self.assertFalse(extra.exists())

    def test_archive_traversal_is_rejected_before_extraction(self):
        self.write_archive({"gpui-pre-macos-0.3.6/../../escaped": b"unsafe"})
        with patch.object(PREPARE, "ARCHIVE_SHA256", hashlib.sha256(self.archive.read_bytes()).hexdigest()):
            with self.assertRaisesRegex(ValueError, "Unsafe archive member"):
                PREPARE.prepare(self.root)
        self.assertFalse(self.destination.exists())
        self.assertFalse((self.directory / "escaped").exists())

    def test_archive_symlink_is_rejected(self):
        with tarfile.open(self.archive, "w:gz") as archive:
            member = tarfile.TarInfo("gpui-pre-macos-0.3.6/src")
            member.type = tarfile.SYMTYPE
            member.linkname = "../../outside"
            archive.addfile(member)
        with patch.object(PREPARE, "ARCHIVE_SHA256", hashlib.sha256(self.archive.read_bytes()).hexdigest()):
            with self.assertRaisesRegex(ValueError, "Unsafe archive member"):
                PREPARE.prepare(self.root)
        self.assertFalse(self.destination.exists())


if __name__ == "__main__":
    unittest.main()
