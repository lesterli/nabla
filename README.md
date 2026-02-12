# agent-rs

A Pi-style coding agent in Rust

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
