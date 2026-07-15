import importlib.util
import math
import struct
import sys
import tempfile
import unittest
import wave
from pathlib import Path


SCRIPT_PATH = Path(__file__).resolve().parents[1] / "scripts" / "harbornavi_voice_replay_demo.py"
SPEC = importlib.util.spec_from_file_location("harbornavi_voice_replay_demo", SCRIPT_PATH)
voice_demo = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = voice_demo
SPEC.loader.exec_module(voice_demo)


def write_wav(path: Path, samples: list[float], sample_rate: int = 16_000) -> None:
    with wave.open(str(path), "wb") as handle:
        handle.setnchannels(1)
        handle.setsampwidth(2)
        handle.setframerate(sample_rate)
        pcm = bytearray()
        for sample in samples:
            value = int(max(-1.0, min(1.0, sample)) * 32767)
            pcm.extend(struct.pack("<h", value))
        handle.writeframes(bytes(pcm))


def synth_live(seed: int, sample_rate: int = 16_000, seconds: float = 2.0) -> list[float]:
    count = int(sample_rate * seconds)
    samples = []
    for index in range(count):
        t = index / sample_rate
        envelope = min(1.0, index / 800) * min(1.0, (count - index) / 800)
        noise = math.sin((index + seed * 17) * 12.9898) * 0.012
        value = (
            0.28 * math.sin(2 * math.pi * (180 + seed) * t)
            + 0.13 * math.sin(2 * math.pi * (430 + seed * 3) * t)
            + 0.055 * math.sin(2 * math.pi * (2200 + seed * 11) * t)
            + noise
        )
        samples.append(value * envelope)
    return samples


def phone_replay_transform(samples: list[float]) -> list[float]:
    smoothed = []
    for index in range(len(samples)):
        window = samples[max(0, index - 5) : index + 1]
        value = sum(window) / len(window)
        value = max(-0.42, min(0.42, value * 1.9))
        smoothed.append(value)
    return smoothed


def classify(tmp_path: Path, candidate_samples: list[float], challenge_passed=None):
    live_paths = []
    for idx, seed in enumerate([1, 2, 3]):
        path = tmp_path / f"live-{idx}.wav"
        write_wav(path, synth_live(seed))
        live_paths.append(path)
    candidate_path = tmp_path / "candidate.wav"
    write_wav(candidate_path, candidate_samples)

    live_features = [voice_demo.extract_features(path) for path in live_paths]
    candidate = voice_demo.extract_features(candidate_path)
    baseline = voice_demo.build_baseline(live_features)
    return voice_demo.decide_replay(candidate, baseline, challenge_passed, 0.65, 0.45)


class VoiceReplayDemoTests(unittest.TestCase):
    def test_live_candidate_passes(self):
        with tempfile.TemporaryDirectory() as tmp:
            decision = classify(Path(tmp), synth_live(4), challenge_passed=True)

        self.assertEqual(decision.decision, "live_passed")
        self.assertEqual(decision.policy_action, "allow_trust_gateway_entry")

    def test_phone_replay_candidate_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            replay = phone_replay_transform(synth_live(4))
            decision = classify(Path(tmp), replay, challenge_passed=True)

        self.assertIn(decision.decision, {"replay_rejected", "uncertain_step_up"})
        self.assertEqual(decision.policy_action, "step_up_required")
        self.assertGreaterEqual(decision.score, 0.45)
        self.assertTrue(
            any(
                reason in decision.reasons
                for reason in [
                    "high_frequency_energy_loss",
                    "transient_detail_loss",
                    "speaker_or_codec_compression",
                ]
            )
        )

    def test_challenge_mismatch_forces_step_up(self):
        with tempfile.TemporaryDirectory() as tmp:
            decision = classify(Path(tmp), synth_live(4), challenge_passed=False)

        self.assertEqual(decision.policy_action, "step_up_required")
        self.assertIn("challenge_phrase_mismatch", decision.reasons)


if __name__ == "__main__":
    unittest.main()
