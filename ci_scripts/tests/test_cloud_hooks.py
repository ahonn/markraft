"""Check hook failure paths without installing tools or running a build."""

import os
from pathlib import Path
import subprocess
import unittest


ROOT = Path(__file__).resolve().parents[2]


class HookTests(unittest.TestCase):
    def invoke(self, name, **variables):
        environment = {key: value for key, value in os.environ.items() if not key.startswith("CI_")}
        environment.update(variables)
        return subprocess.run(
            ["bash", str(ROOT / "ci_scripts" / name)],
            env=environment, capture_output=True, text=True,
        )

    def test_hooks_reject_accidental_local_invocation(self):
        for hook in ("ci_post_clone.sh", "ci_pre_xcodebuild.sh", "ci_post_xcodebuild.sh"):
            with self.subTest(hook=hook):
                result = self.invoke(hook)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("only in", result.stderr)

    def test_invalid_build_numbers_stop_before_tool_installation(self):
        for number in ("0", "-1", "01", "1.2", "42\nOTHER_SETTING=1"):
            with self.subTest(number=number):
                result = self.invoke(
                    "ci_pre_xcodebuild.sh", CI_XCODE_CLOUD="TRUE",
                    CI_PRODUCT_PLATFORM="macOS", CI_BUILD_NUMBER=number,
                    CI_PRIMARY_REPOSITORY_PATH=str(ROOT),
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("positive integer", result.stderr)

    def test_invalid_tags_stop_both_checkout_hooks_before_tool_installation(self):
        for hook in ("ci_post_clone.sh", "ci_pre_xcodebuild.sh"):
            for tag in ("v0.1.7-rc.1", "v999.0.0", "v0.1.7;exit 0"):
                with self.subTest(hook=hook, tag=tag):
                    result = self.invoke(
                        hook, CI_XCODE_CLOUD="TRUE", CI_PRODUCT_PLATFORM="macOS",
                        CI_BUILD_NUMBER="3", CI_PRIMARY_REPOSITORY_PATH=str(ROOT), CI_TAG=tag,
                    )
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("Release tag validation failed", result.stderr)

    def test_wrong_platform_stops_before_tool_installation(self):
        result = self.invoke("ci_pre_xcodebuild.sh", CI_XCODE_CLOUD="TRUE", CI_PRODUCT_PLATFORM="iOS")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("macOS", result.stderr)

    def test_post_hook_preserves_failed_build_and_skips_non_archive_actions(self):
        for action, status in (("build", "0"), ("archive", "65")):
            with self.subTest(action=action, status=status):
                result = self.invoke(
                    "ci_post_xcodebuild.sh", CI_XCODE_CLOUD="TRUE",
                    CI_XCODEBUILD_ACTION=action, CI_XCODEBUILD_EXIT_CODE=status,
                )
                self.assertEqual(result.returncode, 0)
                self.assertIn("Skipping archive validation", result.stdout)

    def test_successful_archive_requires_artifact_path(self):
        result = self.invoke(
            "ci_post_xcodebuild.sh", CI_XCODE_CLOUD="TRUE",
            CI_XCODEBUILD_ACTION="archive", CI_XCODEBUILD_EXIT_CODE="0",
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("archive path", result.stderr)


if __name__ == "__main__":
    unittest.main()
