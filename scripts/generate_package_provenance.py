#!/usr/bin/env python3
"""Bind the final Debian artifact to its deterministic build statement."""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
from pathlib import Path


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--package", required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--arch", required=True)
    parser.add_argument("--artifact", type=Path, required=True)
    parser.add_argument("--build-provenance", type=Path, required=True)
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--source-date-epoch", type=int, required=True)
    parser.add_argument("--container-digest", required=True)
    parser.add_argument("--debian-snapshot", required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()

    created = dt.datetime.fromtimestamp(args.source_date_epoch, dt.UTC).isoformat().replace(
        "+00:00", "Z"
    )
    statement = {
        "_type": "https://in-toto.io/Statement/v1",
        "subject": [
            {
                "name": args.artifact.name,
                "digest": {"sha256": sha256(args.artifact)},
            }
        ],
        "predicateType": "https://slsa.dev/provenance/v1",
        "predicate": {
            "buildDefinition": {
                "buildType": "https://harboros.ai/build-types/deb-package/v1",
                "externalParameters": {
                    "package": args.package,
                    "version": args.version,
                    "arch": args.arch,
                    "source_date_epoch": args.source_date_epoch,
                    "debian_snapshot": args.debian_snapshot,
                },
                "resolvedDependencies": [
                    {
                        "uri": (
                            "git+https://github.com/Bean-Harbor/HarborBeacon@"
                            f"{args.source_commit}"
                        ),
                        "digest": {"gitCommit": args.source_commit},
                    },
                    {
                        "uri": args.build_provenance.name,
                        "digest": {"sha256": sha256(args.build_provenance)},
                    },
                    {
                        "uri": (
                            "https://snapshot.debian.org/archive/debian/"
                            f"{args.debian_snapshot}/"
                        )
                    },
                ],
            },
            "runDetails": {
                "builder": {"id": args.container_digest},
                "metadata": {
                    "invocationId": (
                        f"{args.package}-{args.source_commit}-{args.arch}-package"
                    ),
                    "startedOn": created,
                },
            },
        },
    }
    args.output.write_text(
        json.dumps(statement, ensure_ascii=True, indent=2) + "\n",
        encoding="utf-8",
    )


if __name__ == "__main__":
    main()
