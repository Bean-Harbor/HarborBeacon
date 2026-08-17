#!/usr/bin/env python3
"""Run bounded MobileNetV2 cat verification through the SpaceMIT EP."""

from __future__ import annotations

import argparse
import gc
import hashlib
import importlib
import json
import math
import os
import signal
import sys
import threading
import time
from pathlib import Path
from typing import Any, Callable


MODEL_NAME = "mobilenetv2-cat-binary-v2-int8"
MAX_FRAMES = 9
MINIMUM_POSITIVE_FRAMES = 3
MAX_FRAME_BYTES = 32 * 1024 * 1024
STOP_REQUESTED = threading.Event()


class ClassifierStopRequested(Exception):
    """Stop classification without reporting a partial result."""


def request_stop(_signum: int, _frame: Any) -> None:
    STOP_REQUESTED.set()


def raise_if_stop_requested() -> None:
    if STOP_REQUESTED.is_set():
        raise ClassifierStopRequested


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--model", type=Path, required=True)
    parser.add_argument("--expected-sha256", required=True)
    parser.add_argument("--threshold", type=float, default=0.62)
    parser.add_argument("--ai-threads", type=int, default=4)
    parser.add_argument("--affinity", default="12;13;14;15")
    parser.add_argument("--frame", action="append", default=[])
    parser.add_argument("--probe", action="store_true")
    return parser.parse_args()


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def load_spacemit_runtime(
    import_module: Callable[[str], Any] = importlib.import_module,
) -> Any:
    runtime = import_module("onnxruntime")
    import_module("spacemit_ort")
    return runtime


def read_task_affinities() -> list[tuple[int, str]]:
    tasks = []
    for status_path in sorted(Path(f"/proc/{os.getpid()}/task").glob("*/status")):
        status = status_path.read_text(encoding="utf-8")
        allowed = next(
            line.split(":", 1)[1].strip()
            for line in status.splitlines()
            if line.startswith("Cpus_allowed_list:")
        )
        tasks.append((int(status_path.parent.name), allowed))
    return tasks


def validate_ep_worker_affinity(
    task_affinities: list[tuple[int, str]], expected_cores: list[int]
) -> None:
    observed = sorted(
        int(allowed)
        for _, allowed in task_affinities
        if allowed.isdigit() and 8 <= int(allowed) <= 15
    )
    if observed != sorted(expected_cores):
        raise RuntimeError(
            "SpaceMIT EP worker affinity mismatch: "
            f"expected={sorted(expected_cores)} observed={observed}"
        )


def parse_requested_affinity(ai_threads: int, affinity: str) -> list[int]:
    cores = affinity.split(";")
    if ai_threads < 1 or ai_threads > 4 or len(cores) != ai_threads:
        raise ValueError("affinity must contain one unique core ID per AI thread")
    try:
        parsed = [int(core) for core in cores]
    except ValueError as error:
        raise ValueError("affinity core IDs must be between 12 and 15") from error
    if len(set(parsed)) != len(parsed):
        raise ValueError("affinity must contain unique core IDs")
    if any(core not in range(12, 16) for core in parsed):
        raise ValueError("affinity core IDs must be between 12 and 15")
    return parsed


def parse_frame_specs(specs: list[str], max_frames: int = MAX_FRAMES) -> list[tuple[int, Path]]:
    if not specs or len(specs) > max_frames:
        raise ValueError(f"classifier accepts at most {max_frames} frames")
    parsed: list[tuple[int, Path]] = []
    seen: set[int] = set()
    for spec in specs:
        index_text, separator, path_text = spec.partition("=")
        if not separator or not index_text.isdigit() or not path_text:
            raise ValueError("frame must use INDEX=PATH")
        frame_index = int(index_text)
        if not 1 <= frame_index <= max_frames:
            raise ValueError(f"frame index must be between 1 and {max_frames}")
        if frame_index in seen:
            raise ValueError(f"duplicate frame index: {frame_index}")
        seen.add(frame_index)
        parsed.append((frame_index, Path(path_text)))
    return sorted(parsed)


def aggregate_predictions(
    predictions: list[dict[str, Any]],
    threshold: float,
    minimum_positive_frames: int = MINIMUM_POSITIVE_FRAMES,
) -> dict[str, Any]:
    if not 0.0 <= threshold <= 1.0 or minimum_positive_frames < 1:
        raise ValueError("invalid classifier aggregation policy")
    positive = sorted(
        int(prediction["frame_index"])
        for prediction in predictions
        if float(prediction["cat_probability"]) >= threshold
    )
    cat_present = len(positive) >= minimum_positive_frames
    reason_code = (
        "cat_visible" if cat_present else "no_cat_visible" if not positive else "uncertain"
    )
    return {
        "cat_present": cat_present,
        "cat_frame_indices": positive,
        "reason_code": reason_code,
    }


def create_session(model_path: Path, ai_threads: int, affinity: str) -> tuple[Any, dict[str, Any]]:
    cores = parse_requested_affinity(ai_threads, affinity)

    runtime = load_spacemit_runtime()
    options = runtime.SessionOptions()
    options.intra_op_num_threads = 1
    providers = [
        (
            "SpaceMITExecutionProvider",
            {
                "SPACEMIT_EP_INTRA_THREAD_NUM": str(ai_threads),
                "SPACEMIT_EP_INTRA_THREAD_AFFINITY": affinity,
                "SPACEMIT_EP_INTER_THREAD_NUM": "1",
                "SPACEMIT_EP_USE_GLOBAL_INTRA_THREAD": "1",
            },
        )
    ]
    started = time.perf_counter()
    session = runtime.InferenceSession(
        str(model_path), sess_options=options, providers=providers
    )
    session_creation_ms = round((time.perf_counter() - started) * 1000)
    if not session.get_providers() or session.get_providers()[0] != "SpaceMITExecutionProvider":
        raise RuntimeError("SpaceMITExecutionProvider is not active")
    inputs = session.get_inputs()
    outputs = session.get_outputs()
    if len(inputs) != 1 or inputs[0].shape != [1, 3, 224, 224]:
        raise ValueError("unexpected model input contract")
    if len(outputs) != 1 or outputs[0].shape != [1, 2]:
        raise ValueError("unexpected model output contract")
    validate_ep_worker_affinity(read_task_affinities(), cores)
    return session, {
        "input_name": inputs[0].name,
        "output_name": outputs[0].name,
        "session_creation_ms": session_creation_ms,
    }


def preprocess_image(path: Path) -> Any:
    import numpy as np
    from PIL import Image

    with Image.open(path) as image:
        image = image.convert("RGB").resize((224, 224), resample=Image.Resampling.BILINEAR)
        tensor = np.asarray(image, dtype=np.float32) / 255.0
    mean = np.asarray([0.485, 0.456, 0.406], dtype=np.float32)
    std = np.asarray([0.229, 0.224, 0.225], dtype=np.float32)
    tensor = (tensor - mean) / std
    return np.transpose(tensor, (2, 0, 1))[None, ...]


def softmax_cat(logits: Any) -> float:
    negative = float(logits[0][0])
    positive = float(logits[0][1])
    peak = max(negative, positive)
    negative_exp = math.exp(negative - peak)
    positive_exp = math.exp(positive - peak)
    return positive_exp / (negative_exp + positive_exp)


def prepare_model(args: argparse.Namespace) -> tuple[Path, str]:
    model_path = args.model.resolve(strict=True)
    if not model_path.is_file():
        raise ValueError("model path must be a regular file")
    model_sha256 = sha256(model_path)
    if model_sha256 != args.expected_sha256.strip().lower():
        raise ValueError("model SHA256 mismatch")
    return model_path, model_sha256


def run_probe(args: argparse.Namespace) -> dict[str, Any]:
    model_path, model_sha256 = prepare_model(args)
    session: Any | None = None
    try:
        raise_if_stop_requested()
        session, details = create_session(model_path, args.ai_threads, args.affinity)
        raise_if_stop_requested()
        return {
            "schema_version": "1.0",
            "status": "ok",
            "provider": "SpaceMITExecutionProvider",
            "model_name": MODEL_NAME,
            "model_sha256": model_sha256,
            "session_creation_ms": details["session_creation_ms"],
        }
    finally:
        session = None
        gc.collect()


def run(args: argparse.Namespace) -> dict[str, Any]:
    if getattr(args, "probe", False):
        return run_probe(args)
    if not 0.0 <= args.threshold <= 1.0:
        raise ValueError("threshold must be between 0.0 and 1.0")
    model_path, model_sha256 = prepare_model(args)
    frames = parse_frame_specs(args.frame)
    for _, frame_path in frames:
        size = frame_path.stat().st_size
        if not frame_path.is_file() or size <= 0 or size > MAX_FRAME_BYTES:
            raise ValueError(f"invalid frame file: {frame_path}")

    started = time.perf_counter()
    session: Any | None = None
    tensor: Any | None = None
    logits: Any | None = None
    try:
        raise_if_stop_requested()
        session, session_details = create_session(
            model_path, args.ai_threads, args.affinity
        )
        predictions = []
        total_inference_ms = 0
        for frame_index, frame_path in frames:
            raise_if_stop_requested()
            tensor = preprocess_image(frame_path)
            raise_if_stop_requested()
            inference_started = time.perf_counter()
            logits = session.run(
                [session_details["output_name"]],
                {session_details["input_name"]: tensor},
            )[0]
            raise_if_stop_requested()
            inference_ms = round((time.perf_counter() - inference_started) * 1000)
            total_inference_ms += inference_ms
            probability = softmax_cat(logits)
            predictions.append(
                {
                    "frame_index": frame_index,
                    "cat_probability_ppm": round(probability * 1_000_000),
                    "cat_probability": probability,
                    "inference_ms": inference_ms,
                }
            )
        aggregate_predictions(
            predictions,
            threshold=args.threshold,
            minimum_positive_frames=MINIMUM_POSITIVE_FRAMES,
        )
        return {
            "schema_version": "1.0",
            "status": "ok",
            "provider": "SpaceMITExecutionProvider",
            "model_name": MODEL_NAME,
            "model_sha256": model_sha256,
            "threshold_ppm": round(args.threshold * 1_000_000),
            "sampled_frame_count": len(predictions),
            "predictions": predictions,
            "session_creation_ms": session_details["session_creation_ms"],
            "total_inference_ms": total_inference_ms,
            "elapsed_ms": round((time.perf_counter() - started) * 1000),
        }
    finally:
        logits = None
        tensor = None
        session = None
        gc.collect()


def main() -> int:
    signal.signal(signal.SIGTERM, request_stop)
    try:
        result = run(parse_args())
    except ClassifierStopRequested:
        return 128 + signal.SIGTERM
    except Exception as error:
        print(f"cat_classifier_error={type(error).__name__}:{error}", file=sys.stderr)
        return 2
    print(json.dumps(result, ensure_ascii=True, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
