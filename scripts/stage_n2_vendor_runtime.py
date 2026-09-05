#!/usr/bin/env python3
"""Stage the pinned native chat runtime into a package-private directory."""
import argparse
import hashlib
import json
import shutil
import tarfile
from pathlib import Path, PurePosixPath


def stage(lock_path, source, package):
    lock = json.loads(lock_path.read_text(encoding="utf-8"))
    vendor = package / "usr/lib/harboros-model-runtime/vendor"
    evidence = package / "usr/share/doc/harboros-model-runtime/vendor-licenses"
    for item in lock["archives"]:
        archive = source / item["filename"]
        with archive.open("rb") as stream:
            digest = hashlib.file_digest(stream, "sha256").hexdigest()
        if digest != item["sha256"]:
            raise ValueError(f"vendor archive digest mismatch: {archive.name}")
        with tarfile.open(archive, "r:gz") as bundle:
            members = {entry.name.removeprefix("./"): entry for entry in bundle}
            root = item["root"]
            selected = {}
            for name, member in members.items():
                path = PurePosixPath(name)
                if path.parent == PurePosixPath(root, "lib") and any(
                    path.name == prefix or path.name.startswith(prefix + ".")
                    for prefix in item["library_prefixes"]
                ):
                    selected[member] = vendor / "lib" / path.name
            for binary in item["binaries"]:
                selected[members[f"{root}/bin/{binary}"]] = vendor / "bin" / binary
            selected[members[f"{root}/LICENSE"]] = evidence / item["name"] / "LICENSE"
            for member, destination in selected.items():
                destination.parent.mkdir(parents=True, exist_ok=True)
                if member.issym():
                    target = PurePosixPath(member.linkname)
                    if len(target.parts) != 1 or target.name not in {p.name for p in selected.values()}:
                        raise ValueError(f"unsafe vendor library link: {member.name}")
                    destination.symlink_to(target.as_posix())
                elif member.isfile():
                    with bundle.extractfile(member) as src, destination.open("xb") as dst:
                        shutil.copyfileobj(src, dst)
                    destination.chmod(0o755 if destination.parent.name == "bin" else 0o644)
                else:
                    raise ValueError(f"unsupported vendor member: {member.name}")
            license_file = evidence / item["name"] / "LICENSE"
            if hashlib.sha256(license_file.read_bytes()).hexdigest() != item["license_sha256"]:
                raise ValueError("vendor license digest mismatch")
    shutil.copyfile(lock_path, package / "usr/share/doc/harboros-model-runtime/vendor-runtime.json")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--lock", type=Path, required=True)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--package", type=Path, required=True)
    args = parser.parse_args()
    stage(args.lock, args.source, args.package)
