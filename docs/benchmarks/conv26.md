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
