import unittest
import hashlib
import json
import re
from pathlib import Path


REPOSITORY_ROOT = Path(__file__).parents[1]
BUILD_SCRIPT = REPOSITORY_ROOT / "scripts" / "build_harbornavi_k3_deb.sh"


class K3DebPackagingTests(unittest.TestCase):
    def test_package_detector_model_contract_is_consistent_across_runtime_and_package(self):
        model_directory = (
            REPOSITORY_ROOT
            / "config"
            / "harbornavi-k3"
            / "vision-models"
            / "package-cardboard-v8-320x320-int8-20260826"
        )
        model_path = model_directory / "yolov8n-package-cardboard-v8-320x320.q.onnx"
        labels_path = model_directory / "label.txt"
        rollback_model_path = (
            REPOSITORY_ROOT
            / "config"
            / "harbornavi-k3"
            / "vision-models"
            / "package-roboflow-v1-320x320-fp32"
            / "yolov8n-package-roboflow-v1-320x320.onnx"
        )
        self.assertTrue(
            (model_directory / "runtime-contract.json").is_file(),
            "package detector runtime contract must be packaged",
        )
        self.assertTrue(model_path.is_file(), "package detector model must be packaged")
        self.assertTrue(labels_path.is_file(), "package detector labels must be packaged")
        self.assertTrue(
            rollback_model_path.is_file(),
            "previous package detector model must remain available for rollback",
        )
        runtime_contract = json.loads(
            (model_directory / "runtime-contract.json").read_text(encoding="utf-8")
        )
        actual_sha256 = hashlib.sha256(model_path.read_bytes()).hexdigest()
        systemd_unit = (
            REPOSITORY_ROOT / "debian" / "harboros-beacon.service"
        ).read_text(encoding="utf-8")
        build_script = BUILD_SCRIPT.read_text(encoding="utf-8")

        self.assertEqual(
            actual_sha256,
            "0bfb59702f7968fb6c6c7d61e41876b0d3caafdb9533ff08d476e3874091d158",
        )
        self.assertEqual(runtime_contract["model_sha256"], actual_sha256)
        self.assertEqual(labels_path.read_text(encoding="utf-8").strip(), "package")
        self.assertEqual(runtime_contract["runtime"]["provider"], "SpaceMITExecutionProvider")
        self.assertEqual(runtime_contract["model"]["precision"], "int8")
        self.assertIn(
            "Environment=HARBOR_K3_PACKAGE_YOLO_MODEL="
            "/var/lib/harboros-beacon/vision-models/"
            "package-cardboard-v8-320x320-int8-20260826/"
            "yolov8n-package-cardboard-v8-320x320.q.onnx",
            systemd_unit,
        )
        self.assertIn(
            "Environment=HARBOR_K3_PACKAGE_YOLO_LABELS="
            "/var/lib/harboros-beacon/vision-models/"
            "package-cardboard-v8-320x320-int8-20260826/label.txt",
            systemd_unit,
        )
        self.assertIn(model_path.relative_to(REPOSITORY_ROOT).as_posix(), build_script)
        self.assertIn(labels_path.relative_to(REPOSITORY_ROOT).as_posix(), build_script)
        self.assertIn(
            rollback_model_path.relative_to(REPOSITORY_ROOT).as_posix(), build_script
        )
        self.assertIn("package_yolo_model_sha256=" + actual_sha256, build_script)

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
        self.assertIn(model_path.relative_to(REPOSITORY_ROOT).as_posix(), build_script)
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

    def test_cat_detection_control_uses_the_state_directory(self):
        unit = (REPOSITORY_ROOT / "debian" / "harboros-beacon.service").read_text(
            encoding="utf-8"
        )
        self.assertIn(
            "Environment=HARBOR_K3_CAT_DETECTION_CONTROL_PATH="
            "/var/lib/harboros-beacon/cat-detection-controls.json",
            unit,
        )
        self.assertIn(
            "Environment=HARBOR_K3_PACKAGE_DETECTION_CONTROL_PATH="
            "/var/lib/harboros-beacon/package-detection-controls.json",
            unit,
        )
        self.assertIn(
            "Environment=HARBOR_K3_PACKAGE_EVENT_STORE_PATH="
            "/var/lib/harboros-beacon/package-events.json",
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


if __name__ == "__main__":
    unittest.main()
