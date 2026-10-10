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

Use `agent-memoryd serve --help` for paths, model selection, host/port, LLM and concurrency settings. `--config examples/service-config.json` overrides algorithm tunables. CLI/environment settings override config-file paths, prompt variant and LLM connection settings. Unknown JSON fields and invalid score/concurrency/batch values are rejected before models or stores load.

Configuration environment variables include `MEMORY_DATA_DIR`, `MEMORY_MODELS_DIR`, `MEMORY_MODEL_DEVICE`, `MEMORY_PROMPT_VARIANT`, `MEMORY_TENANT` (MCP), and `MEMORY_LLM_MAX_CALLS`. `QWEN_API_KEY` is supplied separately; the Python launcher reads this key from `.env` and supplies command flags for other settings.

`agent-memoryd reindex --all` or `--vector`/`--fulltext` selects work explicitly. HTTP errors produce a nonzero CLI exit status.

## Minimal search

```sh
curl -s http://127.0.0.1:8765/v1/search_memories \
  -H 'Content-Type: application/json' -H 'X-Memory-Platform: demo' \
  -d '{"query":"current project decisions","scope":{"project":"demo"},"touch":false}'
```

The core schemas are in `crates/memory-service/src/domain/types.rs`; examples live in `examples/`.

Fact extraction accepts verbatim `source.span` excerpts up to 1024 Unicode characters (including spaces and punctuation). Explicit P1-b writes use `fact-values-v1`: the model returns only a JSON array of rows, each with exactly nine positions: `[text, scope, entities, effectiveAt, origin, turnId, span, relations, reason]`. `entities` is a string array, `relations` contains `[targetId, relationType]` pairs, and `reason` is a string or `null`. Empty entities/relations use `[]`; missing positions are invalid. No facts is `[]`. Explicit writes fix origin to `explicit` and turnId to `text`. Conversation formation uses these same fact rows inside the combined format below.

The program constructs `facts`, `source.turnId`/`source.span` and `memoryId`/`relation` objects; the model does not emit object keys. Each malformed fact row is rejected with a diagnostic detail without renumbering subsequent F references. Logs retain both `modelOutput` values and assembled `output`. Source matching, date/role checks and target visibility checks still apply.

## Combined conversation formation

Conversation turns run P1 and P2 in one background job and one model response. It starts after 3 queued turns, when the oldest queued turn has waited 300 seconds, on manual flush (`sync:true` or `/admin/wait_idle`), or on session end. `batchTurns` and `batchWaitSecs` configure the default 3/300 values. Ending an already-processed session completes its receipt without another model call. Explicit `text` writes retain the fact-only P1-b path.

Combined jobs use `formation-values-v1`: a fixed four-section value array `[factRows, analysisRows, experienceRows, summaryValues]`. Fact rows retain the nine positions above. Analysis rows are `[taskName, outcome, turnIds]`; experience rows are `[situation, approach, result, lesson, scope, turnIds, relationPairs]`; summary values are `[goal, outcome, decisions, openIssues]`. All sections and positions remain present even when empty. Program code constructs the object schemas for both extractors. Invalid analysis/experience/summary structure fails the combined job before any memories are committed. Verified outcome checks still reject unconfirmed experiences.

One transaction commits facts, experiences, copied evidence, the session summary/watermark, turn completion, request receipts and index tasks. Generation is asynchronous by default; `sync:true` and `/admin/wait_idle` wait for processing and indexes. Failed jobs retain the input for retry and cannot leave only one extractor committed.

The session's `pendingTasks` names tasks with an `unknown` outcome and their source turn IDs. Future jobs reuse those original turns for experience extraction, while new facts are extracted only from the current batch. A verified outcome (`success`, `failure` or `partial`) releases its retained sources. Purge skips turns still needed by unfinished tasks, even after the normal 3-day retention window. A task expires, releasing its turns, if no batch extends it within 30 turns or it accumulates more than 12 turns; at most 5 are kept per session, and expirations appear as `expiredTasks` in the `P2_validation` log. Older P1-completed/P2-unprocessed rows can backfill P2/summary without recreating their facts; a failing backfill keeps the turn `done` and its receipt `completed`, and stops retrying after 5 failures. A session-end receipt is answered once every turn has formed or permanently failed, and fails only for failures not already reported by an earlier end receipt of the same session. P3/P4/P5 remain separate maintenance jobs.
