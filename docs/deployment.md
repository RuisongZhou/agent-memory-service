# Deployment and operations

Install and operate the service from the repository root. For client registration and dialogue capture,
continue with [MCP and harness integration](mcp-harness.md) after the daemon is ready.

## Deployment components and strategy

| Component | How it runs | Default address or location |
|---|---|---|
| Memory daemon | One shared `agent-memoryd serve` process | `http://127.0.0.1:8765` |
| Embedding model | Hosted `qwen3.7-text-embedding-flash` through an API key (default), or your own `llama-server` | `https://maas.qianwenaiapi.com/compatible-mode/v1` |
| Reranker (optional) | Hosted `qwen3.7-text-rerank` (default), or your own `llama-server` | `https://maas.qianwenaiapi.com/compatible-api/v1` |
| MCP bridge | Each agent client launches `agent-memoryd mcp` as a stdio child process | Forwards tools to the daemon |
| Optional dialogue collector | `agent-memoryd hook capture` (Codex: `hook codex-notify`) run by a Codex, Claude Code or WorkBuddy hook | Writes a local SQLite outbox |
| Optional uploader | The daemon started with `--outbox`, or one `agent-memoryd outbox drain` per outbox | Submits dialogue and polls task receipts |

Configure the model key and start the daemon before connecting agent clients; [one command](mcp-harness.md#one-command-install) then registers a client. MCP registration makes tools available; it does not start the daemon, capture conversations, or automatically inject memory into the agent's context (the installer's `--recall` adds hooks that do).

The daemon exposes ordinary JSON HTTP endpoints, **not an HTTP MCP endpoint**. Configure clients with a stdio command, not an MCP `url` pointing to port 8765. There is no `/mcp`, SSE, or Streamable HTTP MCP endpoint in this repository.

The service runs Mem0 only. Its reranker is optional and disabled by default. See the
[configuration reference](api.md#configuration) for supported settings and compatibility.

## Install and deploy

The supplied shell scripts target macOS and Linux. On Windows, run this workflow in WSL. Commands in this section run from the repository root unless stated otherwise.

### 1. Install prerequisites

Install a current stable Rust toolchain with Cargo, Python **3.11 or later**, Git, Make, curl, and a native C/C++ compiler. The default retrieval models are hosted, so no model weights or GPU are needed. To run the models yourself instead, follow [self-hosted retrieval models](self-hosted-models.md) as well.

On macOS, install the Xcode command-line tools if needed, then install Python using Homebrew:

```sh
xcode-select --install
brew install python
```

On Debian/Ubuntu, the native build prerequisites can be installed with:

```sh
sudo apt-get update
sudo apt-get install build-essential pkg-config libssl-dev python3 python3-venv git curl
```

Install Rust using your preferred toolchain manager, and confirm the tools are on PATH:

```sh
rustc --version
cargo --version
python3 --version
```

### 2. Clone and build the daemon

```sh
git clone https://github.com/RuisongZhou/agent-memory-service.git
cd agent-memory-service
make build
target/fast/agent-memoryd serve --help
```

`make build` uses the locked Rust dependencies and the `fast` profile, producing `target/fast/agent-memoryd`. Limit compilation parallelism with `make build JOBS=2` on a machine with limited memory. The daemon's Rust build does not embed model weights or require a GPU backend; inference runs at the configured model provider.

The Python launcher and installer use only the standard library, so deployment does not require the evaluation packages or dataset. Dialogue capture, automatic recall and the uploader are `agent-memoryd` subcommands and need no Python. If you also want the offline regression suite, `make setup` creates `.venv`, installs [eval/requirements.txt](../eval/requirements.txt), and downloads the public research dataset and tokenizer caches. It makes no paid model requests.

### 3. Configure memory formation

For a new checkout, copy [.env.example](../.env.example), then edit `.env` locally:

```sh
cp -n .env.example .env
chmod 600 .env
```

Fill the provider connection in `.env` using the supplied template: `QWEN_API_KEY` for memory formation and,
in the next step, the retrieval-model key. The launcher reads it as data, and so does the daemon started
directly with `--env-file .env`; without that option it does not load `.env`. For accepted variables, precedence and algorithm tunables,
use the [configuration reference](api.md#configuration) and
[JSON template](../examples/service-config.json). Keep keys outside version control.

### 4. Configure the retrieval models

Embeddings and the optional reranker come from one of two providers, selected with
`MEMORY_MODEL_PROVIDER` (or `--model-provider`):

| Provider | Models | What you configure |
|---|---|---|
| `api` (default) | Hosted `qwen3.7-text-embedding-flash` and `qwen3.7-text-rerank` | An API key in `MEMORY_MODEL_API_KEY` |
| `self-hosted` | Your own `llama-server` processes, for example BGE-M3 and BGE-reranker-v2-m3 | The servers; see [self-hosted retrieval models](self-hosted-models.md) |

For the default, add the key to `.env`:

```sh
MEMORY_MODEL_API_KEY=your-model-api-key
```

When it is empty, the daemon uses `QWEN_API_KEY`, so one key is enough if it covers both generation and
the retrieval models. Embeddings go to `https://maas.qianwenaiapi.com/compatible-mode/v1/embeddings`
and reranking to `https://maas.qianwenaiapi.com/compatible-api/v1/reranks`; the compatible-mode base has
no rerank route. Override the bases with `MEMORY_EMBED_URL`/`MEMORY_RERANK_URL` and the models with
`MEMORY_EMBED_MODEL`/`MEMORY_RERANK_MODEL` for another OpenAI-compatible provider. Each startup makes one
small embedding request to check the key and read the vector dimension, and one rerank request when
reranking is enabled. Stored texts and queries are sent to the provider; use `self-hosted` if they must
stay on your machines.

Switching provider or embedding model on an existing data directory re-embeds its records at the next
start. The hosted default and BGE-M3 both use 1024 dimensions; for a model with another dimension, start
with a new data directory.

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

If agents capture dialogue, add `--outbox`: the daemon then also uploads the collector's outbox
(`~/.local/share/agent-memory-service/harness-outbox.sqlite3`, or the path given after `--outbox`)
and no separate uploader process is needed. The option is never on by default, so evaluation and
check daemons never take conversations from your outbox. The daemon log reports only lock and outbox
failures; a turn whose formation failed or was rejected shows in `agent-memoryd outbox status`.

The binary can also start without the Python launcher. `--env-file` reads the same `.env` keys as
data; a non-empty environment variable still wins. The launcher's `--max-calls 60` corresponds to
`MEMORY_LLM_MAX_CALLS=60`, and its concurrency default of 1 to `--llm-concurrency 1`:

```sh
MEMORY_LLM_MAX_CALLS=60 target/fast/agent-memoryd serve --env-file .env \
  --data-dir "$HOME/.local/share/agent-memory-service/mem0" --llm-concurrency 1 --outbox
```

The launcher accepts `--config`, `--bin`, `--model-provider`, endpoint/port options, concurrency and a
generation-attempt guard. Use [API configuration](api.md#configuration) for defaults and precedence. A release binary
can be selected with `--bin target/release/agent-memoryd` after a release build. For daemon-only flags
such as `--prompt-variant en` or `--no-reranker`, launch the binary directly with exported settings.

### 6. Check readiness and persistence

In another terminal:

```sh
curl -fsS http://127.0.0.1:8765/health
```

Daemon health should report `ok:true`, the selected `memoryStrategy`, `modelBackend:"api"` (or `"llama-server"` when self-hosted), the embedding model and URL, and LLM usage. `rerankModel:null` is expected for default Mem0. Health confirms startup and that the retrieval models answered; it does not validate the generation key or mean queued formation is complete.

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

## Keep services running

For an initial deployment, separate terminals are sufficient. For regular use, keep the daemon and optional uploader under a process manager, along with any [self-hosted model servers](self-hosted-models.md#4-keep-the-servers-running). Use absolute executable/model/data paths and the same OS user that owns the outbox. The repository does not install background services automatically.

Stop previously launched servers/daemon/uploader before enabling managed services to avoid port and lock conflicts. Repeat health and capture checks after switching to the process manager.

### macOS LaunchAgents

Use one LaunchAgent for the daemon and one per self-hosted model server, if any, so each process can restart independently. Create the definition and log directories:

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
    <string>--outbox</string>
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

Each command argument is a separate `<string>`; do not put a shell command or `&` into the array. The daemon launcher reads the checkout's `.env`. With `--outbox` the daemon uploads the logged-in user's default outbox, so no uploader agent is needed; omit `--outbox` if capture is disabled. For self-hosted model servers, create one more plist per server from the same template, with its own label (for example `local.agent-memory-embed`), distinct log paths and the foreground command from [self-hosted retrieval models](self-hosted-models.md#2-download-weights-and-start-the-servers).

Validate and load the selected plists in your logged-in GUI session:

```sh
plutil -lint "$HOME/Library/LaunchAgents/local.agent-memory-daemon.plist"
launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/local.agent-memory-daemon.plist"
launchctl print "gui/$(id -u)/local.agent-memory-daemon"
```

Validate each additional plist before loading it. LaunchAgents run during the user's login session. To stop the daemon, use `launchctl bootout "gui/$(id -u)/local.agent-memory-daemon"`; apply the same command to any model labels when shutting everything down. After changing a plist, boot it out and bootstrap the updated file. See Apple's [LaunchAgent guide](https://developer.apple.com/library/archive/documentation/MacOSX/Conceptual/BPSystemStartup/Chapters/CreatingLaunchdJobs.html) and the local `man launchctl` / `man launchd.plist` references.

### Linux user-systemd services

The following example supervises the daemon, which also uploads captured dialogue. Replace every `/absolute/path` first. For self-hosted model servers, add their units from [self-hosted retrieval models](self-hosted-models.md#4-keep-the-servers-running).

```sh
mkdir -p "$HOME/.config/systemd/user"
```

Create `~/.config/systemd/user/agent-memory.service`:

```ini
[Unit]
Description=Agent Memory daemon

[Service]
WorkingDirectory=/absolute/path/agent-memory-service
ExecStart=/absolute/path/to/python3 /absolute/path/agent-memory-service/scripts/serve.py --memory-strategy mem0 --data-dir %h/.local/share/agent-memory-service/mem0 --max-calls 60 --outbox
Restart=on-failure
RestartSec=5
UMask=0077

[Install]
WantedBy=default.target
```

The launcher reads the checkout's `.env`; no key belongs in the unit file. It probes the retrieval models before opening the HTTP listener and exits if they do not answer, so a restart retries once the network or model servers are up. `--outbox` uploads the user's default outbox; omit it when capture is disabled.

Load and start the units, then repeat the readiness and capture checks:

```sh
systemctl --user daemon-reload
systemctl --user enable --now agent-memory.service
systemctl --user status agent-memory.service
journalctl --user -u agent-memory.service -n 100
```
 To run user services after logout, enable lingering according to the host's administration policy, for example `loginctl enable-linger "$USER"`. These are deployment templates; validate them on the target host. Refer to upstream [systemd service documentation](https://www.freedesktop.org/software/systemd/man/latest/systemd.service.html) for host-specific behavior.

## Operations and troubleshooting

### Storage, backups, and upgrades

Within the selected data root, each tenant stores its embedded database at `tenants/<tenant>/db` and full-text index at `tenants/<tenant>/fulltext`. Primary inputs, memories, provenance, request receipts, and durable index work belong in the database; search indexes are rebuildable. The capture outbox is a separate database and also needs a retention/backup policy.

For a consistent backup, pause new submissions, stop the uploader and daemon, then copy the **entire data root** and the outbox database with any `-wal`/`-shm` companions while no collector is writing. A copy of an active database directory is not a documented snapshot method. Preserve the matching configuration, strategy, source revision, and model identities. `/admin/export` provides selected audit tables; it is not a complete restore backup.

Before upgrading, make that backup, update the source, rebuild the binary, restart with the same data path/strategy, and check health and a known record. Do not run two daemons against one data directory. Restore with services stopped and a matching version/configuration. For unfinished legacy jobs, use the preserved version or a fresh store; see [compatibility](api.md#compatibility).

To request an index rebuild on a running daemon:

```sh
target/fast/agent-memoryd reindex --all --url http://127.0.0.1:8765 --tenant default
```

Use `--vector` or `--fulltext` for a specific index. Changing the model provider, embedding model, or a self-hosted model's alias/quantization causes re-embedding at the next start; expect rebuild work and verify retrieval afterwards. Review local outbox retention periodically: `completed`, `failed`, and `rejected` rows remain until deliberately cleaned up.

### Stop and restart

With the terminal/script setup, stop the foreground daemon (and a standalone uploader, if you run one) with Ctrl-C, then any self-hosted model servers with `scripts/self_hosted_models.sh stop`. Restart self-hosted models first, then the daemon with the same arguments/data directory. Accepted outbox jobs resume polling; pending ones retain their payloads for retry. Durable unfinished server jobs resume when their tenant is opened.

With the LaunchAgent setup, use `launchctl bootout` for the selected service labels. With systemd, stop the daemon and any model units using `systemctl --user stop`. Use the process manager's logs instead of `results/llama/`, which belongs to the self-hosted helper script.

### Common problems

| Symptom | Check or action |
|---|---|
| `model API key not configured` | Set `MEMORY_MODEL_API_KEY` (or `QWEN_API_KEY`) in `.env` or the environment, or switch to `MEMORY_MODEL_PROVIDER=self-hosted` |
| `embedding model probe failed` with HTTP 401/403/404 | Check the model key, `MEMORY_EMBED_URL`/`MEMORY_RERANK_URL` and model names; the rerank base must offer `/reranks` |
| Self-hosted model not ready | See [self-hosted troubleshooting](self-hosted-models.md#troubleshooting) |
| Daemon fails before listening | Check `serve --help`; invalid config fields/tunables are rejected, and the retrieval models must answer |
| `LLM API key not configured` or provider 401 | Check `.env`, exported overrides, and provider access; direct binary launches read `.env` only with `--env-file` |
| Generation attempt guard reached | Inspect usage/errors and deliberately revise the launch guard/provider budget; restarting resets the process guard |
| Legacy jobs at startup | Complete them with the preserved version or use a fresh store; see [compatibility](api.md#compatibility) |

Inspect a daemon task through HTTP using the actual receipt ID:

```sh
curl -fsS http://127.0.0.1:8765/v1/get_task \
  -H 'Content-Type: application/json' \
  -H 'X-Memory-Tenant: default' -H 'X-Memory-Platform: codex' \
  -d '{"requestId":"REPLACE_WITH_RETURNED_REQUEST_ID"}'
```

The daemon enforces loopback binding. For another machine, keep this boundary and deploy an authenticated gateway or an authenticated tunnel to the local service. Tenant/platform labels do not authorize access. The supplied MCP bridge and uploader have no gateway credential options; direct gateway authentication needs an adapter change. A remote stdio bridge/tunnel must also preserve transport and scope correctly. This repository does not provide a turnkey multi-user remote deployment.


For MCP, capture and uploader failures, see [client troubleshooting](mcp-harness.md#troubleshooting).
For offline build verification, see [CONTRIBUTING](../CONTRIBUTING.md#checks).
