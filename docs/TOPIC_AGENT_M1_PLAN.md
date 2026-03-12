# Topic Agent Milestone 1 Plan

**Status: COMPLETED**

This document turns the MVP spec into an implementation plan for Milestone 1.

Milestone 1 target:

- run one end-to-end topic-selection workflow from the CLI
- for one project brief
- using the fixed pipeline:
  `frame -> collect -> screen -> propose`

## 1. Contracts

### Purpose

Define the minimum shared schemas used by the rest of Milestone 1.

### Scope

- `ProjectBrief`
- `PaperId`
- `PaperRecord`
- `ScreeningLabel`
- `ScreeningDecision`
- `TopicCandidate`
- `RunManifest`

### Completion Criteria

- the core structs and enums compile,
- field names are frozen for Milestone 1,
- the other crates can depend on them without redefining local copies.

### Dependency Role

- this is the foundation for all later tasks,
- no other task should invent its own schema before this exists.

## 2. Sources

### Purpose

Fetch and normalize candidate papers into `PaperRecord`.

### Scope

- `OpenAlex` source adapter
- `arXiv` source adapter
- normalization into a shared `PaperRecord`
- deduplication rules for obvious duplicates

### Completion Criteria

- a project brief can produce a candidate paper set,
- the output is normalized into `PaperRecord`,
- duplicate records are merged or filtered consistently,
- each paper keeps source provenance.

### Dependency Role

- depends on `contracts`,
- feeds `storage` and `workflow`.

## 3. Storage

### Purpose

Persist workflow data and exported artifacts for one run.

### Scope

- SQLite schema for core entities
- artifact directory layout
- create/read operations needed by Milestone 1

### Completion Criteria

- projects, papers, screening decisions, topic candidates, and run manifests
  can be persisted,
- artifacts can be written to the filesystem,
- one completed run can be inspected after the CLI exits.

### Dependency Role

- depends on `contracts`,
- used by `workflow` and `cli`.

## 4. Agent Adapters

### Purpose

Provide the semantic generation layer for screening and topic proposal.

### Scope

- adapter trait for topic-agent operations
- at least one working local adapter
- optional second adapter behind the same trait

Trait (implemented):

```rust
pub trait AgentAdapter {
    fn name(&self) -> &'static str;

    fn screen(
        &self,
        brief: &ProjectBrief,
        papers: &[PaperRecord],
    ) -> Result<Vec<ScreeningDecision>>;

    fn propose(
        &self,
        brief: &ProjectBrief,
        papers: &[PaperRecord],
        decisions: &[ScreeningDecision],
    ) -> Result<Vec<TopicCandidate>>;
}
```

Design notes:

- batch screening (`&[PaperRecord]`) reduces LLM round-trips vs per-paper calls,
- `propose` receives `decisions` so it can filter by label without re-screening,
- synchronous for M1 (blocking reqwest); async can be added when real adapters need it.

Recommended order:

- implement one adapter first
- add the second only after the trait and workflow are stable

### Completion Criteria

- the workflow can call one adapter for `screen`,
- the workflow can call one adapter for `propose`,
- adapter output is converted into Milestone 1 contracts.

### Dependency Role

- depends on `contracts`,
- consumed by `workflow`.

## 5. Workflow

### Purpose

Orchestrate the fixed Milestone 1 pipeline.

### Scope

- `frame`: in M1 this is a passthrough that maps CLI input directly to a
  `ProjectBrief` without LLM expansion
- `collect`: calls `sources` to fetch and deduplicate papers
- `screen`: calls `adapters` to label each paper
- `propose`: calls `adapters` to generate topic candidates from included papers
- run manifest updates between phases

### Completion Criteria

- the workflow can execute all four phases in order,
- each phase produces the expected contract object,
- failures stop the run with an inspectable manifest or artifact trail,
- the final output includes a topic brief linked to paper references.

### Error Policy

M1 uses fail-fast within each phase: any unrecoverable error stops the current
phase and marks the run as `Failed`. Partial results from previously completed
phases are preserved in storage. The user can inspect what succeeded and rerun.

### Dependency Role

- depends on `contracts`, `sources`, `storage`, and `adapters`,
- is the main dependency of `cli`.

## 6. CLI

### Purpose

Expose Milestone 1 as one end-to-end executable entry point.

### Scope

- one command to run the topic workflow
- one input style for project brief creation
- one output style for summary plus artifact location

Recommended shape:

- accept a single JSON file as input (`--brief path/to/brief.json`)
- do not add interactive argument input or multiple command variants in
  Milestone 1

### Completion Criteria

- a user can start one full run from the terminal,
- the command executes the fixed workflow,
- the command reports success or failure clearly,
- the command tells the user where results were stored.

### Dependency Role

- depends on `workflow`,
- is the Milestone 1 entry surface.

## 7. Suggested Build Order

Build in this order:

1. `contracts`
2. `sources`, `storage`, `adapters` (can be built in parallel; all depend only
   on `contracts`)
3. `workflow`
4. `cli`

Reason:

- `contracts` must land first as the shared foundation,
- the middle layer has no internal dependencies and can be parallelized,
- `workflow` integrates all three, so it comes after they stabilize,
- `cli` is a thin shell over `workflow`.

## 8. Done Condition For Milestone 1

Milestone 1 is done when:

1. one project brief can be created from the CLI,
2. candidate papers can be fetched and normalized from the selected sources,
3. the screening step produces inspectable contract-level decisions,
4. the proposal step produces 2 to 3 topic directions tied to paper records,
5. the run leaves behind inspectable persisted outputs,
6. and the entire workflow can be rerun from the CLI without changing code.

All six conditions are met.

## 9. Post-M1: Service Layer Extraction

At the end of M1, a `TopicAgentService` application service was extracted into
`crates/service/`. This decouples domain operations from transport:

- the CLI now calls the service instead of wiring workflow components directly,
- the same service will be wrapped by axum (M2) and Tauri commands (M3),
- storage gained query methods (`list_papers`, `list_screening_decisions`,
  `list_topic_candidates`, `get_run_manifest`, `list_run_manifests`) to support
  read-back through the service.

This prepares the codebase for M2 without changing the M1 contract or behavior.
