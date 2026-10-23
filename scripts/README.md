# Service scripts

This directory contains service operations and service verification. Mem0 SDK baselines,
dataset setup, evaluation protocols and experimental tools live in [eval/](../eval/README.md).
Run commands from the repository root.

| Script | Purpose | Generation calls |
|---|---|---|
| `serve.py` | Python alternative to `agent-memoryd serve --env-file .env`: launch the daemon with the checkout's `.env` and concurrency 1 (`agent-memoryd service install` runs the binary as a service instead) | Starting/recovering jobs can invoke the configured provider |
| `install.sh`, `install/` | One-command install: a check of the user's SurrealDB 3.x (never installed here), the binary in `~/.local/bin`, the model key in a private env file and the daemon as a LaunchAgent / systemd user service (`runtime.py`), then MCP, capture and recall registration for Codex, Claude Code and WorkBuddy (as `agent-memoryd hook` commands) or MCP registration for DeepSeek Harness, verified against a temporary mock daemon first | None for the checks, where a local stub forms one fact; the started service probes the retrieval models, and `--verify-model` forms one synthetic fact with the real models |
| `self_hosted_models.sh` | Optional: download BGE GGUF weights and start, stop or inspect your own llama-server retrieval models | None |
| `llama_http_smoke.py` | Self-hosted provider integration with already running real llama-server models | Local deterministic generation stub only |
| `tests/` | Tests of the capture and recall hooks, installer and launchers (`make test-scripts`); they need only pytest, not the evaluation environment or the dataset, and the hook tests need the built daemon | None |
| `check_public.py` | Scan the Git index for credentials | None |

See [deployment instructions](../docs/deployment.md#install-and-deploy) for startup,
[self-hosted retrieval models](../docs/self-hosted-models.md) for the model servers and
[offline verification](../CONTRIBUTING.md#checks) for the mock/stub checks.
The real-model smoke is optional and is not an accuracy benchmark.
Service logs and check artifacts belong under the root `results/`; the real-model
smoke defaults to `results/service-smoke/llama-http-smoke.json`. Evaluation runs,
protocols and raw benchmark evidence belong under `eval/results/`.
