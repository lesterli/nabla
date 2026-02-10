# Architecture

A coding agent is a **constrained state machine**, not a chatbot.

- **LLM** generates candidate actions
- **Runtime** drives the turn loop
- **Policy** gates every side-effect
- **Event log** makes it auditable and recoverable

## Workspace

```
agent-rs/
  core/         # protocol, runtime, policy, tools, memory
  llm/          # multi-provider gateway
  cli/          # terminal adapter
  Cargo.toml    # workspace root
```

Split a module into its own crate when it stabilizes and gains multiple dependents.

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
  → stream LLM response
  → parse text / thinking / tool-calls
  → policy check each tool-call
  → execute tool, emit ToolResult event
  → check stop condition (done / interrupt / error / budget)
  → persist events, return
```

Every step emits an event. Failures too. Any interruption resumes from the last checkpoint.

## Layers

| Layer | Role |
|-------|------|
| **protocol** | `Op` / `Event` types — the single source of truth |
| **runtime** | Turn loop, state transitions, checkpoint/resume |
| **policy** | `Allow` / `Deny` / `AskHuman` before side-effects |
| **tools** | Registration, schema validation, isolated execution |
| **llm-gateway** | Provider adapters, retry/backoff, fallback, cost tracking |
| **memory** | Event persistence, context compression, replay |
| **adapters** | CLI / ACP / RPC — protocol translation, no business logic |