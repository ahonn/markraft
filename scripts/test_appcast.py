"""Behavior tests for the public GitHub release appcast generator."""

import base64
from contextlib import redirect_stderr
from datetime import timedelta
from email.utils import parsedate_to_datetime
import io
from pathlib import Path
import tempfile
import unittest
import xml.etree.ElementTree as ET

from appcast import generate_appcast, main, sparkle


class AppcastTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.archive = Path(self.directory.name) / 'Markraft & "universal" #1.zip'
        self.archive.write_bytes(b"signed archive contents")
        self.signature = base64.b64encode(bytes(range(64))).decode("ascii")

    def generate(self, **overrides):
        arguments = {
            "archive": self.archive,
            "version": "0.2.0",
            "repository": "ahonn/markraft",
            "signature": self.signature,
        }
        arguments.update(overrides)
        return generate_appcast(**arguments)

    def test_feed_contains_signed_release_metadata_and_escaped_archive_url(self):
        document = self.generate()
        root = ET.fromstring(document)
        self.assertEqual(root.tag, "rss")
        self.assertEqual(root.attrib["version"], "2.0")
        item = root.find("channel/item")
        self.assertEqual(item.findtext("title"), "Markraft 0.2.0")
        self.assertEqual(item.findtext(sparkle("version")), "0.2.0")
        self.assertEqual(item.findtext(sparkle("shortVersionString")), "0.2.0")
        self.assertEqual(item.findtext(sparkle("minimumSystemVersion")), "13.0.0")
        release_url = "https://github.com/ahonn/markraft/releases/tag/v0.2.0"
        self.assertEqual(item.findtext("link"), release_url)
        self.assertEqual(item.findtext(sparkle("fullReleaseNotesLink")), release_url)
        self.assertIsNone(item.find(sparkle("releaseNotesLink")))
        self.assertEqual(parsedate_to_datetime(item.findtext("pubDate")).utcoffset(), timedelta(0))
        enclosure = item.find("enclosure")
        self.assertEqual(enclosure.attrib[sparkle("edSignature")], self.signature)
        self.assertEqual(int(enclosure.attrib["length"]), self.archive.stat().st_size)
        self.assertEqual(
            enclosure.attrib["url"],
            "https://github.com/ahonn/markraft/releases/download/v0.2.0/"
            "Markraft%20%26%20%22universal%22%20%231.zip",
        )

    def test_custom_minimum_system_version(self):
        root = ET.fromstring(self.generate(minimum_system_version="14.2"))
        self.assertEqual(root.findtext(f"channel/item/{sparkle('minimumSystemVersion')}"), "14.2")

    def test_invalid_versions_are_rejected(self):
        for version in ["v0.2.0", "0.2", "0.2.0-beta.1", "0.2.0+build", "01.2.0", "1.2.3\n", "١.2.3"]:
            with self.subTest(version=version), self.assertRaises(ValueError):
                self.generate(version=version)

    def test_invalid_repositories_are_rejected(self):
        for repository in ["markraft", "https://github.com/ahonn/markraft", "a/b/c", "a/..", "a/.", "a/b?x", "-a/b", "a/b\n", "a/b&c"]:
            with self.subTest(repository=repository), self.assertRaises(ValueError):
                self.generate(repository=repository)

    def test_invalid_signatures_are_rejected(self):
        for signature in ["", "!!!", self.signature + "\n", base64.b64encode(b"short").decode("ascii"), "é"]:
            with self.subTest(signature=signature), self.assertRaises(ValueError):
                self.generate(signature=signature)

    def test_invalid_minimum_system_version_is_rejected(self):
        for version in ["13", "13.beta", "13.0.0\n", "-13.0", "١٣.0"]:
            with self.subTest(version=version), self.assertRaises(ValueError):
                self.generate(minimum_system_version=version)

    def test_missing_empty_and_non_zip_archives_are_rejected(self):
        empty = self.archive.parent / "empty.zip"
        empty.touch()
        other_format = self.archive.parent / "release.dmg"
        other_format.write_bytes(b"archive")
        for archive in [self.archive.parent / "missing.zip", empty, other_format, self.archive.parent]:
            with self.subTest(archive=archive), self.assertRaises(ValueError):
                self.generate(archive=archive)

    def cli_arguments(self, output):
        return [
            "--archive", str(self.archive),
            "--version", "0.2.0",
            "--repository", "ahonn/markraft",
            "--signature", self.signature,
            "--output", str(output),
        ]

    def test_cli_writes_parseable_feed(self):
        output = self.archive.parent / "appcast.xml"
        main(self.cli_arguments(output))
        self.assertEqual(ET.parse(output).findtext(f"channel/item/{sparkle('version')}"), "0.2.0")

    def test_invalid_cli_metadata_preserves_existing_feed(self):
        output = self.archive.parent / "appcast.xml"
        output.write_bytes(b"existing feed")
        arguments = self.cli_arguments(output)
        arguments[arguments.index("--signature") + 1] = "invalid"
        with redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as error:
            main(arguments)
        self.assertEqual(error.exception.code, 2)
        self.assertEqual(output.read_bytes(), b"existing feed")

    def test_cli_refuses_to_overwrite_release_archive(self):
        contents = self.archive.read_bytes()
        with redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            main(self.cli_arguments(self.archive))
        self.assertEqual(self.archive.read_bytes(), contents)


if __name__ == "__main__":
    unittest.main()
