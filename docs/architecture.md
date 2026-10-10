# Module architecture

The Cargo workspace contains three crates. `memory-service` coordinates the shared store and API and calls embedding/rerank models served by external llama-server processes; `memory-fulltext` owns lexical/entity indexing; `memory-rules` contains deterministic temporal and revision rules without storage dependencies.

Within `memory-service/src`, physical modules follow responsibilities:

| Directory | Responsibility |
|---|---|
| `api/` | HTTP dispatch, MCP forwarding, corrections, deletion and feedback |
| `formation/` | Durable ingest, P1/P1-b facts, P2 experiences, P3 profiles, P4 proposals and P5 skills |
| `retrieval/` | Scope-aware recall and assembly; `scoring.rs` holds pure fusion, packing and diversity helpers |
| `runtime/` | Configuration, engine, tenant registry, LLM client, llama-server model client and index worker |
| `storage/` | SurrealKV schema, transactions and durable index tasks |
| `domain/` | Stored records, request schemas and API errors |
| `diagnostics/` | Offline database and end-to-end selftests |

`lib.rs` provides a stable Rust facade through explicit module paths. This keeps cross-module names consistent while avoiding duplicated rules or unnecessary changes to formation/retrieval semantics. Revision planning, date parsing and identifier utilities are shared through `memory-rules`.

Ingest commits raw turns, idempotency receipts and index tasks before returning. Background workers form facts on the batching schedule, then generate experiences and the summary only after session end. These stages use independent transactions and typed records in the same store. Sources remain retained until session-end processing succeeds, then eligible buffers can be purged. Index queues and usage records survive process restart.

Retrieval combines vector, BM25, entity and time channels; it checks visibility and update chains against stored records, reranks candidates locally, then assembles bounded context. Profiles and recent turns are handled separately. Corrections append records; exact deduplication preserves both source bodies and evidence.

This release supports one local user. Evaluation tenants route isolated stores; HTTP caller headers do not authenticate identity. The daemon therefore accepts only loopback bindings.
