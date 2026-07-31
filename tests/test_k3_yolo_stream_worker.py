import importlib.util
import sys
import types
import unittest
from pathlib import Path


def load_worker_module():
    cv2 = types.ModuleType("cv2")
    cv2.VideoCapture = object
    cv2.CAP_FFMPEG = 0
    cv2.CAP_PROP_BUFFERSIZE = 0
    cv2.CAP_PROP_FPS = 0
    numpy = types.ModuleType("numpy")
    numpy.ndarray = object
    onnxruntime = types.ModuleType("onnxruntime")
    analyzer = types.ModuleType("harbornavi_k3_yolov8_analyzer")
    analyzer.input_hw = lambda _shape: (192, 320)
    analyzer.load_labels = lambda _path: ["cat"]
    analyzer.postprocess = lambda *_args, **_kwargs: []
    analyzer.preprocess = lambda *_args, **_kwargs: (None, None)
    analyzer.provider_list = lambda _provider: ["CPUExecutionProvider"]
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


if __name__ == "__main__":
    unittest.main()
