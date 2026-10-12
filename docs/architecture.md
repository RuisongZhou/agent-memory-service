# Module architecture

The Cargo workspace contains five crates. `memory-service` coordinates the shared store and API and calls embedding/rerank models served by external llama-server processes; `memory-domain` holds the pure shared records, request types, errors and integrity planners; `memory-store` owns SurrealDB persistence on top of them; `memory-fulltext` owns lexical/entity indexing; `memory-rules` contains deterministic temporal and revision rules without storage dependencies. Dependencies point one way: `memory-rules` <- `memory-domain` <- `memory-store` <- `memory-service`.

The demo chooses one complete algorithm at startup using `--memory-strategy` or the existing configuration.
`Engine::new` fixes that selection for the process lifetime. Every tenant in that process uses the same
algorithm. The engine holds an `Arc<dyn MemoryAlgorithm>` (`runtime/algorithm.rs`) and never names a concrete
algorithm; `algorithms::select` is the composition-time factory. The trait covers formation, retrieval, worker
startup, private tables and DDL, and optional session/maintenance/provenance hooks.
The configuration value `memos` selects short-term buffering with asynchronous long-term formation; `mem0` remains the default.

| Algorithm | Formation and lifecycle | Retrieval | Code ownership |
|---|---|---|---|
| `mem0` | Durable ordered inputs, additive extraction, scoped exact deduplication and atomic vector admission | Semantic search with explicitly enabled reranking; raw submitted batches | `algorithms/mem0/{formation.rs,retrieval.rs,runtime.rs,prompts/}` |
| Memos | Complete short-term turns, asynchronous P1 facts, session-end P2 experiences and P3/P4/P5 maintenance | Vector/BM25/entity/time fusion, recent-turn recall and update-chain history | `algorithms/memos/{formation/,retrieval/,runtime.rs}` |

Neither algorithm calls the other algorithm's formation, retrieval or maintenance implementation.
Their prompt contracts, batching rules, provenance, ranking and existing numeric settings remain distinct.
The Memos pipeline is a supported algorithm and benchmark control, not unused Mem0 scaffolding.

Shared components retain their layer boundaries:

| Crate / directory | Responsibility |
|---|---|
| `api/` | HTTP and MCP transport only |
| `app/` | Use cases shared by every algorithm: admission checks, request inspection, explicit instruction writes, corrections, deletion, feedback and administration |
| `retrieval/` | Entry points, scope primitives, scoring helpers, ANN access, authoritative records/evidence/history and context assembly |
| `runtime/` | Configuration, engine, reference time, tenant registry, model clients and the shared index worker |
| `memory-store` crate | SurrealKV schema, transactions, integrity constraints and durable index tasks (`crates/memory-store`); re-exported by `memory-service` as `storage` |
| `memory-domain` crate | Shared records, request schemas, errors and pure integrity planners, with no I/O (`crates/memory-domain`); re-exported by `memory-service` as `domain` |
| `diagnostics/` | Offline database, algorithm-isolation and end-to-end selftests |

Root Rust module names for moved stages are retained as compatibility reexports. Algorithm implementations
use their own stage modules and shared components directly. `memory-rules` owns deterministic planners
and Mem0 identity/ranking rules; `memory-fulltext` owns lexical/entity indexing.

Tenant startup launches the shared index worker and calls only the selected algorithm's `start_workers`.
Mem0 starts one ordered formation worker. Memos starts its fact/session formation worker and periodic
maintenance worker. Only Memos context bootstrap closes earlier open sessions. `/admin/run_jobs`
is Memos maintenance and returns an argument error in Mem0 mode.
Explicit instructions, record reads/corrections/deletion, context assembly and reindexing remain shared.

The preserved `memos` strategy commits raw turns, idempotency receipts and index tasks before returning. Background workers form facts on the batching schedule, then generate experiences and the summary only after session end. These stages use independent transactions and typed records in the same store. Sources remain retained until session-end processing succeeds, then eligible buffers can be purged. Index queues and usage records survive process restart.

The `memos` retrieval strategy combines vector, BM25, entity and time channels; it checks visibility and update chains against stored records, reranks candidates locally, then assembles bounded context. Profiles and recent turns are handled separately. Corrections append records; exact deduplication preserves both source bodies and evidence.

This release supports one local user. Evaluation tenants route isolated stores; HTTP caller headers do not authenticate identity. The daemon therefore accepts only loopback bindings.

## Integrity and retrieval improvements

The current design remains a text-memory service. The following changes adapt SurrealDB Agent Memory's separation of instructions, uncertainty and derived knowledge without replacing formation with an entity/attribute graph.

| Problem | Implementation | Verification |
|---|---|---|
| Behavioural requirements depend on factual similarity retrieval | Explicit `memoryType:instruction` text writes are stored verbatim, with evidence and optional global/project/platform scope. `get_context` selects them directly in a separately labelled, budgeted section. Ordinary extracted facts cannot become instructions. | Scope, malformed input, idempotency and zero-generation checks |
| A corrected fact can leave a current profile or skill referring to an obsolete value | Application-built `derivation` tracks input memory/turn IDs and whether the output is a current view. The storage transaction computes transitive invalidation with pure rules and marks affected views `stale`, queues index removal, and preserves their evidence. | Multi-hop invalidation, cycles, transaction rollback, deletion and restart repair |
| A conflict status does not describe the outstanding question or its resolution | Same-scope conflicts create stable `uncertainties` records referring to both assertions. Search returns visible open questions; explicit confirmation resolves a question and retires the rejected assertion atomically. | Materialisation, explicit two-assertion confirmation and repeated-resolution rejection |
| Repeated queries re-read unchanged active metadata and re-tokenise recurring passages | A per-store metadata snapshot is cleared before every memory commit. A bounded model-server-specific token cache preserves the exact token IDs and truncation algorithm. Neither cache stores ranked answers or changes scores/thresholds. | Snapshot reuse/invalidation, exact retrieval-context comparison, sequential real-model latency replay |

`profile` and `skill` are current views. Session experiences are historical observations: their lineage remains queryable when later facts change. Treating every historical experience as a current factual assertion would erase useful history. Legacy profiles/skills use `usedMemoryIds` as their dependencies; startup repairs missing or non-current inputs before warming indexes. Stale views are excluded immediately through primary-record eligibility, including while an asynchronous index still contains them. Rebuilding a view is explicit P3/P5 work; a read never triggers paid synthesis.

Storage serialises integrity decisions and related writes per store. A source change, transitive invalidation, uncertainty closure and index tasks commit as one transaction. Failed transactions do not publish a new metadata snapshot. The invalidation and conflict-repair rules are pure planners in `memory-domain/src/integrity.rs` with no database, network or model dependencies; `memory-store/src/integrity.rs` only loads their inputs and appends their plans to the transaction. This adds O(N) metadata inspection to a source-retirement transaction; it removes repeated scans from stable retrieval but does not claim to be a large-scale graph implementation.

The tokenizer cache holds at most 2048 entries and only caches entries of up to 4096 tokens. Keys include the model server and exact input text. Models remain fixed for a connected runtime; reconnect/restart replaces the cache. Model IDs and quantisation continue to govern stored-vector rebuilds.

### Evaluation contract

Use the first eight original-index questions in each LoCoMo category 1–4 from conv-26, chosen before score inspection. Copy the same historical primary-store snapshot for both the preserved pre-change binary and the new binary. Rebuild only vector indexes for the current local BGE Q8_0 runtime, then perform sequential retrieval with unchanged topK=10, tokenBudget=2000, full query, thresholds and reference date. Record one cold and two warm passes separately; never mix cache-hit timing with cold timing.

This is a retained-snapshot retrieval experiment, not a new end-to-end formation benchmark and not a physically cleared-buffer main setting. The source snapshot was saved after the historical retention attempt, so historical buffer QA is a labelled reference rather than an identical corpus control. Historical Mem0 answers are compared on the same question IDs. Reuse an existing paid answer only if the complete rendered answer prompt fingerprint matches. Preserve per-query responses, corpus hashes, models, source/binary fingerprints and usage in ignored local results. LoCoMo QA cannot establish instruction handling or correction propagation; those behaviours need separate offline integration checks.

Build changes with `make build` (`--profile fast`), then run `make test-rust test-service selftest test smoke`. No automatic verification performs paid calls or downloads/evaluates the full dataset. The requested 32-question answer/judge experiment is separate from verification.

### Evidence-led recall

Search reranks assertions together with a bounded verbatim evidence excerpt and, when recorded by the human-human adapter, its source question. A low cross-encoder score alone no longer erases a candidate supported by the existing vector/term gate; numeric defaults remain unchanged. The most relevant topK records are selected before MMR reorders them, so diversity cannot introduce a low-relevance record at the expense of supporting evidence. Query-specific facts and requested evidence use the token budget before broad profiles; `get_context` retains its bootstrap purpose. Evidence responses separate source timestamps from storage/effective timestamps. The evaluation formatter preserves that distinction, emits facts and source quotes before summaries, and counts complete context records with one shared tokenizer.

## Additive strategy

For the complete Chinese design report, request lifecycle diagrams, code-reading map and full LoCoMo evidence, see [新方案设计与实现说明](design/mem0-strategy-design.md).

The default `mem0` strategy selects paired formation/retrieval modules. `algorithms/mem0/formation.rs` owns durable
ordered inputs, the pinned one-call additive extraction contract, scoped content identity and batch
vector admission. `algorithms/mem0/retrieval.rs` owns semantic search and explicitly enabled reranking. Pure ranking
and content identity live in `memory-rules::mem0`; generic transactions and vector reads remain in storage.
The Memos algorithm remains selectable as `memos`, with its numeric defaults unchanged.

Primary memories, embeddings, batch provenance, history, index tasks and the `indexing` checkpoint
commit atomically. Vector-cache publication precedes `completed/searchable` status. A crash at the
checkpoint republishes saved vectors without extraction. The lexical worker recognizes preindexed
admission tasks, while ordinary correction/rebuild tasks still regenerate vectors. No read forms memories.

Mem0-mode jobs retain full raw input in `requests`; `mem0_sources` explicitly declares batch provenance.
The model cannot assign visibility scopes or database IDs, and inferred text is not called a verified quote.
Automatic P2 and periodic LLM maintenance do not run in this strategy. Legacy source/formation records are
never silently reinterpreted. A small startup check rejects data directories containing unfinished jobs from the other algorithm.
Demo runs use a separate data directory for each algorithm.
