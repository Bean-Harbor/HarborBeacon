# HarborOS Release Packaging / Install Runbook

更新时间：2026-05-08

## 1. 目的

这份 runbook 只回答一件事：

- 怎样把当前已经接近可用的 HarborBeacon / Harbor Assistant / HarborGate，
  收成 **可重复安装、可升级、可回滚** 的 HarborOS release bundle

它不是业务功能设计文档，也不是 HarborOS live smoke 的替代品，更不是
HarborBeacon 团队自建 ISO 的说明。最终 HarborOS ISO / 镜像构建流程由
HarborOS / ISO 集成同事拥有；本 runbook 只定义我们交给对方的可嵌入
release bundle、安装形态、配置模板和验证门禁。

## 2. 当前发布形态

release-v1 的默认形态固定为：

- Linux builder 负责预构建 HarborBeacon Rust 二进制，包括统一入口 `harborbeacon-service`
- Linux builder 负责预构建 HarborBeacon 单端口推理 facade 所需的内部模型 backend
- Linux builder 负责构建或接收 HarborNAS WebUI production `dist`
- Linux builder 负责预构建 HarborGate Rust binary；HarborGate 主线不再打包 Python runtime fallback
- HarborOS 目标机或 HarborOS ISO 集成流程只负责部署与运行，不在机上执行 `cargo`、`rustc`、`node`、`npm` 或 `pip`
- HarborBeacon Rust Linux 默认目标为 `x86_64-unknown-linux-musl`
- 当目标为 musl 时，builder 使用 `cargo zigbuild --release --target <target>`，并要求 builder 上已有 `cargo-zigbuild` 与 `zig`
- HarborBeacon 单端口封装本地推理 facade；`harbor-model-api`/Candle/VLM 只作为内部 backend，不再成为对外 systemd/API 契约
- Runtime Manager + Model Center 是共享能力层：release 默认保持 Harbor-managed Candle-first local runtime，云端只作为 `semantic.router` 与 `retrieval.answer` 的受控 fallback
- ISO / release bundle 路线允许通过 `harboros-beacon.deb` 内置约 0.5B bootstrap LLM，用于 IM / WebUI 自然语言入口、意图分类、参数抽取和配置引导；大模型、VLM、ASR 与强 embedding 默认仍按需下载或高级外接
- `llm-cloud-siliconflow` preset 使用 `https://api.siliconflow.cn/v1`，API key 由 endpoint metadata secret 保存并通过 admin API redaction 返回
- 本地模型下载默认 mirror 为 `https://hf-mirror.com`；实际优先级为 Harbor Assistant 输入 mirror -> `HF_ENDPOINT` -> 默认 mirror

当前默认 builder：

- HarborBeacon build host `192.168.1.197`
- non-root builder bootstrap 入口：`tools/bootstrap_release_builder.sh`

当前默认 HarborOS 目标机：

- `192.168.3.82`

历史参考：

- `192.168.3.223` 曾作为 Debian verifier / builder baseline。
- `192.168.3.182` 曾作为 HarborOS live target；当前 `.82` 替代它作为
  RC/GA target。

当前默认 install root：

- `/var/lib/harborbeacon-agent-ci`

当前 verified writable root：

- `/var/lib/harborbeacon-agent-ci/writable` on the current `.82` RC2 install.
  `/mnt/software/harborbeacon-agent-ci` remains a supported writable-root
  target when present.

当前 RC baseline：

- HarborBeacon: `a5f6da0`
- HarborGate: `57ff759`
- RC2 bundle:
  `harbor-release-20260430-rc2-beacona5f6da0-gate57ff759.tar.gz`
- SHA256:
  `7119842506d38aac82c7e236b7f96a054244bb50be07c5e6b001ac7b0683484c`

当前 handoff 增量 baseline：

- Harbor Assistant-only cleanup: HarborOS 集成面只保留 `/ui/harbor-assistant` 与 `/api/beacon/**`。
- Media tools packaging: release bundle 必须带
  `media-tools/bin/ffmpeg` 与 `media-tools/bin/ffprobe`，installer 会安装到
  `/var/lib/harborbeacon-agent-ci/runtime/media-tools/bin/` 并写入
  `HARBOR_FFMPEG_BIN` / `HARBOR_FFPROBE_BIN`。
- Debian delivery: GitHub Actions 通过 `.github/workflows/build-deb.yml`
  产出 `harborbeacon-harboros-deb-<version>` artifact，内含
  `harborbeacon-harboros-release_<version>_amd64.deb` 和 checksum。
- `.82` live proof: bundled media tools 安装后，TP-IPC validation 通过，
  Harbor Assistant snapshot endpoint 返回 `200 image/jpeg`。

## 3. 发布物结构

builder 产出一个单一版本化 bundle，结构固定为：

```text
harbor-release-<version>/
  bin/
    harborbeacon-service
    validate-contract-schemas
    run-e2e-suite
  harbor-assistant/dist/harbor-assistant/
  harborgate/bin/harborgate
  install/
    install_harboros_release.sh
    rollback_harboros_release.sh
    verify_release_bundle.py
  media-tools/
    bin/
      ffmpeg
      ffprobe
    NOTICE.txt
    provenance.json
  templates/
    bin/
      harbor-agent-hub-helper
    systemd/
    harborbeacon-agent-hub.env.template
  manifest.json
  checksums.sha256
```

对应目录布局拆成两部分：

```text
/var/lib/harborbeacon-agent-ci/
  releases/<version>/
  current -> releases/<version>
  runtime/
  captures/
  logs/
```

```text
/mnt/software/harborbeacon-agent-ci/
  ... HarborOS writable / mutation root ...
```

说明：

- install root 必须是可执行 release/runtime 根
- writable root 继续承载 HarborOS mutation proof 与 smoke tooling
- 如果 `/mnt/software/harborbeacon-agent-ci` 不可用，installer 才回退到 `<install-root>/writable`

## 4. Builder 侧命令

在 Linux builder 上执行：

```bash
export HARBORGATE_REPO=/path/to/HarborGate
export RUST_TARGET=x86_64-unknown-linux-musl
export BOOTSTRAP_BUILDER_IF_NEEDED=1
bash ./tools/build_release_bundle.sh
```

FFmpeg / ffprobe 是必需 release artifact，不再依赖 HarborOS 现场预装。
默认 media tools 为 BtbN `ffmpeg-master-latest-linux64-lgpl.tar.xz`
(`linux64` x86_64 glibc>=2.28，`lgpl` 排除 GPL-only 库)。如果构建机不能联网，
先下载该 tarball，再显式传入：

```bash
export HARBOR_MEDIA_TOOLS_ARCHIVE=/path/to/ffmpeg-master-latest-linux64-lgpl.tar.xz
export HARBOR_MEDIA_TOOLS_SHA256=<archive-sha256>
bash ./tools/build_release_bundle.sh
```

构建完成后必须运行 bundle verifier：

```bash
python3 ./tools/verify_release_bundle.py \
  dist/release-bundles/harbor-release-<version>.tar.gz \
  --require-execute
```

如果要显式指定版本或输出目录：

```bash
export RELEASE_VERSION=release-v1-20260419
export OUT_DIR=/tmp/harbor-release-bundles
export HARBORGATE_REPO=/path/to/HarborGate
export RUST_TARGET=x86_64-unknown-linux-musl
export BOOTSTRAP_BUILDER_IF_NEEDED=1
bash ./tools/build_release_bundle.sh
```

如果 builder 还没准备好 musl toolchain，也可以先显式执行：

```bash
bash ./tools/bootstrap_release_builder.sh \
  --rust-target x86_64-unknown-linux-musl \
  --rustup-toolchain stable \
  --zig-version 0.15.1
```

builder 预期：

- `cargo-zigbuild` 与 `zig` 已安装在 Linux builder 上
- musl target 在当前用户态 Rust toolchain 中已安装，不要求 root 或 apt 层面的 system-wide 配置
- musl 目标产物必须是 static linkage
- `manifest.json` 必须记录 `rust_target`、`linkage` 和 Linux portability expectation

builder 结果至少应包含：

- `harborbeacon-service` Linux release binary
- HarborNAS WebUI production dist 中的 Harbor Assistant 页面
- HarborGate Rust runtime binary: `harborgate/bin/harborgate`
- BtbN LGPL static media tools: `media-tools/bin/ffmpeg` 与
  `media-tools/bin/ffprobe`
- `media-tools/provenance.json`，记录 source URL、archive sha256、variant
  与 `ffmpeg -version` / `ffprobe -version`
- `manifest.json`
- `checksums.sha256`
- `harbor-release-<version>.tar.gz`

旧 `assistant-task-api`、`agent-hub-admin-api`、`harbor-model-api`、VLM sidecar
可以作为过渡二进制或 benchmark 工具存在，但不再是 release bundle 的主
systemd/API contract。

## 4.1 GitHub Actions / Debian 包交付

本仓新增 `.github/workflows/build-deb.yml`，参考
`HarborNAS/featured-photos` 的 release workflow 形态：在 Linux runner 上构建
release artifact，用 `dpkg-deb --build` 生成 `.deb`，再通过
`actions/upload-artifact` 交给 HarborOS / ISO 集成同事。

触发方式：

- `workflow_dispatch`，可手工输入 `release_version`
- `push` 到 `main` / `master`
- `push` tag `v*` 或 `harbor-release-*`

workflow 固定步骤：

1. checkout HarborBeacon
2. checkout `Bean-Harbor/HarborGate`；私有仓库场景下优先使用
   `BEAN_HARBOR_GITHUB_TOKEN` secret
3. 运行 `tools/build_release_bundle.sh`
4. 运行 `tools/verify_release_bundle.py --require-execute`
5. 运行 `tools/build_harboros_deb.sh`
6. 上传 release bundle 与 `.deb` artifact
7. 如果配置了 `R2_ACCESS_KEY_ID`、`R2_SECRET_ACCESS_KEY`、
   `HARBOROS_R2_ENDPOINT` 和 `HARBOROS_R2_BUCKET`，tag 或手工触发时按
   featured-photos 同款路径上传到 R2 `deb/`

HarborOS 同事取包顺序：

1. 优先下载 Actions artifact `harborbeacon-harboros-deb-<version>`。
2. 如果 HarborOS 发布链已经接入 R2，则从 R2 `deb/` 取同名 `.deb` 和
   `.deb.sha256`。
3. 如需排查 bundle 内容，再下载 companion artifact
   `harborbeacon-release-bundle-<version>`。

Debian package 名称固定为 `harborbeacon-harboros-release`。它是一个 carrier
package，不在 `dpkg -i` 阶段启动或重启 HarborBeacon 服务，也不写入现场凭据。
包内只放入已验证 release bundle、安装脚本和两个 helper：

- `/usr/lib/harborbeacon-harboros-release/bundles/harbor-release-<version>.tar.gz`
- `/usr/lib/harborbeacon-harboros-release/install/install_harboros_release.sh`
- `/usr/lib/harborbeacon-harboros-release/install/verify_release_bundle.py`
- `/usr/sbin/install-harborbeacon-release`
- `/usr/sbin/verify-harborbeacon-release`

HarborOS ISO 或 first-boot 集成流程应显式调用：

```bash
sudo verify-harborbeacon-release --require-execute
sudo install-harborbeacon-release \
  --install-root /var/lib/harborbeacon-agent-ci \
  --writable-root /mnt/software/harborbeacon-agent-ci
```

这样 `.deb` 可以进入 HarborOS 打包链，但服务渲染、env-file、bundled
`ffmpeg` / `ffprobe` 安装、systemd restart 与 rollback 仍由 release bundle
内的 installer 拥有。

## 4.2 Model Backend Benchmark Gate

在把某个 Harbor-managed runtime/model 写成 Harbor Assistant 默认可用之前，
先跑 repo-local benchmark lane。
这里的 lane 分工固定为：

- `.82` 是当前 RC/GA target；`.182` 是历史 Candle runtime gate，保留为可复验 target
- `.197` 是当前 release builder；`.223` 只用于历史 build / prefetch / spawned benchmark 证据
- live path 继续通过 `127.0.0.1:4174/api/inference/v1` 暴露本地推理 facade
- Harbor-managed Candle-first 是默认产品路径；external OpenAI-compatible endpoint 只在用户高级配置后参与
- `4186` 可继续保留给 Candle candidate lane
- ISO nginx / middleware 保持服务级入口：`/api/harbor-beacon/* -> 127.0.0.1:4174` 与 `/api/harbor-gate/* -> 127.0.0.1:8787`，不为模型 runtime 增加新 location
- 现行 `harboros-beacon.service` deb 可继续使用 `/var/lib/harboros-beacon/*.json` 保存 service state；Harbor-managed model-store 独立放在 `/mnt/software/harborbeacon-agent-ci/model-store`
- Bootstrap LLM 由 HarborBeacon release workflow 下载 `Qwen/Qwen2.5-0.5B-Instruct` 并直接打入 `harboros-beacon.deb` 的 `/mnt/software/harborbeacon-agent-ci/model-store/runtimes/harbor-candle/bootstrap-llm`；postinst 只写 env/目录兜底，不在 ISO rootfs 内保留第二份 `/usr/share` 模型副本。

观察已运行服务：

```bash
cargo run --bin benchmark-local-model-backend -- \
  --base-url http://127.0.0.1:4174/api/inference/v1 \
  --healthz-url http://127.0.0.1:4174/api/inference/healthz \
  --backend candle \
  --output /tmp/local-model-benchmark-candle-attached.json
```

正式 promotion gate：

```bash
cargo run --bin benchmark-local-model-backend -- \
  --spawn-binary ./target/x86_64-unknown-linux-musl/release/harbor-model-api \
  --backend candle \
  --bind 127.0.0.1:4186 \
  --candle-chat-model-id Qwen/Qwen3-1.7B \
  --candle-embedding-model-id jinaai/jina-embeddings-v2-base-zh \
  --output /tmp/local-model-benchmark-candle.json
```

只有当报告里的 `gate.promotable=true`，才允许把对应 runtime/model 写成
Harbor Assistant 默认可用。Bootstrap LLM gate 只证明自然语言入口、意图分类、
参数抽取和配置引导可用；full answer / embedding 仍需要自己的 gate。

补充说明：

- `127.0.0.1:4186` 可保留给 `candle` 候选验证实例，例如
  `harbor-model-api-candle-exp` 这种 transient unit；产品默认路径不依赖用户
  `127.0.0.1:11434`。
- Bootstrap LLM 默认打包候选是 `Qwen/Qwen2.5-0.5B-Instruct`，`Qwen/Qwen3-0.6B` 可在通过 gate 后作为同级候选。
- Full local answer / embedding 候选默认组合是 `Qwen/Qwen3-1.7B + jinaai/jina-embeddings-v2-base-zh`。
- `Qwen3.5` 已明确延期到后续 loader/backend 轮次，不进入这一轮 gate。
- HarborOS env template 允许保留这 3 个 side-lane 变量：
  - `HARBOR_MODEL_API_CANDLE_CHAT_MODEL_ID`
  - `HARBOR_MODEL_API_CANDLE_EMBEDDING_MODEL_ID`
  - `HARBOR_MODEL_API_CANDLE_CACHE_DIR`
- `/healthz` 必须区分 runtime alive、model available 与 model loaded；只要
  bootstrap 或 full model 的相应 gate 没过，就不能把该模型写成默认可用。
- `.223` 的 spawned benchmark 只保留为 build / prefetch 兼容性证据；是否允许
  进入 cutover rehearsal，只看 `.182` 的 target-runtime report。
- 当 `.82` / `.182` 已经拿到 `gate.promotable=true` 的 Candle report 时，下一步也仍然是
  单独开一轮 Runtime Manager cutover rehearsal，而不是让 HarborBeacon 自动接管
  用户外部 OpenAI-compatible runtime。

## 5. HarborOS 安装

把 tarball 复制到 HarborOS 后，以 root 安装：

```bash
sudo bash ./install_harboros_release.sh \
  --bundle /path/to/harbor-release-<version>.tar.gz \
  --install-root /var/lib/harborbeacon-agent-ci \
  --writable-root /mnt/software/harborbeacon-agent-ci
```

安装脚本负责：

- 创建/校验 install root 下的 `releases/`, `current/`, `runtime/`, `captures/`, `logs/`
- 创建/校验 HarborOS writable root
- 把 bundle 解包到 `releases/<version>/`
- 更新 `current/` 软链接
- 写入单一 env-file
- 显式写入 `HARBORBEACON_WEB_API_URL=http://127.0.0.1:4174`
- 显式写入 `HARBORBEACON_ADMIN_API_URL=http://127.0.0.1:4174`
- 显式写入 `HARBORBEACON_ADMIN_API_TOKEN=<service-token>`
- 显式写入 `HARBOR_MODEL_API_BASE_URL=http://127.0.0.1:4174/api/inference/v1`
- 显式写入 `HARBOR_MODEL_API_TOKEN=<service-token>`
- 把 bundled media tools 安装到 `${install-root}/runtime/media-tools/bin/`
- 验证 `ffmpeg -version` 与 `ffprobe -version` 可执行；默认失败即停止安装
- 显式写入 `HARBOR_FFMPEG_BIN=/var/lib/harborbeacon-agent-ci/runtime/media-tools/bin/ffmpeg`
- 显式写入 `HARBOR_FFPROBE_BIN=/var/lib/harborbeacon-agent-ci/runtime/media-tools/bin/ffprobe`
- 写入 `HARBOR_HARBOROS_WRITABLE_ROOT=<writable-root>`
- 安装/更新 2 个 systemd 服务单元
- 旧 unit 被 disable/remove，避免旧多端口拓扑在升级后继续漂移
- 更新 `${install-root}/bin/harbor-agent-hub-helper -> current/templates/bin/harbor-agent-hub-helper`
- `daemon-reload`
- 默认 enable/start 2 个 core services

固定安装的 2 个服务单元：

- `harborbeacon.service`
- `harboros-im-gate.service`

clean install 的健康预期：

- 默认活跃服务是 `harborbeacon.service`
- 默认活跃服务是 `harboros-im-gate.service`
- Weixin/Feishu runtime 是 `harboros-im-gate.service` 内部 task，不新增平台级 systemd 服务

## 5.1 `.182` 常驻测试状态助手

安装完成后，`.182` 上固定通过同一个 helper 看 resident stack：

- `/var/lib/harborbeacon-agent-ci/bin/harbor-agent-hub-helper status`
- `/var/lib/harborbeacon-agent-ci/bin/harbor-agent-hub-helper health`
- `sudo /var/lib/harborbeacon-agent-ci/bin/harbor-agent-hub-helper logs gateway --lines 120`

约定：

- `status` 输出 `harborbeacon.service` 与 `harboros-im-gate.service` 的 `is-enabled / is-active / MainPID` 风格摘要 JSON
- `health` 顺序检查 `127.0.0.1:4174`、`127.0.0.1:4174/api/inference/healthz`、`127.0.0.1:8787` 的 loopback health
- `health` 还会带 service auth + `X-Contract-Version: 2.0` 调 `GET /api/gateway/status`
- Weixin 摘要优先读 gateway redacted truth；如果 gateway 暂时不可读，再回退到 `WEIXIN_STATE_DIR/accounts/<account>.runtime.json`
- Weixin 观测至少包含：
  - `last_poll_at`
  - `last_getupdates_error`
  - `last_private_text_message_at`
- `logs` 不起新 daemon，只是展开 `journalctl -u ...`
- `.182` 如果保留默认 journald 权限，`logs` 需要 `sudo` 或 `systemd-journal` 组权限

## 6. 回滚

回滚不是回退数据目录，而是切回上一个版本：

```bash
sudo bash ./rollback_harboros_release.sh \
  --install-root /var/lib/harborbeacon-agent-ci
```

或显式切回某个版本：

```bash
sudo bash ./rollback_harboros_release.sh \
  --install-root /var/lib/harborbeacon-agent-ci \
  --version release-v1-20260419
```

回滚动作固定为：

- 更新 `current/` 指向
- 更新 env-file 中的 `HARBOR_RELEASE_VERSION`，避免回滚后元数据漂移
- 重启 2 个 core systemd 服务
- 回滚前停用旧 unit，保持双服务拓扑

HarborGate 不再在同一个 release 内提供 `python|rust` runtime selector。
如果确实需要回到 Python-capable HarborGate，只能切回上一个已验证的旧 release
artifact，而不是在当前 release 里改 env 开关。

## 7. 安装后验收

安装完成后，继续用现有 HarborOS smoke 做验收，而不是让安装脚本自己冒充 smoke。

这里要保持一个明确口径：

- release install root 可以是 `/var/lib/harborbeacon-agent-ci`
- HarborOS mutation root / writable root 仍然可以是 `/mnt/software/harborbeacon-agent-ci`
- smoke proof 继续引用 writable root，而不是把 install root 当成 mutation proof
- HarborGate admin sync 依赖 `:4174`，不要再让它 fallback 到旧 task API 端口
- 本地 OpenAI-compatible 模型服务固定通过 `:4174/api/inference/v1` 暴露；HarborBeacon 继续拥有检索语义、排序和引用包装
- `HARBOR_KNOWLEDGE_INDEX_ROOT` 必须指向 writable root，例如 `/mnt/software/harborbeacon-agent-ci/knowledge-index`；不要让 retrieval 回落到相对路径 `.harborbeacon/knowledge-index`
- `harbor-agent-hub-helper health` 必须显示 `media_tools.ffmpeg.ok=true`
  与 `media_tools.ffprobe.ok=true`
- camera live acceptance 必须确认
  `/api/beacon/cameras/<device-id>/snapshot.jpg` 返回 `200 image/jpeg`
- `verify-harborbeacon-release --require-execute` 必须能验证 `.deb` 内置 bundle
- 旧 Harbor UI route 不应继续作为 active UI 入口存在
- 旧同源 API prefix 不应继续作为 active HarborBeacon Admin API 前缀存在

Windows host：

```powershell
.\tools\run_harboros_vm_smoke.ps1 `
  -WebSocketUrl ws://192.168.3.182/websocket `
  -Username <harboros-user> `
  -Password '<password>' `
  -AllowMutations `
  -MutationRoot /mnt/software/harborbeacon-agent-ci `
  -ApprovalToken approved `
  -RequiredApprovalToken approved
```

Linux verifier：

```bash
bash ./tools/run_harboros_vm_smoke.sh \
  --websocket-url ws://192.168.3.165/websocket \
  --username <harboros-user> \
  --password '<password>' \
  --allow-mutations \
  --mutation-root /mnt/software/harborbeacon-agent-ci \
  --approval-token approved \
  --required-approval-token approved
```

## 8. 这条 lane 的边界

这条 release packaging / install lane：

- 不新增新的框架对象
- 不新增新的 cross-repo 接口
- 不新增新的 use-case 专用 admin API
- 不新造 Harbor Assistant 独立账号体系

它只负责把现有 v1 能力收成正式安装形态。

## 8.1 Harbor App Layer Boundary

HarborOS multi-app dogfood adds a separate application layer without changing
the core release lane:

- `harboros-beacon` and `harboros-im-gate` remain deb/systemd core services.
- Business apps run through Docker Compose and are managed by the Harbor App
  Manager lane.
- App manifests follow `docs/Harbor-App-Contract-v1.md`.
- App install/start/stop/health/log/exposure operations are HarborOS System
  Domain actions, not IM or Home Device Domain actions.
- App registry, app capability grants, approval, and audit truth remain
  HarborBeacon control-plane state.
- App compose/spec roots default to `/var/lib/harbor/apps/<app_id>/`.
- App data roots default to `/mnt/software/harbor-apps/<app_id>/`.
- App env/secrets default to `/etc/harbor/apps/<app_id>.env`.
- HA Bridge / `home-event-rule-bridge` remains outside the first Harbor App
  batch so its public GitHub-first positioning is not tied to HarborOS.

The release lane may package App Manager helpers later, but ordinary Beacon/Gate
rollback must not depend on a business app container rollback.

## 9. 当前已知 blocker 口径

如果发布安装失败，优先按下面口径归因：

1. exec-root mismatch
   - install root 落在 `noexec` 或不可执行挂载点
   - operator 把 release/runtime 根误放到 writable root
2. binary portability mismatch
   - Rust Linux target 不是预期的 `x86_64-unknown-linux-musl`
   - builder 没产出 static linkage，导致目标机 libc 不匹配
3. IM runtime configuration absence
   - clean install 没有 Weixin 凭据，因此 `harboros-im-gate.service` 内部 Weixin runtime 应被视为 skipped，而不是 bundle 损坏
4. bundle incompleteness
   - 缺 HarborGate Rust binary `harborgate/bin/harborgate`
   - 缺 HarborNAS WebUI production dist / Harbor Assistant 页面
   - 缺 `media-tools/bin/ffmpeg` 或 `media-tools/bin/ffprobe`
   - `manifest.json` 缺 `components.media_tools`
   - 缺 systemd units / env-file / install script
5. builder / host dependency gap
   - builder 缺 `cargo-zigbuild` / `zig`
   - builder 缺 `node/npm`
   - HarborOS 缺 `python3` / `systemd`

如果问题需要靠新增框架对象、改 frozen seam 或加新 admin API 才能解决，
这不是 install lane 内的问题，而是 architect blocker。
