# 评估方案：生物学领域实测

## 目标

用 3 个生物学 test case 跑 nabla 当前实现，用 LabClaw skills 做独立对照评审，从真实输出中发现失败模式，驱动迭代方向。

## 方法

```
nabla CLI (被测系统)          LabClaw + Claude Code (评审系统)
        │                              │
        ▼                              ▼
  paper_set.json                 对照论文集
  screening.json                 独立假设列表
  topic_brief.json               PRISMA 审计报告
        │                              │
        └──────────── diff ────────────┘
                       │
                  失败模式归类
```

不写新代码，用现有工具直接跑。

## Test Case 设计

### Case 1: CRISPR 递送（热门方向，文献充足）

```json
{
  "id": "bio-eval-01",
  "goal": "CRISPR delivery methods for in vivo gene therapy",
  "keywords": ["CRISPR", "delivery", "in vivo", "gene therapy"],
  "constraints": ["Focus on 2020-2026", "Prefer clinical or preclinical studies"],
  "date_range": { "start": "2020-01-01", "end": null }
}
```

**预期覆盖方向**（由 LabClaw 评审验证）：
- 脂质纳米颗粒（LNP）递送
- AAV 载体优化
- 非病毒递送（核糖核蛋白 RNP）
- 体内编辑器官靶向（肝脏、肌肉、脑）
- 新兴递送：VLP、外泌体、工程化细胞外囊泡

**这个 case 测什么**：检全能力。CRISPR delivery 是热门领域，子方向多且术语分散，考验 keyword 覆盖度。

### Case 2: 肠道微生物组与神经退行性疾病（跨领域，术语鸿沟大）

```json
{
  "id": "bio-eval-02",
  "goal": "gut microbiome influence on neurodegenerative disease progression",
  "keywords": ["gut microbiome", "neurodegeneration", "Alzheimer", "Parkinson", "gut-brain axis"],
  "constraints": ["Include both clinical and mechanistic studies"],
  "date_range": { "start": "2018-01-01", "end": null }
}
```

**预期覆盖方向**：
- 微生物代谢产物（短链脂肪酸、色氨酸代谢物）对神经炎症的影响
- 肠-脑轴免疫信号通路
- 特定菌群与 AD/PD 的关联（如 Akkermansia, Prevotella）
- 粪菌移植（FMT）的治疗潜力
- 饮食干预与微生物组重塑

**这个 case 测什么**：跨领域检索。横跨微生物学、神经科学、免疫学，不同子领域术语差异大，考验 query 是否能跨术语发现论文。

### Case 3: 空间转录组学的计算方法（新兴技术，方法论导向）

```json
{
  "id": "bio-eval-03",
  "goal": "computational methods for spatial transcriptomics data analysis",
  "keywords": ["spatial transcriptomics", "computational methods", "cell type deconvolution", "gene expression"],
  "constraints": ["Focus on methods and benchmarks, not pure applications"],
  "date_range": { "start": "2021-01-01", "end": null }
}
```

**预期覆盖方向**：
- 细胞类型解卷积方法（RCTD, cell2location, Tangram）
- 空间域识别与聚类
- 空间可变基因检测
- 多模态整合（scRNA-seq + spatial）
- 基准测试与评估框架

**这个 case 测什么**：检准能力。关键词比较精确，但"computational methods"容易混入大量应用类论文，考验 Screen 阶段能否区分方法论文和应用论文。

## 评审流程

### Step 1: 跑 nabla

```bash
# 每个 case 独立跑
nabla run --brief examples/bio-eval-01.json
nabla run --brief examples/bio-eval-02.json
nabla run --brief examples/bio-eval-03.json
```

收集每个 case 的产出：
- `.nabla/artifacts/<run_id>/paper_set.json`
- `.nabla/artifacts/<run_id>/screening.json`
- `.nabla/artifacts/<run_id>/topic_brief.json`

### Step 2: LabClaw 对照评审

在独立的 Claude Code 实例中，加载 LabClaw 的 literature 和 bio skills，执行三项评审：

**A. 对照检索（测检全）**

用 LabClaw 的 `literature-review` + `openalex-database` + `pubmed-search` 对同一个 goal+keywords 做独立检索。

评审问题：
- nabla 找到了多少篇？LabClaw 找到了多少篇？
- LabClaw 找到但 nabla 没找到的论文里，有多少是高被引 / 高相关的？
- 哪些子方向被 nabla 完全遗漏？

**B. 假设验证（测 topic 质量）**

用 LabClaw 的 `literature_to_hypothesis`，拿 nabla 的 paper_set 独立生成假设。

评审问题：
- LabClaw 从同一批论文中提取了多少个可检验假设？
- nabla 的 topics 覆盖了其中多少个？
- nabla 提出了哪些 LabClaw 没提取到的方向？（可能是创新也可能是幻觉）

**C. 流程审计（测方法论合理性）**

用 LabClaw 的 `literature-review` PRISMA 方法论审查 nabla 的检索和筛选过程。

评审问题：
- 检索策略是否合理？（库的选择、关键词覆盖）
- 筛选标准是否清晰？（Screen 的 include/exclude 理由是否一致）
- 证据链是否完整？（topic → 论文 → 筛选理由 是否可追溯）

### Step 3: 记录观察

每个 case 填一份观察表：

| 维度 | 指标 | Case 1 | Case 2 | Case 3 |
|------|------|--------|--------|--------|
| 检全 | nabla 论文数 | | | |
| 检全 | LabClaw 对照论文数 | | | |
| 检全 | nabla 遗漏的高相关论文数 | | | |
| 检全 | 完全遗漏的子方向数 | | | |
| 检准 | Screen include 率 | | | |
| 检准 | Screen 误判数（人工抽检 10 篇） | | | |
| Topic | nabla topic 数 | | | |
| Topic | LabClaw 假设覆盖率 | | | |
| Topic | 主观质量 1-5 分 | | | |
| 流程 | 有摘要率 | | | |
| 流程 | 证据链完整性 | | | |
| 主要问题 | 自由文本 | | | |

### Step 4: 归类失败模式

跑完 3 个 case 后，将观察到的问题归类：

| 如果主要失败是… | 迭代优先级 |
|----------------|-----------|
| 子方向整片漏掉 | Collect: query 展开（collect-pipeline.md P0/P1）|
| 找到了但 Screen 判断错 | Screen: prompt 调优 |
| Screen 对了但 Topic 空泛 | Propose: prompt 调优或 top-down 反转 |
| 论文数量足够但质量差 | Collect: PreFilter + BM25 |
| 跨术语论文漏检严重 | Collect: LLM 同义词展开（P1）|
| 摘要缺失率高 | Collect: 补充 Semantic Scholar 源 |

## 时间线

| 步骤 | 预估 |
|------|------|
| 准备 3 个 brief JSON 文件 | 10 分钟 |
| nabla CLI 跑 3 个 case | ~5 分钟 |
| LabClaw 对照评审 3 个 case | ~30 分钟 |
| 填写观察表 + 归类失败模式 | 15 分钟 |
| 决定第一轮迭代方向 | 讨论决定 |

## LabClaw Skills 加载方式

在独立 Claude Code 实例的工作目录下，创建 `.claude/CLAUDE.md`，引用需要的 LabClaw skills：

```markdown
# 评审专用 Claude Code 配置

你是 nabla 选题系统的评审 agent。你的任务是用生物医学领域知识评审 nabla 的输出。

加载以下 LabClaw skills 作为领域知识（位于 /Users/lyy/github/LabClaw/skills/）：
- literature/literature-review/SKILL.md
- literature/literature_to_hypothesis/SKILL.md
- literature/openalex-database/SKILL.md
- literature/pubmed-search/SKILL.md
- literature/academic-literature-search/SKILL.md
```

或者直接在 prompt 中引用 skill 文件内容。
