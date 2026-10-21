# MCP and harness integration

This is the client setup and dialogue-capture guide. Start the daemon using
[deployment instructions](deployment.md) first. Request fields and response contracts belong to
[the API reference](api.md); implementation details belong to [architecture](architecture.md).

The bridge is a local stdio child process that forwards tools to the HTTP daemon. It does not start
the daemon, read chats automatically or inject context. The daemon's URL is not an HTTP MCP endpoint.
Each client starts its own bridge; a separate collector and uploader handle automatic capture.
Templates must be merged into the target client's existing configuration with absolute executable paths.
The [one-command installer](#one-command-install) performs that merge for Codex, Claude Code, WorkBuddy and
DeepSeek Harness.

## One-command install

`scripts/install.sh` registers the stdio bridge and, unless `--no-capture` is given, the
[dialogue collector](#automatically-save-conversations) in one agent platform; DeepSeek Harness gets the
bridge only. Without `--apply` it only
previews. With `--apply` it builds `target/fast/agent-memoryd` (as `make build`), checks the exact commands
it will install against a temporary mock daemon, then backs up and writes the platform's configuration.
It never starts or installs the daemon, model servers or uploader and never handles provider keys:
[deploy the daemon](deployment.md) first. After writing, it reports whether a daemon answers at `--url`
and whether an uploader is running, and prints the uploader command when none is.

Requirements: macOS or Linux, Rust/Cargo (or `--binary`), and Python 3.11+. The interpreter running the
installer is the one the hooks and `notify` use; pass `--python` to choose another. Run from the checkout:

| Platform | Command | Configuration written | Capture | Still to do by hand |
|---|---|---|---|---|
| Codex CLI / App | `bash scripts/install.sh --host codex --apply --project my-project` | `~/.codex/config.toml`: `[mcp_servers.shared-memory]` and `notify`; `~/.codex/hooks.json`: `SessionEnd` | `notify` on each completed turn; `SessionEnd` close marker | Restart Codex; trust the `SessionEnd` hook in `/hooks` (App: Settings → Hooks) |
| Claude Code | `bash scripts/install.sh --host claude-code --apply --project my-project --workspace /absolute/path/to/project` | `<project>/.mcp.json` and `<project>/.claude/settings.local.json`; `--scope user`: `~/.claude.json` and `~/.claude/settings.json` | `UserPromptSubmit`, `Stop`, `StopFailure`, `SessionEnd` hooks | Restart Claude Code; approve the project MCP server (`/mcp`) |
| WorkBuddy | `bash scripts/install.sh --host workbuddy --apply --project my-project` | `~/.workbuddy/mcp.json` (GUI), `~/.workbuddy/.mcp.json` (its bundled CodeBuddy CLI), hooks in `~/.workbuddy/settings.json` | `UserPromptSubmit`, `Stop`, `SessionEnd` hooks, [paired without turn IDs](#workbuddy-hooks) | Restart WorkBuddy; trust `shared-memory` in Connectors → Custom connector |
| DeepSeek Harness (Desktop app and `dsh`) | `bash scripts/install.sh --host deepseek-harness --apply` | `~/.dsh/cordis.patch.yml`: one marker-delimited `@deepseek-ai/dsh-mcp-client` row | None: [MCP tools only](#use-with-deepseek-harness); `--project` is refused | Restart DeepSeek Harness and start a new session |

On a platform with capture, keep one uploader running, for example in another terminal or under the
[process manager](deployment.md#keep-services-running). It must use the installer's `--url` (and
`--outbox`, if given); the installer prints the exact command when no uploader is running:

```sh
python3 /absolute/path/agent-memory-service/scripts/harness_memory.py \
  drain --url http://127.0.0.1:8765
```

Capture commands fix `--project`. Codex `notify`, WorkBuddy hooks and Claude Code `--scope user` are
user-level, so every session on that platform is filed under that project; Claude Code's default project
scope binds one project directory. Use the same `--project` and `--tenant` on every platform that should
share memory, and add the [shared memory workflow](#shared-memory-workflow) to `AGENTS.md`/`CLAUDE.md`.
MCP tools are not tied to the installed project.

| Option | Meaning |
|---|---|
| `--dry-run` (default) / `--apply` | Preview only / build, verify and write |
| `--project ID` | Stable shared project ID recorded with captured turns; prompted for when interactive; refused for DeepSeek Harness |
| `--tenant`, `--url` | Daemon store (`default`), recorded by the bridge and collector, and base URL (`http://127.0.0.1:8765`) for the bridge and the printed uploader command |
| `--no-capture` | MCP tools only; removes collector entries this installer manages |
| `--binary`, `--python`, `--outbox` | Use an existing `agent-memoryd`, another Python 3.11+, or a non-default outbox |
| `--codex-home`; `--scope`, `--workspace`, `--claude-config-dir`; `--workbuddy-home`, `--mcp-target`; `--dsh-home`, `--dsh-binary` | Platform locations and, for DeepSeek Harness, the `dsh` CLI that parses the result; host environment variables such as `CODEX_HOME` or `DSH_HOME` are not read |
| `--skip-host-check`, `--non-interactive`, `--home` | Skip the platform's own parser, never prompt, or install into another home (tests) |
| `--rollback [BACKUP]`, `--uninstall` | Restore the files of the latest (or given) backup; remove only this project's entries |

Every `--apply` runs these checks before writing anything; none reaches a model provider or the real store:

1. The bridge, started exactly as configured, completes the MCP handshake, lists the
   [eight tools](api.md#interfaces) and answers `get_context` from a temporary mock-retrieval daemon.
   A separate bridge process answers the 2026-07-28 `server/discover` version probe at once with
   `-32022` and the 2025-era versions it serves, so clients that probe first (DeepSeek Harness) fall back
   to `initialize`. The bridge, collector and uploader keep your proxy variables; they bypass proxies for
   a loopback daemon.
2. With capture, platform-shaped events run through the installed collector command into a temporary
   outbox; the real uploader submits them; a local generation stub forms one fact; the bridge finds it
   with `search_memories` under the configured tenant and project.
3. When the platform's CLI is installed, it parses a copy of the planned files in a temporary home:
   `codex mcp get` and `codex app-server` `hooks/list` for Codex, `claude mcp get` for Claude Code, and
   `dsh --profile web --dump-config` for DeepSeek Harness (found on `PATH` or in the Desktop app), which
   must compose the `shared-memory` row exactly once, as written, with no warning the current file lacks.
   WorkBuddy has no such CLI; its JSON is checked by the installer only.

Merges keep other servers, hooks, settings, comments and ordering. In the files it writes, entries
created from the [examples](../examples) are adopted and replaced rather than duplicated; an entry counts
as the installer's only when it runs the collector itself, so a dispatcher that forwards to
`harness_memory.py` stays yours. Reruns with the same options write nothing, and a file that removing
the installer's entries leaves empty is deleted. The installer refuses, without writing, a different server already named
`shared-memory`, a Codex `notify` that runs another program (chain it to `codex-notify` yourself or use
`--no-capture`), Codex hooks defined under `[hooks]` in `config.toml`, TOML forms it cannot edit safely,
symlinked configuration files, a Claude Code local-scope `shared-memory` for the workspace, which
would take precedence, and a DeepSeek Harness `cordis.patch.yml` that is not a plain top-level YAML list,
has damaged installer markers or already holds another `shared-memory` row. It cannot see a trusted
project's `.codex/config.toml`, a project-level `.codex/hooks.json` collector hook or another project's
`.mcp.json`; remove such entries yourself.
Backups live in `~/.local/state/agent-memory-service/installations/<platform>/`, one history per
Claude Code project or user configuration. `--rollback` undoes the latest installation for the same
target and refuses when a file changed after it, as happens when Codex records hook trust or Claude Code
updates `~/.claude.json`; use `--uninstall` then. Neither touches daemon data or the outbox.

Verification scope: the offline checks above and the CLI parsing checks (Codex CLI 0.163.0, Claude Code
2.1.305, `dsh` from DeepSeek Harness 0.2.1-rc.2) have run on macOS; without a platform's CLI the installer
prints a note instead. WorkBuddy hook payloads follow events recorded from WorkBuddy 5.8.3 by the
[pd-memory](https://github.com/RuisongZhou/pd-memory) integration. For DeepSeek Harness, its bundled MCP
client SDK (`@modelcontextprotocol/client` 2.2.0) was connected to the bridge once by hand: probe,
`-32022`, `initialize` 2025-11-25, eight tools. No installation has been exercised in a live client
session with a real provider; confirm with the [sharing check](#shared-memory-workflow).

## Use with Codex

This and the next section describe the configuration the installer writes, for manual setup or review.
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

## Use with DeepSeek Harness

DeepSeek Harness (`dsh` and the Desktop app) gets the eight tools, named `mcp__shared-memory__<tool>`,
and no automatic capture: its Claude Code and Codex hook bridges pass no transcript and no final reply
and send no `SessionEnd`, so the collector would have nothing to save. Ask the agent to save with
`add_memories`, and put the [shared memory workflow](#shared-memory-workflow) in its instructions.

`~/.dsh/cordis.patch.yml` is the home-level patch list that every profile, the Desktop app's included,
applies after its own layers. The installer keeps one block in it and leaves every other byte alone:

```yaml
# >>> agent-memory-service: managed by scripts/install.sh --host deepseek-harness >>>
- insert:
    - id: mcp-shared-memory
      name: '@deepseek-ai/dsh-mcp-client'
      config:
        serverName: "shared-memory"
        transport: "stdio"
        command: "/absolute/path/agent-memory-service/target/fast/agent-memoryd"
        args: ["mcp", "--url", "http://127.0.0.1:8765", "--platform", "deepseek-harness"]
        env: {"MEMORY_TENANT": "default"}
        toolCallTimeoutMs: 180000
# <<< agent-memory-service <<<
```

To add it by hand, keep the two marker lines so a later install or `--uninstall` manages the block. dsh
refuses to start when this file is empty or holds anything but a top-level YAML list, so the installer
replaces a lone `[]` with the block and writes `[]` back when uninstalling leaves no item. The Desktop
app manages its own profile files under `~/.dsh/profiles/desktop/`; the installer does not touch them.

- `toolCallTimeoutMs` raises dsh's 60-second default to the 180 seconds used for Codex and WorkBuddy.
- dsh starts the bridge with an environment stripped of `DSH_*` and credential-like variables plus the
  row's `env`. The bridge needs no key; the daemon holds the provider configuration.
- dsh's MCP client first probes with `server/discover` (protocol 2026-07-28). The bridge serves the
  2025-era protocol and answers that probe with `-32022`, after which dsh connects with `initialize`.
- With the official DeepSeek API, dsh uploads the session log, tool results such as retrieved memories
  included, unless **Settings → General → Upload Session Log when using the official model API** is off.

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

For session closure, merge [examples/codex.hooks.json](../examples/codex.hooks.json) into user-level `~/.codex/hooks.json`, as the installer does, replacing the script path and project ID. This queues a `SessionEnd` marker. Review and trust the installed definition using `/hooks`; untrusted or changed hooks are skipped. See [official Codex hooks documentation](https://learn.chatgpt.com/docs/hooks). This hook needs an already captured turn; it does not replace `notify`.

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

### WorkBuddy hooks

WorkBuddy and its bundled CodeBuddy CLI use the Claude Code hook dialect but send no `prompt_id` or
`turn_id`. Their collector command ends with `hook --turn-id-field none`: each `UserPromptSubmit` opens a
turn with a generated ID stored in the outbox, and `Stop` pairs with the session's open prompt. A prompt
without text, such as an image only, closes the open turn without starting one. A later `Stop` without
an open prompt, such as a continued reply, is ignored instead of failing; only the first final reply of a
turn is captured. The GUI reads only user-level `~/.workbuddy/settings.json` hooks, and switching sessions
or quitting may not send `SessionEnd`; completed turns are still queued on `Stop`. Use the
[installer](#one-command-install) or copy its hook entries; there is no separate template. Standalone
CodeBuddy Code keeps its configuration in `~/.codebuddy` and is not covered by `--host workbuddy`.

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
| Installer refuses to write | Read the `[error]` line: it names the conflicting entry; nothing was changed. Fix it or edit the configuration by hand as in the sections above |
| MCP is listed but calls fail | Check daemon `/health`, absolute binary path, port, and stdio transport; the bridge does not start the daemon |
| Codex captures no turns | Put `notify` in user-level config, verify the Python path, preserve existing notification dispatch, and inspect the selected outbox |
| Session-end hook is skipped | Review/trust Codex hooks with `/hooks`; restart the client after edits |
| Claude Stop pairing fails | Start a new turn with capture hooks installed; verify required event fields and the matching project/platform |
| Outbox stays `pending` | Ensure one uploader is running and can reach the daemon; inspect `status` errors |
| Outbox stays `accepted` | Query `get_task`; inspect provider failures and unfinished index publication |
| Uploader reports a lock error | Another uploader holds the same outbox lock; keep one worker per queue |
| DeepSeek Harness lists no `mcp__shared-memory__*` tools | Restart it and start a new session; rerun the installer, which reports whether `dsh --dump-config` reads the row; check the daemon URL |
| Cross-agent recall is empty | Check the same daemon, tenant/project IDs, record scope, completion state, token budget, and explicit cross-platform raw-input scope |

For daemon, model and storage problems, see [deployment troubleshooting](deployment.md#common-problems).
