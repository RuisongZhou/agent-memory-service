# Evaluation protocol

Protocol of the current evaluation entry points (see [the evaluation README](../README.md)). The earlier
dual-strategy (Memos) protocol, its clients and replay tools remain on the
[`codex/mem0-memos`](https://github.com/RuisongZhou/agent-memory-service/tree/codex/mem0-memos) branch; frozen
protocols and results under `eval/results/` are preserved unchanged.

The evaluation runs locally using Python runners, the Rust daemon, mem0 OSS 2.2.1 and an official Qwen
endpoint. LoCoMo supplies data; there is no hosted benchmark platform in this workflow. Sessions are processed
in chronological order; session dates are interpreted as UTC. Categories 1–4 contribute 1540 questions across
ten conversations; category 5 is excluded from standard accuracy.

## Metrics

J is the LLM-judge label accuracy using mem0's ACCURACY_PROMPT. F1 and BLEU-1 follow copied mem0 metric definitions and are tested for equivalence. Judge failures preserve already-paid answers; resumes verify the context fingerprint. Invalid labels never count as correct answers. `--judge-cache` reuses only exactly matching judge prompts and model settings across arms.

Usage records include returned malformed replies and retries. The service exports persistent usage with stable IDs; the Python ledger de-duplicates them. Calls and tokens are recorded without pricing or monetary budgets.

Historical 2023 data is written at actual 2026 wall-clock time. Preserve this limitation when interpreting results. mem0 OSS uses dense Qdrant in this environment; optional BM25/full spaCy components were absent. The pilot does not reproduce the Mem0 cloud platform paper.

The `.env` generation settings are shared with the daemon (`QWEN_API_KEY`, `QWEN_BASE_URL`, `LLM_MODEL`); `EVAL_MODEL` optionally overrides the evaluation model.

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

Offline checks: `make test-workspace build`. All compilation uses the Fast profile; these
checks never call a paid provider or run the dataset.

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

The frozen source manifest covers the crates, `eval/common` and the entry points the run uses. Changing them makes resume fail; use a fresh root rather than replacing an existing protocol or source manifest.

Additive retries regenerate from the original input with the concrete validation error, rather than
echoing malformed JSON as an assistant message. Raw provider replies remain durable before validation;
numeric linked IDs are rejected, never silently coerced.
