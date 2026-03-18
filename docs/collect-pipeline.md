# Collect Pipeline 重构

## 动机

生物学领域实测（2026-03，3 个 case）暴露了当前 Collect 的硬伤：

| 问题 | 数据 | 根因 |
|------|------|------|
| arXiv 零贡献 | 3 个 bio case 均 0 篇 | 生物医学论文不在 arXiv |
| 摘要缺失 | Case 3 仅 32% 有摘要 | OpenAlex 倒排索引重建不完整 |
| 排除率高 | Case 1 排除 68% | 单 query 检索精度低 |
| 实质单源 | 100% 来自 OpenAlex | arXiv 对非 CS 领域无效 |

## 产品约束

入口保持低门槛（一句话 goal + 关键词）。`seed_papers` 为 optional side input，不进入主路径。

## 整体流程

```
ProjectBrief (goal, keywords, date_range)
    │
    ▼ ① QueryPlan: 确定性展开（P0）+ 可选 LLM 展开（P1）
    │
    ▼ ② Fetch: query groups × sources 并发检索
    │
    ▼ ③ Dedup: 跨源 ID 规范化合并
    │
    ▼ ④ PreFilter: 软降权 + BM25 排序 → top-K
    │
    ▼ ⑤ Report: CollectReport 写入 artifact
    │
    ▼ → Screen
```

## 各步骤设计

### ① QueryPlan — 查询规划

**确定性层（P0）**：从 keywords 组合生成多组查询，不依赖 LLM。

- 每个 keyword 单独作为一个 query group
- 两两组合作为附加 query group
- goal 全文作为一个 query group

```rust
pub struct QueryGroup {
    pub id: String,            // "kw:CRISPR", "combo:CRISPR+delivery", "goal"
    pub query_text: String,
    pub origin: QueryOrigin,   // Keyword | Combination | Goal | LlmExpanded
}

pub enum QueryOrigin { Keyword, Combination, Goal, LlmExpanded }

pub struct CollectPlan {
    pub query_groups: Vec<QueryGroup>,
}
```

**LLM 展开层（P1，可选）**：LLM 生成 3-5 个补充 query group，覆盖同义术语和相邻子方向。编排逻辑在 `workflow` 层，不在 `sources` 层。

### ② Fetch — 多源检索

**数据源**（按实测数据驱动的优先级）：

| 数据源 | 角色 | API | 实测结果 |
|--------|------|-----|---------|
| **PubMed** | 生物医学主源 | E-utilities (esearch → efetch) | 592 篇，完整摘要 + MeSH |
| **OpenAlex** | 全领域通用源 | REST API | 25 篇，52% 摘要 |
| **arXiv** | CS/Physics/Math 源 | Atom API | bio case 0 篇，保留给 CS 领域 |

**PubMed 两步 API**：
1. `esearch` — 关键词搜索，返回 PMID 列表
2. `efetch` — 批量获取元数据（标题、作者、年份、完整摘要、MeSH 术语）

**OpenAlex 改进**：利用已有但未用的 filter 参数：

```
当前: /works?search={query}&per-page={limit}
改进: /works?search={query}&filter=publication_year:>{year}&sort=relevance_score:desc&per-page={limit}
```

**PaperSource trait 不变**。每个 source 内部处理多 query group，对外接口仍然是 `fn fetch(&self, brief: &ProjectBrief) -> Result<Vec<PaperRecord>>`。P0 阶段由每个 source 内部遍历 keywords 发多次请求；P1 阶段 trait 扩展为接受 `CollectPlan`。

**容错**：单源失败不阻断流程。在 CollectReport 中标记失败源。

### ③ Dedup — 跨源去重

修复现有 TODO。核心问题：同一篇论文从 PubMed 拿到 PMID，从 OpenAlex 拿到 DOI，当前无法识别为同一篇。

**策略**：PubMed efetch 返回的 XML 中包含 DOI 字段。优先用 DOI 作为去重 key：

```
优先级：DOI > PubMed ID > arXiv ID > OpenAlex ID > DerivedHash
```

合并时取最完整的摘要和 URL，保留所有来源名。

### ④ PreFilter — 预筛

**不做硬删除，全部走软降权 + BM25 排序截断。**

| 条件 | 降权系数 | 理由 |
|------|---------|------|
| 无摘要 | ×0.3 | Screen 可用信息少，但不排除关键奠基论文 |
| 年份超出 `date_range` | ×0.5 | 可能是重要前置工作 |
| 标题与所有 query 零词重叠 | ×0.4 | 可能是跨术语论文 |

BM25 以 `goal + keywords` 为查询，对 `title + abstract` 打分（无摘要用 title），乘以降权系数，取 top-K（默认 60）。

### ⑤ Report — 检索质量报告

```rust
pub struct CollectReport {
    pub plan: CollectPlan,
    pub source_stats: HashMap<String, SourceStat>,
    pub total_fetched: usize,
    pub total_after_dedup: usize,
    pub abstract_fill_rate: f64,
    pub passed_to_screen: usize,
}

pub struct SourceStat {
    pub queries_sent: usize,
    pub queries_succeeded: usize,
    pub papers_returned: usize,
}
```

写入 `collect_report.json`。

## 门控规则

| 条件 | 行为 |
|------|------|
| `passed_to_screen < 5` | 提示用户扩展关键词 |
| `abstract_fill_rate < 0.5` | 警告：Screen 质量可能受影响 |
| 某数据源全部失败 | 警告：标记降级源 |

软警告，不阻断流程。

## 工程结构

```
crates/contracts/src/lib.rs   → PaperId 新增 PubMed variant
crates/sources/
├── lib.rs                    → traits + CompositeCollector + dedup（现有，修改）
├── openalex.rs               → OpenAlexSource（从 lib.rs 拆出，加 filter）
├── arxiv.rs                  → ArxivSource（从 lib.rs 拆出）
└── pubmed.rs                 → PubMedSource（新增）
crates/cli/src/main.rs        → 注册 PubMedSource
crates/api/src/main.rs        → 注册 PubMedSource
```

**边界原则**：`sources` crate 保持纯检索层，只依赖 `contracts` + 网络/解析库。不依赖 `adapters`。后续 LLM query 展开在 `workflow` 层。

## 实施优先级

| 阶段 | 内容 | 预期效果 |
|------|------|---------|
| **P0** | PubMedSource + OpenAlex filter + 跨源 DOI 去重 | bio 摘要率 52%→90%+，论文量 25→50+，来源多元化 |
| P1 | 确定性多 query（keyword 单独 + 两两组合）+ CollectReport | 检全改善，可观测性 |
| P2 | LLM query 展开 + 引用链 Round 2 + Semantic Scholar | 覆盖相邻子方向 |
| P3 | BM25 预筛 + 软降权 | 控制 Screen 阶段 LLM 成本 |

## P0 实施详细步骤

### Step 1: contracts — PaperId 新增 PubMed variant

```rust
pub enum PaperId {
    Doi(String),
    Arxiv(String),
    OpenAlex(String),
    PubMed(String),       // 新增：PMID
    DerivedHash(String),
}
```

`as_key()` 返回 `"pubmed:{pmid}"`。Storage 层无需改（paper_id_json 已为 JSON 序列化）。

### Step 2: sources — 拆分文件

将 `lib.rs` 中的 `OpenAlexSource` 拆到 `openalex.rs`，`ArxivSource` 拆到 `arxiv.rs`。`lib.rs` 保留 traits、`CompositeCollector`、`dedup_papers`、`StaticCollector` 和辅助函数。

### Step 3: sources — OpenAlex 加 filter

在 `openalex.rs` 中使用 `date_range` 和 `sort`：

```rust
fn fetch(&self, brief: &ProjectBrief) -> Result<Vec<PaperRecord>> {
    let query = brief.keywords.join(" ");
    let mut url = format!(
        "https://api.openalex.org/works?search={}&sort=relevance_score:desc&per-page={}",
        urlencoding::encode(&query), self.limit
    );
    if let Some(ref dr) = brief.date_range {
        if let Some(ref start) = dr.start {
            if let Some(year) = start.get(..4) {
                url.push_str(&format!("&filter=publication_year:>{}", year.parse::<u16>().unwrap_or(2000) - 1));
            }
        }
    }
    // ... rest unchanged
}
```

### Step 4: sources — 新增 PubMedSource

`pubmed.rs`，两步 API：

```rust
pub struct PubMedSource {
    client: Client,
    limit: usize,
}

impl PaperSource for PubMedSource {
    fn fetch(&self, brief: &ProjectBrief) -> Result<Vec<PaperRecord>> {
        // Step 1: esearch — keyword → PMID list
        let query = brief.keywords.join(" ");
        let search_url = format!(
            "https://eutils.ncbi.nlm.nih.gov/entrez/eutils/esearch.fcgi?db=pubmed&term={}&retmax={}&retmode=json{}",
            urlencoding::encode(&query), self.limit, date_filter(brief)
        );
        let pmids: Vec<String> = parse_esearch_response(&self.client.get(&search_url).send()?.text()?)?;

        if pmids.is_empty() { return Ok(vec![]); }

        // Step 2: efetch — PMID list → full metadata (XML)
        let ids = pmids.join(",");
        let fetch_url = format!(
            "https://eutils.ncbi.nlm.nih.gov/entrez/eutils/efetch.fcgi?db=pubmed&id={}&retmode=xml",
            ids
        );
        let xml = self.client.get(&fetch_url).send()?.text()?;
        parse_pubmed_xml(&xml)
    }
}
```

XML 解析提取：ArticleTitle, AbstractText, AuthorList, DOI (从 ArticleIdList), PMID, PubDate.Year。
- 有 DOI → `PaperId::Doi(doi)`
- 无 DOI → `PaperId::PubMed(pmid)`

### Step 5: sources — 跨源 DOI 去重

修改 `dedup_papers()`：当两篇论文来自不同源但 DOI 相同时合并。需要在合并前对所有论文建 DOI 索引：

```rust
fn dedup_papers(papers: Vec<PaperRecord>) -> Vec<PaperRecord> {
    let mut by_key: BTreeMap<String, PaperRecord> = BTreeMap::new();
    for paper in papers {
        let key = paper.paper_id.as_key();
        match by_key.entry(key) {
            Entry::Vacant(e) => { e.insert(paper); }
            Entry::Occupied(mut e) => { merge_into(e.get_mut(), &paper); }
        }
    }
    by_key.into_values().collect()
}

fn merge_into(existing: &mut PaperRecord, other: &PaperRecord) {
    if existing.abstract_text.is_none() {
        existing.abstract_text = other.abstract_text.clone();
    }
    if existing.source_url.is_none() {
        existing.source_url = other.source_url.clone();
    }
    if !existing.source_name.contains(&other.source_name) {
        existing.source_name = format!("{},{}", existing.source_name, other.source_name);
    }
}
```

PubMed 返回的论文优先用 DOI 作为 PaperId，这样与 OpenAlex 的 DOI 自然去重。

### Step 6: CLI + API — 注册 PubMedSource

`cli/src/main.rs`：

```rust
let collector = Box::new(CompositeCollector::new(vec![
    Box::new(PubMedSource::new(args.pubmed_limit)),   // 新增
    Box::new(OpenAlexSource::new(args.openalex_limit)),
    Box::new(ArxivSource::new(args.arxiv_limit)),
]));
```

新增 CLI 参数 `--pubmed-limit`，默认 25。API server 同理。

### Step 7: 测试

- 单元测试：PubMed XML 解析、跨源 DOI 去重
- 集成测试：用 3 个 bio case 重新跑，对比改进前后的论文数、摘要率、Screen Include 率

### 验收标准

| 指标 | 改进前 | 目标 |
|------|--------|------|
| bio case 论文来源 | 100% OpenAlex | 至少 2 个活跃源 |
| bio case 摘要率 | 32-64% | >85% |
| bio case Screen Include 率 | 20-52% | >40% |
