# Topic Agent Milestone 2 Plan

This document turns the MVP spec into an implementation plan for Milestone 2.

Milestone 2 target:

- expose the topic-agent workflow as a local HTTP service
- build a minimal Web UI for one complete run
- support editable screening decisions and rerun

## 0. Foundation: Service Layer (completed in M1)

The `TopicAgentService` was extracted at the end of M1 as the application
service layer. All M2 transport code builds on top of it.

```
TopicAgentService
  create_run(brief)            -> WorkflowOutput
  get_run(run_id)              -> Option<RunManifest>
  list_runs(project_id)        -> Vec<RunManifest>
  list_project_papers(id)      -> Vec<PaperRecord>
  list_project_screening(id)   -> Vec<ScreeningDecision>
  list_project_topics(id)      -> Vec<TopicCandidate>
```

M2 will add methods to this service, then wrap it with axum.

## 1. Service Extensions

### Purpose

Add write-back and rerun capabilities that M1 did not need.

### New Methods

```rust
impl TopicAgentService {
    /// Overwrite one screening decision (user edit from UI).
    fn update_screening_decision(&self, decision: &ScreeningDecision) -> Result<()>;

    /// Re-run propose phase using current screening state in storage.
    fn rerun_propose(&self, project_id: &str) -> Result<WorkflowOutput>;
}
```

### Storage Additions

```rust
impl SqliteStorage {
    /// Update a single screening decision by (project_id, paper_id).
    fn update_screening_decision(&self, decision: &ScreeningDecision) -> Result<()>;

    /// Load the project brief by ID (needed for rerun).
    fn get_project(&self, project_id: &str) -> Result<Option<ProjectBrief>>;
}
```

### Completion Criteria

- a screening decision can be updated without re-running the full workflow,
- propose can be re-run from stored papers and edited decisions,
- the service layer has no knowledge of HTTP.

## 2. HTTP API (axum)

### Purpose

Wrap `TopicAgentService` as a localhost JSON API. This is a thin adapter layer
with no business logic.

### Crate

`crates/api/` — depends on `nabla-service`, `nabla-contracts`, `axum`, `tokio`.

### Design

```rust
// Shared state: the service wrapped in Arc for thread safety.
type AppState = Arc<TopicAgentService>;
```

axum handlers delegate directly to the service. No domain logic in handlers.

### Routes

```
POST   /api/runs                   create_run(brief)
GET    /api/runs/:run_id           get_run(run_id)
GET    /api/projects/:id/runs      list_runs(project_id)
GET    /api/projects/:id/papers    list_project_papers(project_id)
GET    /api/projects/:id/screening list_project_screening(project_id)
PUT    /api/projects/:id/screening update_screening_decision(decision)
GET    /api/projects/:id/topics    list_project_topics(project_id)
POST   /api/projects/:id/rerun     rerun_propose(project_id)
```

### Error Shape

All errors return:

```json
{ "error": "human-readable message" }
```

with appropriate HTTP status codes (400, 404, 500).

### Completion Criteria

- all routes are callable with curl,
- the API server starts on a configurable localhost port,
- handlers are at most 10 lines each (thin adapter test),
- no business logic exists in the HTTP layer.

## 3. Web UI

### Purpose

Provide a minimal frontend that completes one full run from the browser.

### Stack

As specified in the MVP:

- React + TypeScript
- Vite
- TanStack Query (data fetching)
- Tailwind CSS (styling)

### Location

`apps/web/` — a standalone Vite project that calls the local API.

### Pages

Four pages, one per workflow phase:

1. **Project Brief** (`/`)
   - form: goal, constraints, keywords, date range
   - submit button triggers `POST /api/runs`
   - shows run progress

2. **Paper Set** (`/projects/:id/papers`)
   - table: title, authors, year, source
   - link to source URL
   - count summary

3. **Screening** (`/projects/:id/screening`)
   - table: paper title, label (editable dropdown), rationale, tags, confidence
   - save button triggers `PUT /api/projects/:id/screening`
   - rerun button triggers `POST /api/projects/:id/rerun`

4. **Topic Brief** (`/projects/:id/topics`)
   - card per topic: title, why_now, scope, representative papers, entry_risk,
     fallback_scope
   - linked paper references resolve to paper set

### Navigation

Simple top-level layout with:

- project selector (or single-project for MVP)
- phase tabs: Brief | Papers | Screening | Topics

### Completion Criteria

- one full run can be started from the UI,
- screening decisions can be edited and saved,
- topics can be re-generated after editing screening,
- results are visible without manual file inspection.

## 4. Development Workflow

### Dev Server

Two processes during development:

```bash
# terminal 1: API server
cargo run -p nabla-api -- --port 3001

# terminal 2: Vite dev server with proxy
cd apps/web && npm run dev
```

Vite proxies `/api/*` to `localhost:3001`.

### Production Build

For production, the API server serves the built Vite assets as static files
from a single binary. This is the shape that M3 (Tauri) will wrap.

## 5. Suggested Build Order

Build in this order:

1. **Service extensions** — `update_screening_decision`, `rerun_propose`,
   `get_project` in storage
2. **HTTP API crate** — axum routes, shared state, error handling
3. **Web UI scaffold** — Vite project, routing, TanStack Query hooks
4. **Page: Project Brief** — form + run creation
5. **Page: Paper Set** — read-only table
6. **Page: Screening** — editable table + save + rerun
7. **Page: Topic Brief** — read-only cards with paper links

Reason:

- service extensions must land first so the API has something to call,
- the API must exist before the UI can fetch data,
- pages are built in workflow order so each can be tested against real data
  from the previous step.

## 6. Blocking Considerations

### Thread Safety

`TopicAgentService` currently uses `Box<dyn PaperCollector>` and
`Box<dyn AgentAdapter>`. For axum (`Arc<TopicAgentService>` across async
tasks), the traits and their implementations must be `Send + Sync`.

Action: add `Send + Sync` bounds to `PaperCollector` and `AgentAdapter` traits.
Current implementations (reqwest blocking client, CLI process spawning) already
satisfy these bounds.

### Blocking in Async

`create_run` calls blocking HTTP clients and spawns CLI subprocesses. Under
tokio, these must run on the blocking thread pool.

Action: wrap `create_run` and `rerun_propose` in `tokio::task::spawn_blocking`
inside the axum handlers.

### CORS

The Vite dev server runs on a different port. The API must allow CORS from
`localhost:5173` during development.

## 7. What Is Not In Scope

- user authentication or multi-user support
- project deletion or archival
- paper import from file upload
- custom adapter configuration from the UI
- deployment beyond localhost

These belong to later milestones or post-MVP work.

## 8. Done Condition For Milestone 2

Milestone 2 is done when:

1. the local API server starts and serves all defined routes,
2. the Web UI can create a project brief and start a run,
3. collected papers are displayed in a browsable table,
4. screening decisions can be edited and saved from the UI,
5. topics can be regenerated after editing screening,
6. the full loop (brief -> papers -> screening edit -> topic regeneration) can
   be completed from the browser without touching the CLI,
7. and no business logic exists in the HTTP or UI layers.
