import base64
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("configure_bundle", Path(__file__).with_name("configure-bundle.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class BundleTests(unittest.TestCase):
    def test_release_uses_same_version_as_appcast_and_signed_native_updates(self):
        key = base64.b64encode(bytes(32)).decode()
        config = module.configure({"CFBundleShortVersionString": "0.2.1", "CFBundleVersion": "timestamp"}, key)
        self.assertEqual(config["CFBundleVersion"], "0.2.1")
        self.assertEqual(config["SUPublicEDKey"], key)
        self.assertEqual(config["SUFeedURL"], "https://github.com/ahonn/markraft/releases/latest/download/appcast.xml")
        self.assertTrue(config["LSUIElement"])
        self.assertTrue(config["SUEnableAutomaticChecks"])
        self.assertFalse(config["SUAllowsAutomaticUpdates"])
        self.assertFalse(config["SUAutomaticallyUpdate"])

    def test_local_bundle_cannot_reuse_stale_public_key(self):
        config = module.configure({"CFBundleShortVersionString": "0.1.0", "SUPublicEDKey": "old"}, "")
        self.assertNotIn("SUPublicEDKey", config)

    def test_invalid_keys_and_nonrelease_versions_are_rejected(self):
        for key in ("not-base64", base64.b64encode(bytes(31)).decode()):
            with self.assertRaises(ValueError):
                module.configure({"CFBundleShortVersionString": "0.1.0"}, key)
        for version in ("0.1.0-beta.1", "v1.0.0", "1.2", "01.0.0"):
            with self.assertRaises(ValueError):
                module.configure({"CFBundleShortVersionString": version}, "")

    def test_mock_bundle_has_separate_identity_and_loopback_feed(self):
        config = module.configure({"CFBundleShortVersionString": "0.1.0"}, "", mock_updates=True)
        self.assertTrue(config["MarkraftMockUpdates"])
        self.assertEqual(config["CFBundleIdentifier"], "dev.markraft.update-test")
        self.assertEqual(config["SUFeedURL"], "http://127.0.0.1:8765/appcast.xml")
        self.assertFalse(config["SUEnableAutomaticChecks"])
        self.assertEqual(config["NSAppTransportSecurity"], {"NSAllowsLocalNetworking": True})


if __name__ == "__main__":
    unittest.main()
