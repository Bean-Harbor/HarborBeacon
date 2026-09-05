#!/usr/bin/env python3
"""Generate HarborOS-canonical release materials for a final Beacon deb."""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import tomllib
from pathlib import Path, PurePosixPath
from typing import Any

SCRIPT_DIR = Path(__file__).resolve().parent
if str(SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPT_DIR))

from model_runtime_dependency_contract import (
    CONTROL_URI,
    MAX_CONTROL_BYTES,
    load_dependency_contract,
    load_dependency_contract_bytes,
)


SOURCE_REPOSITORY = "https://github.com/Bean-Harbor/HarborBeacon"
ROOT_LICENSE = "LicenseRef-Harbor-Innovations-Proprietary"
ROOT_COPYRIGHT = "Copyright (c) Harbor Innovations"
SPDX_ID_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9.-]*\+?$")
LOCAL_LICENSE_REF_RE = re.compile(
    r"(?<![A-Za-z0-9.+:-])LicenseRef-[A-Za-z0-9.-]+(?![A-Za-z0-9.+-])"
)
LICENSE_FILE_RE = re.compile(
    r"^(?:LICENSE|LICENCE|COPYING|NOTICE|UNLICENSE)(?:[._-].*)?$",
    re.IGNORECASE,
)
MAX_LICENSE_EVIDENCE_BYTES = 4 * 1024 * 1024
MAX_THIRD_PARTY_LICENSE_BYTES = 64 * 1024 * 1024
MODEL_LICENSE_EVIDENCE_FIELDS = {
    "filename",
    "id",
    "installed_path",
    "kind",
    "purpose",
    "revision",
    "sha256",
    "source",
}
MODEL_LICENSE_FIELDS = {
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
MODEL_NOTICE_REVIEW_FIELDS = {
    "kind",
    "license_paths",
    "notice_paths",
    "review_status",
    "revision",
    "source",
    "tree_sha256",
}


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def canonical_bytes(value: Any) -> bytes:
    return (json.dumps(value, ensure_ascii=True, indent=2, sort_keys=True) + "\n").encode(
        "utf-8"
    )


def load_json(path: Path, label: str) -> dict[str, Any]:
    try:
        payload = read_license_evidence_bytes(
            path, label, max_bytes=MAX_THIRD_PARTY_LICENSE_BYTES
        )
        value = json.loads(payload.decode("utf-8"))
    except (ValueError, UnicodeError, json.JSONDecodeError) as exc:
        raise ValueError(f"invalid {label}: {exc}") from exc
    if not isinstance(value, dict):
        raise ValueError(f"{label} must be a JSON object")
    return value


def load_canonical_json(path: Path, label: str) -> dict[str, Any]:
    try:
        payload = read_license_evidence_bytes(
            path, label, max_bytes=MAX_THIRD_PARTY_LICENSE_BYTES
        )
        value = json.loads(payload.decode("utf-8"))
    except (ValueError, UnicodeError, json.JSONDecodeError) as exc:
        raise ValueError(f"invalid {label}: {exc}") from exc
    if not isinstance(value, dict):
        raise ValueError(f"{label} must be a JSON object")
    if payload != canonical_bytes(value):
        raise ValueError(f"{label} is not canonical JSON: {path}")
    return value


def write_json(path: Path, value: dict[str, Any]) -> None:
    path.write_bytes(canonical_bytes(value))


def read_utf8_bytes(path: Path, label: str) -> tuple[bytes, str]:
    if not path.is_file() or path.is_symlink():
        raise ValueError(f"{label} is missing or unsafe: {path}")
    try:
        payload = path.read_bytes()
        text = payload.decode("utf-8")
    except (OSError, UnicodeError) as exc:
        raise ValueError(f"{label} must be exact UTF-8 bytes: {path}") from exc
    if not payload or text.encode("utf-8") != payload:
        raise ValueError(f"{label} must be non-empty exact UTF-8 bytes: {path}")
    return payload, text


def read_license_evidence_bytes(
    path: Path, label: str, *, max_bytes: int = MAX_LICENSE_EVIDENCE_BYTES
) -> bytes:
    try:
        before = os.lstat(path)
        if not stat.S_ISREG(before.st_mode):
            raise ValueError(f"{label} is missing or unsafe: {path}")
        descriptor = os.open(
            path,
            os.O_RDONLY | getattr(os, "O_BINARY", 0) | getattr(os, "O_NOFOLLOW", 0),
        )
        opened = os.fstat(descriptor)
    except OSError as exc:
        raise ValueError(f"unable to open {label}: {path}") from exc
    if (
        not stat.S_ISREG(opened.st_mode)
        or opened.st_dev != before.st_dev
        or opened.st_ino != before.st_ino
        or opened.st_size <= 0
        or opened.st_size > max_bytes
    ):
        os.close(descriptor)
        raise ValueError(f"{label} is missing, unsafe, or has an invalid size: {path}")
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
    if len(payload) != opened.st_size:
        raise ValueError(f"{label} changed while being read: {path}")
    return bytes(payload)


def local_license_refs(value: dict[str, Any]) -> set[str]:
    refs: set[str] = set()
    for collection, fields in (
        (value.get("packages"), ("licenseDeclared", "licenseConcluded", "licenseInfoFromFiles")),
        (value.get("files"), ("licenseConcluded", "licenseInfoInFiles")),
    ):
        if not isinstance(collection, list):
            continue
        for item in collection:
            if not isinstance(item, dict):
                continue
            for field in fields:
                raw = item.get(field)
                expressions = raw if isinstance(raw, list) else [raw]
                for expression in expressions:
                    if isinstance(expression, str):
                        refs.update(LOCAL_LICENSE_REF_RE.findall(expression))
    return refs


def verify_spdx_extracted_licenses(
    value: dict[str, Any], *, root_license: str, notice_bytes: bytes
) -> None:
    refs = local_license_refs(value)
    extracted = value.get("hasExtractedLicensingInfos")
    if not isinstance(extracted, list):
        raise ValueError("SPDX SBOM omits extracted text for local LicenseRef values")
    by_id: dict[str, dict[str, Any]] = {}
    for item in extracted:
        if not isinstance(item, dict):
            raise ValueError("SPDX SBOM has an invalid extracted license entry")
        license_id = item.get("licenseId")
        text = item.get("extractedText")
        if (
            not isinstance(license_id, str)
            or license_id in by_id
            or not isinstance(text, str)
            or not text.strip()
            or text.strip() in {"NONE", "NOASSERTION"}
        ):
            raise ValueError("SPDX SBOM has an invalid extracted license entry")
        by_id[license_id] = item
    if set(by_id) != refs:
        raise ValueError("SPDX SBOM extracted licenses differ from local LicenseRef values")
    root_refs = set(LOCAL_LICENSE_REF_RE.findall(root_license))
    if root_refs != {ROOT_LICENSE}:
        raise ValueError("root package license must identify the approved local LicenseRef")
    try:
        extracted_bytes = by_id[ROOT_LICENSE]["extractedText"].encode("utf-8")
    except KeyError as exc:
        raise ValueError("SPDX SBOM omits the root LicenseRef extracted text") from exc
    if extracted_bytes != notice_bytes:
        raise ValueError(
            "SPDX root LicenseRef text differs from FIRST_PARTY_RIGHTS.txt bytes"
        )


def normalize_license_expression(value: object) -> str:
    if not isinstance(value, str) or not value.strip():
        return "NOASSERTION"
    return re.sub(r"\s+", " ", re.sub(r"\s*/\s*", " OR ", value.strip()))


def valid_spdx_expression(value: str) -> bool:
    if value in {"NOASSERTION", "NONE"}:
        return False
    tokens = value.replace("(", " ( ").replace(")", " ) ").split()
    position = 0

    def primary() -> bool:
        nonlocal position
        if position >= len(tokens):
            return False
        if tokens[position] == "(":
            position += 1
            if not expression() or position >= len(tokens) or tokens[position] != ")":
                return False
            position += 1
            return True
        if not SPDX_ID_RE.fullmatch(tokens[position]):
            return False
        position += 1
        if position < len(tokens) and tokens[position] == "WITH":
            position += 1
            if position >= len(tokens) or not SPDX_ID_RE.fullmatch(tokens[position]):
                return False
            position += 1
        return True

    def expression() -> bool:
        nonlocal position
        if not primary():
            return False
        while position < len(tokens) and tokens[position] in {"AND", "OR"}:
            position += 1
            if not primary():
                return False
        return True

    return bool(tokens) and expression() and position == len(tokens)


def cargo_lock_packages(cargo_lock: Path) -> dict[tuple[str, str, str], dict[str, Any]]:
    try:
        payload = tomllib.loads(cargo_lock.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, tomllib.TOMLDecodeError) as exc:
        raise ValueError(f"invalid Cargo.lock: {exc}") from exc
    packages: dict[tuple[str, str, str], dict[str, Any]] = {}
    for package in payload.get("package", []):
        if not isinstance(package, dict):
            raise ValueError("Cargo.lock contains an invalid package entry")
        name = package.get("name")
        version = package.get("version")
        source = package.get("source") or "vendored-path"
        if not all(isinstance(value, str) and value for value in (name, version, source)):
            raise ValueError("Cargo.lock contains an incomplete package identity")
        identity = (name, version, source)
        if identity in packages:
            raise ValueError(f"Cargo.lock repeats package identity {name}@{version}")
        packages[identity] = package
    return packages


def _cargo_license_inventory(
    metadata_path: Path,
    root_manifest: Path,
    cargo_lock: Path,
    *,
    source_commit: str | None,
) -> tuple[dict[str, Any], list[dict[str, Any]]]:
    metadata = load_json(metadata_path, "Cargo metadata")
    packages = {package["id"]: package for package in metadata.get("packages", [])}
    locked_packages = cargo_lock_packages(cargo_lock)
    resolve = metadata.get("resolve")
    if not isinstance(resolve, dict) or not isinstance(resolve.get("nodes"), list):
        raise ValueError("Cargo metadata has no resolved dependency graph")
    root_manifest = root_manifest.resolve(strict=True)
    entries = []
    sidecar_entries: list[dict[str, Any]] = []
    sidecar_bytes = 0
    seen: set[tuple[str, str]] = set()
    for package_id in sorted(node["id"] for node in resolve["nodes"]):
        package = packages.get(package_id)
        if not isinstance(package, dict):
            raise ValueError(f"Cargo metadata omits resolved package {package_id}")
        manifest = Path(package["manifest_path"]).resolve(strict=True)
        if manifest == root_manifest:
            continue
        identity = (package["name"], package["version"])
        if identity in seen:
            raise ValueError(
                f"Cargo metadata repeats package identity {identity[0]}@{identity[1]}"
            )
        seen.add(identity)
        package_root = manifest.parent
        source = package.get("source") or "vendored-path"
        locked = locked_packages.get((identity[0], identity[1], source))
        if locked is None:
            raise ValueError(
                f"Cargo.lock omits resolved package {identity[0]}@{identity[1]}"
            )
        evidence_paths = {
            path.resolve(): path
            for path in package_root.iterdir()
            if path.is_file()
            and not path.is_symlink()
            and LICENSE_FILE_RE.fullmatch(path.name)
        }
        explicit_license_file = package.get("license_file")
        if isinstance(explicit_license_file, str) and explicit_license_file:
            path = Path(explicit_license_file).resolve(strict=True)
            try:
                path.relative_to(package_root.resolve())
            except ValueError as exc:
                raise ValueError(
                    f"Cargo license evidence escapes package root: {identity[0]}"
                ) from exc
            if path.is_file() and not path.is_symlink():
                evidence_paths[path] = path
        evidence = [
            {
                "path": path.relative_to(package_root).as_posix(),
                "sha256": sha256(path),
            }
            for path in sorted(evidence_paths, key=lambda item: item.as_posix())
        ]
        local_evidence = {
            item["path"]: read_license_evidence_bytes(
                package_root / item["path"],
                f"Cargo license evidence for {identity[0]}@{identity[1]}",
            )
            for item in evidence
        }
        manifest_bytes = read_license_evidence_bytes(
            manifest, f"Cargo manifest evidence for {identity[0]}@{identity[1]}"
        )
        declared = normalize_license_expression(package.get("license"))
        expression_valid = valid_spdx_expression(declared)
        registry_evidence = None
        checksum = locked.get("checksum")
        if source.startswith("registry+"):
            if not isinstance(checksum, str) or not re.fullmatch(r"[0-9a-f]{64}", checksum):
                raise ValueError(
                    f"Cargo.lock omits registry checksum for {identity[0]}@{identity[1]}"
                )
            registry_key = package_root.parent.name
            crate_archive = (
                package_root.parents[2]
                / "cache"
                / registry_key
                / f"{identity[0]}-{identity[1]}.crate"
            )
            if (
                not crate_archive.is_file()
                or crate_archive.is_symlink()
                or sha256(crate_archive) != checksum
            ):
                raise ValueError(
                    "registry archive differs from Cargo.lock for "
                    f"{identity[0]}@{identity[1]}"
                )
            vcs_path = package_root / ".cargo_vcs_info.json"
            relative_evidence = [manifest.name, *(item["path"] for item in evidence)]
            if vcs_path.is_file() and not vcs_path.is_symlink():
                relative_evidence.append(vcs_path.name)
            archive_prefix = f"{identity[0]}-{identity[1]}"
            requested = {
                f"{archive_prefix}/{relative}": relative for relative in relative_evidence
            }
            archive_contents: dict[str, bytes] = {}
            try:
                with tarfile.open(crate_archive, mode="r:*") as archive:
                    for member in archive:
                        name = member.name
                        while name.startswith("./"):
                            name = name[2:]
                        relative = requested.get(name)
                        if relative is None:
                            continue
                        if relative in archive_contents or not member.isfile():
                            raise ValueError(
                                "registry archive evidence is unsafe or duplicated for "
                                f"{identity[0]}@{identity[1]}: {relative}"
                            )
                        stream = archive.extractfile(member)
                        if stream is None:
                            raise ValueError(
                                "registry archive evidence cannot be read for "
                                f"{identity[0]}@{identity[1]}: {relative}"
                            )
                        if member.size <= 0 or member.size > MAX_LICENSE_EVIDENCE_BYTES:
                            raise ValueError(
                                "registry archive evidence has an invalid size for "
                                f"{identity[0]}@{identity[1]}: {relative}"
                            )
                        payload = stream.read(MAX_LICENSE_EVIDENCE_BYTES + 1)
                        if len(payload) != member.size:
                            raise ValueError(
                                "registry archive evidence size changed for "
                                f"{identity[0]}@{identity[1]}: {relative}"
                            )
                        archive_contents[relative] = payload
            except (OSError, tarfile.TarError) as exc:
                raise ValueError(
                    f"invalid registry archive for {identity[0]}@{identity[1]}: {exc}"
                ) from exc
            expected_contents = {
                manifest.name: manifest_bytes,
                **local_evidence,
            }
            if vcs_path.is_file() and not vcs_path.is_symlink():
                expected_contents[vcs_path.name] = read_license_evidence_bytes(
                    vcs_path, f"Cargo VCS evidence for {identity[0]}@{identity[1]}"
                )
            if archive_contents != expected_contents:
                raise ValueError(
                    "registry extraction bytes differ from checksum-bound archive for "
                    f"{identity[0]}@{identity[1]}"
                )
            vcs_evidence = None
            if vcs_path.is_file() and not vcs_path.is_symlink():
                vcs_payload = load_json(
                    vcs_path, f"Cargo VCS evidence for {identity[0]}@{identity[1]}"
                )
                git = vcs_payload.get("git")
                git_sha = git.get("sha1") if isinstance(git, dict) else None
                if not isinstance(git_sha, str) or not re.fullmatch(
                    r"[0-9a-f]{40}", git_sha
                ):
                    raise ValueError(
                        f"Cargo VCS evidence is invalid for {identity[0]}@{identity[1]}"
                    )
                vcs_evidence = {
                    "git_commit": git_sha,
                    "path": vcs_path.name,
                    "path_in_vcs": vcs_payload.get("path_in_vcs") or ".",
                    "sha256": sha256(vcs_path),
                }
            registry_evidence = {
                "archive_sha256": checksum,
                "crate_archive": {
                    "filename": crate_archive.name,
                    "sha256": sha256(crate_archive),
                },
                "manifest": {"path": manifest.name, "sha256": sha256(manifest)},
                "vcs": vcs_evidence,
            }
        selected_evidence = evidence if evidence else [
            {"path": manifest.name, "sha256": hashlib.sha256(manifest_bytes).hexdigest()}
        ]
        evidence_contents = (
            archive_contents if registry_evidence is not None else local_evidence
        )
        sidecar_evidence = []
        for item in selected_evidence:
            payload = evidence_contents.get(item["path"])
            if payload is None:
                raise ValueError(
                    f"Cargo license bytes are missing for {identity[0]}@{identity[1]}: "
                    f"{item['path']}"
                )
            sidecar_bytes += len(payload)
            if sidecar_bytes > MAX_THIRD_PARTY_LICENSE_BYTES:
                raise ValueError("Cargo third-party license evidence exceeds the size limit")
            sidecar_evidence.append(
                {
                    "content_base64": base64.b64encode(payload).decode("ascii"),
                    "kind": (
                        "license-file"
                        if evidence
                        else "cargo-manifest-license-declaration"
                    ),
                    "path": item["path"],
                    "sha256": hashlib.sha256(payload).hexdigest(),
                    "size": len(payload),
                }
            )
        approved = expression_valid and (
            registry_evidence is not None or bool(evidence)
        )
        entries.append(
            {
                "blocking_reason": (
                    None
                    if approved
                    else (
                        "invalid-or-missing-license-expression"
                        if not expression_valid
                        else "checksum-bound-license-declaration-missing"
                    )
                ),
                "checksum": checksum,
                "declared_license": declared,
                "evidence_basis": (
                    (
                        "cargo-lock-checksum-bound-license-files"
                        if evidence
                        else "cargo-lock-checksum-bound-manifest-declaration"
                    )
                    if registry_evidence is not None
                    else "package-local-license-file"
                ),
                "license_evidence": evidence,
                "registry_evidence": registry_evidence,
                "name": package["name"],
                "purl": f"pkg:cargo/{package['name']}@{package['version']}",
                "review_status": "approved" if approved else "blocked",
                "source": source,
                "version": package["version"],
            }
        )
        if source_commit is not None:
            binding = (
                {"kind": "cargo-lock-checksum", "sha256": checksum}
                if registry_evidence is not None
                else {"git_commit": source_commit, "kind": "source-commit"}
            )
            sidecar_entries.append(
                {
                    "binding": binding,
                    "declared_license": declared,
                    "evidence": sidecar_evidence,
                    "name": package["name"],
                    "purl": f"pkg:cargo/{package['name']}@{package['version']}",
                    "source": source,
                    "version": package["version"],
                }
            )
    entries.sort(key=lambda item: (item["name"], item["version"]))
    approved_count = sum(item["review_status"] == "approved" for item in entries)
    review = {
        "approved": approved_count,
        "blocked": len(entries) - approved_count,
        "dependencies": entries,
        "total": len(entries),
    }
    sidecar_entries.sort(key=lambda item: (item["name"], item["version"]))
    return review, sidecar_entries


def cargo_license_review(
    metadata_path: Path, root_manifest: Path, cargo_lock: Path
) -> dict[str, Any]:
    review, _entries = _cargo_license_inventory(
        metadata_path, root_manifest, cargo_lock, source_commit=None
    )
    return review


def build_cargo_third_party_licenses(
    metadata_path: Path,
    root_manifest: Path,
    cargo_lock: Path,
    *,
    package: str,
    source_commit: str,
) -> tuple[dict[str, Any], dict[str, Any]]:
    if not re.fullmatch(r"[0-9a-f]{40}", source_commit):
        raise ValueError("Cargo license sidecar source commit is invalid")
    review, dependencies = _cargo_license_inventory(
        metadata_path, root_manifest, cargo_lock, source_commit=source_commit
    )
    unresolved = [
        {
            "name": item["name"],
            "reason": item["blocking_reason"],
            "version": item["version"],
        }
        for item in review["dependencies"]
        if item["review_status"] != "approved"
    ]
    sidecar = {
        "cargo_lock": {"filename": cargo_lock.name, "sha256": sha256(cargo_lock)},
        "dependencies": dependencies,
        "package": package,
        "schema_version": 1,
        "source_commit": source_commit,
        "total": len(dependencies),
        "unresolved": unresolved,
    }
    return review, sidecar


def build_empty_cargo_third_party_licenses(
    cargo_lock: Path, *, package: str, source_commit: str
) -> tuple[dict[str, Any], dict[str, Any]]:
    if not re.fullmatch(r"[0-9a-f]{40}", source_commit):
        raise ValueError("Cargo license sidecar source commit is invalid")
    review = {"approved": 0, "blocked": 0, "dependencies": [], "total": 0}
    sidecar = {
        "cargo_lock": {"filename": cargo_lock.name, "sha256": sha256(cargo_lock)},
        "dependencies": [],
        "package": package,
        "schema_version": 1,
        "source_commit": source_commit,
        "total": 0,
        "unresolved": [],
    }
    return review, sidecar


def verify_cargo_third_party_licenses(
    path: Path, expected: dict[str, Any]
) -> dict[str, Any]:
    actual = load_canonical_json(path, "Cargo third-party license sidecar")
    if actual != expected:
        raise ValueError(
            "Cargo third-party license sidecar differs from checksum-bound source evidence"
        )
    if actual.get("unresolved") != []:
        raise ValueError("Cargo third-party license sidecar has unresolved dependencies")
    if actual.get("total") != len(actual.get("dependencies", [])):
        raise ValueError("Cargo third-party license sidecar is incomplete")
    return actual


def model_license_review(path: Path | None) -> dict[str, Any] | None:
    if path is None:
        return None
    payload = load_json(path, "model materials")
    schema_version = payload.get("schema_version")
    if schema_version not in {1, 2}:
        raise ValueError("model materials schema is unsupported")
    entries = []
    evidence_ids: set[str] = set()
    evidence_paths: set[str] = set()
    for material in payload.get("materials", []):
        review = material.get("license")
        if not isinstance(review, dict):
            raise ValueError(f"model material has no license review: {material.get('id')}")
        declared = normalize_license_expression(review.get("declared_license"))
        concluded = normalize_license_expression(review.get("concluded_license"))
        if schema_version == 1:
            if (
                set(review)
                != {
                    "blocking_reason",
                    "concluded_license",
                    "declared_license",
                    "evidence",
                    "review_status",
                }
                or review.get("review_status") != "blocked"
                or review.get("evidence") is not None
            ):
                raise ValueError("schema v1 model license pointers are not release evidence")
            evidence_files: list[dict[str, Any]] = []
            distribution_license_present = False
            evidence_verified = False
            notice_review = None
            notice_status = "review-required"
            approved = False
        else:
            if set(review) != MODEL_LICENSE_FIELDS:
                raise ValueError("schema v2 model license review has an invalid field set")
            evidence_files = review.get("evidence_files")
            if not isinstance(evidence_files, list) or not evidence_files:
                raise ValueError("schema v2 model license review omits evidence files")
            distribution_license_present = review.get("distribution_license_present")
            evidence_verified = review.get("evidence_verified")
            if not isinstance(distribution_license_present, bool) or not isinstance(
                evidence_verified, bool
            ):
                raise ValueError("schema v2 model license booleans are invalid")
            has_distribution_license = False
            for evidence in evidence_files:
                if not isinstance(evidence, dict) or set(evidence) != MODEL_LICENSE_EVIDENCE_FIELDS:
                    raise ValueError("schema v2 model license evidence has invalid fields")
                evidence_id = evidence.get("id")
                installed_path = evidence.get("installed_path")
                if (
                    not isinstance(evidence_id, str)
                    or evidence_id != evidence.get("kind")
                    or evidence_id in evidence_ids
                    or not isinstance(installed_path, str)
                    or not installed_path.startswith(
                        "/usr/share/doc/harboros-model-runtime/model-licenses/"
                    )
                    or installed_path in evidence_paths
                    or not re.fullmatch(r"[0-9a-f]{64}", str(evidence.get("sha256")))
                    or not isinstance(evidence.get("source"), str)
                    or not evidence["source"].startswith("https://")
                    or not isinstance(evidence.get("revision"), str)
                    or not evidence["revision"]
                    or evidence.get("filename") != PurePosixPath(installed_path).name
                    or evidence.get("purpose")
                    not in {"distribution-license", "license-declaration"}
                ):
                    raise ValueError("schema v2 model license evidence identity is invalid")
                evidence_ids.add(evidence_id)
                evidence_paths.add(installed_path)
                has_distribution_license |= evidence["purpose"] == "distribution-license"
            if distribution_license_present is not has_distribution_license:
                raise ValueError("model distribution license flag differs from evidence")
            notice_review = review.get("notice_review")
            if not isinstance(notice_review, dict) or set(notice_review) != (
                MODEL_NOTICE_REVIEW_FIELDS
            ):
                raise ValueError("schema v2 model notice review has invalid fields")
            notice_status = review.get("notice_status")
            review_state = notice_review.get("review_status")
            tree_sha = notice_review.get("tree_sha256")
            if review_state == "verified":
                if not isinstance(tree_sha, str) or not re.fullmatch(r"[0-9a-f]{64}", tree_sha):
                    raise ValueError("verified model notice review has no tree digest")
            elif review_state == "blocked":
                if tree_sha is not None or notice_status != "review-required":
                    raise ValueError("blocked model notice review is inconsistent")
            else:
                raise ValueError("model notice review status is invalid")
            approved = (
                review.get("review_status") == "approved"
                and valid_spdx_expression(declared)
                and valid_spdx_expression(concluded)
                and distribution_license_present
                and evidence_verified
                and review_state == "verified"
                and notice_status == "not-required-with-reviewed-basis"
                and review.get("blocking_reason") is None
            )
            if review.get("review_status") == "approved" and not approved:
                raise ValueError("model license approval is not fully evidenced")
            if review.get("review_status") == "blocked" and not isinstance(
                review.get("blocking_reason"), str
            ):
                raise ValueError("blocked model license review omits its reason")
        entries.append(
            {
                "blocking_reason": None if approved else review.get("blocking_reason"),
                "concluded_license": concluded,
                "declared_license": declared,
                "distribution_license_present": distribution_license_present,
                "evidence_files": evidence_files,
                "evidence_verified": evidence_verified,
                "id": material.get("id"),
                "notice_review": notice_review,
                "notice_status": notice_status,
                "revision": material.get("revision"),
                "review_status": "approved" if approved else "blocked",
                "role": material.get("role"),
                "source": material.get("source"),
            }
        )
    entries.sort(key=lambda item: item["id"])
    approved_count = sum(item["review_status"] == "approved" for item in entries)
    return {
        "approved": approved_count,
        "blocked": len(entries) - approved_count,
        "materials": entries,
        "schema_version": schema_version,
        "total": len(entries),
    }


def frozen_model_license_evidence(
    review: dict[str, Any] | None, package_root: Path | None
) -> list[dict[str, Any]]:
    if review is None or review.get("schema_version") != 2:
        return []
    if package_root is None:
        raise ValueError("schema v2 model materials require --model-license-root")
    try:
        root = package_root.resolve(strict=True)
    except OSError as exc:
        raise ValueError("model license root is missing") from exc
    if package_root.is_symlink() or not root.is_dir():
        raise ValueError("model license root is unsafe")
    frozen = []
    for material in review["materials"]:
        for evidence in material["evidence_files"]:
            installed = PurePosixPath(evidence["installed_path"])
            source = root.joinpath(*installed.relative_to("/").parts)
            payload = read_license_evidence_bytes(
                source, f"model license evidence {evidence['id']}"
            )
            digest = hashlib.sha256(payload).hexdigest()
            if digest != evidence["sha256"]:
                raise ValueError(f"model license evidence differs: {evidence['id']}")
            frozen.append(
                {
                    **evidence,
                    "concluded_license": material["concluded_license"],
                    "declared_license": material["declared_license"],
                    "payload": payload,
                }
            )
    return frozen


def verify_model_license_supply_chain(
    *,
    spdx: dict[str, Any],
    cyclonedx: dict[str, Any],
    provenance: dict[str, Any],
    evidence_files: list[dict[str, Any]],
    package: str,
    version: str,
    architecture: str,
    sidecar_prefix: str,
    installed_spdx_sha256: str,
    installed_cyclonedx_sha256: str,
    model_materials_sha256: str,
) -> None:
    root_packages = [
        item
        for item in spdx.get("packages", [])
        if isinstance(item, dict)
        and item.get("name") == package
        and item.get("versionInfo") == version
    ]
    if len(root_packages) != 1 or not isinstance(root_packages[0].get("SPDXID"), str):
        raise ValueError("SPDX SBOM has no unique model runtime root")
    root_id = root_packages[0]["SPDXID"]
    spdx_files = spdx.get("files")
    relationships = spdx.get("relationships")
    components = cyclonedx.get("components")
    if not all(isinstance(value, list) for value in (spdx_files, relationships, components)):
        raise ValueError("model runtime SBOM collections are invalid")
    subjects = provenance.get("subject")
    resolved = (
        provenance.get("predicate", {})
        .get("buildDefinition", {})
        .get("resolvedDependencies")
    )
    if not isinstance(subjects, list) or not isinstance(resolved, list):
        raise ValueError("build provenance omits subjects or dependencies")
    expected_materials_dependency = {
        "digest": {"sha256": model_materials_sha256},
        "uri": f"{sidecar_prefix}.model-materials.json",
    }
    if sum(item == expected_materials_dependency for item in resolved) != 1:
        raise ValueError("build provenance does not bind the model materials sidecar")

    for evidence in evidence_files:
        evidence_id = evidence["id"]
        spdx_id_value = "SPDXRef-ModelLicenseEvidence-" + re.sub(
            r"[^A-Za-z0-9.-]", "-", evidence_id
        )
        expected_spdx = {
            "SPDXID": spdx_id_value,
            "checksums": [
                {"algorithm": "SHA256", "checksumValue": evidence["sha256"]}
            ],
            "copyrightText": "NOASSERTION",
            "fileName": evidence["installed_path"],
            "licenseConcluded": evidence["concluded_license"],
            "licenseInfoInFiles": [evidence["declared_license"]],
        }
        if sum(item == expected_spdx for item in spdx_files) != 1:
            raise ValueError(f"SPDX does not exactly bind model license evidence: {evidence_id}")
        expected_relationship = {
            "relatedSpdxElement": spdx_id_value,
            "relationshipType": "CONTAINS",
            "spdxElementId": root_id,
        }
        if sum(item == expected_relationship for item in relationships) != 1:
            raise ValueError(f"SPDX does not contain model license evidence: {evidence_id}")
        expected_component = {
            "bom-ref": f"model-license-evidence:{evidence_id}@sha256:{evidence['sha256']}",
            "hashes": [{"alg": "SHA-256", "content": evidence["sha256"]}],
            "licenses": [{"expression": evidence["concluded_license"]}],
            "name": evidence_id,
            "properties": [
                {"name": "harboros:installed-path", "value": evidence["installed_path"]},
                {"name": "harboros:purpose", "value": evidence["purpose"]},
                {"name": "harboros:revision", "value": evidence["revision"]},
                {"name": "harboros:source", "value": evidence["source"]},
            ],
            "type": "file",
        }
        if sum(item == expected_component for item in components) != 1:
            raise ValueError(
                f"CycloneDX does not exactly bind model license evidence: {evidence_id}"
            )
        sidecar = f"{sidecar_prefix}.{evidence_id}.{evidence['filename']}"
        expected_subject = {"digest": {"sha256": evidence["sha256"]}, "name": sidecar}
        if sum(item == expected_subject for item in subjects) != 1:
            raise ValueError(
                f"build provenance does not subject-bind model license evidence: {evidence_id}"
            )
        for uri in (evidence["source"], sidecar):
            dependency = {"digest": {"sha256": evidence["sha256"]}, "uri": uri}
            if sum(item == dependency for item in resolved) != 1:
                raise ValueError(
                    f"build provenance does not dependency-bind {uri}: {evidence_id}"
                )

    expected_sbom_subjects = (
        {
            "digest": {"sha256": installed_spdx_sha256},
            "name": f"/usr/share/doc/{package}/sbom.spdx.json",
        },
        {
            "digest": {"sha256": installed_cyclonedx_sha256},
            "name": f"/usr/share/doc/{package}/sbom.cdx.json",
        },
    )
    for expected in expected_sbom_subjects:
        if sum(item == expected for item in subjects) != 1:
            raise ValueError("build provenance does not bind the installed SBOM bytes")
    parameters = provenance.get("predicate", {}).get("buildDefinition", {}).get(
        "externalParameters"
    )
    if not isinstance(parameters, dict) or (
        parameters.get("version"), parameters.get("arch")
    ) != (version, architecture):
        raise ValueError("build provenance package identity changed")


def verify_package_provenance(
    *,
    provenance: dict[str, Any],
    artifact_name: str,
    artifact_sha256: str,
    build_provenance_name: str,
    build_provenance_sha256: str,
    package: str,
    version: str,
    architecture: str,
    source_commit: str,
) -> None:
    expected_subject = {
        "digest": {"sha256": artifact_sha256},
        "name": artifact_name,
    }
    if provenance.get("subject") != [expected_subject]:
        raise ValueError("package provenance does not bind the final deb")
    definition = provenance.get("predicate", {}).get("buildDefinition", {})
    parameters = definition.get("externalParameters")
    if not isinstance(parameters, dict) or (
        parameters.get("package"),
        parameters.get("version"),
        parameters.get("arch"),
    ) != (package, version, architecture):
        raise ValueError("package provenance identity changed")
    dependencies = definition.get("resolvedDependencies")
    expected_build = {
        "digest": {"sha256": build_provenance_sha256},
        "uri": build_provenance_name,
    }
    expected_source = {
        "digest": {"gitCommit": source_commit},
        "uri": f"git+{SOURCE_REPOSITORY}@{source_commit}",
    }
    if not isinstance(dependencies, list) or sum(
        item == expected_build for item in dependencies
    ) != 1 or sum(item == expected_source for item in dependencies) != 1:
        raise ValueError("package provenance dependencies changed")


def runtime_license_review(
    path: Path | None,
    evidence_path: Path | None,
    architecture: str,
    control_path: Path | None = None,
    source_commit: str | None = None,
    dependency_contract: dict[str, object] | None = None,
) -> dict[str, Any] | None:
    if (
        path is None
        and evidence_path is None
        and control_path is None
        and dependency_contract is None
    ):
        return None
    if path is None or evidence_path is None or (
        control_path is None and dependency_contract is None
    ):
        raise ValueError(
            "runtime manifest, license evidence, and Debian control must be provided together"
        )
    if dependency_contract is None:
        dependency_contract = load_dependency_contract(
            path, control_path, source_commit=source_commit
        )
    evidence_payload = load_canonical_json(
        evidence_path, "model runtime third-party evidence"
    )
    if (
        evidence_payload.get("schema_version") != 1
        or evidence_payload.get("architecture") != architecture
        or not isinstance(evidence_payload.get("repository"), str)
        or not evidence_payload["repository"].startswith("https://")
        or not isinstance(evidence_payload.get("suite"), str)
        or not evidence_payload["suite"]
    ):
        raise ValueError("model runtime third-party evidence has invalid provenance")
    expected = {}
    for value in dependency_contract["bundled_runtime_dependencies"]:
        if not isinstance(value, str) or "=" not in value:
            raise ValueError(f"invalid model runtime dependency: {value!r}")
        name, version = value.split("=", 1)
        if name in expected:
            raise ValueError(f"model runtime dependency is repeated: {name}")
        expected[name] = version
    entries = []
    seen = set()
    for item in evidence_payload.get("packages", []):
        if not isinstance(item, dict) or set(item) != {
            "artifact",
            "blocking_reason",
            "concluded_license",
            "copyright_file",
            "declared_license",
            "name",
            "review_status",
            "version",
        }:
            raise ValueError("model runtime package license evidence has invalid fields")
        name = item.get("name")
        version = item.get("version")
        if not isinstance(name, str) or not isinstance(version, str):
            raise ValueError("model runtime package license evidence has no identity")
        if name in seen or expected.get(name) != version:
            raise ValueError(f"model runtime package evidence differs from manifest: {name}")
        seen.add(name)
        artifact = item.get("artifact")
        copyright_file = item.get("copyright_file")
        for label, value in (("artifact", artifact), ("copyright", copyright_file)):
            if (
                not isinstance(value, dict)
                or not isinstance(value.get("sha256"), str)
                or not re.fullmatch(r"[0-9a-f]{64}", value["sha256"])
            ):
                raise ValueError(f"model runtime {label} evidence is invalid: {name}")
        if set(artifact) != {"filename", "sha256"} or set(copyright_file) != {
            "installed_path",
            "sha256",
        }:
            raise ValueError(f"model runtime evidence has invalid identity fields: {name}")
        declared = normalize_license_expression(item.get("declared_license"))
        concluded = normalize_license_expression(item.get("concluded_license"))
        approved = (
            item.get("review_status") == "approved"
            and valid_spdx_expression(declared)
            and valid_spdx_expression(concluded)
            and item.get("blocking_reason") is None
        )
        entries.append(
            {
                "artifact": artifact,
                "blocking_reason": None if approved else item.get("blocking_reason"),
                "concluded_license": concluded,
                "copyright_file": copyright_file,
                "declared_license": declared,
                "name": name,
                "purl": (
                    f"pkg:generic/spacemit-com/{name}@{version}?arch={architecture}"
                    if name in {"spacemit-llama.cpp", "spine-runtime"}
                    else f"pkg:deb/ubuntu/{name}@{version}?arch={architecture}"
                ),
                "review_status": "approved" if approved else "blocked",
                "version": version,
            }
        )
    if seen != set(expected):
        raise ValueError("model runtime package evidence is incomplete")
    entries.sort(key=lambda item: item["name"])
    approved_count = sum(item["review_status"] == "approved" for item in entries)
    return {
        "approved": approved_count,
        "blocked": len(entries) - approved_count,
        "dependencies": entries,
        "repository": evidence_payload["repository"],
        "suite": evidence_payload["suite"],
        "total": len(entries),
        "bundled_runtime_dependencies": dependency_contract[
            "bundled_runtime_dependencies"
        ],
        "debian_control_dependencies": dependency_contract[
            "debian_control_dependencies"
        ],
    }


def verify_model_runtime_dependency_provenance(
    provenance: dict[str, Any], dependency_contract: dict[str, object]
) -> None:
    definition = provenance.get("predicate", {}).get("buildDefinition", {})
    parameters = definition.get("externalParameters")
    if not isinstance(parameters, dict):
        raise ValueError("model runtime build provenance has no external parameters")
    if "runtime_dependencies" in parameters or (
        parameters.get("bundled_runtime_dependencies")
        != dependency_contract["bundled_runtime_dependencies"]
        or parameters.get("debian_control_dependencies")
        != dependency_contract["debian_control_dependencies"]
    ):
        raise ValueError("model runtime build provenance dependency contract changed")
    expected_control = {
        "digest": {"sha256": dependency_contract["control_sha256"]},
        "uri": CONTROL_URI,
    }
    resolved = definition.get("resolvedDependencies")
    if not isinstance(resolved, list) or sum(
        item == expected_control for item in resolved
    ) != 1:
        raise ValueError("model runtime build provenance omits generated Debian control")


def read_debian_control_tar(stream: Any) -> bytes:
    control: bytes | None = None
    try:
        with tarfile.open(fileobj=stream, mode="r|*") as archive:
            for member in archive:
                if member.name not in {"control", "./control"}:
                    continue
                if control is not None or not member.isfile():
                    raise ValueError("Debian control archive has an unsafe control member")
                if member.size < 1 or member.size > MAX_CONTROL_BYTES:
                    raise ValueError("Debian control member has an invalid size")
                extracted = archive.extractfile(member)
                if extracted is None:
                    raise ValueError("Debian control member cannot be read")
                control = extracted.read(MAX_CONTROL_BYTES + 1)
                if len(control) != member.size:
                    raise ValueError("Debian control member changed while being read")
    except (OSError, tarfile.TarError) as exc:
        raise ValueError("Debian control archive is invalid") from exc
    if control is None:
        raise ValueError("Debian control archive omits control")
    return control


def read_packaged_debian_control(artifact: Path) -> bytes:
    try:
        process = subprocess.Popen(
            ["dpkg-deb", "--ctrl-tarfile", str(artifact)],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
    except FileNotFoundError as exc:
        raise ValueError("dpkg-deb is required to verify packaged Debian control") from exc
    assert process.stdout is not None
    try:
        control = read_debian_control_tar(process.stdout)
    except Exception:
        process.kill()
        process.wait(timeout=30)
        raise
    finally:
        process.stdout.close()
    stderr = process.stderr.read().decode("utf-8", errors="replace") if process.stderr else ""
    return_code = process.wait(timeout=300)
    if return_code:
        raise ValueError(f"dpkg-deb control inspection failed: {stderr.strip()}")
    return control


def vision_runtime_evidence_review(
    path: Path | None,
    model_root: Path | None,
    *,
    package: str,
    version: str,
    architecture: str,
    source_commit: str,
) -> dict[str, Any] | None:
    if path is None and model_root is None:
        return None
    if path is None or model_root is None:
        raise ValueError(
            "vision runtime evidence and vision model root must be provided together"
        )
    payload = load_canonical_json(path, "vision runtime evidence")
    if set(payload) != {
        "decision",
        "kind",
        "model_release_root",
        "models",
        "package",
        "policy",
        "runtime_packages",
        "schema_version",
        "source_commit",
    }:
        raise ValueError("vision runtime evidence has an invalid field set")
    if (
        payload.get("schema_version") != 1
        or payload.get("kind") != "vision-runtime-evidence"
        or payload.get("policy") != "fail-closed"
        or payload.get("model_release_root") != "/data/vision-models"
        or payload.get("source_commit") != source_commit
        or payload.get("package")
        != {"architecture": architecture, "name": package, "version": version}
    ):
        raise ValueError("vision runtime evidence identity is invalid")
    decision = payload.get("decision")
    if not isinstance(decision, dict) or set(decision) != {
        "blocking_reasons",
        "release_eligible",
        "status",
    }:
        raise ValueError("vision runtime decision has an invalid field set")
    blockers = decision.get("blocking_reasons")
    if (
        decision.get("status") not in {"approved", "blocked"}
        or decision.get("release_eligible")
        is not (decision.get("status") == "approved")
        or not isinstance(blockers, list)
        or not all(isinstance(item, str) and item for item in blockers)
        or (decision.get("status") == "approved" and blockers)
        or (decision.get("status") == "blocked" and not blockers)
    ):
        raise ValueError("vision runtime decision is inconsistent")

    model_fields = {
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
    models = payload.get("models")
    if not isinstance(models, list) or len(models) != 2:
        raise ValueError("vision runtime evidence must bind exactly two model files")
    root = model_root.resolve(strict=True)
    if model_root.is_symlink() or not root.is_dir():
        raise ValueError("vision model root is missing or unsafe")
    seen_ids: set[str] = set()
    installed_prefix = "/usr/share/harboros-cat-vision-runtime/models/"
    runtime_prefix = "/data/vision-models/current/"
    expected_models = {
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
    for item in models:
        if not isinstance(item, dict) or set(item) != model_fields:
            raise ValueError("vision model evidence has an invalid field set")
        model_id = item.get("id")
        installed_path = item.get("installed_path")
        runtime_path = item.get("runtime_path")
        expected_model = expected_models.get(model_id)
        if (
            expected_model is None
            or not isinstance(model_id, str)
            or model_id in seen_ids
            or not isinstance(installed_path, str)
            or not installed_path.startswith(installed_prefix)
            or not isinstance(runtime_path, str)
            or not runtime_path.startswith(runtime_prefix)
            or installed_path.removeprefix(installed_prefix)
            != runtime_path.removeprefix(runtime_prefix)
        ):
            raise ValueError("vision model evidence has an invalid identity")
        seen_ids.add(model_id)
        relative = PurePosixPath(installed_path.removeprefix(installed_prefix))
        if relative.is_absolute() or "." in relative.parts or ".." in relative.parts:
            raise ValueError(f"vision model has an unsafe installed path: {model_id}")
        model_file = root.joinpath(*relative.parts)
        expected_size = item.get("size")
        expected_sha = item.get("sha256")
        if (
            relative.as_posix() != expected_model[0]
            or expected_size != expected_model[1]
            or expected_sha != expected_model[2]
            or item.get("source")
            != {"kind": "git", "url": "https://gitee.com/bianbu/spacemit-demo.git"}
            or item.get("revision")
            != {
                "kind": "git-commit",
                "value": "dc4477d3ea712598bb675f730642a43fe280c569",
            }
            or model_file.is_symlink()
            or not model_file.is_file()
            or not isinstance(expected_size, int)
            or expected_size < 1
            or model_file.stat().st_size != expected_size
            or not isinstance(expected_sha, str)
            or not re.fullmatch(r"[0-9a-f]{64}", expected_sha)
            or sha256(model_file) != expected_sha
        ):
            raise ValueError(f"vision model bytes differ from evidence: {model_id}")
        if decision["status"] == "approved" and any(
            item.get(field) is None
            for field in ("copyright", "redistribution_evidence", "source_archive")
        ):
            raise ValueError(f"approved vision model evidence is incomplete: {model_id}")

    runtime_fields = {
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
    artifact_fields = {
        "filename",
        "sha256",
        "size",
        "source_package",
        "source_version",
    }
    provenance_fields = {
        "architecture",
        "component",
        "index_path",
        "index_sha256",
        "release_sha256",
        "repository",
        "signing_key_fingerprint",
        "suite",
    }
    expected_runtime = {
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
    runtime_packages = payload.get("runtime_packages")
    if not isinstance(runtime_packages, list) or len(runtime_packages) != 3:
        raise ValueError("vision runtime evidence must bind exactly three packages")
    seen_runtime: set[str] = set()
    for item in runtime_packages:
        if not isinstance(item, dict) or set(item) != runtime_fields:
            raise ValueError("vision runtime package has an invalid field set")
        name = item.get("name")
        version_value = item.get("version")
        artifact = item.get("artifact")
        provenance = item.get("apt_provenance")
        expected_package = expected_runtime.get(name)
        if (
            name in seen_runtime
            or expected_package is None
            or expected_package[0] != version_value
            or item.get("architecture") != architecture
            or not isinstance(artifact, dict)
            or set(artifact) != artifact_fields
            or artifact.get("source_package") != name
            or artifact.get("source_version") != version_value
            or artifact.get("filename") != expected_package[1]
            or artifact.get("sha256") != expected_package[2]
            or item.get("copyright") != expected_package[3]
            or not isinstance(provenance, dict)
            or set(provenance) != provenance_fields
            or provenance.get("architecture") != architecture
            or provenance.get("repository")
            != "https://ppa.launchpadcontent.net/spacemit/k3/ubuntu"
            or provenance.get("suite") != "resolute"
            or provenance.get("component") != "main"
        ):
            raise ValueError(f"vision runtime package evidence differs: {name}")
        seen_runtime.add(name)
        if decision["status"] == "approved" and any(
            value is None
            for value in (
                artifact.get("sha256"),
                artifact.get("size"),
                provenance.get("index_path"),
                provenance.get("index_sha256"),
                provenance.get("release_sha256"),
                provenance.get("signing_key_fingerprint"),
                item.get("copyright"),
                item.get("license_evidence"),
            )
        ):
            raise ValueError(f"approved vision runtime evidence is incomplete: {name}")
    return payload


def verify_deb_identity(
    artifact: Path, package: str, version: str, architecture: str
) -> None:
    fields = []
    try:
        for field in ("Package", "Version", "Architecture"):
            result = subprocess.run(
                ["dpkg-deb", "--field", str(artifact), field],
                check=True,
                capture_output=True,
                text=True,
            )
            fields.append(result.stdout.strip())
    except (FileNotFoundError, subprocess.CalledProcessError) as exc:
        raise ValueError(f"unable to inspect final Debian artifact: {exc}") from exc
    if fields != [package, version, architecture]:
        raise ValueError(f"final Debian identity changed: {fields!r}")


def verify_installed_evidence_tar(
    payload: Any, entries: list[dict[str, str]], source_date_epoch: int | None = None
) -> None:
    expected = {entry["installed_path"].lstrip("/"): entry for entry in entries}
    if len(expected) != len(entries):
        raise ValueError("installed evidence repeats a package path")
    found: set[str] = set()
    with tarfile.open(fileobj=payload, mode="r|*") as archive:
        for member in archive:
            name = member.name
            while name.startswith("./"):
                name = name[2:]
            if member.isdir() and member.mode & 0o7000:
                raise ValueError(f"package directory has special mode bits: {name}")
            if name not in expected:
                continue
            if name in found or not member.isfile():
                raise ValueError(f"installed evidence is unsafe or duplicated: {name}")
            if (
                member.mode != 0o644
                or member.uid != 0
                or member.gid != 0
                or member.pax_headers
                or (source_date_epoch is not None and member.mtime != source_date_epoch)
            ):
                raise ValueError(f"installed evidence metadata is not canonical: {name}")
            if member.size < 0 or member.size > MAX_THIRD_PARTY_LICENSE_BYTES:
                raise ValueError(f"installed evidence has an invalid size: {name}")
            stream = archive.extractfile(member)
            if stream is None:
                raise ValueError(f"installed evidence cannot be read: {name}")
            digest = hashlib.sha256()
            remaining = member.size
            while remaining:
                chunk = stream.read(min(1024 * 1024, remaining))
                if not chunk:
                    raise ValueError(f"installed evidence is truncated: {name}")
                digest.update(chunk)
                remaining -= len(chunk)
            if stream.read(1):
                raise ValueError(f"installed evidence has an inconsistent size: {name}")
            if digest.hexdigest() != expected[name]["sha256"]:
                raise ValueError(f"installed evidence differs from sidecar: {name}")
            found.add(name)
    missing = sorted(set(expected) - found)
    if missing:
        raise ValueError("installed evidence is missing: " + ", ".join(missing))


def verify_installed_evidence(
    artifact: Path, entries: list[dict[str, str]], source_date_epoch: int | None = None
) -> None:
    try:
        process = subprocess.Popen(
            ["dpkg-deb", "--fsys-tarfile", str(artifact)],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
    except FileNotFoundError as exc:
        raise ValueError("dpkg-deb is required to verify installed evidence") from exc
    assert process.stdout is not None
    try:
        verify_installed_evidence_tar(process.stdout, entries, source_date_epoch)
    finally:
        process.stdout.close()
    stderr = process.stderr.read().decode("utf-8", errors="replace") if process.stderr else ""
    return_code = process.wait(timeout=300)
    if return_code:
        raise ValueError(f"dpkg-deb payload inspection failed: {stderr.strip()}")


def export_copy(source: Path, destination: Path, *, canonical_json: bool = False) -> Path:
    if not source.is_file() or source.is_symlink():
        raise ValueError(f"material source is missing or unsafe: {source}")
    if canonical_json:
        load_canonical_json(source, source.name)
    if source.resolve() != destination.resolve():
        shutil.copyfile(source, destination)
    return destination


def generate(args: argparse.Namespace) -> dict[str, Path]:
    if not args.artifact.is_file() or args.artifact.is_symlink():
        raise ValueError("final Debian artifact is missing or unsafe")
    if not re.fullmatch(r"[0-9a-f]{40}", args.source_commit):
        raise ValueError("source commit must be a full lowercase Git commit")
    if args.architecture not in {"riscv64", "all"}:
        raise ValueError("unsupported package architecture")
    verify_deb_identity(args.artifact, args.package, args.version, args.architecture)
    args.output_dir.mkdir(parents=True, exist_ok=True)
    prefix = args.artifact.name.removesuffix(".deb")
    artifact_digest = sha256(args.artifact)

    rights = load_canonical_json(args.first_party_rights, "first-party rights decision")
    required_rights = {
        "rights_holder": "Harbor Innovations",
        "declared_license": ROOT_LICENSE,
        "copyright": ROOT_COPYRIGHT,
        "authorization": "Inclusion and distribution in HarborNavi qualification artifacts",
    }
    for field, expected in required_rights.items():
        if rights.get(field) != expected:
            raise ValueError(f"first-party rights decision has invalid {field}")
    if rights.get("third_party") != {
        "authorization_basis": "Original upstream license only",
        "covered_by_approval": False,
        "review_required": True,
    }:
        raise ValueError("first-party rights decision does not preserve third-party review")
    notice_bytes, _notice_text = read_utf8_bytes(
        args.first_party_notice, "FIRST_PARTY_RIGHTS.txt"
    )

    contract = load_canonical_json(args.component_contract, "component contract")
    if set(contract) != {"contracts", "package", "schema_version", "source_commit"}:
        raise ValueError("component contract has an invalid field set")
    if (
        contract.get("schema_version") != 1
        or contract.get("package") != args.package
        or contract.get("source_commit") != args.source_commit
        or not isinstance(contract.get("contracts"), list)
        or not contract["contracts"]
    ):
        raise ValueError("component contract does not bind the package source")

    spdx = load_canonical_json(args.sbom_spdx, "base SPDX SBOM")
    installed_spdx_bytes = canonical_bytes(spdx)
    installed_spdx_sha256 = hashlib.sha256(installed_spdx_bytes).hexdigest()
    verify_spdx_extracted_licenses(
        spdx, root_license=ROOT_LICENSE, notice_bytes=notice_bytes
    )
    root_packages = [
        item
        for item in spdx.get("packages", [])
        if item.get("name") == args.package and item.get("versionInfo") == args.version
    ]
    if len(root_packages) != 1:
        raise ValueError("SPDX SBOM does not identify one root package")
    root_package = root_packages[0]
    root_package.update(
        {
            "checksums": [{"algorithm": "SHA256", "checksumValue": artifact_digest}],
            "copyrightText": ROOT_COPYRIGHT,
            "downloadLocation": SOURCE_REPOSITORY,
            "licenseConcluded": ROOT_LICENSE,
            "licenseDeclared": ROOT_LICENSE,
        }
    )

    cyclonedx = load_canonical_json(args.sbom_cyclonedx, "base CycloneDX SBOM")
    installed_cyclonedx_bytes = canonical_bytes(cyclonedx)
    installed_cyclonedx_sha256 = hashlib.sha256(installed_cyclonedx_bytes).hexdigest()
    component = cyclonedx.get("metadata", {}).get("component")
    if not isinstance(component, dict) or (
        component.get("name"), component.get("version")
    ) != (args.package, args.version):
        raise ValueError("CycloneDX SBOM does not identify the root package")
    component.update(
        {
            "hashes": [{"alg": "SHA-256", "content": artifact_digest}],
            "licenses": [{"expression": ROOT_LICENSE}],
            "properties": [
                {"name": "harboros:copyright", "value": ROOT_COPYRIGHT},
                {"name": "harboros:license-concluded", "value": ROOT_LICENSE},
                {"name": "harboros:license-declared", "value": ROOT_LICENSE},
            ],
        }
    )

    if args.no_cargo_dependencies:
        cargo, expected_third_party_licenses = build_empty_cargo_third_party_licenses(
            args.cargo_lock,
            package=args.package,
            source_commit=args.source_commit,
        )
    else:
        cargo, expected_third_party_licenses = build_cargo_third_party_licenses(
            args.cargo_metadata,
            args.root_manifest,
            args.cargo_lock,
            package=args.package,
            source_commit=args.source_commit,
        )
    verify_cargo_third_party_licenses(
        args.third_party_licenses, expected_third_party_licenses
    )
    models = model_license_review(args.model_materials)
    model_evidence = frozen_model_license_evidence(models, args.model_license_root)
    model_materials_sha256 = (
        hashlib.sha256(
            read_license_evidence_bytes(
                args.model_materials,
                "model materials",
                max_bytes=MAX_THIRD_PARTY_LICENSE_BYTES,
            )
        ).hexdigest()
        if args.model_materials is not None
        else None
    )
    build_provenance = load_canonical_json(
        args.build_provenance, "build provenance"
    )
    package_provenance = load_canonical_json(
        args.package_provenance, "package provenance"
    )
    build_provenance_sha256 = hashlib.sha256(
        canonical_bytes(build_provenance)
    ).hexdigest()
    verify_package_provenance(
        provenance=package_provenance,
        artifact_name=args.artifact.name,
        artifact_sha256=artifact_digest,
        build_provenance_name=args.build_provenance.name,
        build_provenance_sha256=build_provenance_sha256,
        package=args.package,
        version=args.version,
        architecture=args.architecture,
        source_commit=args.source_commit,
    )
    if model_evidence:
        assert model_materials_sha256 is not None
        verify_model_license_supply_chain(
            spdx=spdx,
            cyclonedx=cyclonedx,
            provenance=build_provenance,
            evidence_files=model_evidence,
            package=args.package,
            version=args.version,
            architecture=args.architecture,
            sidecar_prefix=prefix,
            installed_spdx_sha256=installed_spdx_sha256,
            installed_cyclonedx_sha256=installed_cyclonedx_sha256,
            model_materials_sha256=model_materials_sha256,
        )
    dependency_contract = None
    if args.runtime_manifest is not None:
        packaged_control = read_packaged_debian_control(args.artifact)
        dependency_contract = load_dependency_contract_bytes(
            args.runtime_manifest,
            packaged_control,
            source_commit=args.source_commit,
        )
    runtime = runtime_license_review(
        args.runtime_manifest,
        args.runtime_license_evidence,
        args.architecture,
        source_commit=args.source_commit,
        dependency_contract=dependency_contract,
    )
    if runtime is not None:
        assert dependency_contract is not None
        verify_model_runtime_dependency_provenance(
            build_provenance, dependency_contract
        )
    vision = vision_runtime_evidence_review(
        args.vision_runtime_evidence,
        args.vision_model_root,
        package=args.package,
        version=args.version,
        architecture=args.architecture,
        source_commit=args.source_commit,
    )
    blockers = []
    if cargo["blocked"]:
        blockers.append("third_party_cargo_license_evidence_incomplete")
    if models is not None and models["blocked"]:
        blockers.append("third_party_model_license_review_incomplete")
    if runtime is not None and runtime["blocked"]:
        blockers.append("third_party_runtime_license_review_incomplete")
    if vision is not None and not vision["decision"]["release_eligible"]:
        blockers.append("vision_runtime_evidence_incomplete")
    approved = not blockers
    decision = {
        "blocking_reasons": blockers,
        "concluded_license": ROOT_LICENSE,
        "copyright": ROOT_COPYRIGHT,
        "declared_license": ROOT_LICENSE,
        "policy": "fail-closed",
        "release_eligible": approved,
        "status": "approved" if approved else "blocked",
    }
    review = {
        "architecture": args.architecture,
        "blocking_reasons": blockers,
        "package": args.package,
        "policy": "fail-closed",
        "release_eligible": approved,
        "review_status": decision["status"],
        "root_component": {
            "approval_id": rights.get("approval_id"),
            "concluded_license": ROOT_LICENSE,
            "copyright": ROOT_COPYRIGHT,
            "declared_license": ROOT_LICENSE,
            "review_status": "approved",
            "rights_holder": rights.get("rights_holder"),
        },
        "schema_version": 1,
        "third_party": {
            "cargo_dependencies": cargo,
            "model_materials": models,
            "policy": "Original upstream license only",
            "runtime_dependencies": runtime,
            "vision_runtime": vision,
        },
        "version": args.version,
    }

    outputs = {
        "component-contract": export_copy(
            args.component_contract,
            args.output_dir / f"{prefix}.component-contract.json",
            canonical_json=True,
        ),
        "first-party-license": export_copy(
            args.first_party_notice,
            args.output_dir / f"{prefix}.FIRST_PARTY_RIGHTS.txt",
        ),
        "first-party-rights": export_copy(
            args.first_party_rights,
            args.output_dir / f"{prefix}.first-party-rights.json",
            canonical_json=True,
        ),
        "third-party-licenses": export_copy(
            args.third_party_licenses,
            args.output_dir / f"{prefix}.third-party-licenses.json",
            canonical_json=True,
        ),
        "build-provenance": args.output_dir / f"{prefix}.build-provenance.json",
        "provenance": args.output_dir / f"{prefix}.package-provenance.json",
        "sbom-spdx": args.output_dir / f"{prefix}.sbom.spdx.json",
        "sbom-cyclonedx": args.output_dir / f"{prefix}.sbom.cdx.json",
        "license-review": args.output_dir / f"{prefix}.license-review.json",
    }
    write_json(outputs["build-provenance"], build_provenance)
    write_json(outputs["provenance"], package_provenance)
    write_json(outputs["sbom-spdx"], spdx)
    write_json(outputs["sbom-cyclonedx"], cyclonedx)
    write_json(outputs["license-review"], review)
    if args.model_materials is not None:
        outputs["model-materials"] = export_copy(
            args.model_materials,
            args.output_dir / f"{prefix}.model-materials.json",
        )
    if model_evidence:
        outputs["installed-sbom-spdx"] = (
            args.output_dir / f"{prefix}.installed-sbom.spdx.json"
        )
        outputs["installed-sbom-cyclonedx"] = (
            args.output_dir / f"{prefix}.installed-sbom.cdx.json"
        )
        outputs["installed-sbom-spdx"].write_bytes(installed_spdx_bytes)
        outputs["installed-sbom-cyclonedx"].write_bytes(installed_cyclonedx_bytes)
        for evidence in model_evidence:
            output = args.output_dir / (
                f"{prefix}.{evidence['id']}.{evidence['filename']}"
            )
            output.write_bytes(evidence["payload"])
            outputs[evidence["id"]] = output
    if args.runtime_manifest is not None:
        outputs["runtime-manifest"] = export_copy(
            args.runtime_manifest,
            args.output_dir / f"{prefix}.runtime-manifest.json",
            canonical_json=True,
        )
    if args.runtime_license_evidence is not None:
        outputs["runtime-license-evidence"] = export_copy(
            args.runtime_license_evidence,
            args.output_dir / f"{prefix}.runtime-license-evidence.json",
            canonical_json=True,
        )
    if args.vision_runtime_evidence is not None:
        outputs["vision-runtime-evidence"] = export_copy(
            args.vision_runtime_evidence,
            args.output_dir / f"{prefix}.vision-runtime-evidence.json",
            canonical_json=True,
        )
        assert args.vision_model_root is not None
        for model in vision["models"]:
            installed_path = model["installed_path"]
            relative = installed_path.removeprefix(
                "/usr/share/harboros-cat-vision-runtime/models/"
            )
            source = args.vision_model_root.joinpath(*PurePosixPath(relative).parts)
            outputs[f"vision-model-{model['id']}"] = export_copy(
                source,
                args.output_dir
                / f"{prefix}.vision-model.{model['id']}.{source.name}",
            )

    checksum = args.output_dir / f"{args.artifact.name}.sha256"
    expected_checksum = f"{artifact_digest}  {args.artifact.name}\n"
    try:
        checksum_payload = checksum.read_text(encoding="ascii")
    except (OSError, UnicodeError) as exc:
        raise ValueError(f"final deb checksum sidecar is invalid: {exc}") from exc
    if checksum_payload != expected_checksum:
        raise ValueError("final deb checksum sidecar is non-canonical or changed")

    material_paths = {
        "deb": args.artifact,
        "deb-sha256": checksum,
        **outputs,
    }
    identities = {
        kind: {"filename": path.name, "kind": kind, "sha256": sha256(path)}
        for kind, path in material_paths.items()
    }
    installed = [
        {
            **identities["component-contract"],
            "installed_path": args.component_contract_installed_path,
        },
        {
            **identities["first-party-rights"],
            "installed_path": f"/usr/share/doc/{args.package}/first-party-rights.json",
        },
        {
            **identities["first-party-license"],
            "installed_path": f"/usr/share/doc/{args.package}/FIRST_PARTY_RIGHTS.txt",
        },
        {
            **identities["third-party-licenses"],
            "installed_path": f"/usr/share/doc/{args.package}/third-party-licenses.json",
        },
    ]
    if "model-materials" in identities:
        installed.append(
            {
                **identities["model-materials"],
                "installed_path": args.model_materials_installed_path,
            }
        )
    if model_evidence:
        installed.extend(
            (
                {
                    **identities["installed-sbom-spdx"],
                    "installed_path": f"/usr/share/doc/{args.package}/sbom.spdx.json",
                },
                {
                    **identities["installed-sbom-cyclonedx"],
                    "installed_path": f"/usr/share/doc/{args.package}/sbom.cdx.json",
                },
            )
        )
        installed.extend(
            {
                **identities[evidence["id"]],
                "installed_path": evidence["installed_path"],
            }
            for evidence in model_evidence
        )
    if "runtime-manifest" in identities:
        installed.append(
            {
                **identities["runtime-manifest"],
                "installed_path": args.runtime_manifest_installed_path,
            }
        )
    if "runtime-license-evidence" in identities:
        installed.append(
            {
                **identities["runtime-license-evidence"],
                "installed_path": args.runtime_license_evidence_installed_path,
            }
        )
    if "vision-runtime-evidence" in identities:
        installed.append(
            {
                **identities["vision-runtime-evidence"],
                "installed_path": (
                    "/usr/share/doc/harboros-cat-vision-runtime/"
                    "vision-runtime-evidence.json"
                ),
            }
        )
        for model in vision["models"]:
            installed.append(
                {
                    **identities[f"vision-model-{model['id']}"],
                    "installed_path": model["installed_path"],
                }
            )
    verify_installed_evidence(args.artifact, installed, args.source_date_epoch)

    binding_kinds = [
        "component-contract",
        "license-review",
        "provenance",
        "sbom-cyclonedx",
        "sbom-spdx",
        "third-party-licenses",
    ]
    if "model-materials" in identities:
        binding_kinds.append("model-materials")
    if model_evidence:
        binding_kinds.extend(("installed-sbom-cyclonedx", "installed-sbom-spdx"))
        binding_kinds.extend(evidence["id"] for evidence in model_evidence)
    if "vision-runtime-evidence" in identities:
        binding_kinds.append("vision-runtime-evidence")

    descriptor = {
        "architecture": args.architecture,
        "artifact": {
            "filename": args.artifact.name,
            "kind": "deb",
            "sha256": artifact_digest,
            "size": args.artifact.stat().st_size,
        },
        "bindings": [identities[kind] for kind in binding_kinds],
        "decision": decision,
        "installed_evidence": installed,
        "materials": [identities[kind] for kind in sorted(identities)],
        "package": args.package,
        "schema_version": 1,
        "source": {"commit": args.source_commit, "repo": SOURCE_REPOSITORY},
        "version": args.version,
    }
    descriptor_path = args.output_dir / f"{args.artifact.name}.release-materials.json"
    write_json(descriptor_path, descriptor)
    manifest_path = args.output_dir / f"{args.artifact.name}.materials.sha256"
    checksummed = [descriptor_path, *material_paths.values()]
    names = [path.name for path in checksummed]
    if len(names) != len(set(names)):
        raise ValueError("release material filenames collide")
    manifest_path.write_text(
        "".join(
            f"{sha256(path)}  {path.name}\n"
            for path in sorted(checksummed, key=lambda item: item.name)
        ),
        encoding="ascii",
    )
    outputs["descriptor"] = descriptor_path
    outputs["materials"] = manifest_path
    return outputs


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--artifact", type=Path, required=True)
    parser.add_argument("--package", required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--architecture", required=True)
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--root-manifest", type=Path, required=True)
    parser.add_argument("--cargo-lock", type=Path, required=True)
    parser.add_argument("--cargo-metadata", type=Path, required=True)
    parser.add_argument("--no-cargo-dependencies", action="store_true")
    parser.add_argument("--component-contract", type=Path, required=True)
    parser.add_argument("--component-contract-installed-path", required=True)
    parser.add_argument("--first-party-rights", type=Path, required=True)
    parser.add_argument("--first-party-notice", type=Path, required=True)
    parser.add_argument("--third-party-licenses", type=Path, required=True)
    parser.add_argument("--sbom-spdx", type=Path, required=True)
    parser.add_argument("--sbom-cyclonedx", type=Path, required=True)
    parser.add_argument("--build-provenance", type=Path, required=True)
    parser.add_argument("--package-provenance", type=Path, required=True)
    parser.add_argument("--model-materials", type=Path)
    parser.add_argument("--model-license-root", type=Path)
    parser.add_argument(
        "--model-materials-installed-path",
        default="/usr/share/harboros-model-runtime/model-materials.json",
    )
    parser.add_argument("--runtime-manifest", type=Path)
    parser.add_argument(
        "--runtime-manifest-installed-path",
        default="/usr/share/doc/harboros-model-runtime/runtime-manifest.json",
    )
    parser.add_argument("--runtime-license-evidence", type=Path)
    parser.add_argument(
        "--runtime-license-evidence-installed-path",
        default=(
            "/usr/share/doc/harboros-model-runtime/runtime-license-evidence.json"
        ),
    )
    parser.add_argument("--vision-runtime-evidence", type=Path)
    parser.add_argument("--vision-model-root", type=Path)
    parser.add_argument("--source-date-epoch", type=int)
    parser.add_argument("--output-dir", type=Path, required=True)
    return parser.parse_args()


def main() -> int:
    try:
        generate(parse_args())
    except ValueError as exc:
        raise SystemExit(f"package material generation failed: {exc}") from exc
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
