# 评估方案

## 核心思路

用可追溯的证据（结构完整性检查 + delta 差异声明），通过三个维度独立的评估指标和独立的用户标注，由用户分别验证每条证据是否成立，再用各维度自身的标注数据校验该维度指标的有效性。

## 维度定义

| 维度 | 评估主体 | 方法 | 用户标注 |
|------|---------|------|---------|
| 结构完整性 | AI 自动 | 字段合规检查 | N/A |
| 新颖性 | AI 生成 + 人工验证 | delta 描述 | `DeltaVerdict` |
| 价值性 | 人工 | 👍/👎 | `TopicVote` |

三个维度，三套独立的数据采集链路，互不混用。

## 结构完整性

Schema-level check，不是语义层面的理解性评估。通过 = 结构完整。

| 检查规则 | Issue 类型 |
|---------|-----------|
| `scope` 非空且 ≤ 200 字 | `ScopeEmpty` / `ScopeTooLong` |
| `why_now` 引用了至少 1 篇 representative paper | `MissingTemporalAnchor` |
| `fallback_scope` 与 `scope` 文本不同 | `FallbackIdenticalToScope` |

```rust
pub enum CompletenessIssue {
    ScopeEmpty,
    ScopeTooLong { len: usize },
    MissingTemporalAnchor,
    FallbackIdenticalToScope,
}

pub fn check_completeness(topic: &TopicCandidate) -> Vec<CompletenessIssue> { ... }
```

## 新颖性

Propose 阶段让 LLM 生成两个字段嵌入 `TopicCandidate`：

- `prior_closest_work`：最接近的已有工作
- `delta`：与 prior 的关键差异，一句话

用户在 TopicsPage 对 delta 做三选一标注（Confirmed / Rejected / Uncertain），独立于价值性投票。

```rust
pub enum DeltaVerdict { Confirmed, Rejected, Uncertain }

pub struct DeltaReview {
    pub project_id: String,
    pub topic_id: String,
    pub verdict: DeltaVerdict,
    pub comment: Option<String>,
    pub created_at: String,
}
```

## 价值性

👍/👎 只回答"值不值得做"，承载用户综合主观判断，**不用于反向校验其他维度**。

```rust
pub enum UserVote { Up, Down }

pub struct TopicVote {
    pub project_id: String,
    pub topic_id: String,
    pub vote: UserVote,
    pub created_at: String,
}
```

## Meta-evaluation

每个维度用自己的标注数据校验自己的指标。

| 维度 | 数据源 | 校验方法 |
|------|--------|---------|
| 结构完整性 | `CompletenessIssue` | 人工抽检 FP/FN，调整规则松紧 |
| 新颖性 | `DeltaReview` | 跟踪 `Confirmed` 率，低则调整 prompt |
| 价值性 | `TopicVote` | 跟踪 👍 率趋势，作为系统整体质量代理指标 |

## 工程结构

```
crates/eval/
├── completeness.rs  → check_completeness() 纯函数
├── delta_review.rs  → DeltaReview 存储与查询
├── vote.rs          → TopicVote 存储与查询
└── lib.rs
```

`eval` 只依赖 `contracts`，不依赖 `adapters`。

## Contracts 扩展

```rust
pub struct TopicCandidate {
    // 现有字段不变
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub why_now: String,
    pub scope: String,
    pub representative_paper_ids: Vec<PaperId>,
    pub entry_risk: String,
    pub fallback_scope: String,
    // 新增：新颖性证据（Propose 阶段由 LLM 生成）
    pub prior_closest_work: Option<String>,
    pub delta: Option<String>,
}
```