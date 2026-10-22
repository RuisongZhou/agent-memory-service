# Evaluation tools

All Mem0 SDK baselines, research datasets, evaluation dependencies, protocols, reports,
and experimental scripts live in this directory. The Rust Mem0 algorithm and its
vendored extraction prompt are service implementation and remain under `crates/`.
See [the evaluation protocol](docs/evaluation.md) before running an experiment.
Commands below run from the repository root.

Main runs only Mem0. The historical dual-strategy clients and replay tools, which need a Memos daemon,
remain on the [`codex/mem0-memos`](https://github.com/RuisongZhou/agent-memory-service/tree/codex/mem0-memos)
branch together with the legacy extraction examples and recovery scripts. Current Mem0/SDK comparison tools
remain here. No frozen protocol, raw evidence or local result/store is rewritten by the branch split.

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

`make test` runs the evaluation tests, with intercepted model calls and local Mem0/Qdrant components.
Service deployment and service checks do not need these dependencies or the dataset. The service's
script tests (`make test-scripts`) and mock/stub HTTP and MCP checks are in [scripts/](../scripts/README.md).
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

## Entry points

| Entry point | Purpose |
|---|---|
| `mem0_run.py` | Two-perspective Mem0 SDK ingest/search using the daemon's BGE-M3 endpoint |
| `mem0_migration.py` | Explicit fresh-store strategy comparison |
| `locomo_full.py` | Explicit full LoCoMo comparison with frozen manifests |
| `answer_and_judge.py` | Unified answer/judge, reference answer prompt and metrics |
| `usage_stats.py` | Request-ledger summaries of calls and tokens |
| `report.py` | Cohort-aware result tables and recorded usage |
| `preliminary_summary.py` | Local usage-export aggregation and deduplication |
| `pilot_plan.py` | Evidence-covered session/question manifest |

These entry points have different stages: ingestion and answer/judge stages can make
paid requests. Never use a paid experiment or the full dataset as an automatic check.
Use fresh isolated stores and preserve the protocol before seeing scores.

### Node SDK 3.3.1 offline comparison

The historical Python baseline remains pinned to `mem0ai==2.2.1`; its version is independent
of the Node SDK. Install the Node comparison in an ignored evaluation cache:

```sh
npm install --prefix eval/.cache/mem0-sdk331 --legacy-peer-deps --save-exact mem0ai@3.3.1 better-sqlite3@12.6.2
.venv/bin/python eval/mem0_sdk331_compare.py --sdk-root eval/.cache/mem0-sdk331 \
  --output-dir eval/results/mem0-sdk331-new-run
```

This deliberately selects the SDK-supported built-in entity/stemming fallback, without
optional `compromise` or `natural`. The runner loads the actual SDK with SQLite stores,
uses the daemon's mock embedding endpoint for both arms, and substitutes only generation
responses. Node is an evaluation dependency, not a service runtime dependency. The output
directory must be new; preserve preliminary failures and final protocol fingerprints.

### Explicit real-model SDK 3.3.1 pilot

`mem0_sdk331_real.py` compares the real service and actual Node SDK on the first four
Conv-26 sessions (76 utterances, 35 evidence-covered questions). Preparation is offline;
ingestion and evaluation make paid calls and require explicit authorization. Both arms
use the same generation model, real embedding endpoint, historical observation date,
and 2,000-token whole-record context budget. Use a fresh result directory:

```sh
.venv/bin/python eval/mem0_sdk331_real.py --sdk-root eval/.cache/mem0-sdk331 \
  --root eval/results/mem0-sdk331-real-new --stage prepare
# After explicit authorization and starting the configured real embedding server:
.venv/bin/python eval/mem0_sdk331_real.py --sdk-root eval/.cache/mem0-sdk331 \
  --root eval/results/mem0-sdk331-real-new --stage all
```

The runner freezes source/binary hashes, keeps independent stores and raw provider
responses, and runs three retrieval passes. SDK corpus export uses `getAll(topK=10000)`
with a collection-count check; all retrieved IDs must exist in the exported corpus.
`mem0_sdk331_judge_audit.py` is a separate, explicit post-run consistency audit and may
make additional paid judge calls. Preserve original labels and disclose any amendment.
The local 2026-10-14 run's final report is
`results/mem0-sdk331-real-20261014/comparison.md`; its export correction and original
incomplete snapshots remain alongside the final results.

`mem0_sdk331_optimization.py` compares two service binaries (`before` and `after`)
on that same fixed cohort. `--mode replay` serves captured extraction responses
locally and asserts identical formed facts, retrieval scores and contexts;
`--mode live` makes explicitly authorized real-model calls. It freezes both binary
hashes, alternates ingestion order, uses isolated stores, and shares the answer or
judge result for identical requests. Pass `--original` pointing to the original
SDK pilot, `--before` and `--after` to frozen binaries, and a fresh `--root`.
Preparation is the default; `--stage all` executes the selected mode. The actual
SDK compatibility check remains the separate offline comparison above.

## Experiments and diagnostics

| Entry point | Purpose |
|---|---|
| `mem0_v3_context.py` | Offline fixed-input comparison of V3 message handling, prompt boundaries and persistence; model doubles, not an accuracy benchmark |
| `mem0_sdk331_compare.py` | Actual Node SDK 3.3.1 comparison of prompts, entities and hybrid scores with the same local mock embeddings |
| `extraction_audit.py` | Count-only audit of saved formation logs or a running daemon's store; no generation |

Historical and new runs live under `eval/results/`, including `legacy-demo/`,
`locomo-full-mem0-20261011-v3/`, `mem0-migration-20261011/`, and the Conv-26,
QA and integrity experiments. Root `results/` contains service artifacts only.
The local `eval/results/relocation-20261012.json` records the directory migration
and verified file checksums. Generated audit/report links use the new paths.

Frozen source/binary/protocol evidence and raw records retain their original bytes.
An old path embedded in a frozen manifest, trace or source snapshot records the run's
original location; use the new path for CLI arguments and when inspecting saved records.
Source changes alter source manifests: do not edit an existing manifest to resume with
different code. Use the recorded source snapshot for that run, or a fresh result
directory for a new experiment.

Runtime results, stores, downloaded data and caches stay ignored. Only curated
historical aggregates belong in [docs/benchmarks/](docs/benchmarks/), including the
[2026-10-12 additive comparison](docs/benchmarks/mem0-additive-20261012.md).
Copied code and dataset terms remain in [licenses/](licenses/), with provenance listed
in [THIRD_PARTY](../THIRD_PARTY.md).
