# Agent Memory Service

[![CI](https://github.com/RuisongZhou/agent-memory-service/actions/workflows/ci.yml/badge.svg)](https://github.com/RuisongZhou/agent-memory-service/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

English | [简体中文](README.zh-CN.md)

**Shared memory for your local AI agents.** Carry project decisions, facts and explicitly saved instructions between
Codex, Claude Code, WorkBuddy and DeepSeek Harness.

Connect your clients to the same local service and use the same project ID. Each client can retrieve shared memories,
inspect their sources and correct outdated information.

With capture and recall hooks enabled, a typical workflow looks like this:

> In Codex, you discuss and choose a storage design. The conversation is captured and processed into memory.
>
> Later, in Claude Code, you ask about that decision. The recall hook searches the project's memories and adds relevant
> results to the conversation. The agent can inspect the original source before answering.

Existing client memories and past conversations can be imported separately.

## What you can do

- **Continue work across clients** with automatic capture and recall, without copying context between conversations.
- **Check and correct memories:** trace a record to its source, add a correction or retract it.
- **Import existing history:** review proposed memories before importing them, with rollback support.

### Supported clients

| Client | MCP tools | Automatic capture | Automatic recall |
|---|:---:|:---:|:---:|
| Codex CLI / App | ✓ | ✓ | ✓ |
| Claude Code | ✓ | ✓ | ✓ |
| WorkBuddy | ✓ | ✓ | ✓ |
| DeepSeek Harness / `dsh` | ✓ | — | — |

MCP exposes tools that the agent can call. Hooks trigger capture and recall at specific conversation events. Without
hooks, tool use depends on the agent's decisions, your requests and project instructions.

Other clients that support local stdio MCP can connect through [examples/mcp.json](examples/mcp.json) and
[other agent platforms](docs/mcp-harness.md#other-agent-platforms).

## Before you install

This is an early-stage project for **one user on one machine**, running on macOS or Linux. On Windows, use WSL.

Memory is stored locally, but **the default model configuration sends relevant text to hosted model providers**:

- Memory formation sends submitted text to the generation model.
- Retrieval uses an embedding model and, optionally, a reranker.
- Automatic recall uses each prompt as a search query, capped at 2,000 characters.

To run the service's model processing locally, configure both a local OpenAI-compatible generation endpoint (see
[configuration](docs/api.md#configuration)) and [self-hosted retrieval models](docs/self-hosted-models.md). Your agent
client's own model configuration is separate.

The daemon listens on loopback (`127.0.0.1:8765`). Project and platform labels organize requests; they are not
authentication or access-control boundaries.

## Quick start

### 1. Install prerequisites

The source installation requires:

- Git
- Rust stable with Cargo and a C compiler toolchain
- Python 3.11+ for the installer
- SurrealDB 3.2 or a later 3.x release
- A model API key; the default configuration uses Qwen MaaS for generation, embeddings and reranking

Install SurrealDB with Homebrew on macOS:

```bash
brew install surrealdb/tap/surreal
```

Or with the official script on macOS or Linux:

```bash
curl -sSf https://install.surrealdb.com | sh
```

Check that `surreal` is on your `PATH` and reports a supported version:

```bash
surreal db version   # the install script's `surreal`; Homebrew's: surreal version
```

The daemon starts and manages this executable for its own storage. You do not need to start a separate database
server. Other installation methods are in [Install SurrealDB](docs/deployment.md#install-surrealdb).

### 2. Get the repository

```bash
git clone https://github.com/RuisongZhou/agent-memory-service.git
cd agent-memory-service
```

### 3. Connect your client

Choose a stable project ID and replace `my-project` below. Clients sharing project memories must connect to the
**same daemon** and use the **same project ID**.

#### Codex CLI / App

```bash
bash scripts/install.sh --host codex --apply --project my-project --recall
```

Restart Codex, then trust the `shared-memory` hooks (five with `--recall`) in `/hooks`, or **Settings → Hooks** in the
app. Capture and recall will not run until their hooks are trusted.

Codex records trust by hook content. If you rerun the installer with a different `--project` or binary path, or an
upgrade changes the hooks, trust them again. See [Codex hooks](docs/mcp-harness.md#codex-hooks).

#### Claude Code

```bash
bash scripts/install.sh --host claude-code --apply --project my-project --workspace /absolute/path/to/project --recall
```

Restart Claude Code in that workspace and approve the `shared-memory` project MCP server. Use `/mcp` to inspect it.

#### WorkBuddy

```bash
bash scripts/install.sh --host workbuddy --apply --project my-project --recall
```

Restart WorkBuddy. Under **Connectors → Custom connector**, trust `shared-memory` and enable all eight tools.

#### DeepSeek Harness

```bash
bash scripts/install.sh --host deepseek-harness --apply
```

Restart DeepSeek Harness and start a new session. This integration provides MCP tools only; it does not accept
`--project` or `--recall`. Specify the project when asking the agent to use memory.

#### What the installer does

On the first installation, the installer prompts for the API key with hidden input. It builds the binary, checks the
integration, merges client configuration and starts the daemon as a user service. Subsequent client installations
reuse the daemon and key.

The installer preserves existing unrelated MCP servers, hooks and settings. Remove `--apply` to preview the
installation.

> **Project routing:** capture and recall hooks use the project ID recorded in their installed command. Codex and
> WorkBuddy hooks are installed at user level, so they use that ID across sessions; they do not select a project from
> the current directory. Claude Code's default installation is scoped to the specified workspace. MCP calls specify
> their own project scope.

**Other configurations:**

- **MCP only:** for Codex, Claude Code or WorkBuddy, add `--no-capture` and omit `--recall`. The installer does not
  require `--project` in this mode. To add hooks later, rerun with the full command.
- **Capture without automatic recall:** omit `--recall`.
- **Existing binary:** use `--binary /absolute/path/to/agent-memoryd` to skip the build, for example with a
  [release archive](docs/deployment.md#release-archive).
- **Custom deployment:** to choose the data directory, port or self-hosted models, follow
  [deployment and operations](docs/deployment.md).
- **Install through an agent:** an agent that can run commands can follow
  [the installation guide](docs/mcp-harness.md#one-command-install). Agent shells usually cannot take hidden input, so
  provide the key yourself with `--key-file` or in a local terminal, never in chat or command arguments. Client
  confirmation in the steps above still needs you.

Full options, configuration changes, backups and uninstall instructions are in the
[installation guide](docs/mcp-harness.md#one-command-install).

### 4. Try sharing a memory

Connect two supported clients to the same daemon. In the first client, ask:

> Use shared-memory to save this test fact for project `my-project`: "The project's test mascot is a copper fox." Use
> `add_memories` with `infer:false` to store the text directly. If the write is asynchronous, poll `get_task` until it
> completes.

In the second client, ask:

> Use shared-memory to search project `my-project` for its test mascot. Show the stored fact and inspect its source with
> `get_memory`.

The second client should retrieve the fact saved by the first. This checks explicit storage, retrieval and
cross-client visibility. It may make model API calls for embeddings or reranking.

To check **automatic capture** separately, complete a conversation turn in a client with trusted capture hooks, then
run:

```bash
~/.local/bin/agent-memoryd outbox status
```

A `completed` entry means upload and memory processing have finished. It does not guarantee that a new memory was
created: extraction can produce no facts, and duplicate content can reuse an existing record.

For **automatic recall**, start a new session with recall enabled and ask about a stored project fact without
requesting an MCP call. Inspect the client's hook output or injected context where available.

## Everyday use

### MCP tools

After installation and a client restart, the agent sees eight `shared-memory` tools:

| Tool | Purpose |
|---|---|
| `add_memories` | Save conversations, facts or explicit instructions (`memoryType: "instruction"`) |
| `search_memories` | Find memories relevant to a question |
| `get_context` | Assemble project and session context, including saved instructions |
| `get_task` | Check whether an asynchronous write has completed (a `requestId` is not completion) |
| `list_turns` | List submitted raw conversation turns |
| `get_memory` | Read one memory with its history and sources |
| `correct_memory` | Add a correction or retract a memory |
| `feedback` | Record whether retrieved results were used |

Request fields are documented in the [API reference](docs/api.md#interfaces).

You can ask the agent to use memory directly:

| Task | Example request |
|---|---|
| Resume project work | "Load shared-memory context for `my-project`, then find our previous storage decision." |
| Save a requirement | "Remember as an instruction for `my-project`: include sources when reporting project decisions." |
| Check a source | "Inspect the source of that memory before relying on it." |
| Correct a record | "Use shared-memory to correct that record: the service port is now 9000." |
| Retract a record | "Retract this memory; it is no longer valid." |

Historical memories are reference material and may be outdated. Explicitly saved instructions are returned separately
and remain subject to current instructions.

For consistent tool use, add the [shared memory workflow](docs/mcp-harness.md#shared-memory-workflow) to your project's
`AGENTS.md` or `CLAUDE.md`. It covers loading context, checking sources, saving instructions and waiting for
asynchronous writes.

### Automatic capture and recall

With capture hooks enabled, ordinary completed turns are saved automatically. With recall enabled, the service loads
context at session start and searches for relevant memories before each prompt. Retrieval is bounded by time and
context budgets; a failed recall or a stopped daemon does not block the prompt. See
[automatically save conversations](docs/mcp-harness.md#automatically-save-conversations) and
[automatic recall](docs/mcp-harness.md#automatic-recall).

### Import existing memories

You can import supported memory files from Claude Code, Codex and WorkBuddy, as well as JSONL exports from other
agents. Past Claude Code and Codex sessions can also be imported. Importing is a separate, reviewed workflow:

```bash
~/.local/bin/agent-memoryd import plan --host codex --project my-project --workspace /absolute/path/to/project
~/.local/bin/agent-memoryd import propose <plan file printed by plan>   # prints RUN; writes no memories
~/.local/bin/agent-memoryd import show RUN                             # writes a review file; mark accept / reject
~/.local/bin/agent-memoryd import apply RUN --review <review file>
~/.local/bin/agent-memoryd import rollback RUN                         # undo the whole run if needed
```

Imported facts remain historical reference material; behavior requirements require explicit acceptance. See
[import existing memories](docs/mcp-harness.md#import-existing-memories) for supported sources and options.

## How it works

`agent-memoryd` is a Rust binary that provides the HTTP service, stdio MCP bridge, capture and recall hooks, and outbox
uploader.

- **Capture:** hooks write completed turns to a local outbox; the uploader submits them to the daemon.
- **Formation:** the service stores original input and uses the configured generation model to extract facts. Exact
  duplicate content can reuse existing memories.
- **Retrieval:** search combines semantic similarity, BM25 and entity scoring, with optional reranking.
- **Storage:** the daemon manages a local SurrealDB process using SurrealKV. No separate vector database is required.

The memory algorithm follows the Node/TypeScript OSS `mem0ai@3.3.1` built-in NLP fallback path. The service has its
own storage, scope rules, job processing and provenance; this is not a claim of full Mem0 SDK or Platform
compatibility.

See [architecture](docs/architecture.md) for implementation details and differences.

## Operations

```bash
~/.local/bin/agent-memoryd service status    # service file, command and whether the daemon responds
curl -fsS http://127.0.0.1:8765/health        # memory strategy, models and LLM usage
~/.local/bin/agent-memoryd outbox status      # whether captured turns were uploaded and processed
```

- **Logs:** `~/Library/Logs/agent-memory-service/daemon.stderr.log` on macOS;
  `journalctl --user -u agent-memory.service` on Linux.
- **Upgrade:** after `git pull`, rerun the original install command for each installed client; a changed binary is
  replaced and the service restarted. Keep SurrealDB on 3.x.
- **Change key or models:** edit `~/.config/agent-memory-service/.env` (see [configuration](docs/api.md#configuration)),
  then restart the service.
- **Uninstall:** add `--uninstall` to the install command to remove a client's configuration; to stop and remove the
  service, see [install the service](docs/deployment.md#install-the-service).
- **Troubleshooting:** see [deployment problems](docs/deployment.md#common-problems) and
  [client troubleshooting](docs/mcp-harness.md#troubleshooting).

Default local paths:

| Content | Location |
|---|---|
| Memory store | `~/.local/share/agent-memory-service/mem0` |
| Captured-turn outbox | `~/.local/share/agent-memory-service/harness-outbox.sqlite3` |
| Model configuration and key | `~/.config/agent-memory-service/.env` |

## Documentation

| Topic | Guide |
|---|---|
| Deployment, service management, backup and troubleshooting | [Deployment](docs/deployment.md) |
| Client installation, hooks and import | [MCP and client integration](docs/mcp-harness.md) |
| Local embedding and reranking models | [Self-hosted retrieval models](docs/self-hosted-models.md) |
| API fields, scopes, completion states and configuration | [API reference](docs/api.md) |
| Implementation and algorithm boundaries | [Architecture](docs/architecture.md) |
| Memory import design | [Memory import](docs/design/memory-import.md) |
| Evaluation methods and historical results | [Evaluation](eval/README.md) |
| Operations and check scripts | [Scripts](scripts/README.md) |

## Project status and contributing

Offline installation checks run in CI. Client CLI configuration parsing has been checked on macOS. Service-manager
commands have unit coverage, but CI does not start real user services.

The documented validation does **not yet include a complete live-client, real-model end-to-end acceptance run**. See
[what the installer verifies](docs/mcp-harness.md#merges-backups-and-rollback) for details. The installer's optional
`--verify-model` check uses real models against a temporary store and may incur API charges.

For development:

```bash
make fmt test-workspace build test-scripts
make check-public
```

These checks use temporary storage and mock models, without paid model calls. Storage and end-to-end installer tests
require a supported SurrealDB executable on `PATH`.

See [CONTRIBUTING.md](CONTRIBUTING.md) for setup and contribution requirements.

## License

[MIT](LICENSE). Third-party code and prompts retain their notices; see [THIRD_PARTY.md](THIRD_PARTY.md).

This is an independent project, not an official product of the supported client vendors.
