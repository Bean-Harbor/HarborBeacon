import hashlib
import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch


REPOSITORY_ROOT = Path(__file__).parents[1]
SCRIPT_PATH = REPOSITORY_ROOT / "scripts" / "harbornavi_k3_cat_quality_runner.py"


def load_runner_module():
    spec = importlib.util.spec_from_file_location("k3_cat_quality_runner", SCRIPT_PATH)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class K3CatQualityRunnerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.runner = load_runner_module()

    def manifest_payload(self, video_path: Path, **overrides):
        video_sha256 = hashlib.sha256(video_path.read_bytes()).hexdigest()
        clip = {
            "clip_id": "clip-0001",
            "camera_id": "harborlink-camera-1",
            "video_path": str(video_path.resolve()),
            "video_sha256": video_sha256,
            "expected_cat": True,
            "low_light": False,
            "hard_negative_kind": None,
            "recording_started_at_epoch_ms": 100_000,
            "recording_ended_at_epoch_ms": 110_000,
            "detection_evidence": [
                {"sequence": 1, "frame_epoch_ms": 101_000, "confidence_ppm": 800_000},
                {"sequence": 2, "frame_epoch_ms": 105_000, "confidence_ppm": 950_000},
                {"sequence": 3, "frame_epoch_ms": 109_000, "confidence_ppm": 700_000},
            ],
        }
        clip.update(overrides.pop("clip_overrides", {}))
        manifest = {
            "schema_version": 1,
            "dataset_id": "signed-holdout-20260817",
            "sampler": {
                "installed_path": str(self.runner.SAMPLER),
                "sha256": "1" * 64,
            },
            "detector": {
                "evidence_schema": self.runner.DETECTION_EVIDENCE_SCHEMA,
                "projection_contract": self.runner.DETECTION_EVIDENCE_PROJECTION,
                "model_id": self.runner.DETECTOR_MODEL_ID,
                "model_installed_path": self.runner.DETECTOR_MODEL_PATH,
                "model_revision": self.runner.DETECTOR_MODEL_REVISION,
                "model_sha256": self.runner.DETECTOR_MODEL_SHA256,
            },
            "clips": [clip],
        }
        manifest.update(overrides)
        return json.dumps(manifest, separators=(",", ":")).encode("ascii")

    def classifier_output(self, positive_indices):
        predictions = []
        for frame_index in range(1, 10):
            predictions.append(
                {
                    "frame_index": frame_index,
                    "cat_probability_ppm": (
                        800_000 if frame_index in positive_indices else 100_000
                    ),
                    "cat_probability": (
                        0.8 if frame_index in positive_indices else 0.1
                    ),
                    "inference_ms": frame_index,
                }
            )
        return {
            "schema_version": "1.0",
            "status": "ok",
            "provider": self.runner.PROVIDER,
            "model_name": self.runner.MODEL_NAME,
            "model_sha256": self.runner.MODEL_SHA256,
            "threshold_ppm": self.runner.THRESHOLD_PPM,
            "sampled_frame_count": self.runner.MAX_FRAMES,
            "predictions": predictions,
        }

    def test_manifest_is_strict_and_binds_video_bytes(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            video_path = Path(temporary_directory) / "clip.mp4"
            video_path.write_bytes(b"signed holdout clip bytes")

            manifest = self.runner.parse_manifest(self.manifest_payload(video_path))

            self.assertEqual(manifest["schema_version"], 1)
            self.assertEqual(manifest["dataset_id"], "signed-holdout-20260817")
            self.assertEqual(manifest["clips"][0]["video_path"], video_path.resolve())
            self.assertEqual(manifest["sampler"]["sha256"], "1" * 64)
            self.assertFalse(manifest["clips"][0]["low_light"])
            self.assertIsNone(manifest["clips"][0]["hard_negative_kind"])

            with self.assertRaisesRegex(ValueError, "video SHA256 mismatch"):
                self.runner.parse_manifest(
                    self.manifest_payload(
                        video_path,
                        clip_overrides={"video_sha256": "0" * 64},
                    )
                )

    def test_manifest_binds_low_light_and_canonical_hard_negative_annotations(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            video_path = Path(temporary_directory) / "clip.mp4"
            video_path.write_bytes(b"signed negative clip")

            manifest = self.runner.parse_manifest(
                self.manifest_payload(
                    video_path,
                    clip_overrides={
                        "expected_cat": False,
                        "low_light": True,
                        "hard_negative_kind": "other-animal",
                    },
                )
            )
            clip = manifest["clips"][0]
            self.assertTrue(clip["low_light"])
            self.assertEqual(clip["hard_negative_kind"], "other-animal")

            with self.assertRaisesRegex(ValueError, "positive clips"):
                self.runner.parse_manifest(
                    self.manifest_payload(
                        video_path,
                        clip_overrides={"hard_negative_kind": "person"},
                    )
                )
            with self.assertRaisesRegex(ValueError, "hard_negative_kind"):
                self.runner.parse_manifest(
                    self.manifest_payload(
                        video_path,
                        clip_overrides={
                            "expected_cat": False,
                            "hard_negative_kind": "other_animal",
                        },
                    )
                )

    def test_manifest_rejects_unknown_fields_and_duplicate_clip_ids(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            video_path = Path(temporary_directory) / "clip.mp4"
            video_path.write_bytes(b"clip")
            with self.assertRaisesRegex(ValueError, "manifest fields"):
                self.runner.parse_manifest(
                    self.manifest_payload(video_path, untrusted_policy={"threshold": 0.1})
                )

            payload = json.loads(self.manifest_payload(video_path))
            payload["clips"].append(dict(payload["clips"][0]))
            with self.assertRaisesRegex(ValueError, "duplicate clip_id"):
                self.runner.parse_manifest(json.dumps(payload).encode("ascii"))

    def test_sampling_uses_the_installed_production_plan_and_binds_evidence(self):
        clip = {
            "recording_started_at_epoch_ms": 100_000,
            "recording_ended_at_epoch_ms": 110_000,
            "detection_evidence": [
                {"sequence": 1, "frame_epoch_ms": 101_000, "confidence_ppm": 800_000}
            ],
        }
        response = {
            "schema_version": 1,
            "strategy": "yolo_guided_hybrid_9",
            "duration_ms": 10_000,
            "eligible_detection_evidence_count": 1,
            "sample_offsets_ms": [
                1_000,
                5_000,
                9_000,
                3_000,
                7_000,
                2_000,
                8_000,
                4_000,
                6_000,
            ],
        }
        completed = SimpleNamespace(
            returncode=0,
            stdout=json.dumps(response).encode("ascii"),
            stderr=b"",
        )
        with patch.object(self.runner.subprocess, "run", return_value=completed) as run:
            plan = self.runner.run_production_sampler(
                Path("/private/cat-sampling-plan"), clip, 10_000
            )

        self.assertEqual(plan, response)
        sent = json.loads(run.call_args.kwargs["input"])
        self.assertEqual(sent["detection_evidence"], clip["detection_evidence"])
        self.assertEqual(sent["recording_started_at_epoch_ms"], 100_000)

        self.assertEqual(self.runner.duration_ms_from_seconds(10.0005), 10_001)
        with self.assertRaisesRegex(ValueError, "5000..600000"):
            self.runner.duration_ms_from_seconds(4.99)
        with self.assertRaisesRegex(ValueError, "5000..600000"):
            self.runner.duration_ms_from_seconds(600.01)

    def test_manifest_rejects_detector_drift_and_malformed_detection_evidence(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            video_path = Path(temporary_directory) / "clip.mp4"
            video_path.write_bytes(b"clip")
            detector_drift = json.loads(self.manifest_payload(video_path))
            detector_drift["detector"]["model_sha256"] = "0" * 64
            with self.assertRaisesRegex(ValueError, "detector identity"):
                self.runner.parse_manifest(json.dumps(detector_drift).encode("ascii"))

            duplicate_evidence = json.loads(self.manifest_payload(video_path))
            duplicate_evidence["clips"][0]["detection_evidence"].append(
                dict(duplicate_evidence["clips"][0]["detection_evidence"][0])
            )
            with self.assertRaisesRegex(ValueError, "sequences must be unique"):
                self.runner.parse_manifest(json.dumps(duplicate_evidence).encode("ascii"))

    def test_frozen_bytes_survive_source_replace_and_mutation_fails_closed(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory).resolve()
            source = root / "source.bin"
            source.write_bytes(b"signed bytes A")
            expected_sha256 = hashlib.sha256(source.read_bytes()).hexdigest()
            frozen, frozen_sha256, frozen_size = self.runner.freeze_regular_file(
                source,
                root / "frozen.bin",
                expected_sha256=expected_sha256,
            )

            replacement = root / "replacement.bin"
            replacement.write_bytes(b"different bytes B")
            replacement.replace(source)

            self.assertEqual(frozen.read_bytes(), b"signed bytes A")
            self.assertEqual(frozen_sha256, expected_sha256)
            self.assertEqual(frozen_size, len(b"signed bytes A"))

            with self.assertRaisesRegex(ValueError, "frozen file SHA256 mismatch"):
                self.runner.freeze_regular_file(
                    source,
                    root / "rejected.bin",
                    expected_sha256=expected_sha256,
                )
            self.assertFalse((root / "rejected.bin").exists())

    def test_classifier_contract_accepts_exact_three_of_nine(self):
        payload = json.dumps(self.classifier_output({2, 5, 7})).encode("ascii")

        decision = self.runner.validate_classifier_output(payload, list(range(1, 10)))

        self.assertTrue(decision["predicted_cat"])
        self.assertEqual(decision["positive_frame_indices"], [2, 5, 7])
        self.assertEqual(decision["reason_code"], "cat_visible")

    def test_classifier_contract_rejects_two_of_nine_and_provider_drift(self):
        payload = json.dumps(self.classifier_output({3, 8})).encode("ascii")
        decision = self.runner.validate_classifier_output(payload, list(range(1, 10)))
        self.assertFalse(decision["predicted_cat"])
        self.assertEqual(decision["reason_code"], "uncertain")

        drifted = self.classifier_output({3, 8, 9})
        drifted["provider"] = "CPUExecutionProvider"
        with self.assertRaisesRegex(ValueError, "EVT.1 runtime contract"):
            self.runner.validate_classifier_output(
                json.dumps(drifted).encode("ascii"), list(range(1, 10))
            )

        malformed_prediction = self.classifier_output({1, 2, 3})
        malformed_prediction["predictions"][0]["untrusted"] = True
        with self.assertRaisesRegex(ValueError, "frame contract mismatch"):
            self.runner.validate_classifier_output(
                json.dumps(malformed_prediction).encode("ascii"), list(range(1, 10))
            )

    def test_runner_paths_and_policy_are_not_environment_configurable(self):
        self.assertEqual(
            self.runner.CLASSIFIER,
            Path("/usr/lib/harboros-beacon/harbornavi_k3_cat_recording_classifier.py"),
        )
        self.assertEqual(
            self.runner.MODEL,
            Path(
                "/usr/share/harboros-beacon/vision-models/"
                "mobilenetv2-cat-binary-v2-20260806/"
                "mobilenetv2_cat_binary_int8.onnx"
            ),
        )
        self.assertEqual(
            self.runner.SAMPLER,
            Path("/usr/lib/harboros-beacon/cat-sampling-plan"),
        )
        self.assertEqual(self.runner.PROVIDER, "SpaceMITExecutionProvider")
        self.assertEqual(self.runner.THRESHOLD_PPM, 620_000)
        self.assertEqual(self.runner.MAX_FRAMES, 9)
        self.assertEqual(self.runner.MINIMUM_POSITIVE_FRAMES, 3)


if __name__ == "__main__":
    unittest.main()
