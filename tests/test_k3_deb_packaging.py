import unittest
import hashlib
import json
import re
from pathlib import Path


REPOSITORY_ROOT = Path(__file__).parents[1]
BUILD_SCRIPT = REPOSITORY_ROOT / "scripts" / "build_harbornavi_k3_deb.sh"
K3_DEBIAN_DIRECTORY = REPOSITORY_ROOT / "debian" / "harbornavi-k3"


class K3DebPackagingTests(unittest.TestCase):
    def test_classifier_model_contract_is_consistent_across_runtime_and_package(self):
        model_directory = (
            REPOSITORY_ROOT
            / "config"
            / "harbornavi-k3"
            / "vision-models"
            / "mobilenetv2-cat-binary-v2-20260806"
        )
        model_path = model_directory / "mobilenetv2_cat_binary_int8.onnx"
        runtime_contract = json.loads(
            (model_directory / "runtime-contract.json").read_text(encoding="utf-8")
        )
        actual_sha256 = hashlib.sha256(model_path.read_bytes()).hexdigest()
        rust_runtime = (
            REPOSITORY_ROOT / "src" / "runtime" / "cat_recording_classifier.rs"
        ).read_text(encoding="utf-8")
        systemd_unit = (
            REPOSITORY_ROOT / "debian" / "harboros-beacon.service"
        ).read_text(encoding="utf-8")
        build_script = BUILD_SCRIPT.read_text(encoding="utf-8")

        rust_sha256 = re.search(
            r'CAT_RECORDING_CLASSIFIER_MODEL_SHA256: &str =\s*"([0-9a-f]{64})"',
            rust_runtime,
        )
        self.assertIsNotNone(rust_sha256)
        expected_sha256 = runtime_contract["model_sha256"]
        self.assertEqual(actual_sha256, expected_sha256)
        self.assertEqual(rust_sha256.group(1), expected_sha256)
        self.assertIn(
            f"HARBOR_K3_CAT_RECORDING_CLASSIFIER_MODEL_SHA256={expected_sha256}",
            systemd_unit,
        )
        self.assertIn(f"cat_recording_classifier_sha256={expected_sha256}", build_script)
        self.assertIn(str(model_path.relative_to(REPOSITORY_ROOT)), build_script)
        self.assertEqual(runtime_contract["video_decision"]["maximum_frames"], 9)
        self.assertEqual(runtime_contract["video_decision"]["minimum_positive_frames"], 3)
        self.assertIn("CAT_RECORDING_CLASSIFIER_MAX_FRAMES: usize = 9", rust_runtime)
        self.assertIn("CAT_RECORDING_CLASSIFIER_MIN_POSITIVE_FRAMES: usize = 3", rust_runtime)
        self.assertIn("HARBOR_K3_CAT_RECORDING_CLASSIFIER_THRESHOLD=0.62", systemd_unit)
        self.assertEqual(runtime_contract["runtime"]["provider"], "SpaceMITExecutionProvider")
        self.assertEqual(runtime_contract["runtime"]["ai_threads"], 4)
        self.assertEqual(runtime_contract["runtime"]["affinity"], "12;13;14;15")

    def test_cat_recording_reconciliation_uses_the_state_directory(self):
        unit = (REPOSITORY_ROOT / "debian" / "harboros-beacon.service").read_text(
            encoding="utf-8"
        )
        self.assertIn(
            "Environment=HARBOR_K3_CAT_RECORDING_RECONCILIATION_PATH="
            "/var/lib/harboros-beacon/cat-recording-reconciliation.json",
            unit,
        )

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

        self.assertIn(
            "debian/harbornavi-k3/semantic-router.service",
            unit_section,
            "K3 must package its standalone unit from the isolated K3 lane",
        )

    def test_k3_uses_isolated_standalone_router_lifecycle(self):
        script = BUILD_SCRIPT.read_text(encoding="utf-8")
        expected_sources = (
            "debian/harbornavi-k3/postinst",
            "debian/harbornavi-k3/prerm",
            "debian/harbornavi-k3/semantic-router.service",
        )
        for source in expected_sources:
            with self.subTest(source=source):
                self.assertIn(source, script)

        self.assertNotRegex(
            script,
            re.compile(r"sed 's/\\r\$//' debian/(?:postinst|prerm)(?:\s|>)"),
            "K3 must not inherit the formal AMD64 maintainer scripts",
        )
        self.assertNotIn(
            "sed 's/\\r$//' debian/semantic-router.service",
            script,
            "K3 must not inherit a top-level standalone unit",
        )

        k3_postinst = (K3_DEBIAN_DIRECTORY / "postinst").read_text(
            encoding="utf-8"
        )
        k3_prerm = (K3_DEBIAN_DIRECTORY / "prerm").read_text(encoding="utf-8")
        self.assertRegex(
            k3_postinst,
            re.compile(
                r'HARBOR_SEMANTIC_ROUTER_TOPOLOGY"\s+"standalone"'
            ),
            "the K3 package must explicitly select its standalone topology",
        )
        self.assertIn("systemctl enable semantic-router.service", k3_postinst)
        self.assertIn("systemctl restart semantic-router.service", k3_postinst)
        self.assertIn("systemctl stop semantic-router.service", k3_prerm)
        self.assertIn("systemctl disable semantic-router.service", k3_prerm)


if __name__ == "__main__":
    unittest.main()
