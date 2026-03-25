# Architecture

## 1. 产品目标

这个系统服务于"研究者已经有一批 PDF 文献，但读不完、找不到、串不起来"的场景。

MVP 要解决的不是"生成最终结论"，而是先把本地 PDF 库变成一个可计算的知识空间：

- 能稳定导入
- 能稳定解析
- 能形成层级结构
- 能检索回原文
- 能回答并给出处

## 2. 功能边界

### MVP 功能

1. 批量导入
2. 解析进度与失败重试
3. 文档预览和页码引用
4. chunk 检索
5. 摘要树检索
6. 问答时返回来源页码和原文片段

### 非 MVP 功能

1. 自动生成完整知识图谱可视化
2. 跨文档 citation network 推理
3. 在线协作和远程同步
4. 复杂标签体系和团队权限

## 3. 为什么不是一开始就 GraphRAG

LazyGraphRAG 很适合"文本中抽取实体/关系，再沿图做多跳推理"的问题。但 PDF first 场景里，第一性问题通常不是推理能力，而是：

- PDF 解析质量是否稳定
- 图谱抽取是否会丢掉精确页码和上下文
- 实体/关系 schema 是否足够稳
- 图构建成本是否和首版收益匹配

对科研文献来说，MVP 的高价值路径通常是：

1. 保留文档结构
2. 先做 chunk 和 section summary
3. 再做 cluster summary
4. 查询时执行"summary 召回 -> source chunk 下钻 -> 答案生成"

这条路线本质上是 RAPTOR 的工程化变体，能先解决 80% 的问题。

## 4. 技术栈选型

### 4.1 存储层：SQLite + LanceDB

| 组件 | 职责 | 选型理由 |
|------|------|---------|
| **SQLite** | System of record：文库、任务、文档状态、chunk 元数据 | 成熟、嵌入式、零运维 |
| **LanceDB** | Retrieval index：向量索引 + FTS/BM25 + hybrid search + RRF | 嵌入式、本地文件型、官方 Rust SDK、一个系统覆盖向量+词法+融合 |

为什么不选 Qdrant：桌面端捆绑 gRPC server 是反模式；Qdrant 的 Rust "local mode" 实际只在 Python client 中支持。

升级路径：如果 CJK 词法检索质量不够，引入 Tantivy 做独立 BM25 层；如果要服务化部署，切换到 Qdrant。

注意事项：LanceDB OSS 新增数据后需要 incremental reindex / `optimize()`，否则新数据走 brute-force 扫描。

### 4.2 PDF 解析层

| 组件 | 职责 | 集成方式 |
|------|------|---------|
| **Docling** | 主 parser：layout understanding、table structure、reading order | Tauri sidecar（Python → PyInstaller standalone） |
| **OCRmyPDF** | OCR fallback：给扫描件加 text layer | sidecar |
| **pdfium-render** | 桌面阅读器：页面渲染、文本/图片提取 | Rust binding（需绑定 Pdfium，注意线程安全） |

Docling 是 Python 生态，通过 sidecar 进程与 Rust pipeline 解耦。Tauri 原生支持 sidecar bundling。

### 4.3 Embedding 模型

| 方案 | 模型 | 运行方式 | 适用场景 |
|------|------|---------|---------|
| **默认（本地）** | `bge-small-zh-v1.5` 或 `bge-m3` | ONNX Runtime (`ort` crate) | 离线可用、中英混合、零 API 费用 |
| **可选（远程）** | `text-embedding-3-small` | HTTP API | 用户 opt-in，质量更高 |

### 4.4 LLM 层

统一通过 `LlmClient` trait 抽象，供 summarize 和 answer 阶段共用。支持：

- `complete(prompt, max_tokens) -> String`
- `complete_json(prompt, max_tokens) -> serde_json::Value`
- `max_context_tokens() -> u32`（用于 token budget 管理）

### 4.5 桌面壳

Tauri：四个主视图（Library / Tasks / Documents / Ask）。sidecar 管理 Docling 和 OCRmyPDF 进程。

## 5. 推荐技术路线

### 5.1 Ingestion

- 输入：单文件、多文件、文件夹拖拽
- 去重：`sha256 + file_size + modified_at`
- 元数据：原路径、导入时间、解析状态、失败原因

### 5.2 Parsing

- 首选：Docling（数字 PDF 文本抽取 + layout understanding）
- 回退：OCRmyPDF（扫描版 OCR）
- 产出：
  - 页级文本
  - 目录/标题层级
  - 文档元信息（标题、作者、年份）

### 5.3 Structuring

- 先按标题层级切 section
- section 内再按 token / 语义边界切 chunk
- 每个 chunk 保留：
  - `document_id`
  - `heading_path`
  - `page_span`
  - `ordinal`
  - `text`

### 5.4 Hierarchical Summaries

构建三层摘要节点即可：

1. `section summary`
2. `cluster summary`
3. `document summary`

这已经足够支持：

- 从高层主题快速召回
- 顺着摘要节点下钻到原始 chunk
- 在单文档和跨文档之间做"主题 -> 证据"跳转

### 5.5 Retrieval

查询链路分为五个阶段，由 `Retriever` trait 编排：

```
Query
  │
  ├── embed query ──→ VectorIndex::query_nearest()   ─┐
  │                                                     │
  ├── normalize ────→ LexicalIndex::query_bm25()     ─┤── FusionStrategy::fuse()
  │                                                     │      │
  └── summary tree traversal ─────────────────────────┘      │
                                                               ▼
                                                     Reranker::rerank() (optional)
                                                               │
                                                               ▼
                                                     expand summary → source chunks
                                                               │
                                                               ▼
                                                     Vec<RetrievalHit>
```

每个 `RetrievalHit` 标记了 `sources: Vec<RecallSource>`，可追溯命中来自 Vector / Lexical / SummaryTree 哪条通道。

默认 fusion 策略：Reciprocal Rank Fusion (RRF)，与 Azure AI Search、LanceDB 官方推荐一致。

### 5.6 Answering

答案至少返回：

- 结论
- 引用文档名
- 页码区间
- 简短原文摘录
- 如果证据冲突，显式说明冲突

## 6. 数据模型

### 核心实体

- `LibraryRecord`: 一个本地知识库
- `ImportBatchRecord`: 一次导入任务
- `DocumentRecord`: 一份 PDF
- `SummaryNode`: 层级摘要树节点
- `ChunkRecord`: 原始检索单元
- `RetrievalQuery`: 查询输入
- `RetrievalHit`: 检索命中（含 RecallSource 溯源）
- `AnswerDraft`: 带引用的回答

### 强类型 ID

所有 ID 使用 newtype 包装，防止混用：

- `LibraryId`, `BatchId`, `DocumentId`, `ChunkId`, `SummaryNodeId`

## 7. 模块划分

### `contracts`

只放稳定领域模型和强类型 ID，不掺杂具体 parser / model / db 实现。

### `core`

放 pipeline 抽象和关键 trait：

- `LlmClient`: LLM 统一接口
- `ProgressSink`: 进度回调
- `DocumentRepository`: 文档仓储
- `DocumentParser`: PDF 解析（接受 ProgressSink）
- `HierarchyBuilder`: 层级摘要树构建（接受 LlmClient + ProgressSink）
- `Embedder`: 向量化（返回 EmbeddingBatchResult，不再是黑盒 `Result<()>`）
- `VectorIndex`: 向量召回
- `LexicalIndex`: 词法/BM25 召回
- `FusionStrategy`: 多通道融合（默认 RRF）
- `Reranker`: 可选精排
- `Retriever`: 检索编排
- `AnswerEngine`: 答案生成

### 后续新增模块

- `storage`: SQLite 表、LanceDB 索引管理、任务状态
- `ingest`: 文件扫描、去重、队列
- `parser`: Docling sidecar 通信、OCRmyPDF fallback
- `retrieval`: VectorIndex / LexicalIndex / FusionStrategy 的 LanceDB 实现
- `apps/desktop`: Tauri 命令、状态同步、sidecar 生命周期管理

## 8. GraphRAG 放在哪一层

GraphRAG 不应该替代 chunk / summary tree，而应该作为 sidecar index：

- 输入来自已经清洗过的 chunk
- 输出为实体、关系、claim graph
- 查询时只有在检测到 multi-hop query 时才参与召回

这样可以避免：

- 图谱抽取错误直接污染主链路
- 普通查询也被迫走高成本图推理
- 难以解释图结论从哪一页来

## 9. 里程碑

### M0

- 文档和接口骨架
- 强类型 ID、LlmClient trait、Retrieval trait 层

### M1

- 单机导入 PDF
- Docling sidecar 文本抽取
- chunk 与摘要树生成
- CLI 查询

### M2

- SQLite + LanceDB 集成
- hybrid search (vector + BM25 + RRF)
- 引用问答
- 失败重试与增量更新

### M3

- Tauri 桌面壳
- 阅读器（pdfium-render）和任务中心

### M4

- GraphRAG sidecar
- 多跳查询路由
- 可选 reranker
