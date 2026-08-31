#!/usr/bin/env python3
"""Fetch the HarborBeacon bootstrap LLM into a release package directory."""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import re
import sys
import time
from urllib.parse import quote
from urllib.request import Request, urlopen


DEFAULT_MODEL_ID = "Qwen/Qwen2.5-0.5B-Instruct"
DEFAULT_REVISION = "7ae557604adf67be50417f59c2c2f167def9a775"
DEFAULT_FILES = [
    "config.json",
    "generation_config.json",
    "LICENSE",
    "merges.txt",
    "model.safetensors",
    "README.md",
    "tokenizer_config.json",
    "tokenizer.json",
    "vocab.json",
]


def validate_immutable_revision(revision: str) -> str:
    revision = revision.strip().lower()
    if not re.fullmatch(r"[0-9a-f]{40}", revision):
        raise ValueError("model revision must be an immutable 40-hex commit")
    return revision


def request_json(url: str, token: str | None) -> dict:
    request = Request(url, headers=request_headers(token))
    with urlopen(request, timeout=60) as response:
        return json.loads(response.read().decode("utf-8"))


def request_headers(token: str | None) -> dict[str, str]:
    headers = {"User-Agent": "harborbeacon-release-bootstrap-model/1.0"}
    if token:
        headers["Authorization"] = f"Bearer {token}"
    return headers


def download_file(url: str, target: Path, token: str | None, retries: int) -> dict:
    target.parent.mkdir(parents=True, exist_ok=True)
    part = target.with_suffix(target.suffix + ".part")
    last_error: Exception | None = None
    for attempt in range(1, retries + 1):
        digest = hashlib.sha256()
        size = 0
        try:
            if part.exists():
                part.unlink()
            request = Request(url, headers=request_headers(token))
            with urlopen(request, timeout=60) as response, part.open("wb") as output:
                while True:
                    chunk = response.read(1024 * 1024)
                    if not chunk:
                        break
                    output.write(chunk)
                    digest.update(chunk)
                    size += len(chunk)
                    if size and size % (128 * 1024 * 1024) < len(chunk):
                        print(f"downloaded {target.name}: {size} bytes", flush=True)
            part.replace(target)
            return {
                "path": target.name,
                "size": size,
                "sha256": digest.hexdigest(),
                "source_url": url,
            }
        except Exception as exc:  # pragma: no cover - network failure path
            last_error = exc
            print(
                f"download attempt {attempt}/{retries} failed for {target.name}: {exc}",
                file=sys.stderr,
                flush=True,
            )
            time.sleep(min(5 * attempt, 20))
    raise RuntimeError(f"failed to download {url}: {last_error}")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--model-id", default=DEFAULT_MODEL_ID)
    parser.add_argument("--revision", default=DEFAULT_REVISION)
    parser.add_argument("--output", required=True)
    parser.add_argument(
        "--endpoint",
        default=os.environ.get("HF_ENDPOINT", "https://huggingface.co"),
        help="Hugging Face-compatible endpoint",
    )
    parser.add_argument(
        "--file",
        action="append",
        dest="files",
        help="File to download; may be repeated. Defaults to the Harbor bootstrap set.",
    )
    parser.add_argument("--retries", type=int, default=3)
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()

    try:
        revision = validate_immutable_revision(args.revision)
    except ValueError as error:
        parser.error(str(error))

    endpoint = args.endpoint.rstrip("/")
    token = os.environ.get("HF_TOKEN") or os.environ.get("HUGGINGFACE_HUB_TOKEN")
    model_api_path = quote(args.model_id, safe="/")
    metadata_url = (
        f"{endpoint}/api/models/{model_api_path}/revision/"
        f"{quote(revision, safe='')}"
    )
    metadata = request_json(metadata_url, token)
    resolved_sha = str(metadata.get("sha", "")).strip().lower()
    if resolved_sha != revision:
        raise SystemExit(
            f"model revision mismatch: requested {revision}, resolved {resolved_sha or 'missing'}"
        )
    siblings = {
        item.get("rfilename")
        for item in metadata.get("siblings", [])
        if item.get("rfilename")
    }
    files = args.files or DEFAULT_FILES
    missing = [name for name in files if name not in siblings]
    if missing:
        raise SystemExit(
            f"model {args.model_id} is missing expected files: {', '.join(missing)}"
        )

    output = Path(args.output)
    print(f"model_id={args.model_id}")
    print(f"revision={revision}")
    print(f"resolved_sha={resolved_sha}")
    print(f"output={output}")
    print("files=" + ",".join(files))
    if args.dry_run:
        return 0

    output.mkdir(parents=True, exist_ok=True)
    downloaded = []
    for name in files:
        file_url = (
            f"{endpoint}/{model_api_path}/resolve/"
            f"{quote(revision, safe='')}/{quote(name, safe='/')}"
        )
        downloaded.append(download_file(file_url, output / name, token, args.retries))

    manifest = {
        "model_id": args.model_id,
        "revision": revision,
        "resolved_sha": resolved_sha,
        "source": f"{endpoint}/{model_api_path}",
        "runtime_profile": "harbor-candle",
        "model_store_target": (
            "/mnt/software/harborbeacon-agent-ci/model-store/"
            "runtimes/harbor-candle/bootstrap-llm"
        ),
        "downloaded_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "files": downloaded,
    }
    (output / "_harbor_bootstrap_model_manifest.json").write_text(
        json.dumps(manifest, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    total_size = sum(item["size"] for item in downloaded)
    print(f"downloaded {len(downloaded)} files, total_size={total_size}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
