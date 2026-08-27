import gc
import importlib.util
import os
import sys
import tempfile
import types
import unittest
from pathlib import Path
from unittest import mock


def load_worker_module():
    cv2 = types.ModuleType("cv2")
    cv2.VideoCapture = object
    cv2.CAP_FFMPEG = 0
    cv2.CAP_PROP_BUFFERSIZE = 0
    cv2.CAP_PROP_FPS = 0
    cv2.FONT_HERSHEY_SIMPLEX = 0
    cv2.LINE_AA = 0
    cv2.rectangle = mock.Mock()
    cv2.putText = mock.Mock()
    numpy = types.ModuleType("numpy")
    numpy.ndarray = object
    onnxruntime = types.ModuleType("onnxruntime")

    class SessionOptions:
        pass

    class InferenceSession:
        def __init__(self, _model, sess_options, providers):
            onnxruntime.last_session_options = sess_options
            onnxruntime.last_providers = providers
            self._providers = [
                provider[0] if isinstance(provider, tuple) else provider
                for provider in providers
            ]

        def get_inputs(self):
            return [types.SimpleNamespace(shape=[1, 3, 192, 320])]

        def get_providers(self):
            return self._providers

    onnxruntime.SessionOptions = SessionOptions
    onnxruntime.InferenceSession = InferenceSession
    analyzer = types.ModuleType("harbornavi_k3_yolov8_analyzer")
    analyzer.input_hw = lambda _shape: (192, 320)
    analyzer.letterbox_pad_value = lambda output_count: 114 if output_count == 1 else 0
    analyzer.load_labels = lambda _path: ["cat"]
    analyzer.postprocess = lambda *_args, **_kwargs: []
    analyzer.preprocess = lambda *_args, **_kwargs: (None, None)
    analyzer.provider_list = lambda provider: [
        "SpaceMITExecutionProvider"
        if provider == "spacemit"
        else "CPUExecutionProvider"
    ]
    modules = {
        "cv2": cv2,
        "numpy": numpy,
        "onnxruntime": onnxruntime,
        "harbornavi_k3_yolov8_analyzer": analyzer,
    }
    previous = {name: sys.modules.get(name) for name in modules}
    sys.modules.update(modules)
    try:
        path = Path(__file__).parents[1] / "scripts" / "harbornavi_k3_yolo_stream_worker.py"
        spec = importlib.util.spec_from_file_location("k3_yolo_stream_worker", path)
        module = importlib.util.module_from_spec(spec)
        assert spec.loader is not None
        spec.loader.exec_module(module)
        return module
    finally:
        for name, value in previous.items():
            if value is None:
                sys.modules.pop(name, None)
            else:
                sys.modules[name] = value


class K3YoloStreamWorkerTests(unittest.TestCase):
    def test_confidence_threshold_uses_environment_override(self):
        worker = load_worker_module()

        with mock.patch.dict(
            os.environ,
            {"HARBOR_K3_YOLO_CONFIDENCE_OVERRIDE": "0.45"},
            clear=True,
        ):
            threshold = worker.confidence_threshold_from_env(0.35)

        self.assertEqual(threshold, 0.45)

    def test_confidence_threshold_uses_command_line_value_without_override(self):
        worker = load_worker_module()

        with mock.patch.dict(os.environ, {}, clear=True):
            threshold = worker.confidence_threshold_from_env(0.35)

        self.assertEqual(threshold, 0.35)

    def test_confidence_threshold_rejects_invalid_environment_override(self):
        worker = load_worker_module()

        for value in ("invalid", "0", "1.01"):
            with self.subTest(value=value):
                with mock.patch.dict(
                    os.environ,
                    {"HARBOR_K3_YOLO_CONFIDENCE_OVERRIDE": value},
                    clear=True,
                ):
                    with self.assertRaisesRegex(
                        ValueError, "HARBOR_K3_YOLO_CONFIDENCE_OVERRIDE"
                    ):
                        worker.confidence_threshold_from_env(0.35)

    def test_build_session_uses_configured_cpu_threads(self):
        worker = load_worker_module()
        args = types.SimpleNamespace(
            labels="labels.txt",
            target_label="cat",
            provider="cpu",
            model="model.onnx",
        )

        with mock.patch.dict(os.environ, {"HARBOR_K3_YOLO_CPU_THREADS": "4"}):
            worker.build_session(args)

        self.assertEqual(worker.ort.last_session_options.intra_op_num_threads, 4)

    def test_build_session_rejects_non_positive_cpu_threads(self):
        worker = load_worker_module()
        args = types.SimpleNamespace(
            labels="labels.txt",
            target_label="cat",
            provider="cpu",
            model="model.onnx",
        )

        with mock.patch.dict(os.environ, {"HARBOR_K3_YOLO_CPU_THREADS": "0"}):
            with self.assertRaisesRegex(ValueError, "must be positive"):
                worker.build_session(args)

    def test_build_session_defaults_to_one_cpu_thread(self):
        worker = load_worker_module()
        args = types.SimpleNamespace(
            labels="labels.txt",
            target_label="cat",
            provider="cpu",
            model="model.onnx",
        )

        with mock.patch.dict(os.environ, {}, clear=True):
            worker.build_session(args)

        self.assertEqual(worker.ort.last_session_options.intra_op_num_threads, 1)

    def test_build_session_configures_spacemit_core_affinity(self):
        worker = load_worker_module()
        args = types.SimpleNamespace(
            labels="labels.txt",
            target_label="cat",
            provider="spacemit",
            model="model.onnx",
        )

        environment = {
            "HARBOR_K3_YOLO_AI_THREADS": "4",
            "HARBOR_K3_YOLO_AI_AFFINITY": "8;9;10;11",
            "HARBOR_K3_YOLO_CPU_THREADS": "8",
        }
        with mock.patch.dict(os.environ, environment, clear=True):
            session, _, _, _, provider = worker.build_session(args)

        self.assertEqual(worker.ort.last_session_options.intra_op_num_threads, 1)
        self.assertEqual(
            worker.ort.last_providers,
            [
                (
                    "SpaceMITExecutionProvider",
                    {
                        "SPACEMIT_EP_INTRA_THREAD_NUM": "4",
                        "SPACEMIT_EP_INTRA_THREAD_AFFINITY": "8;9;10;11",
                        "SPACEMIT_EP_INTER_THREAD_NUM": "1",
                    },
                )
            ],
        )
        self.assertEqual(session.get_providers(), ["SpaceMITExecutionProvider"])
        self.assertEqual(provider, "SpaceMITExecutionProvider")

    def test_build_session_rejects_spacemit_affinity_count_mismatch(self):
        worker = load_worker_module()
        args = types.SimpleNamespace(
            labels="labels.txt",
            target_label="cat",
            provider="spacemit",
            model="model.onnx",
        )

        environment = {
            "HARBOR_K3_YOLO_AI_THREADS": "4",
            "HARBOR_K3_YOLO_AI_AFFINITY": "8;9",
        }
        with mock.patch.dict(os.environ, environment, clear=True):
            with self.assertRaisesRegex(ValueError, "must contain 4 core IDs"):
                worker.build_session(args)

    def test_filter_target_detections_keeps_only_cat(self):
        worker = load_worker_module()
        detections = [
            {"label": "person", "confidence": 0.9},
            {"label": "Cat", "confidence": 0.8},
            {"label": "dog", "confidence": 0.7},
        ]

        self.assertEqual(
            worker.filter_target_detections(detections, "cat"), [detections[1]]
        )

    def test_source_kind_never_returns_the_source_value(self):
        worker = load_worker_module()

        self.assertEqual(
            worker.source_kind("rtsp://user:secret@camera.local/stream"), "rtsp"
        )
        self.assertEqual(worker.source_kind("/data/cat.mp4"), "file")

    def test_snapshot_writes_are_cat_only_and_rate_limited(self):
        worker = load_worker_module()

        self.assertFalse(worker.should_write_snapshot([], 10.0, 0.0))
        self.assertFalse(
            worker.should_write_snapshot([{"label": "cat"}], 10.5, 10.0)
        )
        self.assertTrue(
            worker.should_write_snapshot([{"label": "cat"}], 11.0, 10.0)
        )

    def test_annotate_uses_the_detection_label(self):
        worker = load_worker_module()
        image = mock.Mock()
        image.copy.return_value = "annotated"

        annotated = worker.annotate(
            image,
            [
                {
                    "label": "package",
                    "confidence": 0.88,
                    "x1": 10,
                    "y1": 20,
                    "x2": 110,
                    "y2": 220,
                }
            ],
        )

        self.assertEqual(annotated, "annotated")
        self.assertEqual(worker.cv2.putText.call_args.args[1], "package 0.88")

    def test_consecutive_detection_state_resets_the_opposite_streak(self):
        worker = load_worker_module()
        state = worker.ConsecutiveDetectionState()

        self.assertEqual(
            state.observe(True, 1000),
            {
                "consecutive_present_frames": 1,
                "consecutive_absent_frames": 0,
                "present_since_epoch_ms": 1000,
                "absent_since_epoch_ms": 0,
            },
        )
        self.assertEqual(state.observe(True, 1100)["consecutive_present_frames"], 2)
        self.assertEqual(
            state.observe(False, 1200),
            {
                "consecutive_present_frames": 0,
                "consecutive_absent_frames": 1,
                "present_since_epoch_ms": 0,
                "absent_since_epoch_ms": 1200,
            },
        )
        self.assertEqual(state.observe(False, 1300)["consecutive_absent_frames"], 2)
        self.assertEqual(
            state.observe(True, 1400),
            {
                "consecutive_present_frames": 1,
                "consecutive_absent_frames": 0,
                "present_since_epoch_ms": 1400,
                "absent_since_epoch_ms": 0,
            },
        )

    def test_stopped_reader_does_not_return_a_stale_frame(self):
        worker = load_worker_module()
        reader = worker.LatestFrameReader("/data/cat.mp4")
        reader._frame = types.SimpleNamespace(copy=lambda: "stale-frame")
        reader._sequence = 1
        reader._stopped = True
        reader._error = "video source stopped producing frames"

        with self.assertRaisesRegex(
            RuntimeError, "video source stopped producing frames"
        ):
            reader.wait_next(previous_sequence=1, timeout_seconds=0.0)

    def test_wait_next_observes_stop_event_without_waiting_for_frame_timeout(self):
        worker = load_worker_module()
        reader = worker.LatestFrameReader("rtsp://127.0.0.1/camera")
        stop_event = worker.threading.Event()
        errors = []

        def wait_for_frame():
            try:
                reader.wait_next(
                    previous_sequence=0,
                    timeout_seconds=10.0,
                    stop_event=stop_event,
                )
            except Exception as error:  # noqa: BLE001 - asserted below
                errors.append(error)

        waiter = worker.threading.Thread(target=wait_for_frame)
        waiter.start()
        stop_event.set()
        waiter.join(timeout=0.3)
        stopped_promptly = not waiter.is_alive()

        if waiter.is_alive():
            with reader._lock:
                reader._stopped = True
                reader._lock.notify_all()
            waiter.join(timeout=1.0)

        self.assertTrue(stopped_promptly)
        self.assertEqual(len(errors), 1)
        self.assertIsInstance(errors[0], worker.WorkerStopRequested)

    def test_run_worker_fetches_frame_after_remaining_throttle_wait(self):
        worker = load_worker_module()
        frame_state = {
            "sequence": 1,
            "frame_epoch_ms": 1000,
            "image": types.SimpleNamespace(shape=(720, 1280, 3)),
        }
        reader = mock.Mock()
        reader.wait_next.side_effect = lambda _previous_sequence, stop_event=None: (
            frame_state["sequence"],
            frame_state["frame_epoch_ms"],
            frame_state["image"],
        )
        session = mock.Mock()
        session.get_inputs.return_value = [types.SimpleNamespace(name="images")]
        session.get_outputs.return_value = [types.SimpleNamespace(name="output")]
        latest_results = []
        metrics_results = []
        clock = {"now": 100.0}

        def run_inference(*_args):
            clock["now"] += 0.25
            return [None]

        session.run.side_effect = run_inference

        def record_json(path, payload):
            if path.name == "metrics.json":
                metrics_results.append(payload)
                return
            if path.name != "latest.json":
                return
            latest_results.append(payload)
            if len(latest_results) == 1:
                frame_state.update(sequence=2, frame_epoch_ms=2000)

        stop_requested = mock.Mock()
        stop_requested.is_set.side_effect = lambda: len(latest_results) >= 2

        def publish_fresher_frame(timeout):
            clock["now"] += timeout
            frame_state.update(sequence=3, frame_epoch_ms=3000)
            return False

        stop_requested.wait.side_effect = publish_fresher_frame
        args = types.SimpleNamespace(
            source="rtsp://127.0.0.1/camera",
            model="model.onnx",
            output_dir="unused",
            target_label="cat",
            max_fps=1.0,
            conf_threshold=0.25,
            iou_threshold=0.45,
            max_detections=20,
        )

        with tempfile.TemporaryDirectory() as output_dir:
            args.output_dir = output_dir
            with (
                mock.patch.object(worker, "STOP_REQUESTED", stop_requested),
                mock.patch.object(worker, "LatestFrameReader", return_value=reader),
                mock.patch.object(
                    worker,
                    "build_session",
                    return_value=(session, 320, 320, ["cat"], "CPUExecutionProvider"),
                ),
                mock.patch.object(worker, "file_sha256", return_value="model-sha"),
                mock.patch.object(worker, "atomic_write_json", side_effect=record_json),
                mock.patch.object(
                    worker.time, "monotonic", side_effect=lambda: clock["now"]
                ),
                mock.patch.object(
                    worker.time, "perf_counter", side_effect=lambda: clock["now"]
                ),
                mock.patch.object(worker.time, "time", return_value=3.0),
            ):
                worker.run_worker(args)

        self.assertEqual(
            [result["frame_epoch_ms"] for result in latest_results], [1000, 3000]
        )
        self.assertTrue(
            all(result["frame_width"] == 1280 for result in latest_results)
        )
        self.assertTrue(
            all(result["frame_height"] == 720 for result in latest_results)
        )
        self.assertTrue(
            all(result["worker_started_epoch_ms"] == 3000 for result in latest_results)
        )
        self.assertEqual(
            [result["consecutive_absent_frames"] for result in latest_results],
            [1, 2],
        )
        self.assertTrue(
            all(result["consecutive_present_frames"] == 0 for result in latest_results)
        )
        self.assertTrue(metrics_results)
        self.assertTrue(all("target_frames" in metrics for metrics in metrics_results))
        self.assertTrue(all("cat_frames" not in metrics for metrics in metrics_results))
        stop_requested.wait.assert_called_once_with(0.75)

    def test_run_worker_releases_inference_resources_before_closing_reader(self):
        worker = load_worker_module()
        events = []

        class TrackedReference:
            def __init__(self, resource_name, **attributes):
                self.resource_name = resource_name
                self._cycle = self
                for attribute, value in attributes.items():
                    setattr(self, attribute, value)

            def __del__(self):
                events.append(f"{self.resource_name}_released")

        class TrackedSession:
            def __init__(self):
                self._cycle = self

            def get_inputs(self):
                return [TrackedReference("input", name="images")]

            def get_outputs(self):
                return [TrackedReference("output", name="output")]

            def __del__(self):
                events.append("session_released")

        reader = mock.Mock()
        reader.close.side_effect = lambda: events.append("reader_closed")
        stop_requested = worker.threading.Event()
        stop_requested.set()
        args = types.SimpleNamespace(
            source="rtsp://127.0.0.1/camera",
            model="model.onnx",
            output_dir="unused",
            target_label="cat",
            max_fps=1.0,
            conf_threshold=0.25,
            iou_threshold=0.45,
            max_detections=20,
        )

        with tempfile.TemporaryDirectory() as output_dir:
            args.output_dir = output_dir
            with (
                mock.patch.object(worker, "STOP_REQUESTED", stop_requested),
                mock.patch.object(worker, "LatestFrameReader", return_value=reader),
                mock.patch.object(
                    worker,
                    "build_session",
                    side_effect=lambda _args: (
                        TrackedSession(),
                        320,
                        320,
                        ["cat"],
                        "SpaceMITExecutionProvider",
                    ),
                ),
                mock.patch.object(worker, "file_sha256", return_value="model-sha"),
            ):
                worker.run_worker(args)

        gc.collect()
        reader_closed_index = events.index("reader_closed")
        for resource in ("session_released", "input_released", "output_released"):
            self.assertIn(resource, events)
            self.assertLess(events.index(resource), reader_closed_index)


if __name__ == "__main__":
    unittest.main()
