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


def file_spdx_id(name: str, digest: str) -> str:
    normalized = re.sub(r"[^A-Za-z0-9.-]", "-", name)
    return f"SPDXRef-File-{normalized}-{digest[:12]}"


def runtime_dependency(value: str, arch: str) -> dict[str, str]:
    if "=" not in value:
        raise ValueError(f"runtime dependency must be name=version: {value}")
    name, version = value.split("=", 1)
    if not re.fullmatch(r"[a-z0-9][a-z0-9+.-]+", name) or not version.strip():
        raise ValueError(f"invalid runtime dependency: {value}")
    return {
        "name": name,
        "version": version,
        "purl": f"pkg:deb/ubuntu/{name}@{version}?arch={arch}",
    }


def stable_input_name(path: Path) -> str:
    resolved = path.resolve(strict=True)
    try:
        return resolved.relative_to(Path.cwd().resolve()).as_posix()
    except ValueError:
        return resolved.name


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
    parser.add_argument("--input-file", type=Path, action="append", default=[])
    parser.add_argument("--model-root", type=Path)
    parser.add_argument("--runtime-dependency", action="append", default=[])
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
    inputs = sorted(args.input_file, key=stable_input_name)
    runtime_dependencies = sorted(
        (runtime_dependency(value, args.arch) for value in args.runtime_dependency),
        key=lambda dependency: dependency["name"],
    )
    model_files = []
    if args.model_root is not None:
        model_root = args.model_root.resolve(strict=True)
        model_files = sorted(
            (path for path in model_root.rglob("*") if path.is_file() and not path.is_symlink()),
            key=lambda path: path.relative_to(model_root).as_posix(),
        )
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
    for dependency in runtime_dependencies:
        package_id = spdx_id(dependency["name"], dependency["version"])
        packages.append(
            {
                "name": dependency["name"],
                "SPDXID": package_id,
                "versionInfo": dependency["version"],
                "downloadLocation": "NOASSERTION",
                "filesAnalyzed": False,
                "licenseConcluded": "NOASSERTION",
                "licenseDeclared": "NOASSERTION",
                "externalRefs": [
                    {
                        "referenceCategory": "PACKAGE-MANAGER",
                        "referenceType": "purl",
                        "referenceLocator": dependency["purl"],
                    }
                ],
            }
        )
        relationships.append(
            {
                "spdxElementId": root_id,
                "relationshipType": "DEPENDS_ON",
                "relatedSpdxElement": package_id,
            }
        )

    spdx_files = []
    model_subjects = []
    model_components = []
    if args.model_root is not None:
        for model_file in model_files:
            relative_name = model_file.relative_to(model_root).as_posix()
            model_digest = sha256(model_file)
            model_id = file_spdx_id(relative_name, model_digest)
            package_model_name = f"usr/share/harboros-model-runtime/models/{relative_name}"
            spdx_files.append(
                {
                    "fileName": f"./{package_model_name}",
                    "SPDXID": model_id,
                    "checksums": [
                        {"algorithm": "SHA256", "checksumValue": model_digest}
                    ],
                    "licenseConcluded": "NOASSERTION",
                    "copyrightText": "NOASSERTION",
                }
            )
            relationships.append(
                {
                    "spdxElementId": root_id,
                    "relationshipType": "CONTAINS",
                    "relatedSpdxElement": model_id,
                }
            )
            model_subjects.append(
                {"name": package_model_name, "digest": {"sha256": model_digest}}
            )
            model_components.append(
                {
                    "type": "file",
                    "name": relative_name,
                    "bom-ref": f"model:{relative_name}",
                    "hashes": [{"alg": "SHA-256", "content": model_digest}],
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
        "files": spdx_files,
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
        ]
        + [
            {
                "type": "library",
                "name": dependency["name"],
                "version": dependency["version"],
                "purl": dependency["purl"],
            }
            for dependency in runtime_dependencies
        ]
        + model_components,
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
    for input_file in inputs:
        resolved.append(
            {
                "uri": stable_input_name(input_file),
                "digest": {"sha256": sha256(input_file)},
            }
        )
    for dependency in runtime_dependencies:
        resolved.append(
            {
                "uri": dependency["purl"],
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
        ]
        + model_subjects,
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
                    "runtime_dependencies": [
                        f"{dependency['name']}={dependency['version']}"
                        for dependency in runtime_dependencies
                    ],
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
