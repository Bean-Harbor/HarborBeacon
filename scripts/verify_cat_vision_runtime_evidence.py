#!/usr/bin/env python3
"""Verify the installed EVT.1 cat vision evidence and locked model bytes."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import stat
from pathlib import Path, PurePosixPath
from typing import Any


PACKAGE = "harboros-cat-vision-runtime"
MODEL_INSTALL_PREFIX = "/usr/share/harboros-cat-vision-runtime/models/"
RUNTIME_ROOT = "/data/vision-models/current/"
EXPECTED_MODELS = {
    "detection-coco-labels": (
        "detection/label.txt",
        621,
        "bd17f1ee35d5f3c862a4894605855abbb9dda4b0621fdb0ac4c2c8c7bb7e730a",
    ),
    "detection-yolov8n-192x320": (
        "detection/yolov8n_192x320.q.onnx",
        1925676,
        "d4bf61db2a0925a0126052212479ff5044b621b12c6793420e085d36ae6b5438",
    ),
}
EXPECTED_RUNTIME = {
    "python3-spacemit-ort": (
        "2.0.3+3",
        "python3-spacemit-ort_2.0.3+3_riscv64.deb",
        None,
        None,
    ),
    "spacemit-onnxruntime": (
        "2.0.3+3",
        "spacemit-onnxruntime_2.0.3+3_riscv64.deb",
        "b69cfc955af1ac15abf61f4915a8d5e68c50d20d38451d640d98b4b189da8472",
        {
            "installed_path": "/usr/share/doc/spacemit-onnxruntime/copyright",
            "sha256": "de3e277514b725dd7ea8e481067338850276a662f59a5f88dfc61b65a2859a69",
        },
    ),
    "spacemit-tcm": (
        "3.0.0+3",
        "spacemit-tcm_3.0.0+3_riscv64.deb",
        "2763b8946791f47fd20d4bda55e3680c3548e7a6893e605e30e1cbb4455f5fa5",
        {
            "installed_path": "/usr/share/doc/spacemit-tcm/copyright",
            "sha256": "c5ae5de0b80538d412001b735d05442650c78a1f44d4dfed61da530d5ffa5311",
        },
    ),
}
TOP_FIELDS = {
    "decision",
    "kind",
    "model_release_root",
    "models",
    "package",
    "policy",
    "runtime_packages",
    "schema_version",
    "source_commit",
}
MODEL_FIELDS = {
    "concluded_license",
    "copyright",
    "declared_license",
    "id",
    "installed_path",
    "redistribution_evidence",
    "revision",
    "runtime_path",
    "sha256",
    "size",
    "source",
    "source_archive",
}
RUNTIME_FIELDS = {
    "apt_provenance",
    "architecture",
    "artifact",
    "concluded_license",
    "copyright",
    "declared_license",
    "license_evidence",
    "name",
    "version",
}
APT_FIELDS = {
    "architecture",
    "component",
    "index_path",
    "index_sha256",
    "release_sha256",
    "repository",
    "signing_key_fingerprint",
    "suite",
}
ARTIFACT_FIELDS = {"filename", "sha256", "size", "source_package", "source_version"}
MAX_EVIDENCE_BYTES = 4 * 1024 * 1024


def canonical_bytes(value: Any) -> bytes:
    return (json.dumps(value, ensure_ascii=True, indent=2, sort_keys=True) + "\n").encode(
        "utf-8"
    )


def frozen_regular_bytes(path: Path, *, label: str, maximum: int) -> bytes:
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    if not getattr(os, "O_NOFOLLOW", 0) and path.is_symlink():
        raise ValueError(f"{label} is unsafe")
    try:
        descriptor = os.open(path, flags)
    except OSError as exc:
        raise ValueError(f"{label} is missing or unsafe") from exc
    try:
        before = os.fstat(descriptor)
        if not stat.S_ISREG(before.st_mode) or before.st_size < 1 or before.st_size > maximum:
            raise ValueError(f"{label} has an invalid size")
        remaining = before.st_size
        chunks = []
        while remaining:
            chunk = os.read(descriptor, min(1024 * 1024, remaining))
            if not chunk:
                raise ValueError(f"{label} changed while being read")
            chunks.append(chunk)
            remaining -= len(chunk)
        if os.read(descriptor, 1):
            raise ValueError(f"{label} changed while being read")
        after = os.fstat(descriptor)
        if (
            (before.st_dev, before.st_ino, before.st_size)
            != (after.st_dev, after.st_ino, after.st_size)
            or getattr(before, "st_mtime_ns", None)
            != getattr(after, "st_mtime_ns", None)
        ):
            raise ValueError(f"{label} changed while being read")
        return b"".join(chunks)
    finally:
        os.close(descriptor)


def require_fields(value: object, expected: set[str], label: str) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != expected:
        raise ValueError(f"{label} has an invalid field set")
    return value


def normalized_relative_path(value: str) -> Path:
    pure = PurePosixPath(value)
    if pure.is_absolute() or not pure.parts or "." in pure.parts or ".." in pure.parts:
        raise ValueError(f"unsafe model path: {value}")
    return Path(*pure.parts)


def verify(evidence_path: Path, model_root: Path, package_version: str, architecture: str) -> None:
    evidence_bytes = frozen_regular_bytes(
        evidence_path,
        label="vision runtime evidence",
        maximum=MAX_EVIDENCE_BYTES,
    )
    try:
        payload = json.loads(evidence_bytes.decode("utf-8"))
    except (UnicodeError, json.JSONDecodeError) as exc:
        raise ValueError(f"vision runtime evidence is invalid: {exc}") from exc
    if evidence_bytes != canonical_bytes(payload):
        raise ValueError("vision runtime evidence is not canonical JSON")
    require_fields(payload, TOP_FIELDS, "vision runtime evidence")
    if (
        payload["schema_version"] != 1
        or payload["kind"] != "vision-runtime-evidence"
        or payload["policy"] != "fail-closed"
        or payload["model_release_root"] != "/data/vision-models"
        or not re.fullmatch(r"[0-9a-f]{40}", payload.get("source_commit", ""))
    ):
        raise ValueError("vision runtime evidence identity is invalid")
    if payload["package"] != {
        "architecture": architecture,
        "name": PACKAGE,
        "version": package_version,
    }:
        raise ValueError("vision runtime evidence package identity differs")
    decision = require_fields(
        payload["decision"],
        {"blocking_reasons", "release_eligible", "status"},
        "vision runtime decision",
    )
    blockers = decision["blocking_reasons"]
    if decision["status"] not in {"approved", "blocked"} or not isinstance(blockers, list):
        raise ValueError("vision runtime decision is invalid")
    if decision["release_eligible"] is not (decision["status"] == "approved"):
        raise ValueError("vision runtime decision eligibility differs from status")
    if decision["status"] == "blocked" and (
        not blockers or not all(isinstance(item, str) and item for item in blockers)
    ):
        raise ValueError("blocked vision runtime decision has no reasons")
    if decision["status"] == "approved" and blockers:
        raise ValueError("approved vision runtime decision retains blockers")

    models = payload["models"]
    if not isinstance(models, list) or len(models) != len(EXPECTED_MODELS):
        raise ValueError("vision runtime evidence must bind exactly two model files")
    seen_models: set[str] = set()
    root = model_root.resolve(strict=True)
    if model_root.is_symlink() or not root.is_dir():
        raise ValueError("vision model root is missing or unsafe")
    for raw in models:
        model = require_fields(raw, MODEL_FIELDS, "vision model evidence")
        model_id = model.get("id")
        if model_id in seen_models or model_id not in EXPECTED_MODELS:
            raise ValueError(f"unexpected or duplicate vision model: {model_id}")
        seen_models.add(model_id)
        relative, expected_size, expected_sha = EXPECTED_MODELS[model_id]
        if (
            model["installed_path"] != MODEL_INSTALL_PREFIX + relative
            or model["runtime_path"] != RUNTIME_ROOT + relative
            or model["size"] != expected_size
            or model["sha256"] != expected_sha
            or model["source"]
            != {"kind": "git", "url": "https://gitee.com/bianbu/spacemit-demo.git"}
            or model["revision"]
            != {
                "kind": "git-commit",
                "value": "dc4477d3ea712598bb675f730642a43fe280c569",
            }
        ):
            raise ValueError(f"vision model evidence differs from the EVT.1 lock: {model_id}")
        target = root / normalized_relative_path(relative)
        model_bytes = frozen_regular_bytes(
            target,
            label=f"vision model {relative}",
            maximum=expected_size,
        )
        if len(model_bytes) != expected_size or hashlib.sha256(model_bytes).hexdigest() != expected_sha:
            raise ValueError(f"vision model bytes differ from evidence: {relative}")
        if decision["status"] == "approved" and any(
            model[field] is None
            for field in ("copyright", "redistribution_evidence", "source_archive")
        ):
            raise ValueError(f"approved vision model has incomplete evidence: {model_id}")

    runtime_packages = payload["runtime_packages"]
    if not isinstance(runtime_packages, list) or len(runtime_packages) != len(EXPECTED_RUNTIME):
        raise ValueError("vision runtime evidence must bind exactly three runtime packages")
    seen_runtime: set[str] = set()
    for raw in runtime_packages:
        runtime = require_fields(raw, RUNTIME_FIELDS, "vision runtime package")
        name = runtime.get("name")
        version = runtime.get("version")
        expected = EXPECTED_RUNTIME.get(name)
        if name in seen_runtime or expected is None or expected[0] != version:
            raise ValueError(f"unexpected or duplicate vision runtime package: {name}")
        seen_runtime.add(name)
        if runtime["architecture"] != architecture:
            raise ValueError(f"vision runtime architecture differs: {name}")
        artifact = require_fields(runtime["artifact"], ARTIFACT_FIELDS, "runtime artifact")
        provenance = require_fields(runtime["apt_provenance"], APT_FIELDS, "APT provenance")
        if (
            artifact["source_package"] != name
            or artifact["source_version"] != version
            or artifact["filename"] != expected[1]
            or artifact["sha256"] != expected[2]
            or runtime["copyright"] != expected[3]
            or provenance["architecture"] != architecture
            or provenance["repository"]
            != "https://ppa.launchpadcontent.net/spacemit/k3/ubuntu"
            or provenance["suite"] != "resolute"
            or provenance["component"] != "main"
        ):
            raise ValueError(f"vision runtime package provenance differs: {name}")
        if decision["status"] == "approved":
            required_values = [
                artifact["sha256"],
                artifact["size"],
                provenance["index_path"],
                provenance["index_sha256"],
                provenance["release_sha256"],
                provenance["signing_key_fingerprint"],
                runtime["copyright"],
                runtime["license_evidence"],
            ]
            if any(value is None for value in required_values):
                raise ValueError(f"approved vision runtime package has incomplete evidence: {name}")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--architecture", required=True)
    parser.add_argument("--evidence", type=Path, required=True)
    parser.add_argument("--model-root", type=Path, required=True)
    parser.add_argument("--package-version", required=True)
    args = parser.parse_args()
    try:
        verify(args.evidence, args.model_root, args.package_version, args.architecture)
    except (OSError, ValueError) as exc:
        raise SystemExit(f"vision runtime evidence verification failed: {exc}") from exc
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
