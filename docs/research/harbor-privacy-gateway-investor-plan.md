# Harbor Privacy Gateway 投资口径与研究路线

更新时间：2026-06-10

## 一句话口径

Harbor 的隐私壁垒不应停留在普通“打码”或单点 PII 检测，而应做成家庭全模态数据上云前的本地隐私推理网关：本地先理解任务需要什么、哪些信息会被直接识别或间接推断，再只把云端完成任务所需的最小事实上传。

面向投资人的说法可以更短：

> 我们把最新多模态隐私研究工程化到家庭边缘设备和云端模型路由之间。云端拿到的是任务最小事实，不是家庭原始数据。

## 为什么这是现在的问题

1. **云端算力仍然刚需。** 家庭边缘设备无法长期承担所有 VLM、SLM、RAG、长视频理解和复杂规划任务，产品必须允许云端 fallback。
2. **家庭数据比通用办公数据更敏感。** 摄像头、语音、OCR、设备日志、时间线、房间结构和家庭成员关系叠加后，泄露的不是单个字段，而是家庭日常模式。
3. **传统脱敏太窄。** 只识别人脸、手机号、地址或证件号，无法覆盖跨模态、跨时间线、说话人身份、背景声音和任务相关性带来的推断风险。

## 关键概念

- PII = Personally Identifiable Information，中文可称“个人可识别信息”或“个人身份识别信息”。
- Harbor 要处理的不只是 PII，还包括 contextual privacy 和 inferential privacy：同一条信息在不同关系、场景、目的下敏感度不同；多条弱线索组合后可能推断出住址、作息、健康状态、家庭关系或安全习惯。
- 核心产品抽象：`Home signals -> Local Privacy Gateway -> task-minimal semantic capsule -> Cloud model -> audited answer`。

## 第一优先级论文复核

### 1. SopriBench / Argus

- 机构：The Hong Kong University of Science and Technology (Guangzhou), Wuhan University。
- 来源：https://arxiv.org/html/2606.06784v1
- 价值：研究公共图文内容中的用户级隐私泄露，重点不是单条内容中的显式敏感字段，而是跨帖子、跨图文、跨元数据累积后的隐私推断。
- 对 Harbor 的启发：家庭时间线、多摄像头事件、语音片段、设备状态和 OCR 信息天然会形成更强的跨时间线推断风险。Harbor 可以做家庭版 benchmark 和本地阻断网关。
- 局限：原场景是社交媒体，不是家庭 AIoT；Argus 的完整实现、提示和工具路由未完全公开。
- 合作方向：`Home Multimodal Privacy Benchmark`，把社交媒体的 cross-post leakage 扩展到家庭事件时间线。

### 2. VoxPrivacy

- 机构：CUHK-Shenzhen / Zhizheng Wu 团队相关。
- 来源：https://arxiv.org/abs/2601.19956 和 https://interactionalprivacy.github.io/
- 价值：把 speech language model 放到 shared smart home / multi-user setting 中，评估模型能否根据说话人身份管理信息流。
- 对 Harbor 的启发：家庭语音入口必须知道“谁在问、谁拥有信息、当前是否允许披露”。这不是 ASR 文字脱敏能解决的问题。
- 局限：重点在语音和说话人访问控制，不覆盖完整视频、OCR、设备日志和家庭时间线。
- 合作方向：本地说话人验证、interactional privacy policy、家庭成员权限模型。

### 3. VoxSafeBench

- 机构：CUHK-Shenzhen / Amphion 相关团队。
- 来源：https://arxiv.org/abs/2604.14548 和 https://github.com/AmphionTeam/VoxSafeBench
- 价值：评估语音模型在 safety、fairness、privacy 三个维度上的 social alignment，重点是 who / how / where 这些音频线索会改变回答边界。
- 对 Harbor 的启发：家庭场景中背景儿童声音、情绪、醉酒/失能状态、多人重叠说话，都可能改变是否能回答、是否能调用工具、是否能上云。
- 局限：更偏 benchmark，不是完整产品架构。
- 合作方向：家庭语音入口的 cue-aware privacy gate，尤其是背景音和多人场景。

### 4. PII-Bench

- 机构：Fudan University。
- 来源：https://arxiv.org/html/2502.18545v2
- 价值：提出 query-unrelated PII masking，不是把所有 PII 都抹掉，而是只遮蔽与当前任务无关的个人信息，以保持 LLM 任务质量。
- 对 Harbor 的启发：这是 `task-minimal semantic capsule` 的文本层基础。云端需要的信息可以保留，无关身份、联系方式、家庭成员细节应本地遮蔽。
- 局限：文本为主，无法覆盖完整全模态推断风险。
- 合作方向：中文场景 query-aware masking 复现，扩展到 OCR、聊天记录和设备日志。

### 5. PrivaCI-Bench

- 机构：HKUST KnowComp 相关。
- 来源：https://arxiv.org/abs/2502.17041 和 https://hkust-knowcomp.github.io/privacy/
- 价值：明确指出只看 PII 太窄，要用 Contextual Integrity 理论评估信息流是否符合目的、角色、场景和合规要求。
- 对 Harbor 的启发：家庭隐私不是静态字段表，而是“谁把什么信息、为了什么目的、传给谁、是否可审计”的信息流治理。
- 局限：更偏法律/合规和文本推理，离家庭多模态工程实现还有距离。
- 合作方向：把 contextual integrity 映射到 HarborBeacon 的 route policy、privacy transform、audit evidence。

## Harbor 产品路线

### v0：投资叙事和可演示样例

- 输出一页 PPT 和一份研究路线文档。
- PPT 首页不展示论文证据，除非已经形成明确合作；论文和高校信息先留在研究文档、备份页或 Q&A。
- 选 8-12 个家庭隐私样例：客厅摄像头、门口包裹、孩子声音、老人健康、家庭成员位置、账单 OCR、设备日志、访客记录。
- 每个样例标注：原始输入、任务目标、可上云最小事实、必须本地遮蔽信息、潜在跨模态推断风险。

### v1：Home Multimodal Privacy Benchmark

- 构建 30-50 个合成家庭场景，覆盖文本、语音、图像、OCR、设备状态、时间线。
- 指标分三类：直接 PII 泄露、任务无关 PII 泄露、跨模态/跨时间线推断泄露。
- 产出可用于 investor demo 的 before / after：原始云端输入 vs Harbor semantic capsule。

### v2：Harbor Privacy Gateway 原型

- 本地执行任务意图解析和隐私分类。
- 将原始家庭数据转换为 `task-minimal semantic capsule`。
- 对每次云端调用记录 `privacy_level`、`PrivacyTransformRecord`、`InferenceRun` 关联关系。
- 云端只接收完成当前任务所需事实，不接收完整原始家庭数据。

### v3：产学研合作

- 第一优先级：HKUST(GZ) SopriBench / Argus 团队，合作家庭多模态时间线隐私 benchmark。
- 第二优先级：CUHK-Shenzhen VoxPrivacy / VoxSafeBench 团队，合作多用户家庭语音隐私访问控制。
- 第三优先级：Fudan PII-Bench 团队，合作中文 query-aware masking 复现和全模态扩展。
- 补充方向：HKUST KnowComp / PrivaCI-Bench，用 contextual integrity 完善 Harbor 的隐私信息流治理口径。

## 投资页路线图

投资页不写成研发项目管理清单，而写成投资人能判断的阶段里程碑：

1. 0-90 天：技术可信度。完成 benchmark 复现、Harbor 家庭场景样例集和可演示 Privacy Gateway。
2. 3-6 个月：产品楔子。接入摄像头、语音、OCR、设备日志等关键家庭信号，形成隐私审计和本地 / 云端模型路由。
3. 6-12 个月：商业验证。完成种子用户或试点场景，量化隐私风险下降、云端推理成本和用户可理解的审计体验。
4. 12-18 个月：平台壁垒。沉淀任务最小化策略库、家庭多模态评测集和合作背书，把 Privacy Gateway 固化为 Harbor 模型路由前的默认控制层。

## 内部 90 天执行拆解

1. 第 1-2 周：复现 SopriBench / VoxPrivacy / PII-Bench 的关键评测逻辑，整理可对外讲的 evidence pack。
2. 第 3-4 周：定义 Harbor 家庭隐私 benchmark schema，生成 30-50 个合成场景。
3. 第 5-8 周：做 Privacy Gateway 原型，输出 semantic capsule、脱敏记录和云端 route 决策。
4. 第 9-10 周：准备 investor demo，包含 before / after 样例和审计链路。
5. 第 11-12 周：联系作者，提出 benchmark 共建、实习/顾问、联合技术报告三种合作选项。

## 投资人讲法

> 过去的隐私保护主要是找 PII：人脸、手机号、地址、证件号。家庭 AI 的问题更复杂。一个摄像头画面、一次语音、一个快递单、几次设备状态，单独看可能都不敏感，但组合起来可以推断住址、作息、家庭成员关系和安全习惯。
>
> Harbor 的判断是：本地算力不可能永远覆盖所有模型能力，云端算力仍然要用。但上云前必须经过一个本地 Privacy Gateway。它先理解任务，再判断哪些事实是完成任务必要的，哪些信息会造成直接或推断泄露。最后云端拿到的是任务最小事实，而不是家庭原始数据。
>
> 这条路线不是凭空想象，背后有一批多模态隐私、语音隐私、query-aware masking 和 contextual integrity 的研究趋势。但在合作落地前，投资首页不把论文或学校名字当作背书。首页先讲 Harbor 能沉淀的资产：家庭场景样例集、任务最小化策略库、可审计模型路由，以及后续产学研合作入口。
>
> 我们要做的是把这些研究方向工程化到家庭边缘设备里，成为 Harbor 云端模型路由之前的控制层。

## 对外措辞边界

- 可以说：Harbor 正在构建家庭全模态 Privacy Gateway，把最新多模态隐私研究工程化到边缘设备和云端模型路由之间。
- 可以说：我们关注的不是单一 PII 打码，而是任务最小化、上下文隐私、跨模态/跨时间线推断风险。
- 投资首页暂不展示论文证据墙；除非已经形成合作，否则论文只作为内部研究依据、备份材料或 Q&A。
- 暂不说：我们已经解决所有多模态隐私问题。
- 暂不说：我们已经与论文作者达成合作。
- 暂不说：我们拥有原创基础算法突破。
