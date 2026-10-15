# Evaluation protocol

> Historical dual-strategy protocol. Memos commands and source references in this document require [`codex/mem0-memos`](https://github.com/RuisongZhou/agent-memory-service/tree/codex/mem0-memos) or the recorded frozen binary. Main supports only Mem0. See [current entry points](../README.md). Frozen protocols and results are preserved unchanged.

The evaluation runs locally using Python runners, the Rust daemon, mem0 OSS 2.2.1 and an official Qwen endpoint. LoCoMo supplies data; there is no hosted benchmark platform in this workflow.

## Fixed mapping

One conversation is a project/tenant. Sessions are processed in chronological order. Adjacent utterances form one user/assistant carrier turn; odd final utterances receive an empty assistant. Image captions are appended as text. Session dates are interpreted as UTC, with one-second offsets per utterance. Categories 1–4 contribute 1540 questions across ten conversations; category 5 is excluded.

The adapted English prompt treats both people as first-hand sources and preserves life events. The original Chinese prompt retains agent-role and long-term-value assumptions. Their P3 limits differ: 350 English words versus 500 Unicode characters. Both retain verbatim source spans of at most 1024 Unicode characters (including spaces and punctuation). This comparison includes semantics and limits, not only language.

## Pipeline

Memos conversation formation uses frequent P1 facts (`facts-object-v2`) and independent session-end P2 experiences/summary (`session-experience-object-v1`), with JSON object mode and strict local structural/provenance validation. Explicit fact writes use the same fact contract. See [API reference](../../docs/api.md). The daemon persists every raw HTTP response in `llm_responses`, including failed attempts and retries. Historical combined P1/P2 runs retain their original results and prompt hashes; a protocol change requires a fresh result/store directory.

The legacy service workflow requires `MEMORY_STRATEGY=memos`; `memory_run.py` and archived replays reject a connected daemon reporting `mem0`. Launching extraction comparisons, retained-store replays and the local llama-server smoke pins `memos`, including when the parent environment selects Mem0. Frozen older control binaries ignore this environment variable and retain their original pipeline. The normal service default remains `mem0`.

1. Check rules, fake HTTP/MCP integration and actual model numerical correctness before paid calls.
2. Ingest each session and wait for pending extraction/index tasks and failure counts to be zero.
3. Explicitly run profiles and project notes with `asOf = last ingested session start + 1 day`.
4. Retrieve every selected question while the recent buffer is retained.
5. Purge completed source turns using `retentionDays=0`; verify remaining turns and vectors are zero for the main setting. Unfinished-task evidence may now remain: the existing runner refuses to proceed while turns remain. Report and audit that condition; do not bypass the retention guard or silently relax the main-setting protocol.
6. Retrieve the same questions again for the main setting using `touch:false`.
7. Answer and judge using the same model and prompts. Add the mem0-original answer prompt as a separate reference setting.
8. Compare complete cohorts and audit usage, source rejection and clock effects.

The daemon supplies BGE-M3 embeddings to both systems. Retrieval is measured sequentially to avoid competing GPU workloads. mem0 searches both perspectives with top 30 each; the service returns top 10 with a 2000-token budget, plus profiles and recent turns. Context size uses cl100k_base. Client-side latency includes both mem0 perspective searches; it excludes answer generation.

## Run a new pilot

After building and downloading models locally, start a fresh store:

```sh
.venv/bin/python scripts/serve.py --memory-strategy memos --data-dir eval/results/pilot/service-store --max-calls 512 --llm-concurrency 4
.venv/bin/python eval/memory_run.py --base-url http://127.0.0.1:8765 --tag memory-en --prompt-variant en --convs conv-26 --results-dir eval/results/pilot
.venv/bin/python eval/memory_run.py --base-url http://127.0.0.1:8765 --tag memory-zh --prompt-variant zh --convs conv-26 --results-dir eval/results/pilot
.venv/bin/python eval/mem0_run.py --embed-url http://127.0.0.1:8765/v1 --tag mem0 --convs conv-26 --workers 2 --results-dir eval/results/pilot --stores-dir eval/results/pilot/mem0-stores
.venv/bin/python eval/answer_and_judge.py --contexts eval/results/pilot/memory-en/main/contexts.jsonl --results-dir eval/results/pilot
.venv/bin/python eval/report.py --results-dir eval/results/pilot
```

These are paid commands, not CI checks. Run `--stage` separately when inspecting extraction before retrieval. All settings need answering/judging before comparison. Use a fresh store and run tag for incompatible changes. `memory_run.py` routes en/zh tenants by suffix; a tag alone does not isolate the daemon store.

## Metrics and recovery

`eval/integrity_benchmark.py` performs the separate fixed 32-question retained-snapshot replay described in architecture.md. Its current-versus-improved control uses a preserved binary and cloned primary stores with the same live models; it does not re-run formation or claim historical Mem0 latency is a current measurement. A copied store is mandatory when launching through `--bin`. Per-pass timings, returned contexts, semantic corpus hashes and exact answer fingerprints remain in the local result directory. `--report` refuses incomplete or incompatible answer cohorts.

J is the LLM-judge label accuracy using mem0's ACCURACY_PROMPT. F1 and BLEU-1 follow copied mem0 metric definitions and are tested for equivalence. Judge failures preserve already-paid answers; resumes verify the context fingerprint. Invalid labels never count as correct answers.

Usage records include returned malformed replies and retries. The service exports tenant-persistent usage with stable IDs; the Python ledger de-duplicates them. Calls and tokens are recorded without pricing or monetary budgets.

Historical 2023 data is written at actual 2026 wall-clock time. Existing recency rules clamp future last-access times to zero age. Preserve this limitation when interpreting results. mem0 OSS uses dense Qdrant in this environment; optional BM25/full spaCy components were absent. The pilot does not reproduce the Mem0 cloud platform paper.

The `.env` generation settings are shared with the daemon (`QWEN_API_KEY`, `QWEN_BASE_URL`, `LLM_MODEL`); `EVAL_MODEL` optionally overrides the evaluation model. Existing DeepSeek benchmark records remain historical evidence.

The archived-replay audit in `eval/fair_replay.py` uses a shared 2,000-token cap with whole-record packing, disables recent turns for every arm, and rebuilds copied Mem0 vectors with the same local HTTP embedder. Its source is historical formation, not a fresh end-to-end ingestion run. `eval/diagnose_replay.py` renders every selected QA and its source/formation/retrieval chain. `eval/recover_failed_batches.py` explicitly replays failed source batches using the Fast-build `replay_facts` example; it does not use QA answers, and partial failures must be reported. `--judge-cache` reuses only exactly matching judge prompts and model settings across arms. This is development-cohort validation; a separate held-out run is needed before claiming generalization.

## Fresh-store strategy comparison

`eval/mem0_migration.py` runs an explicitly requested, paid small-sample experiment separately from
offline verification. Freeze `protocol.json` and baseline source/binary snapshots before changes or scores.
All arms start with empty stores and consume the same chronological LoCoMo source sessions. Select only
questions whose reference evidence is contained in those sessions; never pass answers/evidence to formation
or retrieval. Memos retains its native paired-turn pipeline; Mem0/new use both user perspectives with
identical message batching and observation dates. Report that work difference and total provider attempts.

`--stage ingest` measures complete formation and index visibility, excluding startup/exports. `--stage search`
uses generation-disabled processes, sequential passes, a shared whole-record tokenizer cap and no recent turns.
Only `answer_and_judge.py` generates evaluation answers, with common prompts/model/caps and an exact shared
judge cache. Preserve raw provider replies, receipts, corpus snapshots, runtime/binary/source fingerprints,
and per-request timing locally. Do not compare fresh formation with historical saved snapshots.

The small-run report requires complete protocol QA and retrieval-pass coverage, stable contexts, matching answer fingerprints and labels, and nonempty before/after corpus audits verified against the saved snapshots. Missing or inconsistent evidence prevents report generation. Accuracy counts, wrong question IDs and corpus sizes are generated from that run's artifacts.

For the 2026-10-11 experiment the ignored protocol/results live in `eval/results/mem0-migration-20261011/`:
four conv-26 sessions (76 utterances) and all 35 supported category 1–4 questions. This is a small development
cohort, not held-out evidence of generalization. No retrieval parameters are tuned from its scores.

Offline checks: `make build test-rust selftest test smoke`, followed by
`.venv/bin/python scripts/mem0_http_smoke.py`. All compilation uses the Fast profile; these checks never
call a paid provider or run the dataset.

## Explicit full LoCoMo comparison

`eval/locomo_full.py` extends the frozen small-run settings to every conversation, session and
question in `eval/data/locomo10.json`. It compares the new strategy with local Mem0 OSS 2.2.1 from fresh
stores. It must only be run when the user explicitly requests the full paid benchmark:

```sh
.venv/bin/python eval/locomo_full.py --root eval/results/my-locomo-full-run --stage prepare
.venv/bin/python eval/locomo_full.py --root eval/results/my-locomo-full-run --stage run
```

The frozen protocol contains 10 conversations, 272 sessions, 5,882 source utterances and 1,696
eight-message batches per arm across both speaker perspectives. Standard accuracy uses all 1,540
category 1–4 questions. All 446 category 5 questions are also answered, but their semantic abstention
score is reported separately: the dataset's `adversarial_answer` is a distractor, not a gold answer.
This extra score uses a declared unavailable-information reference and is excluded from standard J/F1.

Formation uses equal conversation concurrency in sequential arm phases; each perspective's batches
remain chronological. Retrieval is isolated from formation and runs sequentially. Report both summed
synchronous batch latency and actual phase wall time; concurrent batch sums are not elapsed wall time.
The three retrieval passes retain first-pass and warm latency separately. Binary/source/runtime hashes,
all provider bodies without credentials, complete corpora, input receipts and batch checkpoints remain
in ignored local results. Resuming refuses changed conditions or an ambiguous SDK commit. The final
report requires full batch, QA, answer fingerprint, corpus immutability and context stability audits.

The frozen source manifest includes `eval/fair_replay.py`, which supplies shared whole-record context packing. Changing it makes resume fail; use a fresh root rather than replacing an existing protocol or source manifest.

Additive retries regenerate from the original input with the concrete validation error, rather than
echoing malformed JSON as an assistant message. Raw provider replies remain durable before validation;
numeric linked IDs are rejected, never silently coerced. The legacy strategy retains its repair exchange.

The active algorithm is named `memos`. Archived result directories, protocol arm labels and frozen control binaries named `current` retain their original provenance; readers accept that historical health label. New demo launches use `MEMORY_STRATEGY=memos`.
