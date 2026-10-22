# Deployment and operations

Install and operate the service from the repository root. Install [SurrealDB](#install-surrealdb)
first; the [one-command installer](mcp-harness.md#one-command-install) checks it and performs steps 2–5 of
[install and deploy](#install-and-deploy) for you: it builds the binary into `~/.local/bin`, writes the
model key into `~/.config/agent-memory-service/.env` and runs the daemon as a service, then connects an
agent platform:

```sh
bash scripts/install.sh --host codex --apply --project my-project
```

The rest of this guide describes those steps for a manual or customized deployment and for operating
the daemon. For client registration and dialogue capture by hand, continue with
[MCP and harness integration](mcp-harness.md) after the daemon is ready.

## Deployment components and strategy

| Component | How it runs | Default address or location |
|---|---|---|
| Memory daemon | One shared `agent-memoryd serve` process, in a terminal or as a LaunchAgent/systemd user service (`agent-memoryd service install`) | `http://127.0.0.1:8765` |
| SurrealDB | Your own `surreal` (SurrealDB 3.2 or later 3.x), which the daemon starts on its store and stops with it; this repository does not build or ship it | A random loopback port; data in `<data root>/store/db` |
| Embedding model | Hosted `qwen3.7-text-embedding-flash` through an API key (default), or your own `llama-server` | `https://maas.qianwenaiapi.com/compatible-mode/v1` |
| Reranker (optional) | Hosted `qwen3.7-text-rerank` (default), or your own `llama-server` | `https://maas.qianwenaiapi.com/compatible-api/v1` |
| MCP bridge | Each agent client launches `agent-memoryd mcp` as a stdio child process | Forwards tools to the daemon |
| Optional dialogue collector | `agent-memoryd hook capture` run by a Codex, Claude Code or WorkBuddy hook | Writes a local SQLite outbox |
| Optional uploader | The daemon started with `--outbox`, or one `agent-memoryd outbox drain` per outbox | Submits dialogue and polls task receipts |

Configure the model key and start the daemon before connecting agent clients; [one command](mcp-harness.md#one-command-install) does both and then registers a client. MCP registration alone, by hand or with the installer's `--no-service`, makes tools available; it does not start the daemon, capture conversations, or automatically inject memory into the agent's context (the installer's `--recall` adds hooks that do).

The daemon exposes ordinary JSON HTTP endpoints, **not an HTTP MCP endpoint**. Configure clients with a stdio command, not an MCP `url` pointing to port 8765. There is no `/mcp`, SSE, or Streamable HTTP MCP endpoint in this repository.

The service runs Mem0 only. Its reranker is optional and disabled by default. See the
[configuration reference](api.md#configuration) for supported settings and compatibility.

## Install and deploy

The daemon needs the `agent-memoryd` binary, SurrealDB and a model API key; no Python, model servers or weights.
The binaries and scripts target macOS and Linux. On Windows, run this workflow in WSL. Commands in this
section run from the repository root unless stated otherwise.

### 1. Install prerequisites

| Way to get the binary | Needs |
|---|---|
| [Release archive](#release-archive) | `curl`, `tar` and `shasum` |
| [Build from source](#build-from-source) | Git, Make, curl, a current stable Rust toolchain with Cargo and a native C compiler (SQLite is compiled in) |

Python **3.11 or later** is needed only for the [one-command installer](mcp-harness.md#one-command-install)
(`scripts/install.sh`), the optional [Python launcher](#python-launcher) and
[evaluation](../eval/README.md). The daemon, the MCP bridge and the capture and recall hooks do not use
it. The default retrieval models are hosted, so no model weights or GPU are needed. To run the models
yourself instead, follow [self-hosted retrieval models](self-hosted-models.md) as well.

For a source build on macOS, install the Xcode command-line tools if needed:

```sh
xcode-select --install
```

On Debian/Ubuntu, the native build prerequisites can be installed with:

```sh
sudo apt-get update
sudo apt-get install build-essential pkg-config libssl-dev git curl
```

Install Rust using your preferred toolchain manager, and confirm the tools are on PATH:

```sh
rustc --version
cargo --version
```

For the installer or evaluation, also install Python 3.11+ (`brew install python`, or the distribution's
`python3` and `python3-venv` packages) and check `python3 --version`.

#### Install SurrealDB

The store runs on SurrealDB, which you install yourself from SurrealDB's official distribution; this
repository does not build, bundle or install it. Install SurrealDB 3.2 or a later 3.x release, either with
Homebrew on macOS:

```sh
brew install surrealdb/tap/surreal
```

or with the official install script on macOS or Linux:

```sh
curl -sSf https://install.surrealdb.com | sh
```

The script installs to `/usr/local/bin`, or to `~/.surrealdb` when it cannot write there; add that directory to
PATH. Then check the version:

```sh
surreal version
```

You do not run or configure SurrealDB yourself. The daemon starts `surreal start` on its store's datastore
(`<data root>/store/db`, SurrealKV), bound to a random port on 127.0.0.1, signs in as root with a password it
generates for that datastore, and stops it when the daemon stops; a small shell watchdog stops it as well
when the daemon dies without stopping it. If SurrealDB exits, the daemon exits too, so that its service
manager restarts both. The daemon looks `surreal` up on PATH unless `--surreal-bin PATH` (or
`MEMORY_SURREAL_BIN`) names it; `service install` records the absolute path in the unit, because service
managers start the daemon with a minimal PATH. The daemon refuses, before starting it, a `surreal` older than
3.2 or from another major release, and so does the one-command installer before it writes anything. CI tests
3.2.4. SurrealDB is distributed under its own license (Business Source
License 1.1 with an additional use grant); see [third-party notes](../THIRD_PARTY.md).

### 2. Get the daemon binary

#### Release archive

Each published release on the
[releases page](https://github.com/RuisongZhou/agent-memory-service/releases) carries one archive per
platform, each with a `.sha256` checksum, plus a combined `SHA256SUMS`: `aarch64-apple-darwin` (Apple silicon), `x86_64-apple-darwin`,
`x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu` (glibc 2.35 or later). An archive holds
`agent-memoryd`, `.env.example`, `LICENSE` and `THIRD_PARTY.md`. Download, verify and install one:

```sh
version=v0.1.0               # a published release tag
target=aarch64-apple-darwin  # your platform from the list above
name="agent-memoryd-$version-$target"
base="https://github.com/RuisongZhou/agent-memory-service/releases/download/$version"
curl -fLO "$base/$name.tar.gz"
curl -fLO "$base/$name.tar.gz.sha256"
shasum -a 256 -c "$name.tar.gz.sha256"
tar -xzf "$name.tar.gz"
mkdir -p "$HOME/.local/bin"
install -m 755 "$name/agent-memoryd" "$HOME/.local/bin/agent-memoryd"
"$HOME/.local/bin/agent-memoryd" serve --help
```

The macOS binaries are not notarized. Downloads made with `curl` are not quarantined; for an archive saved
by a browser, run `xattr -d com.apple.quarantine "$name/agent-memoryd"` before installing the binary.
The [release workflow](../.github/workflows/release.yml) builds the archives with `--release` and runs
each binary's mock-model `selftest` first.

The daemon needs nothing else from the repository. To deploy this binary and register agent clients
with it, run the [installer](mcp-harness.md#one-command-install) from a checkout of the same version with
`--binary`; it then builds nothing, writes that path into every command it installs and runs the service
with it:

```sh
bash scripts/install.sh --host codex --apply --project my-project --binary "$HOME/.local/bin/agent-memoryd"
```

#### Build from source

```sh
git clone https://github.com/RuisongZhou/agent-memory-service.git
cd agent-memory-service
make build
target/fast/agent-memoryd serve --help
```

`make build` uses the locked Rust dependencies and the `fast` profile, producing `target/fast/agent-memoryd`. Limit compilation parallelism with `make build JOBS=2` on a machine with limited memory. The daemon's Rust build does not embed model weights or require a GPU backend; inference runs at the configured model provider. `cargo build --locked --release -p memory-service --bin agent-memoryd` produces the optimized `target/release/agent-memoryd` that releases ship.

Building needs no Python. Dialogue capture, automatic recall and the uploader are `agent-memoryd` subcommands. If you also want the offline regression suite, `make setup` creates `.venv`, installs [eval/requirements.txt](../eval/requirements.txt), and downloads the public research dataset and tokenizer caches. It makes no paid model requests.

### 3. Configure memory formation

The daemon reads provider settings from a dotenv file given with `--env-file`. In a checkout, copy
[.env.example](../.env.example), then edit `.env` locally:

```sh
cp -n .env.example .env
chmod 600 .env
```

With a release archive, keep the file outside the extracted directory, for example:

```sh
mkdir -p "$HOME/.config/agent-memory-service"
cp -n "$name/.env.example" "$HOME/.config/agent-memory-service/.env"
chmod 600 "$HOME/.config/agent-memory-service/.env"
```

Fill the provider connection in that file using the supplied template: `QWEN_API_KEY` for memory
formation and, in the next step, the retrieval-model key. The daemon started with `--env-file PATH`
reads it as data, never as shell code, and so does the Python launcher for the checkout's `.env`; without
that option the daemon loads no file. The commands below use the checkout's `.env`; substitute your
file's path. For accepted variables, precedence and algorithm tunables, use the
[configuration reference](api.md#configuration) and [JSON template](../examples/service-config.json).
Keep keys outside version control.

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
the retrieval models. The hosted endpoints and models, and the variables that point them at another
OpenAI-compatible provider, are in the [configuration reference](api.md#configuration). Each startup makes one
small embedding request to check the key and read the vector dimension, and one rerank request when
reranking is enabled. Stored texts and queries are sent to the provider; use `self-hosted` if they must
stay on your machines.

Switching provider or embedding model on an existing data directory re-embeds its records at the next
start. The hosted default and BGE-M3 both use 1024 dimensions; for a model with another dimension, start
with a new data directory.

### 5. Start the shared daemon

Use a persistent directory outside the source checkout, and run the binary from where you installed it
(`target/fast/agent-memoryd` in a checkout, `~/.local/bin/agent-memoryd` from a release archive):

```sh
mkdir -p "$HOME/.local/share/agent-memory-service/mem0"
chmod 700 "$HOME/.local/share/agent-memory-service/mem0"
target/fast/agent-memoryd serve --env-file .env \
  --data-dir "$HOME/.local/share/agent-memory-service/mem0" --llm-concurrency 1
```

This runs in the foreground. Keep the terminal open or [run it as a service](#keep-services-running).
Before it listens, the daemon starts [SurrealDB](#install-surrealdb) on the store; add
`--surreal-bin /path/to/surreal` when `surreal` is not on PATH. Ctrl-C or SIGTERM stops SurrealDB, then the daemon.
`--llm-concurrency 1` sends one generation request at a time instead of the binary's default of 24; the
service setup keeps it. It belongs to this deployment, not to the binary's defaults. `--env-file` reads only the keys listed in
[configuration](api.md#configuration); a non-empty environment variable still wins. An explicit data
path avoids the binary's relative `./memory-data` default.

If agents capture dialogue, add `--outbox`: the daemon then also uploads the collector's outbox
(`~/.local/share/agent-memory-service/harness-outbox.sqlite3`, or the path given after `--outbox`)
and no separate uploader process is needed. The option is never on by default, so another daemon, such as
the installer's check daemon, never takes conversations from your outbox. The daemon log reports only lock and outbox
failures; a turn whose formation failed or was rejected shows in `agent-memoryd outbox status`.

For daemon-only settings such as `--prompt-variant en`, `--no-reranker` or `--config`, see
`agent-memoryd serve --help` and [API configuration](api.md#configuration) for defaults and precedence.

#### Python launcher

`scripts/serve.py` starts the same binary with the checkout's `.env`; existing setups use it, and it
needs Python 3.11. Its concurrency defaults to 1:

```sh
python3 scripts/serve.py \
  --memory-strategy mem0 \
  --data-dir "$HOME/.local/share/agent-memory-service/mem0" --outbox
```

The launcher accepts `--config`, `--bin`, `--model-provider`, endpoint/port options and concurrency; its own
data default is the checkout's relative `daemon-data`. It runs
`target/fast/agent-memoryd` unless `--bin` selects another binary, such as
`--bin target/release/agent-memoryd` after a release build. For daemon-only flags such as
`--prompt-variant en` or `--no-reranker`, launch the binary directly.

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
  -H 'X-Memory-Platform: demo' \
  -d '{"scope":{"project":"deployment-demo","session":"setup-check"},"turnId":"deployment-demo/setup-check/instruction-1","memoryType":"instruction","text":"Include sources when reporting project decisions."}'

curl -fsS http://127.0.0.1:8765/v1/get_context \
  -H 'Content-Type: application/json' \
  -H 'X-Memory-Platform: demo' \
  -d '{"scope":{"project":"deployment-demo","session":"setup-check"},"tokenBudget":1500}'
```

The instruction should appear in `instructions` and the rendered context. Restart with the same strategy and data directory and repeat `get_context` to check persistence. This deliberately creates a demo record; `delete_memory` is available through HTTP to remove it using the returned memory ID.

## Keep services running

For regular use, run the daemon under the user's service manager: a LaunchAgent on macOS or a systemd
user unit on Linux. `agent-memoryd service install` writes that file for you; the
[templates](#macos-launchagents) below show what it writes, for review or a manual setup. Self-hosted
model servers need units of their own ([self-hosted retrieval models](self-hosted-models.md#4-keep-the-servers-running)).
Use the same OS user that owns the outbox.

Stop a daemon or uploader you started by hand before enabling the service, to avoid port and lock
conflicts. Repeat the health and capture checks after switching.

### Install the service

Run `service install` with the binary the service should use; the file records that binary's absolute path:

```sh
target/fast/agent-memoryd service install --env-file .env --outbox
```

Without `--apply` it only previews: it prints the `serve` command, its environment and the complete file.
Rerun with `--apply` to write it. It then creates the data directory (and on macOS the log directory)
private to you, backs up a file it replaces under `~/.local/state/agent-memory-service/service-backups/`,
and prints the commands that start the service. A rerun with the same options writes nothing.

With `--start` as well, it runs those commands itself instead, in your login session, and waits up to
60 seconds until the daemon answers `/health`: launchd bootstraps the job, after booting out a loaded
one whose file changed; systemd reloads and enables the unit and starts or restarts it. A loaded,
unchanged service is left running, unless `--restart` asks to restart it, for example after replacing
its binary. `--start` refuses when a daemon the service did not start already answers on the port, and
a unit under another `--home`. The [one-command installer](mcp-harness.md#one-command-install) runs
`service install --apply --start` and keeps the options of a unit it finds, which it reads with
`agent-memoryd service status --json`.

| Option | Meaning |
|---|---|
| `--env-file PATH` | Required. The dotenv file with the provider keys; the daemon reads it at each start and the unit stores only its absolute path. It must exist; a file other users can read is reported |
| `--data-dir DIR` | Memory store (`~/.local/share/agent-memory-service/mem0`) |
| `--outbox [PATH]` | Also upload captured dialogue; without PATH the default outbox, written as an absolute path. Omit it when no agent captures dialogue |
| `--port` | Daemon port (8765) |
| `--llm-concurrency N` | `serve --llm-concurrency` (1) |
| `--binary PATH` | The executable the unit runs (default: the `agent-memoryd` you invoked) |
| `--surreal-bin PATH` | The SurrealDB `surreal` the daemon starts (`serve --surreal-bin`); the unit records its absolute path (default: the first `surreal` on PATH, refused when there is none) |
| `--manager launchd\|systemd`, `--home DIR` | Service manager (launchd on macOS, systemd elsewhere) and the home directory for the unit and default paths (tests) |
| `--apply` | Write the unit (default: preview) |
| `--start`, `--restart` | With `--apply`: load (or reload) the service and wait until the daemon answers; also restart an unchanged, loaded service |

Without `--start`, run the printed commands. On macOS:

```sh
plutil -lint "$HOME/Library/LaunchAgents/local.agent-memory-daemon.plist"
launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/local.agent-memory-daemon.plist"
```

On Linux:

```sh
systemctl --user daemon-reload
systemctl --user enable --now agent-memory.service
```

`agent-memoryd service status` shows the installed file, the command it runs, whether its env file
exists and is private, whether its SurrealDB executable is still installed, on Linux whether the unit is
enabled, and whether a daemon answers on its port. `service status --json` prints the unit's settings
(binary, env file, data directory, SurrealDB executable, outbox, port, concurrency) as one JSON object for
scripts, without the health check.
To remove the service, stop it first, with `launchctl bootout "gui/$(id -u)/local.agent-memory-daemon"`
or `systemctl --user disable --now agent-memory.service`, then run
`agent-memoryd service uninstall --apply`. It backs up and deletes the unit file and leaves the data,
outbox, logs and env file alone; on Linux it refuses while the unit is still enabled.

The service runs the binary and `surreal` at the recorded paths. A Homebrew or install-script upgrade of
SurrealDB keeps its path, so a restart picks up the new version; after moving `surreal`, rerun
`service install`. In a checkout, `make build` replaces
`target/fast/agent-memoryd` in place and a restart picks up the new build, while `cargo clean` removes
it; install a release binary's upgrades to the same path. The one-command installer records its
`~/.local/bin/agent-memoryd` instead, and a rerun that builds a new binary replaces it and restarts the
service. After changing an option, rerun `service install --apply` (with `--start` to restart it) or
restart the service as printed.

### macOS LaunchAgents

`service install` writes `~/Library/LaunchAgents/local.agent-memory-daemon.plist`. For a manual setup,
create the definition and log directories:

```sh
mkdir -p "$HOME/Library/LaunchAgents" "$HOME/Library/Logs/agent-memory-service"
```

and save this template there. Replace `/absolute/path` and `/Users/YOUR_USER` with real absolute paths, and
the SurrealDB path with the output of `command -v surreal`;
launchd does not expand `$HOME` or `~` inside plist strings.

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>local.agent-memory-daemon</string>
  <key>ProgramArguments</key>
  <array>
    <string>/absolute/path/agent-memory-service/target/fast/agent-memoryd</string>
    <string>serve</string>
    <string>--env-file</string>
    <string>/absolute/path/agent-memory-service/.env</string>
    <string>--data-dir</string>
    <string>/Users/YOUR_USER/.local/share/agent-memory-service/mem0</string>
    <string>--surreal-bin</string>
    <string>/opt/homebrew/bin/surreal</string>
    <string>--port</string>
    <string>8765</string>
    <string>--llm-concurrency</string>
    <string>1</string>
    <string>--outbox</string>
    <string>/Users/YOUR_USER/.local/share/agent-memory-service/harness-outbox.sqlite3</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ThrottleInterval</key><integer>10</integer>
  <key>Umask</key><integer>63</integer>
  <key>StandardOutPath</key><string>/Users/YOUR_USER/Library/Logs/agent-memory-service/daemon.stdout.log</string>
  <key>StandardErrorPath</key><string>/Users/YOUR_USER/Library/Logs/agent-memory-service/daemon.stderr.log</string>
</dict>
</plist>
```

Each command argument is a separate `<string>`; do not put a shell command or `&` into the array. The
daemon reads the keys from the `--env-file` at each start; no key belongs in the plist. `Umask` 63 is
077. With `--outbox` the daemon uploads the logged-in user's default outbox, so no uploader agent is
needed; omit both `--outbox` strings if capture is disabled. For self-hosted model servers, create one
more plist per server from the same template, with its own label (for example
`local.agent-memory-embed`), distinct log paths and the foreground command from
[self-hosted retrieval models](self-hosted-models.md#2-download-weights-and-start-the-servers).

Validate and load the selected plists in your logged-in GUI session:

```sh
plutil -lint "$HOME/Library/LaunchAgents/local.agent-memory-daemon.plist"
launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/local.agent-memory-daemon.plist"
launchctl print "gui/$(id -u)/local.agent-memory-daemon"
```

Validate each additional plist before loading it. LaunchAgents run during the user's login session. To stop the daemon, use `launchctl bootout "gui/$(id -u)/local.agent-memory-daemon"`; apply the same command to any model labels when shutting everything down. After changing a plist, boot it out and bootstrap the updated file. See Apple's [LaunchAgent guide](https://developer.apple.com/library/archive/documentation/MacOSX/Conceptual/BPSystemStartup/Chapters/CreatingLaunchdJobs.html) and the local `man launchctl` / `man launchd.plist` references.

### Linux user-systemd services

`service install` writes `~/.config/systemd/user/agent-memory.service`, which supervises the daemon; the
daemon also uploads captured dialogue. For a manual setup, replace the SurrealDB path with the output of
`command -v surreal`, and every `/absolute/path` and
`/home/YOUR_USER` in this template. For self-hosted model servers, add their units from
[self-hosted retrieval models](self-hosted-models.md#4-keep-the-servers-running).

```sh
mkdir -p "$HOME/.config/systemd/user"
```

Create `~/.config/systemd/user/agent-memory.service`:

```ini
[Unit]
Description=Agent Memory daemon

[Service]
ExecStart=/absolute/path/agent-memory-service/target/fast/agent-memoryd serve --env-file /absolute/path/agent-memory-service/.env --data-dir /home/YOUR_USER/.local/share/agent-memory-service/mem0 --surreal-bin /usr/local/bin/surreal --port 8765 --llm-concurrency 1 --outbox /home/YOUR_USER/.local/share/agent-memory-service/harness-outbox.sqlite3
Restart=on-failure
RestartSec=5
UMask=0077

[Install]
WantedBy=default.target
```

The daemon reads the keys from the `--env-file` at each start; no key belongs in the unit file. It probes
the retrieval models before opening the HTTP listener and exits if they do not answer, so a restart
retries once the network or model servers are up. `--outbox` uploads the user's default outbox; omit it
and its path when capture is disabled. In a hand-written unit, quote a path containing spaces and write
`%` as `%%`.

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

Within the selected data root, the store keeps its SurrealDB datastore (SurrealKV) at `store/db`, the
datastore's root password at `store/db.password` (mode 0600, created with the datastore) and `store/db.lock`,
which a running daemon holds so that a second daemon on the same data directory is refused. Only the daemon
uses the password, to sign in to the SurrealDB server it starts; nothing else needs that server's port.
Earlier versions kept the store under `tenants/default`, which the daemon moves to `store` when it starts, and
an unused full-text index at `store/fulltext`, which it removes. Versions before SurrealDB became an external
dependency ran SurrealDB 3.2 inside the daemon on the same `store/db`: it opens in place with SurrealDB 3.2 or a
later 3.x, and its first start creates the root user and the password file. Primary inputs, memories,
provenance, request receipts, and durable index work belong in the database; search indexes are rebuildable.
The capture outbox is a separate database and also needs a retention/backup policy.

For a consistent backup, pause new submissions, stop the uploader and daemon (which stops its SurrealDB server), then copy the **entire data root** and the outbox database with any `-wal`/`-shm` companions while no collector is writing. Keep `store/db.password` with `store/db`: the daemon cannot sign in to a datastore without its password. A copy of an active database directory is not a documented snapshot method. Preserve the matching configuration, strategy, source revision, and model identities. `/admin/export` provides selected audit tables; it is not a complete restore backup.

Before upgrading, make that backup, rebuild the binary from the updated source or install the new release binary at the same path, restart with the same data path/strategy, and check health and a known record. Upgrade SurrealDB the way you installed it (`brew upgrade surrealdb/tap/surreal`, or the install script again), within 3.x and while the daemon is stopped, after the same backup; the daemon starts the new `surreal` at its next start. Do not run two daemons against one data directory; the second one refuses to start. Restore with services stopped and a matching version/configuration. For unfinished legacy jobs, use the preserved version or a fresh store; see [compatibility](api.md#compatibility).

To re-embed every record on a running daemon:

```sh
target/fast/agent-memoryd reindex --all --url http://127.0.0.1:8765
```

`--vector` does the same; the vector index is the only rebuildable index. Changing the model provider, embedding model, or a self-hosted model's alias/quantization causes re-embedding at the next start; expect rebuild work and verify retrieval afterwards. Review local outbox retention periodically: `completed`, `failed`, and `rejected` rows remain until deliberately cleaned up.

### Stop and restart

With the terminal/script setup, stop the foreground daemon (and a standalone uploader, if you run one) with Ctrl-C, which also stops its SurrealDB server, then any self-hosted model servers with `scripts/self_hosted_models.sh stop`. Restart self-hosted models first, then the daemon with the same arguments/data directory. Accepted outbox jobs resume polling; pending ones retain their payloads for retry. Durable unfinished server jobs resume when the daemon starts.

With the LaunchAgent setup, use `launchctl bootout` for the selected service labels and `launchctl bootstrap` to start them again. With systemd, stop the daemon and any model units using `systemctl --user stop` and start them with `systemctl --user start`. Use the process manager's logs instead of `results/llama/`, which belongs to the self-hosted helper script.

### Common problems

| Symptom | Check or action |
|---|---|
| `model API key not configured` | Set `MEMORY_MODEL_API_KEY` (or `QWEN_API_KEY`) in `.env` or the environment, or switch to `MEMORY_MODEL_PROVIDER=self-hosted` |
| `embedding model probe failed` with HTTP 401/403/404 | Check the model key, `MEMORY_EMBED_URL`/`MEMORY_RERANK_URL` and model names; the rerank base must offer `/reranks` |
| Self-hosted model not ready | See [self-hosted troubleshooting](self-hosted-models.md#troubleshooting) |
| Daemon fails before listening | Check `serve --help`; invalid config fields/tunables are rejected, and the retrieval models must answer |
| Service restarts repeatedly or never answers | Run `agent-memoryd service status`; read `~/Library/Logs/agent-memory-service/daemon.stderr.log` or `journalctl --user -u agent-memory.service`. A missing env file, a missing key or a port already in use stops startup |
| `LLM API key not configured` or provider 401 | Check the env file, exported overrides, and provider access; direct binary launches read a dotenv file only with `--env-file` |
| Legacy jobs at startup | Complete them with the preserved version or use a fresh store; see [compatibility](api.md#compatibility) |
| `SurrealDB is not installed` or `not an executable SurrealDB` | [Install SurrealDB](#install-surrealdb), or name it with `--surreal-bin`. A service runs the path it was installed with; after moving `surreal`, rerun `service install` (or the one-command installer) |
| `SurrealDB (...) did not start on ...` | The lines after it are SurrealDB's own output. Check `surreal version` (3.2 or a later 3.x) and that the data directory is writable |
| `... is in use by another agent-memoryd` | Another daemon serves that data directory; stop it first (`agent-memoryd service status` shows the service's) |
| `cannot sign in to SurrealDB as root` | `store/db.password` does not belong to `store/db`; restore both from the same backup |
| `the store's SurrealDB server stopped` in the daemon log | SurrealDB exited under the daemon, which then exits so its service restarts both; the log line carries SurrealDB's last output |

Inspect a daemon task through HTTP using the actual receipt ID:

```sh
curl -fsS http://127.0.0.1:8765/v1/get_task \
  -H 'Content-Type: application/json' \
  -H 'X-Memory-Platform: codex' \
  -d '{"requestId":"REPLACE_WITH_RETURNED_REQUEST_ID"}'
```

The daemon enforces loopback binding. For another machine, keep this boundary and deploy an authenticated gateway or an authenticated tunnel to the local service. Platform labels do not authorize access. The supplied MCP bridge and uploader have no gateway credential options; direct gateway authentication needs an adapter change. A remote stdio bridge/tunnel must also preserve transport and scope correctly. This repository does not provide a turnkey multi-user remote deployment.


For MCP, capture and uploader failures, see [client troubleshooting](mcp-harness.md#troubleshooting).
For offline build verification, see [CONTRIBUTING](../CONTRIBUTING.md#checks).
