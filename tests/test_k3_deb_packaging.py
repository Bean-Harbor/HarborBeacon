import unittest
from pathlib import Path


REPOSITORY_ROOT = Path(__file__).parents[1]
BUILD_SCRIPT = REPOSITORY_ROOT / "scripts" / "build_harbornavi_k3_deb.sh"


class K3DebPackagingTests(unittest.TestCase):
    def test_all_systemd_units_are_packaged_read_only(self):
        script = BUILD_SCRIPT.read_text(encoding="utf-8")
        unit_copy_start = script.index(
            "sed 's/\\r$//' debian/harboros-beacon.service"
        )
        control_start = script.index("\nsed \\", unit_copy_start)
        unit_section = script[unit_copy_start:control_start]
        mode_block = unit_section[unit_section.index("chmod 0644") :]
        for unit_name in (
            "harboros-beacon.service",
            "semantic-router.service",
        ):
            self.assertIn(
                f'"$pkg_dir/etc/systemd/system/{unit_name}"',
                mode_block,
            )


if __name__ == "__main__":
    unittest.main()
