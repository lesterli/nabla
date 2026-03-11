# Topic Agent MVP

## 1. Minimal Closed Loop

The MVP should expose exactly four user-facing functions:

1. `Project Brief`
   - research goal
   - constraints
   - seed keywords
   - time range

2. `Paper Set`
   - retrieval results
   - imported links
   - deduplicated paper records

3. `Screening`
   - `include / maybe / exclude`
   - short rationale
   - 1 to 2 tags

4. `Topic Brief`
   - 2 to 3 candidate directions
   - why each direction is worth attention
   - representative papers
   - main risk or barrier

The MVP loop is:

`define topic -> collect papers -> screen papers -> generate topic brief`

Anything else is out of scope for the first version.

## 2. Frontend Stack

The product should be Web-first, but the frontend must be portable to a desktop
shell later.

Recommended stack:

- React
- TypeScript
- Vite
- React Router
- TanStack Query
- Zustand
- Tailwind CSS

Desktop-ready target:

- Tauri 2

This keeps initial iteration fast while preserving a clean migration path to a
desktop app.

## 3. Agent And Data Stack

### LLM Runtime

MVP default:

- use local agent CLI adapters first
- keep direct API-provider integration as a later adapter

Initial adapters:

- `codex`
- `claude-code`

Rationale:

- this matches the current working environment,
- avoids blocking on paid API setup,
- and lets the MVP reuse the existing CLI and skill ecosystem.

### Paper Sources

MVP source set:

- `OpenAlex`
- `arXiv`

Reason:

- both are broad enough for the first version,
- both are common in research workflows,
- and this is enough to validate the workflow before adding more sources.

### Workflow Style

The MVP should use a fixed multi-step pipeline:

1. `frame`
2. `collect`
3. `screen`
4. `propose`

It should not use a free-form autonomous agent loop in the first version.

### Integration Boundary

By milestone:

- `M1`: CLI entry
- `M2`: local service plus Web UI
- `M3`: Tauri IPC packaging

This keeps the implementation order simple without changing the long-term
product shape.

## 4. Core Contracts

The `contracts` crate should define the minimum shared schemas used by the UI,
workflow, sources, and storage layers.

### `ProjectBrief`

```rust
pub struct ProjectBrief {
    pub id: String,
    pub goal: String,
    pub constraints: Vec<String>,
    pub keywords: Vec<String>,
    pub date_range: Option<DateRange>,
}
```

### `PaperId`

```rust
pub enum PaperId {
    Doi(String),
    Arxiv(String),
    OpenAlex(String),
    DerivedHash(String),
}
```

Rules:

- prefer external stable IDs when available,
- fall back to a normalized derived hash when none exists,
- deduplication should normalize to a canonical ID with priority
  `DOI > arXiv > OpenAlex > DerivedHash`.

A `PaperRecord` holds one canonical `paper_id`. Alternate IDs from other
sources may be stored separately but are not used as primary keys.

### `PaperRecord`

```rust
pub struct PaperRecord {
    pub paper_id: PaperId,
    pub title: String,
    pub authors: Vec<String>,
    pub year: Option<u16>,
    pub abstract_text: Option<String>,
    pub source_url: Option<String>,
    pub source_name: String,
}
```

### `ScreeningLabel`

```rust
pub enum ScreeningLabel {
    Include,
    Maybe,
    Exclude,
}
```

### `ScreeningDecision`

```rust
pub struct ScreeningDecision {
    pub project_id: String,
    pub paper_id: PaperId,
    pub label: ScreeningLabel,
    pub rationale: String,
    pub tags: Vec<String>,
    pub confidence: Option<f32>,
}
```

Rule:

- `paper_id` is the explicit foreign-key-style link back to a `PaperRecord`.

### `TopicCandidate`

```rust
pub struct TopicCandidate {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub why_now: String,
    pub scope: String,
    pub representative_paper_ids: Vec<PaperId>,
    pub entry_risk: String,
    /// A concrete way to narrow or simplify this direction if resources are limited.
    pub fallback_scope: String,
}
```

Rule:

- representative papers should be stored as references by `paper_id`, not by
  copying full paper records into the topic object.

### `Phase` and `RunStatus`

```rust
pub enum Phase {
    Frame,
    Collect,
    Screen,
    Propose,
}

pub enum RunStatus {
    Pending,
    Running,
    Done,
    Failed,
}
```

### `RunManifest`

```rust
pub struct RunManifest {
    pub run_id: String,
    pub project_id: String,
    pub phase: Phase,
    pub created_at: String,
    pub status: RunStatus,
}
```

## 5. Storage

MVP storage strategy:

- `SQLite` for projects, papers, screening decisions, topic candidates, and run
  metadata
- filesystem for exported artifacts, raw payloads, and logs

Why this shape:

- SQLite is simple and enough for MVP persistence,
- easy to inspect and debug,
- and naturally compatible with a future Tauri desktop app.

## 6. Suggested Repository Structure

After cleanup, the repository should be shaped like this:

```text
nabla/
  apps/
    web/              # React + Vite workspace UI
  crates/
    workflow/         # topic-selection workflow
    sources/          # retrieval, normalization, dedup
    storage/          # SQLite + artifact storage
    contracts/        # shared schemas and DTOs
    adapters/         # LLM interaction layer (codex, claude-code)
  docs/
    TOPIC_AGENT_MVP.md
```

The `adapters` crate owns all LLM interaction. The `workflow` crate calls
adapters through a trait and never invokes CLI tools directly. This ensures
the workflow logic stays independent of any specific LLM provider.

This structure is intentionally narrow. It should map directly to the MVP
workflow before the product grows into a broader system.

## 7. Implementation Order

The implementation order should be:

1. `CLI-first workflow validation`
   - run the full pipeline from the terminal
   - validate contracts, source ingestion, screening, and topic generation

2. `Minimal Web workspace`
   - connect the same contracts and workflow to a UI
   - focus on project brief, paper set, screening review, and topic brief

3. `Desktop packaging`
   - package the same frontend with `Tauri 2`
   - keep the same contracts and storage model

This is an implementation strategy, not a product statement. The product
remains Web-first and desktop-ready.

## 8. Milestones And Acceptance

### Milestone 1: End-to-End Single Project

- create one project brief
- collect one candidate paper set
- review one screening result set
- generate one topic brief
- run the entire workflow from the CLI

### Milestone 2: Better Reviewability

- edit screening decisions
- rerun from the same project brief
- inspect saved outputs
- export results
- complete one run from the Web UI

### Milestone 3: Desktop Packaging

- package the same frontend with `Tauri 2`
- keep the same workflow contracts
- keep the same storage model
- add stronger local project persistence

### Acceptance Criteria

The MVP is acceptable when it can:

1. take one real research question,
2. produce a candidate paper set,
3. support editable screening decisions,
4. generate 2 to 3 candidate topic directions tied to paper references,
5. complete one end-to-end run from the UI without manual file handling,
6. and produce results that the researcher judges to be useful as topic
   decision support.

For MVP quality, the final judge is the researcher in the loop, not an
automatic benchmark.
