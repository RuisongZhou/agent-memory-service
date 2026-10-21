# Deployment and operations

Install and operate the service from the repository root. For client registration and dialogue capture,
continue with [MCP and harness integration](mcp-harness.md) after the daemon is ready.

## Deployment components and strategy

| Component | How it runs | Default address or location |
|---|---|---|
| Memory daemon | One shared `agent-memoryd serve` process | `http://127.0.0.1:8765` |
| Embedding model | `llama-server` with BGE-M3 Q8_0 GGUF | `http://127.0.0.1:8081` |
| Reranker | Separate `llama-server` with BGE-reranker-v2-m3 Q8_0 GGUF | `http://127.0.0.1:8082` |
| MCP bridge | Each agent client launches `agent-memoryd mcp` as a stdio child process | Forwards tools to the daemon |
| Optional dialogue collector | Codex notification, Claude Code or WorkBuddy hook | Writes a local SQLite outbox |
| Optional uploader | One `harness_memory.py drain` process per outbox | Submits dialogue and polls task receipts |

Start the model servers and daemon before connecting agent clients; [one command](mcp-harness.md#one-command-install) then registers a client. MCP registration makes tools available; it does not start the daemon, capture conversations, or automatically inject memory into the agent's context (the installer's `--recall` adds hooks that do).

The daemon exposes ordinary JSON HTTP endpoints, **not an HTTP MCP endpoint**. Configure clients with a stdio command, not an MCP `url` pointing to port 8765. There is no `/mcp`, SSE, or Streamable HTTP MCP endpoint in this repository.

The service runs Mem0 only. Its reranker is optional and disabled by default. See the
[configuration reference](api.md#configuration) for supported settings and compatibility.

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

The Python launcher and dialogue adapter use only the standard library, so deployment does not require the evaluation packages or dataset. If you also want the offline regression suite, `make setup` creates `.venv`, installs [eval/requirements.txt](../eval/requirements.txt), and downloads the public research dataset and tokenizer caches. It makes no paid model requests.

### 3. Configure memory formation

For a new checkout, copy [.env.example](../.env.example), then edit `.env` locally:

```sh
cp -n .env.example .env
chmod 600 .env
```

Fill the provider connection in `.env` using the supplied template. The launcher reads it as data;
the direct daemon does not load `.env`. For accepted variables, precedence and algorithm tunables,
use the [configuration reference](api.md#configuration) and
[JSON template](../examples/service-config.json). Keep keys outside version control.

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

The launcher accepts `--config`, `--bin`, endpoint/port options, concurrency and a generation-attempt
guard. Use [API configuration](api.md#configuration) for defaults and precedence. A release binary
can be selected with `--bin target/release/agent-memoryd` after a release build. For daemon-only flags
such as `--prompt-variant en` or `--no-reranker`, launch the binary directly with exported settings.

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
| `local.agent-memory-embed` | The foreground embedding `llama-server` command in [retrieval-model setup](#4-download-and-start-the-retrieval-models), with absolute executable/model paths and `-ngl all` for Metal |
| `local.agent-memory-rerank` | The foreground reranking command in [retrieval-model setup](#4-download-and-start-the-retrieval-models), with absolute executable/model paths and `-ngl all` for Metal |
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

Before upgrading, make that backup, update the source, rebuild the binary, restart with the same data path/strategy, and check health and a known record. Do not run two daemons against one data directory. Restore with services stopped and a matching version/configuration. For unfinished legacy jobs, use the preserved version or a fresh store; see [compatibility](api.md#compatibility).

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
