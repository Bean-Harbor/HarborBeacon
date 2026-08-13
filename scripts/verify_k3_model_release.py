#!/usr/bin/env python3
"""Verify an installed K3 model tree against the immutable material lock."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import stat
import string
import sys
from pathlib import Path, PurePosixPath


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def locked_files(manifest: Path) -> dict[str, tuple[int, str]]:
    payload = json.loads(manifest.read_text(encoding="utf-8"))
    expected: dict[str, tuple[int, str]] = {}
    for material in payload.get("materials", []):
        if material.get("state") != "locked":
            raise ValueError(f"material is not locked: {material.get('id', '<unknown>')}")
        for entry in material.get("files", []):
            raw_path = entry.get("package_path")
            path = PurePosixPath(raw_path) if isinstance(raw_path, str) else None
            if (
                path is None
                or not raw_path
                or path.is_absolute()
                or "." in path.parts
                or ".." in path.parts
                or raw_path in expected
            ):
                raise ValueError(f"invalid or duplicate package_path: {raw_path!r}")
            size = entry.get("size")
            digest = entry.get("sha256")
            if (
                not isinstance(size, int)
                or size < 1
                or not isinstance(digest, str)
                or len(digest) != 64
                or any(character not in string.hexdigits for character in digest)
                or digest != digest.lower()
            ):
                raise ValueError(f"invalid lock for {raw_path}")
            expected[raw_path] = (size, digest)
    if not expected:
        raise ValueError("manifest has no locked files")
    return expected


def tree_files(root: Path) -> tuple[dict[str, Path], set[str], list[str]]:
    files: dict[str, Path] = {}
    directories = {"."}
    errors: list[str] = []
    root_stat = os.lstat(root)
    if stat.S_ISLNK(root_stat.st_mode) or not stat.S_ISDIR(root_stat.st_mode):
        return files, directories, [f"release root is not a real directory: {root}"]
    for current, dirs, names in os.walk(root, topdown=True, followlinks=False):
        current_path = Path(current)
        safe_dirs: list[str] = []
        for name in dirs:
            path = current_path / name
            mode = os.lstat(path).st_mode
            relative = path.relative_to(root).as_posix()
            if stat.S_ISLNK(mode) or not stat.S_ISDIR(mode):
                errors.append(f"unsafe directory entry: {relative}")
            else:
                safe_dirs.append(name)
                directories.add(relative)
        dirs[:] = safe_dirs
        for name in names:
            path = current_path / name
            relative = path.relative_to(root).as_posix()
            mode = os.lstat(path).st_mode
            if stat.S_ISLNK(mode) or not stat.S_ISREG(mode):
                errors.append(f"unsafe file entry: {relative}")
            else:
                files[relative] = path
    return files, directories, errors


def verify(manifest: Path, root: Path) -> list[str]:
    try:
        expected = locked_files(manifest)
        actual, actual_directories, errors = tree_files(root)
    except (OSError, ValueError, json.JSONDecodeError) as error:
        return [str(error)]
    expected_paths = set(expected)
    actual_paths = set(actual)
    expected_directories = {"."}
    for path in expected_paths:
        parent = PurePosixPath(path).parent
        while str(parent) != ".":
            expected_directories.add(str(parent))
            parent = parent.parent
    for path in sorted(expected_paths - actual_paths):
        errors.append(f"missing locked file: {path}")
    for path in sorted(actual_paths - expected_paths):
        errors.append(f"unexpected file: {path}")
    for path in sorted(actual_directories - expected_directories):
        errors.append(f"unexpected directory: {path}")
    for path in sorted(expected_paths & actual_paths):
        expected_size, expected_sha = expected[path]
        actual_path = actual[path]
        try:
            actual_size = os.lstat(actual_path).st_size
            if actual_size != expected_size:
                errors.append(
                    f"size mismatch: {path} (expected {expected_size}, got {actual_size})"
                )
            elif sha256(actual_path) != expected_sha:
                errors.append(f"SHA256 mismatch: {path}")
        except OSError as error:
            errors.append(f"cannot verify {path}: {error}")
    return errors


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--root", type=Path, required=True)
    args = parser.parse_args()
    errors = verify(args.manifest, args.root)
    if errors:
        for error in errors:
            print(f"error: {error}", file=sys.stderr)
        return 2
    print(f"model release tree verified: {args.root}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
