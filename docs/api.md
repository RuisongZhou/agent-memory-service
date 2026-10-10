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

## Startup

Use `agent-memoryd serve --help` for paths, model selection, host/port, LLM and concurrency settings. `--config examples/service-config.json` overrides algorithm tunables. CLI/environment settings override config-file paths, prompt variant and LLM connection settings. Unknown JSON fields and invalid score/concurrency/batch values are rejected before model servers are contacted or stores load. Startup waits up to 60 s for both llama-server endpoints and fails if either is unreachable (the reranker is skipped with `--no-reranker`). Inputs are truncated to 512 tokens and rerank logits are mapped to 0..1 with a sigmoid. Stored vectors are tagged with the embedding model id (`<alias>-<GGUF type>`); a different model or quantisation triggers re-embedding on the next start.

Configuration environment variables include `MEMORY_DATA_DIR`, `MEMORY_EMBED_URL` (default `http://127.0.0.1:8081`), `MEMORY_RERANK_URL` (default `http://127.0.0.1:8082`), `MEMORY_PROMPT_VARIANT`, `MEMORY_TENANT` (MCP), and `MEMORY_LLM_MAX_CALLS`. `QWEN_API_KEY` is supplied separately; the Python launcher reads this key from `.env` and supplies command flags for other settings.

`agent-memoryd reindex --all` or `--vector`/`--fulltext` selects work explicitly. HTTP errors produce a nonzero CLI exit status.

## Minimal search

```sh
curl -s http://127.0.0.1:8765/v1/search_memories \
  -H 'Content-Type: application/json' -H 'X-Memory-Platform: demo' \
  -d '{"query":"current project decisions","scope":{"project":"demo"},"touch":false}'
```

The core schemas are in `crates/memory-service/src/domain/types.rs`; examples live in `examples/`.

## Recent-turn buffer

`add_memories` stores both messages of a turn verbatim; only the HTTP body limit (axum's default, 2 MB) bounds them. Each turn also has a digest, the user and assistant heads within `digestMaxBytes` (default 2048 bytes). A message longer than its share of the digest is also split into overlapping windows of `digestMaxBytes / 2` bytes, and every window is embedded next to the digest vector. Short turns are stored and indexed exactly as before.

`search_memories` ranks a buffered turn by its best-matching vector. In `recentTurns`, `digest` is the turn digest or, when a window matched better, the digest with that message's head replaced by the window (omissions marked with `…`); `excerpt` then gives the window's `role` and byte `start`/`end`. `complete:false` marks a partial view: `list_turns` returns complete messages. `/admin/stats` reports `index.turnChunkVectors`.

Formation keeps its bounded view of each message: over `maxMessageBytes` (default 8 KB), prompts, source-span checks and P2 evidence use the head and tail 4 KB.

## Fact formation

Conversation P1 runs after 3 queued turns, when the oldest queued turn has waited 300 seconds, on manual fact flush (`sync:true` or `/admin/wait_idle`), or on session end. `batchTurns` and `batchWaitSecs` configure the default 3/300 values. While a session is open, a flush processes facts and indexes only; it does not generate task analysis, experiences or a running summary.

P1 and explicit P1-b writes use `facts-object-v1`: `{"facts":[...]}`. Each fact contains `text`, `scope`, `entities`, `effectiveAt`, `origin`, `source` (`turnId`, `span`) and `relations` (`memoryId`, `relation`). `reason` is omitted. Explicit text fixes origin to `explicit` and turnId to `text`. Evidence spans are copied verbatim and limited to 1024 Unicode characters. Sources, dates, role attribution, relation targets and update chains retain their existing checks.

## Session-end experience formation

P2 runs only once the session is ended, after pending fact jobs have completed or permanently failed. A session ends through `sessionEnded:true`, a new-session lifecycle signal, or the existing idle timeout (default 60 minutes). P2 reads the retained session transcript and returns `session-experience-object-v1`: `analysis`, `experiences` and `summary`. Experiences use `situation`, `approach`, `result`, `lesson`, `scope`, `turnIds` and `relations`; the summary uses `goal`, `outcome`, `decisions` and `openIssues`. Only verified outcomes can form experiences; at most three are generated per ended-session job.

Facts and experiences remain separate typed records (`type:fact` / `type:experience`) in the shared memory store and are committed in independent transactions. Fact receipts complete without depending on P2. P2 atomically commits its experiences, copied evidence, summary/watermark and index tasks. P2 failure retains the original source turns for retry without changing committed facts or their receipts. A session-end receipt waits for both stages and reports failures. Duplicate end signals do not rerun completed P2. Reopening a session processes new facts immediately and waits for its next end before another P2 job; new experiences must cite a turn beyond the previous P2 watermark.

Both stages request `response_format:{"type":"json_object"}`, with thinking disabled and bounded output tokens. The turn-bound structural contracts run locally, not in provider schema decoding. Wrong structure triggers a bounded corrective retry with the validation error and previous output; content/provenance rules remain separate. Every returned HTTP response is retained in tenant-local `llm_responses` before parsing. `POST /admin/export {"table":"llm_responses"}` reads these audit records.

Raw turns cannot be purged until their session-end P2 watermark covers them; unfinished-task sources and failed jobs remain retained under the existing rules. Fact-only open-session drains can therefore be idle while still retaining the transcript for future P2. P3/P4/P5 remain independent maintenance jobs.
