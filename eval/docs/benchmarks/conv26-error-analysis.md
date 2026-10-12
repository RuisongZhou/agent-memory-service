# Conv-26 evidence-led repair

See the local benchmark report at `eval/results/qa-fair-final-20261011/comparison.md` and the 32-question source audit at `eval/results/qa-diagnosis-20261011/analysis.md`. These generated files stay untracked.

This fixed development cohort shares a 2,000-token context cap, source corpus and HTTP embedding runtime, excludes recent turns and shares exact judge decisions. Baseline and retrieval-only replay the same primary records. The repair arm replays all 19 historically rejected batches using the final facts-object-v2 contract; it is a recovery experiment on historical formation, not fresh end-to-end ingestion.

| Arm | J | F1 | Mean context tokens | Warm mean / P95 ms |
|---|---:|---:|---:|---:|
| Before | 26/32 | 44.72% | 959.1 | 268.2 / 322.8 |
| Retrieval/context | 27/32 | 46.54% | 1537.1 | 380.0 / 441.1 |
| Retrieval plus failed-batch recovery | 29/32 | 46.57% | 732.3 | 384.1 / 437.5 |
| Mem0 OSS 2.2.1 | 30/32 | 53.46% | 1987.2 | 18.4 / 23.0 |

21 recovery requests admitted 103 of 103 final candidates; two invalid responses were persisted and repaired. Exact provenance and calendar checks now participate in bounded model repair. Search uses quoted support, preserves semantically recalled candidates, reserves facts/evidence before summaries and distinguishes source dates from event dates. The numeric retrieval defaults were not tuned. Two invalidated profiles remain stale rather than entering the repaired contexts.

The remaining additional error is cross-event attribution; both systems also fail activity-list coverage and an ambiguous historical summer-plan question. Source completeness, typed event attribution and evidence-qualified index text remain follow-up work. Results do not establish held-out generalization, fresh formation superiority or lower latency. Mem0 uses the historical local environment without spaCy/fastembed, with vector search refreshed to the shared runtime.
