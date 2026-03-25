# PDF RAG MVP

面向科研场景的本地优先 PDF 文档理解系统。目标不是先做“选题 workflow”，而是先把用户自己的 PDF 文献库变成一个可导入、可索引、可检索、可追溯问答的基础设施。

## 为什么单独拆 repo

现有 `nabla` 的主流程是 `collect -> screen -> propose`，处理对象是外部论文元数据集合。PDF RAG 的核心问题完全不同：

- 输入是用户本地文件，不是在线检索结果
- 首要难点是解析质量、层级结构、引用追踪，不是主题生成
- 需要长生命周期的索引任务、重试机制、增量更新
- 后续会自然长出桌面文件管理、任务中心、文档阅读器等能力

因此先把它拆成独立目录 `pdf-rag-mvp/`，在父仓库分支 `feat-pdf-rag-mvp` 上推进。

## MVP 目标

- 批量导入 PDF 文件或文件夹
- 抽取正文、页码、章节层级
- 切 chunk 并构建层级摘要树
- 支持本地语义检索和带引用问答
- 为后续 GraphRAG 保留扩展点

## MVP 不做

- 第一版不做完整知识图谱推理
- 第一版不做多人协作/云同步
- 第一版不做复杂文献管理功能
- 第一版不做跨库权限系统

## 方法取舍

### MVP: RAPTOR-lite 优先

先做“结构化 chunk + 递归摘要树 + 分层检索”：

- 更适合 PDF 这类天然带章节结构的输入
- 更容易保留页码和引用链
- 出错时更容易定位到具体 chunk / section
- 工程复杂度显著低于一开始就做图谱抽取和图推理

### Phase 2: GraphRAG 作为增强层

当下面三个条件成立时，再引入 GraphRAG：

- 已积累稳定的 chunk / summary / citation 数据
- 查询日志里明确出现多跳推理失败模式
- 能定义一套对科研文献有价值的实体与关系 schema

## 初始目录

```text
pdf-rag-mvp/
├── Cargo.toml
├── README.md
├── docs/
│   └── architecture.md
└── crates/
    ├── cli/
    ├── contracts/
    └── core/
```

## 当前骨架说明

- `contracts`: 文档、导入任务、chunk、摘要树、检索请求/响应的数据结构
- `core`: MVP pipeline、接口边界、功能取舍清单
- `cli`: 输出架构蓝图和查询示例，便于先把后端形状固定下来

## 后续实施顺序

1. 接入 PDF parser，产出页级文本和目录结构
2. 落 SQLite 元数据表和本地索引目录
3. 接 embedding / reranker / answer 适配器
4. 再包一层 Tauri 桌面壳和任务面板

## 本地查看

```bash
cargo run -p nabla-pdf-rag-cli -- blueprint
cargo run -p nabla-pdf-rag-cli -- query-example --library-id lab --prompt "总结这批论文里关于 operator learning 的主要争议"
```
