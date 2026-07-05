# HarborBeacon 本地智能体规划文档

> 当前执行更新（2026-04-30）
> 当前落地重点已从 v2.0 双仓升级进入 post-RC2 收口。
> IM 仓库负责 `adapter/gateway/route/平台凭据/delivery`；HarborBeacon 负责 `conversation turn/business state/active frame/approval/artifact/audit`。
> 两边只通过 HTTP/JSON contract 通信，不互相 import，也不共享 `.harborbeacon/*.json`。  
> 本文档以下实施阶段与近期行动，均以 HarborBeacon 侧可执行工作为准。  
> 协作术语统一以 `HarborBeacon-Harbor-Collaboration-Contract-v3` 与 `harbor-*` lane 命名为准。
> 已验证 baseline：HarborBeacon `a5f6da0` + HarborGate `57ff759`
> 已打包为 `.82` RC2
> `20260430-rc2-beacona5f6da0-gate57ff759`，并通过 Harbor Assistant、Search、knowledge search/preview、protected
> `POST /api/web/turns` live smoke，`/api/turns` 仅保留为 deprecated alias。
>
> 本文后续早期 v1.5 task-contract 描述保留为历史上下文；当前执行、验收与回滚以
> `HarborBeacon-HarborGate-v2.0-Upgrade-Runbook.md` 和外部 v2.0 contract 为准。

## 0. 当前协作口径

- `harbor-architect`
  负责跨 lane 边界治理、cutover sequencing、发布/回滚 gate 与最终验收。
- `harbor-framework`
  负责 HarborBeacon 共享 runtime、北向 contract、task/session lifecycle、approval、artifact、audit、本地推理抽象、账号权限与智能编排。
- `harbor-im-gateway`
  负责外部 IM 仓库的 adapter/gateway/route/平台凭据/outbound delivery；在本计划中主要作为跨仓 contract 与联调协作者出现。
- `harbor-hos-control`
  负责 HarborOS System Domain，即 `Middleware API -> MidCLI -> Browser/MCP fallback`。
- `harbor-aiot`
  负责 Home Device Domain，即 camera / AIoT / LAN device 的 southbound 控制与协议适配。

当前阶段默认分工:

- HarborBeacon 仓库内的主实施 owner 默认是 `harbor-framework`。
- 本地推理、审计、账号管理、智能编排默认归 `harbor-framework`。
- 跨 lane 边界变更由 `harbor-architect` 做最终批准。
- 每天收工时，各 lane owner 负责各自仓库或职责线的 GitHub 同步；`harbor-architect` 负责跨 lane 的日终收口与是否可合并/可发布判断。
- 每日同步默认模板见 `docs/daily/harbor-daily-sync-template.md`。

## 0.5 模型架构决策（当前权威）

- 模型是 HarborBeacon 的共享能力层，不是独立业务域；业务域仍按 HarborOS System Domain、Home Device Domain、IM Gateway 等边界治理。
- 当前产品路线保持 local-first，但默认体验从“用户自带本地 OpenAI-compatible upstream”调整为 **Harbor-managed Candle-first local runtime**：Harbor Assistant 通过 HarborBeacon 的推理 facade 调用 Runtime Manager，默认由 Harbor 自管 Candle 承接 LLM / embedding；VLM、OCR、ASR 由 Harbor-managed sidecar 或系统 runtime 补齐。
- OpenAI-compatible endpoint 保留为高级外接配置，用于用户自带 Ollama / vLLM / SGLang / cloud API；Harbor 不自动扫描、复用、启动、停止或迁移用户的 `127.0.0.1:11434`。
- ISO 默认通过 `harboros-beacon.deb` 预置 Harbor Candle runtime 和一个约 0.5B 的 bootstrap LLM，用于 IM / WebUI 自然语言入口、意图分类、参数抽取和配置引导；该模型不承诺高质量长问答、复杂推理、RAG 最终答案或多模态理解。首选 `Qwen/Qwen2.5-0.5B-Instruct`，`Qwen/Qwen3-0.6B` 作为通过 runtime gate 后的同级候选。
- Candle 默认启用为 `installed/enabled/idle`，但不在开机时加载模型权重；模型权重在首次自然语言请求、能力绑定或用户显式选择模型时 lazy-load。
- 云端只作为受控 fallback。第一版仅覆盖 `semantic.router` 与 `retrieval.answer`，不覆盖 AIoT 控制、HarborOS 命令、OCR、VLM、embedding 默认路径。
- Harbor Assistant 的 `Models & Policies` 提供 `llm-cloud-siliconflow` preset，使用 OpenAI-compatible `https://api.siliconflow.cn/v1`；API key 作为 endpoint secret 保存，读回时必须 redacted，空 key 不覆盖已保存 secret。
- 可选本地模型下载默认使用 Hugging Face mirror `https://hf-mirror.com`，优先级为 Harbor Assistant 输入 mirror -> `HF_ENDPOINT` -> 默认 mirror；下载模型写入 Harbor 自己的 model-store，不写入用户 Ollama 模型库。

## 1. 项目目标

为 HarborBeacon 构建一个 **混合计算智能体**，具备：
- ✅ **多模态 RAG** - 支持文本、图像、音频、视频的检索增强生成
- ✅ **智能任务编排** - 动态判断任务复杂度，选择最优执行路径
- ✅ **本地优先策略** - 隐私优先，敏感任务不出本地
- ✅ **受控云端 fallback** - semantic router / RAG answer 在策略放行后才调用云模型
- ✅ **可观测性** - 任务流转过程完全可追溯

当前 post-RC2 执行顺序:

1. 补齐 release evidence、rollback notes、daily closeout、ISO integration checklist。
2. 落地 Runtime Manager 的 Candle-first 默认路径：Candle runtime 默认 enabled/idle，healthz 区分 runtime alive 与 model loaded，默认路径不再探测用户 `11434`。
3. 把约 0.5B bootstrap LLM 纳入 HarborBeacon deb / ISO / first-boot 计划，只绑定到自然语言入口、意图分类、参数抽取和配置引导；1.5B+、VLM、ASR、embedding 强模型继续按需下载或高级外接。
4. 继续硬化 Harbor Assistant 产品面，保持真实同源 API，并按 capability 显示 runtime/model readiness。
5. 恢复 Home Agent Hub / AIoT MVP 队列，不把 Home Device Domain 折叠进
   HarborOS System Domain。

---

## 2. 核心架构设计

### 2.1 三层编排框架

```
┌──────────────────────────────────────────────────────────────┐
│             外部 HarborGate Repo（非本仓实现范围）            │
│  adapters / webhook / websocket / long-poll / route registry │
│  platform credentials / outbound delivery / attachment proxy │
└──────────────────────┬───────────────────────────────────────┘
                       │  HTTP/JSON contract only
                       │  POST /api/web/turns
                       │  POST /api/notifications/deliveries
                       │  GET  /api/gateway/status (optional)
┌──────────────────────▼───────────────────────────────────────┐
│                HarborBeacon 用户交互与任务入口                  │
│  assistant_task_api / admin API / WebUI / local automation  │
└──────────────────────┬───────────────────────────────────────┘
                       │
┌──────────────────────▼───────────────────────────────────────┐
│                HarborBeacon 业务与编排核心                      │
│  task intake / task router / workflow state / approvals     │
│  artifacts / audit / notification intent generation         │
└────────┬─────────────────┬──────────────────┬───────────────┘
         │                 │                  │
         ▼                 ▼                  ▼
   ┌─────────────┐  ┌──────────────┐  ┌─────────────────┐
   │ 本地执行器   │  │ 混合执行器    │  │ 云协作执行器     │
   │(L1-Simple)  │  │(L2-Medium)   │  │(L3-Complex)     │
   └─────────────┘  └──────────────┘  └─────────────────┘
         │                 │                  │
         └────────┬────────┴──────────┬───────┘
                  │                   │
          ┌───────▼──────────┐   ┌────▼────────────┐
          │  本地 RAG + 推理  │   │ 脱敏 + 云推理   │
          │  • Ollama/LLaMA  │   │ • 数据脱敏器    │
          │  • LocalAI       │   │ • 云 API 调用   │
          │  • Vector DB     │   │ • 结果转换      │
          └──────────────────┘   └─────────────────┘
```

当前边界约束:

- HarborGate 拥有 route key 生命周期、平台凭据、平台 payload 格式与实际消息投递。
- HarborBeacon 拥有业务会话真相、可恢复流程、审批、artifact、audit 与 notification intent。
- HarborBeacon 可以持久化 `route_key` 作为写入型路由元数据，但不得解释其平台语义。
- HarborBeacon 不得再依赖 IM 仓库内部模型、运行时代码或 `.harborbeacon/*.json`。

### 2.2 任务分类与路由规则

| 任务级别 | 场景示例 | 本地处理 | 云端处理 | 执行器 |
|---------|---------|---------|---------|--------|
| **L1-Simple** | 简单文本理解、本地文件查询、基础搜索 | ✅ | ❌ | LocalLLM |
| **L2-Medium** | 多文件分析、本地+云混合推理、审核任务 | ✅ 预处理+编排 | ✅ 部分 | HybridExecutor |
| **L3-Complex** | 深度分析、多轮推理、实时翻译、视频字幕 | ❌ 脱敏+上报 | ✅ | CloudExecutor |

### 2.3 多模态 RAG 体系

```
┌──────────────────────────────────────────────────────────┐
│                 多模态数据接入层                        │
│  ┌─────────┬─────────┬─────────┬──────────┐              │
│  │  文本   │   图像  │   音频  │   视频   │              │
│  └─────────┴─────────┴─────────┴──────────┘              │
└──────────────┬───────────────────────────────────────────┘
               │
        ┌──────▼──────────┐
        │ 多模态向量化    │
        │ • CLIP (图文)   │
        │ • Whisper (音)  │
        │ • Text Embed    │
        └──────┬──────────┘
               │
        ┌──────▼──────────────────────────────┐
        │ 混合向量数据库                      │
        │ • Milvus / Weaviate                │
        │ • 支持向量+标量混合查询             │
        │ • 本地 SQLite (轻量)                │
        └──────┬───────────────────────────────┘
               │
        ┌──────▼──────────────────────────────┐
        │ 语义检索 + 重排序                    │
        │ • 密集向量检索 (DPR)                │
        │ • BM25 稀疏检索 (混合)              │
        │ • LLM-Based 重排 (可选云端)         │
        └───────────────────────────────────┘
```

---

## 3. 核心模块详细设计

### 3.1 任务复杂度评估器 (Complexity Assessor)

**输入**: 用户查询 + 上下文
**输出**: 复杂度评分 (0-100) + 建议操作

```python
# 伪代码
class ComplexityAssessor:
    def evaluate(self, task):
        score = 0
        
        # 因素1: 查询长度 (0-10)
        score += min(len(task) / 1000, 10)
        
        # 因素2: 需要的数据模态数 (0-20)
        # 文字(5) + 图像(5) + 音频(5) + 视频(5)
        score += len(task.required_modalities) * 5
        
        # 因素3: 推理步数 (0-20)
        score += task.required_reasoning_steps * 2
        
        # 因素4: 需要实时信息 (0-20)
        if task.needs_realtime:
            score += 20
        
        # 因素5: 多语言处理 (0-15)
        if task.num_languages > 1:
            score += task.num_languages * 5
        
        # 因素6: 定制模型需求 (0-15)
        if task.requires_finetuned_model:
            score += 15
        
        return min(score, 100)
    
    def get_routing_decision(self, score):
        if score < 30:
            return "LOCAL"
        elif score < 70:
            return "HYBRID"
        else:
            return "CLOUD"
```

### 3.2 隐私风险分类器 (Privacy Classifier)

**核心逻辑**: PII 检测 + 数据敏感度评分

```python
class PrivacyClassifier:
    def classify(self, task_data):
        pii_entities = self.detect_pii(task_data)  # PII检测
        sensitivity_score = self.rate_sensitivity(task_data)  # 敏感度评分
        
        if pii_entities and sensitivity_score > 0.7:
            return {
                'confidence': 'HIGH',
                'action': 'ANONYMIZE_THEN_CLOUD',
                'pii_found': pii_entities
            }
        elif sensitivity_score > 0.5:
            return {
                'confidence': 'MEDIUM',
                'action': 'ASK_USER',
                'reason': 'Sensitive data detected'
            }
        else:
            return {
                'confidence': 'LOW',
                'action': 'CAN_PROCESS_LOCALLY'
            }
```

**PII 检测清单**:
- 身份证号、护照号
- 电话号码、邮箱
- 位置信息、家庭地址
- 财务数据 (银行卡、社会保险号)
- 医疗信息
- 面部特征 (如有图像)

### 3.3 智能路由决策引擎 (Routing Engine)

```
决策树:
├─ PII 检测
│  ├─ YES → 脱敏处理
│  │       ├─ 本地可处理 → L1_LOCAL
│  │       └─ 需云端 → ANONYMIZE_THEN_CLOUD
│  └─ NO
│      ├─ 复杂度评分
│      │  ├─ < 30 → L1_LOCAL
│      │  ├─ 30-70 → L2_HYBRID
│      │  └─ > 70 → L3_CLOUD
│      └─ 本地资源
│         ├─ 充足 → 降级到本地执行
│         └─ 不足 → 升级到云端或混合
```

### 3.4 数据脱敏器 (Data Anonymizer)

**脱敏策略**:
1. **结构化脱敏**: PII 字段替换为占位符
2. **语义脱敏**: 保留语义，替换具体值
3. **差分隐私**: 添加噪声保护个体隐私

```python
class DataAnonymizer:
    def anonymize(self, data, sensitivity_level='HIGH'):
        # 第一步: 检测PII
        pii_map = self.detect_and_map_pii(data)
        
        # 第二步: 脱敏
        anonymized_data = data
        for pii_type, entities in pii_map.items():
            for entity in entities:
                placeholder = self.get_placeholder(pii_type)
                anonymized_data = anonymized_data.replace(
                    entity, placeholder
                )
        
        # 第三步: 记录映射 (本地密钥保管)
        self.store_mapping_securely(pii_map)
        
        # 第四步: 返回脱敏数据+反向映射密钥
        return {
            'data': anonymized_data,
            'mapping_key': self.encrypt_mapping(pii_map)
        }
    
    def deanonymize_response(self, cloud_response, mapping_key):
        """云端返回结果后，恢复原始信息"""
        pii_map = self.decrypt_mapping(mapping_key)
        result = cloud_response
        for pii_type, mappings in pii_map.items():
            for placeholder, original in mappings.items():
                result = result.replace(placeholder, original)
        return result
```

### 3.5 本地执行器 (Local Executor)

**组件**:
- **推理引擎**: Ollama/LLaMA 2/Mistral
- **向量数据库**: Milvus (中等规模) 或 FAISS (轻量)
- **可视化模型**: CLIP for 图像理解
- **音频处理**: Whisper (本地推理) 或 offlineASR

```yaml
LocalExecutor:
  models:
    text_generation:
      - mistral:7b
      - llama2:13b
      - zephyr:7b
    embeddings:
      - sentence-transformers/all-MiniLM-L6-v2
    multimodal:
      - openai/clip-vit-base-patch32
    speech:
      - openai/whisper-base
  
  vector_db:
    type: milvus  # 本地部署
    config:
      dimension: 384
      metric_type: L2
      
  max_tokens: 2048
  timeout: 30s
  gpu_allocation: 70%  # 留出余量给系统
```

### 3.6 混合执行器 (Hybrid Executor)

**场景**: 预处理+本地初步分析 → 云端精细分析 → 本地后处理

```python
class HybridExecutor:
    async def execute(self, task):
        # 步骤1: 本地预处理
        preprocessed = await self.local_executor.preprocess(task)
        
        # 步骤2: 评估是否需要云端
        if self.should_call_cloud(preprocessed):
            # 步骤3: 脱敏
            anonymized = self.anonymizer.anonymize(preprocessed)
            
            # 步骤4: 云端推理
            cloud_result = await self.cloud_executor.infer(
                anonymized['data'],
                context=preprocessed['context']
            )
            
            # 步骤5: 反脱敏
            result = self.anonymizer.deanonymize_response(
                cloud_result,
                anonymized['mapping_key']
            )
        else:
            result = preprocessed
        
        # 步骤6: 本地后处理
        final_result = await self.local_executor.postprocess(result)
        
        return final_result
```

### 3.7 云端执行器 (Cloud Executor)

**API 抽象层**, 支持多个云服务:
- OpenAI API (GPT-4/3.5)
- Claude (Anthropic)
- 本地私有云部署

```python
class CloudExecutor:
    def __init__(self, config):
        self.providers = {
            'openai': OpenAIProvider(config.openai_key),
            'anthropic': AnthropicProvider(config.anthropic_key),
            'private_cloud': PrivateCloudProvider(config.endpoint)
        }
        self.request_log = RequestLogger(config.audit_db)
    
    async def infer(self, anonymized_data, context=None):
        # 选择提供商 (可负载均衡)
        provider = self.select_provider()
        
        try:
            response = await provider.call(
                data=anonymized_data,
                context=context,
                model_name=self.config.model
            )
            
            # 审计日志: 记录脱敏状态, 时间戳, 使用量
            self.request_log.log({
                'timestamp': now(),
                'task_id': context['task_id'],
                'anonymized': True,
                'provider': provider.name,
                'tokens_used': response.usage.total_tokens,
                'latency_ms': response.latency
            })
            
            return response.text
        except Exception as e:
            self.handle_cloud_failure(e, context)
            raise
```

---

## 4. 多模态 RAG 实现

### 4.1 数据摄入管道

```
HarborBeacon 文件系统
  └─ 文本: Markdown, PDF, TXT
     └─ OCR 提取 (local: PaddleOCR)
     └─ 分块策略: 递归分块 (segment_size=512, overlap=50)
     └─ 向量化: sentence-transformers
     └─ 写入向量DB
     
  └─ 图像: JPG, PNG, WEBP
     └─ 视觉理解 (CLIP)
     └─ 对象检测 (YOLO)
     └─ 元数据提取 (EXIF, 标签)
     └─ 多向量存储 (CLIP emb + metadata)
     
  └─ 音频: MP3, WAV, M4A
     └─ 转录 (Whisper)
     └─ 情感分析 (sentiment-transformers)
     └─ 向量化 (音频特征)
     └─ 存储文本副本 + 音频向量
     
  └─ 视频: MP4, MKV, AVI
     └─ 关键帧提取
     └─ 场景分割
     └─ 字幕/字幕生成 (Whisper on audio track)
     └─ 图像特征 (per frame)
     └─ 音频转录
     └─ 存储: 时间线索引 + 多模态向量
```

### 4.2 查询理解与检索

```
用户查询: "给我看最近拍的包含人物的照片，对吧们讲一下故事"

       ↓
   查询解析器
   ├─ 实体抽取: ["照片", "人物", "最近"]
   ├─ 意图识别: SEARCH + SUMMARIZE
   └─ 模态需求: IMAGE + TEXT
   
       ↓
   多模态检索
   ├─ 图像检索 (CLIP: "photo with people")
   │  └─ 返回 Top-K 图像 + metadata
   ├─ 时间过滤: "最近" → 时间范围
   ├─ 对象过滤: objects.contains("person")
   │  └─ 使用 YOLO 检测结果
   └─ 重排序
      └─ LLM 相关性评分 (可选)
   
       ↓
   上下文增强
   ├─ 提取关联的文本 (eg. 日记、标签)
   ├─ 构建 RAG 上下文
   └─ 添加时间线索引
   
       ↓
   生成响应
   ├─ 本地LLM 生成故事
   ├─ 引用原始文件
   └─ 返回结果 + 多模态预览
```

### 4.2.1 Search 北向独立入口

Search 是多模态检索的 northbound user retrieval surface，作为
HarborNAS WebUI 原生页面 `/ui/harbor-assistant?tab=search` 存在；Harbor Assistant 继续承担
source roots、index、privacy/resource profile 等管理与配置入口。

Search 只消费 HarborBeacon 的真实同源 knowledge API，经 HarborGate 北向边缘
代理后的公开前缀是：
`POST /api/beacon/knowledge/search` 和
`GET /api/beacon/knowledge/preview`。它不新增 shortcut、mock、聚合 API
或绕开运行时的演示层；documents / images / videos 在页面内合并为瀑布流，
并展示 `content_source_kinds`、`content_indexed`、`content_match_used`、
`filename_match_used` 等 evidence 字段，用于证明检索来自内容索引而不是文件名捷径。

### 4.3 向量数据库 Schema

```sql
-- 向量表
CREATE TABLE vectors (
    id BIGINT PRIMARY KEY,
    namespace VARCHAR(50),  -- "text", "image", "audio", "video"
    embedding FLOAT32[384],  -- 向量
    source_file_id BIGINT,
    chunk_id INT,
    metadata JSON,  -- 时间戳、标签、尺寸等
    created_at TIMESTAMP,
    ttl INTERVAL  -- 自动过期
);

-- 文件索引
CREATE TABLE files (
    id BIGINT PRIMARY KEY,
    name VARCHAR(255),
    path VARCHAR(1024),
    type ENUM('text', 'image', 'audio', 'video', 'document'),
    size_bytes BIGINT,
    hash_sha256 VARCHAR(64),  -- 去重
    created_at TIMESTAMP,
    modified_at TIMESTAMP,
    indexed_at TIMESTAMP
);

-- 任务日志 (可观测性)
CREATE TABLE task_logs (
    id UUID PRIMARY KEY,
    task_type VARCHAR(50),
    status ENUM('pending', 'processing', 'completed', 'failed'),
    complexity_score INT,
    routing_decision VARCHAR(20),  -- LOCAL/HYBRID/CLOUD
    input_hash VARCHAR(64),
    output_summary VARCHAR(255),
    local_latency_ms INT,
    cloud_latency_ms INT,
    tokens_used INT,
    cost DECIMAL(10, 6),
    started_at TIMESTAMP,
    completed_at TIMESTAMP,
    error_message TEXT
);
```

---

## 5. 技术栈推荐

### 5.1 核心依赖

| 组件 | 推荐方案 | 备选方案 |
|-----|---------|---------|
| 本地推理 | Ollama | LocalAI, ONNX Runtime |
| 向量DB | Milvus | Weaviate, FAISS, Pinecone |
| 嵌入模型 | sentence-transformers | BGE, ONNX-optimized |
| 多模态 | CLIP, Whisper | MediaPipe, TorchVision |
| Web框架 | FastAPI | Flask, Django |
| 日志/追踪 | OpenTelemetry | ELK stack |
| 消息队列 | Redis Queue | Celery + RabbitMQ |
| 向量搜索 | Milvus | Elasticsearch |

### 5.2 目录结构建议

```
harbor-local-agent/
├── README.md
├── architecture.md
├── requirements.txt
│
├── src/
│   ├── __init__.py
│   ├── main.py                    # 入口点
│   │
│   ├── core/
│   │   ├── __init__.py
│   │   ├── router.py              # 路由决策引擎
│   │   ├── complexity_assessor.py
│   │   └── privacy_classifier.py
│   │
│   ├── executors/
│   │   ├── __init__.py
│   │   ├── base_executor.py
│   │   ├── local_executor.py
│   │   ├── hybrid_executor.py
│   │   └── cloud_executor.py
│   │
│   ├── rag/
│   │   ├── __init__.py
│   │   ├── multimodal_ingester.py  # 多模态数据摄入
│   │   ├── retriever.py            # 检索
│   │   ├── vector_store.py         # 向量DB 封装
│   │   └── query_parser.py         # 查询理解
│   │
│   ├── security/
│   │   ├── __init__.py
│   │   ├── anonymizer.py           # 数据脱敏
│   │   ├── pii_detector.py         # PII 检测
│   │   └── key_manager.py          # 密钥管理
│   │
│   ├── models/
│   │   ├── __init__.py
│   │   ├── local_models.py         # Ollama 集成
│   │   └── cloud_models.py         # 云 API 集成
│   │
│   ├── monitoring/
│   │   ├── __init__.py
│   │   ├── logger.py
│   │   ├── metrics.py              # Prometheus
│   │   └── tracer.py               # OpenTelemetry
│   │
│   ├── api/
│   │   ├── __init__.py
│   │   ├── router.py
│   │   ├── schemas.py              # Pydantic 模型
│   │   └── handlers.py
│   │
│   └── utils/
│       ├── __init__.py
│       ├── config.py
│       └── helpers.py
│
├── tests/
│   ├── __init__.py
│   ├── unit/
│   ├── integration/
│   └── e2e/
│
├── config/
│   ├── local.yaml                  # 本地开发配置
│   ├── prod.yaml                   # 生产配置
│   └── secrets.example.yaml         # 密钥模板
│
├── docker/
│   ├── Dockerfile
│   ├── docker-compose.yaml
│   └── .env.example
│
├── docs/
│   ├── architecture.md
│   ├── api_reference.md
│   ├── deployment.md
│   └── troubleshooting.md
│
└── scripts/
    ├── setup.sh                    # 初始化脚本
    ├── download_models.sh          # 模型下载
    └── migrate_db.sh               # 数据库迁移
```

---

## 6. 实现阶段规划

### Phase 1: HarborBeacon 边界冻结与 Contract 对齐 (Weeks 1-2)
- Owner: `harbor-framework`
- Collaborators: `harbor-architect`, `harbor-im-gateway`
- [x] 将 HarborBeacon 入站接口对齐到 v1.5 `POST /api/tasks`
- [x] 支持 `source.route_key`、顶层 `message` block、RFC 3339 UTC
- [x] 增加 `X-Contract-Version: 1.5` 与服务鉴权校验
- [x] 统一 non-200 shared error envelope
- [x] 建立 `task_id` 幂等重放与冲突检测

### Phase 2: Business State / Approval / Artifact / Audit 加固 (Weeks 3-4)
- Owner: `harbor-framework`
- Collaborators: `harbor-architect`, `harbor-im-gateway`
- [x] 会话状态中持久化 `route_key`，但保持其为 opaque metadata
- [x] 保持 HarborBeacon 独占业务会话真相与 `resume_token` 流程
- [x] 审批、artifact、audit 补齐 `task_id/trace_id/route_key` 关联
- [x] 处理附件 transport metadata，但不引入平台文件语义

### Phase 3: Outbound Notification Intent Cutover (Weeks 5-7)
- Owner: `harbor-framework`
- Collaborators: `harbor-architect`, `harbor-im-gateway`
- [x] 将通知投递改为 HarborBeacon -> HarborGate `POST /api/notifications/deliveries`
- [x] 对齐 `delivery.mode`、`destination.route_key`、`idempotency_key`
- [x] 增加 delivery success / accepted failure / request rejection 处理逻辑
- [x] 删除 HarborBeacon 直连 Feishu / Telegram 等平台消息发送路径

### Phase 4: 管理面去凭据化与状态解耦 (Weeks 8-9)
- Owner: `harbor-framework`
- Collaborators: `harbor-architect`, `harbor-im-gateway`
- [x] 移除 HarborBeacon 对 `app_id/app_secret/bot token` 的长期职责
- [x] 改为读取 HarborGate redacted status，而非本地校验平台凭据
- [x] 清理 `bridge_provider` 与管理台配置中的平台耦合
- [ ] 为迁移期保留受控兼容开关与回滚方案

### Phase 5: Cutover 联调与发布准备 (Weeks 10-12)
- Owner: `harbor-framework`
- Collaborators: `harbor-architect`, `harbor-im-gateway`, `harbor-hos-control`, `harbor-aiot`
- [ ] HarborBeacon 侧 contract / integration / E2E 测试补齐
- [ ] 联调 inbound / resume / approval / artifact / notification 全链路
- [ ] 增加 cutover 清单、回滚清单、迁移文档
- [ ] 清理旧代码路径并完成发布评审

### Phase 6: Cutover 后恢复长期能力建设 (Weeks 13-14)
- Owner: `harbor-framework`
- Collaborators: `harbor-architect`, `harbor-hos-control`, `harbor-aiot`
- [ ] 将更长期的多模态 RAG / skills / reliability backlog 重新排期
- [ ] 继续推进本地执行器、混合执行器与云协作能力
- [ ] 依据 cutover 结果调整后续架构演化路线

---

## 7. 关键指标与 SLA

### 7.1 性能目标

| 指标 | 目标 | 说明 |
|-----|------|------|
| 本地查询延迟 | < 500ms | P95 |
| 混合执行延迟 | < 3s | P95 |
| 向量检索准确率 | > 85% | NDCG@10 |
| 数据脱敏精度 | > 99% | 无遗漏 PII |
| 系统可用性 | > 99% | excludes 云依赖 |
| 隐私合规率 | 100% | 审计通过 |

### 7.2 可观测性指标

```python
metrics = {
    'routing': {
        'local_ratio': 'Gauge',  # 本地执行占比
        'cloud_ratio': 'Gauge',  # 云执行占比
        'hybrid_ratio': 'Gauge'
    },
    'performance': {
        'local_latency': 'Histogram',
        'cloud_latency': 'Histogram',
        'e2e_latency': 'Histogram'
    },
    'privacy': {
        'pii_detected_count': 'Counter',
        'anonymizations_performed': 'Counter',
        'failed_anonymizations': 'Counter'
    },
    'rag': {
        'retrieval_recall': 'Gauge',
        'retrieval_precision': 'Gauge',
        'avg_documents_retrieved': 'Gauge'
    }
}
```

---

## 8. 风险与缓解策略

| 风险 | 影响 | 缓解策略 |
|------|------|---------|
| 本地模型不够强大 | L3 任务质量低 | 定期微调 + 云补充 |
| 脱敏不完全 | 隐私泄露 | 多层检测 + 人工审核 |
| 网络不稳定 | 云组件失败 | 本地降级 + 队列缓冲 |
| 向量检索性能 | 大规模数据慢 | 分区 + 层级索引 |
| 成本失控 | 云调用过多 | 配额限制 + 智能路由 |

---

## 9. 下一步行动

### 立即行动 (Week 0)
1. **入站 Contract 对齐** - 更新 `assistant_task_api` / `runtime/task_api` 支持 v1.5 字段与错误模型
2. **耦合点盘点** - 列出 HarborBeacon 内所有直连 IM 发送、平台凭据校验、状态共享假设
3. **测试基线建立** - 为 inbound/outbound contract、幂等、resume、approval 建立 HarborBeacon 侧回归
4. **cutover 策略确认** - 定义 feature flag、灰度步骤、回滚开关

### Week 1 优先事项
- 扩展 `TaskRequest`，纳入 `source.route_key` 与顶层 `message` block
- 在会话状态中持久化 `route_key`，打通 `needs_input` / `resume_token`
- 统一 `X-Contract-Version: 1.5`、服务鉴权与 shared error envelope
- 抽象 HarborBeacon 侧 notification client，为切换 HarborGate delivery 做准备

### 风险检查清单
- [ ] HarborBeacon 是否仍在任何路径上保存原始 IM 平台凭据？
- [ ] HarborBeacon 是否仍有直接调用平台消息发送接口的路径？
- [ ] `route_key` 是否在 HarborBeacon 中被错误解释为平台 recipient 语义？
- [ ] 是否仍存在依赖 IM 仓库或共享 `.harborbeacon/*.json` 的假设？

