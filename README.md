# Agent Memory Service

A local shared memory service for AI agents, with evidence, append-based revisions, asynchronous extraction, hybrid retrieval, and HTTP/MCP interfaces. The service keeps each agent's private task state outside the shared memory store.

This is an experimental implementation. A 152-question LoCoMo pilot is complete; the remaining nine conversations have not been evaluated. See [evaluation protocol](docs/evaluation.md), [pilot results](docs/benchmarks/conv26.md), and [implementation review](docs/review.md).

## Project layout

```text
crates/
  memory-service/   API, formation, retrieval, runtime, storage and diagnostics
  memory-models/    BGE-M3 and BGE reranker; CPU and optional Candle Metal
  memory-fulltext/  Tantivy, Chinese tokenization and entity aliases
  memory-rules/     Pure date, identifier and revision-planning rules
eval/              LoCoMo runner, mem0 baseline, answer/judge and usage
model_validation/  Torch comparisons, fixture generator and benchmarks
scripts/           Setup, downloads, offline smoke tests and publication checks
examples/          HTTP/MCP and configuration examples
docs/              Architecture, development, review and pilot protocol
licenses/          Notices for copied evaluation code and dataset terms
```

## Build and verify

Install Rust and Python 3.11 or later. Commands run from the repository root.

```sh
make setup                 # Python dependencies, public dataset and tokenizer caches
make build                 # CPU build
make test-rust test        # Small Rust crates and Python tests
make selftest smoke        # Mock models and localhost LLM stub; no paid requests
```

On Apple Silicon, build with `make build-metal`. Explicit Metal selection fails if unavailable. Runtime metadata reports the actual backend/device/dtype. Full model weights and synthetic test weights are **never included in this repository**, Git history, CI artifacts or releases. See [model validation](model_validation/README.md) for local download and verification.

## Run

The generation backend defaults to `qwen3.8-flash` at `https://maas.qianwenaiapi.com/compatible-mode/v1`, with thinking disabled (`enable_thinking=false`). Put `QWEN_API_KEY`, `QWEN_BASE_URL`, and `LLM_MODEL` in `.env` using `.env.example`; `scripts/serve.py` reads the key without executing shell content. Model files must be downloaded separately into `models/`.

```sh
.venv/bin/python scripts/serve.py --device metal --max-calls 60
target/fast/agent-memoryd mcp --url http://127.0.0.1:8765 --platform my-agent
```

The daemon binds only to loopback. Headers are `X-Memory-Tenant` and `X-Memory-Platform`; configuration environment variables use `MEMORY_`. The optional evaluation tenants isolate different prompts, but they are not an authentication boundary. Use an authenticated gateway for any remote access.

Writing dialogue and running maintenance can invoke the configured LLM. Generation budgets are soft process-level guards and reset at process restart; persistent usage is stored separately. Retrieval uses local models and does not invoke a generative model.

## Interfaces

HTTP exposes `add_memories`, `get_task`, `search_memories`, `get_context`, `list_turns`, `get_memory`, `correct_memory`, `delete_memory`, and `feedback` under `/v1/`. The stdio MCP bridge exposes five tools: add, search, get, correct and feedback. See [API reference](docs/api.md).

## Development and attribution

See [CONTRIBUTING](CONTRIBUTING.md) and [architecture](docs/architecture.md). CI performs offline correctness checks and never runs paid evaluation. Open a pull request with the problem, resulting behavior and relevant checks.

The initial memory-service code derives from [memory-mcp-1file](https://github.com/pomazanbohdan/memory-mcp-1file); its MIT copyright notice is retained. Copied evaluation assets have separate Apache-2.0 notices. LoCoMo is downloaded locally under CC BY-NC 4.0 and is not redistributed here. Details are in [THIRD_PARTY](THIRD_PARTY.md).
