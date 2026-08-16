#!/usr/bin/env python3
"""Generate checksum-bound, byte-carrying Cargo license evidence."""

from __future__ import annotations

import argparse
from pathlib import Path

from generate_package_materials import (
    build_cargo_third_party_licenses,
    write_json,
)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--package", required=True)
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--root-manifest", type=Path, required=True)
    parser.add_argument("--cargo-lock", type=Path, required=True)
    parser.add_argument("--cargo-metadata", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    _review, sidecar = build_cargo_third_party_licenses(
        args.cargo_metadata,
        args.root_manifest,
        args.cargo_lock,
        package=args.package,
        source_commit=args.source_commit,
    )
    if sidecar["unresolved"]:
        names = ", ".join(
            f"{item['name']}@{item['version']}" for item in sidecar["unresolved"]
        )
        raise SystemExit(f"Cargo license sidecar generation failed: unresolved {names}")
    args.output.parent.mkdir(parents=True, exist_ok=True)
    write_json(args.output, sidecar)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
