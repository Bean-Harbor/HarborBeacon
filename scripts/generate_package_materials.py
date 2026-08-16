#!/usr/bin/env python3
"""Generate HarborOS-canonical release materials for a final Beacon deb."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import shutil
import subprocess
import tarfile
import tomllib
from pathlib import Path
from typing import Any


SOURCE_REPOSITORY = "https://github.com/Bean-Harbor/HarborBeacon"
ROOT_LICENSE = "LicenseRef-Harbor-Innovations-Proprietary"
ROOT_COPYRIGHT = "Copyright (c) Harbor Innovations"
SPDX_ID_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9.-]*\+?$")
LICENSE_FILE_RE = re.compile(
    r"^(?:LICENSE|LICENCE|COPYING|UNLICENSE)(?:[._-].*)?$",
    re.IGNORECASE,
)


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
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise ValueError(f"invalid {label}: {exc}") from exc
    if not isinstance(value, dict):
        raise ValueError(f"{label} must be a JSON object")
    return value


def load_canonical_json(path: Path, label: str) -> dict[str, Any]:
    value = load_json(path, label)
    if path.read_bytes() != canonical_bytes(value):
        raise ValueError(f"{label} is not canonical JSON: {path}")
    return value


def write_json(path: Path, value: dict[str, Any]) -> None:
    path.write_bytes(canonical_bytes(value))


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


def cargo_license_review(
    metadata_path: Path, root_manifest: Path, cargo_lock: Path
) -> dict[str, Any]:
    metadata = load_json(metadata_path, "Cargo metadata")
    packages = {package["id"]: package for package in metadata.get("packages", [])}
    locked_packages = cargo_lock_packages(cargo_lock)
    resolve = metadata.get("resolve")
    if not isinstance(resolve, dict) or not isinstance(resolve.get("nodes"), list):
        raise ValueError("Cargo metadata has no resolved dependency graph")
    root_manifest = root_manifest.resolve(strict=True)
    entries = []
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
            archive_hashes: dict[str, str] = {}
            try:
                with tarfile.open(crate_archive, mode="r:*") as archive:
                    for member in archive:
                        name = member.name
                        while name.startswith("./"):
                            name = name[2:]
                        relative = requested.get(name)
                        if relative is None:
                            continue
                        if relative in archive_hashes or not member.isfile():
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
                        digest = hashlib.sha256()
                        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                            digest.update(chunk)
                        archive_hashes[relative] = digest.hexdigest()
            except (OSError, tarfile.TarError) as exc:
                raise ValueError(
                    f"invalid registry archive for {identity[0]}@{identity[1]}: {exc}"
                ) from exc
            expected_hashes = {
                manifest.name: sha256(manifest),
                **{item["path"]: item["sha256"] for item in evidence},
            }
            if vcs_path.is_file() and not vcs_path.is_symlink():
                expected_hashes[vcs_path.name] = sha256(vcs_path)
            if archive_hashes != expected_hashes:
                raise ValueError(
                    "registry extraction differs from checksum-bound archive for "
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
                    "cargo-lock-checksum-bound-manifest-declaration"
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
    entries.sort(key=lambda item: (item["name"], item["version"]))
    approved_count = sum(item["review_status"] == "approved" for item in entries)
    return {
        "approved": approved_count,
        "blocked": len(entries) - approved_count,
        "dependencies": entries,
        "total": len(entries),
    }


def model_license_review(path: Path | None) -> dict[str, Any] | None:
    if path is None:
        return None
    payload = load_json(path, "model materials")
    entries = []
    for material in payload.get("materials", []):
        review = material.get("license")
        if not isinstance(review, dict):
            raise ValueError(f"model material has no license review: {material.get('id')}")
        declared = normalize_license_expression(review.get("declared_license"))
        concluded = normalize_license_expression(review.get("concluded_license"))
        evidence = review.get("evidence")
        approved = (
            review.get("review_status") == "approved"
            and valid_spdx_expression(declared)
            and valid_spdx_expression(concluded)
            and isinstance(evidence, dict)
        )
        entries.append(
            {
                "blocking_reason": None if approved else review.get("blocking_reason"),
                "concluded_license": concluded,
                "declared_license": declared,
                "evidence": evidence,
                "id": material.get("id"),
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
        "total": len(entries),
    }


def runtime_license_review(
    path: Path | None, evidence_path: Path | None, architecture: str
) -> dict[str, Any] | None:
    if path is None and evidence_path is None:
        return None
    if path is None or evidence_path is None:
        raise ValueError("runtime manifest and license evidence must be provided together")
    payload = load_canonical_json(path, "model runtime manifest")
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
    for value in payload.get("runtime_dependencies", []):
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
                "purl": f"pkg:deb/ubuntu/{name}@{version}?arch={architecture}",
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
    }


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


def verify_installed_evidence(artifact: Path, entries: list[dict[str, str]]) -> None:
    expected = {entry["installed_path"].lstrip("/"): entry for entry in entries}
    found: set[str] = set()
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
        with tarfile.open(fileobj=process.stdout, mode="r|*") as archive:
            for member in archive:
                name = member.name
                while name.startswith("./"):
                    name = name[2:]
                if name not in expected:
                    continue
                if name in found or not member.isfile():
                    raise ValueError(f"installed evidence is unsafe or duplicated: {name}")
                stream = archive.extractfile(member)
                if stream is None:
                    raise ValueError(f"installed evidence cannot be read: {name}")
                digest = hashlib.sha256()
                for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                    digest.update(chunk)
                if digest.hexdigest() != expected[name]["sha256"]:
                    raise ValueError(f"installed evidence differs from sidecar: {name}")
                found.add(name)
    finally:
        process.stdout.close()
    stderr = process.stderr.read().decode("utf-8", errors="replace") if process.stderr else ""
    return_code = process.wait(timeout=300)
    if return_code:
        raise ValueError(f"dpkg-deb payload inspection failed: {stderr.strip()}")
    missing = sorted(set(expected) - found)
    if missing:
        raise ValueError("installed evidence is missing: " + ", ".join(missing))


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

    cargo = cargo_license_review(
        args.cargo_metadata, args.root_manifest, args.cargo_lock
    )
    models = model_license_review(args.model_materials)
    runtime = runtime_license_review(
        args.runtime_manifest, args.runtime_license_evidence, args.architecture
    )
    blockers = []
    if cargo["blocked"]:
        blockers.append("third_party_cargo_license_evidence_incomplete")
    if models is not None and models["blocked"]:
        blockers.append("third_party_model_license_review_incomplete")
    if runtime is not None and runtime["blocked"]:
        blockers.append("third_party_runtime_license_review_incomplete")
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
        "build-provenance": export_copy(
            args.build_provenance,
            args.output_dir / f"{prefix}.build-provenance.json",
            canonical_json=True,
        ),
        "provenance": export_copy(
            args.package_provenance,
            args.output_dir / f"{prefix}.package-provenance.json",
            canonical_json=True,
        ),
        "sbom-spdx": args.output_dir / f"{prefix}.sbom.spdx.json",
        "sbom-cyclonedx": args.output_dir / f"{prefix}.sbom.cdx.json",
        "license-review": args.output_dir / f"{prefix}.license-review.json",
    }
    write_json(outputs["sbom-spdx"], spdx)
    write_json(outputs["sbom-cyclonedx"], cyclonedx)
    write_json(outputs["license-review"], review)
    if args.model_materials is not None:
        outputs["model-materials"] = export_copy(
            args.model_materials,
            args.output_dir / f"{prefix}.model-materials.json",
        )
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
    ]
    if "model-materials" in identities:
        installed.append(
            {
                **identities["model-materials"],
                "installed_path": "/usr/share/harboros-model-runtime/model-materials.json",
            }
        )
    if "runtime-manifest" in identities:
        installed.append(
            {
                **identities["runtime-manifest"],
                "installed_path": "/usr/share/doc/harboros-model-runtime/runtime-manifest.json",
            }
        )
    if "runtime-license-evidence" in identities:
        installed.append(
            {
                **identities["runtime-license-evidence"],
                "installed_path": (
                    "/usr/share/doc/harboros-model-runtime/"
                    "runtime-license-evidence.json"
                ),
            }
        )
    verify_installed_evidence(args.artifact, installed)

    descriptor = {
        "architecture": args.architecture,
        "artifact": {
            "filename": args.artifact.name,
            "kind": "deb",
            "sha256": artifact_digest,
            "size": args.artifact.stat().st_size,
        },
        "bindings": [
            identities[kind]
            for kind in (
                "component-contract",
                "license-review",
                "provenance",
                "sbom-cyclonedx",
                "sbom-spdx",
            )
        ],
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
    parser.add_argument("--component-contract", type=Path, required=True)
    parser.add_argument("--component-contract-installed-path", required=True)
    parser.add_argument("--first-party-rights", type=Path, required=True)
    parser.add_argument("--first-party-notice", type=Path, required=True)
    parser.add_argument("--sbom-spdx", type=Path, required=True)
    parser.add_argument("--sbom-cyclonedx", type=Path, required=True)
    parser.add_argument("--build-provenance", type=Path, required=True)
    parser.add_argument("--package-provenance", type=Path, required=True)
    parser.add_argument("--model-materials", type=Path)
    parser.add_argument("--runtime-manifest", type=Path)
    parser.add_argument("--runtime-license-evidence", type=Path)
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
