import importlib.util
import sys
import types
import unittest
from pathlib import Path

import numpy as np


def load_analyzer_module():
    modules = {
        "cv2": types.ModuleType("cv2"),
        "onnxruntime": types.ModuleType("onnxruntime"),
    }
    previous = {name: sys.modules.get(name) for name in modules}
    sys.modules.update(modules)
    try:
        path = Path(__file__).parents[1] / "scripts" / "harbornavi_k3_yolov8_analyzer.py"
        spec = importlib.util.spec_from_file_location("k3_yolov8_analyzer", path)
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


class K3Yolov8AnalyzerTests(unittest.TestCase):
    def test_letterbox_padding_matches_model_output_format(self):
        analyzer = load_analyzer_module()

        self.assertEqual(analyzer.letterbox_pad_value(1), 114)
        self.assertEqual(analyzer.letterbox_pad_value(9), 0)

    def test_postprocess_decodes_single_combined_cat_output(self):
        analyzer = load_analyzer_module()
        output = np.array(
            [[[160.0], [100.0], [80.0], [40.0], [0.9]]], dtype=np.float32
        )
        letterbox = {
            "ratio": 1.0,
            "dw": 0.0,
            "dh": 80.0,
            "input_h": 320,
            "input_w": 320,
        }

        detections = analyzer.postprocess(
            [output], ["cat"], letterbox, (160, 320), 0.25, 0.45, 20
        )

        self.assertEqual(
            detections,
            [
                {
                    "label": "cat",
                    "confidence": 0.9,
                    "x1": 120.0,
                    "y1": 0.0,
                    "x2": 200.0,
                    "y2": 40.0,
                }
            ],
        )

    def test_postprocess_combined_output_filters_and_suppresses_boxes(self):
        analyzer = load_analyzer_module()
        output = np.array(
            [
                [
                    [160.0, 162.0, 20.0],
                    [160.0, 162.0, 20.0],
                    [100.0, 100.0, 10.0],
                    [100.0, 100.0, 10.0],
                    [0.9, 0.8, 0.1],
                ]
            ],
            dtype=np.float32,
        )
        letterbox = {
            "ratio": 1.0,
            "dw": 0.0,
            "dh": 0.0,
            "input_h": 320,
            "input_w": 320,
        }

        detections = analyzer.postprocess(
            [output], ["cat"], letterbox, (320, 320), 0.25, 0.45, 20
        )

        self.assertEqual(len(detections), 1)
        self.assertEqual(detections[0]["confidence"], 0.9)
        self.assertEqual(
            {key: detections[0][key] for key in ("x1", "y1", "x2", "y2")},
            {"x1": 110.0, "y1": 110.0, "x2": 210.0, "y2": 210.0},
        )

    def test_postprocess_combined_output_returns_empty_below_threshold(self):
        analyzer = load_analyzer_module()
        output = np.array(
            [[[160.0], [160.0], [80.0], [80.0], [0.2]]], dtype=np.float32
        )
        letterbox = {
            "ratio": 1.0,
            "dw": 0.0,
            "dh": 0.0,
            "input_h": 320,
            "input_w": 320,
        }

        detections = analyzer.postprocess(
            [output], ["cat"], letterbox, (320, 320), 0.25, 0.45, 20
        )

        self.assertEqual(detections, [])

    def test_postprocess_uses_class_confidence_for_nine_output_raw_head_format(self):
        analyzer = load_analyzer_module()
        position = np.zeros((1, 8, 1, 1), dtype=np.float32)
        class_score = np.array([[[[0.9]]]], dtype=np.float32)
        ignored_class_sum = np.zeros((1, 1, 1, 1), dtype=np.float32)
        outputs = [
            position,
            class_score,
            ignored_class_sum,
            position,
            class_score,
            ignored_class_sum,
            position,
            class_score,
            ignored_class_sum,
        ]
        letterbox = {
            "ratio": 1.0,
            "dw": 0.0,
            "dh": 0.0,
            "input_h": 320,
            "input_w": 320,
        }

        detections = analyzer.postprocess(
            outputs, ["cat"], letterbox, (320, 320), 0.25, 0.45, 20
        )

        self.assertEqual(
            detections,
            [
                {
                    "label": "cat",
                    "confidence": 0.9,
                    "x1": 0.0,
                    "y1": 0.0,
                    "x2": 320.0,
                    "y2": 320.0,
                }
            ],
        )

    def test_preprocess_uses_requested_letterbox_padding(self):
        analyzer = load_analyzer_module()
        observed = {}
        analyzer.cv2.INTER_LINEAR = 1
        analyzer.cv2.COLOR_BGR2RGB = 2
        analyzer.cv2.BORDER_CONSTANT = 3
        analyzer.cv2.resize = lambda _image, size, interpolation: np.zeros(
            (size[1], size[0], 3), dtype=np.uint8
        )
        analyzer.cv2.cvtColor = lambda image, _conversion: image

        def copy_make_border(image, top, bottom, left, right, _border, value):
            observed["value"] = value
            return np.pad(
                image,
                ((top, bottom), (left, right), (0, 0)),
                constant_values=value[0],
            )

        analyzer.cv2.copyMakeBorder = copy_make_border
        image = np.zeros((100, 200, 3), dtype=np.uint8)

        tensor, _letterbox = analyzer.preprocess(
            image, input_h=320, input_w=320, pad_value=114
        )

        self.assertEqual(observed["value"], (114, 114, 114))
        self.assertEqual(tensor.shape, (1, 3, 320, 320))


if __name__ == "__main__":
    unittest.main()
