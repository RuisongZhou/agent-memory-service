# Agent Memory Service

A local shared memory service for AI agents. Codex, Claude Code and other clients can save facts
and explicit instructions, recall project decisions, inspect sources and correct earlier memories.
Each client keeps its own task state and native configuration.

The Rust daemon exposes a JSON HTTP API and a stdio MCP bridge; the same `agent-memoryd` binary also
provides the hook commands that capture dialogue and inject memory, and uploads captured dialogue itself. Main supports **Mem0 only**:
ordered additive formation, persistent raw inputs, scoped exact deduplication and
semantic/BM25/entity scoring aligned with the Node SDK 3.3.1 built-in NLP fallback.
SurrealDB/SurrealKV runs in-process. Embeddings and optional reranking come from hosted models through
an API key by default, or from your own llama-server processes. A separately configured generation
provider performs inferred memory formation.

## Start here

Running the service takes the `agent-memoryd` binary, from a
[release archive](docs/deployment.md#release-archive) or a Rust source build, and a model API key.
Python 3.11+ is needed only for the client installer (`scripts/install.sh`) and evaluation.

| Task | Authoritative guide |
|---|---|
| Install, configure model keys, start the daemon or run it as a service, back up or troubleshoot | [Deployment and operations](docs/deployment.md) |
| Run the embedding and rerank models yourself instead of the hosted API | [Self-hosted retrieval models](docs/self-hosted-models.md) |
| Install into Codex, Claude Code, WorkBuddy or DeepSeek Harness with one command | [One-command install](docs/mcp-harness.md#one-command-install) |
| Connect Codex/Claude Code, capture dialogue and upload it | [MCP and harness integration](docs/mcp-harness.md) |
| Look up request fields, completion states, scope or configuration defaults | [API and configuration](docs/api.md) |
| Understand write/read paths, storage, entity projection and SDK adaptations | [Architecture](docs/architecture.md) |
| Build, validate or contribute a change | [Contributing](CONTRIBUTING.md) |
| Run a controlled SDK comparison or inspect historical evidence | [Evaluation](eval/README.md) |
| Find service operations and verification scripts | [Script inventory](scripts/README.md) |

Follow deployment first, then client integration. `agent-memoryd service install` keeps the daemon
running as a macOS LaunchAgent or a systemd user unit. After the daemon is running, one command registers
the bridge and dialogue capture in an agent platform, for example
`bash scripts/install.sh --host codex --apply --project my-project` (also `workbuddy`, `claude-code`
with `--workspace`, and `deepseek-harness`, which gets MCP tools only). Add `--recall` on the first three
to inject relevant memory automatically at session start and with each prompt. MCP registration alone does not start the daemon, capture conversations or inject
memory into the client. The daemon binds to loopback for one local user; tenant/platform headers are
routing metadata, not authentication.

## Repository layout

| Location | Contents |
|---|---|
| `crates/` | Service implementation; layer responsibilities are in the architecture guide |
| `docs/` | Current service documentation, one guide per subject |
| `examples/` | Editable service, MCP and capture templates |
| `scripts/` | Service operations and verification |
| `eval/` | SDK baselines, datasets, protocols, experiments and historical reports |
| `results/` | Ignored service logs and checks |
| `eval/results/` | Ignored benchmark evidence, runs and stores |

Docs in this checkout describe the current Mem0 service. The previous Mem0 + Memos implementation
is preserved on [`codex/mem0-memos`](https://github.com/RuisongZhou/agent-memory-service/tree/codex/mem0-memos).
Older design revisions remain in Git history; benchmark reports in `eval/` retain their recorded versions.
See [compatibility](docs/api.md#compatibility) before reusing a legacy data directory.

## Licensing

See [LICENSE](LICENSE) and [third-party attribution](THIRD_PARTY.md). Copied source and prompts retain
their notices. Keys, model weights, generated databases, raw traces and local caches are not shipped.
