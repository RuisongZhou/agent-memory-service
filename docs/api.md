# API and configuration

All service calls are JSON `POST /v1/<interface>` requests. `X-Memory-Platform` identifies the calling adapter; `X-Memory-Tenant` defaults to `default` and is primarily for evaluation isolation. Never expose these headers as authorization claims.

| Interface | Purpose |
|---|---|
| `add_memories` | Write a completed user/assistant turn or explicit text; receive a durable request id |
| `get_task` | Poll request status before treating extraction as complete |
| `search_memories` | Return bounded memory, profiles and recent turns |
| `get_context` | Compose current context for a project/session |
| `list_turns` | Inspect visible recent turns |
| `get_memory` | Read a memory with optional history and evidence |
| `correct_memory` | Append a correction to a current record |
| `delete_memory` | Delete a record and repair references/indexes |
| `feedback` | Record deduplicated feedback for an actual retrieval |

`GET /health` reports backend, actual device, dtype, loaded tenants and LLM usage. Admin actions under `/admin/` include stats, read-only exports, index rebuild, maintenance and wait-idle. `POST /admin/export` accepts `table` values `llm_usage`, `extract_log`, `retrieval_log`, and `proposals`, all scoped to the calling tenant. Seed is enabled only in `MEMORY_MOCK_MODELS=1` mode.

`POST /v1/embeddings` accepts a string or a nonempty array of at most 128 strings. Non-string array elements are rejected; token usage is an approximation, not provider billing.

## MCP and harness integration

The stdio bridge exposes `add_memories`, `search_memories`, `get_context`, `get_task`, `list_turns`,
`get_memory`, `correct_memory`, and `feedback`, forwarding their JSON arguments to the matching HTTP
endpoint. Launch it with `agent-memoryd mcp --url http://127.0.0.1:8765 --platform <harness>` after
starting the daemon. HTTP failures become MCP tool results with `isError:true`; success content is
JSON text. There is no Streamable HTTP MCP endpoint.

See [MCP/harness integration](mcp-harness.md) for parameter/result examples, the different Mem0/Memos
`list_turns` shapes, Codex/Claude Code configuration and automatic dialogue capture through a durable
local outbox. MCP registration alone does not capture conversations or inject bootstrap context.

## Startup

Use `agent-memoryd serve --help` for paths, model selection, host/port, LLM and concurrency settings. `--config examples/service-config.json` overrides algorithm tunables. CLI/environment settings override config-file paths, prompt variant and LLM connection settings. Unknown JSON fields and invalid score/concurrency/batch values are rejected before model servers are contacted or stores load. Startup waits up to 60 s for the embedding endpoint and any enabled reranker and fails if either is unreachable (the reranker is skipped with `--no-reranker`). Inputs are truncated to 512 tokens and rerank logits are mapped to 0..1 with a sigmoid. Stored vectors are tagged with the embedding model id (`<alias>-<GGUF type>`); a different model or quantisation triggers re-embedding on the next start.

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

## Recent-turn buffer

`add_memories` stores both messages of a turn verbatim; only the HTTP body limit (axum's default, 2 MB) bounds them. Each turn also has a digest, the user and assistant heads within `digestMaxBytes` (default 2048 bytes). A message longer than its share of the digest is also split into overlapping windows of `digestMaxBytes / 2` bytes, and every window is embedded next to the digest vector. Short turns are stored and indexed exactly as before.

`search_memories` ranks a buffered turn by its best-matching vector. In `recentTurns`, `digest` is the turn digest or, when a window matched better, the digest with that message's head replaced by the window (omissions marked with `…`); `excerpt` then gives the window's `role` and byte `start`/`end`. `complete:false` marks a partial view: `list_turns` returns complete messages. `/admin/stats` reports `index.turnChunkVectors`.

Formation keeps its bounded view of each message: over `maxMessageBytes` (default 8 KB), prompts, source-span checks and P2 evidence use the head and tail 4 KB.

## Memory strategy

`memoryStrategy:"mem0"` is the default. `--memory-strategy memos` (or `MEMORY_STRATEGY=memos`)
selects the preserved fact/experience formation and hybrid retrieval implementation described below.
Both use the existing Rust daemon, SurrealDB store, HTTP/MCP interfaces and explicit instruction/correction APIs.
The algorithm is chosen before the daemon starts serving requests and is fixed for that process.
All tenants use that same choice. HTTP/MCP requests do not select an algorithm.
Use a separate data directory for each demo algorithm; startup rejects unfinished jobs from the other algorithm.

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
input job; it does not run P2. Automatic profile/experience/merge/skill maintenance is disabled. `/admin/run_jobs` belongs to the
Memos algorithm and returns HTTP 400 in Mem0 mode; select `memos` to run its maintenance.

For Mem0 `list_turns`, `since` is an inclusive RFC3339 cutoff against the batch's final message. It compares actual instants across UTC offsets, rejects invalid timestamps, and returns complete matching batches.

New provenance is labelled `sourceBatch`, `level:"batch"`, `verifiedClaimSpan:false`; it references
complete durable input requests, not a model-selected verified quote. Event dates are retained in
memory text; an unknown `effectiveAt` is empty. Observation and ingestion times are distinct.
Existing verified evidence and historical records remain intact.

Search defaults to semantic similarity, `mem0TopK:20` (`topK` overrides per request), `mem0Threshold:0.1`, and the configured token budget.
It returns whole memory records in score order, without profiles or recent messages. Retrieval makes
no generative call. `mem0Rerank:false` avoids even loading the reranker. To configure it, use
`mem0Rerank:true`; requests may override with `rerank:false/true`. Evidence reads are explicit.
`mem0MaxTokens` defaults to the SDK's 2,000 output tokens; `mem0CustomInstructions` supplies domain-specific
formation guidance. Projects remain explicit visibility namespaces; a search with project `"*"`
selects all visible project records in the single-user store. Caller headers remain routing metadata,
not authentication.

The extraction prompt is vendored unchanged with its Apache 2.0 license and NOTICE in
`crates/memory-algo-mem0/src/prompts/`. Strict parsing, scoped hash deduplication,
atomic primary/vector storage and batch provenance are application adaptations, not SDK guarantees.

## Memos strategy: fact formation

### Explicit instructions (both strategies)

`add_memories` accepts `memoryType:"instruction"` with `text` and the usual project/session scope. This path copies text into a separate instruction record and evidence, completes its receipt synchronously and makes no generative-model call. It rejects `messages`, `sessionEnded`, empty text, more than 4 KB or more than 1024 Unicode characters. `memoryScope` optionally selects global, current project or caller platform; otherwise the instruction is project-scoped. An optional `turnId` provides a platform-specific idempotency key. A retry preserves the first payload and scope. `memoryType:"fact"` or omission retains existing fact extraction; `memoryScope` is rejected on that path.

`get_context` returns `instructions` and renders only visible, current, explicitly written instructions in a separate section. The section shares the context token budget. Generic search does not mix instructions into its fact/experience/skill result slots. Update or retract an instruction using `correct_memory`; deletion uses the existing `delete_memory` lifecycle.

### Uncertainty and derivation

Same-scope competing assertions create an open `uncertainties` record. Search returns visible records in `uncertainties`; administrative inspection uses `POST /admin/export {"table":"uncertainties"}`. Explicit confirmation uses `POST /v1/resolve_uncertainty` with `{id, keepMemoryId, reason}`. The chosen record must belong to the question, and both competing records must still be conflicting in the same scope. Confirmation preserves original evidence, expires the rejected assertion and records the resolution. Other surviving questions keep the selected assertion conflicting until they are resolved too.

Retiring an assertion also closes questions involving it and removes obsolete conflict edges in the same transaction. A surviving assertion returns to `current` when its final dispute closes; any remaining open question keeps it `conflict`.

Derived records expose application-built `derivation: {stage,inputMemoryIds,inputTurnIds,currentView}`. Profiles and skills are current views; if a source is superseded, disputed, edited or deleted, they and their transitive dependents become `stale`. Their originals remain inspectable with `staleReason`; retrieval excludes them before index updates finish. Legacy current views are checked at startup. Historical experiences keep their lineage and remain observations of the original event. No read automatically regenerates a stale profile.

Conversation P1 runs after 3 queued turns, when the oldest queued turn has waited 300 seconds, on manual fact flush (`sync:true` or `/admin/wait_idle`), or on session end. `batchTurns` and `batchWaitSecs` configure the default 3/300 values. While a session is open, a flush processes facts and indexes only; it does not generate task analysis, experiences or a running summary.

P1 and explicit P1-b model replies use `facts-object-v2`: `{"facts":[...]}`. Each fact contains `text`, `scope`, `entities` (name strings), `effectiveAt`, `origin`, flat `sourceTurnId` / `sourceSpan`, and `relations` (`memoryId`, `relation`). Code constructs the internal nested `source`; external API records remain unchanged. There is no legacy-shape fallback. Explicit text fixes origin to `explicit` and sourceTurnId to `text`. Verbatim evidence, real dates and source roles are validated before admission; errors enter bounded model repair with the original reply and public schema, and every raw response is persisted before parsing. Evidence remains limited to 1024 Unicode characters. Relation and update-chain checks remain in formation.

For the English human-human adapter, an assistant-role reply may carry a separate `evidence.context: {span, statedAt}` containing its paired first-hand question (at most 1024 characters). The copied question supplies discourse qualifiers, not a new assertion. It commits with the verified reply; ordinary agent conversations do not enable this adapter.

## Memos strategy: session-end experience formation

P2 runs only once the session is ended, after pending fact jobs have completed or permanently failed. A session ends through `sessionEnded:true`, a new-session lifecycle signal, or the existing idle timeout (default 60 minutes). P2 reads the retained session transcript and returns `session-experience-object-v1`: `analysis`, `experiences` and `summary`. Experiences use `situation`, `approach`, `result`, `lesson`, `scope`, `turnIds` and `relations`; the summary uses `goal`, `outcome`, `decisions` and `openIssues`. Only verified outcomes can form experiences; at most three are generated per ended-session job.

Facts and experiences remain separate typed records (`type:fact` / `type:experience`) in the shared memory store and are committed in independent transactions. Fact receipts complete without depending on P2. P2 atomically commits its experiences, copied evidence, summary/watermark and index tasks. P2 failure retains the original source turns for retry without changing committed facts or their receipts. A session-end receipt waits for both stages and reports failures. Duplicate end signals do not rerun completed P2. Reopening a session processes new facts immediately and waits for its next end before another P2 job; new experiences must cite a turn beyond the previous P2 watermark.

Both stages request `response_format:{"type":"json_object"}`, with thinking disabled and bounded output tokens. The turn-bound structural contracts run locally, not in provider schema decoding. Wrong structure triggers a bounded corrective retry with the validation error and previous output; content/provenance rules remain separate. Every returned HTTP response is retained in tenant-local `llm_responses` before parsing. `POST /admin/export {"table":"llm_responses"}` reads these audit records.

Raw turns cannot be purged until their session-end P2 watermark covers them; unfinished-task sources and failed jobs remain retained under the existing rules. Fact-only open-session drains can therefore be idle while still retaining the transcript for future P2. P3/P4/P5 remain independent maintenance jobs.
