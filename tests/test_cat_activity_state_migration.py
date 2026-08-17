import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).parents[1]
MIGRATOR = ROOT / "debian" / "migrate-cat-activity-state"


class CatActivityStateMigrationTests(unittest.TestCase):
    def setUp(self):
        if shutil.which("sh") is None:
            self.skipTest("POSIX sh is unavailable")
        self.temporary_directory = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary_directory.name)
        (self.root / "legacy").mkdir()
        (self.root / "target").mkdir()

    def tearDown(self):
        self.temporary_directory.cleanup()

    def run_migration(self):
        environment = os.environ.copy()
        environment["HARBORBEACON_MIGRATION_TEST_MODE"] = "1"
        environment["HARBORBEACON_MIGRATION_TEST_ROOT"] = str(self.root)
        return subprocess.run(
            ["sh", str(MIGRATOR)],
            env=environment,
            text=True,
            capture_output=True,
            check=False,
        )

    def test_conflicting_old_and_new_state_stops_without_overwrite(self):
        legacy = self.root / "legacy" / "cat-recording-reconciliation.json"
        target = self.root / "target" / "reconciliation.json"
        legacy.write_text('{"generation":"old"}\n', encoding="utf-8")
        target.write_text('{"generation":"new"}\n', encoding="utf-8")

        result = self.run_migration()

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("conflicting cat activity state", result.stderr)
        self.assertEqual(legacy.read_text(encoding="utf-8"), '{"generation":"old"}\n')
        self.assertEqual(target.read_text(encoding="utf-8"), '{"generation":"new"}\n')
        self.assertFalse((self.root / "target" / ".legacy-state-migrated-v1").exists())

    def test_both_legacy_files_migrate_before_marker_is_written(self):
        reconciliation = b'{"camera":"camera-1"}\n'
        validations = b'{"validation":"catval-1"}\n'
        (self.root / "legacy" / "cat-recording-reconciliation.json").write_bytes(
            reconciliation
        )
        (self.root / "legacy" / "cat-recording-validations.jsonl").write_bytes(
            validations
        )

        result = self.run_migration()

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            (self.root / "target" / "reconciliation.json").read_bytes(),
            reconciliation,
        )
        self.assertEqual(
            (self.root / "target" / "validations.jsonl").read_bytes(),
            validations,
        )
        self.assertFalse(
            (self.root / "legacy" / "cat-recording-reconciliation.json").exists()
        )
        self.assertFalse(
            (self.root / "legacy" / "cat-recording-validations.jsonl").exists()
        )
        self.assertTrue((self.root / "target" / ".legacy-state-migrated-v1").exists())

    def test_symlink_state_is_rejected(self):
        outside = self.root / "outside.json"
        outside.write_text('{"outside":true}\n', encoding="utf-8")
        legacy = self.root / "legacy" / "cat-recording-reconciliation.json"
        try:
            legacy.symlink_to(outside)
        except OSError as error:
            self.skipTest(f"symlink creation is unavailable: {error}")

        result = self.run_migration()

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("not a regular file", result.stderr)
        self.assertEqual(outside.read_text(encoding="utf-8"), '{"outside":true}\n')


if __name__ == "__main__":
    unittest.main()
