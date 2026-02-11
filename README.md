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
```

Then run:

```bash
cargo run -p agent-cli -- "Write a rust function to parse csv"
```
