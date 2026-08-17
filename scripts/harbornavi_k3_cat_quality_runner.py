#!/usr/bin/env python3
"""Run the fixed EVT.1 cat validator against a canonical holdout manifest."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import stat
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any


SCHEMA_VERSION = 1
CLASSIFIER = Path("/usr/lib/harboros-beacon/harbornavi_k3_cat_recording_classifier.py")
SAMPLER = Path("/usr/lib/harboros-beacon/cat-sampling-plan")
MODEL = Path(
    "/usr/share/harboros-beacon/vision-models/"
    "mobilenetv2-cat-binary-v2-20260806/mobilenetv2_cat_binary_int8.onnx"
)
MODEL_SHA256 = "d0c1bdcf973ca7f6efc6e62af764ff59300e0d27abbc75c20c7f86515769d825"
MODEL_NAME = "mobilenetv2-cat-binary-v2-int8"
PROVIDER = "SpaceMITExecutionProvider"
THRESHOLD_PPM = 620_000
MAX_FRAMES = 9
MINIMUM_POSITIVE_FRAMES = 3
MAX_MANIFEST_BYTES = 16 * 1024 * 1024
MAX_CLIPS = 10_000
FFMPEG = Path("/usr/bin/ffmpeg")
FFPROBE = Path("/usr/bin/ffprobe")
PYTHON = Path("/usr/bin/python3")
MINIMUM_DURATION_MS = 5_000
MAXIMUM_DURATION_MS = 600_000
MAXIMUM_DETECTION_EVIDENCE = 256
DETECTOR_MODEL_ID = "detection-yolov8n-192x320"
DETECTOR_MODEL_PATH = "/data/vision-models/current/detection/yolov8n_192x320.q.onnx"
DETECTOR_MODEL_REVISION = "dc4477d3ea712598bb675f730642a43fe280c569"
DETECTOR_MODEL_SHA256 = "d4bf61db2a0925a0126052212479ff5044b621b12c6793420e085d36ae6b5438"
DETECTION_EVIDENCE_SCHEMA = "harbornavi.k3.yoloDetectionResult.v1"
DETECTION_EVIDENCE_PROJECTION = "cat-recording-validation-v5"
HARD_NEGATIVE_KINDS = {"person", "shadow", "other-animal"}


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def freeze_regular_file(
    source: Path,
    destination: Path,
    *,
    expected_sha256: str | None = None,
    executable: bool = False,
) -> tuple[Path, str, int]:
    """Copy one opened inode into a private file and bind the copied bytes."""
    if not source.is_absolute() or not destination.is_absolute():
        raise ValueError("freeze paths must be absolute")
    before = os.lstat(source)
    if stat.S_ISLNK(before.st_mode) or not stat.S_ISREG(before.st_mode):
        raise ValueError("freeze source must be a non-symlink regular file")
    source_flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0)
    source_flags |= getattr(os, "O_NOFOLLOW", 0)
    destination_flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    destination_flags |= getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    source_fd = os.open(source, source_flags)
    destination_fd = -1
    digest = hashlib.sha256()
    size = 0
    try:
        opened = os.fstat(source_fd)
        if not stat.S_ISREG(opened.st_mode) or (
            opened.st_dev,
            opened.st_ino,
        ) != (before.st_dev, before.st_ino):
            raise ValueError("freeze source changed while it was opened")
        destination_fd = os.open(destination, destination_flags, 0o600)
        while True:
            block = os.read(source_fd, 1024 * 1024)
            if not block:
                break
            digest.update(block)
            size += len(block)
            view = memoryview(block)
            while view:
                written = os.write(destination_fd, view)
                if written <= 0:
                    raise OSError("short write while freezing file")
                view = view[written:]
        os.fsync(destination_fd)
    except Exception:
        destination.unlink(missing_ok=True)
        raise
    finally:
        if destination_fd >= 0:
            os.close(destination_fd)
        os.close(source_fd)
    actual_sha256 = digest.hexdigest()
    if expected_sha256 is not None and actual_sha256 != expected_sha256:
        destination.unlink(missing_ok=True)
        raise ValueError("frozen file SHA256 mismatch")
    if os.name == "posix":
        destination.chmod(0o500 if executable else 0o400)
    return destination, actual_sha256, size


def require_regular_file(path: Path, label: str) -> Path:
    if not path.is_absolute() or path.is_symlink() or not path.is_file():
        raise ValueError(f"{label} must be an absolute non-symlink regular file")
    return path.resolve(strict=True)


def validate_identifier(value: Any, field: str) -> str:
    if not isinstance(value, str):
        raise ValueError(f"{field} must be a string")
    value = value.strip()
    if not value or len(value) > 128 or not all(
        character.isascii()
        and (character.isalnum() or character in "._:-")
        for character in value
    ):
        raise ValueError(f"{field} is invalid")
    return value


def validate_sha256(value: Any, field: str) -> str:
    if not isinstance(value, str):
        raise ValueError(f"{field} must be a SHA256 string")
    value = value.strip().lower()
    if len(value) != 64 or any(character not in "0123456789abcdef" for character in value):
        raise ValueError(f"{field} must be a lowercase SHA256")
    return value


def validate_uint(value: Any, field: str, minimum: int, maximum: int) -> int:
    if (
        not isinstance(value, int)
        or isinstance(value, bool)
        or not minimum <= value <= maximum
    ):
        raise ValueError(f"{field} must be an integer within {minimum}..{maximum}")
    return value


def canonical_sha256(value: Any) -> str:
    payload = json.dumps(value, ensure_ascii=True, separators=(",", ":"), sort_keys=True)
    return hashlib.sha256(payload.encode("ascii")).hexdigest()


def parse_manifest(payload: bytes) -> dict[str, Any]:
    if not payload or len(payload) > MAX_MANIFEST_BYTES:
        raise ValueError("manifest is empty or exceeds its size limit")
    manifest = json.loads(payload)
    if not isinstance(manifest, dict) or set(manifest) != {
        "schema_version",
        "dataset_id",
        "sampler",
        "detector",
        "clips",
    }:
        raise ValueError("manifest fields do not match the canonical dataset schema")
    if manifest["schema_version"] != SCHEMA_VERSION:
        raise ValueError("manifest schema_version must be 1")
    dataset_id = validate_identifier(manifest["dataset_id"], "dataset_id")
    sampler = manifest["sampler"]
    if not isinstance(sampler, dict) or set(sampler) != {"installed_path", "sha256"}:
        raise ValueError("sampler fields do not match installed_path/sha256")
    if sampler["installed_path"] != str(SAMPLER):
        raise ValueError("sampler installed_path does not match the production contract")
    sampler_sha256 = validate_sha256(sampler["sha256"], "sampler.sha256")
    detector = manifest["detector"]
    expected_detector = {
        "evidence_schema": DETECTION_EVIDENCE_SCHEMA,
        "projection_contract": DETECTION_EVIDENCE_PROJECTION,
        "model_id": DETECTOR_MODEL_ID,
        "model_installed_path": DETECTOR_MODEL_PATH,
        "model_revision": DETECTOR_MODEL_REVISION,
        "model_sha256": DETECTOR_MODEL_SHA256,
    }
    if detector != expected_detector:
        raise ValueError("detector identity does not match the production evidence contract")
    clips = manifest["clips"]
    if not isinstance(clips, list) or not clips or len(clips) > MAX_CLIPS:
        raise ValueError(f"manifest clips must contain 1..{MAX_CLIPS} items")
    normalized = []
    seen_ids: set[str] = set()
    for raw_clip in clips:
        if not isinstance(raw_clip, dict) or set(raw_clip) != {
            "clip_id",
            "camera_id",
            "video_path",
            "video_sha256",
            "expected_cat",
            "low_light",
            "hard_negative_kind",
            "recording_started_at_epoch_ms",
            "recording_ended_at_epoch_ms",
            "detection_evidence",
        }:
            raise ValueError("clip fields do not match the canonical schema")
        clip_id = validate_identifier(raw_clip["clip_id"], "clip_id")
        if clip_id in seen_ids:
            raise ValueError(f"duplicate clip_id: {clip_id}")
        seen_ids.add(clip_id)
        camera_id = validate_identifier(raw_clip["camera_id"], "camera_id")
        if not isinstance(raw_clip["video_path"], str):
            raise ValueError("video_path must be a string")
        video_path = require_regular_file(Path(raw_clip["video_path"]), "video_path")
        video_sha256 = validate_sha256(raw_clip["video_sha256"], "video_sha256")
        if sha256(video_path) != video_sha256:
            raise ValueError(f"video SHA256 mismatch for clip {clip_id}")
        if not isinstance(raw_clip["expected_cat"], bool):
            raise ValueError("expected_cat must be boolean")
        if not isinstance(raw_clip["low_light"], bool):
            raise ValueError("low_light must be boolean")
        hard_negative_kind = raw_clip["hard_negative_kind"]
        if hard_negative_kind is not None and hard_negative_kind not in HARD_NEGATIVE_KINDS:
            raise ValueError(
                "hard_negative_kind must be null, person, shadow, or other-animal"
            )
        if raw_clip["expected_cat"] and hard_negative_kind is not None:
            raise ValueError("positive clips must not declare a hard_negative_kind")
        recording_started_at_epoch_ms = validate_uint(
            raw_clip["recording_started_at_epoch_ms"],
            "recording_started_at_epoch_ms",
            1,
            2**64 - 1,
        )
        recording_ended_at_epoch_ms = validate_uint(
            raw_clip["recording_ended_at_epoch_ms"],
            "recording_ended_at_epoch_ms",
            recording_started_at_epoch_ms,
            2**64 - 1,
        )
        raw_evidence = raw_clip["detection_evidence"]
        if (
            not isinstance(raw_evidence, list)
            or not raw_evidence
            or len(raw_evidence) > MAXIMUM_DETECTION_EVIDENCE
        ):
            raise ValueError("detection_evidence must contain 1..256 records")
        detection_evidence = []
        seen_sequences: set[int] = set()
        for item in raw_evidence:
            if not isinstance(item, dict) or set(item) != {
                "sequence",
                "frame_epoch_ms",
                "confidence_ppm",
            }:
                raise ValueError("detection evidence fields do not match the production schema")
            sequence = validate_uint(item["sequence"], "evidence.sequence", 1, 2**64 - 1)
            if sequence in seen_sequences:
                raise ValueError("detection evidence sequences must be unique")
            seen_sequences.add(sequence)
            detection_evidence.append(
                {
                    "sequence": sequence,
                    "frame_epoch_ms": validate_uint(
                        item["frame_epoch_ms"], "evidence.frame_epoch_ms", 1, 2**64 - 1
                    ),
                    "confidence_ppm": validate_uint(
                        item["confidence_ppm"], "evidence.confidence_ppm", 0, 1_000_000
                    ),
                }
            )
        normalized.append(
            {
                "clip_id": clip_id,
                "camera_id": camera_id,
                "video_path": video_path,
                "video_sha256": video_sha256,
                "expected_cat": raw_clip["expected_cat"],
                "low_light": raw_clip["low_light"],
                "hard_negative_kind": hard_negative_kind,
                "recording_started_at_epoch_ms": recording_started_at_epoch_ms,
                "recording_ended_at_epoch_ms": recording_ended_at_epoch_ms,
                "detection_evidence": detection_evidence,
            }
        )
    return {
        "schema_version": SCHEMA_VERSION,
        "dataset_id": dataset_id,
        "sampler": {"installed_path": str(SAMPLER), "sha256": sampler_sha256},
        "detector": expected_detector,
        "clips": normalized,
    }


def duration_ms_from_seconds(duration_seconds: float) -> int:
    if not math.isfinite(duration_seconds):
        raise ValueError("clip duration is not finite")
    duration_ms = math.floor(duration_seconds * 1000.0 + 0.5)
    if not MINIMUM_DURATION_MS <= duration_ms <= MAXIMUM_DURATION_MS:
        raise ValueError("clip duration is outside the production 5000..600000ms range")
    return duration_ms


def run_production_sampler(
    sampler_path: Path, clip: dict[str, Any], duration_ms: int
) -> dict[str, Any]:
    request = {
        "schema_version": SCHEMA_VERSION,
        "duration_ms": duration_ms,
        "recording_started_at_epoch_ms": clip["recording_started_at_epoch_ms"],
        "recording_ended_at_epoch_ms": clip["recording_ended_at_epoch_ms"],
        "detection_evidence": clip["detection_evidence"],
    }
    result = subprocess.run(
        [str(sampler_path)],
        input=json.dumps(request, ensure_ascii=True, separators=(",", ":")).encode("ascii"),
        capture_output=True,
        timeout=5,
        check=False,
    )
    if result.returncode != 0:
        raise RuntimeError("production sampling plan failed")
    plan = json.loads(result.stdout)
    if not isinstance(plan, dict) or set(plan) != {
        "schema_version",
        "strategy",
        "duration_ms",
        "eligible_detection_evidence_count",
        "sample_offsets_ms",
    }:
        raise ValueError("sampling plan fields do not match the production contract")
    if (
        plan["schema_version"] != SCHEMA_VERSION
        or plan["duration_ms"] != duration_ms
        or plan["strategy"] not in {"uniform_9", "yolo_guided_hybrid_9"}
        or not isinstance(plan["eligible_detection_evidence_count"], int)
        or isinstance(plan["eligible_detection_evidence_count"], bool)
        or not 0 <= plan["eligible_detection_evidence_count"] <= len(clip["detection_evidence"])
    ):
        raise ValueError("sampling plan metadata is invalid")
    offsets = plan["sample_offsets_ms"]
    if (
        not isinstance(offsets, list)
        or len(offsets) != MAX_FRAMES
        or len(set(offsets)) != MAX_FRAMES
        or any(
            not isinstance(offset, int)
            or isinstance(offset, bool)
            or not 100 <= offset <= duration_ms - 100
            for offset in offsets
        )
    ):
        raise ValueError("sampling plan offsets are invalid")
    media_end = clip["recording_started_at_epoch_ms"] + duration_ms
    has_in_window_evidence = any(
        clip["recording_started_at_epoch_ms"]
        <= item["frame_epoch_ms"]
        <= media_end
        for item in clip["detection_evidence"]
    )
    if not has_in_window_evidence or plan["strategy"] != "yolo_guided_hybrid_9":
        raise ValueError("holdout clip lacks production-eligible YOLO evidence")
    return plan


def probe_duration_seconds(video_path: Path) -> float:
    result = subprocess.run(
        [
            str(FFPROBE),
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
            str(video_path),
        ],
        capture_output=True,
        text=True,
        timeout=20,
        check=False,
    )
    if result.returncode != 0:
        raise RuntimeError("ffprobe failed")
    return float(result.stdout.strip())


def extract_frame(video_path: Path, output_path: Path, offset_ms: int) -> None:
    timestamp = f"{offset_ms / 1000.0:.3f}"
    common = ["-hide_banner", "-loglevel", "error", "-nostdin", "-y"]
    tail = ["-map", "0:v:0", "-frames:v", "1", "-an", "-sn", "-dn", "-q:v", "3"]
    attempts = [
        common + ["-ss", timestamp, "-i", str(video_path)] + tail + [str(output_path)],
        common + ["-i", str(video_path), "-ss", timestamp] + tail + [str(output_path)],
    ]
    for command in attempts:
        output_path.unlink(missing_ok=True)
        result = subprocess.run(
            [str(FFMPEG), *command],
            capture_output=True,
            timeout=20,
            check=False,
        )
        if result.returncode == 0 and output_path.is_file() and output_path.stat().st_size > 0:
            return
    raise RuntimeError("ffmpeg frame extraction failed")


def validate_classifier_output(payload: bytes, expected_indices: list[int]) -> dict[str, Any]:
    output = json.loads(payload)
    if not isinstance(output, dict):
        raise ValueError("classifier output must be an object")
    if (
        output.get("schema_version") != "1.0"
        or output.get("status") != "ok"
        or output.get("provider") != PROVIDER
        or output.get("model_name") != MODEL_NAME
        or output.get("model_sha256") != MODEL_SHA256
        or output.get("threshold_ppm") != THRESHOLD_PPM
        or output.get("sampled_frame_count") != MAX_FRAMES
    ):
        raise ValueError("classifier output does not match the EVT.1 runtime contract")
    predictions = output.get("predictions")
    if not isinstance(predictions, list) or len(predictions) != MAX_FRAMES:
        raise ValueError("classifier output frame contract mismatch")
    if not all(
        isinstance(item, dict)
        and set(item)
        == {
            "frame_index",
            "cat_probability_ppm",
            "cat_probability",
            "inference_ms",
        }
        for item in predictions
    ) or [item["frame_index"] for item in predictions] != expected_indices:
        raise ValueError("classifier output frame contract mismatch")
    probabilities = []
    for prediction in predictions:
        probability = prediction.get("cat_probability_ppm")
        if not isinstance(probability, int) or not 0 <= probability <= 1_000_000:
            raise ValueError("classifier probability is invalid")
        floating_probability = prediction.get("cat_probability")
        if (
            not isinstance(floating_probability, (int, float))
            or isinstance(floating_probability, bool)
            or not math.isfinite(float(floating_probability))
            or not 0.0 <= float(floating_probability) <= 1.0
            or round(float(floating_probability) * 1_000_000) != probability
        ):
            raise ValueError("classifier floating probability is inconsistent")
        inference_ms = prediction.get("inference_ms")
        if not isinstance(inference_ms, int) or isinstance(inference_ms, bool) or inference_ms < 0:
            raise ValueError("classifier inference time is invalid")
        probabilities.append(probability)
    positive_indices = [
        index for index, probability in zip(expected_indices, probabilities) if probability >= THRESHOLD_PPM
    ]
    predicted_cat = len(positive_indices) >= MINIMUM_POSITIVE_FRAMES
    reason_code = (
        "cat_visible"
        if predicted_cat
        else "no_cat_visible"
        if not positive_indices
        else "uncertain"
    )
    return {
        "predicted_cat": predicted_cat,
        "positive_frame_indices": positive_indices,
        "reason_code": reason_code,
        "predictions": predictions,
    }


def classify_clip(
    clip: dict[str, Any],
    work_root: Path,
    classifier_path: Path,
    model_path: Path,
    sampler_path: Path,
) -> dict[str, Any]:
    duration_seconds = probe_duration_seconds(clip["video_path"])
    duration_ms = duration_ms_from_seconds(duration_seconds)
    sampling_plan = run_production_sampler(sampler_path, clip, duration_ms)
    offsets = sampling_plan["sample_offsets_ms"]
    frame_specs = []
    for frame_index, offset_ms in enumerate(offsets, start=1):
        frame_path = work_root / f"frame-{frame_index}.jpg"
        extract_frame(clip["video_path"], frame_path, offset_ms)
        frame_specs.extend(["--frame", f"{frame_index}={frame_path}"])
    result = subprocess.run(
        [
            str(PYTHON),
            str(classifier_path),
            "--model",
            str(model_path),
            "--expected-sha256",
            MODEL_SHA256,
            "--threshold",
            "0.620000",
            "--ai-threads",
            "4",
            "--affinity",
            "12;13;14;15",
            *frame_specs,
        ],
        capture_output=True,
        timeout=90,
        check=False,
    )
    if result.returncode != 0:
        raise RuntimeError("production classifier failed")
    decision = validate_classifier_output(result.stdout, list(range(1, MAX_FRAMES + 1)))
    return {
        **decision,
        "duration_ms": duration_ms,
        "sampling_strategy": sampling_plan["strategy"],
        "eligible_detection_evidence_count": sampling_plan[
            "eligible_detection_evidence_count"
        ],
        "sample_offsets_ms": offsets,
    }


def run_manifest(manifest: dict[str, Any]) -> int:
    started = time.perf_counter()
    with tempfile.TemporaryDirectory(prefix="harbor-cat-quality-run-") as run_directory:
        run_root = Path(run_directory).resolve()
        classifier_path, classifier_sha256, _ = freeze_regular_file(
            require_regular_file(CLASSIFIER, "classifier"),
            run_root / "classifier.py",
        )
        model_path, installed_model_sha256, _ = freeze_regular_file(
            require_regular_file(MODEL, "model"),
            run_root / "model.onnx",
            expected_sha256=MODEL_SHA256,
        )
        if installed_model_sha256 != MODEL_SHA256:
            raise ValueError("installed MobileNet model SHA256 mismatch")
        sampler_path, sampler_sha256, _ = freeze_regular_file(
            require_regular_file(SAMPLER, "sampler"),
            run_root / "cat-sampling-plan",
            expected_sha256=manifest["sampler"]["sha256"],
            executable=True,
        )
        runner_sha256 = sha256(require_regular_file(Path(__file__), "quality runner"))
        for clip_index, clip in enumerate(manifest["clips"], start=1):
            clip_root = run_root / f"clip-{clip_index:05d}"
            clip_root.mkdir(mode=0o700)
            frozen_video, video_sha256, _ = freeze_regular_file(
                clip["video_path"],
                clip_root / "video.bin",
                expected_sha256=clip["video_sha256"],
            )
            frozen_clip = {**clip, "video_path": frozen_video}
            decision = classify_clip(
                frozen_clip,
                clip_root,
                classifier_path,
                model_path,
                sampler_path,
            )
            detection_evidence_sha256 = canonical_sha256(clip["detection_evidence"])
            result = {
                "schema_version": SCHEMA_VERSION,
                "kind": "cat-quality-clip-result",
                "dataset_id": manifest["dataset_id"],
                "clip_id": clip["clip_id"],
                "camera_id": clip["camera_id"],
                "video_sha256": video_sha256,
                "expected_cat": clip["expected_cat"],
                "low_light": clip["low_light"],
                "hard_negative_kind": clip["hard_negative_kind"],
                "predicted_cat": decision["predicted_cat"],
                "correct": decision["predicted_cat"] == clip["expected_cat"],
                "reason_code": decision["reason_code"],
                "positive_frame_indices": decision["positive_frame_indices"],
                "predictions": decision["predictions"],
                "sampling_strategy": decision["sampling_strategy"],
                "sample_offsets_ms": decision["sample_offsets_ms"],
                "duration_ms": decision["duration_ms"],
                "detection_evidence": clip["detection_evidence"],
                "detection_evidence_sha256": detection_evidence_sha256,
                "eligible_detection_evidence_count": decision[
                    "eligible_detection_evidence_count"
                ],
                "detector": manifest["detector"],
                "provider": PROVIDER,
                "model_name": MODEL_NAME,
                "model_sha256": installed_model_sha256,
                "classifier_sha256": classifier_sha256,
                "runner_sha256": runner_sha256,
                "sampler_sha256": sampler_sha256,
                "threshold_ppm": THRESHOLD_PPM,
                "max_frames": MAX_FRAMES,
                "minimum_positive_frames": MINIMUM_POSITIVE_FRAMES,
            }
            print(json.dumps(result, ensure_ascii=True, separators=(",", ":")), flush=True)
        summary = {
            "schema_version": SCHEMA_VERSION,
            "kind": "cat-quality-run-summary",
            "dataset_id": manifest["dataset_id"],
            "clip_count": len(manifest["clips"]),
            "completed_clip_count": len(manifest["clips"]),
            "provider": PROVIDER,
            "model_sha256": installed_model_sha256,
            "classifier_sha256": classifier_sha256,
            "runner_sha256": runner_sha256,
            "sampler_sha256": sampler_sha256,
            "detector": manifest["detector"],
            "threshold_ppm": THRESHOLD_PPM,
            "max_frames": MAX_FRAMES,
            "minimum_positive_frames": MINIMUM_POSITIVE_FRAMES,
            "elapsed_ms": round((time.perf_counter() - started) * 1000),
            "status": "complete",
        }
        print(json.dumps(summary, ensure_ascii=True, separators=(",", ":")), flush=True)
    return 0


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", default="-", help="canonical manifest path or - for stdin")
    return parser.parse_args()


def main() -> int:
    try:
        args = parse_args()
        if args.manifest == "-":
            payload = sys.stdin.buffer.read(MAX_MANIFEST_BYTES + 1)
        else:
            manifest_path = require_regular_file(Path(args.manifest), "manifest")
            if manifest_path.stat().st_size > MAX_MANIFEST_BYTES:
                raise ValueError("manifest exceeds its size limit")
            payload = manifest_path.read_bytes()
        return run_manifest(parse_manifest(payload))
    except Exception as error:
        print(f"cat_quality_runner_error={type(error).__name__}:{error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
