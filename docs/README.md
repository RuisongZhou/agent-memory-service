# Documentation

Documentation of the current service. The [README](../README.md) is the user flow: what the project does, how to install
it, connect a client and use it. The guides here are the reference for each part, and the
[changelog](../CHANGELOG.md) says what changed between versions and what to do when you upgrade.

## Find a guide

| I want to… | Read |
|---|---|
| Know what the service does and what leaves my machine | [README](../README.md), including [Before you install](../README.md#before-you-install) |
| Install the service and connect my first client | [README: Quick start](../README.md#quick-start) |
| Connect another platform, or stop the hooks in one project | [MCP and client integration](mcp-harness.md) |
| Know which project my memories go to | [Project routing](mcp-harness.md#project-routing) |
| Browse, correct or delete memories in a page | [README: Web UI](../README.md#web-ui) |
| Import what a client or another assistant remembers | [Import existing memories](mcp-harness.md#import-existing-memories) |
| Upgrade or uninstall | [README: Operations](../README.md#operations), then the [installer reference](mcp-harness.md#installer) |
| Run the daemon myself, back it up or restore it | [Deployment and operations](deployment.md) |
| Keep embedding and reranking on this machine | [Self-hosted retrieval models](self-hosted-models.md); the generation model is set in the [configuration](api.md#configuration) |
| Call the HTTP API, or change a setting | [API reference](api.md) |
| Understand how memories are formed, retrieved and stored | [Architecture](architecture.md) |
| Fix something that does not work | [Troubleshooting](#troubleshooting) below |
| Change the code or the docs | [CONTRIBUTING](../CONTRIBUTING.md) |
| Run or extend the evaluation | [Evaluation](../eval/README.md) |

## Troubleshooting

There are three pages, by where the problem is:

| The problem is | Page |
|---|---|
| The daemon does not start or answer; a model key or provider; SurrealDB; storage, backup or restore | [Deployment: common problems](deployment.md#common-problems) |
| A client is not connected or lists no tools; hooks capture or recall nothing; the outbox is stuck; memories land in the wrong project; an import fails | [Client troubleshooting](mcp-harness.md#troubleshooting) |
| Self-hosted models are not ready | [Self-hosted models: troubleshooting](self-hosted-models.md#troubleshooting) |

## Terms

| Term | Meaning |
|---|---|
| Daemon | `agent-memoryd serve`: the one process per machine that holds the store and answers HTTP on `127.0.0.1:8765`. |
| Service | The daemon installed as a user service (a LaunchAgent on macOS, a systemd user unit on Linux) by `install.sh service`. |
| Client, platform | An agent product connected to the daemon: Codex, Claude Code, WorkBuddy or DeepSeek Harness. A request also carries a *platform* label, one per client. The installer and `import` name a client with `--host`; *harness* survives only in the file name `mcp-harness.md` and in the name DeepSeek Harness. |
| MCP bridge | `agent-memoryd mcp`: a stdio child process the client starts, which forwards the eight `shared-memory` tools to the daemon. The daemon has no HTTP MCP endpoint. |
| Hook | A command a client runs on a conversation event. `agent-memoryd hook capture` saves turns and `agent-memoryd hook recall` injects memories. |
| Turn | One user prompt and the assistant's final reply, the unit that capture saves. |
| Outbox | The local SQLite queue of captured turns until the uploader, the daemon started with `--outbox`, has submitted them. |
| Capture, recall | Capture saves completed turns automatically. Recall injects relevant memories into the conversation at session start and before each prompt. |
| Memory formation | The daemon's processing of submitted text: it keeps the original input and has the generation model extract facts. |
| Fact, instruction | A fact is historical reference material and may be outdated. An instruction is an explicitly saved behavior requirement (`memoryType: "instruction"`), returned separately by `get_context` and subject to current instructions. |
| Project | The folder a session works in: inside a git repository, the repository's root. Its absolute path is the project ID. See [project routing](mcp-harness.md#project-routing). |
| Global memory | A memory with no project. Every project sees it. |
| Scope | The project (or `global`) and the session a request belongs to; the platform label travels in a header. See [scope and idempotency](api.md#scope-and-idempotency). |
| Request, task | The durable receipt of a write, identified by `requestId`. The ID alone is not completion: a task is done when its status is `completed`. See [tasks and completion](api.md#tasks-and-completion). |
| Import run | A proposed set of memories from a client's memory files or another assistant. You review it, it writes only what you accept, and it can be rolled back as one run. |
| Consolidation | Optional merging of duplicate or overlapping facts, off by default. |

## Languages

The README has a [Chinese version](../README.zh-CN.md). The web UI's text and the design notes in `design/` are Chinese;
everything else is English.

## Design notes

`design/` holds the Chinese notes behind features that are implemented: the research, the choices and their reasons.
Each opens with a status line. Code comments cite their sections, so their file names and section numbers stay stable.
The guides above, and the code, describe the behavior.

| Note | Subject |
|---|---|
| [memory-import.md](design/memory-import.md) | Importing the memories and past sessions a client already holds |
| [memory-import-mcp.md](design/memory-import-mcp.md) | Importing from another assistant inside a conversation |
| [project-routing.md](design/project-routing.md) | Deriving a session's project from its folder |
| [platform-connect.md](design/platform-connect.md) | Connecting platforms from the web UI |
| [web-ui.md](design/web-ui.md) | The web UI: features, back-end interfaces, security premise, hosting |
