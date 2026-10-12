# Evaluation tools

All Mem0 SDK baselines, research datasets, evaluation dependencies, protocols, reports,
and experimental scripts live in this directory. The Rust Mem0 algorithm and its
vendored extraction prompt are service implementation and remain under `crates/`.
See [the evaluation protocol](docs/evaluation.md) before running an experiment.
Commands below run from the repository root.

## Setup and offline checks

```sh
make setup
make test
```

`make setup` invokes `eval/setup_eval.py`: it creates the root `.venv`, installs
`eval/requirements.txt`, downloads the checksum-pinned dataset to
`eval/data/locomo10.json`, and prepares tokenizer caches. To download or verify only
the dataset, use `python3 eval/download_data.py`. Neither setup command invokes a
paid model or evaluates the dataset.

Python tests use intercepted model calls and local Mem0/Qdrant components.
Service deployment does not need these dependencies or the dataset. The service's
mock/stub HTTP and MCP checks remain in [scripts/](../scripts/README.md).
If invoking a vendored upstream CLI directly, use `eval/` as its working directory;
its original relative `results/` defaults then stay under `eval/results/`.

## Layout

```text
eval/
  *.py          Evaluation entry points, setup and analysis tools
  common/       Shared dataset, protocol, audit, prompt and usage helpers
  tests/        Offline regression tests
  data/         Dataset documentation and ignored local downloads
  docs/         Evaluation protocol and curated historical benchmark summaries
  licenses/     Dataset and copied evaluation-code notices
  third_party/  Pinned Mem0 evaluation assets, preserved verbatim
  results/      Ignored historical and new-run artifacts, stores, receipts and raw evidence
  stores/       Ignored local baseline stores
  .cache/       Ignored tokenizer and SDK caches
```

## Current entry points

| Entry point | Purpose |
|---|---|
| `memory_run.py` | Memos service ingest, jobs, buffer search, purge and main search |
| `mem0_run.py` | Two-perspective Mem0 SDK ingest/search using the daemon's BGE-M3 endpoint |
| `mem0_migration.py` | Explicit fresh-store strategy comparison |
| `locomo_full.py` | Explicit full LoCoMo comparison with frozen manifests |
| `answer_and_judge.py` | Unified answer/judge, reference answer prompt and metrics |
| `usage_stats.py` | Request-ledger summaries of calls and tokens |
| `report.py` | Cohort-aware result tables and recorded usage |
| `preliminary_summary.py` | Local usage-export aggregation and deduplication |
| `pilot_plan.py` | Evidence-covered session/question manifest |
| `conv26_report.py` | Complete-pilot result and usage aggregation |

These entry points have different stages: ingestion and answer/judge stages can make
paid requests. Never use a paid experiment or the full dataset as an automatic check.
Use fresh isolated stores and preserve the protocol before seeing scores.

## Experiments and diagnostics

| Entry point | Purpose |
|---|---|
| `extraction_compare.py` | Explicit real-model extraction comparisons in isolated service/SDK stores |
| `real_extraction_example.py` | Explicit real-model Chinese formation example with request/response evidence |
| `locomo_trace_example.py` | Explicit source-to-formation/retrieval trace example |
| `extraction_audit.py` | Count-only audit of saved formation logs or already opened daemon tenants; no generation |
| `longterm_context_ablation.py` | Remove the recent-turn section from saved contexts without purging sources |
| `diagnose_replay.py` | Inspect historical QA, source, formation and retrieval chains |
| `fair_replay.py` | Replay saved corpora with a shared whole-record context budget |
| `recover_failed_batches.py` | Explicitly retry historical failed source batches |
| `integrity_benchmark.py` | Fixed retained-snapshot replay and compatible-cohort reports |

Historical and new runs live under `eval/results/`, including `legacy-demo/`,
`locomo-full-mem0-20261011-v3/`, `mem0-migration-20261011/`, and the Conv-26,
QA and integrity experiments. Root `results/` contains service artifacts only.
The local `eval/results/relocation-20261012.json` records the directory migration
and verified file checksums. Generated audit/report links use the new paths.

Frozen source/binary/protocol evidence and raw records retain their original bytes.
An old path embedded in a frozen manifest, trace or source snapshot records the run's
original location. `common.env.resolve_historical_result_path` resolves saved
repository `results/` references to `eval/results/` when the integrity tools read them;
absolute paths from an older checkout ending in `agent-memory-service/results/`
are handled too. Use the new path for CLI arguments and when inspecting saved records.
The dataset relocation and helper/source changes alter source manifests: do not edit
an existing manifest to resume with different code. Use the recorded source snapshot
for that run, or a fresh result directory for a new experiment.

Runtime results, stores, downloaded data and caches stay ignored. Only curated
historical aggregates belong in [docs/benchmarks/](docs/benchmarks/).
Copied code and dataset terms remain in [licenses/](licenses/), with provenance listed
in [THIRD_PARTY](../THIRD_PARTY.md).
