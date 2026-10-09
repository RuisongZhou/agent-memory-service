# Evaluation tools

Start with [the protocol](../docs/evaluation.md). All commands run from the repository root; `make setup` creates `.venv`, locally downloads the checksum-pinned research dataset and prepares tokenizer caches.

| Entry point | Purpose |
|---|---|
| `memory_run.py` | Service ingest, jobs, buffer search, purge and main search |
| `mem0_run.py` | Two-perspective mem0 ingest/search using the daemon's BGE-M3 endpoint |
| `answer_and_judge.py` | Unified answer/judge, reference answer prompt and metrics |
| `costs.py` | Request-ledger summaries; CNY conversion is explicitly estimated |
| `report.py` | Cohort-aware result tables and conditional cost scenarios |
| `preliminary_summary.py` | Local usage-export and balance reconciliation |
| `pilot_plan.py` | Evidence-covered two-session/ten-question manifest |
| `conv26_report.py` | Historical complete-pilot aggregation, with official CNY rates |
| `common/` | Dataset mapping, environment, protocols, prompts, metrics and billing |
| `tests/` | Offline regression tests with real mem0/Qdrant and intercepted LLM calls |

Run `make test`. Python tests do not read a public repository API key or invoke a paid model. The original eval entry point is now `memory_run.py`; request headers use `X-Memory-*`. No compatibility aliases or previous naming are shipped.

Runtime results, stores, downloaded data and caches are ignored. Only curated historical aggregates are committed under `docs/benchmarks/`.
