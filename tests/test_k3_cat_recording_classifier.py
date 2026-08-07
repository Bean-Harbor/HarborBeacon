import gc
import importlib.util
import signal
import sys
import tempfile
import types
import unittest
from pathlib import Path
from unittest import mock


REPOSITORY_ROOT = Path(__file__).parents[1]
SCRIPT_PATH = REPOSITORY_ROOT / "scripts" / "harbornavi_k3_cat_recording_classifier.py"


def load_classifier_module():
    spec = importlib.util.spec_from_file_location("k3_cat_recording_classifier", SCRIPT_PATH)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class K3CatRecordingClassifierTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.classifier = load_classifier_module()

    def test_space_runtime_import_order_is_stable(self):
        imports = []
        sentinel = object()

        def fake_import(name):
            imports.append(name)
            return sentinel if name == "onnxruntime" else object()

        runtime = self.classifier.load_spacemit_runtime(fake_import)

        self.assertIs(runtime, sentinel)
        self.assertEqual(imports, ["onnxruntime", "spacemit_ort"])

    def test_ep_worker_affinity_must_match_requested_cluster(self):
        self.classifier.validate_ep_worker_affinity(
            [(101, "0-7"), (102, "12"), (103, "13"), (104, "14"), (105, "15")],
            [12, 13, 14, 15],
        )

        with self.assertRaisesRegex(RuntimeError, "worker affinity mismatch"):
            self.classifier.validate_ep_worker_affinity(
                [(101, "0-7"), (102, "8"), (103, "9"), (104, "10"), (105, "11")],
                [12, 13, 14, 15],
            )

    def test_requested_affinity_rejects_duplicate_cores(self):
        self.assertEqual(
            self.classifier.parse_requested_affinity(4, "12;13;14;15"),
            [12, 13, 14, 15],
        )
        with self.assertRaisesRegex(ValueError, "unique"):
            self.classifier.parse_requested_affinity(4, "12;13;14;14")

    def test_session_enables_global_intra_pool_with_numeric_one(self):
        captured = {}

        class FakeSessionOptions:
            intra_op_num_threads = 0

        class FakeSession:
            def __init__(self, _model, sess_options, providers):
                captured["intra_op_num_threads"] = sess_options.intra_op_num_threads
                captured["providers"] = providers

            def get_providers(self):
                return ["SpaceMITExecutionProvider", "CPUExecutionProvider"]

            def get_inputs(self):
                return [types.SimpleNamespace(name="images", shape=[1, 3, 224, 224])]

            def get_outputs(self):
                return [types.SimpleNamespace(name="scores", shape=[1, 2])]

        runtime = types.SimpleNamespace(
            SessionOptions=FakeSessionOptions,
            InferenceSession=FakeSession,
        )
        with (
            mock.patch.object(self.classifier, "load_spacemit_runtime", return_value=runtime),
            mock.patch.object(
                self.classifier,
                "read_task_affinities",
                return_value=[
                    (101, "0-7"),
                    (102, "12"),
                    (103, "13"),
                    (104, "14"),
                    (105, "15"),
                ],
            ),
        ):
            self.classifier.create_session(Path("model.onnx"), 4, "12;13;14;15")

        options = captured["providers"][0][1]
        self.assertEqual(captured["intra_op_num_threads"], 1)
        self.assertEqual(options["SPACEMIT_EP_USE_GLOBAL_INTRA_THREAD"], "1")

    def test_frame_specs_are_unique_bounded_and_sorted(self):
        frames = self.classifier.parse_frame_specs(
            ["9=/tmp/nine.jpg", "1=/tmp/one.jpg", "5=/tmp/five.jpg"],
            max_frames=9,
        )

        self.assertEqual([frame_index for frame_index, _ in frames], [1, 5, 9])
        with self.assertRaisesRegex(ValueError, "duplicate frame index"):
            self.classifier.parse_frame_specs(
                ["1=/tmp/one.jpg", "1=/tmp/duplicate.jpg"], max_frames=9
            )
        with self.assertRaisesRegex(ValueError, "at most 9 frames"):
            self.classifier.parse_frame_specs(
                [f"{index}=/tmp/{index}.jpg" for index in range(1, 11)],
                max_frames=9,
            )

    def test_three_of_nine_positive_frames_accepts_recording(self):
        predictions = [
            {"frame_index": index, "cat_probability": probability}
            for index, probability in enumerate(
                [0.10, 0.81, 0.12, 0.20, 0.79, 0.11, 0.88, 0.30, 0.22],
                start=1,
            )
        ]

        decision = self.classifier.aggregate_predictions(
            predictions, threshold=0.62, minimum_positive_frames=3
        )

        self.assertTrue(decision["cat_present"])
        self.assertEqual(decision["reason_code"], "cat_visible")
        self.assertEqual(decision["cat_frame_indices"], [2, 5, 7])

    def test_zero_positive_frames_rejects_and_two_require_review(self):
        negative = [
            {"frame_index": index, "cat_probability": 0.10}
            for index in range(1, 10)
        ]
        uncertain = list(negative)
        uncertain[4] = {"frame_index": 5, "cat_probability": 0.90}
        uncertain[7] = {"frame_index": 8, "cat_probability": 0.84}

        rejected = self.classifier.aggregate_predictions(
            negative, threshold=0.62, minimum_positive_frames=3
        )
        review = self.classifier.aggregate_predictions(
            uncertain, threshold=0.62, minimum_positive_frames=3
        )

        self.assertFalse(rejected["cat_present"])
        self.assertEqual(rejected["reason_code"], "no_cat_visible")
        self.assertFalse(review["cat_present"])
        self.assertEqual(review["reason_code"], "uncertain")

    def test_sigterm_stops_future_inference_and_releases_session(self):
        self.assertTrue(
            hasattr(self.classifier, "request_stop"),
            "classifier must expose a SIGTERM handler",
        )
        events = []

        class TrackedSession:
            def __init__(self):
                self._cycle = self

            def run(self, _outputs, _inputs):
                events.append("inference")
                self.classifier.request_stop(signal.SIGTERM, None)
                return [[[0.0, 1.0]]]

            def __del__(self):
                events.append("session_released")

        TrackedSession.classifier = self.classifier

        def create_tracked_session(*_args):
            return TrackedSession(), {
                "input_name": "images",
                "output_name": "scores",
                "session_creation_ms": 1,
            }

        self.classifier.STOP_REQUESTED.clear()
        try:
            with tempfile.TemporaryDirectory() as temporary_directory:
                root = Path(temporary_directory)
                model_path = root / "model.onnx"
                model_path.write_bytes(b"model")
                frames = []
                for frame_index in (1, 2):
                    frame_path = root / f"frame-{frame_index}.jpg"
                    frame_path.write_bytes(b"frame")
                    frames.append(f"{frame_index}={frame_path}")
                args = types.SimpleNamespace(
                    model=model_path,
                    expected_sha256=self.classifier.sha256(model_path),
                    threshold=0.62,
                    ai_threads=4,
                    affinity="12;13;14;15",
                    frame=frames,
                )

                with (
                    mock.patch.object(
                        self.classifier,
                        "create_session",
                        side_effect=create_tracked_session,
                    ),
                    mock.patch.object(
                        self.classifier, "preprocess_image", return_value=object()
                    ),
                ):
                    with self.assertRaises(self.classifier.ClassifierStopRequested):
                        self.classifier.run(args)
        finally:
            self.classifier.STOP_REQUESTED.clear()

        gc.collect()
        self.assertEqual(events, ["inference", "session_released"])

    def test_main_registers_sigterm_and_returns_signal_exit_code(self):
        self.assertTrue(
            hasattr(self.classifier, "request_stop"),
            "classifier must expose a SIGTERM handler",
        )

        with (
            mock.patch.object(self.classifier.signal, "signal") as register_signal,
            mock.patch.object(self.classifier, "parse_args", return_value=object()),
            mock.patch.object(
                self.classifier,
                "run",
                side_effect=self.classifier.ClassifierStopRequested,
            ),
        ):
            exit_code = self.classifier.main()

        register_signal.assert_any_call(signal.SIGTERM, self.classifier.request_stop)
        self.assertEqual(exit_code, 128 + signal.SIGTERM)


if __name__ == "__main__":
    unittest.main()
