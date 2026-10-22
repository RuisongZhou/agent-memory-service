# MCP and harness integration

This is the client setup and dialogue-capture guide. The [one-command installer](#one-command-install)
deploys the daemon and connects a client in one step; for a manual setup, start the daemon using
[deployment instructions](deployment.md) first. Request fields and response contracts belong to
[the API reference](api.md); implementation details belong to [architecture](architecture.md).

The bridge is a local stdio child process that forwards tools to the HTTP daemon. It does not start
the daemon, read chats automatically or inject context. The daemon's URL is not an HTTP MCP endpoint.
Each client starts its own bridge; a collector hook and an uploader handle automatic capture, and
optional [recall hooks](#automatic-recall) inject memory into the agent's context. The bridge, the
collector and recall are all subcommands of the one `agent-memoryd` binary, and the daemon itself is
the uploader when started with `--outbox`.
Templates must be merged into the target client's existing configuration with absolute executable paths.
The [one-command installer](#one-command-install) performs that merge for Codex, Claude Code, WorkBuddy and
DeepSeek Harness, and installs and starts the daemon they share.

## One-command install

`scripts/install.sh` deploys the daemon and connects one agent platform to it. It registers the stdio
bridge, unless `--no-capture` is given the [dialogue collector](#automatically-save-conversations), and
with `--recall` the [automatic recall](#automatic-recall) hooks; DeepSeek Harness gets the bridge only.
Without `--apply` it only previews. With `--apply`, in this order:

1. It asks for the model API key, hidden, when the daemon's env file has none yet (see below).
2. It builds `agent-memoryd` (as `make build`) and copies it to `~/.local/bin/agent-memoryd`, so the
   installed commands survive `cargo clean` or a moved checkout. `--binary PATH` uses an existing
   executable as is, such as a [release binary](deployment.md#release-archive) of the same version.
3. It checks the exact commands it will register, run from the installed binary, against a temporary
   mock daemon and the platform's own CLI ([checks](#installer-checks)).
4. It backs up and writes the platform's configuration.
5. It installs the daemon as a LaunchAgent or systemd user service with
   [`agent-memoryd service install --apply --start`](deployment.md#install-the-service), with `--outbox`
   when the platform captures dialogue, and waits until `/health` answers.

If a step before the last fails, the installer puts back the configuration files, the binary and the
env file it changed. A service that does not start leaves the written configuration in place: the
error names the cause, typically the key, and a rerun continues from there.

The env file is `~/.config/agent-memory-service/.env` (directory 0700, file 0600), created from
[.env.example](../.env.example) with the hosted defaults; an installed service keeps the env file it
already uses, and `--env-file` names another. The key goes into `QWEN_API_KEY`, which the daemon also
uses for the hosted retrieval models; it comes from `--key-file`, else the environment variable named by
`--api-key-env` (`QWEN_API_KEY`), else the hidden prompt, and never from a command-line argument or the
output. An env file that already has a key is reused untouched, unless `--key-file` replaces the key;
an edited file is first copied to `~/.local/state/agent-memory-service/installations/env/`. Other
settings, such as another generation model or [self-hosted retrieval models](self-hosted-models.md),
are edited in that file; the service reads it at each start.

The binary, env file and service are shared: a later run for another platform reuses them, so only the
first run asks for the key. A rerun replaces the binary when the build changed and then restarts the
service. An installed service keeps its data directory, env file, port, concurrency and generation
guard; only its binary changes, and capture adds `--outbox`. A unit that `service install` did not write
and a daemon started by hand are left alone and used as they are. `--no-service` registers the platform
only, as for a daemon you run yourself; the installer then needs no key. A `--url` other than a loopback
`http://` URL with a port is never served by the installed service.

Requirements: macOS or Linux (on Windows, WSL), a checkout of this repository, Python 3.11+ to run the
installer itself (the only component that needs Python), Rust/Cargo to build the binary unless
`--binary` is given, and a model API key unless `--no-service` is given. The bridge, the hooks and
hooks all run `agent-memoryd`, so the agent host needs no Python. Run from the checkout:

| Platform | Command | Configuration written | Capture | Still to do by hand |
|---|---|---|---|---|
| Codex CLI / App | `bash scripts/install.sh --host codex --apply --project my-project` | `~/.codex/config.toml`: `[mcp_servers.shared-memory]`; `~/.codex/hooks.json`: `UserPromptSubmit`, `Stop`, `SessionEnd` | [Hooks](#codex-hooks) paired by `turn_id`; `notify` is left to other programs | Restart Codex; trust the three collector hooks in `/hooks` (App: Settings → Hooks); nothing is captured before that |
| Claude Code | `bash scripts/install.sh --host claude-code --apply --project my-project --workspace /absolute/path/to/project` | `<project>/.mcp.json` and `<project>/.claude/settings.local.json`; `--scope user`: `~/.claude.json` and `~/.claude/settings.json` | `UserPromptSubmit`, `Stop`, `StopFailure`, `SessionEnd` hooks | Restart Claude Code; approve the project MCP server (`/mcp`) |
| WorkBuddy | `bash scripts/install.sh --host workbuddy --apply --project my-project` | `~/.workbuddy/mcp.json` (GUI), `~/.workbuddy/.mcp.json` (its bundled CodeBuddy CLI), hooks in `~/.workbuddy/settings.json` | `UserPromptSubmit`, `Stop`, `SessionEnd` hooks, [paired without turn IDs](#workbuddy-hooks) | Restart WorkBuddy; trust `shared-memory` in Connectors → Custom connector |
| DeepSeek Harness (Desktop app and `dsh`) | `bash scripts/install.sh --host deepseek-harness --apply` | `~/.dsh/cordis.patch.yml`: one marker-delimited `@deepseek-ai/dsh-mcp-client` row | None: [MCP tools only](#use-with-deepseek-harness); `--project` is refused | Restart DeepSeek Harness and start a new session |

With `--recall`, Codex, Claude Code and WorkBuddy also get a `SessionStart` and a `UserPromptSubmit`
recall hook (Codex: trust both in `/hooks` as well). `--recall` needs `--project` and a loopback `--url`;
it is refused for DeepSeek Harness, which gets MCP tools only. Every prompt becomes a search query, which
the default hosted retrieval models receive; see [privacy](#automatic-recall).

On a platform with capture, captured turns also need an uploader. The installed service is one: it runs
the daemon with `--outbox` on the outbox the collector writes, the default one unless the installer's
`--outbox` names another. With `--no-service` or a daemon started by hand, start that daemon with
`--outbox` ([deployment](deployment.md#5-start-the-shared-daemon)), or keep a standalone uploader running
against the installer's `--url`; the installer prints the exact command when no uploader holds the
outbox:

```sh
"$HOME/.local/bin/agent-memoryd" outbox drain --url http://127.0.0.1:8765
```

Capture and recall commands fix `--project`. Codex hooks, WorkBuddy hooks and Claude Code `--scope user` are
user-level, so every session on that platform is filed under that project; Claude Code's default project
scope binds one project directory. Use the same `--project` and `--tenant` on every platform that should
share memory, and add the [shared memory workflow](#shared-memory-workflow) to `AGENTS.md`/`CLAUDE.md`.
MCP tools are not tied to the installed project.

| Option | Meaning |
|---|---|
| `--dry-run` (default) / `--apply` | Preview only / build, verify, write and start the daemon service |
| `--project ID` | Stable shared project ID recorded with captured turns; prompted for when interactive; refused for DeepSeek Harness |
| `--tenant`, `--url` | Daemon store (`default`), recorded by the bridge and collector, and base URL (`http://127.0.0.1:8765`) for the bridge, the service's port and the printed uploader command |
| `--no-capture` | No dialogue collector; removes collector entries this installer manages |
| `--recall` | Add the [automatic recall](#automatic-recall) hooks (not for DeepSeek Harness); without it, a rerun removes recall entries this installer manages |
| `--no-service` | Register the platform only: no env file, key or daemon service |
| `--env-file`, `--key-file`, `--api-key-env NAME` | The daemon's env file; a file whose key replaces the env file's; the variable read when the env file has no key (`QWEN_API_KEY`) |
| `--verify-model` | Also check the env file's real models in a temporary store (below); makes a few model calls |
| `--binary`, `--outbox` | Use an existing `agent-memoryd` as is (it must have the `hook` commands and `service install --start`), or a non-default outbox |
| `--codex-home`; `--scope`, `--workspace`, `--claude-config-dir`; `--workbuddy-home`, `--mcp-target`; `--dsh-home`, `--dsh-binary` | Platform locations and, for DeepSeek Harness, the `dsh` CLI that parses the result; host environment variables such as `CODEX_HOME` or `DSH_HOME` are not read |
| `--skip-host-check`, `--non-interactive`, `--home` | Skip the platform's own parser, never prompt (fail on a missing project ID or key), or install into another home (tests; needs `--no-service`) |
| `--rollback [BACKUP]`, `--uninstall` | Restore the files of the latest (or given) backup; remove only this project's entries. Both keep the binary, env file, service, memory data and outbox for the other platforms |

### Installer checks

Every `--apply` runs these checks before writing the platform's configuration; none of the first four
reaches a model provider or the real store:

1. The bridge, started exactly as configured, completes the MCP handshake, lists the
   [eight tools](api.md#interfaces) and answers `get_context` from a temporary mock-retrieval daemon.
   A separate bridge process answers the 2026-07-28 `server/discover` version probe at once with
   `-32022` and the 2025-era versions it serves, so clients that probe first (DeepSeek Harness) fall back
   to `initialize`. The bridge, collector and uploader keep your proxy variables; they bypass proxies for
   a loopback daemon.
2. With capture, platform-shaped events run through the installed collector command into a temporary
   outbox; the temporary daemon's own uploader (`serve --outbox`) submits them; a local generation stub
   forms one fact; the bridge finds it with `search_memories` under the configured tenant and project.
   Codex's events include a turn that ends without a final message, which must queue nothing.
3. With recall (after forming the fact through the bridge when capture is off), platform-shaped
   `SessionStart` and `UserPromptSubmit` events run through the installed recall command. Each output
   must be one JSON object of the shape Codex's hook schema allows, and both injected contexts must
   contain the formed fact. A prompt in the session that already saw it must inject nothing, and so must
   a prompt when no daemon answers.
4. When the platform's CLI is installed, it parses a copy of the planned files in a temporary home:
   `codex mcp get` and `codex app-server` `hooks/list` (collector and recall hooks) for Codex,
   `claude mcp get` for Claude Code, and `dsh --profile web --dump-config` for DeepSeek Harness (found
   on `PATH` or in the Desktop app), which must compose the `shared-memory` row exactly once, as written,
   with no warning the current file lacks.
   WorkBuddy has no such CLI; its JSON is checked by the installer only.
5. With `--verify-model`, a temporary daemon started with the real env file on a temporary data
   directory forms one synthetic fact from a two-message dialogue and must find it with
   `search_memories`. This makes real calls: the retrieval-model probe at startup, one formation (at
   most four generation attempts) and two embeddings. Without it, the service's start checks the
   retrieval models only; the generation key is first used by the first captured turn.

### Merges, backups and rollback

Merges keep other servers, hooks, settings, comments and ordering. In the files it writes, entries
created from the [examples](../examples) are adopted and replaced rather than duplicated; an entry counts
as the installer's only when it runs `agent-memoryd hook capture`, `codex-notify` or `recall` itself, or
the Python script an earlier installer wrote instead (see [migration](#migrate-from-the-python-hook-scripts)),
so a dispatcher that forwards to them stays yours. Reruns with the same options write nothing, and a file
that removing the installer's entries leaves empty is deleted. The installer refuses, without writing, a different server already named
`shared-memory`, Codex hooks defined under `[hooks]` in `config.toml`, TOML forms it cannot edit safely,
symlinked configuration files, a Claude Code local-scope `shared-memory` for the workspace, which
would take precedence, and a DeepSeek Harness `cordis.patch.yml` that is not a plain top-level YAML list,
has damaged installer markers or already holds another `shared-memory` row. It cannot see a trusted
project's `.codex/config.toml`, a project-level `.codex/hooks.json` collector hook or another project's
`.mcp.json`; remove such entries yourself.
Backups live in `~/.local/state/agent-memory-service/installations/<platform>/`, one history per
Claude Code project or user configuration. `--rollback` undoes the latest installation for the same
target and refuses when a file changed after it, as happens when Codex records hook trust or Claude Code
updates `~/.claude.json`; use `--uninstall` then. Neither touches the binary, the env file, the daemon
service, its data or the outbox, which the other platforms share; to remove the service, stop it and
run `agent-memoryd service uninstall --apply` ([deployment](deployment.md#install-the-service)).

Verification scope: the offline checks above and the CLI parsing checks (Codex CLI 0.163.0, Claude Code
2.1.305, `dsh` from DeepSeek Harness 0.2.1-rc.2) have run on macOS; without a platform's CLI the installer
prints a note instead. Recall output follows the hook schemas embedded in Codex CLI 0.163.0 and the
Claude Code hooks reference. WorkBuddy hook payloads follow events recorded from WorkBuddy 5.8.3 by the
[pd-memory](https://github.com/RuisongZhou/pd-memory) integration. For DeepSeek Harness, its bundled MCP
client SDK (`@modelcontextprotocol/client` 2.2.0) was connected to the bridge once by hand: probe,
`-32022`, `initialize` 2025-11-25, eight tools. The service step's launchctl and systemctl commands
are unit-tested as a plan; CI installs every platform with `--no-service`. No installation has been
exercised in a live client session with a real provider; confirm with `--verify-model` and the
[sharing check](#shared-memory-workflow).

## Use with Codex

This and the next section describe the configuration the installer writes, for manual setup or review.
Install Codex separately and start the shared daemon first. The templates run the checkout's build:
replace `/absolute/path/agent-memory-service/target/fast/agent-memoryd` with its actual absolute path,
or with the installed `~/.local/bin/agent-memoryd` (as an absolute path) that the installer writes.

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

Add the [shared memory workflow](#shared-memory-workflow) to the target project's `AGENTS.md`. For automatic capture, follow the separate [Codex hooks setup](#codex-hooks). A local stdio setup requires the executable and daemon to be reachable from the host running the client; a cloud container has its own loopback address.

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
At session start or resume, call get_context with a modest tokenBudget, unless
shared-memory context was already loaded automatically.
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
With [automatic recall](#automatic-recall) installed, the session-start `get_context` and a search per
prompt happen without the agent; the remaining steps still apply.

For example, ask either agent: “Use shared-memory to load context for project `my-project`, then search for our previous storage decision and show its source.” To save a behavior requirement, ask: “Remember as an instruction for `my-project`: include sources when reporting project decisions.” These prompts use tools; they do not require the agent to copy its complete conversation.

To verify sharing, save that project instruction in Codex, then ask Claude Code to call `get_context` for the same project. It should see the instruction despite using a different platform/session. Repeat after a daemon restart with the same data directory. For inferred facts, first check the write receipt and then use `search_memories`; instructions are read through `get_context` rather than ordinary fact search.

## Automatically save conversations

Automatic saving requires **both a collector and a running uploader**. The collector (`agent-memoryd hook capture`, run by Codex, Claude Code and WorkBuddy hooks) captures completed user/assistant text into a local durable queue without waiting for the daemon. The uploader, the daemon itself when started with `--outbox` or a standalone `agent-memoryd outbox drain`, submits it without involving the agent's main model. Capture does not include tool traces, reasoning, images, or every possible client/subagent lifecycle.

Use the same `--project`, `--platform`, and `--tenant` in capture commands as in the corresponding MCP setup. `--tenant default` is the collector default; `MEMORY_TENANT` configures the MCP bridge and is not read by the collector. If you change `--outbox`, every collector, uploader, and status command must use that same path. One outbox can contain both Codex and Claude Code records for one daemon.

### Codex hooks

Codex captures turns through hooks, as Claude Code does. Merge
[examples/codex.hooks.json](../examples/codex.hooks.json) into user-level `~/.codex/hooks.json`, as the
installer does, replacing the binary path and project ID:

| Event | Adapter behavior |
|---|---|
| `UserPromptSubmit` | Stores the prompt under Codex's `turn_id` |
| `Stop` | Pairs that prompt with `last_assistant_message` and queues a completed turn; a turn that ended without a final message (`null`) queues nothing |
| `SessionEnd` | Clears incomplete input and queues a close marker for a captured session |

Codex runs a hook only after you trust it: review and trust the three collector hooks with `/hooks`
(App: Settings → Hooks). Codex records trust per command, so changing `--project`, the binary path or
another option needs trust again, and untrusted or changed hooks are skipped. Start a new turn after
installing; a `Stop` from a turn whose prompt was not captured cannot be paired. The payload fields come
from the hook input schemas embedded in Codex CLI 0.163.0 (`turn_id` is Codex's extension). See the
[official Codex hooks documentation](https://learn.chatgpt.com/docs/hooks).

Capture leaves `notify` alone; Codex runs only one notify program, and Codex Computer Use, for example,
uses it. Earlier installers captured turns with `notify = [<agent-memoryd>, "hook", "codex-notify", ...]`,
wrapping a notify program already set (after `--`). A rerun of the installer, `--no-capture` or
`--uninstall` removes that entry and puts a wrapped program back. `agent-memoryd hook codex-notify`
remains for such setups: it captures the `agent-turn-complete` event Codex appends as the last
argument, and after `--` it first starts the program it wraps with the same event, waiting up to
10 seconds for it.

### Claude Code hooks

Merge the `hooks` object from [examples/claude-code.settings.json](../examples/claude-code.settings.json) into the target project's `.claude/settings.local.json`. Replace every binary path and project ID; preserve existing hooks and permissions. The template sets a three-second timeout and runs the collector synchronously only for local SQLite writes.

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
`turn_id`. Their collector command ends with `--turn-id-field none`: each `UserPromptSubmit` opens a
turn with a generated ID stored in the outbox, and `Stop` pairs with the session's open prompt. A prompt
without text, such as an image only, closes the open turn without starting one. A later `Stop` without
an open prompt, such as a continued reply, is ignored instead of failing; only the first final reply of a
turn is captured. The GUI reads only user-level `~/.workbuddy/settings.json` hooks, and switching sessions
or quitting may not send `SessionEnd`; completed turns are still queued on `Stop`. Use the
[installer](#one-command-install) or copy its hook entries; there is no separate template. Standalone
CodeBuddy Code keeps its configuration in `~/.codebuddy` and is not covered by `--host workbuddy`.

### Run and inspect the uploader

Start the daemon with `--outbox` ([deployment](deployment.md#5-start-the-shared-daemon)); it then uploads
for as long as it runs. Without that option, run a standalone uploader in a separate terminal or
supervised process:

```sh
/absolute/path/agent-memory-service/target/fast/agent-memoryd \
  outbox drain --url http://127.0.0.1:8765
```

By default, the outbox is `~/.local/share/agent-memory-service/harness-outbox.sqlite3`. The collector creates a private database with WAL and FULL synchronous commits; new parent directories use mode 0700 and the database uses 0600. Keep it outside version control: it contains raw conversation text and receipts.

After completing a turn in the client, inspect local status:

```sh
/absolute/path/agent-memory-service/target/fast/agent-memoryd outbox status
```

Status output includes identifiers, receipts, and errors without dumping chat text. An empty list means no turn was captured in that outbox. The daemon's uploader makes a pass every second and calls the interface in-process; the standalone one scans every five seconds with a five-second HTTP timeout. Both hold the same process lock (`<outbox>.uploader.lock`), so only one uploader handles a queue: a daemon started while another uploader runs logs that it is waiting and takes over once the lock is released. `outbox drain --once` performs one upload/poll pass; it does not wait for every request to finish.

| Outbox status | Meaning |
|---|---|
| `pending` | Stored locally; awaiting submission or a retry after a transient failure |
| `accepted` | Daemon returned a request ID; uploader is polling formation/visibility |
| `completed` | Receipt satisfies the API completion contract |
| `rejected` | Permanent HTTP request error; input retained for investigation |
| `failed` | Formation failed; input and request ID retained |

Transient connection errors, lost responses, HTTP 408/429, and 5xx retry the same payload and turn ID. The service deduplicates by `platform + turnId` within a tenant; retries preserve the first input and timestamps. After acceptance, the uploader polls instead of resubmitting. Failed/rejected records are not automatically resubmitted, and terminal records are not automatically deleted.

A durable request ID is not formation completion. The uploader follows the [task contract](api.md#tasks-and-completion).

`sessionEnded:true` is a session-close signal, not an end-of-every-turn flag. It closes an ordered input job; no experience pipeline runs. The collector does not inject context; that is the separate [recall hooks](#automatic-recall)' job.

### Other agent platforms

Any client supporting local stdio MCP can use [examples/mcp.json](../examples/mcp.json), with an absolute binary path and its own `--platform` label. A host wrapper can enqueue completed dialogue without relying on either client's private transcript format:

```sh
/absolute/path/agent-memory-service/target/fast/agent-memoryd \
  outbox enqueue --platform my-agent <<'JSON'
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

## Automatic recall

`agent-memoryd hook recall` injects shared memory through the hooks of Codex, Claude Code and WorkBuddy.
The installer adds it with `--recall`, as its own hook groups next to the collector's. Capture is
unchanged and recall works without it. DeepSeek Harness gets MCP tools only, and the installer refuses
`--recall` there. Recall is its own hook command because its failure contract is the reverse of the
collector's: the collector exits 1 so faults surface, while recall always exits 0 so even a broken
command cannot block a prompt. Neither ever exits 2, which blocks a prompt, not even for invalid
arguments. Recall also needs neither the outbox nor a non-loopback path.
Each hook makes one request to the daemon and prints the result as `hookSpecificOutput.additionalContext`:

| Event | Request | Injected |
|---|---|---|
| `SessionStart` (any source: startup, resume, clear, compact, fork) | `get_context` for the project and native session, `tokenBudget` 1000 | The context text; also starts the session's record of shown memories over |
| `UserPromptSubmit` | `search_memories` with the prompt (at most 2000 characters; the `<user_query>` part when the host wraps it), the project, `topK` 5, `tokenBudget` 400 | Results this session has not been shown yet, one line each with its date, platform and ID; an empty prompt searches nothing |

The injected block is framed for the agent. Only the explicit-instructions section of `get_context`
holds behavior requirements, subject to current instructions. All other sections and every search result
are labeled historical reference, not instructions. Search results are fact records: instructions come
only through `get_context`. Stored text is flattened to one line and cannot close the
`<shared-memory>` wrapper.

| Constraint | Behavior |
|---|---|
| Never block or fail the host | Every outcome prints exactly one JSON object and exits 0. Nothing to inject, a daemon that is down, slow or failing, invalid input, invalid arguments or a non-loopback URL all print `{}`, with only the error type on stderr. It never exits 2, which would block a prompt, and a panic still prints `{}`. Codex's output schema allows no extra fields; `hookSpecificOutput` appears only on these two events, and the collector's `Stop`/`SessionEnd` output stays `{}` |
| Time | `--timeout` (2 s) bounds the whole recall; the installed hook timeout (5 s) is the host's backstop |
| Tokens | The daemon packs both budgets. `--max-chars` (6000) also caps the block in whole lines, marked `[truncated]`, below Claude Code's 10,000-character limit for one `additionalContext` |
| Repeats | A per-session record of shown memory IDs (`~/.local/state/agent-memory-service/recall-state.sqlite3`, mode 0600, IDs only, pruned after 30 days) keeps prompts from re-injecting what this session was already shown. If the record fails, recall injects unfiltered; each prompt still stays within budget |
| Scope | `--project`, `--tenant` and `--platform` come from the installed command, as for capture; the session comes from the event. Search covers that project plus visible global records |
| Network | The URL must be loopback (`127.0.0.1`, `localhost`, `::1`), and proxy variables are ignored; the installer refuses `--recall` with another `--url` |
| Daemon down | Nothing is injected and the prompt proceeds; recall resumes when the daemon answers |

Privacy: every prompt goes to the local daemon as a search query, even without capture, and the daemon
records it in its retrieval log, which is readable through `POST /admin/export`. The daemon embeds the
query with its [retrieval models](deployment.md#4-configure-the-retrieval-models). With the default
hosted provider (`api`), **every prompt (up to 2000 characters) is sent to the hosted embedding API**,
and, when Mem0 reranking is enabled, to the rerank API together with the candidate memories. Only
`self-hosted` models on this machine keep prompts local. `SessionStart` (`get_context`) makes no model
call. The installer notes which provider the daemon at `--url` uses.

Hosts differ in when the hooks run. Codex runs them only after you trust them in `/hooks` (App:
Settings → Hooks). Claude Code runs the collector and recall `UserPromptSubmit` hooks in parallel and
adds every returned context. WorkBuddy sends `SessionStart` without `cwd`, and its order relative to the
first prompt is not guaranteed; a late `SessionStart` starts the record over, so the next prompt may
repeat what the first one injected. Change
`--context-budget`, `--search-budget`, `--top-k`, `--max-chars` or `--timeout` by editing the installed
command, and keep `--timeout` below the hook timeout. A rerun of the installer replaces edited commands.
To see what a hook would inject, run it by hand:

```sh
echo '{"hook_event_name":"SessionStart","session_id":"manual-check"}' | \
  /absolute/path/agent-memory-service/target/fast/agent-memoryd hook recall \
  --project my-project --platform codex --state /tmp/recall-check.sqlite3
```

### Migrate from the Python hook scripts

Installers before `agent-memoryd hook` wrote hooks that run `scripts/harness_memory.py` and
`scripts/harness_recall.py` with Python 3.11. Those scripts stay in the repository with the same
contracts, outbox and recall record, so existing entries keep working; deleting them would make Python
exit 2 and block prompts. Rerun the installer with the same options to replace every entry with
`agent-memoryd hook` commands. A Python `drain` and a daemon started with `--outbox` exclude each other
through the outbox lock, so either can finish the queue the other started; stop the Python uploader
once the daemon uploads.

## Troubleshooting

| Symptom | Check or action |
|---|---|
| Installer refuses to write | Read the `[error]` line: it names the conflicting entry; nothing was changed. Fix it or edit the configuration by hand as in the sections above |
| MCP is listed but calls fail | Check daemon `/health`, absolute binary path, port, and stdio transport; the bridge does not start the daemon |
| Codex captures no turns | Trust the `UserPromptSubmit`, `Stop` and `SessionEnd` collector hooks in `/hooks` (again after the command changed), start a new turn, verify the `agent-memoryd` path, and inspect the selected outbox |
| A Codex collector or recall hook is skipped | Review/trust Codex hooks with `/hooks`; restart the client after edits |
| Claude Stop pairing fails | Start a new turn with capture hooks installed; verify required event fields and the matching project/platform |
| Outbox stays `pending` | Start the daemon with `--outbox` (or run `outbox drain`) on the same outbox path; inspect `outbox status` errors |
| Outbox stays `accepted` | Query `get_task`; inspect provider failures and unfinished index publication |
| Uploader reports a lock error, or the daemon log says another uploader holds the outbox lock | Another uploader (a Python `drain`, `outbox drain` or a second daemon) holds the same outbox lock; keep one per queue |
| DeepSeek Harness lists no `mcp__shared-memory__*` tools | Restart it and start a new session; rerun the installer, which reports whether `dsh --dump-config` reads the row; check the daemon URL |
| Cross-agent recall is empty | Check the same daemon, tenant/project IDs, record scope, completion state, token budget, and explicit cross-platform raw-input scope |
| Automatic recall injects nothing | Check the daemon at the loopback `--url`, trust the recall hooks in Codex, and run the hook by hand as in [automatic recall](#automatic-recall); stderr names the error type |

For daemon, model and storage problems, see [deployment troubleshooting](deployment.md#common-problems).
