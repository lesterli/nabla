# Nabla

A simple AI4Science agent

## LLM Provider Setup

By default, CLI runs with a local mock provider.

To call a real LLM provider (OpenAI-compatible API), set:

```bash
export AGENT_LLM_PROVIDER=openai
export AGENT_LLM_API_KEY=your_api_key
export AGENT_LLM_BASE_URL=https://api.openai.com/v1
export AGENT_LLM_MODEL=gpt-4o-mini
# Optional tool-call declaration to provider:
export AGENT_LLM_TOOLS=echo
export AGENT_LLM_TOOL_CHOICE=required
```

Then run:

```bash
cargo run -p agent-cli -- "Write a rust function to parse csv"
```

To verify provider tool-calls end-to-end, run:

```bash
cargo run -p agent-cli -- "Call the echo tool with text=hello and then answer briefly."
```

Expected additional events in output when tool-calling is triggered:
- `tool_call_proposed`
- `policy_evaluated`
- `tool_executed`

## PDE Workflow Mode

`agent-cli` now supports a PDE-oriented research workflow that drives the agent through:

1. `understand`
2. `improve`
3. `verify`
4. `reproduce`

Run it with a persistent event store:

```bash
cargo run -p agent-cli -- run \
  --workflow pde \
  --submission-id pde-exp-1 \
  --store-file /tmp/pde-exp-1.jsonl \
  "Improve the PDE neural-operator baseline and leave a reproducible report."
```

The CLI orchestrates the workflow on top of the existing runtime and emits workflow events such as:
- `workflow_started`
- `workflow_phase_started`
- `workflow_phase_completed`
- `workflow_completed`

Internally, the workflow logic now lives in `agent-orchestrator` rather than in
the CLI adapter, which is the first step toward a general AI4Science agent
substrate.
