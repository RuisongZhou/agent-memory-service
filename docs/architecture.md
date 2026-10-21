# Architecture

This document describes the current Mem0 implementation. Public request and configuration contracts
are defined in [API and configuration](api.md); installation is in [deployment](deployment.md).

## Implementation basis and boundaries

The algorithm follows the **Node/TypeScript OSS `mem0ai@3.3.1`** built-in NLP fallback, without
optional `natural` or `compromise`. It does not claim equivalence with every SDK API or NLP installation.
The service keeps its own SurrealKV storage, scopes, durable jobs, strict validation and provenance.
Copied code and prompts retain their [licenses and notices](../THIRD_PARTY.md).

Pinned upstream references are the [memory implementation](https://github.com/mem0ai/mem0/blob/ts-v3.3.1/mem0-ts/src/oss/src/memory/index.ts),
[prompt](https://github.com/mem0ai/mem0/blob/ts-v3.3.1/mem0-ts/src/oss/src/prompts/index.ts),
[entity extraction](https://github.com/mem0ai/mem0/blob/ts-v3.3.1/mem0-ts/src/oss/src/utils/entity_extraction.ts),
[scoring](https://github.com/mem0ai/mem0/blob/ts-v3.3.1/mem0-ts/src/oss/src/utils/scoring.ts),
[lemmatization](https://github.com/mem0ai/mem0/blob/ts-v3.3.1/mem0-ts/src/oss/src/utils/lemmatization.ts)
and [SQLite vector/BM25 store](https://github.com/mem0ai/mem0/blob/ts-v3.3.1/mem0-ts/src/oss/src/vector_stores/memory.ts).

## Layers and ownership

| Crate | Responsibility | Workspace dependencies |
|---|---|---|
| `memory-rules` | Pure message formatting, identity, entity, scoring, time and dependency rules | none |
| `memory-fulltext` | Lexical and entity indexing | none |
| `memory-config` | Startup configuration and validation | none |
| `memory-domain` | Records, request types, errors and pure integrity planners | rules |
| `memory-store` | SurrealDB transactions, repositories, revisions and integrity glue | domain, rules |
| `memory-engine` | Tenants, workers, model clients, context and application operations | config, domain, store, rules, fulltext |
| `memory-algo-mem0` | Formation, search, entity projection and ordered worker | engine and its dependencies |
| `memory-service` | HTTP/MCP, composition, diagnostics and daemon | service layers |

The engine holds a `MemoryAlgorithm` interface; only the service composition root selects the concrete
Mem0 implementation. This keeps runtime and storage independent of algorithm logic despite having one
strategy. The [architecture checker](../scripts/check_architecture.py) enforces crate dependencies,
SQL placement and the service/evaluation boundary.

## Write path

1. The transport parses the request; application admission validates scope and routes explicit
   instructions to their synchronous path. Mem0 validates message roles, timestamp order and input shape.
2. Under the ingest lock, formation checks the platform/turn idempotency key and commits the complete
   input, sequence number and `queued` request. Asynchronous acknowledgement confirms this commit.
3. One ordered worker per tenant takes the formation lock. It prepares prior messages and retrieves
   up to ten visible existing facts using **pure vector recall**, not the public hybrid search path.
4. The prompt combines the vendored additive template, historical messages, existing facts with
   temporary IDs, full new messages, observation/current dates and optional custom instructions.
   The `zh` mode adds an instruction to match the new messages' language and script.
5. Generation returns a `memory` array. Strict local validation checks sequential string IDs, nonempty
   text, attribution and links to supplied existing IDs. Malformed output receives bounded repair;
   raw provider responses are persisted before parsing. The service assigns database IDs and scope.
6. A project-scoped BLAKE3 content key separates raw and inferred writes. Duplicate text reuses an active
   memory and extends batch provenance. This is exact content deduplication, not semantic deduplication.
7. New fact vectors are embedded in a batch. Primary memories, vectors, source batches, history,
   index tasks and the `indexing` checkpoint commit atomically.
8. Entity projection is reconciled, vectors are published to the query cache and the request becomes
   `completed` with `formed:true` and `searchable:true`. Interrupted publication resumes from saved
   vectors and projection state without repeating the extraction call.

`infer:false` skips generation and entity extraction, storing nonempty message text as raw facts.
Explicit instructions use the application path instead of this extraction pipeline. Correction and
deletion are explicit operations; additive formation does not automatically rewrite older assertions.

### Three distinct text views

| View | Rule |
|---|---|
| Stored input and current formation batch | Full original messages; no old 8 KB head/tail clipping |
| Historical prompt context | Latest ten messages from preceding formed, inferred requests in the same tenant/platform/project/session; at most 300 UTF-16 units per message |
| Embedding/reranking input | Model adapter token limit; truncating the model input does not alter stored text |

Historical eligibility requires `formed:true` and status `indexing` or `completed`; raw and failed jobs
are excluded. Successful zero-memory extraction still contributes history. Ordering uses input sequence
and message-array order. UTF-16 clipping keeps complete characters if the boundary falls inside an emoji.
New messages are joined as role/content lines without an extra trailing newline. Observation date uses
the input's timestamp rather than replacing a historical event date with today's date.

## Read path

1. Validate the query, scope, requested types, topK and budget; embed the query.
2. Recall a semantic candidate pool of `max(4 × topK, 60)` using exact search or HNSW.
3. Compute BM25 over the complete visible document corpus, not just semantic candidates. The built-in
   fallback uses stop-word/simple-stem processing, k1=1.5, b=0.75 and query-length-dependent normalization.
4. Reconcile entity projection before scoring. Use the first eight query entities before deduplication,
   up to 500 entity matches per query entity, and a cosine threshold of 0.5. Each memory takes its
   strongest entity contribution; heavily linked entities receive a smaller boost.
5. Apply the semantic threshold, fuse scores, sort and select topK. Keyword/entity scores do not add
   memories absent from the semantic candidate pool.
6. Optionally rerank these topK records. Preserve the hybrid `score`, add `rerankScore` when available,
   and keep hybrid ordering if the optional reranker is absent or fails.
7. Reread authoritative primary records, filter retired or out-of-scope candidates and pack complete
   records into the token budget. Retrieval never calls the generation model.

```text
entityBoost = similarity × 0.5 / (1 + 0.001 × (linkedCount - 1)²)
score = min((semantic + normalizedBM25 + entityBoost)
            / (1 + hasBM25 + 0.5 × hasEntity), 1)
```

`hasBM25` and `hasEntity` describe whether each complete score map is nonempty, not whether a single
candidate has a match. Entity cosine uses f64 accumulation at the 0.5/0.95 boundaries; public scores
retain the service's f32 representation. No thresholds are selected from benchmark scores.

## Entity projection and storage

| Private table | Purpose |
|---|---|
| `mem0_hashes` | Exact content identity and active memory mapping |
| `mem0_sources` | Complete input-request references and batch provenance |
| `mem0_history` | Formation history |
| `mem0_entities` | Canonical text/type/scope, vector, linked memory IDs and rule version |
| `mem0_entity_state` | Rules/model versions, primary fingerprint and inferred eligibility |
| `mem0_entity_inputs` | Reusable per-fact entity extraction results |

Entity extraction copies/adapts the built-in PROPER, QUOTED, TOPIC and IDENTIFIER rules. Canonicalization
first matches normalized text exactly, then existing visible entities at cosine ≥0.95. Newly proposed
entities in one batch do not semantically merge with each other. Formation position preserves batch
order; older records without it use stable ID order, which cannot recover lost original ordering.

Primary records are durable; entity/vector/full-text indexes are rebuildable. Projection rows and their
state marker publish atomically after checking the dependency revision under the store write lock.
Failed projection work leaves the request at `indexing`, never at a false completed checkpoint.

A bounded revision journal supplies changed memory IDs. Warm queries compare in-memory revisions and
reuse tenant-owned typed entity caches. Ordinary changes point-read affected facts and persist a delta;
canonical entity merges still replay cached history in memory, so this is not an O(new records) graph
update. Restart, an expired journal, opaque writes or projection changes trigger authoritative recovery.
Malformed intermediate caches are rebuilt. Correction/deletion removes obsolete links and extraction
cache entries; maintenance may call the embedding model but never generation.

Older Mem0 records are backfilled only when formed source provenance or a correction predecessor proves
inferred eligibility. Unverified records remain available to semantic/BM25 retrieval without being
mislabelled as inferred. Entity-count checks detect missing rows, not arbitrary external corruption.

Shared integrity rules retain historical record compatibility: retiring a source invalidates dependent
current views, repairs conflict references and queues index changes in the same transaction. Historical
experiences keep their lineage. Main does not form profiles, skills or session experiences, and no read
regenerates them. Startup rejects unfinished legacy formation work; see [compatibility](api.md#compatibility).

## Code map and evidence

| Question | Entry point |
|---|---|
| Where are HTTP requests dispatched? | [api/http.rs](../crates/memory-service/src/api/http.rs) |
| How are inputs admitted and instructions routed? | [app/ingest.rs](../crates/memory-engine/src/app/ingest.rs), [app/admission.rs](../crates/memory-engine/src/app/admission.rs) |
| How are inputs queued, extracted and published? | [formation.rs](../crates/memory-algo-mem0/src/formation.rs), [runtime.rs](../crates/memory-algo-mem0/src/runtime.rs) |
| Where are candidate and hybrid scoring rules? | [retrieval.rs](../crates/memory-algo-mem0/src/retrieval.rs), [mem0_hybrid.rs](../crates/memory-rules/src/mem0_hybrid.rs) |
| How does entity recovery work? | [entities.rs](../crates/memory-algo-mem0/src/entities.rs), [repo.rs](../crates/memory-algo-mem0/src/repo.rs) |
| How is bootstrap context assembled? | [context.rs](../crates/memory-engine/src/retrieval/context.rs) |
| Where do transaction integrity and revision checks live? | [memory-store](../crates/memory-store/src/), [domain integrity](../crates/memory-domain/src/integrity.rs) |

The [evaluation guide](../eval/README.md) owns SDK comparison commands and evidence locations.
Its deterministic SDK comparison tests processing/score equivalence, not real-model answer quality or
throughput. Historical benchmark scores retain their original version and protocol. The normal offline
validation workflow is maintained in [CONTRIBUTING](../CONTRIBUTING.md#checks).
