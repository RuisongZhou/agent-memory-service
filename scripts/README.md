# Service scripts

This directory contains service operations and service verification. Mem0 SDK baselines,
dataset setup, evaluation protocols and experimental tools live in [eval/](../eval/README.md).
Run commands from the repository root.

| Script | Purpose | Generation calls |
|---|---|---|
| `serve.py` | Launch the daemon using local `.env` settings | Starting/recovering jobs can invoke the configured provider |
| `install.sh`, `install/` | One-command MCP, capture and recall registration for Codex, Claude Code and WorkBuddy, and MCP registration for DeepSeek Harness, verified against a temporary mock daemon | None; a local stub forms the check's one fact |
| `harness_memory.py` | Collect completed agent dialogue into an outbox, upload inputs and poll receipts | Capture is local only; uploaded dialogue can trigger service formation |
| `harness_recall.py` | Inject bounded `get_context` / `search_memories` results through SessionStart and UserPromptSubmit hooks | None; reads only |
| `self_hosted_models.sh` | Optional: download BGE GGUF weights and start, stop or inspect your own llama-server retrieval models | None |
| `http_smoke.py` | Offline daemon and MCP integration checks, including raw Mem0 writes and restart | None |
| `mem0_http_smoke.py` | Offline checks of the service's default Mem0 algorithm | Local deterministic stub only; does not import the Mem0 SDK |
| `mem0_history_http_smoke.py` | Mem0 message windows, session/raw/failure isolation and restart checks | Local deterministic stub only; includes exhausted job retries |
| `mem0_hybrid_http_smoke.py` | Entity persistence, correction/delete repair, scope and restart checks | Local deterministic stub only |
| `llama_http_smoke.py` | Self-hosted provider integration with already running real llama-server models | Local deterministic generation stub only |
| `check_architecture.py` | Enforce Rust boundaries and separation between service and evaluation files | None |
| `check_public.py` | Check the Git index for weights, secrets and generated state | None |

See [deployment instructions](../docs/deployment.md#install-and-deploy) for startup,
[self-hosted retrieval models](../docs/self-hosted-models.md) for the model servers and
[offline verification](../CONTRIBUTING.md#checks) for the mock/stub checks.
The real-model smoke is optional and is not an accuracy benchmark.
Service logs and check artifacts belong under the root `results/`; the real-model
smoke defaults to `results/service-smoke/llama-http-smoke.json`. Evaluation runs,
protocols and raw benchmark evidence belong under `eval/results/`.
