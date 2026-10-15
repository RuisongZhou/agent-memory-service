# Agent Memory Service

A local shared memory service for AI agents. Codex, Claude Code, and other agent platforms can use the same persistent store to recall project decisions, save facts and explicit instructions, inspect their sources, and correct earlier memories. Each platform keeps its own task state and native configuration.

The Rust daemon provides a JSON HTTP API and a stdio MCP bridge. It uses embedded SurrealDB/SurrealKV for persistence, local `llama-server` processes for embedding and optional reranking, and a separately configured generation model for memory formation. No standalone database server is required.

This is an experimental implementation. The instructions below describe the current code; benchmark documents describe their recorded protocols and historical runs, rather than deployment guarantees. See the [evaluation protocol](eval/docs/evaluation.md) and [implementation review](docs/review.md).

## Contents

- [Deployment components and strategy](#deployment-components-and-strategy)
- [Install and deploy](#install-and-deploy)
- [Use with Codex](#use-with-codex)
- [Use with Claude Code](#use-with-claude-code)
- [Shared memory workflow](#shared-memory-workflow)
- [Automatically save conversations](#automatically-save-conversations)
- [Keep services running](#keep-services-running)
- [Operations and troubleshooting](#operations-and-troubleshooting)
- [Development and evaluation](#development-and-evaluation)

## Deployment components and strategy

| Component | How it runs | Default address or location |
|---|---|---|
| Memory daemon | One shared `agent-memoryd serve` process | `http://127.0.0.1:8765` |
| Embedding model | `llama-server` with BGE-M3 Q8_0 GGUF | `http://127.0.0.1:8081` |
| Reranker | Separate `llama-server` with BGE-reranker-v2-m3 Q8_0 GGUF | `http://127.0.0.1:8082` |
| MCP bridge | Each agent client launches `agent-memoryd mcp` as a stdio child process | Forwards tools to the daemon |
| Optional dialogue collector | Codex notification or Claude Code hook | Writes a local SQLite outbox |
| Optional uploader | One `harness_memory.py drain` process per outbox | Submits dialogue and polls task receipts |

Start the model servers and daemon before connecting agent clients. MCP registration makes tools available; it does not start the daemon, capture conversations, or automatically inject memory into the agent's context.

The daemon exposes ordinary JSON HTTP endpoints, **not an HTTP MCP endpoint**. Configure clients with a stdio command, not an MCP `url` pointing to port 8765. There is no `/mcp`, SSE, or Streamable HTTP MCP endpoint in this repository.

The main branch supports the Mem0 strategy:

| Strategy | Formation and retrieval | Reranker |
|---|---|---|
| `mem0` (default) | Ordered additive formation based on Node SDK 3.3.1; entity projection and semantic/BM25/entity scoring using its built-in NLP fallback; complete input batches remain readable | Disabled by default; enable with `mem0Rerank:true` in JSON configuration |

`--memory-strategy mem0` remains accepted for existing launchers. Other strategy values and retired
buffer/maintenance settings are rejected before models or stores are opened. The previous combined
Mem0 + Memos implementation is preserved on [`codex/mem0-memos`](https://github.com/RuisongZhou/agent-memory-service/tree/codex/mem0-memos) at the split point
`971ac13`. Use that branch and a separate data directory to run the historical strategy.
Main rejects stores containing unfinished legacy formation jobs; finish them with the preserved
version first. Switching branches does not migrate a data directory. See [API and configuration](docs/api.md#memory-strategy).

## Install and deploy

The supplied shell scripts and dialogue uploader target macOS and Linux. On Windows, run this workflow in WSL; the uploader uses Unix `fcntl` locking. Commands in this section run from the repository root unless stated otherwise.

### 1. Install prerequisites

Install a current stable Rust toolchain with Cargo, Python **3.11 or later**, Git, Make, curl, and a native C/C++ compiler. Building llama.cpp from source also requires CMake. Allow disk space and RAM for both downloaded GGUF files and the Rust build; model weights are downloaded separately and never shipped in the repository.

On macOS, install the Xcode command-line tools if needed, then install llama.cpp and Python using Homebrew:

```sh
xcode-select --install
brew install llama.cpp python
```

On Debian/Ubuntu, the native build prerequisites can be installed with:

```sh
sudo apt-get update
sudo apt-get install build-essential cmake pkg-config libssl-dev python3 python3-venv git curl
```

Install Rust using your preferred toolchain manager, and confirm the tools are on PATH:

```sh
rustc --version
cargo --version
python3 --version
```

For Linux, build llama.cpp outside this repository or install a suitable binary. For example, in a separate source directory:

```sh
git clone https://github.com/ggml-org/llama.cpp.git
cd llama.cpp
cmake -B build -DCMAKE_BUILD_TYPE=Release
cmake --build build --config Release --target llama-server -j 2
export PATH="$PWD/build/bin:$PATH"
llama-server --help
```

This is the CPU build. For CUDA, Vulkan, or another accelerator, follow the upstream [llama.cpp build instructions](https://github.com/ggml-org/llama.cpp/blob/master/docs/build.md). Return to the memory-service checkout for the following steps.

### 2. Clone and build the daemon

```sh
git clone https://github.com/RuisongZhou/agent-memory-service.git
cd agent-memory-service
make build
target/fast/agent-memoryd serve --help
```

`make build` uses the locked Rust dependencies and the `fast` profile, producing `target/fast/agent-memoryd`. Limit compilation parallelism with `make build JOBS=2` on a machine with limited memory. The daemon's Rust build does not embed model weights or require a GPU backend; inference runs in llama.cpp.

The Python launcher and dialogue adapter use only the standard library, so deployment does not require the evaluation packages or dataset. If you also want the offline regression suite, `make setup` creates `.venv`, installs [eval/requirements.txt](eval/requirements.txt), and downloads the public research dataset and tokenizer caches. It makes no paid model requests.

### 3. Configure memory formation

For a new checkout, copy [.env.example](.env.example), then edit `.env` locally:

```sh
cp -n .env.example .env
chmod 600 .env
```

The shipped defaults are:

```dotenv
QWEN_API_KEY=
QWEN_BASE_URL=https://maas.qianwenaiapi.com/compatible-mode/v1
LLM_MODEL=qwen3.8-flash
MEMORY_STRATEGY=mem0
```

Set `QWEN_API_KEY` to your generation-provider key. This service key is separate from your Codex or Claude subscription/login. The daemon sends OpenAI-compatible chat-completions requests with JSON output and `enable_thinking:false`; a different provider/model must support that request contract. Generation sends the formation input to the configured provider, so choose conversation capture scope accordingly.

`scripts/serve.py` reads only `QWEN_API_KEY`, `QWEN_BASE_URL`, `LLM_MODEL`, and `MEMORY_STRATEGY` from the checkout's `.env`. Existing nonempty environment values take precedence. It parses the file as data and does not execute shell content. Export other supported environment variables in the launch environment; putting them in `.env` alone has no effect. Direct `agent-memoryd serve` does not load `.env`.

For algorithm tunables, start from [examples/service-config.json](examples/service-config.json) and pass a JSON file with `--config`. The file uses camelCase keys and rejects unknown fields. Strategy precedence is explicit `--memory-strategy`, then `MEMORY_STRATEGY`, then JSON `memoryStrategy`, then `mem0`. Other paths, model URLs, prompt and LLM connection settings are set by the daemon's CLI/environment; see the [configuration reference](docs/api.md#startup).

### 4. Download and start the retrieval models

```sh
scripts/llama_models.sh
scripts/llama_servers.sh start
scripts/llama_servers.sh status
```

The download script places `bge-m3-Q8_0.gguf` and `bge-reranker-v2-m3-Q8_0.gguf` in ignored `models/gguf/`. Downloads are resumable. `GGUF_DIR` changes the download and server model directory; `HF_ENDPOINT` changes the download host. Keep weights and caches out of Git, releases, and CI artifacts.

The server script runs both models in the background, requests GPU offload with `-ngl all`, and is used for Metal on Apple Silicon. PID files and logs go to `results/llama/`. It waits for each model's `/health` endpoint. For a CPU deployment, or to supervise each process separately, run these commands in separate terminals instead:

```sh
# Embedding server
llama-server -m models/gguf/bge-m3-Q8_0.gguf \
  --host 127.0.0.1 --port 8081 --embedding --pooling cls \
  -c 8192 -np 4 -ub 2048 -b 2048 -ngl 0 --alias bge-m3 --no-webui
```

```sh
# Reranking server
llama-server -m models/gguf/bge-reranker-v2-m3-Q8_0.gguf \
  --host 127.0.0.1 --port 8082 --reranking \
  -c 8192 -np 4 -ub 2048 -b 2048 -ngl 0 --alias bge-reranker-v2-m3 --no-webui
```

Change `-ngl 0` to the offload setting appropriate for your llama.cpp build. Default Mem0 only needs the embedding server; start the reranker only when explicitly enabling Mem0 reranking.

The script also accepts `LLAMA_EMBED_MODEL`, `LLAMA_RERANK_MODEL`, `LLAMA_EMBED_PORT`, `LLAMA_RERANK_PORT`, `LLAMA_CTX`, `LLAMA_PARALLEL`, and `LLAMA_RUN_DIR`. If you change ports, supply matching `--embed-url`/`--rerank-url` to the Python launcher. Keep the model endpoints on loopback.

### 5. Start the shared daemon

Use a persistent directory outside the source checkout:

```sh
mkdir -p "$HOME/.local/share/agent-memory-service/mem0"
chmod 700 "$HOME/.local/share/agent-memory-service/mem0"
python3 scripts/serve.py \
  --memory-strategy mem0 \
  --data-dir "$HOME/.local/share/agent-memory-service/mem0" \
  --max-calls 60
```

This runs in the foreground. Keep the terminal open or use the [process-manager setup](#keep-services-running). An explicit data path avoids the launcher's relative `daemon-data` default and the direct binary's different `./memory-data` default.

The launcher defaults to one concurrent generation request. Use `--llm-concurrency` to change it. `--max-calls 60` limits process-local generation attempts, including retries; it is a soft guard that resets on restart, not a persistent spending limit. Long-running installations must choose an appropriate guard and provider-side budget. `--max-calls 0` removes the launcher's guard. Dialogue formation and explicit fact extraction can use generation; search uses local retrieval models and makes no generative call.

Additional launcher options include `--port`, `--embed-url`, `--rerank-url`, `--config`, and `--bin`. A release build can be selected with `--bin target/release/agent-memoryd` after `cargo build --locked -p memory-service --bin agent-memoryd --release`. To use daemon-only flags such as `--prompt-variant en` or `--no-reranker`, run the binary directly with the required environment variables exported; consult `serve --help`.

For example, add `--config examples/service-config.json` to the launcher command to use the shipped tunable template. Keep local configuration and credentials separate. The direct daemon defaults to 24 concurrent generation requests; the Python launcher explicitly sets its safer default of one.

### 6. Check readiness and persistence

In another terminal:

```sh
curl -fsS http://127.0.0.1:8081/health
curl -fsS http://127.0.0.1:8082/health
curl -fsS http://127.0.0.1:8765/health
```

Skip the port-8082 check if you intentionally run default Mem0 without a reranker. Daemon health should report `ok:true`, the selected `memoryStrategy`, `modelBackend:"llama-server"`, embedding model metadata, and LLM usage. `rerankModel:null` is expected for default Mem0. Health confirms startup and model connectivity; it does not validate the generation key or mean queued formation is complete.

For a write/read check that makes **no generation request**, save an explicit instruction through HTTP:

```sh
curl -fsS http://127.0.0.1:8765/v1/add_memories \
  -H 'Content-Type: application/json' \
  -H 'X-Memory-Tenant: default' -H 'X-Memory-Platform: demo' \
  -d '{"scope":{"project":"deployment-demo","session":"setup-check"},"turnId":"deployment-demo/setup-check/instruction-1","memoryType":"instruction","text":"Include sources when reporting project decisions."}'

curl -fsS http://127.0.0.1:8765/v1/get_context \
  -H 'Content-Type: application/json' \
  -H 'X-Memory-Tenant: default' -H 'X-Memory-Platform: demo' \
  -d '{"scope":{"project":"deployment-demo","session":"setup-check"},"tokenBudget":1500}'
```

The instruction should appear in `instructions` and the rendered context. Restart with the same strategy and data directory and repeat `get_context` to check persistence. This deliberately creates a demo record; `delete_memory` is available through HTTP to remove it using the returned memory ID.

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

Use either registration method for this server and preserve existing settings. Local Codex clients share MCP configuration on the same host; project configuration requires a trusted project. Restart/reload the client after changing its configuration, inspect `/mcp` in the CLI, and confirm the eight tools listed below are available. See the [official Codex MCP documentation](https://learn.chatgpt.com/docs/extend/mcp?surface=cli).

Add the [shared memory workflow](#shared-memory-workflow) to the target project's `AGENTS.md`. For automatic capture, follow the separate [Codex notification setup](#codex-notifications-and-session-end). A local stdio setup requires the executable and daemon to be reachable from the host running the client; a cloud container has its own loopback address.

## Use with Claude Code

From the **target project** directory, register the bridge:

```sh
claude mcp add --transport stdio --scope project shared-memory -- \
  /absolute/path/agent-memory-service/target/fast/agent-memoryd \
  mcp --url http://127.0.0.1:8765 --platform claude-code
claude mcp list
```

Alternatively, merge [examples/claude-code.mcp.json](examples/claude-code.mcp.json) into the target project's `.mcp.json`:

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

| Field | Example | Meaning |
|---|---|---|
| Tenant | `default` | Storage namespace; MCP selects it with `MEMORY_TENANT` |
| Project | `my-project` | Shared logical project ID, independent of machine paths |
| Platform | `codex` or `claude-code` | Caller identity set by the bridge's `--platform` |
| Session | Native thread/session ID | Groups the ordered input history for formation |
| Turn ID | `my-project/session-id/native-turn-id` | Stable retry key; the adapter URL-encodes each component |

Tenant and platform headers are routing metadata, **not authentication**. Global memories and matching project memories are shared across platforms; platform-scoped memories are normally visible only to their originating platform. Mem0 raw-input reads default to the calling platform; use `scope.platform:"*"` when intentionally reading inputs from all platforms.

`get_context` reads context without closing sessions or triggering formation. Supply the actual current session ID when starting or resuming work.

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

The MCP server exposes eight tools:

| Tool | Purpose |
|---|---|
| `add_memories` | Submit completed dialogue, an explicit fact, or an explicit instruction |
| `search_memories` | Retrieve relevant memories within a scope and token budget |
| `get_context` | Build project/session context, including explicit instructions |
| `get_task` | Poll a durable write request |
| `list_turns` | Read complete submitted message batches |
| `get_memory` | Inspect a record, its history, and evidence |
| `correct_memory` | Append a correction or retraction |
| `feedback` | Record which retrieved memories were used and the outcome |

For example, ask either agent: “Use shared-memory to load context for project `my-project`, then search for our previous storage decision and show its source.” To save a behavior requirement, ask: “Remember as an instruction for `my-project`: include sources when reporting project decisions.” These prompts use tools; they do not require the agent to copy its complete conversation.

To verify sharing, save that project instruction in Codex, then ask Claude Code to call `get_context` for the same project. It should see the instruction despite using a different platform/session. Repeat after a daemon restart with the same data directory. For inferred facts, first check the write receipt and then use `search_memories`; instructions are read through `get_context` rather than ordinary fact search.

HTTP exposes the matching tools under `POST /v1/<name>`, plus `delete_memory` and additional management interfaces. Requests use camelCase fields. MCP success content is JSON text; HTTP failures become `isError:true` tool results. See [API reference](docs/api.md) and [MCP/harness integration](docs/mcp-harness.md) for full contracts.

## Automatically save conversations

Automatic saving requires **both a collector and a running uploader**. The supplied adapter captures completed user/assistant text into a local durable queue; another process submits it without involving the agent's main model. It does not capture tool traces, reasoning, images, or every possible client/subagent lifecycle.

Use the same `--project`, `--platform`, and `--tenant` in capture commands as in the corresponding MCP setup. `--tenant default` is the adapter default; `MEMORY_TENANT` configures the MCP bridge and is not read by the collector. If you change `--outbox`, every collector, uploader, and status command must use that same path. One outbox can contain both Codex and Claude Code records for one daemon.

### Codex notifications and session end

Merge the optional `notify` setting from [examples/codex.toml](examples/codex.toml) into **user-level `~/.codex/config.toml`**, before any TOML tables:

```toml
notify = ["python3", "/absolute/path/agent-memory-service/scripts/harness_memory.py", "--project", "my-project", "--platform", "codex", "codex-notify"]
```

Current Codex ignores `notify` in a project-local `.codex/config.toml`; project-scoped MCP configuration is still supported. Preserve an existing notification program by having it forward the event to the collector. This notification command fixes the project ID, so enable it for sessions belonging to that project, or use a dispatcher that explicitly maps projects; otherwise unrelated sessions will be filed under the same project. See [Codex configuration reference](https://learn.chatgpt.com/docs/config-file/config-reference).

Codex sends an `agent-turn-complete` event as a single JSON argument containing thread/turn IDs, user inputs, and the last assistant message. The collector joins multiple user inputs into one user text and records the observed completion time because the event supplies no source message timestamps. See [official notification documentation](https://learn.chatgpt.com/docs/config-file/config-advanced#notifications).

For session closure, merge [examples/codex.hooks.json](examples/codex.hooks.json) into the target project's `.codex/hooks.json`, replacing the script path and project ID. This queues a `SessionEnd` marker. Review and trust the installed definition using `/hooks`; untrusted or changed hooks are skipped. See [official Codex hooks documentation](https://learn.chatgpt.com/docs/hooks). This hook needs an already captured turn; it does not replace `notify`.

### Claude Code hooks

Merge the `hooks` object from [examples/claude-code.settings.json](examples/claude-code.settings.json) into the target project's `.claude/settings.local.json`. Replace every script path and project ID; preserve existing hooks and permissions. The template sets a three-second timeout and runs the collector synchronously only for local SQLite writes.

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
| `completed` | Receipt satisfies the selected algorithm's completion contract |
| `rejected` | Permanent HTTP request error; input retained for investigation |
| `failed` | Formation failed; input and request ID retained |

Transient connection errors, lost responses, HTTP 408/429, and 5xx retry the same payload and turn ID. The service deduplicates by `platform + turnId` within a tenant; retries preserve the first input and timestamps. After acceptance, the uploader polls instead of resubmitting. Failed/rejected records are not automatically resubmitted, and terminal records are not automatically deleted.

For Mem0, completion requires `status:"completed"`, `formed:true`, and `searchable:true`; `queued`, `running`, or `indexing` is unfinished. See [lifecycle details](docs/api.md#memory-strategy).

`sessionEnded:true` is a session-close signal, not an end-of-every-turn flag. It closes an ordered input job; no experience pipeline runs. The collector also does not inject startup context. Use `get_context` through the agent, or implement a separate bounded SessionStart adapter.

### Other agent platforms

Any client supporting local stdio MCP can use [examples/mcp.json](examples/mcp.json), with an absolute binary path and its own `--platform` label. A host wrapper can enqueue completed dialogue without relying on either client's private transcript format:

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

Supply actual IDs and available source timestamps for real input. Mem0 accepts 1–128 ordered user/assistant messages per batch. Normal capture uses inference and can invoke the configured generation provider. Use `infer:false` for verbatim storage without generation. See the [adapter guide](docs/mcp-harness.md) for retry and scope rules.

## Keep services running

For an initial deployment, separate terminals are sufficient. For regular use, keep the model processes, daemon, and optional uploader under a process manager. Use absolute executable/model/data paths and the same OS user that owns the outbox. The repository does not install background services automatically.

Stop previously launched servers/daemon/uploader before enabling managed services to avoid port and lock conflicts. Repeat health and capture checks after switching to the process manager.

### macOS LaunchAgents

Use one LaunchAgent per model, daemon, and optional uploader so each process can restart independently. Create the definition and log directories:

```sh
mkdir -p "$HOME/Library/LaunchAgents" "$HOME/Library/Logs/agent-memory-service"
```

Save this daemon template as `~/Library/LaunchAgents/local.agent-memory-daemon.plist`. Replace `/absolute/path` and `/Users/YOUR_USER` with real absolute paths; launchd does not expand `$HOME` or `~` inside plist strings.

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>local.agent-memory-daemon</string>
  <key>ProgramArguments</key>
  <array>
    <string>/absolute/path/to/python3</string>
    <string>/absolute/path/agent-memory-service/scripts/serve.py</string>
    <string>--memory-strategy</string><string>mem0</string>
    <string>--data-dir</string><string>/Users/YOUR_USER/.local/share/agent-memory-service/mem0</string>
    <string>--max-calls</string><string>60</string>
  </array>
  <key>WorkingDirectory</key><string>/absolute/path/agent-memory-service</string>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ThrottleInterval</key><integer>10</integer>
  <key>StandardOutPath</key><string>/Users/YOUR_USER/Library/Logs/agent-memory-service/daemon.stdout.log</string>
  <key>StandardErrorPath</key><string>/Users/YOUR_USER/Library/Logs/agent-memory-service/daemon.stderr.log</string>
</dict>
</plist>
```

Create the other plists from the same template, using the following labels as filenames plus `.plist`. Change the `ProgramArguments` array and give each process distinct stdout/stderr log paths:

| Label | Program arguments |
|---|---|
| `local.agent-memory-embed` | The foreground embedding `llama-server` command in step 4, with absolute executable/model paths and `-ngl all` for Metal |
| `local.agent-memory-rerank` | The foreground reranking command in step 4, with absolute executable/model paths and `-ngl all` for Metal |
| `local.agent-memory-uploader` | Absolute Python executable, absolute `scripts/harness_memory.py` path, `drain`, `--url`, `http://127.0.0.1:8765` |

Each command argument is a separate `<string>`; do not put a shell command or `&` into the array. The daemon launcher reads the checkout's `.env`. The uploader uses the logged-in user's default outbox. Omit the reranker for default Mem0 if desired, and omit the uploader if capture is disabled.

Validate and load the selected plists in your logged-in GUI session:

```sh
plutil -lint "$HOME/Library/LaunchAgents/local.agent-memory-daemon.plist"
launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/local.agent-memory-embed.plist"
launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/local.agent-memory-rerank.plist"
launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/local.agent-memory-daemon.plist"
launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/local.agent-memory-uploader.plist"
launchctl print "gui/$(id -u)/local.agent-memory-daemon"
```

Validate each additional plist before loading it. The daemon waits for its required models, and the uploader retries while the daemon is unavailable. LaunchAgents run during the user's login session. To stop the daemon, use `launchctl bootout "gui/$(id -u)/local.agent-memory-daemon"`; apply the same command to the uploader and model labels when shutting everything down. After changing a plist, boot it out and bootstrap the updated file. See Apple's [LaunchAgent guide](https://developer.apple.com/library/archive/documentation/MacOSX/Conceptual/BPSystemStartup/Chapters/CreatingLaunchdJobs.html) and the local `man launchctl` / `man launchd.plist` references.

### Linux user-systemd services

The following example supervises all four processes. Replace every `/absolute/path` first, and choose the CPU/GPU flags for your model build.

```sh
mkdir -p "$HOME/.config/systemd/user"
```

Create `~/.config/systemd/user/agent-memory-embed.service`:

```ini
[Unit]
Description=Agent Memory embedding model

[Service]
ExecStart=/absolute/path/to/llama-server -m /absolute/path/agent-memory-service/models/gguf/bge-m3-Q8_0.gguf --host 127.0.0.1 --port 8081 --embedding --pooling cls -c 8192 -np 4 -ub 2048 -b 2048 -ngl 0 --alias bge-m3 --no-webui
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
```

Create `~/.config/systemd/user/agent-memory-rerank.service`:

```ini
[Unit]
Description=Agent Memory reranking model

[Service]
ExecStart=/absolute/path/to/llama-server -m /absolute/path/agent-memory-service/models/gguf/bge-reranker-v2-m3-Q8_0.gguf --host 127.0.0.1 --port 8082 --reranking -c 8192 -np 4 -ub 2048 -b 2048 -ngl 0 --alias bge-reranker-v2-m3 --no-webui
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
```

Create `~/.config/systemd/user/agent-memory.service`:

```ini
[Unit]
Description=Agent Memory daemon
Wants=agent-memory-embed.service agent-memory-rerank.service
After=agent-memory-embed.service agent-memory-rerank.service

[Service]
WorkingDirectory=/absolute/path/agent-memory-service
ExecStart=/absolute/path/to/python3 /absolute/path/agent-memory-service/scripts/serve.py --memory-strategy mem0 --data-dir %h/.local/share/agent-memory-service/mem0 --max-calls 60
Restart=on-failure
RestartSec=5
UMask=0077

[Install]
WantedBy=default.target
```

The launcher reads the checkout's `.env`; no key belongs in the unit file. It connects and probes the required model servers before opening the HTTP listener, so process start order is followed by an application readiness check. Default Mem0 can omit the reranker unit and its dependency.

Create `~/.config/systemd/user/agent-memory-uploader.service` if using capture:

```ini
[Unit]
Description=Agent Memory dialogue uploader
Wants=agent-memory.service
After=agent-memory.service

[Service]
ExecStart=/absolute/path/to/python3 /absolute/path/agent-memory-service/scripts/harness_memory.py drain --url http://127.0.0.1:8765
Restart=on-failure
RestartSec=5
UMask=0077

[Install]
WantedBy=default.target
```

Load and start the units, then repeat the readiness and capture checks:

```sh
systemctl --user daemon-reload
systemctl --user enable --now agent-memory-embed.service agent-memory-rerank.service agent-memory.service
systemctl --user enable --now agent-memory-uploader.service
systemctl --user status agent-memory.service agent-memory-uploader.service
journalctl --user -u agent-memory.service -u agent-memory-uploader.service -n 100
```

Omit uploader commands when capture is disabled. To run user services after logout, enable lingering according to the host's administration policy, for example `loginctl enable-linger "$USER"`. These are deployment templates; validate them on the target host. Refer to upstream [systemd service documentation](https://www.freedesktop.org/software/systemd/man/latest/systemd.service.html) for host-specific behavior.

## Operations and troubleshooting

### Storage, backups, and upgrades

Within the selected data root, each tenant stores its embedded database at `tenants/<tenant>/db` and full-text index at `tenants/<tenant>/fulltext`. Primary inputs, memories, provenance, request receipts, and durable index work belong in the database; search indexes are rebuildable. The capture outbox is a separate database and also needs a retention/backup policy.

For a consistent backup, pause new submissions, stop the uploader and daemon, then copy the **entire data root** and the outbox database with any `-wal`/`-shm` companions while no collector is writing. A copy of an active database directory is not a documented snapshot method. Preserve the matching configuration, strategy, source revision, and model identities. `/admin/export` provides selected audit tables; it is not a complete restore backup.

Before upgrading, make that backup, update the source, rebuild the binary, restart with the same data path/strategy, and check health and a known record. Do not run two daemons against one data directory. Restore with services stopped and a matching version/configuration. Changing strategy requires a separate store, not just changing a flag over live data.

To request an index rebuild on a running daemon:

```sh
target/fast/agent-memoryd reindex --all --url http://127.0.0.1:8765 --tenant default
```

Use `--vector` or `--fulltext` for a specific index. Changing the embedding model alias/quantization identity causes re-embedding at the next start; expect rebuild work and verify retrieval afterwards. Review local outbox retention periodically: `completed`, `failed`, and `rejected` rows remain until deliberately cleaned up.

### Stop and restart

With the terminal/script setup, stop the uploader and foreground daemon with Ctrl-C, then stop model servers with `scripts/llama_servers.sh stop`. Restart models first, then the daemon with the same arguments/data directory, then the uploader. Accepted outbox jobs resume polling; pending ones retain their payloads for retry. Durable unfinished server jobs resume when their tenant is opened.

With the LaunchAgent setup, use `launchctl bootout` for the selected service labels. With systemd, stop the uploader, daemon, and model units using `systemctl --user stop`. Use the process manager's logs instead of `results/llama/`, which belongs to the shell-script setup.

### Common problems

| Symptom | Check or action |
|---|---|
| Missing GGUF / model health fails | Run `scripts/llama_models.sh`; verify model paths, ports, llama-server flags, and `results/llama/*.log` for the script setup |
| Daemon fails before listening | Check model readiness and `serve --help`; invalid config fields/tunables are rejected, and a required model endpoint must be reachable |
| `LLM API key not configured` or provider 401 | Check `.env`, exported overrides, and provider access; direct binary launches do not read `.env` |
| Generation attempt guard reached | Inspect usage/errors and deliberately revise the launch guard/provider budget; restarting resets the process guard |
| MCP is listed but calls fail | Check daemon `/health`, absolute binary path, port, and stdio transport; the bridge does not start the daemon |
| Codex captures no turns | Put `notify` in user-level config, verify the Python path, preserve existing notification dispatch, and inspect the selected outbox |
| Session-end hook is skipped | Review/trust Codex hooks with `/hooks`; restart the client after edits |
| Claude Stop pairing fails | Start a new turn with capture hooks installed; verify required event fields and the matching project/platform |
| Outbox stays `pending` | Ensure one uploader is running and can reach the daemon; inspect `status` errors |
| Outbox stays `accepted` | Query `get_task`; inspect provider failures and unfinished index publication |
| Uploader reports a lock error | Another uploader holds the same outbox lock; keep one worker per queue |
| Cross-agent recall is empty | Check the same daemon, tenant/project IDs, record scope, completion state, token budget, and explicit cross-platform raw-input scope |
| Wrong strategy jobs at startup | Use the matching strategy/data root or a new isolated directory |

Inspect a daemon task through HTTP using the actual receipt ID:

```sh
curl -fsS http://127.0.0.1:8765/v1/get_task \
  -H 'Content-Type: application/json' \
  -H 'X-Memory-Tenant: default' -H 'X-Memory-Platform: codex' \
  -d '{"requestId":"REPLACE_WITH_RETURNED_REQUEST_ID"}'
```

The daemon enforces loopback binding. For another machine, keep this boundary and deploy an authenticated gateway or an authenticated tunnel to the local service. Tenant/platform labels do not authorize access. The supplied MCP bridge and uploader have no gateway credential options; direct gateway authentication needs an adapter change. A remote stdio bridge/tunnel must also preserve transport and scope correctly. This repository does not provide a turnkey multi-user remote deployment.

### Offline verification

Use the existing mock/stub workflow to validate a build without a provider key or downloaded models:

```sh
make setup
make fmt test-rust test-service test build selftest smoke
.venv/bin/python scripts/mem0_http_smoke.py
make check-arch check-public
```

`make selftest` sets `MEMORY_MOCK_MODELS=1`. HTTP smoke tests use mock retrieval and a localhost generation stub, temporary stores, and test ports. They exercise persistence and HTTP/MCP behavior without paid calls. `scripts/serve.py --mock` only selects mock retrieval and removes the provider key; it does not implement a generation stub and is not a substitute for the smoke suite. Mock output is correctness evidence, not retrieval-quality or benchmark performance.

## Development and evaluation

```text
crates/
  memory-service/   Daemon, HTTP/MCP transports, algorithm selection and selftests
  memory-engine/    Shared tenants, workers, model clients and application use cases
  memory-algo-mem0/ Ordered additive formation and semantic retrieval
  memory-config/    Startup configuration and tunables
  memory-domain/    Records, API types, errors and integrity planning
  memory-store/     Embedded SurrealDB persistence, schema and repositories
  memory-fulltext/  Tantivy, Chinese tokenization and entity aliases
  memory-rules/     Pure date, identifier and revision-planning rules
scripts/           Service launch, retrieval models, client adapters and service checks
examples/          MCP, hooks and service configuration templates
eval/              SDK baselines, datasets, evaluation docs/licenses, experiments and tests
docs/              Service API, architecture, design and client integration
results/           Ignored service logs, service checks and service documentation artifacts
eval/results/      Ignored evaluation runs, stores, receipts, reports and raw evidence
```

Keep API, formation, retrieval, runtime, storage, and pure rules separate. Follow [CONTRIBUTING](CONTRIBUTING.md), [architecture](docs/architecture.md), and the existing [CI workflow](.github/workflows/ci.yml). Run the pure Rust crate tests, relevant service/Python tests, and the mock selftest for changes. CI uses offline correctness checks and does not run paid evaluation. See the [service script inventory](scripts/README.md) for the scripts retained outside `eval/`.

Benchmarking is a separate opt-in workflow; deployment requires no LoCoMo run. Read [eval/README.md](eval/README.md), [evaluation protocol](eval/docs/evaluation.md), and [historical pilot results](eval/docs/benchmarks/conv26.md) before running it. Preserve protocols and raw evidence locally, use clean arm-specific stores, and do not tune retrieval parameters using pilot scores. Do not run a paid model or the full dataset as automatic verification.

The initial service derives from [memory-mcp-1file](https://github.com/pomazanbohdan/memory-mcp-1file), whose MIT notice is retained. Copied Mem0 evaluation assets and the additive extraction prompt retain their Apache-2.0 notices. LoCoMo is downloaded locally under CC BY-NC 4.0 and is not redistributed. Model dependencies have their own licenses. See [LICENSE](LICENSE) and [THIRD_PARTY](THIRD_PARTY.md). Never commit keys, model weights (including synthetic fixtures), databases, raw traces, or local caches.
