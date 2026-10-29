# Changelog

What changed between versions of Agent Memory Service, and what to do when you upgrade. History before this file is in
`git log`.

**No version has been published yet.** The workspace version is 0.1.0. Release archives and the installer's web UI
download exist only for a published version tag (see [Releases](CONTRIBUTING.md#releases)), so install from source,
and install Node.js and npm to get the web UI.

**Upgrading from source:** `git pull`, then `bash scripts/install.sh service --apply`. It rebuilds, replaces the
binary and the web UI and restarts the service, however many platforms are connected. Rerun
`bash scripts/install.sh connect --host HOST --apply`, with your usual options, only for a platform an entry below
names; `connect` is idempotent, so rerunning it is always safe.

## Unreleased

### Upgrade notes

Each entry says what to do if your checkout or data predates the change.

- **The project comes from the session's folder.** No command names a project, and a read that gives no `scope` uses the
  session's project instead of every project: pass `project: "*"` to search them all. Hooks written by earlier
  installers pass `--project` and fail with `unexpected argument '--project'`: rerun `connect` for each platform. An
  earlier Claude Code installation in one workspace also wrote `<workspace>/.claude/settings.local.json` and
  `<workspace>/.mcp.json`; delete their `shared-memory` hook entries and server there. See
  [project routing](docs/mcp-harness.md#project-routing).
- **The installer has two commands.** `install.sh service` installs the daemon, and is also the upgrade;
  `install.sh connect --host HOST` registers one platform. The earlier single form `install.sh --host HOST` prints the
  usage and exits 2. See the [installer](docs/mcp-harness.md#installer).
- **Hooks are `agent-memoryd` commands.** Hooks that run `scripts/harness_memory.py` or `scripts/harness_recall.py`
  point at scripts that are no longer in the repository, and make Python exit 2, which blocks prompts: rerun `connect`
  with the same options to replace them ([migration](docs/mcp-harness.md#migrate-from-the-python-hook-scripts)). Codex
  turns are captured with hooks instead of `notify`: `connect` removes the old `notify` entry and puts back the program
  it wrapped. Trust Codex's hooks again in `/hooks`.
- **Retrieval models are hosted by default.** The provider defaults to `api`. A llama-server deployment must set
  `MEMORY_MODEL_PROVIDER=self-hosted` before restarting; otherwise the daemon uses the hosted models (with
  `QWEN_API_KEY` when no model key is set), sends stored text to the provider and re-embeds every store at the next
  start. `scripts/llama_models.sh` and `scripts/llama_servers.sh` are now `scripts/self_hosted_models.sh download` and
  `start|stop|status`. See [self-hosted retrieval models](docs/self-hosted-models.md).
- **SurrealDB is installed separately.** It is no longer compiled into the daemon: install SurrealDB 3.2 or a later
  3.x ([install SurrealDB](docs/deployment.md#install-surrealdb)); without `surreal` on PATH or `--surreal-bin` the
  daemon does not start. An existing `store/db` opens in place, and the first start creates the datastore's root user
  and `store/db.password`. A store under `tenants/default` moves to `store` at the next start, and an unused
  `store/fulltext` index is removed.
- **Only Mem0 is available.** Retired strategies and buffer/batch/maintenance tunables are rejected, and
  `/admin/run_jobs` and `/admin/seed` return HTTP 404. See [compatibility](docs/api.md#compatibility).
- **Recall is selective.** The reranker is now loaded by default, and a search returns only memories whose reranker
  score clears the reranker's floor, so a prompt with no relevant memory gets none; the SessionStart hook injects the
  explicit instructions and a one-line index instead of the newest facts. The hosted `qwen3.7-text-rerank` and a
  self-hosted `bge-reranker-v2-m3` have built-in floors; another rerank model needs `mem0RecallFloor`, or the daemon
  does not start. `"mem0Rerank": false` in your configuration keeps the earlier top-k search: remove it to select.
  A deployment without a reranker (a hosted provider with no `/reranks` route, or a self-hosted setup running only the
  embedding server) now fails to start: set `"mem0Rerank": false`, or start the reranker.
  Each search now makes one rerank call (about 0.2 s hosted) and no entity-embedding call. The entity projection drops
  its TOPIC rows and rebuilds once, when the next turn is formed. See [selective recall](docs/design/selective-memory.md).
- **Recall skips prompts no person typed.** The recall hooks inject nothing, and search nothing, for prompts a host
  writes itself: the Codex app's own requests (thread titles, suggested messages, reviews), which run in threads
  without a transcript, and Claude Code's background-task notifications, other sessions' messages and app events. The search query also drops `<system-reminder>` blocks. Nothing to rerun: `install.sh service --apply`
  replaces the binary the hooks run. See
  [selective recall](docs/design/selective-memory.md#2-read-path-selective-recall).
- **Formation batches a session's turns.** `mem0SegmentTurns` now defaults to 4: one extraction call forms up to four
  queued turns of a session, and its prompt marks each turn and asks for what each would give alone. On LoCoMo this
  made 71% fewer extraction calls and covered as many questions as forming every turn alone (see
  [selective recall](docs/design/selective-memory.md#5-results)). A turn's facts become searchable when its segment
  forms: after four turns, at the session's end marker, when a caller waits for the result (`sync`), or once the
  oldest turn has waited `mem0SegmentWaitSecs` (600 s), and each of its facts names every turn of the segment as its
  source. Set `"mem0SegmentTurns": 1` to form every turn alone. Formation also shows the model the facts the session
  already gave, which reduces duplicates without a setting.
