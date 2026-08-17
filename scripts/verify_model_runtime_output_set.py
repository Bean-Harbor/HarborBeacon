#!/usr/bin/env python3
"""Verify the exact model-runtime release output set derived from manifest v2."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
if str(SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPT_DIR))

from model_runtime_dependency_contract import load_dependency_contract


def expected_names(manifest: dict[str, object], version: str, architecture: str) -> set[str]:
    if manifest.get("schema_version") != 2:
        raise ValueError("model runtime output verification requires manifest schema v2")
    prefix = f"harboros-model-runtime_{version}_{architecture}"
    artifact = f"{prefix}.deb"
    names = {
        artifact,
        f"{artifact}.sha256",
        f"{artifact}.materials.sha256",
        f"{artifact}.release-materials.json",
        f"{prefix}.FIRST_PARTY_RIGHTS.txt",
        f"{prefix}.build-provenance.json",
        f"{prefix}.component-contract.json",
        f"{prefix}.first-party-rights.json",
        f"{prefix}.installed-sbom.cdx.json",
        f"{prefix}.installed-sbom.spdx.json",
        f"{prefix}.license-review.json",
        f"{prefix}.model-materials.json",
        f"{prefix}.package-provenance.json",
        f"{prefix}.runtime-license-evidence.json",
        f"{prefix}.runtime-manifest.json",
        f"{prefix}.sbom.cdx.json",
        f"{prefix}.sbom.spdx.json",
        f"{prefix}.third-party-licenses.json",
    }
    evidence_count = 0
    for material in manifest.get("materials", []):
        if not isinstance(material, dict) or not isinstance(material.get("license"), dict):
            raise ValueError("model runtime manifest material is malformed")
        evidence_files = material["license"].get("evidence_files")
        if not isinstance(evidence_files, list) or not evidence_files:
            raise ValueError("model runtime manifest omits evidence_files")
        for evidence in evidence_files:
            if not isinstance(evidence, dict):
                raise ValueError("model runtime evidence entry is malformed")
            kind = evidence.get("kind")
            filename = evidence.get("filename")
            if (
                not isinstance(kind, str)
                or kind != evidence.get("id")
                or not isinstance(filename, str)
                or not filename
            ):
                raise ValueError("model runtime evidence output identity is invalid")
            sidecar = f"{prefix}.{kind}.{filename}"
            if sidecar in names:
                raise ValueError(f"model runtime evidence output collides: {sidecar}")
            names.add(sidecar)
            evidence_count += 1
    if len(names) != 18 + evidence_count:
        raise ValueError("model runtime output count is not manifest-derived")
    return names


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--runtime-manifest", type=Path, required=True)
    parser.add_argument("--debian-control", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--architecture", required=True)
    args = parser.parse_args()
    try:
        manifest = json.loads(args.manifest.read_text(encoding="utf-8"))
        load_dependency_contract(args.runtime_manifest, args.debian_control)
        expected = expected_names(manifest, args.version, args.architecture)
    except (OSError, UnicodeError, json.JSONDecodeError, ValueError) as exc:
        raise SystemExit(f"model runtime output verification failed: {exc}") from exc
    prefix = f"harboros-model-runtime_{args.version}_{args.architecture}"
    actual = {
        path.name
        for path in args.output_dir.iterdir()
        if path.is_file() and path.name.startswith(prefix)
    }
    if actual != expected:
        missing = sorted(expected - actual)
        unexpected = sorted(actual - expected)
        if missing:
            print("missing model runtime outputs: " + ", ".join(missing), file=sys.stderr)
        if unexpected:
            print(
                "unexpected model runtime outputs: " + ", ".join(unexpected),
                file=sys.stderr,
            )
        return 2
    print(f"verified {len(actual)} manifest-derived model runtime outputs")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
