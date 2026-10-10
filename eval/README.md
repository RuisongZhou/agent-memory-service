# Evaluation tools

Start with [the protocol](../docs/evaluation.md). All commands run from the repository root; `make setup` creates `.venv`, locally downloads the checksum-pinned research dataset and prepares tokenizer caches.

| Entry point | Purpose |
|---|---|
| `memory_run.py` | Service ingest, jobs, buffer search, purge and main search |
| `mem0_run.py` | Two-perspective mem0 ingest/search using the daemon's BGE-M3 endpoint |
| `answer_and_judge.py` | Unified answer/judge, reference answer prompt and metrics |
| `usage_stats.py` | Request-ledger summaries of calls and tokens |
| `report.py` | Cohort-aware result tables and recorded usage |
| `preliminary_summary.py` | Local usage-export aggregation and deduplication |
| `pilot_plan.py` | Evidence-covered two-session/ten-question manifest |
| `conv26_report.py` | Complete-pilot result and usage aggregation |
| `common/` | Dataset mapping, environment, protocols, prompts, metrics and usage |
| `tests/` | Offline regression tests with real mem0/Qdrant and intercepted LLM calls |

Run `make test`. Python tests do not read a public repository API key or invoke a paid model. The original eval entry point is now `memory_run.py`; request headers use `X-Memory-*`. No compatibility aliases or previous naming are shipped.

Runtime results, stores, downloaded data and caches are ignored. Only curated historical aggregates are committed under `docs/benchmarks/`.
