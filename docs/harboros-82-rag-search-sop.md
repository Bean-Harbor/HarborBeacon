# HarborOS .82 RAG Search 测试 SOP

最后验证日期：2026-06-24

## 适用范围

本文档用于测试工程师在 HarborOS `.82` 上验收 Harbor Assistant 的 RAG Search 功能。

本轮已经验证通过的范围：

- Settings 页面可以浏览 `/mnt`，并保存 RAG 文档目录。
- Search tab 继续复用现有瀑布流结果形态；不新增 Ask/RAG Chat tab。
- Search 结果卡展示 ranking 诊断信息：`score`、`hybrid_score`、`lexical_score`、`embedding_score`；如果后端返回 `rerank_score`，也会展示 `rerank_score`。
- `reply_pack.citations` 会展示在瀑布流结果下方，方便测试工程师核对 citation 顺序和最终结果排序。
- `.82` 上的 reranker 真机验收暂未通过：rerank-capable Beacon 加 TEI sidecar 在这台 11 GiB 内存、无 swap 的主机上触发 OOM，因此已经恢复到稳定的已安装 Beacon。

## 当前 .82 基线

- WebUI 入口：`http://192.168.3.82/ui/harbor-assistant`
- Settings 深链：`http://192.168.3.82/ui/harbor-assistant?tab=settings&section=ai&focus=semantic-index`
- Search 深链：`http://192.168.3.82/ui/harbor-assistant?tab=search`
- 测试文档目录：`/mnt/software/harborbeacon-agent-ci/rag-test-docs`
- 已配置 source root id：`rag-test-docs`
- 已索引文件数：4
- 已观察到的 chunk/embedding 数：364
- 当前稳定 Beacon 二进制：`/usr/bin/harboros-beacon`
- 当前 reranker 状态：TEI sidecar 已停止；在部署足够内存环境下的 rerank-capable Beacon 之前，`rerank_score` 预期为空或不出现。

注意：本轮用于验证的 WebUI bundle 是在 `.82` 上通过 live test overlay 挂载的。如果 `.82` 重启，测试新 ranking/citation 展示前，需要先请工程同学重新挂载或安装 Harbor Assistant WebUI bundle。

## 测试文档

测试目录中应包含以下文件：

- `weknora-readme.md`
- `weknora-readme-cn.md`
- `rust-book-ownership.md`
- `python-tutorial-venv.txt`

如果缺少任一文件，停止测试，并请工程同学刷新 `/mnt/software/harborbeacon-agent-ci/rag-test-docs`。

## 添加数据源

1. 打开 `http://192.168.3.82/ui/harbor-assistant?tab=settings&section=ai&focus=semantic-index`。
2. 使用环境负责人提供的 HarborOS WebUI 测试账号登录。
3. 进入 `Settings -> AI -> Data sources`。
4. 点击 `Add data source`。
5. 在目录选择器中浏览到 `/mnt/software/harborbeacon-agent-ci/rag-test-docs`。
6. 点击 `Use this folder`。
7. 确认该目录出现在 `Added data sources` 列表中。
8. 刷新页面。
9. 确认刷新后该 source root 仍然存在。

预期结果：

- 目录选择器可以浏览 `/mnt`。
- 选择的目录可以保存。
- 保存成功后，UI 会提示下一步点击 `Start indexing`；点击 `Use this folder` 不应自动启动索引。

## 启动索引

1. 保持在 Settings 的 AI 区域。
2. 点击 `Start indexing`。
3. 等待 index status 变为 `ready`、`completed`，或明确显示 `degraded`。
4. 记录以下信息：
   - status
   - document count
   - embedding count
   - warnings/blockers

当前 `.82` 的预期基线：

- `status=ready`
- `document_count=4`
- 无 blockers
- 测试文档有 embedding entries

以下情况判定为失败：

- 刷新后 source root 消失
- index status 一直停留在 `needs-config`
- index root 不可写
- document count 为 `0`

## Search 测试

打开 `http://192.168.3.82/ui/harbor-assistant?tab=search`。

依次执行以下查询：

1. `WeKnora 混合检索 rerank 知识库 RAG`
2. `Rust ownership memory compiler checks rules`
3. `Python virtual environments packages venv installation`

每条 query 需要记录：

- 瀑布流 top 3 结果标题
- top 3 的 chunk id 和行号范围
- 页面可见的 `SCORE`
- 页面可见的 `HYBRID`
- 页面可见的 `LEXICAL`
- 页面可见的 `VECTOR`
- 页面可见的 `RERANK`，如果有
- warning 文案，如果有
- citation 数量和 top 3 citation 标题

当前 `.82` 的预期结果：

- WeKnora query 首位应为 `weknora-readme-cn.md`。
- Rust query 首位应为 `rust-book-ownership.md`。
- Python venv query 首位应为 `python-tutorial-venv.txt`。
- Citations 面板应出现在瀑布流结果下方，并与后端返回的最终顺序一致。
- 当前稳定 Beacon 未启用 reranker，因此不预期出现 `RERANK`。

## Reranker 降级测试

当前 `.82` 已处于稳定的无 reranker 状态。

预期行为：

- Search 仍返回 HTTP 200 和瀑布流结果。
- `rerank_score` 保持为空或不出现。
- 不应配置或选择 cloud reranker。

当工程同学在足够内存的环境重新部署 rerank-capable Beacon 后，再执行以下 reranker 验收：

1. 仅在 loopback 启动本地 TEI。
2. 启用 local/sidecar 类型的 `rerank_compatible` endpoint。
3. 确认 endpoint smoke test 会真实 POST 到 `/rerank`。
4. 重新执行三条 Search query。
5. 确认 reranked 结果中出现 `RERANK`，并且 citation 顺序变化符合语义预期。
6. 停止 TEI，确认 Search 仍返回 RRF 结果，并显示 reranker warning，而不是请求失败。

## Pass/Fail 标准

当前 Search 和数据源配置验收通过条件：

- 测试工程师可以完全通过 WebUI 添加 RAG 文档目录。
- 保存后的 source root 刷新后仍存在。
- Indexing 可以完成，或以明确 warning 降级。
- 三条 Search query 都返回预期文档。
- 瀑布流结果卡展示 ranking diagnostics。
- Citation 面板出现，并且可以与结果顺序对照。

以下条件全部满足之前，不要将 `.82` reranker 验收标记为通过：

- rerank-capable Beacon 运行时不再 OOM。
- local/sidecar TEI endpoint smoke test 通过。
- 至少两条 query 的 reranked citation 顺序更符合语义预期。
- 停用 TEI 后 Search 能优雅降级为 RRF 结果，而不是搜索失败。

## 常见 blocker 处理表

| 现象 | 可能原因 | 测试处理方式 |
| --- | --- | --- |
| 无法浏览 `/mnt` | WebUI session 过期，或 Beacon API 不可用 | 刷新页面并重新登录后重试；仍失败则截图错误并停止测试。 |
| source root 保存失败 | Settings API 失败，或目录重复校验失败 | 截图错误，检查该目录是否已经在列表中。 |
| index status 为 `needs-config` | 没有启用的 source root | 添加 `/mnt/software/harborbeacon-agent-ci/rag-test-docs` 后重新点击 `Start indexing`。 |
| index root 不可写 | 主机存储或权限问题 | 停止测试，请工程同学检查 `/mnt/software/harborbeacon-agent-ci/knowledge-index`。 |
| `document_count=0` | 目录选错、扩展名不支持，或测试文档为空 | 确认四个预期测试文件存在。 |
| embedding degraded | 本地 embedding endpoint 不可用，或 warmup 失败 | Search 可能仍可做词法降级测试；记录 warning 后只继续验证 lexical fallback。 |
| reranker unavailable | TEI 停止、endpoint disabled，或当前 Beacon 不支持 reranker | 当前 `.82` 基线不启用 reranker；不要因此判定 Search-only 测试失败。 |
| reranker 配置后 Search 返回 500 | Beacon state 不兼容，或 Beacon 被 OOM kill | 停止测试，恢复稳定 Beacon，并通知工程同学。 |
| Preview 失败 | 文件 preview route 或路径权限问题 | 记录失败 result path 和 query；Search ranking 可单独继续评估。 |
