#!/usr/bin/env python3
"""Run an on-demand YOLOv8 stream worker for one target label."""

from __future__ import annotations

import argparse
import gc
import hashlib
import json
import os
import signal
import sys
import threading
import time
from collections import deque
from pathlib import Path
from typing import Any

import cv2
import numpy as np
import onnxruntime as ort

from harbornavi_k3_yolov8_analyzer import (
    input_hw,
    letterbox_pad_value,
    load_labels,
    postprocess,
    preprocess,
    provider_list,
)


DEFAULT_MODEL = "/var/lib/harboros-beacon/models/yolov8n_192x320.q.onnx"
DEFAULT_LABELS = "/var/lib/harboros-beacon/models/label.txt"
DEFAULT_TARGET_LABEL = "cat"
CONFIDENCE_OVERRIDE_ENV = "HARBOR_K3_YOLO_CONFIDENCE_OVERRIDE"
STOP_REQUESTED = threading.Event()


class WorkerStopRequested(Exception):
    """Stop the worker through its normal resource cleanup path."""


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="On-demand K3 YOLOv8 stream worker")
    parser.add_argument("--source", required=True)
    parser.add_argument("--model", default=DEFAULT_MODEL)
    parser.add_argument("--labels", default=DEFAULT_LABELS)
    parser.add_argument("--output-dir", required=True)
    parser.add_argument("--target-label", default=DEFAULT_TARGET_LABEL)
    parser.add_argument("--provider", choices=["cpu", "spacemit"], default="cpu")
    parser.add_argument("--max-fps", type=float, default=25.0)
    parser.add_argument("--conf-threshold", type=float, default=0.35)
    parser.add_argument("--iou-threshold", type=float, default=0.45)
    parser.add_argument("--max-detections", type=int, default=20)
    return parser.parse_args()


def file_sha256(path: str) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def confidence_threshold_from_env(command_line_value: float) -> float:
    raw_value = os.environ.get(CONFIDENCE_OVERRIDE_ENV)
    if raw_value is None:
        return command_line_value
    try:
        value = float(raw_value.strip())
    except ValueError as error:
        raise ValueError(f"{CONFIDENCE_OVERRIDE_ENV} must be a number") from error
    if not 0 < value <= 1:
        raise ValueError(
            f"{CONFIDENCE_OVERRIDE_ENV} must be greater than 0 and at most 1"
        )
    return value


def filter_target_detections(
    detections: list[dict[str, Any]], target_label: str
) -> list[dict[str, Any]]:
    normalized = target_label.strip().lower()
    return [
        detection
        for detection in detections
        if str(detection.get("label", "")).strip().lower() == normalized
    ]


def source_kind(source: str) -> str:
    lowered = source.strip().lower()
    if lowered.startswith("rtsp://") or lowered.startswith("rtsps://"):
        return "rtsp"
    return "file"


def should_write_snapshot(
    detections: list[dict[str, Any]], now: float, last_write: float
) -> bool:
    return bool(detections) and now - last_write >= 1.0


class ConsecutiveDetectionState:
    def __init__(self) -> None:
        self._present_frames = 0
        self._absent_frames = 0
        self._present_since_epoch_ms = 0
        self._absent_since_epoch_ms = 0

    def observe(self, present: bool, frame_epoch_ms: int) -> dict[str, int]:
        if present:
            if self._present_frames == 0:
                self._present_since_epoch_ms = frame_epoch_ms
            self._present_frames += 1
            self._absent_frames = 0
            self._absent_since_epoch_ms = 0
        else:
            if self._absent_frames == 0:
                self._absent_since_epoch_ms = frame_epoch_ms
            self._absent_frames += 1
            self._present_frames = 0
            self._present_since_epoch_ms = 0
        return {
            "consecutive_present_frames": self._present_frames,
            "consecutive_absent_frames": self._absent_frames,
            "present_since_epoch_ms": self._present_since_epoch_ms,
            "absent_since_epoch_ms": self._absent_since_epoch_ms,
        }


def atomic_write_json(path: Path, payload: dict[str, Any]) -> None:
    temporary = path.with_suffix(f"{path.suffix}.tmp")
    temporary.write_text(
        json.dumps(payload, ensure_ascii=False, separators=(",", ":")) + "\n",
        encoding="utf-8",
    )
    os.replace(temporary, path)


def atomic_write_jpeg(path: Path, image: np.ndarray) -> None:
    ok, encoded = cv2.imencode(".jpg", image, [cv2.IMWRITE_JPEG_QUALITY, 88])
    if not ok:
        raise RuntimeError("failed to encode annotated frame")
    temporary = path.with_suffix(f"{path.suffix}.tmp")
    temporary.write_bytes(encoded.tobytes())
    os.replace(temporary, path)


def annotate(image: np.ndarray, detections: list[dict[str, Any]]) -> np.ndarray:
    annotated = image.copy()
    for detection in detections:
        x1 = int(round(float(detection["x1"])))
        y1 = int(round(float(detection["y1"])))
        x2 = int(round(float(detection["x2"])))
        y2 = int(round(float(detection["y2"])))
        label = str(detection["label"]).strip()
        confidence = float(detection["confidence"])
        cv2.rectangle(annotated, (x1, y1), (x2, y2), (0, 220, 80), 2)
        cv2.putText(
            annotated,
            f"{label} {confidence:.2f}",
            (x1, max(18, y1 - 6)),
            cv2.FONT_HERSHEY_SIMPLEX,
            0.55,
            (0, 220, 80),
            2,
            cv2.LINE_AA,
        )
    return annotated


class LatestFrameReader:
    def __init__(self, source: str) -> None:
        self._source = source
        self._lock = threading.Condition()
        self._capture: cv2.VideoCapture | None = None
        self._frame: np.ndarray | None = None
        self._frame_epoch_ms = 0
        self._sequence = 0
        self._error: str | None = None
        self._stopped = False
        self._thread = threading.Thread(target=self._run, name="k3-yolo-frame-reader")

    def start(self) -> None:
        self._thread.start()

    def close(self) -> None:
        self._stopped = True
        with self._lock:
            self._lock.notify_all()
        if self._capture is not None:
            self._capture.release()
        self._thread.join(timeout=3)

    def wait_next(
        self,
        previous_sequence: int,
        timeout_seconds: float = 10.0,
        stop_event: threading.Event | None = None,
    ) -> tuple[int, int, np.ndarray]:
        deadline = time.monotonic() + timeout_seconds
        with self._lock:
            while self._sequence <= previous_sequence:
                if stop_event is not None and stop_event.is_set():
                    raise WorkerStopRequested
                if self._error is not None:
                    raise RuntimeError(self._error)
                if self._stopped:
                    raise RuntimeError("video source stopped before a fresh frame was available")
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError("stream did not produce a fresh frame")
                self._lock.wait(timeout=min(remaining, 0.1))
            if self._frame is None:
                raise RuntimeError(self._error or "stream ended before a frame was available")
            return self._sequence, self._frame_epoch_ms, self._frame.copy()

    def _run(self) -> None:
        capture = cv2.VideoCapture(self._source, cv2.CAP_FFMPEG)
        self._capture = capture
        capture.set(cv2.CAP_PROP_BUFFERSIZE, 1)
        if not capture.isOpened():
            self._set_error("failed to open video source")
            return
        file_frame_interval = 0.0
        if source_kind(self._source) == "file":
            source_fps = float(capture.get(cv2.CAP_PROP_FPS))
            if source_fps > 0:
                file_frame_interval = 1.0 / source_fps
        consecutive_failures = 0
        while not self._stopped and not STOP_REQUESTED.is_set():
            ok, frame = capture.read()
            if not ok or frame is None:
                consecutive_failures += 1
                if consecutive_failures >= 20:
                    self._set_error("video source stopped producing frames")
                    return
                time.sleep(0.05)
                continue
            consecutive_failures = 0
            with self._lock:
                self._frame = frame
                self._frame_epoch_ms = int(time.time() * 1000)
                self._sequence += 1
                self._lock.notify_all()
            if file_frame_interval > 0:
                STOP_REQUESTED.wait(file_frame_interval)
        with self._lock:
            self._stopped = True
            self._lock.notify_all()

    def _set_error(self, message: str) -> None:
        with self._lock:
            self._error = message
            self._stopped = True
            self._lock.notify_all()


def build_session(args: argparse.Namespace) -> tuple[ort.InferenceSession, int, int, list[str], str]:
    labels = load_labels(args.labels)
    if args.target_label.strip().lower() not in {label.lower() for label in labels}:
        raise ValueError("target label is not present in the label file")
    providers = provider_list(args.provider)
    options = ort.SessionOptions()
    if args.provider == "spacemit":
        ai_threads = int(os.environ.get("HARBOR_K3_YOLO_AI_THREADS", "4"))
        if ai_threads <= 0:
            raise ValueError("HARBOR_K3_YOLO_AI_THREADS must be positive")
        affinity = os.environ.get(
            "HARBOR_K3_YOLO_AI_AFFINITY", "8;9;10;11"
        )
        core_ids = [core_id.strip() for core_id in affinity.split(";")]
        if len(core_ids) != ai_threads or any(
            not core_id.isdigit() for core_id in core_ids
        ):
            raise ValueError(
                "HARBOR_K3_YOLO_AI_AFFINITY "
                f"must contain {ai_threads} core IDs"
            )
        options.intra_op_num_threads = 1
        providers = [
            (
                providers[0],
                {
                    "SPACEMIT_EP_INTRA_THREAD_NUM": str(ai_threads),
                    "SPACEMIT_EP_INTRA_THREAD_AFFINITY": affinity,
                    "SPACEMIT_EP_INTER_THREAD_NUM": "1",
                },
            )
        ]
    else:
        cpu_threads = int(os.environ.get("HARBOR_K3_YOLO_CPU_THREADS", "1"))
        if cpu_threads <= 0:
            raise ValueError("HARBOR_K3_YOLO_CPU_THREADS must be positive")
        options.intra_op_num_threads = cpu_threads
    session = ort.InferenceSession(args.model, sess_options=options, providers=providers)
    input_height, input_width = input_hw(session.get_inputs()[0].shape)
    provider = session.get_providers()[0] if session.get_providers() else providers[0]
    return session, input_height, input_width, labels, provider


def run_worker(args: argparse.Namespace) -> int:
    if not 0 < args.max_fps <= 30:
        raise ValueError("max-fps must be greater than 0 and at most 30")
    confidence_threshold = confidence_threshold_from_env(args.conf_threshold)
    if not 0 < confidence_threshold <= 1:
        raise ValueError("conf-threshold must be greater than 0 and at most 1")
    output_dir = Path(args.output_dir)
    output_dir.mkdir(parents=True, exist_ok=True)
    session, input_height, input_width, labels, provider = build_session(args)
    input_info = session.get_inputs()[0]
    output_names = [output.name for output in session.get_outputs()]
    model_sha256 = file_sha256(args.model)
    frame_interval = 1.0 / args.max_fps
    inference_samples: deque[int] = deque(maxlen=1000)
    started_monotonic = time.monotonic()
    worker_started_epoch_ms = int(time.time() * 1000)
    sequence = 0
    processed = 0
    target_frames = 0
    reader = LatestFrameReader(args.source)
    reader.start()
    last_processed_started = 0.0
    last_snapshot_write = 0.0
    detection_state = ConsecutiveDetectionState()
    tensor: Any | None = None
    outputs: Any | None = None
    try:
        while not STOP_REQUESTED.is_set():
            wait_seconds = frame_interval - (
                time.monotonic() - last_processed_started
            )
            if wait_seconds > 0:
                STOP_REQUESTED.wait(wait_seconds)
                if STOP_REQUESTED.is_set():
                    break
            sequence, frame_epoch_ms, image = reader.wait_next(
                sequence,
                stop_event=STOP_REQUESTED,
            )
            now = time.monotonic()
            last_processed_started = now
            tensor, letterbox = preprocess(
                image,
                input_height,
                input_width,
                letterbox_pad_value(len(output_names)),
            )
            inference_started = time.perf_counter()
            outputs = session.run(output_names, {input_info.name: tensor})
            inference_ms = int((time.perf_counter() - inference_started) * 1000)
            detections = postprocess(
                outputs,
                labels,
                letterbox,
                image.shape[:2],
                confidence_threshold,
                args.iou_threshold,
                args.max_detections,
            )
            target_detections = filter_target_detections(detections, args.target_label)
            processed += 1
            target_frames += int(bool(target_detections))
            inference_samples.append(inference_ms)
            processed_epoch_ms = int(time.time() * 1000)
            frame_height, frame_width = image.shape[:2]
            result = {
                "schema": "harbornavi.k3.yoloDetectionResult.v1",
                "ok": True,
                "sequence": processed,
                "worker_started_epoch_ms": worker_started_epoch_ms,
                "source_kind": source_kind(args.source),
                "target_label": args.target_label.strip().lower(),
                "provider": provider,
                "model_sha256": model_sha256,
                "confidence_threshold": confidence_threshold,
                "frame_epoch_ms": frame_epoch_ms,
                "processed_epoch_ms": processed_epoch_ms,
                "result_age_ms": max(0, processed_epoch_ms - frame_epoch_ms),
                "frame_width": int(frame_width),
                "frame_height": int(frame_height),
                "inference_ms": inference_ms,
                "detection_count": len(target_detections),
                "detections": target_detections,
            }
            result.update(
                detection_state.observe(bool(target_detections), frame_epoch_ms)
            )
            atomic_write_json(output_dir / "latest.json", result)
            if should_write_snapshot(target_detections, now, last_snapshot_write):
                atomic_write_jpeg(
                    output_dir / "latest.jpg", annotate(image, target_detections)
                )
                last_snapshot_write = now
            ordered = sorted(inference_samples)
            p95_index = max(0, int(round((len(ordered) - 1) * 0.95)))
            metrics = {
                "schema": "harbornavi.k3.yoloDetectionMetrics.v1",
                "status": "running",
                "source_kind": source_kind(args.source),
                "target_label": args.target_label.strip().lower(),
                "provider": provider,
                "confidence_threshold": confidence_threshold,
                "frames_processed": processed,
                "target_frames": target_frames,
                "average_inference_ms": int(sum(ordered) / len(ordered)),
                "p95_inference_ms": ordered[p95_index],
                "uptime_ms": int((time.monotonic() - started_monotonic) * 1000),
                "updated_at_epoch_ms": processed_epoch_ms,
            }
            atomic_write_json(output_dir / "metrics.json", metrics)
    except WorkerStopRequested:
        return 0
    except (RuntimeError, TimeoutError) as error:
        if source_kind(args.source) == "file" and processed > 0:
            return 0
        raise error
    finally:
        outputs = None
        tensor = None
        input_info = None
        output_names = []
        session = None
        gc.collect()
        reader.close()
    return 0


def request_stop(_signum: int, _frame: Any) -> None:
    STOP_REQUESTED.set()


def main() -> int:
    signal.signal(signal.SIGTERM, request_stop)
    signal.signal(signal.SIGINT, request_stop)
    return run_worker(parse_args())


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as error:
        print(
            json.dumps(
                {"ok": False, "error": str(error)},
                ensure_ascii=False,
                separators=(",", ":"),
            ),
            file=sys.stderr,
        )
        raise
