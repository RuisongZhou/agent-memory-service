# Evaluation protocol

The evaluation runs locally using Python runners, the Rust daemon, mem0 OSS 2.2.1 and an official Qwen endpoint. LoCoMo supplies data; there is no hosted benchmark platform in this workflow.

## Fixed mapping

One conversation is a project/tenant. Sessions are processed in chronological order. Adjacent utterances form one user/assistant carrier turn; odd final utterances receive an empty assistant. Image captions are appended as text. Session dates are interpreted as UTC, with one-second offsets per utterance. Categories 1–4 contribute 1540 questions across ten conversations; category 5 is excluded.

The adapted English prompt treats both people as first-hand sources and preserves life events. The original Chinese prompt retains agent-role and long-term-value assumptions. Their P3 limits differ: 350 English words versus 500 Unicode characters. Both retain verbatim source spans of at most 1024 Unicode characters (including spaces and punctuation). This comparison includes semantics and limits, not only language.

## Pipeline

Current conversation formation combines P1/P2 with `formation-values-v1`, whose fact rows use the same nine positions as explicit-write `fact-values-v1`; see [api.md](api.md). The daemon assembles object schemas and preserves raw model values in extraction logs. Experiences and the running summary are processed at each batch trigger, rather than only at session end. Historical runs retain their original results and prompt hashes; a protocol change requires a fresh result/store directory.

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
.venv/bin/python scripts/serve.py --data-dir results/pilot/service-store --device metal --max-calls 512 --llm-concurrency 4
.venv/bin/python eval/memory_run.py --base-url http://127.0.0.1:8765 --tag memory-en --prompt-variant en --convs conv-26 --results-dir results/pilot
.venv/bin/python eval/memory_run.py --base-url http://127.0.0.1:8765 --tag memory-zh --prompt-variant zh --convs conv-26 --results-dir results/pilot
.venv/bin/python eval/mem0_run.py --embed-url http://127.0.0.1:8765/v1 --tag mem0 --convs conv-26 --workers 2 --results-dir results/pilot --stores-dir results/pilot/mem0-stores
.venv/bin/python eval/answer_and_judge.py --contexts results/pilot/memory-en/main/contexts.jsonl --results-dir results/pilot
.venv/bin/python eval/report.py --results-dir results/pilot
```

These are paid commands, not CI checks. Run `--stage` separately when inspecting extraction before retrieval. All settings need answering/judging before comparison. Use a fresh store and run tag for incompatible changes. `memory_run.py` routes en/zh tenants by suffix; a tag alone does not isolate the daemon store.

## Metrics and recovery

J is the LLM-judge label accuracy using mem0's ACCURACY_PROMPT. F1 and BLEU-1 follow copied mem0 metric definitions and are tested for equivalence. Judge failures preserve already-paid answers; resumes verify the context fingerprint. Invalid labels never count as correct answers.

Usage records include returned malformed replies and retries. The service exports tenant-persistent usage with stable IDs; the Python ledger de-duplicates them. Calls and tokens are recorded without pricing or monetary budgets.

Historical 2023 data is written at actual 2026 wall-clock time. Existing recency rules clamp future last-access times to zero age. Preserve this limitation when interpreting results. mem0 OSS uses dense Qdrant in this environment; optional BM25/full spaCy components were absent. The pilot does not reproduce the Mem0 cloud platform paper.

The `.env` generation settings are shared with the daemon (`QWEN_API_KEY`, `QWEN_BASE_URL`, `LLM_MODEL`); `EVAL_MODEL` optionally overrides the evaluation model. Existing DeepSeek benchmark records remain historical evidence.
