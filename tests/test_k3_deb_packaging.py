import unittest
import hashlib
import json
import re
from pathlib import Path


REPOSITORY_ROOT = Path(__file__).parents[1]
BUILD_SCRIPT = REPOSITORY_ROOT / "scripts" / "build_harbornavi_k3_deb.sh"
K3_DEBIAN_DIRECTORY = REPOSITORY_ROOT / "debian" / "n2"


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
            K3_DEBIAN_DIRECTORY / "harboros-beacon.service"
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
        self.assertEqual(
            runtime_contract["video_decision"]["sampling"]["implementation"],
            "/usr/lib/harboros-beacon/cat-sampling-plan",
        )
        self.assertEqual(
            runtime_contract["video_decision"]["sampling"]["maximum_guided_frames"],
            5,
        )
        self.assertIn("--bin cat-sampling-plan", build_script)
        self.assertIn(
            '"$pkg_dir/usr/lib/harboros-beacon/cat-sampling-plan"', build_script
        )
        self.assertIn("ffmpeg", build_script)
        self.assertIn("CAT_RECORDING_CLASSIFIER_MAX_FRAMES: usize = 9", rust_runtime)
        self.assertIn("CAT_RECORDING_CLASSIFIER_MIN_POSITIVE_FRAMES: usize = 3", rust_runtime)
        self.assertIn("HARBOR_K3_CAT_RECORDING_CLASSIFIER_THRESHOLD=0.62", systemd_unit)
        self.assertEqual(runtime_contract["runtime"]["provider"], "SpaceMITExecutionProvider")
        self.assertEqual(runtime_contract["runtime"]["ai_threads"], 4)
        self.assertEqual(runtime_contract["runtime"]["affinity"], "12;13;14;15")

    def test_cat_recording_reconciliation_uses_the_state_directory(self):
        unit = (K3_DEBIAN_DIRECTORY / "harboros-beacon.service").read_text(
            encoding="utf-8"
        )
        self.assertIn(
            "Environment=HARBOR_K3_CAT_RECORDING_RECONCILIATION_PATH="
            "/data/harborbeacon/cat-activity/reconciliation.json",
            unit,
        )

    def test_beacon_systemd_unit_is_packaged_read_only(self):
        script = BUILD_SCRIPT.read_text(encoding="utf-8")
        self.assertIn(
            "install -m 0644 debian/n2/harboros-beacon.service \\\n"
            '  "$pkg_dir/usr/lib/systemd/system/harboros-beacon.service"',
            script,
        )
        self.assertNotIn("$pkg_dir/etc/systemd/system", script)
        self.assertNotIn("semantic-router.service", script)

    def test_beacon_boot_verifies_exact_package_and_pointer_generation(self):
        script = BUILD_SCRIPT.read_text(encoding="utf-8")
        unit = (K3_DEBIAN_DIRECTORY / "harboros-beacon.service").read_text(
            encoding="utf-8"
        )
        verifier = (
            REPOSITORY_ROOT / "debian" / "verify-beacon-k3-generation"
        ).read_text(encoding="utf-8")
        control = (K3_DEBIAN_DIRECTORY / "control").read_text(
            encoding="utf-8"
        )

        self.assertIn(
            "ExecStartPre=+/usr/lib/harborbeacon/verify-k3-generation", unit
        )
        self.assertIn("debian/verify-beacon-k3-generation", script)
        self.assertIn("ffmpeg", control)
        self.assertIn("ffmpeg", script)
        for package in (
            "harboros-beacon",
            "harboros-model-runtime",
            "harboros-cat-vision-runtime",
        ):
            self.assertIn(package, verifier)
        self.assertIn('version="$(package_identity "$package")"', verifier)
        self.assertIn("verify_pointer /data/models", verifier)
        self.assertIn("verify_pointer /data/vision-models", verifier)
        self.assertIn("/usr/lib/harboros-model-runtime/verify-release", verifier)
        self.assertIn("/usr/lib/harboros-cat-vision-runtime/verify-release", verifier)
        self.assertIn("/usr/lib/harboros-cat-vision-runtime/verify-evidence", verifier)

    def test_n2_uses_fixed_runtime_and_isolated_lifecycle(self):
        script = BUILD_SCRIPT.read_text(encoding="utf-8")
        for source in ("debian/n2/postinst", "debian/n2/prerm", "debian/n2/harboros-beacon.service"):
            self.assertIn(source, script)
        self.assertIn("--no-default-features --features fixed-local-models", script)
        self.assertNotIn("--features embedded-model-runtime", script)
        self.assertNotIn("semantic-router.service", script)
        for filename in ("postinst", "prerm"):
            content = (K3_DEBIAN_DIRECTORY / filename).read_text(encoding="utf-8")
            self.assertNotIn("systemctl restart semantic-router.service", content)
        unit = (K3_DEBIAN_DIRECTORY / "harboros-beacon.service").read_text(encoding="utf-8")
        self.assertIn("EnvironmentFile=/etc/default/harboros-fixed-models", unit)
        self.assertIn("ExecStartPre=+/usr/bin/systemctl restart harboros-model-runtime.service", unit)
        self.assertLess(unit.index("verify-k3-generation"), unit.index("systemctl restart harboros-model-runtime.service"))
        runtime = (REPOSITORY_ROOT / "debian" / "harboros-model-runtime.service").read_text(encoding="utf-8")
        self.assertIn("EnvironmentFile=/etc/default/harboros-fixed-models", runtime)
        self.assertIn("KillMode=control-group", runtime)
        self.assertNotIn("CANDLE", runtime)


if __name__ == "__main__":
    unittest.main()
