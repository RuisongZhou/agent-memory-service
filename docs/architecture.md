# Module architecture

Main contains eight crates and one memory algorithm, Mem0. The previous dual-strategy implementation
is preserved on [`codex/mem0-memos`](https://github.com/RuisongZhou/agent-memory-service/tree/codex/mem0-memos),
starting at `971ac13`. Historical evaluation protocols and raw evidence keep their original meaning.

| Crate | Responsibility | Workspace dependencies |
|---|---|---|
| `memory-rules` | Pure temporal, identity, ranking and integrity rules | none |
| `memory-fulltext` | Lexical and entity indexing | none |
| `memory-config` | Startup configuration and tunables | none |
| `memory-domain` | Records, request types, errors and pure integrity planners | `memory-rules` |
| `memory-store` | SurrealDB persistence and transaction integrity | `memory-domain`, `memory-rules` |
| `memory-engine` | Tenants, workers, model clients, context and application operations | config, domain, store, rules, fulltext |
| `memory-algo-mem0` | Ordered additive formation, hybrid retrieval, entity projection and its worker | engine and its dependencies |
| `memory-service` | HTTP/MCP, composition, diagnostics and `agent-memoryd` | service layers |

The engine retains the `MemoryAlgorithm` interface to separate runtime and application code from
formation and retrieval. Only the service composition root names the concrete Mem0 implementation.
`scripts/check_architecture.py` enforces the dependency graph, storage boundaries and evaluation layout.
The single-value strategy setting preserves existing Mem0 launcher compatibility; unknown strategies fail early.

Tenant startup launches the durable index worker and one ordered Mem0 formation worker.
`get_context` reads current context without closing sessions or scheduling formation. Explicit instructions,
record reads, correction, deletion, feedback and reindexing remain application operations.
There is no session-experience scheduler, periodic maintenance worker or `/admin/run_jobs` endpoint.

## Storage compatibility and integrity

Existing Mem0 stores retain their schema. Shared historical records, source evidence and integrity rules
remain readable so corrections and deletions can repair references instead of orphaning old data.
Profiles and skills already in a store are current views: source changes invalidate their dependents
transitively. Historical experiences retain their lineage. Main does not generate these record types.
Startup repairs missing/non-current dependencies and rejects unfinished legacy formation work.
Switching a Git branch does not migrate or clear a database.

Storage commits source changes, invalidations, uncertainty closure and index tasks atomically. Pure planners
live in `memory-domain`; `memory-store` loads their inputs and applies the plan. Cached metadata is cleared
before a memory commit, and failed transactions never publish a new snapshot. Explicit instruction writes
remain synchronous, scoped and idempotent, and make no generative call.

Model servers, vector/full-text indexes and entity projections remain separate from primary records.
Indexes can be rebuilt; raw inputs and source evidence remain durable. This release supports one local user;
tenant/platform headers route requests and do not authenticate identity. The daemon binds only to loopback.

## Verification

Use `make fmt test-rust test-service test build selftest smoke check-arch check-public`.
Pure Rust and Python tests are offline; selftest uses mock retrieval models. HTTP checks use local stubs
or verbatim writes, and verify all eight MCP tools, persistence, correction/deletion, strict configuration,
source boundaries and recovery. The optional `llama_http_smoke.py` checks already running local retrieval
models with deterministic generation. No automatic check invokes a paid model or runs the full dataset.
Frozen benchmark manifests and local evidence stay under `eval/`; historical replay clients require the
matching preserved binary. See [evaluation tools](../eval/README.md).

## Additive strategy

For the complete Chinese design report, request lifecycle diagrams, code-reading map and full LoCoMo evidence, see [新方案设计与实现说明](design/mem0-strategy-design.md).

The default `mem0` strategy selects paired formation/retrieval modules. `memory-algo-mem0/src/formation.rs` owns durable
ordered inputs, the pinned one-call additive extraction contract, scoped content identity and batch
vector admission. `memory-algo-mem0/src/retrieval.rs` owns the Node SDK 3.3.1 semantic/BM25/entity scoring pipeline and optional reranking. Pure ranking
and content identity live in `memory-rules::mem0`; generic transactions and vector reads remain in storage.

Primary memories, embeddings, batch provenance, history, index tasks and the `indexing` checkpoint
commit atomically. Vector-cache publication precedes `completed/searchable` status. A crash at the
checkpoint republishes saved vectors without extraction. The lexical worker recognizes preindexed
admission tasks, while ordinary correction/rebuild tasks still regenerate vectors. No read forms memories.

Mem0-mode jobs retain full raw input in `requests`; `mem0_sources` explicitly declares batch provenance.
Current extraction messages remain complete. Historical prompt context uses the last ten messages
from durably formed inferred requests in the same platform/project/session, with a 300-UTF-16-unit
view per message. Failed/raw requests remain auditable but do not enter that conversation history;
project memory visibility is unchanged. Rebuildable `mem0_entities` and `mem0_entity_state` track entity
vectors/links and projection versions. Formation publishes them after the primary commit; search repairs
stale projections under the formation lock before entity scoring. See [SDK 3.3.1 alignment](design/mem0-sdk-3.3.1-alignment.md).
The model cannot assign visibility scopes or database IDs, and inferred text is not called a verified quote.
Legacy source/formation records are never silently reinterpreted. Startup rejects data directories containing unfinished legacy jobs. Use the preserved branch to finish those jobs or a fresh Mem0 store.
