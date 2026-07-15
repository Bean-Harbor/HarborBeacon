# HarborNavi Voice Replay Demo

更新时间：2026-06-15

## 1. Demo 目标

做一个极简可演示闭环：

```text
真人现场说话 -> 通过
手机播放本人录音 -> 失败或进入 step-up
UI/报告展示失败原因
```

这个 demo 的定位是 `replay-aware voice entry`，用于证明 Harbor Trust Gateway 可以把语音入口可信度纳入本地决策。它不声明银行级声纹认证，也不把 voice spoof detector 当成唯一安全边界。

## 2. 算力约束

先按 `.82` 上的 RTX 5060 Ti 作为上限约束：

- P0 demo 不依赖 GPU，只用 Python 标准库分析 WAV。
- P0 不调用云端。
- P0 不要求 PyTorch、torchaudio、numpy、scipy。
- 5060 Ti 留给 P1：本地 ASR、speaker embedding、神经网络 replay/spoof 模型或更完整的 UI 实时推理。

这个取法能保证第一版先跑起来；后续模型替换时不改变 Trust Gateway 的输入输出形态。

## 3. 当前实现

脚本：

```text
scripts/harbornavi_voice_replay_demo.py
scripts/harbornavi_voice_capture_panel.py
```

测试：

```text
tests/test_harbornavi_voice_replay_demo.py
```

脚本输入：

- 多段真人 live WAV，建立当前用户和当前 capture source 的 baseline。
- 一段 candidate WAV，判断是 live 还是 replay 风险。
- 可选 challenge phrase，用于动态挑战。

采样面板：

- 在 K3 或能访问摄像头 RTSP 音频的机器上运行。
- 页面提供 `录真人样本`、`录手机回放`、`生成判断报告` 三个按钮。
- 点击后按页面倒计时抓取固定长度音频，避免远程聊天带来的时机偏差。
- RTSP URL 通过环境变量或本地 0600 文件注入，不写入仓库。

脚本输出：

- JSON report
- HTML report
- Trust Gateway projection：
  - `capture_source_kind`
  - `audio_live_mode`
  - `spoof_risk`
  - `policy_action`
  - `raw_audio_retention_policy`

报告只包含 metadata 和派生特征。原始音频由 demo 输入文件保留在本地，不进入 report 或审计。

## 4. 录音建议

在 `.82` 或连接了麦克风的机器上创建样本目录：

```bash
mkdir -p /var/tmp/harbornavi-voice-replay-demo
```

真人录 3 段 live baseline：

```bash
ffmpeg -f alsa -i default -t 3 -ar 16000 -ac 1 /var/tmp/harbornavi-voice-replay-demo/live-1.wav
ffmpeg -f alsa -i default -t 3 -ar 16000 -ac 1 /var/tmp/harbornavi-voice-replay-demo/live-2.wav
ffmpeg -f alsa -i default -t 3 -ar 16000 -ac 1 /var/tmp/harbornavi-voice-replay-demo/live-3.wav
```

手机播放录音，使用同一个麦克风录 candidate：

```bash
ffmpeg -f alsa -i default -t 3 -ar 16000 -ac 1 /var/tmp/harbornavi-voice-replay-demo/phone-replay.wav
```

如果没有 `alsa default` 设备，先用 `ffmpeg -sources alsa` 或系统录音工具确认设备名。

使用摄像头麦克风时，可启动采样面板：

```bash
export HARBORNAVI_VOICE_RTSP_URL='rtsp://USER:PASSWORD@192.168.3.231/stream2'
python3 scripts/harbornavi_voice_capture_panel.py \
  --host 0.0.0.0 \
  --port 8092 \
  --source-id tp-link-231-stream2 \
  --output-root /tmp/harbornavi-voice-spoof-demo
```

生产化前不要把 RTSP URL 写入命令历史或仓库。现场演示更推荐使用
`--rtsp-url-file` 指向权限为 `0600` 的本地文件。

## 5. 运行方式

无动态挑战：

```bash
python3 scripts/harbornavi_voice_replay_demo.py \
  --live /var/tmp/harbornavi-voice-replay-demo/live-1.wav \
  --live /var/tmp/harbornavi-voice-replay-demo/live-2.wav \
  --live /var/tmp/harbornavi-voice-replay-demo/live-3.wav \
  --candidate /var/tmp/harbornavi-voice-replay-demo/phone-replay.wav \
  --label phone-replay \
  --json-out /var/tmp/harbornavi-voice-replay-demo/phone-replay.report.json \
  --html-out /var/tmp/harbornavi-voice-replay-demo/phone-replay.report.html
```

带动态挑战：

```bash
python3 scripts/harbornavi_voice_replay_demo.py \
  --live /var/tmp/harbornavi-voice-replay-demo/live-1.wav \
  --live /var/tmp/harbornavi-voice-replay-demo/live-2.wav \
  --live /var/tmp/harbornavi-voice-replay-demo/live-3.wav \
  --candidate /var/tmp/harbornavi-voice-replay-demo/phone-replay.wav \
  --expected-challenge 4837 \
  --observed-transcript "hey harbor" \
  --json-out /var/tmp/harbornavi-voice-replay-demo/challenge.report.json \
  --html-out /var/tmp/harbornavi-voice-replay-demo/challenge.report.html
```

动态挑战失败会直接进入 `step_up_required`，这是 demo 最稳的拦截路径。

## 6. 验收标准

P0 demo 通过条件：

1. 真人 live candidate 输出 `decision=live_passed`。
2. 手机录音回放输出 `decision=replay_rejected` 或 `decision=uncertain_step_up`。
3. HTML 报告显示至少一个风险原因，例如：
   - `challenge_phrase_mismatch`
   - `high_frequency_energy_loss`
   - `transient_detail_loss`
   - `speaker_or_codec_compression`
   - `channel_mismatch_from_live_baseline`
4. JSON report 中 `trust_gateway_projection.policy_action` 为：
   - live：`allow_trust_gateway_entry`
   - replay/uncertain：`step_up_required`
5. 报告不包含 raw audio、设备凭据、RTSP URL、API key 或本地私有路径以外的敏感材料。

## 7. 下一步接入

P0 先作为离线 demo。接入 Harbor 产品路径时，按这个顺序推进：

1. Harbor Assistant 增加一个本地 demo panel，上传 live/candidate WAV 后展示 HTML/JSON 结果。
2. HarborBeacon 增加 `trust_gateway.voice_replay.evaluate` audit action。
3. Voice Ingress Adapter 把 `spoof_risk`、`risk_reason`、`audio_live_mode` 写入 `TrustGatewayRequestContext`。
4. L2/L3 语音动作在 `spoof_risk != low` 时进入 approval。
5. P1 在 5060 Ti 上替换或叠加神经网络 replay/spoof 模型，保留同一份 JSON 输出形态。

## 8. 边界

- 这个 demo 不要求 HarborNavi 本体内置麦克风。
- 手机 App、WebUI、USB 麦克风、智能音箱、摄像头麦克风都可以作为 capture source。
- 声纹只能作为 identity confidence feature，不能单独授权 L2/L3。
- replay detector 不可用或判断不确定时，策略降级为 step-up。
