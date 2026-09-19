"""Checks for the isolated local Sparkle feed."""

import base64
import importlib.util
import functools
import http.client
import http.server
import threading
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import xml.etree.ElementTree as ET

from appcast import sparkle

SPEC = importlib.util.spec_from_file_location("mock_updates", Path(__file__).with_name("mock-updates.py"))
mock_updates = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(mock_updates)


class LocalFeedTests(unittest.TestCase):
    def test_feed_points_entirely_to_loopback_and_keeps_signature_and_size(self):
        with tempfile.TemporaryDirectory() as directory:
            archive = Path(directory) / "Markraft-Update-Test-0.1.1.zip"
            archive.write_bytes(b"fixture archive")
            signature = base64.b64encode(bytes(range(64))).decode("ascii")
            document = mock_updates.local_appcast(archive, "0.1.1", signature, 8765)
            root = ET.fromstring(document)
            self.assertNotIn(b"github.com", document)
            self.assertEqual(root.find("channel/link").text, "http://127.0.0.1:8765")
            item = root.find("channel/item")
            self.assertEqual(item.find(sparkle("version")).text, "0.1.1")
            self.assertEqual(item.find(sparkle("fullReleaseNotesLink")).text,
                             "http://127.0.0.1:8765/release-notes.html")
            enclosure = item.find("enclosure")
            self.assertEqual(enclosure.get("url"), "http://127.0.0.1:8765/" + archive.name)
            self.assertEqual(enclosure.get(sparkle("edSignature")), signature)
            self.assertEqual(int(enclosure.get("length")), archive.stat().st_size)

    def test_server_switches_live_scenarios_and_does_not_expose_private_keys(self):
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            public = work / "public"
            public.mkdir()
            (work / "private-key.bin").write_bytes(b"secret")
            for scenario in mock_updates.SCENARIOS:
                (public / f"{scenario}.xml").write_text(scenario)
            handler = functools.partial(mock_updates.Handler, directory=str(public))
            with patch.object(mock_updates, "WORK", work):
                with http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler) as server:
                    thread = threading.Thread(target=server.serve_forever, daemon=True)
                    thread.start()
                    connection = http.client.HTTPConnection(*server.server_address)
                    try:
                        for scenario in mock_updates.SCENARIOS:
                            (work / "scenario.txt").write_text(scenario)
                            connection.request("GET", "/appcast.xml?cache=ignored")
                            response = connection.getresponse()
                            self.assertEqual(response.status, 200)
                            self.assertEqual(response.getheader("Cache-Control"), "no-store")
                            self.assertEqual(response.read().decode(), scenario)
                        connection.request("GET", "/../private-key.bin")
                        response = connection.getresponse()
                        self.assertEqual(response.status, 404)
                        self.assertNotIn(b"secret", response.read())
                        (work / "scenario.txt").write_text("unknown")
                        connection.request("GET", "/appcast.xml")
                        response = connection.getresponse()
                        self.assertEqual(response.status, 500)
                        response.read()
                    finally:
                        connection.close()
                        server.shutdown()
                        thread.join()
            self.assertIn("GET", (work / "access.log").read_text())


if __name__ == "__main__":
    unittest.main()
