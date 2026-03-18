# 选题 Agent

## 问题

科研选题的困难有：
* 信息过载，太多论文读不完；
* 方向模糊，不知道哪些子方向更有潜在的价值；
* 候选方向散落在笔记里，缺乏结构化比较。

选题 Agent 协助研究者把选题过程**高效化、标准化、可追溯**。

## 用户与边界

**目标用户**：尚未确定选题的科研人员，主要是有一定文献判断力的硕博生和转方向研究者。

**做什么**：多源自动检索 + AI 筛选标注 + 候选方向结构化对比 + 证据链支撑决策。

**不做什么**：不做完整文献综述、不做全文解读、不做实验设计、不替代导师判断。系统到"确定选题方向"为止。

## 核心 Workflow

```
  ProjectBrief (一句话兴趣 + 关键词)
                 │
      ┌──── Frame ────┐
      │  解析意图       │
      │  生成检索策略    │
      └──────┬────────┘
             ▼
      ┌──── Collect ───┐
      │  OpenAlex 检索  │
      │  arXiv 检索     │
      │  去重 & 合并    │
      └──────┬─────────┘
             ▼
       [用户扩展检索]    ← ① 追加关键词触发增量检索
             ▼
      ┌──── Screen ────┐
      │  LLM 逐篇筛选   │
      │  Include/Maybe  │
      │  /Exclude       │
      │  + 理由 + 置信度 │
      └──────┬─────────┘
             ▼
       [用户修正筛选]    ← ② 修改 label，注入领域知识
             ▼
      ┌──── Propose ───┐
      │  聚类 & 提炼     │
      │  生成 2-3 候选   │
      │  附证据链 & 风险  │
      └──────┬─────────┘
             ▼
       [用户决策]        ← ③ 接受 / 拒绝，重新探索

  TopicBrief (候选方向 + scope + why-now + delta + 风险 + 退路)
```

### 回流路径

| 用户动作 | 回流到 | 说明 |
|---------|-------|------|
| 追加关键词 | Collect | 增量检索，新建 run |
| 修改筛选 label 后重新生成 | Propose | 已实现：`rerun_propose` |
| 调整 goal/constraints | Frame | **重新检索**，旧检索完整保留 |

### 异常路径

| 情况 | 系统行为 |
|------|---------|
| 检索结果过少（< 5 篇） | 提示用户扩展关键词 |
| 筛选后 Include 为 0（手动模式） | 阻止进入 Propose，跳转 Screening 页面提示放宽筛选 |
| 筛选后 Include 为 0（自动模式） | 阻止进入 Propose，跳转 Screening 页面，要求用户手动修正后重新生成 |
| 数据源冲突（同一论文元数据不一致） | 去重时 OpenAlex 优先，保留两个来源的 URL |

## 输入与输出

**输入**：门槛要低 — 一句话 goal + 几个关键词。

```json
{
  "goal": "neural operator methods for PDE discovery",
  "keywords": ["neural", "operator", "PDE", "discovery"]
}
```

**输出**：每个候选方向回答五个问题 — 具体做什么、为什么值得现在做、与已有工作的差异、风险多大、退路是什么，并附可追溯的论文。

```json
{
  "title": "Causal Operator Discovery with Neural Operator Architectures",
  "scope": "Apply neural operator architectures to discover causal structure in PDE systems",
  "why_now": "Neural operators are maturing but not yet combined with causal discovery...",
  "prior_closest_work": "FNO (Li et al., 2021) applied neural operators to PDE forward solving",
  "delta": "This direction targets causal structure discovery, not forward solving",
  "representative_paper_ids": ["arxiv:2506.20181v1"],
  "entry_risk": "Medium",
  "fallback_scope": "Apply causal discovery with standard neural networks"
}
```

## 数据源与可信度

| 级别 | 来源 | 覆盖领域 | 处理方式 |
|------|------|---------|---------|
| L1 | OpenAlex | 全领域，240M+ 论文 | 结构化元数据完整，直接使用 |
| L1 | PubMed | 生物医学，36M+ 论文 | E-utilities API，完整摘要 + MeSH 语义标签 |
| L2 | arXiv | CS/Physics/Math 预印本 | 标注来源 |

### 数据源选型依据（2026-03 生物学实测）

针对 CRISPR delivery / gut-brain axis / spatial transcriptomics 三个生物学 case 实测：

| 数据源 | CRISPR case 命中 | 摘要覆盖率 | 关键优势 |
|--------|-----------------|-----------|---------|
| arXiv | **0 篇** | — | 生物学 q-bio 类目覆盖极小 |
| OpenAlex | 25 篇 | 52% | 广覆盖但摘要为倒排索引重建，不完整 |
| **PubMed** | **592 篇** | **完整** | MeSH 语义标签自动解决跨术语问题 |
| Semantic Scholar | 2117 篇（bulk） | 100% | 跨领域广，引用图 API 强，但相关性排序弱 |

**选择 PubMed 作为生物医学主源的理由**：
1. MeSH 术语体系天然解决跨术语检索（`gut microbiome` 自动匹配 `gastrointestinal microbiome` / `intestinal flora`），减少对 LLM query 展开的依赖
2. 完整摘要保障下游 Screen 阶段判断质量
3. PMC 子集已包含 bioRxiv/medRxiv 预印本

Semantic Scholar 作为后续引用链扩展的备选源（详见 [collect-pipeline.md](./collect-pipeline.md) P2）。

**证据链**：选题建议 → scope 定义 → delta 差异声明 → 代表性论文 → 筛选理由 → 置信度。从结论到原始论文可追溯。

> 可观测性：当前证据链能解释"结论依据哪些论文"，但不能解释"为什么选这几篇"以及"topic 如何从论文集聚类形成"。聚类过程发生在 LLM 的 Propose 调用内部不可见。后续版本可通过让 LLM 输出聚类理由来改善。

**去重**：DOI > PubMed ID > arXiv ID > OpenAlex ID > 标题哈希。冲突时取摘要最完整的记录。

## 评估方案

详见 [evaluation.md](./evaluation.md)。