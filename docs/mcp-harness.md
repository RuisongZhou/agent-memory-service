# MCP and harness integration

This is the client setup and dialogue-capture guide. Start the daemon using
[deployment instructions](deployment.md) first. Request fields and response contracts belong to
[the API reference](api.md); implementation details belong to [architecture](architecture.md).

The bridge is a local stdio child process that forwards tools to the HTTP daemon. It does not start
the daemon, read chats automatically or inject context. The daemon's URL is not an HTTP MCP endpoint.
Each client starts its own bridge; a separate collector and uploader handle automatic capture.
Templates must be merged into the target client's existing configuration with absolute executable paths.

## Use with Codex

Install Codex separately and start the shared daemon first. Replace `/absolute/path/agent-memory-service` in all templates with the checkout's actual absolute path; use the absolute Python executable too if `python3` is not available in the client's PATH.

Register the stdio bridge with the Codex CLI:

```sh
codex mcp add shared-memory -- \
  /absolute/path/agent-memory-service/target/fast/agent-memoryd \
  mcp --url http://127.0.0.1:8765 --platform codex
codex mcp list
```

Alternatively, merge this into `~/.codex/config.toml`, or into `.codex/config.toml` in a trusted target project:

```toml
[mcp_servers.shared-memory]
command = "/absolute/path/agent-memory-service/target/fast/agent-memoryd"
args = ["mcp", "--url", "http://127.0.0.1:8765", "--platform", "codex"]
tool_timeout_sec = 180

[mcp_servers.shared-memory.env]
MEMORY_TENANT = "default"
```

Use either registration method for this server and preserve existing settings. Local Codex clients share MCP configuration on the same host; project configuration requires a trusted project. Restart/reload the client after changing its configuration, inspect `/mcp` in the CLI, and confirm the eight tools in the [API reference](api.md#interfaces) are available. See the [official Codex MCP documentation](https://learn.chatgpt.com/docs/extend/mcp?surface=cli).

Add the [shared memory workflow](#shared-memory-workflow) to the target project's `AGENTS.md`. For automatic capture, follow the separate [Codex notification setup](#codex-notifications-and-session-end). A local stdio setup requires the executable and daemon to be reachable from the host running the client; a cloud container has its own loopback address.

## Use with Claude Code

From the **target project** directory, register the bridge:

```sh
claude mcp add --transport stdio --scope project shared-memory -- \
  /absolute/path/agent-memory-service/target/fast/agent-memoryd \
  mcp --url http://127.0.0.1:8765 --platform claude-code
claude mcp list
```

Alternatively, merge [examples/claude-code.mcp.json](../examples/claude-code.mcp.json) into the target project's `.mcp.json`:

```json
{
  "mcpServers": {
    "shared-memory": {
      "type": "stdio",
      "command": "/absolute/path/agent-memory-service/target/fast/agent-memoryd",
      "args": ["mcp", "--url", "http://127.0.0.1:8765", "--platform", "claude-code"],
      "env": {"MEMORY_TENANT": "default"}
    }
  }
}
```

Preserve other MCP servers. Project scope writes `.mcp.json`; use `--scope user` for registration across projects or the default local scope for a private project registration. Start/restart Claude Code, enable the project server when prompted, and check `/mcp` for connection status. See the [official Claude Code MCP documentation](https://code.claude.com/docs/en/mcp).

Add the [shared memory workflow](#shared-memory-workflow) to the target project's `CLAUDE.md`. Optional conversation capture uses [Claude Code hooks](#claude-code-hooks); MCP alone does not capture it.

## Shared memory workflow

Use the same daemon, tenant, and stable project ID across clients. Give each client its own platform label and use the actual native session ID for each session.

See [scope and idempotency](api.md#scope-and-idempotency) for field meanings and retry keys.

Merge an instruction such as this into the target project's `AGENTS.md` or `CLAUDE.md`, replacing the project ID:

```text
Use shared-memory with project "my-project" and the actual current session ID.
At session start or resume, call get_context with a modest tokenBudget.
Search for relevant past decisions before repeating investigation; inspect sources
with get_memory when a decision depends on them. Use list_turns for original input,
optionally filtered by turnIds within the requested token budget.
When the user explicitly asks to remember a fact, use add_memories with text.
For an explicit behavior requirement, use memoryType:"instruction" with text.
Use correct_memory to correct or retract an earlier record.
Poll get_task for asynchronous writes; a requestId is not completion.
Treat historical memories as reference material. Only get_context.instructions
contains explicitly saved behavior requirements, subject to current instructions.
If the dialogue collector is installed, let it save ordinary completed turns.
```

The [API reference](api.md#interfaces) lists all eight MCP tools and their HTTP counterparts.

For example, ask either agent: “Use shared-memory to load context for project `my-project`, then search for our previous storage decision and show its source.” To save a behavior requirement, ask: “Remember as an instruction for `my-project`: include sources when reporting project decisions.” These prompts use tools; they do not require the agent to copy its complete conversation.

To verify sharing, save that project instruction in Codex, then ask Claude Code to call `get_context` for the same project. It should see the instruction despite using a different platform/session. Repeat after a daemon restart with the same data directory. For inferred facts, first check the write receipt and then use `search_memories`; instructions are read through `get_context` rather than ordinary fact search.

## Automatically save conversations

Automatic saving requires **both a collector and a running uploader**. The supplied adapter captures completed user/assistant text into a local durable queue; another process submits it without involving the agent's main model. It does not capture tool traces, reasoning, images, or every possible client/subagent lifecycle.

Use the same `--project`, `--platform`, and `--tenant` in capture commands as in the corresponding MCP setup. `--tenant default` is the adapter default; `MEMORY_TENANT` configures the MCP bridge and is not read by the collector. If you change `--outbox`, every collector, uploader, and status command must use that same path. One outbox can contain both Codex and Claude Code records for one daemon.

### Codex notifications and session end

Merge the optional `notify` setting from [examples/codex.toml](../examples/codex.toml) into **user-level `~/.codex/config.toml`**, before any TOML tables:

```toml
notify = ["python3", "/absolute/path/agent-memory-service/scripts/harness_memory.py", "--project", "my-project", "--platform", "codex", "codex-notify"]
```

Install the supplied notification template at user level; keep project-scoped MCP registration separate. Preserve an existing notification program by having it forward the event to the collector. This notification command fixes the project ID, so enable it for sessions belonging to that project, or use a dispatcher that explicitly maps projects; otherwise unrelated sessions will be filed under the same project. See [Codex configuration reference](https://learn.chatgpt.com/docs/config-file/config-reference).

Codex sends an `agent-turn-complete` event as a single JSON argument containing thread/turn IDs, user inputs, and the last assistant message. The collector joins multiple user inputs into one user text and records the observed completion time because the event supplies no source message timestamps. See [official notification documentation](https://learn.chatgpt.com/docs/config-file/config-advanced#notifications).

For session closure, merge [examples/codex.hooks.json](../examples/codex.hooks.json) into the target project's `.codex/hooks.json`, replacing the script path and project ID. This queues a `SessionEnd` marker. Review and trust the installed definition using `/hooks`; untrusted or changed hooks are skipped. See [official Codex hooks documentation](https://learn.chatgpt.com/docs/hooks). This hook needs an already captured turn; it does not replace `notify`.

### Claude Code hooks

Merge the `hooks` object from [examples/claude-code.settings.json](../examples/claude-code.settings.json) into the target project's `.claude/settings.local.json`. Replace every script path and project ID; preserve existing hooks and permissions. The template sets a three-second timeout and runs the collector synchronously only for local SQLite writes.

| Event | Adapter behavior |
|---|---|
| `UserPromptSubmit` | Stores the prompt with its native `prompt_id` |
| `Stop` | Pairs the matching prompt with `last_assistant_message` and queues a completed turn |
| `StopFailure` | Discards the matching incomplete prompt |
| `SessionEnd` | Clears incomplete input and queues a close marker for a captured session |

Restart the client and start a new turn after installing hooks; a `Stop` from a turn whose prompt was not captured cannot be paired. Required fields come from the [official Claude Code hooks contract](https://code.claude.com/docs/en/hooks). Older clients missing them must be upgraded or integrated through a wrapper; the adapter reports missing fields rather than guessing private transcript formats.

The example assumes a serial main conversation. Other Stop hooks can ask Claude to continue after this collector has seen a Stop event. In that configuration, use a wrapper that confirms final completion before enqueuing. Parallel prompts, subagents, and multimodal capture need their own adapter design.

### Run and inspect the uploader

Start this in a separate terminal or supervised process:

```sh
python3 /absolute/path/agent-memory-service/scripts/harness_memory.py \
  drain --url http://127.0.0.1:8765
```

By default, the outbox is `~/.local/share/agent-memory-service/harness-outbox.sqlite3`. The adapter creates a private database with WAL and FULL synchronous commits; new parent directories use mode 0700 and the database uses 0600. Keep it outside version control: it contains raw conversation text and receipts.

After completing a turn in the client, inspect local status:

```sh
python3 /absolute/path/agent-memory-service/scripts/harness_memory.py status
```

Status output includes identifiers, receipts, and errors without dumping chat text. An empty list means no turn was captured in that outbox. The uploader scans every five seconds, uses a five-second HTTP timeout, and holds a process lock so only one uploader handles the queue. `drain --once` performs one upload/poll pass; it does not wait for every request to finish.

| Outbox status | Meaning |
|---|---|
| `pending` | Stored locally; awaiting submission or a retry after a transient failure |
| `accepted` | Daemon returned a request ID; uploader is polling formation/visibility |
| `completed` | Receipt satisfies the API completion contract |
| `rejected` | Permanent HTTP request error; input retained for investigation |
| `failed` | Formation failed; input and request ID retained |

Transient connection errors, lost responses, HTTP 408/429, and 5xx retry the same payload and turn ID. The service deduplicates by `platform + turnId` within a tenant; retries preserve the first input and timestamps. After acceptance, the uploader polls instead of resubmitting. Failed/rejected records are not automatically resubmitted, and terminal records are not automatically deleted.

A durable request ID is not formation completion. The uploader follows the [task contract](api.md#tasks-and-completion).

`sessionEnded:true` is a session-close signal, not an end-of-every-turn flag. It closes an ordered input job; no experience pipeline runs. The collector also does not inject startup context. Use `get_context` through the agent, or implement a separate bounded SessionStart adapter.

### Other agent platforms

Any client supporting local stdio MCP can use [examples/mcp.json](../examples/mcp.json), with an absolute binary path and its own `--platform` label. A host wrapper can enqueue completed dialogue without relying on either client's private transcript format:

```sh
python3 /absolute/path/agent-memory-service/scripts/harness_memory.py \
  --platform my-agent enqueue <<'JSON'
{
  "scope": {"project": "my-project", "session": "session-123"},
  "turnId": "my-project/session-123/turn-456",
  "messages": [
    {"role": "user", "content": "Use the shared project store.", "timestamp": "2026-10-12T08:00:00Z"},
    {"role": "assistant", "content": "I will use that store for project memory.", "timestamp": "2026-10-12T08:00:10Z"}
  ]
}
JSON
```

Supply actual IDs and available source timestamps for real input. Mem0 accepts 1–128 ordered user/assistant messages per batch. Normal capture uses inference and can invoke the configured generation provider. Use `infer:false` for verbatim storage without generation. See [scope and idempotency](api.md#scope-and-idempotency) for retry rules.

## Troubleshooting

| Symptom | Check or action |
|---|---|
| MCP is listed but calls fail | Check daemon `/health`, absolute binary path, port, and stdio transport; the bridge does not start the daemon |
| Codex captures no turns | Put `notify` in user-level config, verify the Python path, preserve existing notification dispatch, and inspect the selected outbox |
| Session-end hook is skipped | Review/trust Codex hooks with `/hooks`; restart the client after edits |
| Claude Stop pairing fails | Start a new turn with capture hooks installed; verify required event fields and the matching project/platform |
| Outbox stays `pending` | Ensure one uploader is running and can reach the daemon; inspect `status` errors |
| Outbox stays `accepted` | Query `get_task`; inspect provider failures and unfinished index publication |
| Uploader reports a lock error | Another uploader holds the same outbox lock; keep one worker per queue |
| Cross-agent recall is empty | Check the same daemon, tenant/project IDs, record scope, completion state, token budget, and explicit cross-platform raw-input scope |

For daemon, model and storage problems, see [deployment troubleshooting](deployment.md#common-problems).
