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
      "tool_calls": [],
      "steps": 1,
      "latency_ms": 12,
      "token_usage": {
        "estimated_input_tokens": 4,
        "estimated_output_tokens": 3,
        "estimation_method": "heuristic_word_count"
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
- `tool_calls`: tool calls proposed during execution.
- `steps`: number of `context_built` events (control-loop steps).
- `latency_ms`: wall-clock latency for the task run.
- `token_usage`: estimated token usage (heuristic word-count in PR16).
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
