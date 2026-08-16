#!/usr/bin/env python3
"""HarborNavi K3 short-command YOLOv8 analyzer.

The model/output shape follows the Bianbu spacemit-demo YOLOv8n 192x320 ONNX
recipe. CPUExecutionProvider is the P0 default while the K3 TCM issue is open.
"""

import argparse
import hashlib
import json
import sys
import time

import cv2
import numpy as np
import onnxruntime as ort


ANALYZER_NAME = "spacemit-yolov8n-192x320-short-command"
PET_LABELS = {"cat", "dog"}
VEHICLE_LABELS = {"car", "bus", "truck", "motorcycle", "bicycle"}


def main() -> int:
    parser = argparse.ArgumentParser(description="Run K3 YOLOv8 snapshot analysis")
    parser.add_argument("--image", required=True)
    parser.add_argument("--model", required=True)
    parser.add_argument("--label", required=True)
    parser.add_argument("--provider", choices=["cpu", "spacemit"], default="cpu")
    parser.add_argument("--conf-threshold", type=float, default=0.25)
    parser.add_argument("--iou-threshold", type=float, default=0.45)
    parser.add_argument("--max-detections", type=int, default=50)
    args = parser.parse_args()

    started = time.perf_counter()
    labels = load_labels(args.label)
    model_sha256 = file_sha256(args.model)
    image_sha256 = file_sha256(args.image)

    providers = provider_list(args.provider)
    session_options = ort.SessionOptions()
    session_options.intra_op_num_threads = 1
    session = ort.InferenceSession(args.model, sess_options=session_options, providers=providers)
    input_info = session.get_inputs()[0]
    output_names = [output.name for output in session.get_outputs()]
    input_h, input_w = input_hw(input_info.shape)

    image = cv2.imread(args.image)
    if image is None:
        raise RuntimeError(f"failed to read image: {args.image}")

    pre_started = time.perf_counter()
    tensor, letterbox = preprocess(
        image, input_h, input_w, letterbox_pad_value(len(output_names))
    )
    preprocess_ms = elapsed_ms(pre_started)

    infer_started = time.perf_counter()
    outputs = session.run(output_names, {input_info.name: tensor})
    inference_ms = elapsed_ms(infer_started)

    post_started = time.perf_counter()
    detections = postprocess(
        outputs,
        labels,
        letterbox,
        image.shape[:2],
        args.conf_threshold,
        args.iou_threshold,
        args.max_detections,
    )
    postprocess_ms = elapsed_ms(post_started)

    event_type, confidence, detected_labels = classify(detections)
    result = {
        "ok": True,
        "analyzer": ANALYZER_NAME,
        "provider": session.get_providers()[0] if session.get_providers() else providers[0],
        "requested_provider": args.provider,
        "model_sha256": model_sha256,
        "image_sha256": image_sha256,
        "latency_ms": elapsed_ms(started),
        "preprocess_ms": preprocess_ms,
        "inference_ms": inference_ms,
        "postprocess_ms": postprocess_ms,
        "event_type": event_type,
        "confidence": confidence,
        "labels": detected_labels,
        "detections": detections,
    }
    print(json.dumps(result, ensure_ascii=False, separators=(",", ":")))
    return 0


def provider_list(provider: str) -> list[str]:
    if provider == "spacemit":
        import spacemit_ort  # noqa: F401

        return ["SpaceMITExecutionProvider"]
    return ["CPUExecutionProvider"]


def load_labels(path: str) -> list[str]:
    with open(path, "r", encoding="utf-8") as handle:
        return [line.strip() for line in handle if line.strip()]


def file_sha256(path: str) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def input_hw(shape) -> tuple[int, int]:
    if len(shape) < 4:
        raise RuntimeError(f"unsupported model input shape: {shape}")
    return int(shape[2]), int(shape[3])


def letterbox_pad_value(output_count: int) -> int:
    return 114 if output_count == 1 else 0


def preprocess(image, input_h: int, input_w: int, pad_value: int = 0):
    original_h, original_w = image.shape[:2]
    ratio = min(input_h / original_h, input_w / original_w)
    resized_w = int(round(original_w * ratio))
    resized_h = int(round(original_h * ratio))
    dw = (input_w - resized_w) / 2
    dh = (input_h - resized_h) / 2
    resized = cv2.resize(image, (resized_w, resized_h), interpolation=cv2.INTER_LINEAR)
    rgb = cv2.cvtColor(resized, cv2.COLOR_BGR2RGB)
    top = int(round(dh - 0.1))
    bottom = int(round(dh + 0.1))
    left = int(round(dw - 0.1))
    right = int(round(dw + 0.1))
    padded = cv2.copyMakeBorder(
        rgb,
        top,
        bottom,
        left,
        right,
        cv2.BORDER_CONSTANT,
        value=(pad_value, pad_value, pad_value),
    )
    tensor = padded.astype(np.float32) / 255.0
    tensor = np.transpose(tensor, (2, 0, 1))[None, :, :, :]
    return tensor, {"ratio": ratio, "dw": dw, "dh": dh, "input_h": input_h, "input_w": input_w}


def postprocess(outputs, labels, letterbox, original_shape, conf_threshold, iou_threshold, limit):
    if len(outputs) == 1:
        return postprocess_combined_output(
            outputs[0],
            labels,
            letterbox,
            original_shape,
            conf_threshold,
            iou_threshold,
            limit,
        )
    if len(outputs) % 3 != 0:
        raise RuntimeError(f"unexpected YOLOv8 output count: {len(outputs)}")
    pair_per_branch = len(outputs) // 3
    boxes, class_scores = [], []
    input_h = letterbox["input_h"]
    input_w = letterbox["input_w"]

    for index in range(3):
        position = outputs[pair_per_branch * index]
        classes = outputs[pair_per_branch * index + 1]
        boxes.append(flatten_head(box_process(position, input_h, input_w)))
        class_scores.append(flatten_head(classes))

    boxes = np.concatenate(boxes)
    class_scores = np.concatenate(class_scores)
    class_ids = np.argmax(class_scores, axis=-1)
    scores = np.max(class_scores, axis=-1)
    keep = np.where(scores >= conf_threshold)[0]
    boxes = boxes[keep]
    class_ids = class_ids[keep]
    scores = scores[keep]
    if boxes.size == 0:
        return []
    return format_detections(
        boxes,
        class_ids,
        scores,
        labels,
        letterbox,
        original_shape,
        iou_threshold,
        limit,
    )


def postprocess_combined_output(
    output, labels, letterbox, original_shape, conf_threshold, iou_threshold, limit
):
    predictions = np.squeeze(output, axis=0)
    expected_columns = 4 + len(labels)
    if predictions.ndim == 2 and predictions.shape[0] == expected_columns:
        predictions = predictions.transpose()
    if predictions.ndim != 2 or predictions.shape[1] != expected_columns:
        raise RuntimeError(f"unexpected combined YOLOv8 output shape: {output.shape}")

    class_scores = predictions[:, 4:]
    class_ids = np.argmax(class_scores, axis=-1)
    scores = np.max(class_scores, axis=-1)
    keep = np.where(scores >= conf_threshold)[0]
    predictions = predictions[keep]
    class_ids = class_ids[keep]
    scores = scores[keep]
    if predictions.size == 0:
        return []

    center_x, center_y, width, height = predictions[:, :4].transpose()
    boxes = np.column_stack(
        (
            center_x - width / 2,
            center_y - height / 2,
            center_x + width / 2,
            center_y + height / 2,
        )
    )
    return format_detections(
        boxes,
        class_ids,
        scores,
        labels,
        letterbox,
        original_shape,
        iou_threshold,
        limit,
    )


def format_detections(
    boxes, class_ids, scores, labels, letterbox, original_shape, iou_threshold, limit
):
    boxes = scale_boxes_to_original(boxes, letterbox, original_shape)
    kept = []
    for class_id in sorted(set(class_ids.tolist())):
        indexes = np.where(class_ids == class_id)[0]
        for keep_index in nms(boxes[indexes], scores[indexes], iou_threshold):
            kept.append(indexes[keep_index])

    kept = sorted(kept, key=lambda item: float(scores[item]), reverse=True)[:limit]
    detections = []
    for index in kept:
        class_id = int(class_ids[index])
        label = labels[class_id] if class_id < len(labels) else str(class_id)
        x1, y1, x2, y2 = [float(value) for value in boxes[index]]
        detections.append(
            {
                "label": label,
                "confidence": round(float(scores[index]), 6),
                "x1": round(x1, 2),
                "y1": round(y1, 2),
                "x2": round(x2, 2),
                "y2": round(y2, 2),
            }
        )
    return detections


def flatten_head(value):
    channels = value.shape[1]
    return value.transpose(0, 2, 3, 1).reshape(-1, channels)


def box_process(position, input_h: int, input_w: int):
    grid_h, grid_w = position.shape[2:4]
    col, row = np.meshgrid(np.arange(grid_w), np.arange(grid_h))
    grid = np.stack((col, row), axis=0).reshape(1, 2, grid_h, grid_w)
    stride = np.array([input_h // grid_h, input_w // grid_w], dtype=np.float32).reshape(1, 2, 1, 1)
    position = dfl(position)
    box_xy1 = grid + 0.5 - position[:, 0:2, :, :]
    box_xy2 = grid + 0.5 + position[:, 2:4, :, :]
    return np.concatenate((box_xy1 * stride, box_xy2 * stride), axis=1)


def dfl(position):
    batch, channels, height, width = position.shape
    bins = channels // 4
    value = position.reshape(batch, 4, bins, height, width)
    value = value - np.max(value, axis=2, keepdims=True)
    value = np.exp(value)
    value = value / np.sum(value, axis=2, keepdims=True)
    weights = np.arange(bins, dtype=np.float32).reshape(1, 1, bins, 1, 1)
    return (value * weights).sum(axis=2)


def scale_boxes_to_original(boxes, letterbox, original_shape):
    original_h, original_w = original_shape
    ratio = letterbox["ratio"]
    dw = letterbox["dw"]
    dh = letterbox["dh"]
    boxes = boxes.copy()
    boxes[:, [0, 2]] = (boxes[:, [0, 2]] - dw) / ratio
    boxes[:, [1, 3]] = (boxes[:, [1, 3]] - dh) / ratio
    boxes[:, [0, 2]] = np.clip(boxes[:, [0, 2]], 0, original_w)
    boxes[:, [1, 3]] = np.clip(boxes[:, [1, 3]], 0, original_h)
    return boxes


def nms(boxes, scores, threshold):
    order = scores.argsort()[::-1]
    keep = []
    areas = np.maximum(0.0, boxes[:, 2] - boxes[:, 0]) * np.maximum(0.0, boxes[:, 3] - boxes[:, 1])
    while order.size > 0:
        index = order[0]
        keep.append(index)
        xx1 = np.maximum(boxes[index, 0], boxes[order[1:], 0])
        yy1 = np.maximum(boxes[index, 1], boxes[order[1:], 1])
        xx2 = np.minimum(boxes[index, 2], boxes[order[1:], 2])
        yy2 = np.minimum(boxes[index, 3], boxes[order[1:], 3])
        width = np.maximum(0.0, xx2 - xx1)
        height = np.maximum(0.0, yy2 - yy1)
        inter = width * height
        union = areas[index] + areas[order[1:]] - inter
        iou = inter / np.maximum(union, 1e-6)
        order = order[np.where(iou <= threshold)[0] + 1]
    return keep


def classify(detections):
    labels = []
    max_confidence = 0.0
    has_person = False
    has_pet = False
    has_vehicle = False
    for detection in detections:
        label = detection["label"].strip().lower()
        if label and label not in labels:
            labels.append(label)
        max_confidence = max(max_confidence, float(detection["confidence"]))
        has_person = has_person or label == "person"
        has_pet = has_pet or label in PET_LABELS
        has_vehicle = has_vehicle or label in VEHICLE_LABELS
    if has_person:
        event_type = "person_detected"
    elif has_pet:
        event_type = "pet_detected"
    elif has_vehicle:
        event_type = "vehicle_detected"
    else:
        event_type = "motion_like_scene"
    return event_type, round(max_confidence, 6), labels


def elapsed_ms(started) -> int:
    return int((time.perf_counter() - started) * 1000)


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as exc:
        print(json.dumps({"ok": False, "error": str(exc)}, ensure_ascii=False), file=sys.stderr)
        raise
