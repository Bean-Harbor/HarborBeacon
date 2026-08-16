import unittest
import hashlib
import json
import re
from pathlib import Path


REPOSITORY_ROOT = Path(__file__).parents[1]
BUILD_SCRIPT = REPOSITORY_ROOT / "scripts" / "build_harbornavi_k3_deb.sh"


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
        first_party_provenance = json.loads(
            (model_directory / "first-party-provenance.json").read_text(
                encoding="utf-8"
            )
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
        self.assertIn(model_directory.relative_to(REPOSITORY_ROOT).as_posix(), build_script)
        self.assertIn(model_path.name, build_script)
        self.assertEqual(first_party_provenance["artifact"], model_path.name)
        self.assertEqual(first_party_provenance["artifact_sha256"], expected_sha256)
        self.assertEqual(first_party_provenance["rights_holder"], "Harbor Innovations")
        self.assertEqual(
            first_party_provenance["declared_license"],
            "LicenseRef-Harbor-Innovations-Proprietary",
        )
        self.assertIn("first-party-provenance.json", build_script)
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

    def test_beacon_systemd_unit_is_packaged_read_only(self):
        script = BUILD_SCRIPT.read_text(encoding="utf-8")
        self.assertIn(
            "install -m 0644 debian/harboros-beacon.service \\\n"
            '  "$pkg_dir/usr/lib/systemd/system/harboros-beacon.service"',
            script,
        )
        self.assertNotIn("$pkg_dir/etc/systemd/system", script)
        self.assertNotIn("semantic-router.service", script)


if __name__ == "__main__":
    unittest.main()
