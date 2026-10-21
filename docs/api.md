# API and configuration

This is the current Mem0 request, response and configuration reference. Use
[deployment](deployment.md) for installation, [MCP integration](mcp-harness.md) for client setup,
and [architecture](architecture.md) for implementation and scoring rules.

## Interfaces

Requests use camelCase JSON with `POST /v1/<name>`. The [Rust request types](../crates/memory-domain/src/types.rs)
and [HTTP dispatcher](../crates/memory-service/src/api/http.rs) define the accepted payloads.

| Interface | Main arguments | MCP | Purpose |
|---|---|---|---|
| `add_memories` | `scope`, `messages`/`text`, `turnId`, `sync`, `infer`, `memoryType`, `memoryScope`, `sessionEnded` | yes | Submit facts, raw input or explicit instructions |
| `get_task` | `requestId` | yes | Inspect a durable write request |
| `search_memories` | `query`, `scope`, `topK`, `tokenBudget`, `types`, `rerank`, `includeEvidence`, `touch` | yes | Retrieve bounded memories; facts by default |
| `get_context` | `scope`, `tokenBudget` | yes | Assemble project/session context and explicit instructions |
| `list_turns` | `scope`, `turnIds`, `since`, `tokenBudget` | yes | Inspect complete submitted message batches |
| `get_memory` | `id`, `includeHistory`, `includeEvidence` | yes | Read one record and optional history/provenance |
| `correct_memory` | `id`, optional `text`, `effectiveAt`, `reason` | yes | Append a correction; omission of text retracts |
| `feedback` | `retrievalId`, `used`, `outcome` | yes | Record use of actual retrieved memories |
| `delete_memory` | `id` | no | Delete a record and repair references/indexes |
| `resolve_uncertainty` | `id`, `keepMemoryId`, `reason` | no | Explicitly resolve a legacy competing assertion |

The stdio bridge forwards the eight marked tools to HTTP. Success content is JSON text;
HTTP errors become MCP `isError:true` results. There is no HTTP MCP, SSE or `/mcp` endpoint.

## Scope and idempotency

| Field | Meaning |
|---|---|
| `X-Memory-Tenant` | Independent store; defaults to `default`. MCP sets it through `MEMORY_TENANT` |
| `X-Memory-Platform` | Calling adapter; MCP sets it through `--platform`, not a model-selected identity |
| `scope.project` | Stable shared project ID, independent of machine paths |
| `scope.session` | Actual native session ID; use a stable device prefix if IDs can collide |
| `turnId` | Retry identity within the tenant and calling platform |

Writes require nonempty project/session. The idempotency key is `platform + turnId`, without
project/session. Adapters encode `project/session/nativeTurnId` component by component and retain the
first payload and timestamps on retries. A new turn ID is a new event even if the text repeats.

Global and matching project memories are visible across clients; platform-scoped records default to
the caller's platform. Search defaults to all visible projects when project is omitted or `"*"`.
Raw input reads default to the caller's platform; explicit `scope.platform:"*"` reads across platforms.
Use the same daemon, tenant and project to share memory. These labels route requests and do not authenticate users.

## Writes

`messages` and `text` are mutually exclusive. A message batch requires `turnId` and 1–128 ordered
user/assistant messages with RFC3339 timestamps. An all-whitespace batch is rejected. Current messages
remain complete; the HTTP body limit still applies. Explicit `text` has a 4 KB limit.

```json
{
  "scope": {"project": "my-project", "session": "session-123"},
  "turnId": "my-project/session-123/turn-456",
  "messages": [
    {"role": "user", "content": "The service port is 8765.", "timestamp": "2026-10-15T00:00:00Z"}
  ],
  "sync": false
}
```

Inference is the default and can call the configured generation provider. `infer:false` stores nonempty
message text verbatim without generation. `messages` is asynchronous by default; `text` waits by default.
`sync:true` waits for retrieval visibility. A session-close signal uses `sessionEnded:true`; it closes an
ordered input job and does not form experiences. It should not be sent as an end-of-every-turn flag.

For an explicit behavior requirement, use `memoryType:"instruction"` with `text`. This is synchronous,
verbatim and generation-free; it rejects messages/session-end, empty text, over 4 KB or over 1024 Unicode
characters. Optional `memoryScope` selects global, the current project or the caller's platform; project
is the default. It is rejected for fact writes. An optional turn ID makes instruction writes idempotent.

## Tasks and completion

`get_task` accepts `{"requestId":"<returned ID>"}` and returns `requestId`, `status`, `memoryIds` and `error`.
Fact tasks additionally return `inputSaved`, `formed`, `searchable` and `latencyMs`.

| Status | Meaning |
|---|---|
| `queued` / `running` | Input is durable; formation is incomplete |
| `indexing` | Primary facts/vectors are committed; publication/recovery is incomplete |
| `completed` with `formed:true`, `searchable:true` | Fact formation and retrieval visibility are complete |
| `failed` | Inspect the error; raw input and request identity remain available |

A request ID alone is not completion. Explicit instruction receipts complete synchronously and omit
the extra fact-formation visibility fields. After acceptance, poll the task rather than resubmitting.
Raw model responses, including failures and repair attempts, are available through admin export.

## Reads and provenance

`search_memories` returns complete records in score order within the token budget, with `retrievalId`,
`results`, `gap`, `tokensUsed` and latency. It defaults to facts and returns empty `profiles`,
`recentTurns` and `uncertainties` compatibility arrays. Instructions are selected through `get_context`.
`topK` and `tokenBudget` must be positive; `topK` cannot exceed `maxTopK`. Search makes no generation call.

Optional reranking runs after hybrid topK selection. `score` remains the hybrid score and `rerankScore`
is added when available. Request `rerank:true` cannot load a server omitted at startup; absence or failure
keeps hybrid ordering. `touch:false` avoids access-time updates; retrieval logging still occurs.

`includeEvidence:true` on search exposes `sourceBatch`, with `level:"batch"` and
`verifiedClaimSpan:false`: complete source input is not a verified quote for every inferred claim.
Use `get_memory` for record history and evidence. Unknown effective dates remain empty; observation,
event and ingestion times are distinct. Existing verified historical evidence remains readable.

`get_context` accepts `{"scope":{"project":"my-project","session":"session-123"},"tokenBudget":1500}`.
It returns text plus `instructions`, `profile`, `projectNotes`, `projectFacts`, `facts`, `skills`,
`tokensUsed` and `retrievalId`. Only explicitly stored instructions are behavior requirements; other
sections are historical reference. Reads do not close sessions, generate memory or await unrelated writes.

`list_turns` returns ordered complete batches containing `requestId`, `messages`, `complete:true` and
`extractState`, even when formation failed. `since` is an inclusive RFC3339 cutoff against the final
message's actual instant. `turnIds` still obeys time and budget filters. A batch that exceeds the budget
is skipped rather than truncated; increase the budget or inspect the stored request for long-input audits.

## Health and administration

`GET /health` reports strategy, backend/model identity, device/dtype, loaded tenants and process LLM usage.
It confirms readiness, not provider-key validity or completion of queued writes.

| Endpoint | Behavior |
|---|---|
| `POST /v1/embeddings` | OpenAI-compatible local embeddings; `input` is a string or a nonempty array of at most 128 strings |
| `POST /admin/wait_idle` | Drain formation/index work and report remaining or failed requests |
| `POST /admin/stats` | Record counts, persisted/process usage, indexes and pending jobs |
| `POST /admin/export` | Read memories or selected audit tables in the current tenant; not a full backup |
| `POST /admin/reindex` | Queue vector/full-text rebuild; CLI requires `--all`, `--vector` or `--fulltext` |
| `POST /admin/seed` | Direct fixture admission, available only with mock models |

Export table names include `llm_usage`, `llm_responses`, `extract_log`, `retrieval_log`, `requests`,
`store_revisions`, `uncertainties`, `proposals` and [Mem0 private tables](architecture.md#entity-projection-and-storage).
Embedding token usage is approximate, not provider billing. Reindex HTTP errors produce a nonzero CLI exit.

## Configuration

Use [.env.example](../.env.example), [examples/service-config.json](../examples/service-config.json)
and `agent-memoryd serve --help`. JSON keys use camelCase; unknown fields and invalid tunables fail
before model connections or stores open. [ServiceConfig](../crates/memory-config/src/lib.rs) defines the fields.

The Python launcher, and the daemon given `--env-file PATH`, read only `QWEN_API_KEY`, `QWEN_BASE_URL`,
`LLM_MODEL` and `MEMORY_STRATEGY` from `.env`; nonempty exported values take precedence. Both parse
data, never shell code, and the daemon logs key names only. Without `--env-file`, direct daemon launches
do not load `.env`; export all needed variables explicitly. The provider must support the service's
OpenAI-compatible chat-completions/JSON contract and `enable_thinking:false`.

| Startup value | CLI / environment | Direct-daemon default |
|---|---|---|
| Data root | `--data-dir` / `MEMORY_DATA_DIR` | `./memory-data`; Python launcher defaults to checkout `daemon-data` |
| Embedding endpoint | `--embed-url` / `MEMORY_EMBED_URL` | `http://127.0.0.1:8081` |
| Reranking endpoint | `--rerank-url` / `MEMORY_RERANK_URL` | `http://127.0.0.1:8082` |
| Formation/context language | `--prompt-variant` / `MEMORY_PROMPT_VARIANT` | `zh`; `en` is also accepted |
| Generation endpoint | `--llm-base-url` / `QWEN_BASE_URL` | `https://maas.qianwenaiapi.com/compatible-mode/v1` |
| Generation model | `--llm-model` / `LLM_MODEL` | `qwen3.8-flash` |
| Concurrency | `--llm-concurrency` / `LLM_MAX_CONCURRENCY` | 24; Python launcher defaults to 1 |
| Algorithm | `--memory-strategy` / `MEMORY_STRATEGY` | `mem0` only |
| Dialogue upload | `--outbox [PATH]` (no environment variable) | Off; without PATH, `~/.local/share/agent-memory-service/harness-outbox.sqlite3` |

For the algorithm, precedence is explicit CLI, environment, JSON `memoryStrategy`, then default.
Other startup fields in the table are assigned from the CLI/environment parser after JSON merging,
including parser defaults. Set them through flags/environment rather than relying on their JSON values.

| JSON tunable | Default | Meaning |
|---|---|---|
| `mem0Threshold` | 0.1 | Semantic threshold before hybrid fusion; range 0–1 |
| `mem0TopK` | 20 | Default search result count, bounded by `maxTopK` |
| `mem0Rerank` | false | Connect/use the optional reranker |
| `mem0MaxTokens` | 2000 | Formation output-token limit |
| `mem0CustomInstructions` | empty | Additional formation guidance |
| `maxTopK` | 50 | Maximum request topK |
| `defaultTokenBudget` | 2000 | Search/input-list budget |
| `contextTokenBudget` | 1500 | Bootstrap context budget |
| `bruteForceLimit` | 20000 | Exact/ANN selection boundary; zero forces ANN |
| `useReranker` | true | Global gate; `--no-reranker` disables it |
| `llmTimeoutSecs` | 180 | Generation request timeout |

`zh` adds a formation instruction to match the new messages' language/script and selects Chinese context
labels; it does not unconditionally translate facts to Simplified Chinese. `en` uses the base prompt.
Embedding/reranking inputs are capped at 512 tokens by the model adapter; stored messages remain complete.
Startup waits for required model servers; a model identity/quantization change schedules re-embedding.

`MEMORY_LLM_MAX_CALLS` guards process-local generation attempts, including retries. The Python launcher's
`--max-calls` defaults to 60; zero removes the guard. Restart resets the counter, so it is not a durable
spending budget. `MEMORY_MOCK_MODELS=1` selects mock retrieval, not a generation stub. `MEMORY_TENANT`
sets the MCP store. Deployment commands and process-manager templates live in [deployment](deployment.md).

## Compatibility

Only Mem0 is available on main. The previous dual-strategy implementation is preserved on
[`codex/mem0-memos`](https://github.com/RuisongZhou/agent-memory-service/tree/codex/mem0-memos),
starting at `971ac13`. Retired strategies and buffer/batch/maintenance tunables are rejected;
`/admin/run_jobs` has been removed and returns HTTP 404.

Existing Mem0 stores retain their schema and historical records. Startup rejects unfinished legacy
formation work; finish it with the matching preserved binary or use a fresh store. Switching branches
does not migrate or clear a database. Historical profiles/skills and conflict records remain readable
and repairable, but main does not generate them. See [storage integrity](architecture.md#entity-projection-and-storage)
for dependency invalidation and recovery behavior.
