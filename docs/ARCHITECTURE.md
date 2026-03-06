# Architecture

An AI4Science agent is a **constrained state machine**, not a chatbot.

- **LLM** generates candidate actions
- **Runtime** drives the turn loop
- **Policy** gates every side-effect
- **Event log** makes it auditable and reproducible

## Workspace

```
nabla/
  core/        → nabla          # protocol, runtime, policy, tools, memory
  llm/         → nabla-llm      # LLM gateway: provider adapters, token tracking
  cli/         → nabla-cli      # terminal ingress adapter
    src/
      main.rs       # entry point
      cli.rs        # argument parsing, command definitions
      commands.rs   # command execution, extension wiring
      eval.rs       # eval framework (single + batch)
      extensions/   # CLI extension host (turn hooks)
      tooling/      # tool implementations (read, write, edit, bash, grep, find, ls)
  Cargo.toml        # workspace root
```

Split a module into its own crate only when it stabilizes and gains multiple dependents.

## Invariants

1. Every `Op` carries a `submission_id`. Every `Event` traces back to one.
2. No side-effect without a `PolicyDecision` (auto or human).
3. Runtime depends only on protocol types — never on adapters.
4. Any session is reconstructible from its event log.
5. Protocol schema is versioned and frozen (snapshot tests).

## Turn Loop

```
UserInput
  → build context (recent events + compressed summary)
  → LLM response
  → parse text / tool-calls
  → policy check each tool-call
  → execute tool, emit ToolResult event
  → check stop condition (done / interrupt / error / budget)
  → persist events, return
```

Every step emits an event. Failures too. Any interruption resumes from the last checkpoint.

## Layers

| Layer | Crate | Role |
|-------|-------|------|
| **protocol** | `nabla` | `Op` / `Event` types — the single source of truth |
| **runtime** | `nabla` | Turn loop, state transitions, checkpoint/resume |
| **policy** | `nabla` | `Allow` / `Deny` / `AskHuman` before side-effects |
| **tools** | `nabla` | Registration, schema validation, isolated execution |
| **memory** | `nabla` | Event persistence (in-memory + JSONL file), context compression, replay |
| **llm-gateway** | `nabla-llm` | Provider adapters (OpenAI-compatible), retry/backoff, fallback, token tracking |
| **ingress** | `nabla-cli` | CLI parsing, command dispatch, eval harness — no business logic |

## Dependency Flow

```
nabla-cli → nabla-llm → nabla
          → nabla
```

`nabla` has zero external dependencies beyond `serde` / `serde_json`.
`nabla-llm` adds `reqwest` for HTTP. This split keeps the core light.

## Layer Responsibilities

### Ingress (`nabla-cli`)

- Parse requests, map identifiers, render output.
- No scientific provenance, no approval state.

### Runtime (`nabla`)

- Single-run turn loop, tool dispatch, policy gating, event emission, checkpoint/resume.
- Domain-agnostic.

## Design Principles

1. **Conservative crate boundaries.** The logical architecture can be broad;
   the crate architecture should be conservative. Split only where there is
   already clear pressure.

2. **Promote only when stable.** New crates only after schemas stabilize
   and more than one crate needs them.

3. **No speculative adapters.** HTTP/API ingress should be created only when
   there is an actual service boundary with defined run/session/auth contracts.

## Follow-Up

1. **Workflow orchestration** — multi-phase research loops (e.g. PDE: understand →
   improve → verify → reproduce) will be added as a separate crate when ready.
2. **Unified run contract** — first-class fields for `run_id`, `session_id`,
   `actor_id`, `trace_id`, `risk_level`, `policy_context`.
3. **Core split** — when `nabla` grows large enough, split into `nabla-protocol`
   and `nabla-runtime`.
