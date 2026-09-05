#!/usr/bin/env python3
"""Validate and optionally stage the exact K3 release model materials."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import stat
import sys
from pathlib import Path, PurePosixPath


LOCKED_STATE = "locked"
RELEASE_ID = "harboros-navi-k3-0.1.0-evt.1"
TOP_FIELDS = {"schema_version", "release_id", "materials"}
MATERIAL_FIELDS = {"files", "id", "license", "revision", "role", "source", "state"}
FILE_FIELDS = {"package_path", "sha256", "size", "source_path"}
V1_LICENSE_FIELDS = {
    "blocking_reason",
    "concluded_license",
    "declared_license",
    "evidence",
    "review_status",
}
V2_LICENSE_FIELDS = {
    "blocking_reason",
    "concluded_license",
    "declared_license",
    "distribution_license_present",
    "evidence_files",
    "evidence_verified",
    "notice_review",
    "notice_status",
    "review_status",
}
EVIDENCE_FILE_FIELDS = {
    "filename",
    "id",
    "installed_path",
    "kind",
    "purpose",
    "revision",
    "sha256",
    "source",
}
NOTICE_REVIEW_FIELDS = {
    "kind",
    "license_paths",
    "notice_paths",
    "review_status",
    "revision",
    "source",
    "tree_sha256",
}
VISION_V1_IDS = {"detection-coco-labels", "detection-yolov8n-192x320"}
MODEL_LICENSE_PREFIX = "/usr/share/doc/harboros-model-runtime/model-licenses/"
MAX_LICENSE_EVIDENCE_BYTES = 8 * 1024 * 1024
MAX_MANIFEST_BYTES = 4 * 1024 * 1024

EXPECTED_MODEL_EVIDENCE = {
    "semantic-router-bootstrap-llm": [
        {
            "filename": "LICENSE",
            "id": "model-license-qwen2.5-1.5b-instruct-gguf",
            "installed_path": (
                "/usr/share/doc/harboros-model-runtime/model-licenses/"
                "qwen2.5-1.5b-instruct-gguf/LICENSE"
            ),
            "kind": "model-license-qwen2.5-1.5b-instruct-gguf",
            "purpose": "distribution-license",
            "revision": "91cad51170dc346986eccefdc2dd33a9da36ead9",
            "sha256": "832dd9e00a68dd83b3c3fb9f5588dad7dcf337a0db50f7d9483f310cd292e92e",
            "source": (
                "https://huggingface.co/Qwen/Qwen2.5-1.5B-Instruct-GGUF/resolve/"
                "91cad51170dc346986eccefdc2dd33a9da36ead9/LICENSE"
            ),
        }
    ],
    "rag-embedding-model": [
        {
            "filename": "README.md",
            "id": "model-license-declaration-jina-embeddings-v2-base-zh",
            "installed_path": (
                "/usr/share/doc/harboros-model-runtime/model-licenses/"
                "jina-embeddings-v2-base-zh/README.md"
            ),
            "kind": "model-license-declaration-jina-embeddings-v2-base-zh",
            "purpose": "license-declaration",
            "revision": "998b9133910ffcb127a7bff233f41db6ed9be4d2",
            "sha256": "d94c902fe88c81437eb2a1877d21c829a967351c38bc720f4516c312632c1b33",
            "source": (
                "https://huggingface.co/jinaai/jina-embeddings-v2-base-zh/resolve/"
                "998b9133910ffcb127a7bff233f41db6ed9be4d2/README.md"
            ),
        },
        {
            "filename": "LICENSE",
            "id": "model-distribution-license-jina-embeddings-v2-base-zh",
            "installed_path": (
                "/usr/share/doc/harboros-model-runtime/model-licenses/"
                "jina-embeddings-v2-base-zh/LICENSE"
            ),
            "kind": "model-distribution-license-jina-embeddings-v2-base-zh",
            "purpose": "distribution-license",
            "revision": "Apache-2.0",
            "sha256": "cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30",
            "source": "https://www.apache.org/licenses/LICENSE-2.0.txt",
        },
    ],
}
EXPECTED_NOTICE_REVIEWS = {
    "semantic-router-bootstrap-llm": {
        "license_paths": ["LICENSE"],
        "revision": "91cad51170dc346986eccefdc2dd33a9da36ead9",
        "source": (
            "https://huggingface.co/api/models/Qwen/Qwen2.5-1.5B-Instruct-GGUF/tree/"
            "91cad51170dc346986eccefdc2dd33a9da36ead9?recursive=true&expand=false"
        ),
    },
    "rag-embedding-model": {
        "license_paths": [],
        "revision": "998b9133910ffcb127a7bff233f41db6ed9be4d2",
        "source": (
            "https://huggingface.co/api/models/jinaai/jina-embeddings-v2-base-zh/tree/"
            "998b9133910ffcb127a7bff233f41db6ed9be4d2?recursive=true&expand=false"
        ),
    },
}


def is_lower_sha256(value: object) -> bool:
    return (
        isinstance(value, str)
        and len(value) == 64
        and all(character in "0123456789abcdef" for character in value)
    )


def relative_path(value: object, field: str) -> PurePosixPath:
    if not isinstance(value, str) or not value:
        raise ValueError(f"{field} must be a non-empty string")
    path = PurePosixPath(value)
    if path.is_absolute() or ".." in path.parts or "." in path.parts:
        raise ValueError(f"{field} must be a normalized relative path: {value}")
    return path


def installed_path(value: object, field: str) -> PurePosixPath:
    if not isinstance(value, str) or not value.startswith(MODEL_LICENSE_PREFIX):
        raise ValueError(f"{field} must be below {MODEL_LICENSE_PREFIX}")
    path = PurePosixPath(value)
    if not path.is_absolute() or ".." in path.parts or "." in path.parts:
        raise ValueError(f"{field} must be a normalized absolute path: {value}")
    return path


def open_regular_file(path: Path, *, max_bytes: int, label: str) -> tuple[int, int]:
    try:
        before = os.lstat(path)
        if not stat.S_ISREG(before.st_mode):
            raise ValueError(f"{label} is missing or unsafe: {path}")
        flags = os.O_RDONLY | getattr(os, "O_BINARY", 0) | getattr(os, "O_NOFOLLOW", 0)
        descriptor = os.open(path, flags)
        opened = os.fstat(descriptor)
    except (OSError, ValueError) as exc:
        if isinstance(exc, ValueError):
            raise
        raise ValueError(f"unable to open {label}: {path}") from exc
    if (
        not stat.S_ISREG(opened.st_mode)
        or opened.st_dev != before.st_dev
        or opened.st_ino != before.st_ino
        or opened.st_size < 1
        or opened.st_size > max_bytes
    ):
        os.close(descriptor)
        raise ValueError(f"{label} is missing, unsafe, or has an invalid size: {path}")
    return descriptor, opened.st_size


def digest_and_stage(
    path: Path,
    *,
    max_bytes: int,
    label: str,
    destination: Path | None = None,
) -> tuple[int, str]:
    descriptor, expected_size = open_regular_file(path, max_bytes=max_bytes, label=label)
    digest = hashlib.sha256()
    written = 0
    output = None
    try:
        if destination is not None:
            destination.parent.mkdir(parents=True, exist_ok=True)
            output = destination.open("wb")
        while chunk := os.read(descriptor, 1024 * 1024):
            written += len(chunk)
            if written > max_bytes:
                raise ValueError(f"{label} exceeds its size limit: {path}")
            digest.update(chunk)
            if output is not None:
                output.write(chunk)
    except OSError as exc:
        raise ValueError(f"unable to read {label}: {path}") from exc
    finally:
        os.close(descriptor)
        if output is not None:
            output.close()
    if written != expected_size:
        raise ValueError(f"{label} changed while being read: {path}")
    if destination is not None:
        destination.chmod(0o644)
    return written, digest.hexdigest()


def read_regular_bytes(path: Path, *, max_bytes: int, label: str) -> bytes:
    descriptor, expected_size = open_regular_file(path, max_bytes=max_bytes, label=label)
    payload = bytearray()
    try:
        while chunk := os.read(descriptor, min(1024 * 1024, max_bytes + 1)):
            payload.extend(chunk)
            if len(payload) > max_bytes:
                raise ValueError(f"{label} exceeds its size limit: {path}")
    except OSError as exc:
        raise ValueError(f"unable to read {label}: {path}") from exc
    finally:
        os.close(descriptor)
    if len(payload) != expected_size:
        raise ValueError(f"{label} changed while being read: {path}")
    return bytes(payload)


def validate_v1_vision_license(material_id: str, review: object, errors: list[str]) -> None:
    if not isinstance(review, dict) or set(review) != V1_LICENSE_FIELDS:
        errors.append(f"{material_id} has an invalid legacy vision license review")
        return
    if review.get("review_status") != "blocked" or review.get("evidence") is not None:
        errors.append("schema v1 is allowed only for blocked pointer-free cat vision materials")
    blocker = review.get("blocking_reason")
    if not isinstance(blocker, str) or not blocker:
        errors.append(f"{material_id} has no license blocking reason")


def validate_v2_license(
    material_id: str,
    material_revision: object,
    review: object,
    evidence_ids: set[str],
    installed_paths: set[str],
    errors: list[str],
) -> None:
    if not isinstance(review, dict) or set(review) != V2_LICENSE_FIELDS:
        errors.append(f"{material_id} has an invalid schema v2 license review")
        return
    for field in ("declared_license", "concluded_license"):
        if not isinstance(review.get(field), str) or not review[field]:
            errors.append(f"{material_id} has an invalid {field}")
    if not isinstance(review.get("distribution_license_present"), bool):
        errors.append(f"{material_id} distribution_license_present must be boolean")
    elif review.get("distribution_license_present") is not True:
        errors.append(f"{material_id} has no distribution license evidence")
    if not isinstance(review.get("evidence_verified"), bool):
        errors.append(f"{material_id} evidence_verified must be boolean")
    elif review.get("evidence_verified") is not True:
        errors.append(f"{material_id} model license evidence is not verified")

    evidence_files = review.get("evidence_files")
    expected_evidence = EXPECTED_MODEL_EVIDENCE.get(material_id)
    if not isinstance(evidence_files, list) or not evidence_files:
        errors.append(f"{material_id} has no ordered license evidence files")
    elif expected_evidence is None or evidence_files != expected_evidence:
        errors.append(f"{material_id} license evidence identity or order changed")
    else:
        for index, evidence in enumerate(evidence_files):
            label = f"{material_id}.license.evidence_files[{index}]"
            if not isinstance(evidence, dict) or set(evidence) != EVIDENCE_FILE_FIELDS:
                errors.append(f"{label} has an invalid field set")
                continue
            evidence_id = evidence.get("id")
            if evidence_id != evidence.get("kind") or evidence_id in evidence_ids:
                errors.append(f"{label} id/kind is invalid or duplicated")
            else:
                evidence_ids.add(evidence_id)
            try:
                path = installed_path(evidence.get("installed_path"), f"{label}.installed_path")
            except ValueError as exc:
                errors.append(str(exc))
            else:
                path_value = path.as_posix()
                if path_value in installed_paths:
                    errors.append(f"duplicate model license installed path: {path_value}")
                installed_paths.add(path_value)
                if evidence.get("filename") != path.name:
                    errors.append(f"{label} filename differs from installed path")
            if not is_lower_sha256(evidence.get("sha256")):
                errors.append(f"{label}.sha256 must be a lowercase SHA256")
            if evidence.get("purpose") not in {
                "distribution-license",
                "license-declaration",
            }:
                errors.append(f"{label}.purpose is invalid")
            source = evidence.get("source")
            revision = evidence.get("revision")
            if not isinstance(source, str) or not source.startswith("https://"):
                errors.append(f"{label}.source must be HTTPS")
            if not isinstance(revision, str) or not revision:
                errors.append(f"{label}.revision must be explicit")
            if evidence.get("purpose") == "license-declaration" and revision != material_revision:
                errors.append(f"{label} is not bound to the model revision")

    notice = review.get("notice_review")
    expected_notice = EXPECTED_NOTICE_REVIEWS.get(material_id)
    if not isinstance(notice, dict) or set(notice) != NOTICE_REVIEW_FIELDS:
        errors.append(f"{material_id} has an invalid notice review")
        notice_status = None
    else:
        notice_status = notice.get("review_status")
        if (
            expected_notice is None
            or notice.get("kind") != "pinned-source-tree-review"
            or notice.get("source") != expected_notice["source"]
            or notice.get("revision") != expected_notice["revision"]
            or notice.get("license_paths") != expected_notice["license_paths"]
            or notice.get("notice_paths") != []
        ):
            errors.append(f"{material_id} notice review identity changed")
        tree_sha = notice.get("tree_sha256")
        if notice_status == "verified":
            if not is_lower_sha256(tree_sha):
                errors.append(f"{material_id} verified notice review has no tree SHA256")
        elif notice_status == "blocked":
            if tree_sha is not None:
                errors.append(f"{material_id} blocked notice review must use null tree SHA256")
        else:
            errors.append(f"{material_id} notice review status is invalid")

    review_status = review.get("review_status")
    blocker = review.get("blocking_reason")
    notice_value = review.get("notice_status")
    approved = (
        review_status == "approved"
        and review.get("distribution_license_present") is True
        and review.get("evidence_verified") is True
        and notice_status == "verified"
        and notice_value == "not-required-with-reviewed-basis"
    )
    if review_status == "approved":
        if not approved or blocker is not None:
            errors.append(f"{material_id} license approval is not fully evidenced")
    elif review_status == "blocked":
        if not isinstance(blocker, str) or not blocker:
            errors.append(f"{material_id} has no license blocking reason")
        if notice_status == "blocked" and notice_value != "review-required":
            errors.append(f"{material_id} blocked notice review must remain review-required")
    else:
        errors.append(f"{material_id} has an invalid license review status")


def validate_manifest(payload: object, bundle_root: Path | None, stage: Path | None) -> list[str]:
    errors: list[str] = []
    if not isinstance(payload, dict) or set(payload) != TOP_FIELDS:
        return ["manifest has an invalid top-level shape"]
    if payload.get("release_id") != RELEASE_ID:
        errors.append("release_id does not identify EVT.1")
    materials = payload.get("materials")
    if not isinstance(materials, list) or not materials:
        return [*errors, "materials must be a non-empty list"]
    material_ids = {
        material.get("id") for material in materials if isinstance(material, dict)
    }
    schema = payload.get("schema_version")
    legacy_vision = schema == 1 and material_ids == VISION_V1_IDS
    if schema != 2 and not legacy_vision:
        errors.append("model runtime manifests must use schema_version 2")

    ids: set[str] = set()
    package_paths: set[str] = set()
    evidence_ids: set[str] = set()
    evidence_installed_paths: set[str] = set()
    for index, material in enumerate(materials):
        label = f"materials[{index}]"
        if not isinstance(material, dict) or set(material) != MATERIAL_FIELDS:
            errors.append(f"{label} has an invalid field set")
            continue
        material_id = material.get("id")
        if not isinstance(material_id, str) or not material_id:
            errors.append(f"{label}.id must be a non-empty string")
            material_id = label
        elif material_id in ids:
            errors.append(f"duplicate material id: {material_id}")
        ids.add(material_id)
        if material.get("state") != LOCKED_STATE:
            errors.append(f"{material_id} is not locked")
        revision = material.get("revision")
        if not isinstance(revision, str) or not revision or revision.lower() in {
            "main",
            "master",
            "latest",
        }:
            errors.append(f"{material_id} has no immutable revision")
        source = material.get("source")
        if not isinstance(source, str) or not source.startswith("https://"):
            errors.append(f"{material_id} has no HTTPS source")
        if legacy_vision:
            validate_v1_vision_license(material_id, material.get("license"), errors)
        else:
            validate_v2_license(
                material_id,
                revision,
                material.get("license"),
                evidence_ids,
                evidence_installed_paths,
                errors,
            )

        files = material.get("files")
        if not isinstance(files, list) or not files:
            errors.append(f"{material_id} has no locked files")
            continue
        for file_index, file_entry in enumerate(files):
            file_label = f"{material_id}.files[{file_index}]"
            if not isinstance(file_entry, dict) or set(file_entry) != FILE_FIELDS:
                errors.append(f"{file_label} has an invalid shape")
                continue
            try:
                source_path = relative_path(file_entry["source_path"], f"{file_label}.source_path")
                package_path = relative_path(file_entry["package_path"], f"{file_label}.package_path")
            except (KeyError, ValueError) as exc:
                errors.append(str(exc))
                continue
            package_key = package_path.as_posix()
            if package_key in package_paths:
                errors.append(f"duplicate package path: {package_key}")
            package_paths.add(package_key)
            expected_size = file_entry.get("size")
            expected_sha = file_entry.get("sha256")
            if not isinstance(expected_size, int) or expected_size < 1:
                errors.append(f"{file_label}.size must be a positive integer")
                continue
            if not is_lower_sha256(expected_sha):
                errors.append(f"{file_label}.sha256 must be a lowercase SHA256")
                continue
            if bundle_root is None:
                continue
            source_file = bundle_root.joinpath(*source_path.parts)
            destination = stage.joinpath(*package_path.parts) if stage is not None else None
            try:
                actual_size, actual_sha = digest_and_stage(
                    source_file,
                    max_bytes=expected_size,
                    label=f"model material {source_path.as_posix()}",
                    destination=destination,
                )
            except ValueError as exc:
                errors.append(str(exc))
                continue
            if actual_size != expected_size:
                errors.append(f"size mismatch: {source_path.as_posix()}")
            if actual_sha != expected_sha:
                errors.append(f"SHA256 mismatch: {source_path.as_posix()}")
    return errors


def verify_and_stage_license_evidence(
    payload: dict[str, object], evidence_root: Path, stage_root: Path | None
) -> list[str]:
    if payload.get("schema_version") != 2:
        return ["license evidence verification requires a schema v2 model manifest"]
    errors: list[str] = []
    for material in payload["materials"]:
        for evidence in material["license"]["evidence_files"]:
            evidence_file = evidence_root / evidence["sha256"]
            destination = None
            if stage_root is not None:
                relative = installed_path(
                    evidence["installed_path"],
                    f"{evidence['id']}.installed_path",
                ).relative_to("/")
                destination = stage_root.joinpath(*relative.parts)
            try:
                size, digest = digest_and_stage(
                    evidence_file,
                    max_bytes=MAX_LICENSE_EVIDENCE_BYTES,
                    label=f"model license evidence {evidence['id']}",
                    destination=destination,
                )
            except ValueError as exc:
                errors.append(str(exc))
                continue
            if size < 1 or digest != evidence["sha256"]:
                errors.append(f"{evidence['id']} local license evidence SHA256 changed")
    return errors


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--bundle-root", type=Path)
    parser.add_argument("--stage", type=Path)
    parser.add_argument("--expect-blocked", action="store_true")
    parser.add_argument("--verify-license-evidence", action="store_true")
    parser.add_argument("--license-evidence-root", type=Path)
    parser.add_argument("--license-stage-root", type=Path)
    parser.add_argument("--manifest-stage", type=Path)
    args = parser.parse_args()
    if args.stage is not None and args.bundle_root is None:
        parser.error("--stage requires --bundle-root")
    if args.verify_license_evidence and args.license_evidence_root is None:
        parser.error("--verify-license-evidence requires --license-evidence-root")
    if args.license_evidence_root is not None and not args.verify_license_evidence:
        parser.error("--license-evidence-root requires --verify-license-evidence")
    if args.license_stage_root is not None and args.license_evidence_root is None:
        parser.error("--license-stage-root requires --license-evidence-root")
    if args.license_evidence_root is not None and (
        not args.license_evidence_root.is_dir() or args.license_evidence_root.is_symlink()
    ):
        parser.error("--license-evidence-root must be a safe directory")
    try:
        manifest_bytes = read_regular_bytes(
            args.manifest, max_bytes=MAX_MANIFEST_BYTES, label="model manifest"
        )
        payload = json.loads(manifest_bytes.decode("utf-8"))
    except (ValueError, UnicodeError, json.JSONDecodeError) as exc:
        parser.error(f"invalid model manifest: {exc}")
    errors = validate_manifest(payload, args.bundle_root, args.stage)
    if args.verify_license_evidence and not errors:
        errors.extend(
            verify_and_stage_license_evidence(
                payload, args.license_evidence_root, args.license_stage_root
            )
        )
    if args.expect_blocked:
        if not errors:
            print("error: model manifest unexpectedly became release-ready", file=sys.stderr)
            return 1
        print("model manifest is intentionally blocked:")
        for error in errors:
            print(f"- {error}")
        return 0
    if errors:
        for error in errors:
            print(f"error: {error}", file=sys.stderr)
        return 2
    if args.manifest_stage is not None:
        args.manifest_stage.parent.mkdir(parents=True, exist_ok=True)
        args.manifest_stage.write_bytes(manifest_bytes)
        args.manifest_stage.chmod(0o644)
    if args.verify_license_evidence:
        print("model material and license evidence byte locks are verified")
    else:
        print("model material byte lock and manifest shape are verified")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
