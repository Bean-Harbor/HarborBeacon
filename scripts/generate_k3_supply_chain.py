#!/usr/bin/env python3
"""Generate deterministic SBOM and provenance documents for K3 packages."""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import re
import subprocess
import tomllib
import uuid
from pathlib import Path


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def spdx_id(name: str, version: str) -> str:
    return "SPDXRef-" + re.sub(r"[^A-Za-z0-9.-]", "-", f"Package-{name}-{version}")


def cargo_components(lock_path: Path) -> list[dict[str, str]]:
    payload = tomllib.loads(lock_path.read_text(encoding="utf-8"))
    return [
        {
            "name": package["name"],
            "version": package["version"],
            "purl": f"pkg:cargo/{package['name']}@{package['version']}",
            "checksum": package.get("checksum", ""),
        }
        for package in payload.get("package", [])
    ]


def command_version(*command: str) -> str:
    completed = subprocess.run(command, check=True, capture_output=True, text=True)
    return (completed.stdout or completed.stderr).strip()


def debian_package_versions(*packages: str) -> list[str]:
    completed = subprocess.run(
        (
            "dpkg-query",
            "--show",
            "--showformat=${binary:Package}=${Version}\\n",
            *packages,
        ),
        check=True,
        capture_output=True,
        text=True,
    )
    resolved = sorted(line for line in completed.stdout.splitlines() if line)
    if len(resolved) != len(packages):
        raise RuntimeError("Packaging provenance is missing a required Debian package version")
    return resolved


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--package", required=True)
    parser.add_argument("--binary", type=Path, action="append", required=True)
    parser.add_argument("--cargo-lock", type=Path, required=True)
    parser.add_argument("--materials", type=Path)
    parser.add_argument("--version", required=True)
    parser.add_argument("--target", required=True)
    parser.add_argument("--arch", required=True)
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--source-date-epoch", type=int, required=True)
    parser.add_argument("--container-digest", required=True)
    parser.add_argument("--debian-snapshot", required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args()

    created = dt.datetime.fromtimestamp(args.source_date_epoch, dt.UTC).isoformat().replace(
        "+00:00", "Z"
    )
    binaries = sorted(args.binary, key=lambda path: path.name)
    root_purl = f"pkg:deb/{args.package}@{args.version}?arch={args.arch}"
    root_id = spdx_id(args.package, args.version)
    components = cargo_components(args.cargo_lock)
    packages = [
        {
            "name": args.package,
            "SPDXID": root_id,
            "versionInfo": args.version,
            "downloadLocation": "NOASSERTION",
            "filesAnalyzed": False,
            "licenseConcluded": "NOASSERTION",
            "licenseDeclared": "NOASSERTION",
            "externalRefs": [
                {
                    "referenceCategory": "PACKAGE-MANAGER",
                    "referenceType": "purl",
                    "referenceLocator": root_purl,
                }
            ],
        }
    ]
    relationships = [
        {
            "spdxElementId": "SPDXRef-DOCUMENT",
            "relationshipType": "DESCRIBES",
            "relatedSpdxElement": root_id,
        }
    ]
    for component in components:
        package_id = spdx_id(component["name"], component["version"])
        value = {
            "name": component["name"],
            "SPDXID": package_id,
            "versionInfo": component["version"],
            "downloadLocation": "NOASSERTION",
            "filesAnalyzed": False,
            "licenseConcluded": "NOASSERTION",
            "licenseDeclared": "NOASSERTION",
            "externalRefs": [
                {
                    "referenceCategory": "PACKAGE-MANAGER",
                    "referenceType": "purl",
                    "referenceLocator": component["purl"],
                }
            ],
        }
        if component["checksum"]:
            value["checksums"] = [
                {"algorithm": "SHA256", "checksumValue": component["checksum"]}
            ]
        packages.append(value)
        relationships.append(
            {
                "spdxElementId": root_id,
                "relationshipType": "DEPENDS_ON",
                "relatedSpdxElement": package_id,
            }
        )

    spdx = {
        "spdxVersion": "SPDX-2.3",
        "dataLicense": "CC0-1.0",
        "SPDXID": "SPDXRef-DOCUMENT",
        "name": f"{args.package}-{args.version}-{args.arch}",
        "documentNamespace": (
            f"https://harboros.ai/sbom/{args.package}/{args.version}/{args.arch}"
        ),
        "creationInfo": {"created": created, "creators": ["Organization: Harbor"]},
        "packages": packages,
        "relationships": relationships,
    }
    cyclonedx = {
        "bomFormat": "CycloneDX",
        "specVersion": "1.6",
        "serialNumber": f"urn:uuid:{uuid.uuid5(uuid.NAMESPACE_URL, root_purl)}",
        "version": 1,
        "metadata": {
            "timestamp": created,
            "component": {
                "type": "application",
                "name": args.package,
                "version": args.version,
                "purl": root_purl,
            },
        },
        "components": [
            {
                "type": "library",
                "name": component["name"],
                "version": component["version"],
                "purl": component["purl"],
                **(
                    {"hashes": [{"alg": "SHA-256", "content": component["checksum"]}]}
                    if component["checksum"]
                    else {}
                ),
            }
            for component in components
        ],
    }
    resolved = [
        {
            "uri": (
                "git+https://github.com/Bean-Harbor/HarborBeacon@"
                f"{args.source_commit}"
            ),
            "digest": {"gitCommit": args.source_commit},
        },
        {"uri": "Cargo.lock", "digest": {"sha256": sha256(args.cargo_lock)}},
    ]
    if args.materials is not None:
        resolved.append(
            {
                "uri": args.materials.name,
                "digest": {"sha256": sha256(args.materials)},
            }
        )
    resolved.append(
        {
            "uri": (
                "https://snapshot.debian.org/archive/debian/"
                f"{args.debian_snapshot}/"
            )
        }
    )
    provenance = {
        "_type": "https://in-toto.io/Statement/v1",
        "subject": [
            {"name": binary.name, "digest": {"sha256": sha256(binary)}}
            for binary in binaries
        ],
        "predicateType": "https://slsa.dev/provenance/v1",
        "predicate": {
            "buildDefinition": {
                "buildType": "https://harboros.ai/build-types/rust-deb/v1",
                "externalParameters": {
                    "target": args.target,
                    "arch": args.arch,
                    "version": args.version,
                    "source_date_epoch": args.source_date_epoch,
                    "debian_snapshot": args.debian_snapshot,
                },
                "resolvedDependencies": resolved,
            },
            "runDetails": {
                "builder": {"id": args.container_digest},
                "metadata": {
                    "invocationId": f"{args.package}-{args.source_commit}-{args.arch}",
                    "startedOn": created,
                    "toolchain": {
                        "cargo": command_version("cargo", "--version"),
                        "debian_packages": debian_package_versions(
                            "dpkg-dev",
                            "gcc-riscv64-linux-gnu",
                            "libc6-dev-riscv64-cross",
                            "python3",
                            "xz-utils",
                        ),
                        "dpkg_deb": command_version("dpkg-deb", "--version").splitlines()[0],
                        "python": command_version("python3", "--version"),
                        "riscv64_linux_gnu_gcc": command_version(
                            "riscv64-linux-gnu-gcc", "--version"
                        ).splitlines()[0],
                        "rustc": command_version("rustc", "--version", "--verbose"),
                        "xz": command_version("xz", "--version").splitlines()[0],
                    },
                },
            },
        },
    }

    args.output_dir.mkdir(parents=True, exist_ok=True)
    for name, payload in (
        ("sbom.spdx.json", spdx),
        ("sbom.cdx.json", cyclonedx),
        ("build-provenance.json", provenance),
    ):
        (args.output_dir / name).write_text(
            json.dumps(payload, ensure_ascii=True, indent=2) + "\n",
            encoding="utf-8",
        )


if __name__ == "__main__":
    main()
