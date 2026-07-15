#!/usr/bin/env python3
"""HarborNavi voice replay demo.

This is a deliberately small demo runner for the "live owner voice passes,
phone replay is rejected" investor/product prototype. It uses only Python's
standard library so it can run before the .82 CUDA/PyTorch stack is fixed.

The detector is not a production anti-spoofing model. It combines a random
challenge check with lightweight channel/replay heuristics and emits a
Trust-Gateway-shaped JSON/HTML report that can later be wired into Beacon audit.
"""

from __future__ import annotations

import argparse
import html
import json
import math
import statistics
import sys
import time
import wave
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Iterable


POLICY_VERSION = "harbornavi-voice-replay-demo-p0"


@dataclass
class AudioFeatures:
    path: str
    sample_rate: int
    duration_sec: float
    rms_dbfs: float
    peak_dbfs: float
    crest_db: float
    zcr: float
    diff_ratio: float
    diff2_ratio: float
    energy_cv: float
    silence_fraction: float
    clipping_fraction: float
    dc_offset: float


@dataclass
class FeatureStats:
    mean: float
    stdev: float


@dataclass
class ReplayDecision:
    decision: str
    spoof_risk: str
    score: float
    reasons: list[str]
    challenge_passed: bool | None
    policy_action: str


FEATURE_KEYS = [
    "rms_dbfs",
    "crest_db",
    "zcr",
    "diff_ratio",
    "diff2_ratio",
    "energy_cv",
    "silence_fraction",
    "clipping_fraction",
    "dc_offset",
]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Run HarborNavi voice replay demo")
    parser.add_argument("--live", action="append", required=True, help="Live owner WAV sample. Pass multiple times.")
    parser.add_argument("--candidate", required=True, help="Candidate WAV sample to classify.")
    parser.add_argument("--label", default="candidate", help="Human-readable candidate label.")
    parser.add_argument("--expected-challenge", help="Random challenge phrase, for example 4837.")
    parser.add_argument("--observed-transcript", help="Transcript observed from ASR or manual demo input.")
    parser.add_argument("--json-out", help="Write JSON report to this path.")
    parser.add_argument("--html-out", help="Write HTML report to this path.")
    parser.add_argument("--replay-threshold", type=float, default=0.65)
    parser.add_argument("--uncertain-threshold", type=float, default=0.45)
    args = parser.parse_args(argv)

    live_features = [extract_features(Path(path)) for path in args.live]
    candidate = extract_features(Path(args.candidate))
    baseline = build_baseline(live_features)
    challenge_passed = evaluate_challenge(args.expected_challenge, args.observed_transcript)
    decision = decide_replay(
        candidate,
        baseline,
        challenge_passed,
        replay_threshold=args.replay_threshold,
        uncertain_threshold=args.uncertain_threshold,
    )

    report = build_report(args.label, live_features, candidate, baseline, decision)
    json_text = json.dumps(report, ensure_ascii=False, indent=2)

    if args.json_out:
        Path(args.json_out).write_text(json_text + "\n", encoding="utf-8")
    else:
        print(json_text)

    if args.html_out:
        Path(args.html_out).write_text(render_html(report), encoding="utf-8")

    return 0 if decision.decision == "live_passed" else 2


def read_wav_mono(path: Path) -> tuple[int, list[float]]:
    with wave.open(str(path), "rb") as handle:
        channels = handle.getnchannels()
        sample_width = handle.getsampwidth()
        sample_rate = handle.getframerate()
        frame_count = handle.getnframes()
        raw = handle.readframes(frame_count)

    if sample_width not in (1, 2, 3, 4):
        raise ValueError(f"unsupported WAV sample width {sample_width} in {path}")
    if channels < 1:
        raise ValueError(f"invalid WAV channel count {channels} in {path}")

    samples: list[float] = []
    frame_bytes = sample_width * channels
    scale = float(2 ** (8 * sample_width - 1))
    offset = 0
    while offset + frame_bytes <= len(raw):
        values = []
        for channel in range(channels):
            start = offset + channel * sample_width
            chunk = raw[start : start + sample_width]
            if sample_width == 1:
                value = int.from_bytes(chunk, "little", signed=False) - 128
                values.append(value / 128.0)
            else:
                value = int.from_bytes(chunk, "little", signed=True)
                values.append(max(-1.0, min(1.0, value / scale)))
        samples.append(sum(values) / len(values))
        offset += frame_bytes

    return sample_rate, trim_edges(samples)


def trim_edges(samples: list[float]) -> list[float]:
    if not samples:
        return samples
    peak = max(abs(sample) for sample in samples) or 1.0
    threshold = max(peak * 0.015, 0.002)
    first = 0
    while first < len(samples) and abs(samples[first]) < threshold:
        first += 1
    last = len(samples) - 1
    while last > first and abs(samples[last]) < threshold:
        last -= 1
    trimmed = samples[first : last + 1]
    return trimmed if len(trimmed) >= min(len(samples), 512) else samples


def extract_features(path: Path) -> AudioFeatures:
    sample_rate, samples = read_wav_mono(path)
    if not samples:
        raise ValueError(f"empty WAV: {path}")

    rms = root_mean_square(samples)
    peak = max(abs(sample) for sample in samples)
    diffs = [samples[index] - samples[index - 1] for index in range(1, len(samples))]
    diff2 = [diffs[index] - diffs[index - 1] for index in range(1, len(diffs))]
    zcr = zero_crossing_rate(samples)
    frame_rms = short_time_rms(samples, max(256, min(2048, sample_rate // 40)))
    mean_frame = statistics.fmean(frame_rms) if frame_rms else rms
    energy_cv = safe_div(stdev(frame_rms), mean_frame)
    silence_threshold = max(rms * 0.18, 0.003)
    silence_fraction = safe_div(sum(1 for value in frame_rms if value < silence_threshold), len(frame_rms))
    clipping_fraction = safe_div(sum(1 for sample in samples if abs(sample) > 0.985), len(samples))
    dc_offset = statistics.fmean(samples)

    return AudioFeatures(
        path=str(path),
        sample_rate=sample_rate,
        duration_sec=len(samples) / sample_rate,
        rms_dbfs=dbfs(rms),
        peak_dbfs=dbfs(peak),
        crest_db=dbfs(peak) - dbfs(rms),
        zcr=zcr,
        diff_ratio=safe_div(root_mean_square(diffs), rms),
        diff2_ratio=safe_div(root_mean_square(diff2), rms),
        energy_cv=energy_cv,
        silence_fraction=silence_fraction,
        clipping_fraction=clipping_fraction,
        dc_offset=dc_offset,
    )


def build_baseline(features: list[AudioFeatures]) -> dict[str, FeatureStats]:
    if not features:
        raise ValueError("at least one live sample is required")
    baseline = {}
    for key in FEATURE_KEYS:
        values = [float(getattr(item, key)) for item in features]
        baseline[key] = FeatureStats(mean=statistics.fmean(values), stdev=stdev(values))
    return baseline


def decide_replay(
    candidate: AudioFeatures,
    baseline: dict[str, FeatureStats],
    challenge_passed: bool | None,
    replay_threshold: float,
    uncertain_threshold: float,
) -> ReplayDecision:
    reasons: list[str] = []
    weighted_distance = 0.0
    weights = {
        "rms_dbfs": 0.9,
        "crest_db": 1.2,
        "zcr": 1.0,
        "diff_ratio": 1.4,
        "diff2_ratio": 1.2,
        "energy_cv": 1.0,
        "silence_fraction": 0.7,
        "clipping_fraction": 1.0,
        "dc_offset": 0.5,
    }
    total_weight = sum(weights.values())

    for key, weight in weights.items():
        stats = baseline[key]
        value = float(getattr(candidate, key))
        scale = max(stats.stdev, abs(stats.mean) * 0.08, feature_floor(key))
        weighted_distance += weight * min(abs(value - stats.mean) / scale, 4.0)

    distance_score = min(1.0, weighted_distance / (total_weight * 3.0))
    artifact_score = 0.0

    if challenge_passed is False:
        reasons.append("challenge_phrase_mismatch")
        artifact_score += 0.5

    if candidate.diff_ratio < baseline["diff_ratio"].mean * 0.82:
        reasons.append("high_frequency_energy_loss")
        artifact_score += 0.18
    if candidate.diff2_ratio < baseline["diff2_ratio"].mean * 0.78:
        reasons.append("transient_detail_loss")
        artifact_score += 0.14
    if candidate.crest_db < baseline["crest_db"].mean - 2.5:
        reasons.append("speaker_or_codec_compression")
        artifact_score += 0.13
    if candidate.energy_cv < baseline["energy_cv"].mean * 0.70:
        reasons.append("over_smooth_replay_energy")
        artifact_score += 0.10
    if candidate.clipping_fraction > max(0.01, baseline["clipping_fraction"].mean + 0.015):
        reasons.append("speaker_clipping_artifact")
        artifact_score += 0.10
    if abs(candidate.rms_dbfs - baseline["rms_dbfs"].mean) > 8.0:
        reasons.append("capture_level_mismatch")
        artifact_score += 0.08

    score = min(1.0, distance_score * 0.72 + artifact_score)
    if not reasons and score >= uncertain_threshold:
        reasons.append("channel_mismatch_from_live_baseline")
    if not reasons:
        reasons.append("matches_live_voice_channel_baseline")

    if score >= replay_threshold:
        return ReplayDecision(
            decision="replay_rejected",
            spoof_risk="high",
            score=round(score, 4),
            reasons=reasons,
            challenge_passed=challenge_passed,
            policy_action="step_up_required",
        )
    if score >= uncertain_threshold:
        return ReplayDecision(
            decision="uncertain_step_up",
            spoof_risk="medium",
            score=round(score, 4),
            reasons=reasons,
            challenge_passed=challenge_passed,
            policy_action="step_up_required",
        )
    return ReplayDecision(
        decision="live_passed",
        spoof_risk="low",
        score=round(score, 4),
        reasons=reasons,
        challenge_passed=challenge_passed,
        policy_action="allow_trust_gateway_entry",
    )


def build_report(
    label: str,
    live_features: list[AudioFeatures],
    candidate: AudioFeatures,
    baseline: dict[str, FeatureStats],
    decision: ReplayDecision,
) -> dict:
    return {
        "ok": decision.decision == "live_passed",
        "demo": "harbornavi_voice_replay_demo",
        "policy_version": POLICY_VERSION,
        "generated_at_unix": int(time.time()),
        "compute_profile": {
            "target": ".82 RTX 5060 Ti limit",
            "p0_runtime": "python-standard-library-cpu",
            "p1_upgrade_slot": "local ASR + neural replay/spoof model on CUDA when available",
            "cloud_required": False,
        },
        "candidate_label": label,
        "decision": asdict(decision),
        "candidate_features": asdict(candidate),
        "live_sample_count": len(live_features),
        "live_features": [asdict(item) for item in live_features],
        "baseline": {key: asdict(value) for key, value in baseline.items()},
        "trust_gateway_projection": {
            "capture_source_kind": "voice_demo_wav",
            "audio_live_mode": "live_or_replay_candidate",
            "speaker_hint": "owner_demo",
            "spoof_risk": decision.spoof_risk,
            "policy_action": decision.policy_action,
            "audit_material": "metadata_only",
            "raw_audio_retention_policy": "demo_inputs_local_only",
        },
    }


def render_html(report: dict) -> str:
    decision = report["decision"]
    passed = decision["decision"] == "live_passed"
    accent = "#138a50" if passed else "#b42318"
    title = "Live voice passed" if passed else "Replay blocked / step-up required"
    reasons = "".join(f"<li>{html.escape(reason)}</li>" for reason in decision["reasons"])
    features = report["candidate_features"]
    rows = "".join(
        f"<tr><td>{html.escape(key)}</td><td>{html.escape(str(round(value, 5) if isinstance(value, float) else value))}</td></tr>"
        for key, value in features.items()
        if key != "path"
    )
    return f"""<!doctype html>
<html lang=\"zh-CN\">
<meta charset=\"utf-8\">
<title>HarborNavi Voice Replay Demo</title>
<style>
body {{ font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; margin: 36px; color: #15151b; }}
.hero {{ border-left: 6px solid {accent}; padding: 14px 18px; background: #f7f7fb; }}
.score {{ font-size: 44px; font-weight: 750; color: {accent}; margin: 12px 0; }}
.grid {{ display: grid; grid-template-columns: 1fr 1fr; gap: 20px; margin-top: 24px; }}
.card {{ border: 1px solid #e6e1ef; border-radius: 8px; padding: 18px; }}
table {{ border-collapse: collapse; width: 100%; }}
td {{ border-bottom: 1px solid #eee; padding: 7px 4px; font-size: 13px; }}
h1, h2 {{ margin: 0 0 12px; }}
code {{ background: #f0edf8; padding: 2px 5px; border-radius: 4px; }}
</style>
<body>
  <div class=\"hero\">
    <h1>HarborNavi Voice Replay Demo</h1>
    <div>{html.escape(title)}</div>
    <div class=\"score\">{decision["score"]}</div>
    <div>Policy action: <code>{html.escape(decision["policy_action"])}</code></div>
  </div>
  <div class=\"grid\">
    <section class=\"card\">
      <h2>Why</h2>
      <ul>{reasons}</ul>
      <p>Raw audio stays local to the demo input files. This report contains metadata and derived features only.</p>
    </section>
    <section class=\"card\">
      <h2>Trust Gateway Projection</h2>
      <pre>{html.escape(json.dumps(report["trust_gateway_projection"], ensure_ascii=False, indent=2))}</pre>
    </section>
    <section class=\"card\">
      <h2>Candidate Features</h2>
      <table>{rows}</table>
    </section>
    <section class=\"card\">
      <h2>Compute Profile</h2>
      <pre>{html.escape(json.dumps(report["compute_profile"], ensure_ascii=False, indent=2))}</pre>
    </section>
  </div>
</body>
</html>
"""


def evaluate_challenge(expected: str | None, observed: str | None) -> bool | None:
    if expected is None:
        return None
    if observed is None:
        return False
    return normalize_phrase(expected) in normalize_phrase(observed)


def normalize_phrase(value: str) -> str:
    return "".join(ch.lower() for ch in value if ch.isalnum())


def root_mean_square(values: Iterable[float]) -> float:
    values = list(values)
    if not values:
        return 0.0
    return math.sqrt(sum(value * value for value in values) / len(values))


def short_time_rms(samples: list[float], frame_size: int) -> list[float]:
    if frame_size <= 0:
        return []
    hop = max(1, frame_size // 2)
    return [root_mean_square(samples[index : index + frame_size]) for index in range(0, len(samples) - frame_size + 1, hop)]


def zero_crossing_rate(samples: list[float]) -> float:
    if len(samples) < 2:
        return 0.0
    crossings = 0
    prev = samples[0]
    for sample in samples[1:]:
        if (prev < 0 <= sample) or (prev >= 0 > sample):
            crossings += 1
        prev = sample
    return crossings / (len(samples) - 1)


def dbfs(value: float) -> float:
    return 20.0 * math.log10(max(abs(value), 1e-9))


def safe_div(numerator: float, denominator: float | int) -> float:
    if not denominator:
        return 0.0
    return numerator / denominator


def stdev(values: list[float]) -> float:
    if len(values) < 2:
        return 0.0
    return statistics.stdev(values)


def feature_floor(key: str) -> float:
    if key in {"rms_dbfs", "crest_db"}:
        return 1.0
    if key in {"zcr", "silence_fraction", "clipping_fraction"}:
        return 0.01
    if key == "dc_offset":
        return 0.005
    return 0.05


if __name__ == "__main__":
    sys.exit(main())
