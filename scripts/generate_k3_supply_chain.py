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


ROOT_LICENSE = "LicenseRef-Harbor-Innovations-Proprietary"
ROOT_COPYRIGHT = "Copyright (c) Harbor Innovations"
SOURCE_REPOSITORY = "https://github.com/Bean-Harbor/HarborBeacon"
SPDX_ID_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9.-]*\+?$")
LICENSE_FILE_RE = re.compile(
    r"^(?:LICENSE|LICENCE|COPYING|NOTICE|UNLICENSE)(?:[._-].*)?$",
    re.IGNORECASE,
)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def canonical_json_sha256(value: object) -> str:
    payload = json.dumps(
        value, ensure_ascii=True, separators=(",", ":"), sort_keys=True
    ).encode("ascii")
    return hashlib.sha256(payload).hexdigest()


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


def cargo_lock_packages(cargo_lock: Path) -> dict[tuple[str, str, str], dict[str, object]]:
    try:
        payload = tomllib.loads(cargo_lock.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, tomllib.TOMLDecodeError) as exc:
        raise RuntimeError(f"invalid Cargo.lock: {exc}") from exc
    packages: dict[tuple[str, str, str], dict[str, object]] = {}
    for package in payload.get("package", []):
        if not isinstance(package, dict):
            raise RuntimeError("Cargo.lock contains an invalid package entry")
        name = package.get("name")
        version = package.get("version")
        source = package.get("source") or "vendored-path"
        if not all(isinstance(value, str) and value for value in (name, version, source)):
            raise RuntimeError("Cargo.lock contains an incomplete package identity")
        identity = (name, version, source)
        if identity in packages:
            raise RuntimeError(f"Cargo.lock repeats package identity {name}@{version}")
        packages[identity] = package
    return packages


def cargo_components(
    metadata_path: Path, root_manifest: Path, cargo_lock: Path
) -> list[dict[str, object]]:
    payload = json.loads(metadata_path.read_text(encoding="utf-8"))
    packages = {package["id"]: package for package in payload.get("packages", [])}
    locked_packages = cargo_lock_packages(cargo_lock)
    resolve = payload.get("resolve")
    if not isinstance(resolve, dict) or not isinstance(resolve.get("nodes"), list):
        raise RuntimeError("Cargo metadata has no resolved dependency graph")
    reachable = {node["id"] for node in resolve["nodes"]}
    root_manifest = root_manifest.resolve(strict=True)
    components: list[dict[str, object]] = []
    seen: set[tuple[str, str]] = set()
    for package_id in sorted(reachable):
        package = packages.get(package_id)
        if not isinstance(package, dict):
            raise RuntimeError(f"Cargo metadata omits resolved package {package_id}")
        manifest = Path(package["manifest_path"]).resolve(strict=True)
        if manifest == root_manifest:
            continue
        name = package["name"]
        version = package["version"]
        identity = (name, version)
        if identity in seen:
            raise RuntimeError(f"Cargo metadata repeats package identity {name}@{version}")
        seen.add(identity)
        declared_license = normalize_license_expression(package.get("license"))
        source = package.get("source") or "vendored-path"
        locked = locked_packages.get((name, version, source))
        if locked is None:
            raise RuntimeError(f"Cargo.lock omits resolved package {name}@{version}")
        checksum = locked.get("checksum") or ""
        if source.startswith("registry+"):
            if not isinstance(checksum, str) or not re.fullmatch(r"[0-9a-f]{64}", checksum):
                raise RuntimeError(f"Cargo.lock omits registry checksum for {name}@{version}")
            package_root = manifest.parent
            registry_key = package_root.parent.name
            crate_archive = (
                package_root.parents[2]
                / "cache"
                / registry_key
                / f"{name}-{version}.crate"
            )
            if (
                not crate_archive.is_file()
                or crate_archive.is_symlink()
                or sha256(crate_archive) != checksum
            ):
                raise RuntimeError(
                    f"registry archive differs from Cargo.lock for {name}@{version}"
                )
        license_files = sorted(
            (
                {
                    "filename": path.name,
                    "sha256": sha256(path),
                }
                for path in manifest.parent.iterdir()
                if path.is_file() and not path.is_symlink() and LICENSE_FILE_RE.fullmatch(path.name)
            ),
            key=lambda item: item["filename"],
        )
        components.append(
            {
                "name": name,
                "version": version,
                "purl": f"pkg:cargo/{name}@{version}",
                "checksum": checksum,
                "declared_license": declared_license,
                "concluded_license": (
                    declared_license if valid_spdx_expression(declared_license) else "NOASSERTION"
                ),
                "license_files": license_files,
                "source": source,
            }
        )
    return sorted(components, key=lambda item: (item["name"], item["version"]))


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
    parser.add_argument("--cargo-metadata", type=Path, required=True)
    parser.add_argument("--first-party-notice", type=Path, required=True)
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

    try:
        notice_bytes = args.first_party_notice.read_bytes()
        notice_text = notice_bytes.decode("utf-8")
    except (OSError, UnicodeError) as exc:
        raise RuntimeError("FIRST_PARTY_RIGHTS.txt must be exact UTF-8 bytes") from exc
    if (
        not args.first_party_notice.is_file()
        or args.first_party_notice.is_symlink()
        or not notice_bytes
        or notice_text.encode("utf-8") != notice_bytes
    ):
        raise RuntimeError("FIRST_PARTY_RIGHTS.txt must be non-empty exact UTF-8 bytes")

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
    model_license_by_path: dict[str, dict[str, object]] = {}
    if args.materials is not None:
        materials_payload = json.loads(args.materials.read_text(encoding="utf-8"))
        for material in materials_payload.get("materials", []):
            license_review = material.get("license", {})
            for file_entry in material.get("files", []):
                model_license_by_path[file_entry["package_path"]] = license_review
    if args.model_root is not None:
        model_root = args.model_root.resolve(strict=True)
        model_files = sorted(
            (path for path in model_root.rglob("*") if path.is_file() and not path.is_symlink()),
            key=lambda path: path.relative_to(model_root).as_posix(),
        )
    root_purl = f"pkg:deb/{args.package}@{args.version}?arch={args.arch}"
    root_id = spdx_id(args.package, args.version)
    components = cargo_components(
        args.cargo_metadata,
        args.cargo_lock.parent / "Cargo.toml",
        args.cargo_lock,
    )
    packages = [
        {
            "name": args.package,
            "SPDXID": root_id,
            "versionInfo": args.version,
            "downloadLocation": SOURCE_REPOSITORY,
            "filesAnalyzed": False,
            "licenseConcluded": ROOT_LICENSE,
            "licenseDeclared": ROOT_LICENSE,
            "copyrightText": ROOT_COPYRIGHT,
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
            "licenseConcluded": component["concluded_license"],
            "licenseDeclared": component["declared_license"],
            "copyrightText": "NOASSERTION",
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
                "copyrightText": "NOASSERTION",
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
            license_review = model_license_by_path.get(relative_name, {})
            declared_license = normalize_license_expression(
                license_review.get("declared_license")
            )
            concluded_license = normalize_license_expression(
                license_review.get("concluded_license")
            )
            if not valid_spdx_expression(concluded_license):
                concluded_license = "NOASSERTION"
            spdx_files.append(
                {
                    "fileName": f"./{package_model_name}",
                    "SPDXID": model_id,
                    "checksums": [
                        {"algorithm": "SHA256", "checksumValue": model_digest}
                    ],
                    "licenseConcluded": concluded_license,
                    "licenseInfoInFiles": [declared_license],
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
                    **(
                        {"licenses": [{"expression": concluded_license}]}
                        if concluded_license != "NOASSERTION"
                        else {}
                    ),
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
        "hasExtractedLicensingInfos": [
            {"extractedText": notice_text, "licenseId": ROOT_LICENSE}
        ],
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
                "licenses": [{"expression": ROOT_LICENSE}],
                "properties": [
                    {"name": "harboros:copyright", "value": ROOT_COPYRIGHT},
                    {"name": "harboros:license-concluded", "value": ROOT_LICENSE},
                    {"name": "harboros:license-declared", "value": ROOT_LICENSE},
                ],
            },
        },
        "components": [
            {
                "type": "library",
                "name": component["name"],
                "version": component["version"],
                "purl": component["purl"],
                **(
                    {"licenses": [{"expression": component["concluded_license"]}]}
                    if component["concluded_license"] != "NOASSERTION"
                    else {}
                ),
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
                f"git+{SOURCE_REPOSITORY}@{args.source_commit}"
            ),
            "digest": {"gitCommit": args.source_commit},
        },
        {"uri": "Cargo.lock", "digest": {"sha256": sha256(args.cargo_lock)}},
        {
            "uri": "cargo-metadata:resolved-packages",
            "digest": {"sha256": canonical_json_sha256(components)},
        },
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
            json.dumps(payload, ensure_ascii=True, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )


if __name__ == "__main__":
    main()
