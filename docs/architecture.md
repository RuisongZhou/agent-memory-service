# Architecture

This document describes the current Mem0 implementation. Public request and configuration contracts
are defined in [API and configuration](api.md); installation is in [deployment](deployment.md).

## Implementation basis and boundaries

The algorithm follows the **Node/TypeScript OSS `mem0ai@3.3.1`** built-in NLP fallback, without
optional `natural` or `compromise`. It does not claim equivalence with every SDK API or NLP installation.
The service keeps its own SurrealDB storage, scopes, durable jobs, strict validation and provenance.
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
| `memory-config` | Startup configuration and validation | none |
| `memory-domain` | Records, request types, errors and pure integrity and consolidation planners | rules |
| `memory-store` | The store's SurrealDB server and RPC client, transactions, repositories, revisions and integrity glue | domain, rules |
| `memory-engine` | The store, workers, model clients, context and application operations | config, domain, store, rules |
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
3. One ordered worker takes the formation lock. It prepares prior messages and retrieves
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
Opt-in [consolidation](#consolidation) proposes such rewrites and applies them only on request.

### Three distinct text views

| View | Rule |
|---|---|
| Stored input and current formation batch | Full original messages; no old 8 KB head/tail clipping |
| Historical prompt context | Latest ten messages from preceding formed, inferred requests in the same platform/project/session; at most 300 UTF-16 units per message |
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

The database is the user's own SurrealDB 3.x; no part of it is compiled into the service. Opening a store
checks `surreal version` (3.2 or a later 3.x), starts `surreal start` on the store's SurrealKV datastore under
a shell watchdog, on a random loopback port, signs in as root with the datastore's generated password and
selects (in a new datastore, creates) the `memory` namespace and `main` database. Queries are SurrealQL sent as SurrealDB RPC
`query` calls over HTTP with CBOR bodies, the encoding SurrealDB's SDKs use; its JSON format would turn
strings that look like datetimes or record IDs into those types. Each statement batch, including a
`BEGIN … COMMIT` transaction, is one call, so a failed statement still rolls the whole transaction back.
The watchdog stops the server when the store shuts down or the daemon exits however it exits, and a lock
next to the datastore keeps a second daemon out.

Primary records are durable; entity and vector indexes are rebuildable. Projection rows and their
state marker publish atomically after checking the dependency revision under the store write lock.
Failed projection work leaves the request at `indexing`, never at a false completed checkpoint.

A bounded revision journal supplies changed memory IDs. Warm queries compare in-memory revisions and
reuse store-owned typed entity caches. Ordinary changes point-read affected facts and persist a delta;
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
facts, and applies a reviewed run on request. It deviates from the pinned Mem0 algorithm, so proposing
and applying are off unless the service configuration sets `consolidationEnabled:true`, there is no
scheduler, and evaluation protocols under `eval/` never enable it. Proposing writes the run record,
proposals and model logs, never a memory record.

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

### Apply and rollback

`POST /admin/consolidation_apply {"runId":"…"}` (`agent-memoryd consolidate --apply RUN`) applies a
`proposed` run and `POST /admin/consolidation_rollback` (`--rollback RUN`) undoes an applied one. Both hold
the formation lock and commit one transaction per op, containing the op's record changes and its
proposal's bookkeeping. Apply needs `consolidationEnabled`; rollback does not, because it is the recovery
path. The record changes are pure plans in
[domain consolidation](../crates/memory-domain/src/consolidation/apply.rs): field patches that keep the
values they replaced. The algorithm adds its private tables.

- **Recheck.** Apply processes `proposed` ops in proposal order. An op is `skipped` (with `skipReason`)
  when an input's content key differs from the one recorded at proposal time, an input is no longer a
  current candidate (superseded, disputed, deleted), the op no longer validates, or a supersede's `new`
  already replaces another record. The snapshot revision is audit data: any write advances it.
- **Logical ops only.** Every retirement is a status flip plus a link. Apply never calls the physical
  `delete_memory`, so rollback is a logical inverse that keeps unrelated later writes and works across
  any number of consolidation generations.
- **Merge.** A new fact M holds the merged text with `derivation {stage:"consolidation",
  inputMemoryIds, inputTurnIds, currentView:false}`, `supersedes` unset and `proposal {runId,
  proposalId}`. Each input becomes `superseded` with `supersededBy: M` and no `validUntil`, since a merge
  is not a change of value. `currentView:true` would be wrong here: `current_dependency` in
  [domain integrity](../crates/memory-domain/src/integrity.rs) stales a current view whose inputs are
  superseded, and they are superseded in the same transaction. `supersedes` holds one predecessor, so N→1
  lineage lives in the derivation.
  - Origin is `assistant` if any input is, otherwise `imported` if any input is, otherwise `inferred` if
    any input is, otherwise `user`. This keeps `get_context` from presenting a suggestion or a copied
    note as established.
  - `statedAt` is the latest input's. `ingestionMode` is `inferred` and `contentHash` is M's own key.
  - The indexer embeds M (an embedding call, not generation).
  - `mem0_sources.inputRequestIds` is the union of the inputs' requests in input order. Formation's
    deduplication requires that row, and it keeps M eligible for the entity projection.
- **Content keys.** Mem0 deduplication reuses a `mem0_hashes` target only while that target is
  `current`; otherwise it forms a new fact and overwrites the row. A merge therefore points at M every
  key that currently resolves to one of its inputs, including keys an earlier merge redirected to an
  input. M's own key is included too, unless another current record already owns it. The proposal records
  each key's previous target. Re-extracting an input's text then extends M's sources instead of
  re-creating the duplicate. Only merges redirect keys: a supersede, a conflict or a user retraction
  leaves them alone.
- **Supersede.** `old` becomes `superseded` with `supersededBy: new`, and `validUntil` is the new value's
  effective time, or its statement time when that is unknown, as for Mem0 facts. `new.supersedes = old`.
- **Conflict.** A contradiction is never auto-picked. Apply opens one `uncertainties` row (sorted ids,
  project scope, reason) and sets the members to `conflict`, which is what `resolve_uncertainty` checks.
  It writes no `related` edges, because the startup repair opens a separate question for every
  `conflicts` edge pair.
- **Rollback** runs over `applied` ops, newest first. An op is undone only while every field it wrote
  still holds the written value and no other open question names its records. M must also be current,
  and the question must still be open (it becomes `withdrawn`, not `resolved`).
  - Undoing a merge sets M to `superseded`, marks it `consolidationRollback`, returns the inputs to
    `current` and points each key back at its previous target unless deduplication has moved it since.
  - Requests deduplicated into M while it was applied stay in M's sources.
  - An op whose result a person or a later run changed stays `applied` with `rollbackSkipReason`. That
    covers a corrected, deleted or consumed M, a resolved question, or a moved link. The run is then
    `partially_rolled_back`, and rollback can be retried after the later run is rolled back.
  - A person's decision is never undone.
- **Deletion.** A merge restates its inputs, so `delete_memory` follows `derivation.inputMemoryIds`
  separately from the `supersedes` chain, and relinks a predecessor only through a successor whose
  `supersedes` names the deleted record.
  - Deleting M leaves its inputs `superseded`, clearing their `supersededBy`, just as deleting a chain
    head revives nothing. Its rollback is then skipped.
  - Deleting one of M's inputs deletes M as well, transitively, as profiles and skills are, because M's
    text holds the deleted content. M's other inputs return to `current`, and their keys point back at
    them, since their own texts never held it.
  - Keys left pointing at a deleted M behave like any deleted fact's keys: a re-extraction forms a new fact.
- **Integrity.** Superseding inputs stales legacy current views (profiles, skills) built on them through
  the existing rules. M is a historical restatement and is not staled by its own inputs.

Run statuses: `proposed` → `applying` → `applied` → `rolling_back` → `rolled_back` or
`partially_rolled_back`. An interrupted apply or rollback records `error` and resumes from its op
statuses. Op statuses: `proposed`, `rejected`, `aborted` or `failed` at proposal; `applied` or `skipped`
at apply; `rolled_back` at rollback. History rows record each change in `mem0_history`.
Runs are started by hand only; there is no scheduler.

## Import

`agent-memoryd import` copies the memories a host already keeps (Claude Code auto memory, Codex rollout
summaries, WorkBuddy memory files, JSONL a host's agent exported, and past Claude Code and Codex sessions) into the
store; the
[design](design/memory-import.md) explains the choices and the [harness guide](mcp-harness.md#import-existing-memories)
the commands. Like consolidation it proposes first and changes records only on request, and evaluation
protocols never call it.

1. **Plan, in the client.** Host adapters only read files; a host-agnostic planner turns each section into a
   unit, redacts credentials, and picks its mode from the unit cap: the record cost (`est_tokens + 8`) may be at
   most half the recall budget (default 400), so every imported record fits automatic recall beside another.
   A unit that fits stays one record (`verbatim`); a longer one is extracted, or split deterministically
   with `--no-extract`. Codex sections are always extracted, and an extraction input over 8 KB is cut into parts.
2. **Propose.** Units arrive in batches of at most 64 on a `proposing` run. A unit whose hash matches its
   ledger row (`import_ledger`, one per project and unit) is `unchanged`. An extract unit costs one generation
   call ([the engine's import prompt](../crates/memory-engine/src/app/import/extract.txt), shown the project's ten
   nearest facts so known content is left out), cached in
   `import_extractions` by project, unit text, prompt and model. Every item is checked by pure rules in
   [memory-rules](../crates/memory-rules/src/import/validate.rs): its `evidence` must quote the unit, its numbers and
   named entities must occur in the unit or its metadata, and it must fit the cap; a failing item is stored as
   `rejected` with the reason. Proposals are rows of the shared `proposals` table with `kind: "import"`; nothing
   is written to `memories`, and no lock is held during model calls.
3. **Apply**, under the formation lock, one proposal per transaction. A declined proposal is `dropped`. An accepted
   fact becomes a `fact` with origin `imported`, project scope, the source's `statedAt`, `ingestionMode:
   imported`, `detail.import` provenance and an `evidence` row; the indexer embeds it. A current record of the
   same text is linked instead of duplicated, and an instruction a person accepted becomes an explicit
   instruction (global only when the person chose so). A ledger row holds the unit's live links, carried from
   import to import: a changed one-record unit's new fact, and its new accepted instruction, supersede the ones
   its earlier import wrote, unless another unit's row still holds them. A unit of which nothing was applied gets
   a `declined` row that keeps its earlier links, so the same text is not offered again unless proposed with
   `reconsider`.
4. **Rollback** inverts applied proposals newest first: a written record becomes `superseded` with
   `importRollback`, the record it replaced returns to `current` (also when a link replaced it), and ledger rows
   return to the rows they replaced. A record a person or a later run changed, or another unit holds, stays,
   with `rollbackSkipReason`.

Past session turns (`--include sessions`, Claude Code and Codex) take another path. Transcript adapters keep only
the turns a person asked (by the host's own provenance fields), and each turn keeps the turn ID capture gives it.
Propose registers a turn as a row of the run without a model call; apply marks it queued, and the client puts it
into the dialogue outbox with the run's `importRunId`. The uploader submits it like a captured turn: the daemon
admits it only as a turn the run applied, deduplicates it by `platform + turnId`, and Mem0 formation forms it with
the session's earlier turns as context. The request records the run, so rollback, after closing the run to further
turns under an import gate, retires only the facts whose every source request is the run's own.

Import is a shared use case, not part of an algorithm: the engine owns its prompt and its record writes, and asks the
algorithm two generic questions through `MemoryAlgorithm`: `nearest_facts`, the facts formation would recall for a
text (the extraction context and the reviewer's `similar`), and `source_requests`, the requests a fact was formed
from (rolling back session turns). An algorithm's `add_memories` keeps `importRunId` on the request row it commits.
Import writes no algorithm history; its runs' proposals record what each one wrote, superseded and rolled back. The
[architecture checker](../scripts/check_architecture.py) keeps import modules out of algorithm crates.

`imported` is not in `context_origins`, so `get_context` never presents imported facts as established; search
and recall find them. They are consolidation candidates, and a merge with an imported input is never `user`.
The entity projection leaves them out: they are not formation output. Run statuses are `proposing`, `proposed`,
`applying`, `applied`, `rolling_back`, `rolled_back` and `partially_rolled_back`; an interrupted apply or rollback
records `error` and resumes.

## Code map and evidence

| Question | Entry point |
|---|---|
| Where are HTTP requests dispatched? | [api/http.rs](../crates/memory-service/src/api/http.rs) |
| How are inputs admitted and instructions routed? | [app/ingest.rs](../crates/memory-engine/src/app/ingest.rs) |
| How are inputs queued, extracted and published? | [formation.rs](../crates/memory-algo-mem0/src/formation.rs), [runtime.rs](../crates/memory-algo-mem0/src/runtime.rs) |
| Where are candidate and hybrid scoring rules? | [retrieval.rs](../crates/memory-algo-mem0/src/retrieval.rs), [mem0_hybrid.rs](../crates/memory-rules/src/mem0_hybrid.rs) |
| How does entity recovery work? | [entities.rs](../crates/memory-algo-mem0/src/entities.rs), [repo.rs](../crates/memory-algo-mem0/src/repo.rs) |
| How is bootstrap context assembled? | [context.rs](../crates/memory-engine/src/retrieval/context.rs) |
| Where do transaction integrity and revision checks live? | [memory-store](../crates/memory-store/src/), [domain integrity](../crates/memory-domain/src/integrity.rs) |
| How is SurrealDB started, stopped and queried? | [server and watchdog](../crates/memory-store/src/surreal/server.rs), [RPC client](../crates/memory-store/src/surreal/mod.rs), [CBOR values](../crates/memory-store/src/surreal/cbor.rs) |
| How are consolidation proposals selected, generated and validated? | [rules](../crates/memory-rules/src/consolidation.rs), [domain](../crates/memory-domain/src/consolidation.rs), [Mem0 prompt and provenance](../crates/memory-algo-mem0/src/consolidation.rs), [run storage](../crates/memory-engine/src/app/consolidation.rs) |
| How are runs applied, rolled back and deleted? | [domain plans](../crates/memory-domain/src/consolidation/apply.rs), [Mem0 keys and sources](../crates/memory-algo-mem0/src/consolidation/apply.rs), [run use cases](../crates/memory-engine/src/app/consolidation.rs), [deletion](../crates/memory-engine/src/app/management/deletion.rs) |
| How are a host's existing memories imported? | [host adapters and planner](../crates/memory-service/src/harness/import/), [pure rules](../crates/memory-rules/src/import/), [wire types and record plans](../crates/memory-domain/src/import.rs), [use cases](../crates/memory-engine/src/app/import/), [session turns](../crates/memory-engine/src/app/import/turns.rs), [extraction](../crates/memory-engine/src/app/import/extract.rs) |

The [evaluation guide](../eval/README.md) owns SDK comparison commands and evidence locations.
Its deterministic SDK comparison tests processing/score equivalence, not real-model answer quality or
throughput. Historical benchmark scores retain their original version and protocol. The normal offline
validation workflow is maintained in [CONTRIBUTING](../CONTRIBUTING.md#checks).
