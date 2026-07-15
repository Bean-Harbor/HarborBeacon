import importlib.util
import math
import struct
import sys
import tempfile
import time
import unittest
import wave
from pathlib import Path


SCRIPT_PATH = Path(__file__).resolve().parents[1] / "scripts" / "harbornavi_voice_capture_panel.py"
SPEC = importlib.util.spec_from_file_location("harbornavi_voice_capture_panel", SCRIPT_PATH)
capture_panel = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = capture_panel
SPEC.loader.exec_module(capture_panel)


def write_wav(path: Path, seconds: float = 1.0, sample_rate: int = 16_000) -> None:
    total = int(seconds * sample_rate)
    with wave.open(str(path), "wb") as handle:
        handle.setnchannels(1)
        handle.setsampwidth(2)
        handle.setframerate(sample_rate)
        pcm = bytearray()
        for index in range(total):
            value = int(math.sin(2 * math.pi * 440 * index / sample_rate) * 12000)
            pcm.extend(struct.pack("<h", value))
        handle.writeframes(bytes(pcm))


class VoiceCapturePanelTests(unittest.TestCase):
    def test_wav_metrics_extract_basic_signal_shape(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "sample.wav"
            write_wav(path)

            metrics = capture_panel.wav_metrics(path)

        self.assertEqual(metrics["sample_rate"], 16_000)
        self.assertEqual(metrics["channels"], 1)
        self.assertAlmostEqual(metrics["duration_sec"], 1.0, places=2)
        self.assertLess(metrics["rms_dbfs"], 0)
        self.assertGreater(metrics["peak"], 10_000)

    def test_redact_url_removes_credentials(self):
        redacted = capture_panel.redact_url("rtsp://admin:secret@example.test:554/stream2")

        self.assertEqual(redacted, "rtsp://<cred>@example.test:554/stream2")

    def test_build_report_command_keeps_all_live_samples(self):
        cmd = capture_panel.build_report_command(
            "python3",
            Path("demo.py"),
            [Path("live-1.wav"), Path("live-2.wav")],
            Path("phone.wav"),
            Path("report.json"),
            Path("report.html"),
        )

        self.assertEqual(cmd.count("--live"), 2)
        self.assertIn("phone.wav", cmd)
        self.assertIn("report.html", cmd)

    def test_reset_returns_state_without_deadlock(self):
        with tempfile.TemporaryDirectory() as tmp:
            panel = capture_panel.VoiceCapturePanel(
                rtsp_url="rtsp://example.test/stream2",
                output_root=Path(tmp),
                ffmpeg_bin="ffmpeg",
                python_bin="python3",
                demo_script=Path("demo.py"),
                source_id="test-source",
                default_seconds=1,
                default_countdown=0,
            )

            state = panel.reset()

        self.assertEqual(state["source_id"], "test-source")
        self.assertIsNotNone(state["session_dir"])

    def test_index_contains_clear_capture_cues(self):
        page = capture_panel.render_index()

        self.assertIn("准备采样", page)
        self.assertIn("现在说话", page)
        self.assertIn("现在播放手机录音", page)
        self.assertIn("清空本轮", page)

    def test_snapshot_includes_server_side_stage_timing(self):
        with tempfile.TemporaryDirectory() as tmp:
            panel = capture_panel.VoiceCapturePanel(
                rtsp_url="rtsp://example.test/stream2",
                output_root=Path(tmp),
                ffmpeg_bin="ffmpeg",
                python_bin="python3",
                demo_script=Path("demo.py"),
                source_id="test-source",
                default_seconds=1,
                default_countdown=0,
            )
            with panel.lock:
                job = capture_panel.JobState(
                    job_id="job-test",
                    kind="live",
                    label="live-1",
                    status="countdown",
                    stage="countdown",
                    started_at=capture_panel.iso_now(),
                    stage_started_at=capture_panel.iso_now(),
                    stage_started_unix=time.time() - 1.0,
                    seconds=8,
                    countdown_seconds=3,
                )
                panel.state.active_job = job
                state = panel._snapshot_unlocked()

        self.assertIn("stage_elapsed_sec", state["active_job"])
        self.assertIn("stage_remaining_sec", state["active_job"])
        self.assertLess(state["active_job"]["stage_remaining_sec"], 3)


if __name__ == "__main__":
    unittest.main()
