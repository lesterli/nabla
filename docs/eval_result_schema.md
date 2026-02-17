# Evaluation Result Schema (`agent-cli eval`)

`agent-cli eval` outputs one JSON object to stdout.

## Top-level shape

```json
{
  "schema_version": 1,
  "version": "0.1.0",
  "results": [
    {
      "schema_version": 1,
      "task_id": "task-1",
      "outcome_or_status": "done",
      "stop_facts": {
        "stop_reason": "done",
        "tool_error_count": 0,
        "last_tool_calls": [],
        "has_pending_approval": false
      },
      "enabled_tools": ["read", "write", "edit", "bash"],
      "tool_calls": [],
      "tool_call_stats": {
        "total_proposed": 0,
        "total_executed": 0,
        "total_errors": 0,
        "by_tool": []
      },
      "steps": 1,
      "latency_ms": 12,
      "token_usage": {
        "estimated_input_tokens": 4,
        "estimated_output_tokens": 3,
        "estimation_method": "provider_native"
      },
      "error_type": null,
      "version": "0.1.0",
      "submission_id": "eval-task-1",
      "run_metadata": {
        "submission_id": "eval-task-1",
        "provider": "mock",
        "model": "mock-static",
        "seed": "42",
        "protocol_schema_version": 1
      }
    }
  ]
}
```

## Field semantics

- `schema_version`: schema version for the eval envelope/result (`1`).
- `version`: `agent-cli` binary version.
- `results`: list of task-level results.

Task-level fields:
- `task_id`: task identifier from CLI (`--task-id`) or task file.
- `outcome_or_status`: current stop status (`done`, `error`, `budget_exceeded`, `policy_denied`, `human_approval_required`, `interrupted`).
- `stop_facts`: final `turn_stopped.facts` if available.
- `enabled_tools`: tools enabled for this eval task execution.
- `tool_calls`: tool calls proposed during execution.
- `tool_call_stats`: aggregated tool-call metrics:
  - `total_proposed`
  - `total_executed`
  - `total_errors`
  - `by_tool[]` (`name`, `proposed`, `executed`, `errors`)
- `steps`: number of `context_built` events (control-loop steps).
- `latency_ms`: wall-clock latency for the task run.
- `token_usage`: usage metrics (`provider_native` preferred, fallback to heuristic):
  - `estimation_method`: `provider_native | mixed_provider_native_and_heuristic | heuristic_word_count`
- `error_type`: nullable normalized error classification.
- `submission_id`: submission identifier used for this task run.
- `run_metadata`: deterministic run metadata.

`run_metadata` fields:
- `submission_id`: repeated for convenience.
- `provider`: resolved from `AGENT_LLM_PROVIDER` (default `mock`).
- `model`: resolved from `AGENT_LLM_MODEL` (default `mock-static` for mock, else `gpt-4o-mini`).
- `seed`: optional `AGENT_EVAL_SEED` if provided.
- `protocol_schema_version`: event protocol schema version from `agent-core`.

## Input modes

Single task:

```bash
agent-cli eval --task-id task-1 --submission-id eval-task-1 "你好，请用一句话介绍自己"
```

Batch tasks (`--tasks-file`):
- Supports JSON array (`[{"task_id":"...","prompt":"..."}]`)
- Supports JSONL (one JSON object per line)

Task object fields:
- `task_id` (string, required)
- `prompt` (string, required)
- `submission_id` (string, optional)

## Tooling baseline fixtures

- Reproducible tooling eval fixtures live under:
  - `agent-cli/fixtures/eval/tools/default-four-tools.jsonl`
  - `agent-cli/fixtures/eval/tools/extended-tools.jsonl`
  - `agent-cli/fixtures/eval/tools/read-only-tools.jsonl`
  - `agent-cli/fixtures/eval/tools/write-tools.jsonl`
  - `agent-cli/fixtures/eval/tools/execute-tools.jsonl`
- Coverage:
  - Default four tools baseline: `read`, `write`, `edit`, `bash`
  - Extended tools baseline: `grep`, `find`, `ls`
  - Layered task slices:
    - read-only tasks (`read`, `grep`, `find`, `ls`)
    - write tasks (`write`, `edit`)
    - execute tasks (`bash`)

Examples:

```bash
agent-cli eval --tasks-file agent-cli/fixtures/eval/tools/default-four-tools.jsonl --tools read,write,edit,bash
agent-cli eval --tasks-file agent-cli/fixtures/eval/tools/extended-tools.jsonl --tools grep,find,ls
```
