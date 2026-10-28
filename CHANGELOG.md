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
