#!/usr/bin/env python3
"""Validate and optionally stage the exact K3 release model materials."""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import sys
from pathlib import Path, PurePosixPath


LOCKED_STATE = "locked"
LICENSE_FIELDS = {
    "blocking_reason",
    "concluded_license",
    "declared_license",
    "evidence",
    "review_status",
}
LICENSE_EVIDENCE_FIELDS = {"kind", "sha256", "source"}


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def relative_path(value: object, field: str) -> PurePosixPath:
    if not isinstance(value, str) or not value:
        raise ValueError(f"{field} must be a non-empty string")
    path = PurePosixPath(value)
    if path.is_absolute() or ".." in path.parts or "." in path.parts:
        raise ValueError(f"{field} must be a normalized relative path: {value}")
    return path


def validate_manifest(payload: object, bundle_root: Path | None, stage: Path | None) -> list[str]:
    errors: list[str] = []
    if not isinstance(payload, dict) or set(payload) != {
        "schema_version",
        "release_id",
        "materials",
    }:
        return ["manifest has an invalid top-level shape"]
    if payload["schema_version"] != 1:
        errors.append("schema_version must be 1")
    if payload["release_id"] != "harboros-navi-k3-0.1.0-evt.1":
        errors.append("release_id does not identify EVT.1")
    materials = payload["materials"]
    if not isinstance(materials, list) or not materials:
        return [*errors, "materials must be a non-empty list"]

    ids: set[str] = set()
    package_paths: set[str] = set()
    for index, material in enumerate(materials):
        label = f"materials[{index}]"
        if not isinstance(material, dict):
            errors.append(f"{label} must be an object")
            continue
        material_id = material.get("id")
        if not isinstance(material_id, str) or not material_id:
            errors.append(f"{label}.id must be a non-empty string")
            material_id = label
        elif material_id in ids:
            errors.append(f"duplicate material id: {material_id}")
        ids.add(material_id)

        state = material.get("state")
        if state != LOCKED_STATE:
            blocker = material.get("blocker", "no blocker recorded")
            errors.append(f"{material_id} is {state!r}: {blocker}")
        revision = material.get("revision")
        if not isinstance(revision, str) or not revision or revision.lower() in {
            "main",
            "master",
            "latest",
        }:
            errors.append(f"{material_id} has no immutable revision")
        source = material.get("source")
        if not isinstance(source, str) or not source:
            errors.append(f"{material_id} has no source")
        license_review = material.get("license")
        if not isinstance(license_review, dict) or set(license_review) != LICENSE_FIELDS:
            errors.append(f"{material_id} has an invalid license review")
        else:
            review_status = license_review.get("review_status")
            if review_status not in {"approved", "blocked", "declared"}:
                errors.append(f"{material_id} has an invalid license review status")
            for field in ("declared_license", "concluded_license"):
                if not isinstance(license_review.get(field), str) or not license_review[field]:
                    errors.append(f"{material_id} has an invalid {field}")
            blocking_reason = license_review.get("blocking_reason")
            if review_status == "approved" and blocking_reason is not None:
                errors.append(f"{material_id} repeats a blocker after approval")
            if review_status != "approved" and (
                not isinstance(blocking_reason, str) or not blocking_reason
            ):
                errors.append(f"{material_id} has no license blocking reason")
            evidence = license_review.get("evidence")
            if evidence is not None:
                if not isinstance(evidence, dict) or set(evidence) != LICENSE_EVIDENCE_FIELDS:
                    errors.append(f"{material_id} has invalid license evidence")
                else:
                    evidence_sha = evidence.get("sha256")
                    if (
                        not isinstance(evidence_sha, str)
                        or len(evidence_sha) != 64
                        or any(character not in "0123456789abcdef" for character in evidence_sha)
                    ):
                        errors.append(f"{material_id} has invalid license evidence SHA256")
                    for field in ("kind", "source"):
                        if not isinstance(evidence.get(field), str) or not evidence[field]:
                            errors.append(f"{material_id} has invalid license evidence {field}")
            elif review_status == "approved":
                errors.append(f"{material_id} approval has no license evidence")
        files = material.get("files")
        if not isinstance(files, list) or not files:
            errors.append(f"{material_id} has no locked files")
            continue
        for file_index, file_entry in enumerate(files):
            file_label = f"{material_id}.files[{file_index}]"
            if not isinstance(file_entry, dict) or set(file_entry) != {
                "source_path",
                "package_path",
                "size",
                "sha256",
            }:
                errors.append(f"{file_label} has an invalid shape")
                continue
            try:
                source_path = relative_path(file_entry["source_path"], f"{file_label}.source_path")
                package_path = relative_path(file_entry["package_path"], f"{file_label}.package_path")
            except ValueError as error:
                errors.append(str(error))
                continue
            package_key = package_path.as_posix()
            if package_key in package_paths:
                errors.append(f"duplicate package path: {package_key}")
            package_paths.add(package_key)
            expected_size = file_entry["size"]
            expected_sha = file_entry["sha256"]
            if not isinstance(expected_size, int) or expected_size < 1:
                errors.append(f"{file_label}.size must be a positive integer")
            if (
                not isinstance(expected_sha, str)
                or len(expected_sha) != 64
                or any(character not in "0123456789abcdef" for character in expected_sha)
            ):
                errors.append(f"{file_label}.sha256 must be a lowercase SHA256")
            if bundle_root is None:
                continue
            source_file = bundle_root.joinpath(*source_path.parts)
            if not source_file.is_file() or source_file.is_symlink():
                errors.append(f"missing or unsafe material file: {source_path.as_posix()}")
                continue
            if isinstance(expected_size, int) and source_file.stat().st_size != expected_size:
                errors.append(f"size mismatch: {source_path.as_posix()}")
            if isinstance(expected_sha, str) and sha256(source_file) != expected_sha:
                errors.append(f"SHA256 mismatch: {source_path.as_posix()}")
            if stage is not None and not errors:
                destination = stage.joinpath(*package_path.parts)
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(source_file, destination)

    return errors


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--bundle-root", type=Path)
    parser.add_argument("--stage", type=Path)
    parser.add_argument("--expect-blocked", action="store_true")
    args = parser.parse_args()
    if args.stage is not None and args.bundle_root is None:
        parser.error("--stage requires --bundle-root")
    payload = json.loads(args.manifest.read_text(encoding="utf-8"))
    errors = validate_manifest(payload, args.bundle_root, args.stage)
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
    print("model material byte lock and files are verified")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
