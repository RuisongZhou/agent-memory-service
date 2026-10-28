# API and configuration

This is the current Mem0 request, response and configuration reference. Use
[deployment](deployment.md) for installation, [MCP and client integration](mcp-harness.md) for client setup,
and [architecture](architecture.md) for implementation and scoring rules.

## Interfaces

Requests use camelCase JSON with `POST /v1/<name>`. The [Rust request types](../crates/memory-domain/src/types.rs)
and [HTTP dispatcher](../crates/memory-service/src/api/http.rs) define the accepted payloads.

| Interface | Main arguments | MCP | Purpose |
|---|---|---|---|
| `add_memories` | `scope`, `messages`/`text`, `turnId`, `sync`, `infer`, `memoryType`, `memoryScope`, `sessionEnded`; `importRunId` (HTTP only) | yes | Submit facts, raw input or explicit instructions |
| `get_task` | `requestId` | no | Inspect a durable write request |
| `search_memories` | `query`, `scope`, `topK`, `tokenBudget`, `types`, `rerank`, `includeEvidence`, `touch` | yes | Retrieve bounded memories; facts by default |
| `get_context` | `scope`, `tokenBudget` | yes | Assemble project/session context and explicit instructions |
| `list_turns` | `scope`, `turnIds`, `since`, `tokenBudget` | yes | Inspect complete submitted message batches |
| `get_memory` | `id`, `includeHistory`, `includeEvidence` | yes | Read one record and optional history/provenance |
| `correct_memory` | `id`, optional `text`, `effectiveAt`, `reason` | yes | Append a correction; omission of text retracts |
| `feedback` | `retrievalId`, `used`, `outcome` | no | Record use of actual retrieved memories |
| `delete_memory` | `id` | no | Delete a record and repair references/indexes |
| `resolve_uncertainty` | `id`, `keepMemoryId`, `reason` | no | Explicitly resolve a legacy competing assertion |

The stdio bridge forwards the six marked tools to HTTP. Success content is JSON text;
HTTP errors become MCP `isError:true` results. There is no HTTP MCP, SSE or `/mcp` endpoint. Two more MCP
tools are the bridge's own and have no `/v1` route: `propose_import` (`scope.project`, `export` page of at most
64 KiB, `source?`, `runId?`, `finish?`) plans one page of an assistant's memory export and proposes it, and
`apply_import` (`runId`, `expectedProposals`) applies the run after the person agreed in the conversation. Both
call the [import endpoints](#import-runs) as the connection's platform; the MCP prompt `import_memories`
(`project`, `source`, `lang?`) tells the agent the steps ([guide](mcp-harness.md#import-from-another-assistant-in-a-conversation)).

## Scope and idempotency

| Field | Meaning |
|---|---|
| `X-Memory-Platform` | Calling adapter; MCP sets it through `--platform`, not a model-selected identity |
| `X-Memory-Client` | The platform's program that calls: `hook` (its hooks, and the uploader that sends what they captured), `mcp` (its MCP bridge) or `http` (the default when absent: any other HTTP client). Any other value is `400 INVALID_ARGUMENT`. Recall and write logs record it as `client`, for [`platform_activity`](#health-and-administration) |
| `scope.project` | The project a memory belongs to: normally the absolute path of the session's folder, such as `/Users/me/code/repo`, which the hooks and the MCP bridge choose ([project from the session folder](design/project-routing.md)). Any nonempty string other than `*` is valid |
| `scope.kind` | `"project"` (the default) or `"global"`, a session with no project folder. `"global"` comes without `scope.project`: sending both, or any other kind, is `400 INVALID_ARGUMENT` |
| `scope.session` | Actual native session ID; use a stable device prefix if IDs can collide |
| `turnId` | Retry identity within the calling platform |

Writes require a nonempty project and session, or `kind:"global"` and a session. A write with neither a project nor
`kind:"global"` is a 400, so a client that forgets the project cannot file facts as global by accident. The
idempotency key is `platform + turnId`, without project/session. Adapters encode `project/session/nativeTurnId`
component by component (the project component is empty for a global session, giving `/session/nativeTurnId`) and
retain the first payload and timestamps on retries. A new turn ID is a new event even if the text repeats.

A global session's dialogue forms global facts: formation recalls existing facts and earlier batches as for a project
with no project facts, so only global and platform facts are its context, and the exact-text key that deduplicates
facts names no project. An explicit instruction from a global session is global by default (`memoryScope` may name
the caller's platform; a project scope is a 400, as the session has no current project). The write's request row
records the project `""` and `scopeKind:"global"`, a `list_turns` entry shows `project:""`, and a global read is
logged under the project `""`. `list_requests` and `list_retrievals` select them with `project:""`.
Consolidation and import take a project and do not handle global facts.

Global and matching project memories are visible across clients; platform-scoped records default to
the caller's platform. A read with `kind:"global"` sees global and platform memories only: `get_context` returns no
project notes or facts, `search_memories` searches no project and `list_turns` lists the global sessions' turns.
Search defaults to all visible projects when project is omitted or `"*"`, and that does not change.
Raw input reads default to the caller's platform; explicit `scope.platform:"*"` reads across platforms.
Use the same daemon and the same project (open the same folder) to share memory. These labels route requests and do
not authenticate users. A daemon serves one store, under its data directory. Through the MCP bridge `scope.project`
may be left out: it fills in the project it took from the session folder, or `kind:"global"`
([MCP bridge](mcp-harness.md)).

The daemon answers only requests addressed to `127.0.0.1`, `localhost` or `[::1]` on its own port, and refuses with
`403 FORBIDDEN` what a browser page on another origin could send: another `Host` (DNS rebinding), an `Origin` that is
not the daemon's own, or `Sec-Fetch-Site: cross-site`. curl, the MCP bridge, the hooks and the installer send no
`Origin` or `Sec-Fetch-Site` and are not affected.

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

Credentials in `text` and in message contents are redacted before anything is stored, formed or sent to a
model, by the rules import uses ([`memory-rules` redaction](../crates/memory-rules/src/import/redact.rs)):
provider keys, bearer tokens, URL passwords, `--password`-style flags, `KEY=value` assignments, `Password: …`
labels and a password stated in a sentence (`the password is hunter2`, `密码是 …`, when the value has a digit
or a symbol) become `[redacted:<kind>]`. Stored input, formed facts and instructions keep the marker.

For an explicit behavior requirement, use `memoryType:"instruction"` with `text`. This is synchronous,
verbatim apart from redaction, and generation-free; it rejects messages/session-end, empty text, over 4 KB or over 1024 Unicode
characters. Optional `memoryScope` selects global, the current project or the caller's platform; project
is the default. It is rejected for fact writes. An optional turn ID makes instruction writes idempotent.

`importRunId` names the [import run](#import-runs) a past session turn belongs to. Only
`agent-memoryd import apply` sets it, through the dialogue outbox; the MCP bridge drops it. The daemon accepts it
only on an asynchronous message batch that is a turn the run applied (same platform, project, session, `turnId`
and messages, compared after redaction) while the run is `applying` or `applied`, and refuses it otherwise (`INVALID_ARGUMENT`, or
`NOT_FOUND` for an unknown run). The turn is then deduplicated like any other by `platform + turnId`; a request it
creates records the run's ID.

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
Raw model responses, including failures and repair attempts, are available through admin export and
`list_model_responses`.

## Reads and provenance

`search_memories` returns complete records in score order within the token budget, with `retrievalId`,
`results`, `gap`, `tokensUsed` and latency. It defaults to facts and returns empty `profiles`,
`recentTurns` and `uncertainties` compatibility arrays. Instructions are selected through `get_context`.
`topK` and `tokenBudget` must be positive; `topK` cannot exceed `maxTopK`. Search makes no generation call.

Optional reranking runs after hybrid topK selection. `score` remains the hybrid score and `rerankScore`
is added when available. Request `rerank:true` cannot connect a reranker omitted at startup (`mem0Rerank:false`); absence or failure
keeps hybrid ordering. `touch:false` avoids access-time updates; retrieval logging still occurs.

`includeEvidence:true` on search exposes `sourceBatch`, with `level:"batch"` and
`verifiedClaimSpan:false`: complete source input is not a verified quote for every inferred claim.
Use `get_memory` for record history and evidence; its `memory` and `history` records also carry `statedAt`,
when the supporting text was said (empty when unknown). Unknown effective dates remain empty; observation,
event and ingestion times are distinct. Existing verified historical evidence remains readable.

`get_context` accepts `{"scope":{"project":"my-project","session":"session-123"},"tokenBudget":1500}`.
It returns text plus `instructions`, `profile`, `projectNotes`, `projectFacts`, `facts`, `skills`,
`tokensUsed` and `retrievalId`. Only explicitly stored instructions are behavior requirements; other
sections are historical reference. A fact line in `text` ends with the day the fact holds from (`（2026-11-06 起 ·
codex）`, `(since … · codex)`) or, for a fact without an effective date such as an inferred one, the day it was
said (`（2026-10-25 提到 · codex）`, `(stated …)`), then the platform. Reads do not close sessions, generate memory or await unrelated writes.

`list_turns` returns ordered complete batches containing `requestId`, `messages`, `complete:true` and
`extractState`, even when formation failed. `since` is an inclusive RFC3339 cutoff against the final
message's actual instant. `turnIds` still obeys time and budget filters. A batch that exceeds the budget
is skipped rather than truncated; increase the budget or inspect the stored request for long-input audits.

## Health and administration

`GET /health` reports the daemon `version`, strategy, backend/model identity, device/dtype and process LLM usage.
It confirms readiness, not provider-key validity or completion of queued writes.

| Endpoint | Behavior |
|---|---|
| `POST /v1/embeddings` | OpenAI-compatible embeddings from the daemon's configured model; `input` is a string or a nonempty array of at most 128 strings |
| `POST /admin/wait_idle` | Drain formation/index work and report remaining or failed requests |
| `POST /admin/stats` | Record counts, persisted/process usage, indexes and pending jobs; `retrieval` has `gapSearches` and, over the active records (`current`, `conflict`, `candidate`), `neverRecalled` and `notRecalled30d` (see [recalls](#recalls)); `features` tells whether `consolidation` is enabled (`consolidationEnabled`) and a `generation` key is configured |
| `POST /admin/export` | Read memories or selected audit tables of the store; not a full backup |
| `POST /admin/reindex` | Queue a vector rebuild (re-embed every memory); CLI requires `--all` or `--vector` |
| `POST /admin/consolidate` | Propose consolidation ops for one project and store the run; requires `consolidationEnabled` and `dryRun:true` |
| `POST /admin/consolidation_apply` | Apply a proposed run's ops, one transaction per op; requires `consolidationEnabled` |
| `POST /admin/consolidation_rollback` | Undo an applied run's ops, newest first; ops changed since stay applied |
| `POST /admin/import_propose`, `import_run`, `import_runs`, `import_apply`, `import_rollback` | [Import runs](#import-runs): a host's existing memories as reviewable proposals, applied and rolled back on request |
| `POST /admin/list_memories` | One page of records as `get_memory` returns them, newest recorded first, and the `total` matching; each record also carries `lastRecalledAt` (a time, or null) and `recallCount` (0 when never recalled), see [recalls](#recalls); filters `project`, `scopeKind`, `sourcePlatform`, `types`, `statuses` (default `current`, `conflict`, `candidate`), `origins`, `text` (case-insensitive substring), `neverRecalled` (`true`: records no search or context read ever returned) and `notRecalledDays` (1–3650: records recorded more than that many days ago and not returned in that many days; not together with `neverRecalled`, which is a 400); `limit` 1–200 (default 50); send `nextCursor` back as `cursor` until it is null |
| `POST /admin/memory_facets` | Record counts over every status: `projects`, `sourcePlatforms` and `origins` as `{name, count}` sorted by name, `byTypeStatus` and `total` |
| `POST /admin/memory_lineage` | `{id, depth?}` (`depth` 1–6, default 3; a malformed `id` or a bad `depth` is a 400, an unknown record a 404): the records linked to `id`, found breadth first in both directions, as `{root, nodes, edges, truncated}`. A node is `{id, type, status, text, scope, recordedAt, effectiveAt, statedAt, origin, platform, depth}` (`depth` is its distance from the root; the root comes first). An edge is `{from, to, kind}` with `kind` `supersedes` (newer to older; held by `supersedes` on the newer record, `supersededBy` on the older, or both, and listed once), `derived` (to each `derivation.inputMemoryIds`, with `stage`), `uses` (to each `usedMemoryIds` of a profile or skill) or `related` (to each `related` id, with `relation`; a pair named by both records is one edge per relation, from the smaller id). Records that do not exist are skipped and edges join only returned nodes. At most 200 nodes: past that, `truncated` is true |
| `POST /admin/list_entities` | One page of the algorithm's entities (Mem0's entity projection), most linked memories first, ties by id descending: `{entities, types, nextCursor, projectionUpdatedAt}`. An entity is `{id, text, entityType, scope, memoryCount}`, never its embedding. Filters `project` (`scope.project`), `entityType`, `text` (case-insensitive substring of the text or normalized text); `limit` 1–200 (default 50); cursor as for `list_memories`. `types` is `{name, count}` per entity type, sorted by name, under the `project` filter only. Entities are read as stored: reading does not update them (search and formation do, which embeds new entity texts), so `projectionUpdatedAt` says when they were last written, or is null before they were ever built |
| `POST /admin/entity` | `{id}` (an entity id from `list_entities`): `{entity, memories, projectionUpdatedAt}`. `entity` is as listed plus `normalizedText`; `memories` are the linked records that still exist, newest recorded first, as `list_memories` returns them. A malformed `id` is a 400, an unknown entity a 404 |
| `POST /admin/memory_entities` | `{id}` (a memory id): `{entities, projectionUpdatedAt}`, the entities linking that memory, as listed, sorted by text. A malformed `id` is a 400, an unknown memory a 404 |
| `POST /admin/list_uncertainties` | Questions of one `status`, `open` (default), `resolved` or `withdrawn`, newest first, as stored with `id`; each carries its member records in `memoryIds` order as `memories` (as `get_memory` returns them; deleted ones left out); filter `project`; `limit` 1–200 (default 50), no cursor. Resolve with `resolve_uncertainty` |
| `POST /admin/consolidation_runs` | Consolidation runs, newest first, without `clusters`, `proposalIds` and `responseLogIds`; filter `project`; `limit` 1–200 (default 20); not gated by `consolidationEnabled` |
| `POST /admin/consolidation_run` | `{runId}`: `run` and `proposals` as `consolidation_apply` returns them, and `memories`, a summary `{id, type, status, text, scope}` of every memory a proposal names (deleted ones left out) |
| `POST /admin/list_retrievals` | One page of `retrieval_log` rows with `id`, newest first, and `memories` summarizing each memory they returned; filters `kind` (`search` or `context`), `platform`, `excludePlatforms` (rows without a platform are kept), `project`, `session`, `memoryId` (rows of the last 30 days only; the response then has `window: "30d"`); `limit` 1–200 (default 50); cursor as for `list_memories` |
| `POST /admin/platform_activity` | The platforms that called in the last 30 days, by channel: `{window: "30d", platforms}`, the platform heard from last first. A platform is `{platform, lastAt, capture?, hookRecall?, mcp?, other?}`, each channel `{count, lastAt}` and absent without calls: `capture` counts the write requests of `client: "hook"` (captured dialogue; the turns of import runs are left out), `hookRecall` the `retrieval_log` rows of `client: "hook"`, `mcp` both of `client: "mcp"`, and `other` both of `client: "http"` and rows logged before the client was recorded. The web UI's calls and rows without a platform are left out |
| `POST /admin/platforms` | The installer's report of every agent platform it can connect ([platform page](design/platform-connect.md)): the daemon runs its `--installer` copy with `status --json` and returns that object. `{ok, service: {url, error}, hosts, log, error}`; a host is `{id, label, platform, capture, recall, files, detected, connected, options: {capture, recall} \| null, upToDate, canRollback, error}`. `detected` is the configuration directory or CLI found on this machine (or null); `upToDate` says whether connecting again with the current `options` would change nothing (null when not connected, or when `service.error` says the daemon platforms connect to cannot be read). Without `--installer`: 404 `NOT_FOUND` |
| `POST /admin/connect` | `{host, action, capture?, recall?}`: runs `connect --json --host H`: a preview (`action: "preview"`, no mode flag), or with `--apply`, `--uninstall` or `--rollback`, plus `--no-capture` when `capture` is false and `--recall` when `recall` is true (both for `preview` and `apply`; defaults true and false). Returns the installer's object `{ok, host, mode, changes: [{path, action: create\|update\|delete}], backup, log, error}`; a run that fails (configuration it cannot read, another installer running, a failed offline check) is still 200 with `ok: false` and `error`. `host` must be lowercase letters and hyphens (400 otherwise; an unknown platform is 400 from the installer); other fields are 400. Up to 10 minutes: `apply` first checks the hooks against a temporary daemon. A dropped request does not stop a write. Without `--installer`: 404 `NOT_FOUND` |
| `POST /admin/list_requests` | One page of write requests with `id`, newest first, without `inputMessages` but with `messageCount`; filters `statuses`, `kind` (`mem0` or `instruction`), `platform`, `project`; `limit` 1–200 (default 50); cursor as for `list_memories` |
| `POST /admin/request` | `{requestId}`: the whole `request`, its `extract_log` rows (`extractions`, oldest first) and the `llm_responses` rows they name (`responses`), each with `id`. A request whose every generation call failed has neither; see `list_model_responses` |
| `POST /admin/list_model_responses` | One page of `llm_responses` rows as stored with `id`, newest first; filters `stage`, `failedOnly` (no `validOutputShape:true`: a transport error, an HTTP status outside 2xx, a body that is not JSON or output of the wrong shape); `limit` 1–100 (default 20); cursor as for `list_memories` |

Export table names include `llm_usage`, `llm_responses`, `extract_log`, `retrieval_log`, `memory_recalls`, `requests`,
`store_revisions`, `uncertainties`, `proposals`, `consolidation_runs`, `import_runs`, `import_ledger`,
`import_extractions` and [Mem0 private tables](architecture.md#entity-projection-and-storage).
A `retrieval_log` row is one `search_memories` or `get_context` call: `kind` (`search` or `context`), the
caller's `platform` and `client`, `project`, `session`, `createdAt`/`createdTs`, `returned.memories` (ids and ranks),
`returned.tokens` and, for searches, `query` and `returned.gap` (nothing found; counted in `stats`
`retrieval.gapSearches`). Startup fills in `kind`, `createdTs` and `gap` on searches logged before these
fields existed; their platform, project, session and `returned.tokens` stay absent. Write requests also record the
caller's `platform` and `client`; rows logged before the client was recorded have no `client`.
Embedding token usage is approximate, not provider billing. Reindex HTTP errors produce a nonzero CLI exit.

### Recalls

`memory_recalls` has one row per recalled memory, keyed by its id: `memoryId`, `lastRecalledAt`,
`lastRecalledTs` and `recallCount`. A `search_memories` or `get_context` call writes its `retrieval_log` row and
counts the call, once per returned memory, in the same transaction. Calls whose platform is `web` (the web UI's own
searches and context previews) are logged but not counted. `lastAccessedAt` is no substitute: it only changes
when a search sends `touch: true`. Startup sums an existing `retrieval_log` into an empty `memory_recalls`, once,
using each row's `createdTs`. Deleting a memory removes its row in the same transaction.
`list_memories` reads the summary for `lastRecalledAt`, `recallCount` and its two filters, and `stats` counts the
active records with no row (`retrieval.neverRecalled`) and those recorded more than 30 days ago and not returned in
the last 30 (`retrieval.notRecalled30d`).

### Consolidation proposals

Consolidation is an opt-in deviation from the pinned Mem0 algorithm. Proposing changes no memory record;
a run changes records only when it is [applied](#applying-and-rolling-back-consolidation-runs).
[Architecture](architecture.md#consolidation) describes candidate selection, validation, limits, apply
and rollback.

```json
POST /admin/consolidate
{"project": "my-project", "dryRun": true}
```

The request fails with `INVALID_ARGUMENT` unless the configuration sets `consolidationEnabled:true`,
`dryRun` is `true` and `project` names one project (not `*`); it fails with `MODEL_UNAVAILABLE` when
there are clusters and no generation key. Each selected cluster costs one generation call (with the
contract's bounded repair). The response is `{"run": {...}, "proposals": [...]}`, also stored in
`consolidation_runs` and `proposals`:

| Field | Meaning |
|---|---|
| `run.status` | `proposed`; `aborted` when accepted ops would retire more than the per-run share of the project's current facts; `failed` when a generation call failed |
| `run.snapshotRevision`, `llmModel`, `embeddingModel`, `promptFormat`, `promptHash` | What the proposals were generated from |
| `run.projectFacts`, `eligibleFacts`, `clusters`, `responseLogIds` | Candidate counts, each cluster's members, links, input hash and `llm_responses` id |
| `run.acceptedOps`, `rejectedOps`, `retireCount`, `retireFraction`, `limits` | Validation outcome and the fixed limits applied |
| `proposals[].op` | `merge` (`ids`, `text`), `supersede` (`old`, `new`), `conflict` (`ids`, `reason`) or `keep` (`ids`), with memory ids |
| `proposals[].status`, `rejectReason` | `proposed`, `rejected` (failed validation, with the reason), or the run's `aborted`/`failed` |
| `proposals[].detail` | Algorithm provenance for applying: input content keys; for merges the union of input request ids and the merged content key |

`agent-memoryd consolidate --project P --dry-run [--url URL]` posts the same request to a
running daemon and prints the run. HTTP errors exit nonzero.

### Applying and rolling back consolidation runs

```json
POST /admin/consolidation_apply
{"runId": "cr…"}
```

Apply requires `consolidationEnabled:true` and a run in status `proposed`, or `applying` to resume an
interrupted apply. Under the formation lock, each `proposed` op is rechecked against the stored records.
It then commits in its own transaction, and its proposal becomes `applied`, keeping in `applied` the
values it replaced. An op is marked `skipped` with `skipReason` when an input changed or is no longer a
current candidate. The run becomes `applied` with `appliedOps` and `skippedOps`. What each op writes:

| Op | Writes |
|---|---|
| merge | A new current fact (`applied.mergedId`) with `derivation.stage:"consolidation"`; its inputs become `superseded` with `supersededBy` pointing at it |
| supersede | `old` becomes `superseded` with `supersededBy` and `validUntil`; `new.supersedes` is set |
| conflict | One open uncertainty (`applied.uncertaintyId`); the members become `conflict` and are settled with `resolve_uncertainty` |
| keep | Nothing |

```json
POST /admin/consolidation_rollback
{"runId": "cr…"}
```

Rollback accepts runs in status `applied`, `applying`, `rolling_back` or `partially_rolled_back`. It is
not gated by `consolidationEnabled`, so a run can still be undone after the flag is turned off. It
inverts `applied` ops, newest first:

- A merged record becomes `superseded` (it is never deleted) and its inputs return to `current`.
- A supersede link is removed.
- An open question becomes `withdrawn` and its members return to `current`.

An op stays `applied`, with `rollbackSkipReason`, when a person or a later run changed its result: the
merged record was corrected, deleted or merged again, the question was resolved, or a link moved. The run
is then `partially_rolled_back` and can be rolled back again later; otherwise it is `rolled_back`, which
is final. Both actions return `{"run": {...}, "proposals": [...]}`.

`agent-memoryd consolidate --apply RUN` and `--rollback RUN` post these requests. `--dry-run`, `--apply`
and `--rollback` are mutually exclusive.

`delete_memory` also follows merges. Deleting a merged record leaves its inputs superseded. Deleting one
of its inputs deletes the merged record too and returns the other inputs to `current`; these are listed
in `restoredInputs`.

### Import runs

`agent-memoryd import` drives these endpoints ([commands](mcp-harness.md#import-existing-memories),
[processing](architecture.md#import)); the [Rust types](../crates/memory-domain/src/import.rs) define every
field. They are not gated by a configuration flag and are not MCP tools themselves; the bridge's `propose_import` and
`apply_import` call `import_propose`, `import_run` and `import_apply` for an export pasted into a conversation.

| Endpoint | Body | Answer |
|---|---|---|
| `POST /admin/import_propose` | `{project, host, runId?, finish, limits: {recallBudget, recallTopK}, planHash?, reconsider?, units: [...]}`, at most 64 units | `{run, proposals, units}` of this batch |
| `POST /admin/import_run` | `{runId}` | `{run, proposals}` |
| `POST /admin/import_runs` | `{project?, limit?}` (1–200, default 20) | `{runs}`, newest first |
| `POST /admin/import_apply` | `{runId, decisions: {proposalId: "accept" \| "reject"}, confirmedVia?: "chat"}` | `{run, proposals}` |
| `POST /admin/import_rollback` | `{runId}` | `{run, proposals}` |

A unit is `{unit, host, mode, text, pieces?, statedAt, kindHint?, headingPath, fileHash, unitHash, source,
turn?}`: `mode` is `verbatim` (the text is one proposal), `split` (each piece is one),
`extract` (one generation call; the text is at most 8 KB) or `session-turn`. The first batch omits `runId` and
creates the run; later batches name it, one at a time; the batch with `finish:true` makes the run `proposed`.
`recallBudget` must be at least 40 and `recallTopK` positive. `unitHash` must be the blake3 hex of `text`, unit
keys are unique within a run, and an `extract` batch needs the generation key.

A `session-turn` unit is a past session turn: `turn` is `{turnId, session, nativeId, messages}`, one user message
and then one assistant message with RFC3339 times in order, and `turnId` must be the capture turn key
`project/session/nativeId`. It is registered without a model call as a row of the run's proposals with
`itemKind:"session-turn"` and the `turn`, at most once per `turnId`. `import_apply` marks it `applied` with
`applied:{"queued":true}` and writes nothing: the client queues it into the dialogue outbox with `importRunId`
(see [Writes](#writes)), and when it is submitted the row gains `applied.requestId` and `applied.duplicate`
(whether the daemon already had the turn). `import_rollback` closes the run to further turns, then retires each
fact formed only from the run's own requests (`superseded`, marked `importRollback`); a turn the daemon had before,
or never received, needs nothing, and one still forming stays `applied` with `rollbackSkipReason` until another
rollback. Run counts keep session turns apart from proposals: `sessionTurns`, `turnsQueued`, `turnsRolledBack`.

| Status | Values |
|---|---|
| Run | `proposing` → `proposed` → `applying` → `applied` → `rolling_back` → `rolled_back` or `partially_rolled_back` |
| Proposal | `proposed` or `rejected` (with `rejectReason`); then `dropped`, `applied` or `skipped` (with `skipReason`); then `rolled_back`, or still `applied` with `rollbackSkipReason` |
| Unit outcome | `new`, `changed`, `unchanged`, `reconsidered` or `failed` (with `error`; the run continues); a changed unit lists the current records its earlier import left in `previousRecords` |

Each proposal carries `itemKind` (`fact`, `preference` or `reference`; each is written as an imported fact, and
import never writes an instruction), `scopeHint`, `evidence`, `duplicateOf` (accepting it links the unit to that
record), `supersedes` (the record the unit's earlier import wrote that this one replaces) and up to three
`similar` memories. `import_apply` takes each proposal's decision from `decisions` and accepts a proposal it does
not name; `confirmedVia: "chat"` (sent by `apply_import`) is kept on the run, so runs a person confirmed only in a
conversation can be found. `reconsider` proposes again the units whose last import applied nothing
although their text is unchanged. Errors are `INVALID_ARGUMENT` (400) for validation, a bad `runId` or a run in the
wrong status, `NOT_FOUND` (404), `MODEL_UNAVAILABLE` (503) for extract units without a generation key, and
`INTERNAL` (500) during apply or rollback, which is recorded on `run.error`; repeating the call resumes.

## Configuration

Use [.env.example](../.env.example), [examples/service-config.json](../examples/service-config.json)
and `agent-memoryd serve --help`. JSON keys use camelCase; unknown fields and invalid tunables fail
before model connections or stores open. [ServiceConfig](../crates/memory-config/src/lib.rs) defines the fields.

The daemon given `--env-file PATH`, and the Python launcher for the checkout's `.env`, read only `QWEN_API_KEY`, `QWEN_BASE_URL`,
`LLM_MODEL`, `MEMORY_STRATEGY` and the `MEMORY_MODEL_*`, `MEMORY_EMBED_*` and `MEMORY_RERANK_*` variables
below from `.env`; nonempty exported values take precedence. Both parse data, never shell code, and the
daemon logs key names only. Without `--env-file`, direct daemon launches do not load `.env`; export all
needed variables explicitly. The provider must support the service's
OpenAI-compatible chat-completions/JSON contract and `enable_thinking:false`.

| Startup value | CLI / environment | Direct-daemon default |
|---|---|---|
| Data root | `--data-dir` / `MEMORY_DATA_DIR` | `./memory-data`; `service install` defaults to `~/.local/share/agent-memory-service/mem0`, the Python launcher to checkout `daemon-data` |
| SurrealDB executable | `--surreal-bin` / `MEMORY_SURREAL_BIN` (JSON `surrealBin`) | `surreal` on PATH; the daemon starts it on `<data root>/store/db` ([install SurrealDB](deployment.md#install-surrealdb)); `service install` records an absolute path |
| Retrieval-model provider | `--model-provider` / `MEMORY_MODEL_PROVIDER` | `api`; `self-hosted` uses [your own llama-server](self-hosted-models.md) |
| Retrieval-model API key | `MEMORY_MODEL_API_KEY` (environment only) | Falls back to `QWEN_API_KEY`; required for `api` |
| Embedding base URL | `--embed-url` / `MEMORY_EMBED_URL` | `api`: `https://maas.qianwenaiapi.com/compatible-mode/v1` (`/embeddings`); `self-hosted`: `http://127.0.0.1:8081` |
| Reranking base URL | `--rerank-url` / `MEMORY_RERANK_URL` | `api`: `https://maas.qianwenaiapi.com/compatible-api/v1` (`/reranks`); `self-hosted`: `http://127.0.0.1:8082` |
| Embedding model | `--embed-model` / `MEMORY_EMBED_MODEL` | `qwen3.7-text-embedding-flash`; `api` only |
| Reranking model | `--rerank-model` / `MEMORY_RERANK_MODEL` | `qwen3.7-text-rerank`; `api` only |
| Formation/context language | `--prompt-variant` / `MEMORY_PROMPT_VARIANT` | `zh`; `en` is also accepted |
| Generation endpoint | `--llm-base-url` / `QWEN_BASE_URL` | `https://maas.qianwenaiapi.com/compatible-mode/v1` |
| Generation model | `--llm-model` / `LLM_MODEL` | `qwen3.8-flash` |
| Concurrency | `--llm-concurrency` / `LLM_MAX_CONCURRENCY` | 24; `service install` and the Python launcher default to 1 |
| Algorithm | `--memory-strategy` / `MEMORY_STRATEGY` | `mem0` only |
| Dialogue upload | `--outbox [PATH]` (no environment variable) | Off; without PATH, `~/.local/share/agent-memory-service/harness-outbox.sqlite3` |
| Web UI | `--ui-dir` / `MEMORY_UI_DIR` | Off: `/ui` answers 404 `NOT_FOUND`. A directory holding the front end's `index.html`, checked at startup; served read-only at `/ui/` with `Cache-Control: no-cache` |
| Platform page's installer | `--installer PATH` (no environment variable) | Off: `/admin/platforms` and `/admin/connect` answer 404 `NOT_FOUND`. The launcher of the installer copy `install.sh service` writes; an executable file, checked at startup |

For the algorithm and the retrieval-model provider, URLs and models, precedence is explicit CLI,
environment, JSON (`memoryStrategy`, `modelProvider`, `embedUrl`, `rerankUrl`, `embedModel`,
`rerankModel`), then default; URL defaults follow the selected provider. Other startup fields in the
table are assigned from the CLI/environment parser after JSON merging, including parser defaults. Set
them through flags/environment rather than relying on their JSON values. The model API key is never
read from JSON.

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
| `consolidationEnabled` | false | Allow proposing and applying consolidation runs; evaluation protocols never set it |

`zh` adds a formation instruction to match the new messages' language/script and selects Chinese context
labels; it does not unconditionally translate facts to Simplified Chinese. `en` uses the base prompt.
Self-hosted embedding/reranking inputs are capped at 512 tokens and hosted ones at 6,000/3,000 characters
by the model adapter; stored messages remain complete. Startup probes the retrieval models once (and waits
for self-hosted servers to load); a provider, model or quantization change schedules re-embedding.
Health reports `modelBackend` as `api`, `llama-server` or `mock`, with the embedding base URL as `modelDevice`.

`MEMORY_MOCK_MODELS=1` selects mock retrieval, not a generation stub. Deployment commands and
process-manager templates live in [deployment](deployment.md).

## Compatibility

The retrieval-model provider defaults to `api`: the daemon uses the hosted models (with `QWEN_API_KEY` when no model
key is set) and sends stored text to the provider. A llama-server deployment sets
`MEMORY_MODEL_PROVIDER=self-hosted`; changing the provider re-embeds every store at the next start.

Only Mem0 is available. Retired strategies and buffer/batch/maintenance tunables are rejected;
`/admin/run_jobs` and the mock-only `/admin/seed` do not exist and return HTTP 404.

SurrealDB is an external dependency, not compiled into the daemon: install SurrealDB 3.2 or a later 3.x
([install SurrealDB](deployment.md#install-surrealdb)). An existing `store/db` opens in place; its first start
creates the datastore's root user and `store/db.password`. Without `surreal` on PATH or `--surreal-bin`, the
daemon does not start.

What changed between versions, and what to do when upgrading, is in the [changelog](../CHANGELOG.md).
