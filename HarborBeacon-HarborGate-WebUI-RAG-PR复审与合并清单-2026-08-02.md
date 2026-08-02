# HarborBeacon、HarborGate、WebUI RAG PR 复审与合并清单

更新时间：2026-08-02

审核范围：HarborBeacon #37、HarborGate `codex/rag-trusted-principal`、WebUI `codex/rag-dualstack-merge-safe`。

## 结论

本轮已按 Gate -> WebUI -> Beacon 的顺序完成 Harbor Assistant RAG 用户端点收口。三仓代码、独立复审、PR CI、目标分支 CI 和 squash 合并均已完成，实际 head 与合并提交记录见本清单后半部分。

本轮边界已经锁定：

- HarborBeacon 继续拒绝 `source_scope=dvr_library`。录像检索和制品接口由 Camera/HarborLink 工作线新增，本轮不恢复 Beacon 文件扫描，也不改写 Camera 的 422。
- 浏览器不持有 HarborBeacon 共享服务 bearer。HarborOS WebUI 用户端点使用 30 秒单次 token 向 Gate 认证，Gate 校验后替换可信 principal 头，并以 `HARBORBEACON_WEB_API_TOKEN` 调用 Beacon。
- Harbor Assistant 本轮仅允许 HarborOS `webui_access=true` 且拥有 `FULL_ADMIN` 的用户，按 `harboros:uid:<pw_uid>` 隔离会话。
- 一次性 token 仅用于搜索、会话列表/详情/删除和会话设置。Camera、直播、媒体预览与 Range 请求保持原协议和原路径。
- 不修改 middleware 仓库，不操作 `.82`，不合并 HarborGate 现有无关 PR，也不在本轮执行 release/deploy。

## 锁定基线

- HarborBeacon #37：`feature/RAG` -> `master`，复审 head `b02550daafb6dbe3dade88bc3001b0ca4620a6f5`，base `bdac607c1767926f43afcfffae1bff0801e61ef4`。锁定时 `harborlink_package_contract`、`rust_quality`、`schema_check` 均成功。
- WebUI：`feature/RAG` head `49d095eddeacbe332990bd71c9e3ae77b001c0b9`，目标 `develop` head `6fccfb59c09558df05d3f14b050dc8cc8b725096`；锁定时尚无 PR。
- HarborGate：`main` head `00f1a5605bbb7b29b2e95249ad6261e99d9ce378`；现有无关 PR #8、#9 不在本轮范围。

工作分支：

- HarborGate：`codex/rag-trusted-principal` -> `main`
- WebUI：`codex/rag-dualstack-merge-safe` -> `develop`
- HarborBeacon：`codex/rag-merge-safe` -> `feature/RAG`；合入后再复审并 squash 合并 #37

## 阻断项关闭情况

### HarborGate

- [x] 新增 `/api/harbor-gate/api/beacon/**` 代理别名；保留 `/api/beacon/**` 给 HarborNavi。
- [x] 对搜索、会话列表/详情/删除和会话设置要求 `X-HarborOS-Auth-Token`。
- [x] 通过 `ws://127.0.0.1:6000/api/current` 调用 `auth.login_ex`；单次认证总超时 5 秒。
- [x] 仅接受 `webui_access=true` 且角色包含 `FULL_ADMIN` 的用户；principal 稳定映射为 `harboros:uid:<pw_uid>`。
- [x] 清除浏览器提供的身份头、身份 query、`Authorization` 和一次性 token，不把它们转发给 Beacon。
- [x] 上游身份只由 Gate 注入：`X-Harbor-Principal-Source`、`X-Harbor-Principal-Id`、`X-Harbor-Principal-Roles`、`X-Harbor-Workspace-Id`。
- [x] RAG/admin proxy 只接受 `HARBORBEACON_WEB_API_TOKEN`；旧 task token 不再作为 Web API token fallback。
- [x] 缺服务 token 返回 503，用户 token 无效返回 401，权限不足返回 403；错误与日志不包含 token。
- [x] 媒体预览不触发用户 token 校验，并完整保留 `Range`、`If-Range`、206、`Content-Range`、`Accept-Ranges` 等响应语义。
- [x] HarborGate v3 合同更新为用户端点 MUST 认证、MUST 替换身份头。
- [x] 增加 PR CI：fmt、clippy、test、release build。

### WebUI

- [x] 标准 WebUI 对五类用户端点逐请求调用 `auth.generate_token [30, {}, false, true]`，通过 Gate 路径发送。
- [x] HarborNavi 保持 `/api/beacon/**`，不依赖 middleware token；Camera、直播和媒体预览保持原路径。
- [x] 搜索只发一次 POST；运行时识别旧平面响应或新 `kind: "rag.answer"` envelope，不做探测或失败重试。
- [x] 新响应以 envelope 的 answer/citations 为准，并合并 degraded、review scope、warnings；旧响应对象原样透传；非法结构只产生一次明确错误。
- [x] 清空最后一个知识来源时把 retrieval mode 切为 `off`，不发送空 `source_root_ids`；重新选择来源后不自动切回 Auto/Force。
- [x] 搜索、加载、新建和删除会话互斥；请求捕获 conversation，过期响应不能写入当前会话。
- [x] 删除会话增加确认和逐行 busy 状态。
- [x] 启动时删除旧 localStorage 查询历史键；建议历史仅保留在当前页面内存。
- [x] 修复移动端纵向布局、popover/标签溢出及 i18n/lint 阻断。
- [x] `harbor-assistant-camera.component.ts` 与 `develop` 完全一致，不吞掉或改写 Camera 工作线的 422。
- [x] 从 PR 移除与 RAG 无关的 `.codex/config.toml` 和 `AGENTS.md`；不合入未锁版本的 `npx @playwright/mcp@latest` 或仓库外默认测试凭据/分支规则。

### HarborBeacon

- [x] 用户作用域端点使用明确的 `GateAuthenticatedPrincipal`；先验证 `HARBORBEACON_WEB_API_TOKEN`，再读取 workspace 和新 principal 头。
- [x] 所有 Beacon 路由别名在归一化后应用同一身份策略；无 bearer 不得 fallback。
- [x] 完全拒绝 legacy 身份头、身份 query 和 owner fallback，包括百分号编码的身份 query 名。
- [x] workspace 必须匹配 active workspace；`FULL_ADMIN` 仅映射为本次请求的 Beacon admin principal，不持久化 membership。
- [x] 缺服务配置返回 503，缺失/错误 bearer 返回 401，身份字段缺失返回 400，角色或 workspace 不符返回 403。
- [x] 会话 ID 包含 principal；跨用户读删返回 404，旧 `local-owner` 历史不自动迁移或暴露。
- [x] ZIP 索引禁用。
- [x] ASR 硬上限为 256 MiB、900 秒，ffprobe 最长 15 秒；环境变量只能进一步收紧。
- [x] embedding 超时后不再留下后台写入，过期结果被丢弃，旧索引保持不变。
- [x] 服务成功 bind 后才回收本实例可见的 stale index job，避免第二实例启动失败时误改活动任务。
- [x] 生成答案引用校验失败时使用确定性 fallback；每个非空行都必须有有效引用标记。
- [x] UTF-8 日志按字符边界截断。
- [x] 音频预览白名单包括 MP3、WAV、M4A、FLAC、AAC、OGG、OPUS，复用既有索引、路径归一化和 Range 边界。
- [x] `dvr_library` 拒绝逻辑保持不变；未修改 Camera/HarborLink adapter。

## Camera/HarborLink 交接

`dvr_library` 的 422 来自 HarborBeacon `resolve_admin_search_source_scope()` 的显式拒绝分支。这个分支不是 middleware 行为，也不是本轮 WebUI Camera 代码引入。

Camera/HarborLink 工作线后续负责：

- 提供录像 timeline/search 接口，支持 camera、时间范围和分页/游标。
- 提供受控 artifact/preview 接口，不把 HarborLink 私有路径暴露给浏览器。
- 定义无结果、索引未就绪、超时和依赖不可用的稳定错误合同。
- 补齐 Camera 侧 422 的 UI 映射和端到端测试。

在该接口完成前，Beacon 必须继续失败关闭，不能以扫描 DVR 目录或“空 ID 等于全部目录”绕过所有权边界。

## 本地验证证据

### HarborGate

- [x] `cargo fmt --all -- --check`
- [x] `cargo test --locked --all-targets`：52 项通过（44 lib、6 proxy integration、2 guard）。
- [x] `cargo build --locked --release`
- [x] CI 使用的 clippy 命令通过；仓库全局严格 clippy 仍有 5 个既有 lint，均位于本轮未修改文件。
- [x] `git diff --check`

覆盖伪造身份头/query 清除、恶意浏览器 Authorization 覆盖、token 过期、middleware 超时/不可用、webui_access/角色拒绝、Web 服务 token 缺失、task token 不得 fallback、日志脱敏和媒体 Range。

### WebUI

- [x] Harbor Assistant 10 suites / 176 tests 全部通过。
- [x] ESLint 0 error；仅 Camera 既有 5 条 HLS warning。
- [x] Stylelint、i18n 675 keys/25 files、delivery check 通过。
- [x] 标准 production build 与 HarborNavi K3 production build 通过；仅项目既有 CommonJS/selector warning。
- [x] `git diff --check`。
- [x] `320x568`、`390x844`、`768x1024` 浏览器验收：无页面横向滚动，popover 保持在视口内，tab/正文不重叠。

### HarborBeacon

- [x] `cargo fmt --all` 与 `git diff --check`。
- [x] 显式设置 `HARBORBEACON_SOUTHBOUND_MODE=harborlink` 后，`agent-hub-admin-api` 全量 144 项测试通过；未设置时按仓库 cutover 规则失败关闭。
- [x] ASR 定向 7 项、Gate principal、embedding deadline，以及空 preview fallback、文档列表、近期媒体列表三类逐行引用测试通过。
- [x] `cargo clippy --lib --tests` 通过（只保留仓库既有 warning）。
- [x] `cargo check --bin harboros-beacon --bin agent-hub-admin-api` 通过。
- [x] release `validate-contract-schemas` 构建和执行通过；本机无 `midclt`/`midcli`，live probe 按工具缺失跳过。
- [x] HarborBeacon #38、#37 与合并后的 master push 均通过 Linux `rust_quality`（611 项 lib、144 项 API binary）、`harborlink_package_contract` 和 `schema_check`。

Windows 全量 lib 的修复前基线为 610 项中 576 通过、33 失败、1 ignored。首个 knowledge child retrieval 失败会导致后续锁 poisoning；代表性 knowledge/task_api 失败已在未修改的 `b02550d` 基线上原样复现，因此不是本轮回归。本轮新增的第 611 项引用回归测试已单独通过；不再等待整套 Windows lib 重跑，Linux PR CI 作为最终门禁。

## PR、CI 与合并记录

- HarborGate [#12](https://github.com/Bean-Harbor/HarborGate/pull/12) 已在 head `4648d00490edbe727818310143ca51a776938520` 全绿后 squash 合入 `main`：`ac79547310da9b7179d131fb81ce8498b4a93e72`。合并后的 main push CI 成功。
- WebUI [#2](https://github.com/Bean-Harbor/webui/pull/2) 已在 head `9c6f3c93168e4779913fea82ee9ab401638453ea` 的构建、变更 lint、Harbor Assistant 测试、HarborNavi 合同、翻译检查和 Docker 全绿后 squash 合入 `develop`：`6c59c207d8fa9c61c565384639d47d2ca3f3ca45`。基线 lint 成功；整仓 baseline tests 明确为 non-blocking，合并时仍在运行，随后也成功完成。该 fork 未生成 develop push workflow。
- HarborBeacon 收口 [#38](https://github.com/Bean-Harbor/HarborBeacon/pull/38) 已在最终 head `eff0835d0b768888a936f97d07d76f960c86b0b3` 三项 CI 全绿、独立复核无 P1/P2 后 squash 合入 `feature/RAG`：`9d6a9f4673819f443ebed457dc3ef3ed2d4f57dd`。
- HarborBeacon [#37](https://github.com/Bean-Harbor/HarborBeacon/pull/37) 已在最终 head `9d6a9f4673819f443ebed457dc3ef3ed2d4f57dd` 三项 CI 全绿后 squash 合入 `master`：`a31cd00751a9e1875db8f8aeafc06193a4c1468a`。合并后的 master push CI 成功。

实际合并顺序：HarborGate #12 -> WebUI #2 -> HarborBeacon #38 -> HarborBeacon #37。每一步均在对应阻断 CI 成功后执行；Beacon CI 暴露的问题先完成根因修复和独立复核，再继续后续合并。

## 不阻塞本轮的后续项

- Camera/HarborLink `dvr_library` 检索与制品接口。
- WebUI 双栈至少保留一个完整发布周期后再评估移除旧平面响应。
- Harbor Assistant 空闲轮询优化、ASR 语言缓存、通用 admin proxy 鉴权。
- HarborLink 连续 DVR retention 和系统级磁盘配额/容量告警；它们仍是 DVR release gate，但不要求 Beacon 越权接管录像目录。
- K3 同维护窗口发布和跨仓集成验收。本轮只完成代码合并，不在 `.82` 上部署或验证。
