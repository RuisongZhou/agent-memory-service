# Historical conv-26 pilot

Measured on 2026-10-09 on a local macOS arm64 machine with 18 GiB RAM. The pilot processed all 19 sessions and 152 category 1–4 questions. Every setting uses the same question cohort. These results precede the packaging/refactor review; no paid accuracy rerun or remaining nine-conversation run was performed during publication preparation.

| Setting | J % | F1 % | BLEU-1 % | Context tokens | Search P95 ms |
|---|---:|---:|---:|---:|---:|
| English main | 70.39 | 37.00 | 32.08 | 722.6 | 567.8 |
| English buffer | 71.71 | 38.00 | 32.94 | 1139.6 | 593.2 |
| Chinese main | 38.82 | 11.47 | 9.34 | 565.9 | 601.5 |
| Chinese buffer | 51.97 | 20.65 | 16.51 | 976.3 | 622.3 |
| mem0 unified | 83.55 | 54.37 | 46.83 | 3491.0 | 25.4 |
| mem0 original answer prompt | 83.55 | 53.65 | 45.68 | 3491.0 | 25.4 |

See [the aggregate JSON](conv26.json) for per-category metrics and the fixed protocol. Total conv-26 generation usage corresponds to 6.0703 CNY at the official per-token CNY rates verified on that date. Raw account balance, local conversation traces, absolute machine paths and request logs are not public artifacts.

Fixed-workload model performance with eight CPU threads, one warm-up and three rounds:

| Workload | CPU | Metal | Speedup |
|---|---:|---:|---:|
| BGE-M3, 32 inputs | 12.7 inputs/s | 55.2 inputs/s | 4.35x |
| BGE reranker, 30 pairs | 2.272 s | 0.637 s | 3.57x |

Token IDs matched; embedding minimum cosine against Torch was about 0.9999999999987; maximum rerank difference was about 2.086e-7. Empty and long inputs passed additional checks. These are local fixed workloads, not an end-to-end QA speedup or cloud CPU benchmark.

Source-span validation discarded 147 of 610 English candidates and 167 of 290 Chinese candidates; over-80-character spans accounted for 142 and 112 respectively. A school-event temporal fact had a 107-character source span and was rejected, leaving the corresponding main answer without its date. Thresholds were not tuned to improve scores.

## 2026-10-11: integrity changes and fixed 32-question replay

Implemented explicit instructions, durable unresolved conflicts with explicit confirmation, application-built derivation and atomic transitive invalidation of current profiles/skills, and bounded metadata/tokenization caches. The [implementation plan](../architecture.md#integrity-and-retrieval-improvements) describes the responsibility boundaries, compatibility and validation. Historical experiences keep their event meaning; current views become stale when their inputs change. The fact/experience scheduling and strict evidence contract are retained.

The subset was fixed before implementation: first eight original-index questions in each category 1–4 of conv-26, 32 unique questions. The preserved pre-change binary and final binary used copies of the same historical primary-store snapshot, the same live BGE-M3/BGE-reranker-v2-m3 Q8_0 llama-server processes, sequential queries, topK=10, tokenBudget=2000, unchanged ranking parameters and the original reference date. Each received one first pass and two warm passes. No timing outliers were removed. Fast-profile compilation and offline checks finished before the final timing run.

| Setting, same 32 question IDs | J % | F1 % | Mean context tokens |
|---|---:|---:|---:|
| Historical service buffer reference | 81.25 | 39.15 | 1350.6 |
| Historical Mem0 OSS reference | 93.75 | 39.27 | 3176.8 |
| Current binary, retained snapshot | 71.88 | 42.40 | 1392.7 |
| Improved binary, retained snapshot | 71.88 | 42.40 | 1392.7 |

Current and improved returned byte-identical answering contexts for all 32 questions and across all passes. Their answers/judgements are therefore shared only through exact complete-prompt fingerprints. Historical answers were reused where the prompt matched; 31 changed current contexts required 62 new answer/judge calls, with zero failures. Retrieval itself performed zero generation calls.

| Live timing | Current ms | Improved ms |
|---|---:|---:|
| First-pass mean | 290.9 | 279.6 |
| First-pass P95 | 320.9 | 309.7 |
| Warm mean, 64 requests | 376.8 | 252.8 |
| Warm median | 267.3 | 250.7 |
| Warm P95 | 300.3 | 285.5 |
| Warm metadata/parsing stage mean | 16.53 | 1.00 |

Warm mean fell 32.9%, median 6.2%, and P95 4.9%. The average benefit is dominated by fewer long-tail rerank-stage stalls (two requests over one second before, zero after) plus avoiding unchanged metadata scans. First-pass timing remains variable; the change does not establish a universal cold-query speedup. The original exploratory and confirmation runs are retained alongside the frozen final run rather than having their outliers filtered away.

Historical Mem0 search P95 for these 32 questions was 50.0 ms. That is a historical dense-search measurement, not a fresh timing control with the current runtime. Its larger context and higher J remain a useful quality reference; the service has not caught up on J in this subset. The older service reference used a different runtime and an unpurged buffer. Neither historical quality row is an identical-corpus current-versus-improved ablation.

This experiment reuses formed memories from a post-retention snapshot with 53 source turns remaining. It does not rerun formation, does not constitute a physically cleared-buffer main result, and does not establish end-to-end write/formation speed or full-LoCoMo quality. Instruction handling, conflict resolution, stale-view propagation, rollback, restart repair and historical lineage are checked separately with mock integration tests.

Raw evidence is local and ignored under `results/memory-integrity-final-20261011/`: immutable selection/protocol and source/binary fingerprints, complete per-query responses, corpus audits, contexts, answers and `comparison.json`. Both primary-corpus hashes stayed equal and unchanged during measurement. Weights, keys, databases and raw traces are not publication artifacts.

Validation: 56 pure Rust tests, 97 Python tests, `MEMORY_MOCK_MODELS=1 agent-memoryd selftest`, and HTTP/MCP smoke passed. Builds used `--profile fast` throughout; the final incremental builds took approximately 4–6 seconds. The existing LLM-client deprecation warning remains unrelated to this change.

## 2026-10-11: 32-question error audit and fair replay

The new evidence audit and recovery experiment is documented in [conv26-error-analysis.md](conv26-error-analysis.md). Under a shared 2,000-token cap with no recent turns, the frozen development cohort scores 26/32 before, 27/32 with retrieval/context changes, 29/32 after failed-batch recovery, and 30/32 for refreshed Mem0 retrieval. This is a separate protocol from the earlier retained-buffer comparison. The final local raw report is `results/qa-fair-final-20261011/comparison.md`; all generated traces remain ignored.

### Fresh Mem0 strategy comparison (2026-10-11)

See the ignored local report `results/mem0-migration-20261011/comparison.md` and frozen source protocol.
The first four sessions provide 76 utterances and 35 supported questions. Every arm is freshly formed;
current uses its native paired-turn pipeline and new/SDK both speaker perspectives. Shared 2,000-token
cap, Qwen/model/output settings, answer/judge prompts and exact judge cache are preserved.

Current scored 35/35 (F1 58.74%), new 33/35 (60.72%), SDK 33/35 (57.40%). Save wall was 149.4/90.4/80.8 s;
final warm search mean/P95 was 364.1/414.4, 11.9/12.5, 13.6/16.9 ms respectively. Primary records stayed
unchanged. The new flow materially improves latency, with two quality failures still present. This small
cohort and nearly full-corpus SDK contexts do not establish selective retrieval or generalization.
Model calls (17/24/24), output tokens, cache state, native work differences and raw evidence remain local.
