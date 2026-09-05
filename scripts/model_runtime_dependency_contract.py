#!/usr/bin/env python3
"""Validate the model-runtime manifest against the generated Debian control."""

from __future__ import annotations

import hashlib
import json
import os
import re
import stat
from pathlib import Path


PACKAGE = "harboros-model-runtime"
CONTROL_URI = f"debian-control:{PACKAGE}"
MAX_CONTROL_BYTES = 128 * 1024
MAX_MANIFEST_BYTES = 128 * 1024
MANIFEST_FIELDS = {
    "bundled_runtime_dependencies",
    "debian_control_dependencies",
    "package",
    "schema_version",
    "services",
    "source_commit",
}
EXPECTED_SERVICES = [
    {
        "bind": "127.0.0.1:8792",
        "health": "http://127.0.0.1:8792/healthz",
        "unit": "harboros-model-runtime.service",
    }
]
FIXED_BUNDLED_DEPENDENCIES = ["spacemit-llama.cpp=0.1.8", "spine-runtime=0.6.1"]
FIXED_SERVICES = EXPECTED_SERVICES + [{
    "bind": "127.0.0.1:8793",
    "health": "http://127.0.0.1:8793/health",
    "unit": "harboros-model-runtime.service",
}]
DEPENDENCY_RE = re.compile(
    r"^[a-z0-9][a-z0-9+.-]*(?::[a-z0-9][a-z0-9-]*)?"
    r"(?: \((?:<<|<=|=|>=|>>) [0-9A-Za-z.+:~_-]+\))?$"
)
SOURCE_COMMIT_RE = re.compile(r"^[0-9a-f]{40}$")


def read_regular_bytes(path: Path, *, max_bytes: int, label: str) -> bytes:
    """Read one bounded regular file through a single no-follow descriptor."""
    descriptor = -1
    try:
        before = os.lstat(path)
        if not stat.S_ISREG(before.st_mode):
            raise ValueError(f"{label} is missing or unsafe: {path}")
        descriptor = os.open(
            path,
            os.O_RDONLY | getattr(os, "O_BINARY", 0) | getattr(os, "O_NOFOLLOW", 0),
        )
        opened = os.fstat(descriptor)
        path_after_open = os.lstat(path)
        identity = lambda value: (
            value.st_dev,
            value.st_ino,
            value.st_size,
            value.st_mtime_ns,
            value.st_ctime_ns,
        )
        # Windows path and handle stat calls expose different ctime semantics.
        # Compare path ctime across path snapshots there; POSIX requires the full
        # path-before/opened tuple requested by the qualification contract.
        path_handle_identity = lambda value: (
            value.st_dev,
            value.st_ino,
            value.st_size,
            value.st_mtime_ns,
        )
        path_matches_open = (
            path_handle_identity(before) == path_handle_identity(opened)
            if os.name == "nt"
            else identity(before) == identity(opened)
        )
        if (
            not stat.S_ISREG(opened.st_mode)
            or not stat.S_ISREG(path_after_open.st_mode)
            or identity(path_after_open) != identity(before)
            or not path_matches_open
            or opened.st_size < 1
            or opened.st_size > max_bytes
        ):
            raise ValueError(
                f"{label} is missing, unsafe, or has an invalid size: {path}"
            )
        payload = bytearray()
        while chunk := os.read(descriptor, min(64 * 1024, max_bytes + 1)):
            payload.extend(chunk)
            if len(payload) > max_bytes:
                raise ValueError(f"{label} exceeds its size limit: {path}")
        after = os.fstat(descriptor)
        path_after_read = os.lstat(path)
        if (
            identity(after) != identity(opened)
            or identity(path_after_read) != identity(before)
            or len(payload) != opened.st_size
        ):
            raise ValueError(f"{label} changed while being read: {path}")
    except OSError as exc:
        raise ValueError(f"unable to open or read {label}: {path}") from exc
    finally:
        if descriptor >= 0:
            os.close(descriptor)
    return bytes(payload)


def parse_debian_control(control_bytes: bytes) -> tuple[dict[str, str], list[str]]:
    try:
        text = control_bytes.decode("utf-8")
    except UnicodeError as exc:
        raise ValueError("Debian control must be exact UTF-8 bytes") from exc
    if text.encode("utf-8") != control_bytes or "\x00" in text:
        raise ValueError("Debian control must be exact UTF-8 bytes")
    fields: dict[str, str] = {}
    current: str | None = None
    for line in text.splitlines():
        if not line:
            if fields:
                raise ValueError("Debian control must contain exactly one stanza")
            continue
        if line[0].isspace():
            if current is None:
                raise ValueError("Debian control has an orphan continuation")
            fields[current] += " " + line.strip()
            continue
        match = re.fullmatch(r"([A-Za-z0-9][A-Za-z0-9-]*):[ \t]*(.*)", line)
        if match is None or match.group(1) in fields:
            raise ValueError("Debian control has an invalid or repeated field")
        current = match.group(1)
        fields[current] = match.group(2).strip()
    if fields.get("Package") != PACKAGE:
        raise ValueError("Debian control package identity changed")
    raw_dependencies = fields.get("Depends")
    if not raw_dependencies:
        raise ValueError("Debian control omits Depends")
    dependencies = [value.strip() for value in raw_dependencies.split(",")]
    if (
        any(not value or DEPENDENCY_RE.fullmatch(value) is None for value in dependencies)
        or len(set(dependencies)) != len(dependencies)
    ):
        raise ValueError("Debian control Depends is not an ordered canonical expression list")
    return fields, dependencies


def load_dependency_contract_bytes(
    manifest_path: Path,
    control_bytes: bytes,
    *,
    source_commit: str | None = None,
) -> dict[str, object]:
    manifest_bytes = read_regular_bytes(
        manifest_path, max_bytes=MAX_MANIFEST_BYTES, label="model runtime manifest"
    )
    if not control_bytes or len(control_bytes) > MAX_CONTROL_BYTES:
        raise ValueError("generated Debian control has an invalid size")
    try:
        manifest = json.loads(manifest_bytes.decode("utf-8"))
    except (UnicodeError, json.JSONDecodeError) as exc:
        raise ValueError("model runtime manifest is invalid JSON") from exc
    if not isinstance(manifest, dict) or set(manifest) != MANIFEST_FIELDS:
        raise ValueError("model runtime manifest has an invalid field set")
    canonical = (
        json.dumps(manifest, ensure_ascii=True, indent=2, sort_keys=True) + "\n"
    ).encode("ascii")
    if canonical != manifest_bytes:
        raise ValueError("model runtime manifest is not canonical JSON")
    manifest_source = manifest.get("source_commit")
    if source_commit is None:
        if manifest_source != "SOURCE_COMMIT_PLACEHOLDER" and (
            not isinstance(manifest_source, str)
            or SOURCE_COMMIT_RE.fullmatch(manifest_source) is None
        ):
            raise ValueError("model runtime manifest source commit is invalid")
    elif manifest_source != source_commit or SOURCE_COMMIT_RE.fullmatch(source_commit) is None:
        raise ValueError("model runtime manifest source commit changed")
    fixed = manifest.get("schema_version") == 3
    bundled = FIXED_BUNDLED_DEPENDENCIES if fixed else []
    if (
        manifest.get("schema_version") not in (2, 3)
        or manifest.get("package") != PACKAGE
        or manifest.get("services") != (FIXED_SERVICES if fixed else EXPECTED_SERVICES)
        or manifest.get("bundled_runtime_dependencies") != bundled
    ):
        raise ValueError("model runtime manifest contract changed")
    _fields, control_dependencies = parse_debian_control(control_bytes)
    if manifest.get("debian_control_dependencies") != control_dependencies:
        raise ValueError("model runtime manifest differs from generated Debian Depends")
    if not fixed and any("spacemit" in value or "llama" in value for value in control_dependencies):
        raise ValueError("model runtime Debian Depends includes a forbidden runtime")
    if fixed:
        pinned = {"python3-spacemit-ort (= 2.0.3+3)", "spacemit-onnxruntime (= 2.0.3+3)", "spacemit-tcm (= 3.0.0+3)"}
        if {value for value in control_dependencies if "spacemit" in value} != pinned:
            raise ValueError("fixed runtime must preserve the qualified vision runtime versions")
        if any("llama" in value or "candle" in value for value in control_dependencies):
            raise ValueError("native chat dependencies must remain package-private")
    return {
        "bundled_runtime_dependencies": bundled,
        "control_sha256": hashlib.sha256(control_bytes).hexdigest(),
        "debian_control_dependencies": control_dependencies,
    }


def load_dependency_contract(
    manifest_path: Path,
    control_path: Path,
    *,
    source_commit: str | None = None,
) -> dict[str, object]:
    control_bytes = read_regular_bytes(
        control_path, max_bytes=MAX_CONTROL_BYTES, label="generated Debian control"
    )
    return load_dependency_contract_bytes(
        manifest_path, control_bytes, source_commit=source_commit
    )
