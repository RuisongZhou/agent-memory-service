# API and configuration

All service calls are JSON `POST /v1/<interface>` requests. `X-Memory-Platform` identifies the calling adapter; `X-Memory-Tenant` defaults to `default` and is primarily for evaluation isolation. Never expose these headers as authorization claims.

| Interface | Purpose |
|---|---|
| `add_memories` | Write an ordered message batch or explicit text; receive a durable request id |
| `get_task` | Poll request status before treating extraction as complete |
| `search_memories` | Return bounded fact memories |
| `get_context` | Compose current context for a project/session |
| `list_turns` | Inspect complete submitted message batches |
| `get_memory` | Read a memory with optional history and evidence |
| `correct_memory` | Append a correction to a current record |
| `delete_memory` | Delete a record and repair references/indexes |
| `feedback` | Record deduplicated feedback for an actual retrieval |

`GET /health` reports backend, actual device, dtype, loaded tenants and LLM usage. Admin actions under `/admin/` include stats, read-only exports, index rebuild and wait-idle. `POST /admin/export` accepts `table` values `llm_usage`, `extract_log`, `retrieval_log`, and `proposals`, all scoped to the calling tenant. Seed is enabled only in `MEMORY_MOCK_MODELS=1` mode.

`POST /v1/embeddings` accepts a string or a nonempty array of at most 128 strings. Non-string array elements are rejected; token usage is an approximation, not provider billing.

## MCP and harness integration

The stdio bridge exposes `add_memories`, `search_memories`, `get_context`, `get_task`, `list_turns`,
`get_memory`, `correct_memory`, and `feedback`, forwarding their JSON arguments to the matching HTTP
endpoint. Launch it with `agent-memoryd mcp --url http://127.0.0.1:8765 --platform <harness>` after
starting the daemon. HTTP failures become MCP tool results with `isError:true`; success content is
JSON text. There is no Streamable HTTP MCP endpoint.

See [MCP/harness integration](mcp-harness.md) for parameter/result examples, the Mem0
`list_turns` shape, Codex/Claude Code configuration and automatic dialogue capture through a durable
local outbox. MCP registration alone does not capture conversations or inject bootstrap context.

## Startup

Use `agent-memoryd serve --help` for paths, model selection, host/port, LLM and concurrency settings. `--config examples/service-config.json` overrides algorithm tunables. CLI/environment settings override config-file paths, prompt variant and LLM connection settings. Unknown JSON fields and invalid score/concurrency values are rejected before model servers are contacted or stores load. Startup waits up to 60 s for the embedding endpoint and any enabled reranker and fails if either is unreachable (the reranker is skipped with `--no-reranker`). Inputs are truncated to 512 tokens and rerank logits are mapped to 0..1 with a sigmoid. Stored vectors are tagged with the embedding model id (`<alias>-<GGUF type>`); a different model or quantisation triggers re-embedding on the next start.

Configuration environment variables include `MEMORY_DATA_DIR`, `MEMORY_EMBED_URL` (default `http://127.0.0.1:8081`), `MEMORY_RERANK_URL` (default `http://127.0.0.1:8082`), `MEMORY_PROMPT_VARIANT`, `MEMORY_TENANT` (MCP), and `MEMORY_LLM_MAX_CALLS`. `QWEN_API_KEY` is supplied separately; the Python launcher reads this key from `.env` and supplies command flags for other settings.

Strategy selection follows explicit `--memory-strategy`, then `MEMORY_STRATEGY`, then the config file's `memoryStrategy`, then the default `mem0`. The Python launcher preserves this order; omitting a CLI strategy does not override the config file.

`agent-memoryd reindex --all` or `--vector`/`--fulltext` selects work explicitly. HTTP errors produce a nonzero CLI exit status.

## Minimal search

```sh
curl -s http://127.0.0.1:8765/v1/search_memories \
  -H 'Content-Type: application/json' -H 'X-Memory-Platform: demo' \
  -d '{"query":"current project decisions","scope":{"project":"demo"},"touch":false}'
```

The core schemas are in `crates/memory-domain/src/types.rs`; examples live in `examples/`.

## Memory strategy

Mem0 preserves the full current `messages` batch for inference. Only the historical prompt view is
limited to the latest ten messages and 300 UTF-16 units per message, preserving whole characters. History is restricted to
the same tenant/platform/project/session and successfully formed inferred requests; raw writes and
failed formation requests are excluded. Successful zero-memory extraction still contributes context.
Long-term project facts remain available across sessions. Current messages are not clipped by a formation byte limit. See [the Node SDK 3.3.1 design](design/mem0-sdk-3.3.1-alignment.md).

`list_turns` returns complete batches that fit its `tokenBudget`; it can skip a long batch rather than
truncate it. Increase the inspection budget or inspect the stored request when auditing long inputs.

### Prompt families

The service loads `crates/memory-algo-mem0/src/prompts/additive.txt`, with its Apache license and NOTICE.
`--prompt-variant zh` appends a Simplified Chinese output instruction; `en` uses the original prompt language.
The copied assets under `eval/third_party/mem0_evaluation/` belong to SDK evaluation and are not loaded by the daemon.

Only `memoryStrategy:"mem0"` is accepted. The engine fixes its algorithm at startup for all tenants.
The previous dual-strategy code and its P1–P5 prompts live on
[`codex/mem0-memos`](https://github.com/RuisongZhou/agent-memory-service/tree/codex/mem0-memos).
Retired buffer, batching and maintenance configuration fields are rejected as unknown fields.
Existing Mem0 stores keep their records and schema. Unfinished legacy jobs cause startup to fail;
complete those jobs with the preserved branch before changing binaries, or use a fresh store.
The schema retains historical record types for reading, correction and integrity repair; it does not
run the retired formation pipeline. `/admin/run_jobs` has been removed and returns HTTP 404.

In Mem0 mode each submitted request is a durable ordered job. `messages` accepts 1–128 user/assistant
messages in timestamp order; `turnId` remains required. `text` has the existing 4 KB bound. `infer:false`
stores nonempty messages verbatim without a model call. Input messages and idempotency keys survive
failures. An extraction uses the Apache-licensed Mem0 2.2.1 additive prompt, ten preceding messages
within the project/adapter and ten semantically relevant existing facts. New facts are appended; only
explicit correction/deletion changes old assertions. Exact same-scope deduplication preserves the first
record. Raw and inferred content use separate identities. Model output has one strict `memory` array;
malformed responses receive bounded corrective retries and remain in `llm_responses`.

Vectors are generated as a batch and committed with the primary facts and an `indexing` receipt.
After cache publication the receipt becomes `completed`, `formed:true`, `searchable:true`.
Interrupted publication resumes from durable vectors without another model call. `sync:true` waits
for this visibility; an asynchronous `requestId` confirms input persistence only. `get_task` reports
these phases. `list_turns` returns complete submitted message batches. `sessionEnded` closes an ordered
input job. Automatic profile, experience, merge and skill maintenance is not part of this service.

For Mem0 `list_turns`, `since` is an inclusive RFC3339 cutoff against the batch's final message. It compares actual instants across UTC offsets, rejects invalid timestamps, and returns complete matching batches.

New provenance is labelled `sourceBatch`, `level:"batch"`, `verifiedClaimSpan:false`; it references
complete durable input requests, not a model-selected verified quote. Event dates are retained in
memory text; an unknown `effectiveAt` is empty. Observation and ingestion times are distinct.
Existing verified evidence and historical records remain intact.

Search follows Node SDK 3.3.1's semantic candidate pool plus BM25 and entity scoring, using its built-in
NLP fallback without optional natural/compromise. Defaults remain `mem0TopK:20` (`topK` overrides per request),
`mem0Threshold:0.1` (the semantic gate before fusion), and the configured token budget.
It returns whole memory records in score order, without profiles or recent messages. Retrieval makes
no generative call. `mem0Rerank:false` avoids even loading the reranker. To configure it, use
`mem0Rerank:true`; requests may override with `rerank:false/true`. Reranking applies after hybrid topK;
`score` remains the hybrid score and `rerankScore` is added when available. A missing or failed optional
reranker leaves hybrid ordering intact. Evidence reads are explicit.

Entity projection is built after inferred formation and repaired before search when primary memories
change. Correction/deletion can leave projection rows pending until the next search, which reconciles
them before scoring and rechecks authoritative memory state before returning results. Old Mem0 stores
are backfilled from verified inferred provenance; raw writes do not create entities. These paths use
local embeddings but no generative calls. Projection tables are inspectable through admin export.
`mem0MaxTokens` defaults to the SDK's 2,000 output tokens; `mem0CustomInstructions` supplies domain-specific
formation guidance. Projects remain explicit visibility namespaces; a search with project `"*"`
selects all visible project records in the single-user store. Caller headers remain routing metadata,
not authentication.

The extraction prompt is vendored unchanged with its Apache 2.0 license and NOTICE in
`crates/memory-algo-mem0/src/prompts/`. Strict parsing, scoped hash deduplication,
atomic primary/vector storage and batch provenance are application adaptations, not SDK guarantees.

## Explicit instructions

`add_memories` accepts `memoryType:"instruction"` with `text` and the usual project/session scope. This path copies text into a separate instruction record and evidence, completes its receipt synchronously and makes no generative-model call. It rejects `messages`, `sessionEnded`, empty text, more than 4 KB or more than 1024 Unicode characters. `memoryScope` optionally selects global, current project or caller platform; otherwise the instruction is project-scoped. An optional `turnId` provides a platform-specific idempotency key. A retry preserves the first payload and scope. `memoryType:"fact"` or omission retains existing fact extraction; `memoryScope` is rejected on that path.

`get_context` returns `instructions` and renders only visible, current, explicitly written instructions in a separate section. The section shares the context token budget. Generic search does not mix instructions into its fact/experience/skill result slots. Update or retract an instruction using `correct_memory`; deletion uses the existing `delete_memory` lifecycle.

### Uncertainty and derivation

Legacy conflicting records remain inspectable and repairable. The Mem0 additive extractor does not infer new conflicts. For existing `uncertainties` records, administrative inspection uses `POST /admin/export {"table":"uncertainties"}`. Explicit confirmation uses `POST /v1/resolve_uncertainty` with `{id, keepMemoryId, reason}`. The chosen record must belong to the question, and both competing records must still be conflicting in the same scope. Confirmation preserves original evidence, expires the rejected assertion and records the resolution. Other surviving questions keep the selected assertion conflicting until they are resolved too.

Retiring an assertion also closes questions involving it and removes obsolete conflict edges in the same transaction. A surviving assertion returns to `current` when its final dispute closes; any remaining open question keeps it `conflict`.

Derived records expose application-built `derivation: {stage,inputMemoryIds,inputTurnIds,currentView}`. Profiles and skills are current views; if a source is superseded, disputed, edited or deleted, they and their transitive dependents become `stale`. Their originals remain inspectable with `staleReason`; retrieval excludes them before index updates finish. Legacy current views are checked at startup. Historical experiences keep their lineage and remain observations of the original event. No read automatically regenerates a stale profile.

## Configuration reference

The shipped [configuration template](../examples/service-config.json) contains only supported settings.
Additional service tunables are `maxTopK`, `defaultTokenBudget`, `contextTokenBudget`, `bruteForceLimit`
and `useReranker`. A zero `bruteForceLimit` forces the ANN path. `promptVariant` controls formation language
and context presentation. Connection paths, endpoints, model, concurrency and timeout are declared in
`ServiceConfig`; daemon CLI/environment values override the corresponding startup options.
There are no batch-size, recent-buffer, P2–P5, RRF or MMR settings in this version.
