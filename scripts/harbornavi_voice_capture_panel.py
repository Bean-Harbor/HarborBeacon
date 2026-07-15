#!/usr/bin/env python3
"""Small browser panel for HarborNavi voice replay sampling.

The panel records short WAV windows from a configured RTSP audio source and
keeps raw audio local to the host running this script. It is intentionally
standalone so it can run on K3 during early voice-spoof demos without changing
the HarborBeacon or HarborGate product contracts.
"""

from __future__ import annotations

import argparse
import datetime as dt
import html
import json
import math
import os
import re
import secrets
import shutil
import statistics
import subprocess
import sys
import threading
import time
import urllib.parse
import wave
from dataclasses import asdict, dataclass, field
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any


DEFAULT_OUTPUT_ROOT = "/tmp/harbornavi-voice-spoof-demo"
DEFAULT_SECONDS = 8.0
DEFAULT_COUNTDOWN_SECONDS = 3.0
MAX_SECONDS = 30.0


@dataclass
class CaptureFile:
    kind: str
    label: str
    path: str
    captured_at: str
    metrics: dict[str, Any]


@dataclass
class JobState:
    job_id: str
    kind: str
    label: str
    status: str
    stage: str
    started_at: str
    stage_started_at: str
    stage_started_unix: float
    seconds: float
    countdown_seconds: float
    path: str | None = None
    error: str | None = None
    metrics: dict[str, Any] | None = None
    finished_at: str | None = None


@dataclass
class PanelState:
    source_id: str
    output_root: str
    session_dir: str | None = None
    files: list[CaptureFile] = field(default_factory=list)
    active_job: JobState | None = None
    report_json: str | None = None
    report_html: str | None = None
    report: dict[str, Any] | None = None
    last_error: str | None = None


class VoiceCapturePanel:
    def __init__(
        self,
        *,
        rtsp_url: str,
        output_root: Path,
        ffmpeg_bin: str,
        python_bin: str,
        demo_script: Path,
        source_id: str,
        default_seconds: float,
        default_countdown: float,
    ) -> None:
        self.rtsp_url = rtsp_url
        self.output_root = output_root
        self.ffmpeg_bin = ffmpeg_bin
        self.python_bin = python_bin
        self.demo_script = demo_script
        self.source_id = source_id
        self.default_seconds = default_seconds
        self.default_countdown = default_countdown
        self.lock = threading.Lock()
        self.state = PanelState(source_id=source_id, output_root=str(output_root))
        self.output_root.mkdir(parents=True, exist_ok=True)
        self._load_latest_session()

    def snapshot(self) -> dict[str, Any]:
        with self.lock:
            return self._snapshot_unlocked()

    def reset(self) -> dict[str, Any]:
        with self.lock:
            if self._busy_unlocked():
                raise RuntimeError("capture is running")
            self.state.session_dir = None
            self.state.files.clear()
            self.state.report_json = None
            self.state.report_html = None
            self.state.report = None
            self.state.last_error = None
            self._ensure_session_unlocked(force_new=True)
            self._write_manifest_unlocked()
            return self._snapshot_unlocked()

    def start_capture(self, kind: str, seconds: float | None, countdown_seconds: float | None) -> JobState:
        if kind not in {"live", "replay"}:
            raise ValueError("kind must be live or replay")
        seconds = clamp_seconds(seconds if seconds is not None else self.default_seconds)
        countdown_seconds = clamp_countdown(
            countdown_seconds if countdown_seconds is not None else self.default_countdown
        )
        with self.lock:
            if self._busy_unlocked():
                raise RuntimeError("capture is already running")
            session = self._ensure_session_unlocked()
            label = self._next_label_unlocked(kind)
            path = session / f"{label}.wav"
            job = JobState(
                job_id=secrets.token_hex(6),
                kind=kind,
                label=label,
                status="queued",
                stage="queued",
                started_at=iso_now(),
                stage_started_at=iso_now(),
                stage_started_unix=time.time(),
                seconds=seconds,
                countdown_seconds=countdown_seconds,
                path=str(path),
            )
            self.state.active_job = job
            self.state.last_error = None
            self._write_manifest_unlocked()
        thread = threading.Thread(target=self._run_capture_job, args=(job,), daemon=True)
        thread.start()
        return job

    def build_report(self) -> dict[str, Any]:
        with self.lock:
            if self._busy_unlocked():
                raise RuntimeError("capture is running")
            session = self._ensure_session_unlocked()
            live_files = self._live_files_unlocked()
            replay_file = self._replay_file_unlocked()
            if not live_files:
                raise RuntimeError("record at least one live sample first")
            if replay_file is None:
                raise RuntimeError("record a replay sample first")
            json_out = session / "phone-replay.report.json"
            html_out = session / "phone-replay.report.html"

        cmd = build_report_command(
            self.python_bin,
            self.demo_script,
            [Path(item.path) for item in live_files],
            Path(replay_file.path),
            json_out,
            html_out,
        )
        proc = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, check=False)
        ok = json_out.exists()
        report: dict[str, Any] | None = None
        if ok:
            report = json.loads(json_out.read_text(encoding="utf-8"))

        with self.lock:
            self.state.report_json = str(json_out) if json_out.exists() else None
            self.state.report_html = str(html_out) if html_out.exists() else None
            self.state.report = report
            if not ok:
                self.state.last_error = redact_sensitive(proc.stderr or proc.stdout, self.rtsp_url)[-800:]
            self._write_manifest_unlocked()
            return {
                "ok": ok,
                "returncode": proc.returncode,
                "json": self.state.report_json,
                "html": self.state.report_html,
                "report": report,
                "stderr_hint": redact_sensitive(proc.stderr, self.rtsp_url)[-500:],
            }

    def _run_capture_job(self, job: JobState) -> None:
        try:
            self._set_job(job, status="countdown", stage="countdown")
            time.sleep(job.countdown_seconds)
            self._set_job(job, status="recording", stage="recording")
            path = Path(job.path or "")
            path.parent.mkdir(parents=True, exist_ok=True)
            cmd = [
                self.ffmpeg_bin,
                "-y",
                "-nostdin",
                "-hide_banner",
                "-loglevel",
                "error",
                "-rtsp_transport",
                "tcp",
                "-i",
                self.rtsp_url,
                "-map",
                "0:a:0",
                "-t",
                f"{job.seconds:.3f}",
                "-vn",
                "-ac",
                "1",
                "-ar",
                "16000",
                "-acodec",
                "pcm_s16le",
                str(path),
            ]
            proc = subprocess.run(
                cmd,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                timeout=max(20, int(job.seconds + job.countdown_seconds + 15)),
                check=False,
            )
            if proc.returncode != 0 or not path.exists() or path.stat().st_size < 1000:
                raise RuntimeError(redact_sensitive(proc.stderr or "ffmpeg capture failed", self.rtsp_url)[-800:])
            metrics = wav_metrics(path)
            with self.lock:
                captured = CaptureFile(
                    kind=job.kind,
                    label=job.label,
                    path=str(path),
                    captured_at=iso_now(),
                    metrics=metrics,
                )
                self._remove_existing_label_unlocked(job.label)
                self.state.files.append(captured)
                self.state.report_json = None
                self.state.report_html = None
                self.state.report = None
                job.metrics = metrics
                job.status = "done"
                job.stage = "done"
                job.finished_at = iso_now()
                self.state.active_job = None
                self._write_manifest_unlocked()
        except Exception as exc:  # pragma: no cover - exercised through HTTP in live use
            with self.lock:
                job.status = "failed"
                job.stage = "failed"
                job.error = str(exc)
                job.finished_at = iso_now()
                self.state.last_error = str(exc)
                self.state.active_job = None
                self._write_manifest_unlocked()

    def _set_job(self, job: JobState, *, status: str, stage: str) -> None:
        with self.lock:
            job.status = status
            job.stage = stage
            job.stage_started_at = iso_now()
            job.stage_started_unix = time.time()
            self._write_manifest_unlocked()

    def _busy_unlocked(self) -> bool:
        return self.state.active_job is not None and self.state.active_job.status in {
            "queued",
            "countdown",
            "recording",
            "running",
        }

    def _snapshot_unlocked(self) -> dict[str, Any]:
        payload = asdict(self.state)
        if self.state.active_job is not None:
            job_payload = payload.get("active_job") or {}
            elapsed = max(0.0, time.time() - self.state.active_job.stage_started_unix)
            if self.state.active_job.stage == "countdown":
                remaining = max(0.0, self.state.active_job.countdown_seconds - elapsed)
            elif self.state.active_job.stage == "recording":
                remaining = max(0.0, self.state.active_job.seconds - elapsed)
            else:
                remaining = 0.0
            job_payload["stage_elapsed_sec"] = round(elapsed, 3)
            job_payload["stage_remaining_sec"] = round(remaining, 3)
            payload["active_job"] = job_payload
        payload["ready"] = {
            "has_live": bool(self._live_files_unlocked()),
            "has_replay": self._replay_file_unlocked() is not None,
            "can_report": bool(self._live_files_unlocked()) and self._replay_file_unlocked() is not None,
            "busy": self._busy_unlocked(),
        }
        return payload

    def _ensure_session_unlocked(self, *, force_new: bool = False) -> Path:
        if force_new or not self.state.session_dir:
            session = self.output_root / dt.datetime.now().strftime("%Y%m%dT%H%M%S")
            suffix = 1
            while session.exists():
                suffix += 1
                session = self.output_root / f"{dt.datetime.now().strftime('%Y%m%dT%H%M%S')}-{suffix}"
            session.mkdir(parents=True, exist_ok=True)
            self.state.session_dir = str(session)
            self.state.files.clear()
        session = Path(self.state.session_dir)
        session.mkdir(parents=True, exist_ok=True)
        return session

    def _load_latest_session(self) -> None:
        manifests = sorted(self.output_root.glob("*/manifest.json"))
        if not manifests:
            return
        try:
            payload = json.loads(manifests[-1].read_text(encoding="utf-8"))
            self.state.session_dir = payload.get("session_dir")
            self.state.files = [CaptureFile(**item) for item in payload.get("files", [])]
            self.state.report_json = payload.get("report_json")
            self.state.report_html = payload.get("report_html")
            self.state.report = payload.get("report")
            self.state.last_error = payload.get("last_error")
        except Exception:
            self.state.last_error = "latest manifest could not be loaded"

    def _write_manifest_unlocked(self) -> None:
        if not self.state.session_dir:
            return
        manifest = Path(self.state.session_dir) / "manifest.json"
        payload = asdict(self.state)
        payload["active_job"] = asdict(self.state.active_job) if self.state.active_job else None
        manifest.write_text(json.dumps(payload, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")

    def _live_files_unlocked(self) -> list[CaptureFile]:
        return [item for item in self.state.files if item.kind == "live"]

    def _replay_file_unlocked(self) -> CaptureFile | None:
        for item in reversed(self.state.files):
            if item.kind == "replay":
                return item
        return None

    def _next_label_unlocked(self, kind: str) -> str:
        if kind == "replay":
            return "phone-replay"
        live_count = len(self._live_files_unlocked()) + 1
        return f"live-{live_count}"

    def _remove_existing_label_unlocked(self, label: str) -> None:
        self.state.files = [item for item in self.state.files if item.label != label]


def build_report_command(
    python_bin: str,
    demo_script: Path,
    live_files: list[Path],
    replay_file: Path,
    json_out: Path,
    html_out: Path,
) -> list[str]:
    cmd = [python_bin, str(demo_script)]
    for path in live_files:
        cmd.extend(["--live", str(path)])
    cmd.extend(
        [
            "--candidate",
            str(replay_file),
            "--label",
            "phone-replay",
            "--json-out",
            str(json_out),
            "--html-out",
            str(html_out),
        ]
    )
    return cmd


def wav_metrics(path: Path) -> dict[str, Any]:
    with wave.open(str(path), "rb") as handle:
        channels = handle.getnchannels()
        sample_width = handle.getsampwidth()
        sample_rate = handle.getframerate()
        frame_count = handle.getnframes()
        raw = handle.readframes(frame_count)

    if sample_width != 2:
        raise ValueError(f"expected 16-bit PCM WAV, got sample width {sample_width}")
    if channels != 1:
        raise ValueError(f"expected mono WAV, got {channels} channels")

    samples = [
        int.from_bytes(raw[index : index + 2], "little", signed=True)
        for index in range(0, len(raw) - 1, 2)
    ]
    if samples:
        rms = math.sqrt(sum(sample * sample for sample in samples) / len(samples))
        peak = max(abs(sample) for sample in samples)
        crossings = sum(
            1
            for prev, sample in zip(samples, samples[1:])
            if (prev < 0 <= sample) or (prev >= 0 > sample)
        )
        p95 = percentile_abs(samples, 0.95)
        p99 = percentile_abs(samples, 0.99)
        frame_rms = short_time_rms(samples, max(256, min(2048, sample_rate // 8)))
        energy_cv = safe_div(stdev(frame_rms), statistics.fmean(frame_rms) if frame_rms else rms)
    else:
        rms = peak = crossings = p95 = p99 = energy_cv = 0.0

    duration = safe_div(frame_count, sample_rate)
    return {
        "duration_sec": round(duration, 3),
        "sample_rate": sample_rate,
        "channels": channels,
        "bytes": path.stat().st_size,
        "rms": round(rms, 2),
        "peak": int(peak),
        "p95_abs": int(p95),
        "p99_abs": int(p99),
        "rms_dbfs": round(dbfs(rms / 32768.0), 2),
        "peak_dbfs": round(dbfs(peak / 32768.0), 2),
        "zero_crossings": int(crossings),
        "zcr_per_sec": round(safe_div(crossings, duration), 1),
        "energy_cv": round(float(energy_cv), 4),
    }


def percentile_abs(samples: list[int], percentile: float) -> int:
    if not samples:
        return 0
    values = sorted(abs(sample) for sample in samples)
    index = min(len(values) - 1, max(0, int((len(values) - 1) * percentile)))
    return values[index]


def short_time_rms(samples: list[int], frame_size: int) -> list[float]:
    if frame_size <= 0 or len(samples) < frame_size:
        return []
    hop = max(1, frame_size // 2)
    values = []
    for index in range(0, len(samples) - frame_size + 1, hop):
        frame = samples[index : index + frame_size]
        values.append(math.sqrt(sum(sample * sample for sample in frame) / len(frame)))
    return values


def stdev(values: list[float]) -> float:
    if len(values) < 2:
        return 0.0
    return statistics.stdev(values)


def safe_div(numerator: float, denominator: float | int) -> float:
    if not denominator:
        return 0.0
    return numerator / denominator


def dbfs(value: float) -> float:
    return 20.0 * math.log10(max(abs(value), 1e-9))


def clamp_seconds(value: float) -> float:
    return max(1.0, min(MAX_SECONDS, float(value)))


def clamp_countdown(value: float) -> float:
    return max(0.0, min(30.0, float(value)))


def iso_now() -> str:
    return dt.datetime.now(dt.timezone.utc).replace(microsecond=0).isoformat()


def redact_sensitive(text: str, rtsp_url: str) -> str:
    if not text:
        return ""
    redacted = text.replace(rtsp_url, redact_url(rtsp_url))
    parsed = urllib.parse.urlparse(rtsp_url)
    if parsed.password:
        redacted = redacted.replace(parsed.password, "<password>")
    return re.sub(r"rtsp://[^\s]+@", "rtsp://<cred>@", redacted)


def redact_url(url: str) -> str:
    parsed = urllib.parse.urlparse(url)
    if not parsed.username and not parsed.password:
        return url
    host = parsed.hostname or ""
    if parsed.port:
        host = f"{host}:{parsed.port}"
    return urllib.parse.urlunparse((parsed.scheme, f"<cred>@{host}", parsed.path, "", "", ""))


def read_rtsp_url(args: argparse.Namespace) -> str:
    value = args.rtsp_url or os.environ.get("HARBORNAVI_VOICE_RTSP_URL")
    if args.rtsp_url_file:
        value = Path(args.rtsp_url_file).read_text(encoding="utf-8").strip()
    if not value:
        raise SystemExit("provide --rtsp-url, --rtsp-url-file, or HARBORNAVI_VOICE_RTSP_URL")
    return value


def make_handler(panel: VoiceCapturePanel) -> type[BaseHTTPRequestHandler]:
    class Handler(BaseHTTPRequestHandler):
        server_version = "HarborNaviVoiceCapturePanel/0.1"

        def do_GET(self) -> None:  # noqa: N802
            parsed = urllib.parse.urlparse(self.path)
            if parsed.path == "/":
                self._send_html(render_index())
                return
            if parsed.path == "/api/state":
                self._send_json(panel.snapshot())
                return
            if parsed.path == "/report.html":
                state = panel.snapshot()
                report_path = state.get("report_html")
                if report_path and Path(report_path).exists():
                    self._send_html(Path(report_path).read_text(encoding="utf-8"))
                    return
                self._send_json({"ok": False, "error": "report not found"}, status=HTTPStatus.NOT_FOUND)
                return
            self._send_json({"ok": False, "error": "not found"}, status=HTTPStatus.NOT_FOUND)

        def do_POST(self) -> None:  # noqa: N802
            parsed = urllib.parse.urlparse(self.path)
            try:
                payload = self._read_json()
                if parsed.path == "/api/capture":
                    job = panel.start_capture(
                        str(payload.get("kind", "")),
                        payload.get("seconds"),
                        payload.get("countdown_seconds"),
                    )
                    self._send_json({"ok": True, "job": asdict(job)})
                    return
                if parsed.path == "/api/report":
                    self._send_json(panel.build_report())
                    return
                if parsed.path == "/api/reset":
                    self._send_json(panel.reset())
                    return
                self._send_json({"ok": False, "error": "not found"}, status=HTTPStatus.NOT_FOUND)
            except Exception as exc:
                self._send_json({"ok": False, "error": str(exc)}, status=HTTPStatus.BAD_REQUEST)

        def log_message(self, fmt: str, *args: Any) -> None:
            sys.stderr.write("%s - %s\n" % (self.address_string(), fmt % args))

        def _read_json(self) -> dict[str, Any]:
            length = int(self.headers.get("Content-Length", "0") or "0")
            if length <= 0:
                return {}
            body = self.rfile.read(length).decode("utf-8")
            return json.loads(body) if body else {}

        def _send_json(self, payload: dict[str, Any], status: HTTPStatus = HTTPStatus.OK) -> None:
            data = json.dumps(payload, ensure_ascii=False, indent=2).encode("utf-8")
            self.send_response(status)
            self.send_header("Content-Type", "application/json; charset=utf-8")
            self.send_header("Cache-Control", "no-store")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def _send_html(self, text: str, status: HTTPStatus = HTTPStatus.OK) -> None:
            data = text.encode("utf-8")
            self.send_response(status)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Cache-Control", "no-store")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

    return Handler


def render_index() -> str:
    return """<!doctype html>
<html lang="zh-CN">
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>HarborNavi Voice Capture</title>
<style>
:root {
  color-scheme: light;
  --ink: #17181f;
  --muted: #5f6572;
  --line: #dfe3ea;
  --panel: #ffffff;
  --band: #f3f5f8;
  --accent: #245fcb;
  --accent-2: #0f8a6d;
  --warn: #b45309;
}
* { box-sizing: border-box; }
body {
  margin: 0;
  font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", "Noto Sans SC", sans-serif;
  color: var(--ink);
  background: #fafafa;
}
main { max-width: 980px; margin: 0 auto; padding: 28px; }
header { display: flex; justify-content: space-between; align-items: flex-start; gap: 20px; margin-bottom: 22px; }
h1 { font-size: 32px; line-height: 1.1; margin: 0; font-weight: 760; letter-spacing: 0; }
.source { color: var(--muted); margin-top: 8px; font-size: 14px; }
.status {
  min-width: 260px;
  padding: 14px 16px;
  border: 1px solid var(--line);
  background: var(--panel);
}
.status strong { display: block; font-size: 18px; margin-bottom: 3px; }
.cue {
  margin: 0 0 18px;
  padding: 20px 22px;
  border: 1px solid var(--line);
  background: #fff;
}
.cue-title {
  font-size: 42px;
  line-height: 1.05;
  font-weight: 820;
  letter-spacing: 0;
}
.cue-detail { color: var(--muted); margin-top: 8px; font-size: 15px; }
.meter {
  width: 100%;
  height: 12px;
  background: var(--band);
  margin-top: 14px;
  overflow: hidden;
}
.meter-bar {
  width: 0%;
  height: 100%;
  background: var(--accent);
  transition: width .25s linear;
}
.cue.recording .cue-title { color: var(--accent-2); }
.cue.recording .meter-bar { background: var(--accent-2); }
.cue.countdown .cue-title { color: var(--warn); }
.cue.done .cue-title { color: var(--accent); }
.controls {
  display: grid;
  grid-template-columns: repeat(4, minmax(0, 1fr));
  gap: 12px;
  margin-bottom: 20px;
}
button {
  min-height: 64px;
  border: 1px solid var(--line);
  background: var(--panel);
  color: var(--ink);
  font-size: 18px;
  font-weight: 720;
  cursor: pointer;
}
button.primary { background: var(--accent); color: white; border-color: var(--accent); }
button.secondary { background: var(--accent-2); color: white; border-color: var(--accent-2); }
button:disabled { opacity: .48; cursor: wait; }
.settings {
  display: flex;
  align-items: center;
  gap: 14px;
  padding: 12px 14px;
  border: 1px solid var(--line);
  background: var(--panel);
  margin-bottom: 18px;
}
label { color: var(--muted); font-size: 14px; }
input {
  width: 84px;
  padding: 8px 9px;
  border: 1px solid var(--line);
  font-size: 15px;
}
.grid { display: grid; grid-template-columns: 1fr 1fr; gap: 14px; }
section {
  border: 1px solid var(--line);
  background: var(--panel);
  padding: 18px;
}
h2 { margin: 0 0 12px; font-size: 20px; letter-spacing: 0; }
.sample { border-top: 1px solid var(--line); padding: 12px 0; }
.sample:first-of-type { border-top: 0; }
.sample-title { display: flex; justify-content: space-between; gap: 10px; font-weight: 710; }
.metric { color: var(--muted); font-size: 13px; margin-top: 4px; overflow-wrap: anywhere; }
.report { grid-column: 1 / -1; }
.decision { font-size: 28px; font-weight: 780; margin: 4px 0; }
.reasons { color: var(--muted); font-size: 14px; }
a { color: var(--accent); font-weight: 650; }
@media (max-width: 760px) {
  main { padding: 18px; }
  header, .settings { display: block; }
  .status { min-width: 0; margin-top: 14px; }
  .controls, .grid { grid-template-columns: 1fr; }
  button { min-height: 58px; }
}
</style>
<main>
  <header>
    <div>
      <h1>HarborNavi Voice Capture</h1>
      <div class="source" id="source">TP-Link 231 / stream2</div>
    </div>
    <div class="status">
      <strong id="status-title">准备就绪</strong>
      <div id="status-detail" class="source">等待采样</div>
    </div>
  </header>
  <div class="cue" id="cue">
    <div class="cue-title" id="cue-title">准备采样</div>
    <div class="cue-detail" id="cue-detail">点击按钮后先倒计时，再开始录制。</div>
    <div class="meter"><div class="meter-bar" id="meter-bar"></div></div>
  </div>
  <div class="settings">
    <label>倒计时 <input id="countdown" type="number" min="0" max="30" step="1" value="3"> 秒</label>
    <label>采样 <input id="seconds" type="number" min="1" max="30" step="1" value="8"> 秒</label>
  </div>
  <div class="controls">
    <button class="primary" id="live">录真人样本</button>
    <button class="secondary" id="replay">录手机回放</button>
    <button id="report">生成判断报告</button>
    <button id="reset">清空本轮</button>
  </div>
  <div class="grid">
    <section>
      <h2>真人样本</h2>
      <div id="live-list"></div>
    </section>
    <section>
      <h2>回放样本</h2>
      <div id="replay-list"></div>
    </section>
    <section class="report">
      <h2>判断结果</h2>
      <div id="report-box" class="reasons">暂无报告</div>
    </section>
  </div>
</main>
<script>
const els = {
  live: document.querySelector("#live"),
  replay: document.querySelector("#replay"),
  report: document.querySelector("#report"),
  reset: document.querySelector("#reset"),
  countdown: document.querySelector("#countdown"),
  seconds: document.querySelector("#seconds"),
  statusTitle: document.querySelector("#status-title"),
  statusDetail: document.querySelector("#status-detail"),
  source: document.querySelector("#source"),
  cue: document.querySelector("#cue"),
  cueTitle: document.querySelector("#cue-title"),
  cueDetail: document.querySelector("#cue-detail"),
  meterBar: document.querySelector("#meter-bar"),
  liveList: document.querySelector("#live-list"),
  replayList: document.querySelector("#replay-list"),
  reportBox: document.querySelector("#report-box"),
};
async function api(path, body) {
  const res = await fetch(path, {
    method: body ? "POST" : "GET",
    headers: body ? {"Content-Type": "application/json"} : undefined,
    body: body ? JSON.stringify(body) : undefined,
  });
  const data = await res.json();
  if (!res.ok || data.ok === false) throw new Error(data.error || "request failed");
  return data;
}
async function capture(kind) {
  els.cue.className = "cue countdown";
  els.cueTitle.textContent = "准备";
  els.cueDetail.textContent = "采样已触发，等待倒计时开始";
  els.meterBar.style.width = "0%";
  await api("/api/capture", {
    kind,
    countdown_seconds: Number(els.countdown.value || 0),
    seconds: Number(els.seconds.value || 8),
  });
  refresh();
}
async function buildReport() {
  els.report.disabled = true;
  try { await api("/api/report", {}); }
  catch (err) { els.statusTitle.textContent = "报告失败"; els.statusDetail.textContent = err.message; }
  finally { els.report.disabled = false; refresh(); }
}
async function resetSession() {
  if (!confirm("清空当前真人/回放样本并开始新一轮？")) return;
  await api("/api/reset", {});
  refresh();
}
function metricLine(item) {
  const m = item.metrics || {};
  return `${m.duration_sec || "-"}s / RMS ${m.rms_dbfs ?? "-"} dBFS / Peak ${m.peak_dbfs ?? "-"} dBFS`;
}
function renderSamples(list, node) {
  node.innerHTML = "";
  if (!list.length) {
    node.innerHTML = `<div class="metric">暂无</div>`;
    return;
  }
  for (const item of list) {
    const div = document.createElement("div");
    div.className = "sample";
    div.innerHTML = `<div class="sample-title"><span>${item.label}</span><span>${item.kind}</span></div><div class="metric">${metricLine(item)}</div><div class="metric">${item.path}</div>`;
    node.appendChild(div);
  }
}
function renderReport(state) {
  const report = state.report;
  if (!report) {
    els.reportBox.textContent = state.ready?.can_report ? "可生成报告" : "暂无报告";
    return;
  }
  const decision = report.decision || {};
  const reasons = (decision.reasons || []).join(" / ");
  els.reportBox.innerHTML = `<div class="decision">${decision.decision || "-"}</div><div>score ${decision.score ?? "-"} / ${decision.policy_action || "-"}</div><div class="reasons">${reasons}</div><p><a href="/report.html" target="_blank">打开 HTML 报告</a></p>`;
}
function renderCue(state) {
  const job = state.active_job;
  if (state.ready?.busy && job) {
    if (job.stage === "countdown") {
      const elapsed = Number(job.stage_elapsed_sec || 0);
      const remain = Math.max(0, Math.ceil(Number(job.stage_remaining_sec ?? job.countdown_seconds ?? 0)));
      const total = Math.max(job.countdown_seconds || 0, 0.001);
      const pct = Math.min(100, Math.max(0, elapsed / total * 100));
      els.cue.className = "cue countdown";
      els.cueTitle.textContent = remain > 0 ? `准备，${remain}` : "马上开始";
      els.cueDetail.textContent = job.kind === "live" ? "还不用说，等出现“现在说话”" : "还不用播放，等出现“现在播放手机录音”";
      els.meterBar.style.width = `${pct}%`;
      return;
    }
    if (job.stage === "recording") {
      const elapsed = Number(job.stage_elapsed_sec || 0);
      const remain = Math.max(0, Math.ceil(Number(job.stage_remaining_sec ?? job.seconds ?? 0)));
      const total = Math.max(job.seconds || 0, 0.001);
      const pct = Math.min(100, Math.max(0, elapsed / total * 100));
      els.cue.className = "cue recording";
      els.cueTitle.textContent = job.kind === "live" ? "现在说话" : "现在播放手机录音";
      els.cueDetail.textContent = `正在录 ${job.label}，剩余约 ${remain} 秒`;
      els.meterBar.style.width = `${pct}%`;
      return;
    }
  }
  if (state.files?.length) {
    els.cue.className = "cue done";
    els.cueTitle.textContent = "录制完成";
    els.cueDetail.textContent = state.ready?.can_report ? "真人和回放样本已就绪，可以生成报告。" : "继续录另一类样本，或多录几条真人 baseline。";
    els.meterBar.style.width = state.ready?.can_report ? "100%" : "45%";
    return;
  }
  els.cue.className = "cue";
  els.cueTitle.textContent = "准备采样";
  els.cueDetail.textContent = "点击按钮后先倒计时，再开始录制。";
  els.meterBar.style.width = "0%";
}
async function refresh() {
  let state;
  try { state = await api("/api/state"); }
  catch (err) { els.statusTitle.textContent = "连接失败"; els.statusDetail.textContent = err.message; return; }
  els.source.textContent = `${state.source_id}`;
  const busy = state.ready?.busy;
  els.live.disabled = busy;
  els.replay.disabled = busy;
  els.report.disabled = busy || !state.ready?.can_report;
  els.reset.disabled = busy;
  if (busy && state.active_job) {
    els.statusTitle.textContent = state.active_job.stage === "countdown" ? "倒计时" : "录制中";
    els.statusDetail.textContent = `${state.active_job.label} / ${state.active_job.seconds}s`;
  } else if (state.last_error) {
    els.statusTitle.textContent = "需要处理";
    els.statusDetail.textContent = state.last_error;
  } else {
    els.statusTitle.textContent = "准备就绪";
    els.statusDetail.textContent = state.session_dir || "等待采样";
  }
  const files = state.files || [];
  renderSamples(files.filter(x => x.kind === "live"), els.liveList);
  renderSamples(files.filter(x => x.kind === "replay"), els.replayList);
  renderCue(state);
  renderReport(state);
}
els.live.addEventListener("click", () => capture("live"));
els.replay.addEventListener("click", () => capture("replay"));
els.report.addEventListener("click", buildReport);
els.reset.addEventListener("click", resetSession);
setInterval(refresh, 700);
refresh();
</script>
</html>
"""


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Run the HarborNavi voice sampling button panel")
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=8092)
    parser.add_argument("--rtsp-url")
    parser.add_argument("--rtsp-url-file")
    parser.add_argument("--output-root", default=DEFAULT_OUTPUT_ROOT)
    parser.add_argument("--ffmpeg-bin", default=shutil.which("ffmpeg") or "ffmpeg")
    parser.add_argument("--python-bin", default=sys.executable)
    parser.add_argument("--demo-script", default=str(Path(__file__).with_name("harbornavi_voice_replay_demo.py")))
    parser.add_argument("--source-id", default="tp-link-231-stream2")
    parser.add_argument("--seconds", type=float, default=DEFAULT_SECONDS)
    parser.add_argument("--countdown", type=float, default=DEFAULT_COUNTDOWN_SECONDS)
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    rtsp_url = read_rtsp_url(args)
    panel = VoiceCapturePanel(
        rtsp_url=rtsp_url,
        output_root=Path(args.output_root),
        ffmpeg_bin=args.ffmpeg_bin,
        python_bin=args.python_bin,
        demo_script=Path(args.demo_script),
        source_id=args.source_id,
        default_seconds=clamp_seconds(args.seconds),
        default_countdown=clamp_countdown(args.countdown),
    )
    server = ThreadingHTTPServer((args.host, args.port), make_handler(panel))
    print(f"HarborNavi voice capture panel listening on http://{args.host}:{args.port}", flush=True)
    server.serve_forever()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
