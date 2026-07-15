# HarborNavi Voice Trust Model

更新时间：2026-06-12

## 1. 结论

HarborNavi 的语音安全不应按“声纹认证盒子”设计，而应按家庭场景的分层可信交互设计：

```text
语音是意图入口，不是身份事实。
```

HarborNavi 本体当前不带麦克风，也不应为了第一版语音能力强行把麦克风列为硬件前提。语音入口来自外部 capture source，例如：

- 手机 App / WebUI 的语音输入
- IM 语音消息或语音转文字入口
- 已有智能音箱、电视、门口屏、摄像头、对讲设备
- 后续可选的 USB / LAN / BLE 麦克风外设

因此 HarborNavi 负责的是：接收外部语音入口产生的意图、评估这次语音是否可信、按动作风险决定是否需要二次确认，并把最终可执行动作交给 HarborBeacon / Runtime / Home Device Domain 的既有策略链路。

语音只是 [HarborNavi Trust Gateway](./harbornavi-trust-gateway.md) 的一个入口类型。账号分组、私有信息、可分享信息和云端隐私策略由 Trust Gateway 统一治理。

## 2. 设计原则

1. 不把“像某个家庭成员的声音”当成可执行授权。
2. 不要求 HarborNavi BOM 内置麦克风。
3. 不把 voice spoof detector 做成唯一安全边界。
4. 低风险动作保持自然体验，高风险动作必须 step-up。
5. raw audio 默认不持久化；保留必要的 metadata、risk evidence 和 audit record。
6. 外部 capture source 必须显式建模，不能假设“语音来自 HarborNavi 本机附近”。

## 3. 风险分层

| 层级 | 场景 | 语音策略 |
| --- | --- | --- |
| L0 低风险 | 播放音乐、天气、开关灯、普通问答 | wake/session + intent 即可 |
| L1 隐私风险 | 读日程、读家庭消息、查看摄像头摘要、访问家庭时间线 | 需要 household identity confidence、source binding、context risk 共同放行 |
| L2 安全 / 财产风险 | 开门、关闭报警、关闭摄像头、购买、导出视频、分享家庭资料 | 语音只能发起，必须通过 App / WebUI / 本机按钮 / PIN / 已绑定设备确认 |
| L3 管理员风险 | 改权限、解绑设备、删除数据、开启云同步、恢复出厂 | 永远不能 voice-only，必须走管理员确认与审计 |

家庭场景不是安全等级天然更低，而是摩擦预算更低、亲密空间后果更复杂。L0/L1 要顺滑，L2/L3 要克制。

## 4. 架构位置

```text
External Voice Capture Source
  -> Voice Ingress Adapter
  -> ASR / Intent
  -> Household Identity Confidence
  -> Voice Spoof Risk
  -> Context Risk
  -> Action Policy
  -> Step-up Approval
  -> Local Audit
  -> Domain Action Execution
```

### 4.1 Voice Ingress Adapter

负责接收外部语音入口，不假设入口一定带可信硬件。每次语音请求至少应携带：

- `capture_source_id`
- `capture_source_kind`
- `source_binding_state`
- `audio_live_mode`：live / uploaded / im_voice / transcribed_text
- `transcription_source`：local_asr / remote_asr / external_text
- `source_user_hint`
- `latency_ms`
- `raw_audio_retention_policy`

如果入口只提供文字转写，而不提供音频样本，则 spoof risk 只能降级为 `unknown`，不得用于放行 L2/L3。

### 4.2 Household Identity Confidence

这是家庭成员置信度，不是法律身份认证。可选信号包括：

- 本地声纹 embedding
- 已绑定手机 / BLE / LAN presence
- WebUI / App 登录态
- 最近一次本地物理交互
- 家庭角色与权限
- 账号分组：root / admin / member / guest / system
- 信息分享状态：private / home_shared / role_shared / temporary_shared / system_only

声纹只作为 confidence feature，不能单独授权高风险动作。

### 4.3 Voice Spoof Risk

第一版只要求 replay-aware guardrail，不追求一次性覆盖所有 deepfake。候选信号：

- 动态挑战是否通过
- 音频压缩 / 重构 / 残差信号中的播放链路痕迹
- TTS / VC / replay detector 分数
- capture source 的历史基线偏移
- SNR、混响、频响、压缩伪影、截断与回放噪声

由于 HarborNavi 本体不带麦克风，不能依赖本机麦克风阵列、到达方向或本机房间声学作为 P0 必备信号。这些只能在外部 capture source 支持时作为增强项。

### 4.4 Context Risk

家庭语音风险必须结合上下文，而不是只看音频：

- 当前动作风险层级
- 发起入口是否已绑定家庭账户
- 触发位置是否合理
- 摄像头 / VLM presence 是否允许并可用
- 是否有儿童、访客、陌生人、多人的上下文
- 是否来自电视、音箱、手机外放等高 replay 风险入口
- 当前是否处于夜间、离家、报警、访客模式

## 5. Step-up 规则

L2/L3 动作默认必须 step-up。可选确认方式：

- App push 确认
- WebUI 管理员确认
- 本机实体按钮
- PIN / passphrase
- 已绑定手机近场确认
- 摄像头 presence 确认
- 家庭管理员二次审批

动态语音挑战可以作为 step-up 的一部分，但不能替代管理员确认。对于上传的 IM 语音、转写文本、第三方 ASR 文本，动态挑战不可用时必须转入非语音确认。

## 6. P0 / P1 / P2 路线

### P0：Replay-aware Guardrail

- 定义 L0-L3 action policy。
- L2/L3 voice-only 全部阻断并转 step-up。
- 对外部语音入口记录 `capture_source_id`、`audio_live_mode`、risk result 和 audit ref。
- 对 uploaded / IM voice / external text 默认不给高风险授权。
- 可接入轻量 spoof score，但 score 只影响 step-up 文案和风险记录。
- 不把 raw audio 写入默认审计记录。

### P1：Household Voice Confidence

- 可选本地注册家庭成员声纹。
- 声纹 embedding 本地保存，可删除、可重建。
- 成员置信度用于个性化与 L1 访问控制，不用于 L2/L3 单点授权。
- 引入家庭成员、访客、儿童、老人等角色策略。

### P2：Home Replay Evaluation Pack

构建家庭 replay 测试包，用于发布前回归：

- 手机播放主人录音
- 电视 / 音箱播放主人视频声音
- IM 语音消息重放
- TTS / voice conversion 样本
- 不同房间、不同距离、不同噪声环境

必须通过的门槛：

- replayed owner voice 不能开门
- replayed owner voice 不能关闭摄像头
- replayed owner voice 不能解除报警
- replayed owner voice 不能导出、删除或分享家庭资料
- 检测不确定时必须降级为 step-up，而不是直接放行

## 7. Harbor 边界

### HarborBeacon / Framework

拥有：

- 语音入口后的 task / conversation truth
- action policy
- approval state
- audit record
- risk evidence schema
- 本地优先与云端补能策略

### Home Device / AIoT Domain

拥有：

- 摄像头、门锁、报警、灯、传感器等设备动作
- capture source 设备注册与能力声明
- 设备状态、事件、媒体能力

### HarborOS System Domain

拥有：

- HarborOS 系统操作
- middleware API / midcli fallback
- 本机系统服务、存储、账号等系统域动作

### 外部入口

IM、App、WebUI、智能音箱、电视、摄像头、门口屏等都只是入口或 capture source。它们不能绕过 HarborBeacon 的 policy / approval / audit，也不能直接获得 Home Device Domain 执行权。

## 8. 非目标

第一版不做：

- HarborNavi 本体麦克风阵列
- 银行级声纹认证
- voice-only 门锁 / 报警 / 摄像头关闭
- 默认保存家庭 raw audio
- 依赖云端 deepfake 检测作为唯一判定
- 把第三方 ASR 文本当作可信身份信号

## 9. 验收门槛

1. 任意 L2/L3 动作通过语音发起时都生成 approval request，而不是直接执行。
2. 外部语音入口没有音频样本时，spoof risk 必须为 `unknown` 或等价降级状态。
3. 审计记录不包含 raw audio、RTSP URL、设备凭据、API key、私有路径。
4. replay / TTS / IM voice 样本触发敏感动作时必须进入 step-up。
5. spoof detector 不可用时，系统降级为更保守的 policy，而不是默认放行。
6. 可选麦克风外设必须作为 Home Device / AIoT capture source 注册，不成为 HarborNavi 本体假设。
