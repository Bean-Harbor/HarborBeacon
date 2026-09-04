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
    cv2.COLOR_BGR2GRAY = 0
    cv2.CMP_GT = 0
    cv2.INTER_AREA = 0
    cv2.rectangle = mock.Mock()
    cv2.putText = mock.Mock()
    cv2.cvtColor = mock.Mock(side_effect=lambda image, _conversion: image)
    cv2.meanStdDev = mock.Mock(return_value=([[128.0]], [[20.0]]))
    cv2.resize = mock.Mock()
    cv2.absdiff = mock.Mock()
    cv2.compare = mock.Mock()
    cv2.countNonZero = mock.Mock()
    cv2.phaseCorrelate = mock.Mock()
    numpy = types.ModuleType("numpy")
    numpy.ndarray = object
    numpy.isfinite = lambda value: value == value and abs(value) != float("inf")
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


def scene_frame(name):
    frame = mock.Mock(name=name)
    frame.shape = (100, 100, 3)
    frame.copy.return_value = frame
    return frame


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

    def test_frame_observability_rejects_dark_or_low_information_frames(self):
        worker = load_worker_module()
        image = types.SimpleNamespace(shape=(720, 1280, 3))

        worker.cv2.meanStdDev.return_value = ([[4.0]], [[20.0]])
        self.assertEqual(worker.frame_observability(image), (False, "underexposed"))
        worker.cv2.meanStdDev.return_value = ([[128.0]], [[2.0]])
        self.assertEqual(worker.frame_observability(image), (False, "low_information"))

    def test_frame_observability_accepts_a_detailed_exposed_frame(self):
        worker = load_worker_module()
        image = types.SimpleNamespace(shape=(720, 1280, 3))
        worker.cv2.meanStdDev.return_value = ([[128.0]], [[20.0]])

        self.assertEqual(worker.frame_observability(image), (True, "observable"))

    def test_frame_observability_uses_only_the_configured_delivery_zone(self):
        worker = load_worker_module()

        class Image:
            shape = (100, 200, 3)

            def __init__(self):
                self.requested_slice = None
                self.cropped = types.SimpleNamespace(shape=(50, 100, 3))

            def __getitem__(self, requested_slice):
                self.requested_slice = requested_slice
                return self.cropped

        image = Image()
        worker.cv2.meanStdDev.return_value = ([[128.0]], [[2.0]])

        self.assertEqual(
            worker.frame_observability(image, (0.25, 0.25, 0.75, 0.75)),
            (False, "low_information"),
        )
        self.assertEqual(
            image.requested_slice,
            (slice(25, 75), slice(50, 150)),
        )
        self.assertIs(worker.cv2.cvtColor.call_args.args[0], image.cropped)

    def test_frame_continuity_gate_accepts_stable_absence_without_background(self):
        worker = load_worker_module()
        gate = worker.FrameContinuityGate()
        present = scene_frame("present")
        absent = scene_frame("absent")
        detection = [{"x1": 30, "y1": 30, "x2": 70, "y2": 70}]

        with (
            mock.patch.object(
                worker,
                "frame_observability",
                return_value=(True, "observable"),
            ),
            mock.patch.object(
                worker,
                "camera_motion_observability",
                return_value=(True, "observable_stable_view"),
            ) as motion,
        ):
            self.assertEqual(gate.observe(present, detection), (True, "observable"))
            self.assertEqual(
                gate.observe(absent, []),
                (True, "observable_stable_view"),
            )

        motion.assert_called_once()

    def test_frame_continuity_gate_rejects_camera_movement(self):
        worker = load_worker_module()
        gate = worker.FrameContinuityGate()
        present = scene_frame("present")
        moved = scene_frame("moved")
        detection = [{"x1": 30, "y1": 30, "x2": 70, "y2": 70}]

        with (
            mock.patch.object(
                worker,
                "frame_observability",
                return_value=(True, "observable"),
            ),
            mock.patch.object(
                worker,
                "camera_motion_observability",
                return_value=(False, "camera_moved"),
            ),
        ):
            gate.observe(present, detection)
            self.assertEqual(gate.observe(moved, []), (False, "camera_moved"))

    def test_frame_continuity_gate_reanchors_when_package_is_visible(self):
        worker = load_worker_module()
        gate = worker.FrameContinuityGate()
        first = scene_frame("first")
        moved_with_package = scene_frame("moved_with_package")
        absent = scene_frame("absent")
        detection = [{"x1": 30, "y1": 30, "x2": 70, "y2": 70}]

        with (
            mock.patch.object(
                worker,
                "frame_observability",
                return_value=(True, "observable"),
            ),
            mock.patch.object(
                worker,
                "camera_motion_observability",
                return_value=(True, "observable_stable_view"),
            ) as motion,
        ):
            gate.observe(first, detection)
            gate.observe(moved_with_package, detection)
            gate.observe(absent, [])

        self.assertIs(motion.call_args.args[0], moved_with_package)

    def test_frame_continuity_gate_allows_idle_frames_without_an_anchor(self):
        worker = load_worker_module()
        gate = worker.FrameContinuityGate()

        with mock.patch.object(
            worker,
            "frame_observability",
            return_value=(True, "observable"),
        ):
            self.assertEqual(
                gate.observe(scene_frame("idle"), []),
                (True, "observable_no_target_anchor"),
            )

    def test_camera_motion_observability_accepts_a_small_translation(self):
        worker = load_worker_module()
        sample = types.SimpleNamespace(shape=(128, 160))
        worker.cv2.phaseCorrelate.return_value = ((1.0, 1.0), 0.8)

        with (
            mock.patch.object(worker, "camera_motion_sample", return_value=sample),
            mock.patch.object(worker, "mask_motion_region", return_value=sample),
        ):
            self.assertEqual(
                worker.camera_motion_observability("before", "after", None),
                (True, "observable_stable_view"),
            )

    def test_camera_motion_observability_rejects_a_large_translation(self):
        worker = load_worker_module()
        sample = types.SimpleNamespace(shape=(128, 160))
        worker.cv2.phaseCorrelate.return_value = ((5.0, 0.0), 0.8)

        with (
            mock.patch.object(worker, "camera_motion_sample", return_value=sample),
            mock.patch.object(worker, "mask_motion_region", return_value=sample),
        ):
            self.assertEqual(
                worker.camera_motion_observability("before", "after", None),
                (False, "camera_moved"),
            )

    def test_camera_motion_observability_rejects_an_ambiguous_frame(self):
        worker = load_worker_module()
        sample = types.SimpleNamespace(shape=(128, 160))
        worker.cv2.phaseCorrelate.return_value = ((0.0, 0.0), 0.1)

        with (
            mock.patch.object(worker, "camera_motion_sample", return_value=sample),
            mock.patch.object(worker, "mask_motion_region", return_value=sample),
        ):
            self.assertEqual(
                worker.camera_motion_observability("before", "after", None),
                (False, "frame_discontinuous"),
            )

    def test_target_reference_zone_expands_valid_boxes_and_rejects_outside_boxes(self):
        worker = load_worker_module()
        image = types.SimpleNamespace(shape=(100, 100, 3))

        self.assertEqual(
            worker.target_reference_zone(
                image,
                [{"x1": 30, "y1": 30, "x2": 70, "y2": 70}],
                (0.2, 0.2, 0.8, 0.8),
            ),
            (0.24, 0.24, 0.76, 0.76),
        )
        self.assertIsNone(
            worker.target_reference_zone(
                image,
                [{"x1": 0, "y1": 0, "x2": 10, "y2": 10}],
                (0.2, 0.2, 0.8, 0.8),
            )
        )
        self.assertIsNone(
            worker.target_reference_zone(
                image,
                [{"x1": 50, "y1": 50, "x2": 50, "y2": 70}],
                None,
            )
        )

    def test_parse_observability_zone_validates_normalized_coordinates(self):
        worker = load_worker_module()

        self.assertEqual(
            worker.parse_observability_zone("0.1,0.2,0.8,0.9"),
            (0.1, 0.2, 0.8, 0.9),
        )
        for invalid in ["", "0,0,1", "-0.1,0,1,1", "0.5,0,0.5,1"]:
            with self.subTest(invalid=invalid):
                with self.assertRaises(ValueError):
                    worker.parse_observability_zone(invalid)

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
