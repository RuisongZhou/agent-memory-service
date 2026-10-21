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
| `memory-rules` | Pure message formatting, identity, entity, scoring, time, dependency and consolidation rules | none |
| `memory-fulltext` | Lexical and entity indexing | none |
| `memory-config` | Startup configuration and validation | none |
| `memory-domain` | Records, request types, errors and pure integrity and consolidation planners | rules |
| `memory-store` | SurrealDB transactions, repositories, revisions and integrity glue | domain, rules |
| `memory-engine` | Tenants, workers, model clients, context and application operations | config, domain, store, rules, fulltext |
| `memory-algo-mem0` | Formation, search, entity projection, consolidation prompt and ordered worker | engine and its dependencies |
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
Opt-in [consolidation](#consolidation) only proposes such rewrites; nothing applies them yet.

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

## Consolidation

Mem0 formation only adds: duplicate and contradicting facts accumulate, and only `correct_memory`
builds a supersede chain. Consolidation proposes merges, supersedes and conflicts over a project's
facts. It deviates from the pinned Mem0 algorithm, so it is off unless the service configuration sets
`consolidationEnabled:true`, has no scheduler, and evaluation protocols under `eval/` never enable it.
This stage only proposes: a run writes its run record, proposals and model logs, never a memory record.

### Proposal stage

`POST /admin/consolidate {"project":"P","dryRun":true}` (or `agent-memoryd consolidate --project P
--dry-run`) runs these steps; see the [API](api.md#consolidation-proposals) for the response.

1. **Candidates.** Under the formation lock, reconcile the entity projection and read the memory revision,
   the project's current facts, open uncertainties, canonical entity rows and cached vectors. Candidates
   are `current` facts scoped to the project whose origin is not `explicit` and that no open uncertainty
   names. Instructions, raw `infer:false` text and corrections (origin `explicit`) are never candidates.
2. **Clusters, no model call.** Two candidates link when their existing vectors reach the cosine link or
   they share a canonical entity row of kind PROPER, IDENTIFIER or QUOTED (TOPIC rows are lowercase word
   runs, not entities). An entity row linked to more candidates than one cluster holds links nothing.
   Links join clusters strongest first (cosine, then shared entity, then ids) only while the result stays
   within the maximum size, so clusters are connected and bounded without splitting afterwards. Clusters
   of at least two members are ranked by their strongest link and capped per run. Pair scoring is quadratic
   in the candidates and runs off the async runtime.
3. **One generation call per cluster**, after releasing the lock. The model sees temporary ids `"0".."n"`
   in statement order with each fact's `statedAt`, and returns `{"operations":[...]}` with `merge
   {ids,text}`, `supersede {old,new}`, `conflict {ids,reason}` or `keep {ids}`; unnamed members are kept.
   The JSON contract (strict fields, string ids from the cluster, each id in at most one op) gets the
   same bounded repair as formation; raw provider responses are persisted before parsing, and an
   `extract_log` row records the parsed output with system and input hashes.
4. **Validation**, pure and without repair. Ids must be cluster members and appear in at most one op.
   Merge text must be nonempty, within the byte cap and no longer than its inputs together; every named
   entity (rule-based extractor, PROPER/IDENTIFIER/QUOTED) and every digit group of a number or date in
   it must occur in the merged inputs. A supersede's `new` must be stated strictly after `old`
   (`statedAt`, then position); same-time or undated pairs are rejected. A conflict needs a short reason.
   A failing op is stored as `rejected` with its reason; other ops of the response stand.
5. **Retire cap.** Accepted merges retire their inputs and supersedes their `old` record. After each
   cluster, a run whose retirements exceed the per-run share of the project's current facts stops as
   `aborted`. A generation failure stops it as `failed`. Ops of aborted or failed runs keep that status.
6. **Storage.** One `consolidation_runs` record (status, snapshot revision, embedding and generation
   model, prompt format and hash, candidate and cluster counts, cluster members and links, input hashes,
   response log ids, op counts, retire share and the limits below) and one `proposals` row per op
   (`runId`, cluster, op fields, `memoryIds`, status, rejection reason, response log id and algorithm
   provenance) commit in one transaction. For Mem0 the provenance holds each input's content key and,
   for merges, the union of the inputs' source requests in input order and the merged text's content key.

| Limit | Value | Reason |
|---|---|---|
| Vector link | cosine ≥ 0.85 | Selection only nominates; the model may keep every member, so a loose link costs one call, never a write |
| Cluster size | 2–8 | Enough for a duplicate family; small enough to review in one prompt |
| Clusters per run | 16 | Bounds generation calls per manual run |
| Merged text | ≤ 4096 bytes and ≤ the inputs together | Same cap as explicit text; a merge never expands |
| Conflict reason | ≤ 1024 bytes | A label for the reviewer, not new content |
| Retire share | ≤ 0.2 of the project's current facts | One run cannot rewrite most of a project |

These are fixed design limits in [memory-rules](../crates/memory-rules/src/consolidation.rs), not
configuration; none was selected from pilot or benchmark scores.

### Apply and rollback (design for the next stage)

Only `proposed` ops of a `proposed` run are applicable, and each op is rechecked first: every input still
`current` with the content key recorded in its proposal. The snapshot revision is audit data; any write
advances it, so apply does not compare it. Each op commits in its own transaction with its proposal
marked `applied` and the previous values of every field it changed.

- **Logical ops only.** Every retirement is a status flip to `superseded` plus `supersededBy`, never the
  physical `delete_memory`. Rollback is then the logical inverse of each applied op, in reverse order,
  keeps unrelated later writes and works across any number of consolidation generations.
- **Merge.** A new fact M stores the merged text with `derivation {stage:"consolidation",
  input_memory_ids, input_turn_ids, current_view:false}` and `supersedes: None`; each input becomes
  `superseded` with `supersededBy: M` and no `validUntil` (a merge is not a change of value). A
  `current_view:true` derivation would be wrong: `current_dependency` in
  [domain integrity](../crates/memory-domain/src/integrity.rs) stales a current view as soon as its inputs
  are superseded, which happens in the same transaction. `supersedes` holds one predecessor, so N→1 lineage
  lives in `derivation.input_memory_ids`. M's `mem0_sources.inputRequestIds` is the union of the inputs'
  requests ordered by input sequence, which keeps batch provenance and the entity eligibility that
  `entities::inferred` derives from formed inferred requests. M sets `ingestionMode:"inferred"`, its own
  `contentHash`, an embedding of the merged text (an embedding call, not generation) and origin
  `assistant` if any input was assistant-attributed, else `user`, so `get_context` does not present a
  suggestion as established. `statedAt` is the latest input's.
- **Content keys.** Mem0 deduplication reuses a `mem0_hashes` target only while it is `current` and
  otherwise creates a new fact and overwrites the row. A merge therefore points each input's content key,
  and M's own key, at M, so re-extracting an input's text extends M's sources instead of re-creating the
  duplicate. Only merges redirect: a supersede, a conflict or a user retraction leaves keys alone.
- **Supersede.** `old` becomes `superseded` with `supersededBy: new` and `validUntil` = the new value's
  effective time; `new.supersedes` is set only when empty, since a chain cannot fork. Hash rows are kept.
- **Conflict.** Contradictions are never auto-picked: apply opens an `uncertainties` row (sorted ids,
  project scope, reason, run id) and sets the members to `conflict` with symmetric `related: conflicts`
  edges, which is what the existing `resolve_uncertainty` requires to settle it.
- **Rollback of a merge** supersedes M (it is not deleted) and restores each input whose `supersededBy`
  is still M, re-pointing its hash row. If M was corrected, deleted or consumed by a later run, the op is
  reported and skipped (or the later run must be rolled back first); a user decision is never undone.
  Rolling back a supersede restores `old` only if it still points at `new`; rolling back a conflict
  withdraws the question only while it is open.
- **Deletion.** `delete_one` in [deletion.rs](../crates/memory-engine/src/app/management/deletion.rs)
  relinks a single predecessor/successor, and for an input whose successor is M its `(None, Some(n))`
  branch would clear `M.supersedes`, which a merge never sets. Deleting M leaves its inputs `superseded`
  (as deleting a chain head revives nothing today), clears their `supersededBy`, drops hash rows that point
  at M and marks the op so rollback skips it. Deleting an input of M deletes M transitively, as profiles and
  skills are, because M's text contains the deleted content; the other inputs return to `current` with
  their hash rows, since their own texts never contained it. Deletion has to follow
  `derivation.input_memory_ids` for merges, separately from the `supersedes` chain.
- **Integrity.** Superseding inputs stales legacy current views (profiles, skills) built on them through
  the existing rules; M itself is a historical observation and is not staled by its own inputs.
- **Scheduling** stays manual until apply and rollback have shipped.

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
| How are consolidation proposals selected, generated and validated? | [rules](../crates/memory-rules/src/consolidation.rs), [domain](../crates/memory-domain/src/consolidation.rs), [Mem0 prompt and provenance](../crates/memory-algo-mem0/src/consolidation.rs), [run storage](../crates/memory-engine/src/app/consolidation.rs) |

The [evaluation guide](../eval/README.md) owns SDK comparison commands and evidence locations.
Its deterministic SDK comparison tests processing/score equivalence, not real-model answer quality or
throughput. Historical benchmark scores retain their original version and protocol. The normal offline
validation workflow is maintained in [CONTRIBUTING](../CONTRIBUTING.md#checks).
