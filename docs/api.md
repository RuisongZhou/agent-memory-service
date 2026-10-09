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

`GET /health` reports backend, actual device, dtype, loaded tenants and LLM usage. Admin actions under `/admin/` include stats, read-only exports, index rebuild, maintenance and wait-idle. Seed is enabled only in `MEMORY_MOCK_MODELS=1` mode.

`POST /v1/embeddings` accepts a string or a nonempty array of at most 128 strings. Non-string array elements are rejected; token usage is an approximation, not provider billing.

## Startup

Use `agent-memoryd serve --help` for paths, model selection, host/port, LLM and concurrency settings. `--config examples/service-config.json` overrides algorithm tunables. CLI/environment settings override config-file paths, prompt variant and LLM connection settings. Unknown JSON fields and invalid score/concurrency/batch values are rejected before models or stores load.

Configuration environment variables include `MEMORY_DATA_DIR`, `MEMORY_MODELS_DIR`, `MEMORY_MODEL_DEVICE`, `MEMORY_PROMPT_VARIANT`, `MEMORY_TENANT` (MCP), `MEMORY_LLM_MAX_CALLS`, `MEMORY_LLM_MAX_USD`, and `MEMORY_OFF_PEAK_ONLY`. `DEEPSEEK_API_KEY` is supplied separately; the Python launcher reads this key from `.env` and supplies command flags for other settings.

`agent-memoryd reindex --all` or `--vector`/`--fulltext` selects work explicitly. HTTP errors produce a nonzero CLI exit status.

## Minimal search

```sh
curl -s http://127.0.0.1:8765/v1/search_memories \
  -H 'Content-Type: application/json' -H 'X-Memory-Platform: demo' \
  -d '{"query":"current project decisions","scope":{"project":"demo"},"touch":false}'
```

The core schemas are in `crates/memory-service/src/domain/types.rs`; examples live in `examples/`.
