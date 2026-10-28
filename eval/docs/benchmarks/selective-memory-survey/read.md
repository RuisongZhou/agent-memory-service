<!-- Survey record behind the [selective-memory report](../selective-memory.md), compiled 2026-10-29 by research agents from primary sources (arXiv metadata and HTML, official repositories); every arXiv id and repository was opened. Kept as written. -->

# Read-time selective retrieval: survey for agent-memory-service

Scope: work first released 2024-10 to 2026-10 (older work marked **background**). Compiled 2026-10-29.

How things were verified:
- **arXiv IDs.** Every ID below was resolved through the arXiv export API (`export.arxiv.org/api/query?id_list=…`), which returned title, first-version date, comment and abstract. Except where a row says "abstract only", the full paper was also read from `arxiv.org/html/<id>`.
- **Code.** Repos were checked with `gh api repos/<owner>/<repo>` on 2026-10-29, which gave the license, last push date and star count. "Maintained" means a push in the last ~3 months.
- **Claims.** Statements I inferred myself (not stated in a source) are marked *(our inference)*.

## Summary

| # | Name | arXiv | First | Venue | Landable | Labelled calibration |
|---|------|-------|-------|-------|----------|----------------------|
| 1 | Magnitude Mirage (score-distribution QPP) | 2609.15578 | 2026-09 | EMNLP 2026 | **yes** | small |
| 2 | Per-query gating of LLM rerankers | 2609.22880 | 2026-09 | – | partial | large |
| 3 | RPP/GPP: predicting retrieval utility (Tian et al.) | 2601.14546 | 2026-01 | ECIR 2026 | partial | small–large |
| 4 | Adaptive-k (largest similarity gap) | 2506.08479 | 2025-06 | EMNLP 2025 | **yes** (truncation only) | none |
| 5 | Surprise: EVT result-list truncation (background) | 2010.09797 | 2020-10 | – | partial | none (α only) |
| 6 | RLT for LLM re-ranking (Meng et al., background) | 2404.18185 | 2024-04 | SIGIR 2024 | partial (as a negative result) | large / none |
| 7 | Conformal context engineering | 2511.17908 | 2025-11 | ECIR 2026 | **yes** | small (positives only) |
| 8 | CONFLARE (background) | 2404.04287 | 2024-04 | – | yes | small |
| 9 | Qwen3 Embedding / Qwen3-Reranker report | 2506.05176 | 2025-06 | – | yes (score semantics) | small (per reranker) |
| 10 | Sufficient Context | 2411.06037 | 2024-11 | ICLR 2025 (per brief) | no | small–large |
| 11 | Adaptive retrieval vs uncertainty (AdaRAGUE) | 2501.12835 | 2025-01 | – | no | – |
| 12 | SimpleMem | 2601.02553 | 2026-01 | – | partial | none |
| 13 | MemoryOS | 2506.06326 | 2025-05 | – | partial | none |
| 14 | Zep / Graphiti | 2501.13956 | 2025-01 | – | partial | none |
| 15 | MIRIX | 2507.07957 | 2025-07 | – | no | none |
| 16 | Hindsight | 2512.12818 | 2025-12 | – | partial | none |
| 17 | A-MEM | 2502.12110 | 2025-02 | NeurIPS 2025 | n/a (no gate) | none |
| 18 | Mem0 | 2504.19413 | 2025-04 | – | n/a (no gate) | none |
| 19 | LongMemEval (evaluation handle) | 2410.10813 | 2024-10 | ICLR 2025 | yes (as test data) | provides labels |

LLM-based gates are background only and are listed in a compact table after row 19.

---

## A. Does this prompt need memory at all? (query-level gate / QPP)

### 1. Magnitude Mirage
- **Title:** The Magnitude Mirage: Rethinking Confidence for Reasoning-Intensive Retrieval.
- **First arXiv:** 2026-09. **Venue:** EMNLP 2026 (from the arXiv comment).
- **arXiv:** [2609.15578](https://arxiv.org/abs/2609.15578). Verified, full HTML read.
- **Code:** https://github.com/JamieHoldcroft/the-magnitude-mirage. Verified: no license file, last push 2026-10-19, 2 stars. This is new research code, not a library.
- **Mechanism:** Abstention is treated as post-retrieval query performance prediction over the query's own sorted top-k scores s₁ ≥ … ≥ s_k. Six zero-cost statistics are compared:
  - MaxScore = s₁
  - Gap@k = s₁ − s_k
  - Std@k = σ(S_k)
  - NQC@k = σ(S_k) / ((1/k)·Σ|sᵢ|)
  - σ_max@k = max_{i∈[2,k]} σ(S_i)
  - LSMV@k = s₁·σ(S_k)

  The system abstains when the statistic falls below a threshold. The headline cutoff is k = 25, and the ranking of the metrics is stable for k ∈ {5, 10, 25, 50}.
- **Extra read-path cost:** none. It is arithmetic over scores that are already computed.
- **Reported results:** 11 retrievers × 28 datasets (BEIR, BRIGHT, TEMPO). The metric is AUROC for predicting retrieval success (NDCG@k > 0).
  - MaxScore is adequate on BEIR for BGE (.749) and SFR (.754). It is weaker for reasoning encoders (ReasonIR .621, RaDeR .666) and collapses to near-random on TEMPO.
  - Switching to any variance-based metric gains .048 (SFR) to .158 (Instructor-Large, .583 → .741) on BEIR. The spread among the variance metrics is at most .017.
  - Gap gains up to +.125 on TEMPO. LSMV is best on most BEIR configurations, and Gap is stronger on several BRIGHT and TEMPO ones. For BM25 on TEMPO, NQC beats Gap (.602 vs .526).
  - For answer correctness, all 11 retrievers have Gap@25 AUROC ≥ MaxScore AUROC.
- **Landable:** **yes.** It is a few lines of Rust over the existing dense pool (≥ 60 cosines) and replaces the absolute 0.1 floor as the "is anything relevant?" signal.
- **Calibration:** **small.** AUROC is threshold-free, but a deployed τ is not.
  - Gap and LSMV are in cosine units: Gap is shift-invariant but not scale-invariant. NQC is scale-invariant but not shift-invariant. So τ has to be set per embedding model.
  - Caveat: "success" here means the retriever found a judged-relevant document in a corpus where one exists. The case "no relevant memory exists" is not tested.

### 2. Per-query gating of LLM rerankers
- **Title:** Per-Query Gating of LLM Rerankers for Multi-Hop Retrieval.
- **First arXiv:** 2026-09. **Venue:** none stated.
- **arXiv:** [2609.22880](https://arxiv.org/abs/2609.22880). Verified, HTML read.
- **Code:** no GitHub repo. The arXiv comment says the evaluation code and per-query results are in the arXiv source archive. **none**
- **Mechanism:** A learned per-query gate decides whether to call the reranker, using only features available before that call: 27 score and lexical statistics of the two retrieval lists, plus a PCA of a small query embedding. When the reranker is skipped, an executable fallback ranking is served instead. The threshold and the fallback are chosen inside the training fold.
  - v2 adds a loss-budget rule: skip only while the expected harmful-skip rate stays under a budget, using Platt-calibrated harm probabilities.
- **Extra cost:** feature computation plus a tiny classifier. It saves the rerank call whenever it skips.
- **Reported results:**
  - The setting: LLM rerankers cost about $0.2–0.3 per 1,000 queries and roughly 1 s of tail latency. They improve last-hop top-K coverage in 7 of 9 (dataset, K) cells, by up to +34.8 pp.
  - Across nine cells (2WikiMultiHopQA, MuSiQue, HotpotQA), the gate skips **51%** of reranker calls at an average held-out cost of **1.2 pp** LastHop@K. Four cells meet a pre-registered 1 pp rule.
  - Harmful skips occur in 8 cells: **190 harmful vs 136 beneficial**.
  - A random gate at the same skip rate loses 2–11 pp on the high-lift cells.
- **Landable:** **partial.**
  - Portable: the feature recipe (score statistics of the lists) and the idea of skipping the expensive call.
  - Blocking: the gate is learned, and it was evaluated with an LLM reranker on multi-hop QA. With a cross-encoder the savings per skip are smaller.
- **Calibration:** **large.** It needs per-query labels of whether reranking helped (a training fold) plus Platt calibration.

### 3. Predicting retrieval utility and answer quality (Tian, Ganguly, Macdonald)
- **Title:** Predicting Retrieval Utility and Answer Quality in Retrieval-Augmented Generation.
- **First arXiv:** 2026-01. **Venue:** ECIR 2026.
- **arXiv:** [2601.14546](https://arxiv.org/abs/2601.14546). Verified, HTML read.
- **Code:** **none** linked in the paper.
- **Mechanism:** Defines two tasks:
  - RPP: the utility of the retrieved documents, measured as the gain from using the context versus answering without it.
  - GPP: the quality of the final answer.

  It adapts pre- and post-retrieval QPP predictors, adds a reader-side feature (PerpA: the LLM's perplexity of the context given the query), and combines the predictors by linear regression.
- **Extra cost:** the QPP part is free. PerpA needs an LLM forward pass.
- **Reported results:** A linear-regression ensemble of QPP predictors beats the best single predictor, especially for BM25 and E5. For GPP with E5, PerpA+QPP reaches a correlation of 0.3915, versus 0.2873 for PerpA alone and 0.2952 for QPP alone (significant under Fisher's z-test).
- **Landable:** **partial.** Only the LLM-free QPP ensemble half fits. Correlations around 0.3–0.4 make it a weak per-query gate on its own.
- **Calibration:** small to large, because the regression needs labelled utility.

A related analysis, also verified: *Am I on the Right Track? What Can Predicted Query Performance Tell Us about the Search Behaviour of Agentic RAG* ([2507.10411](https://arxiv.org/abs/2507.10411), 2025-07, abstract plus HTML). It finds that QPP estimates of the queries an agent issues correlate positively with final answer quality. *(our inference)* That supports returning the gate statistic alongside results from the agent-invoked search tool.

---

## B. Which candidates to return? (truncation, calibration, abstention)

### 4. Adaptive-k
- **Title:** Efficient Context Selection for Long-Context QA: No Tuning, No Iteration, Just Adaptive-k.
- **First arXiv:** 2025-06. **Venue:** EMNLP 2025 Main.
- **arXiv:** [2506.08479](https://arxiv.org/abs/2506.08479). Verified, Algorithm 1 read.
- **Code:** https://github.com/megagonlabs/adaptive-k-retrieval. Verified: BSD-3-Clause, last push 2025-09-15, 13 stars. Not actively maintained.
- **Mechanism (Algorithm 1):**
  1. Sort the cosine similarities s in descending order.
  2. Compute the gaps gᵢ = sᵢ − sᵢ₊₁.
  3. Take k = argmax g, the position of the steepest drop.
  4. Return the top k plus a fixed buffer **B = 5**.

  The gap search is restricted to the **top 90%** of sorted documents, so that a large gap deep in the irrelevant tail cannot select a huge k.
- **Extra cost:** an O(n log n) sort. No model calls.
- **Reported results:** On factoid and aggregation QA it matches or beats fixed-k while using up to **10× fewer tokens than full context**, and it still retrieves **70% of relevant passages**. Accuracy improves across five long-context LLMs and two embedders. The setting is 1,000-document (HELMET) or 100k-token (HoloBench) contexts, with chunks of about 40–100 tokens.
- **Landable:** **yes, as truncation only.** It always returns at least 1 item (plus B), so it is **not an abstention rule**. It was designed for thousands of candidates, so with ≤ 60 candidates the largest gap is noisier *(our inference)*.
- **Calibration:** **none.** B and the 90% window are fixed constants. argmax(gap) is invariant to positive affine rescaling of the scores but not to a sigmoid. Run it consistently on either logits or probabilities.

### 5. Surprise (background)
- **Title:** Surprise: Result List Truncation via Extreme Value Theory (Bahri et al.).
- **First arXiv:** 2020-10. **Venue:** none in the arXiv metadata.
- **arXiv:** [2010.09797](https://arxiv.org/abs/2010.09797). Verified via the API. **Abstract only**: the ar5iv HTML timed out.
- **Code:** **none** found (no link in the abstract).
- **Mechanism (from the abstract):** At query time, it fits a Generalized Pareto distribution (the peaks-over-threshold law from extreme value theory) to the query's own ranked scores. Each score becomes an interpretable, calibrated "surprise" score with a hypothesis-test / p-value reading, and the list is truncated where results stop being surprising. The exact tail selection and decision threshold were not verified.
- **Extra cost:** one small per-query distribution fit over ≤ 60–100 scores.
- **Reported results:** The abstract claims effectiveness on result-list truncation across image, text and IR datasets against classical and recent baselines. No numbers were read.
- **Landable:** **partial.** It is query-relative and label-free, needing only α. That makes it the best theoretical fit for "survives swapping the embedder or reranker". But a tail fit on about 60 points is fragile, and the details are unverified. Prototype it before relying on it.
- **Calibration:** **none** (choose α).

### 6. Ranked-list truncation for LLM re-ranking (Meng et al., background)
- **Title:** Ranked List Truncation for Large Language Model-based Re-Ranking.
- **First arXiv:** 2024-04. **Venue:** SIGIR 2024 (long paper).
- **arXiv:** [2404.18185](https://arxiv.org/abs/2404.18185). Verified, HTML read.
- **Code:** https://github.com/ChuanMeng/RLT4Reranking. Verified: no license, last push 2024-11-22, 51 stars. Unmaintained.
- **Mechanism:** Reproduces 8 ranked-list-truncation methods for choosing, per query, how many first-stage candidates to send to the re-ranker. The methods include Fixed-k, Choppy, AttnCut, MtCut (MMoECut) and LeCut, plus an oracle. The test bed is TREC-DL 19/20 with BM25, SPLADE++ and RepLLaMA retrievers, and RankLLaMA among the re-rankers.
- **Extra cost:** the supervised methods run a small transformer or LSTM over list features.
- **Reported results:**
  - Oracle per-query cut-offs are significantly better than every fixed cut-off (paired t-test, p < 0.05), so the headroom is real.
  - But in the RankLLaMA pipelines, **Fixed-k(20) consistently gives the best effectiveness/efficiency trade-off** against the supervised methods. Their gains are not significant: for example, MtCut beats Fixed-k(20) by only +0.006 nDCG@10.
  - Findings on truncation for retrieval do not generalize to retrieve-then-re-rank.
- **Landable:** **partial, as a negative result.** Keep a fixed rerank candidate count, and do not build learned pre-rerank truncation.
- **Calibration:** large for the supervised methods, none for Fixed-k.

### 7. Conformal context engineering
- **Title:** Principled Context Engineering for RAG: Statistical Guarantees via Conformal Prediction.
- **First arXiv:** 2025-11. **Venue:** ECIR 2026.
- **arXiv:** [2511.17908](https://arxiv.org/abs/2511.17908). Verified, HTML read.
- **Code:** https://github.com/hltcoe/conformal-context-engineering. Verified: MIT, last push 2026-03-16, 4 stars.
- **Mechanism:** Uses a nonconformity score A(q,s), where lower means more relevant (for example the negative cosine for the embedding scorer). Given a labelled calibration set D_cal and a miscoverage α:

  τ̂_α = Quantile_{1−α}( { A(q,s) : (q,s) ∈ D_cal, r(q,s) = 1 } )

  A snippet is kept iff A(q,s) ≤ τ̂_α, which retains about (1−α) of truly relevant snippets (marginal coverage). The paper tests both embedding-based and LLM-based scoring functions.
- **Extra cost:** none at read time, since the threshold is a constant. Calibration runs offline.
- **Reported results:** It meets the target coverage on NeuCLIR and RAGTIME and cuts retained context by **2–3×** compared with unfiltered retrieval. On NeuCLIR, ARGUE F1 improves under strict filtering and stays stable at moderate coverage.
- **Landable:** **yes.** Store one τ̂ per (scorer, version) and recompute it when bge or Qwen is swapped in. This is the explicit calibration step the constraints ask for.
- **Calibration:** **small.** Only *relevant* pairs are needed for the coverage guarantee. It does **not** bound false injections when nothing is relevant.

### 8. CONFLARE (background)
- **Title:** CONFLARE: CONFormal LArge language model REtrieval.
- **First arXiv:** 2024-04. **Venue:** report.
- **arXiv:** [2404.04287](https://arxiv.org/abs/2404.04287). Verified, abstract only.
- **Code:** https://github.com/Mayo-Radiology-Informatics-Lab/conflare. Verified: MIT, last push 2024-04-19. Unmaintained.
- **Mechanism (abstract):**
  1. Build a calibration set of questions that are answerable from the knowledge base.
  2. Record the similarity scores of the chunks that contain each answer.
  3. For a user-specified error rate α, derive a similarity cutoff, and retrieve every chunk above it.

  This is the same split-conformal idea that row 7 evaluates at scale.
- **Extra cost:** none at read time.
- **Reported results:** none read.
- **Landable:** yes (as row 7). **Calibration:** small.

### 9. Qwen3 Embedding / Qwen3-Reranker report
- **Title:** Qwen3 Embedding: Advancing Text Embedding and Reranking Through Foundation Models.
- **First arXiv:** 2025-06. **Venue:** none.
- **arXiv:** [2506.05176](https://arxiv.org/abs/2506.05176). Verified, HTML read.
- **Code:** https://github.com/QwenLM/Qwen3-Embedding. Verified: the repo has no detected license (the abstract says the models are Apache-2.0), last push 2025-09-30, 2,056 stars.
- **Mechanism:** The reranker is an instruction-conditioned yes/no judge. Its prompt says the answer can only be "yes" or "no", and the score is

  score(q,d) = e^{P(yes|I,q,d)} / (e^{P(yes|I,q,d)} + e^{P(no|I,q,d)})

  That is a two-way softmax over the yes/no outputs, so the score lies in (0, 1).

  For contrast, the bge-reranker-v2-m3 model card (fetched) says its raw logit "can be mapped to a float value in [0,1] by sigmoid function" (`normalize=True`).
- **Extra cost:** one rerank call, which is already in the budget.
- **Reported results:** state of the art on multilingual MTEB and on retrieval tasks (abstract). The report has no calibration study.
- **Landable:** **yes, as score semantics.** Both scores land in [0,1], but they are **not on a shared calibrated scale**, so a τ tuned on bge does not transfer to Qwen. Calibrate per reranker (row 7, or Platt scaling). The relevance-score semantics of the hosted Qwen text-rerank API were **not verified**.
- **Calibration:** small, per reranker.

### 10. Sufficient Context
- **Title:** Sufficient Context: A New Lens on Retrieval Augmented Generation Systems.
- **First arXiv:** 2024-11. **Venue:** ICLR 2025 according to the brief; the arXiv metadata lists no venue.
- **arXiv:** [2411.06037](https://arxiv.org/abs/2411.06037). Verified, HTML read.
- **Code:** https://github.com/hljoren/sufficientcontext. Verified: no license, last push 2025-07-22, 70 stars. It holds prompts and analysis.
- **Mechanism:** An LLM autorater (Gemini 1.5 Pro 1-shot, and FLAMe later) labels whether the (query, context) pair is sufficient to answer. For selective generation, a smaller "intervention" model combines that sufficiency signal with the answering model's self-rated confidence to decide between answering and abstaining. This gives a tunable coverage/accuracy trade-off. The authors note that abstaining whenever the context is insufficient is too heavy-handed, because models still answer some of those questions correctly.
- **Extra cost:** an LLM autorater call per query, plus confidence elicitation.
- **Reported results:** it raises the fraction of correct answers among responses by **2–10%** for Gemini, GPT and Gemma.
- **Landable:** **no**, because it needs an LLM per query. The lesson for us is evaluative: keep "is a relevant memory present?" separate from "did the agent use it?".
- **Calibration:** small to large, since the intervention model needs labels.

### 11. Adaptive retrieval vs uncertainty estimation (Moskvoretskii et al.)
- **Title:** Adaptive Retrieval Without Self-Knowledge? Bringing Uncertainty Back Home.
- **First arXiv:** 2025-01. **Venue:** none on arXiv.
- **arXiv:** [2501.12835](https://arxiv.org/abs/2501.12835). Verified, HTML read.
- **Code:** https://github.com/s-nlp/AdaRAGUE. Verified: MIT, last push 2025-05-17, 21 stars.
- **Mechanism:** Compares 35 ways of deciding *whether to retrieve* from the answering LLM's own uncertainty: 8 adaptive-retrieval pipelines and 27 uncertainty estimators. The comparison covers 6 QA datasets and 10 metrics (QA quality, self-knowledge, efficiency).
- **Extra cost:** LLM logits or samples before retrieval, often extra forward passes.
- **Reported results:**
  - Established uncertainty methods match the recent adaptive-retrieval pipelines on QA, are more efficient, and show stronger self-knowledge.
  - Under out-of-distribution transfer, downstream performance holds but self-knowledge drops significantly.
- **Landable:** **no.** The signal lives inside the answering model (Claude, Codex), which the hook cannot reach.
- **Calibration:** –.

---

## C. How agent-memory systems gate retrieval in practice

### 12. SimpleMem
- **Title:** SimpleMem: Efficient Lifelong Memory for LLM Agents.
- **First arXiv:** 2026-01. **Venue:** not stated on arXiv.
- **arXiv:** [2601.02553](https://arxiv.org/abs/2601.02553). Verified, HTML read.
- **Code:** https://github.com/aiming-lab/SimpleMem. Verified: MIT, last push 2026-07-24, 3,855 stars. Maintained.
- **Mechanism:** "Intent-Aware Retrieval Planning". A planning step infers the latent search intent and emits rewritten queries for each index (q_sem, q_lex, q_sym) plus an **adaptive depth d** that reflects estimated query complexity. The candidate limit is n ∝ d. Results from the semantic, lexical and symbolic indexes are merged by **ID-based deduplication** rather than weighted score fusion.
- **Extra cost:** one generative planning call per query on the read path.
- **Reported results:** On LoCoMo with GPT-4.1-mini, average F1 is **43.24**, against 34.20 for Mem0 and 18.70 for full context. Usage is about **530–580 tokens per query**, against about 16,900 for full context (roughly 30×). The paper claims +26.4% F1 over Mem0.
- **Landable:** **partial.**
  - Portable: ID-dedup across channels, and "depth from complexity" if we can find a non-LLM proxy for complexity.
  - Not portable: the LLM planner.
- **Calibration:** none.

### 13. MemoryOS
- **Title:** Memory OS of AI Agent.
- **First arXiv:** 2025-05. **Venue:** not stated on arXiv.
- **arXiv:** [2506.06326](https://arxiv.org/abs/2506.06326). Verified, HTML read.
- **Code:** https://github.com/BAI-LAB/MemoryOS. Verified: Apache-2.0, last push 2026-07-07, 1,614 stars. Maintained.
- **Mechanism:**
  - Reads are fixed-size. The system takes the top-m mid-term segments by a matching score, then the top-k pages inside them by semantic similarity. User KB and Assistant Traits each return their **top-10**, and profiles are always injected in full.
  - The thresholds sit on the write side. A page merges into a segment if its similarity exceeds θ, and Heat = α·N_visit + β·L_interaction + γ·R_recency drives eviction and promotion.
- **Extra cost:** embedding similarity on the read path. LLM calls happen at generation and update time.
- **Reported results:** On LoCoMo with GPT-4o-mini, +49.11% F1 and +46.18% BLEU-1 on average over the baselines. A top-k ablation is included.
- **Landable:** **partial.** There is no read-time abstention. The heat score is a maintenance signal, outside this bucket.
- **Calibration:** none.

### 14. Zep / Graphiti
- **Title:** Zep: A Temporal Knowledge Graph Architecture for Agent Memory.
- **First arXiv:** 2025-01. **Venue:** none.
- **arXiv:** [2501.13956](https://arxiv.org/abs/2501.13956). Verified, HTML read.
- **Code:** https://github.com/getzep/graphiti. Verified: Apache-2.0, last push 2026-10-28, 32,305 stars. Maintained.
- **Mechanism:**
  - **Paper:** hybrid search (cosine, BM25, graph BFS) followed by rerankers: RRF, MMR, an episode-mentions reranker, node distance, and a cross-encoder.
  - **Code defaults** (`graphiti_core/search`, read 2026-10-29):
    - `sim_min_score` = `DEFAULT_MIN_SCORE` = **0.6**, an absolute cosine floor.
    - `mmr_lambda` = **0.5**.
    - `reranker_min_score` = **0**.
    - `limit` = 10.
    - Default reranker: RRF.
  - **Graphiti's MMR is one-shot, not greedy:** mmr(d) = λ·⟨q,d⟩ + (λ−1)·max_{d′≠d ∈ candidates} cos(d,d′). Candidates are then sorted and filtered by `min_score` (default −2.0). Because the max runs over *all* other candidates, **both** members of a near-duplicate pair are demoted.
  - `OpenAIRerankerClient` asks an LLM for "True"/"False" with logprobs and scores each candidate by exp(logprob).
- **Extra cost:** O(n²) dot products for MMR. The cross-encoder option is one model call; the OpenAI client is an LLM call.
- **Reported results:** DMR 94.8% vs 93.4% for MemGPT. On LongMemEval, accuracy improves by up to 18.5% with about 90% lower latency than the baseline.
- **Landable:** **partial.**
  - Portable: MMR (better implemented greedily) and a reranker floor once it is calibrated.
  - Not suitable: the 0.6 absolute floor is the pattern to escape, and the logprob reranker breaks the no-LLM rule.
- **Calibration:** none (λ is fixed).

### 15. MIRIX
- **Title:** MIRIX: Multi-Agent Memory System for LLM-Based Agents.
- **First arXiv:** 2025-07. **Venue:** none.
- **arXiv:** [2507.07957](https://arxiv.org/abs/2507.07957). Verified; HTML read via curl after the first urllib attempt failed.
- **Code:** https://github.com/Mirix-AI/MIRIX. Verified: Apache-2.0, last push 2026-10-28, 3,456 stars. Maintained.
- **Mechanism:** "Active Retrieval". Before answering, the agent **generates a current topic** from the input context. That topic is used to retrieve from each of six memory components (Core, Episodic, Semantic, Procedural, Resource, Knowledge Vault), and the results are **injected into the system prompt**. The agent can also call several retrieval tools. The section I read describes no relevance floor.
- **Extra cost:** one LLM generation per turn, plus per-component retrieval.
- **Reported results:** 85.4% on LOCOMO. On ScreenshotVQA, +35% accuracy over a RAG baseline with 99.9% less storage.
- **Landable:** **no**, because topic generation is an LLM call.
- **Calibration:** none.

### 16. Hindsight
- **Title:** Hindsight is 20/20: Building Agent Memory that Retains, Recalls, and Reflects.
- **First arXiv:** 2025-12. **Venue:** none.
- **arXiv:** [2512.12818](https://arxiv.org/abs/2512.12818). Verified, HTML read.
- **Code:** https://github.com/vectorize-io/hindsight. Verified: MIT, last push 2026-10-28, 54,874 stars. Maintained.
- **Mechanism:** `Recall(B, Q, k)` takes **k as a token budget**, plus an optional cost or latency budget that caps how far the search expands. The pipeline:
  1. Four parallel channels: semantic vector, BM25, graph traversal, and temporal filtering.
  2. Reciprocal Rank Fusion of the four lists.
  3. A cross-encoder rerank (`cross-encoder/ms-marco-MiniLM-L-6-v2`, with formatted temporal information included in the text).
  4. Return the most relevant facts whose combined size fits the budget.

  The paper describes no relevance floor or abstention.
- **Extra cost:** one small cross-encoder pass. There is no LLM call on recall.
- **Reported results:** With a 20B open-source backbone, LongMemEval accuracy rises from 39.0% to **83.6%** over full context on the same backbone, and LoCoMo from 75.78% to 85.67%. Larger backbones reach 91.4% and 89.61% respectively.
- **Landable:** **partial.** Token-budget packing ports directly. Hindsight does not address problem (1).
- **Calibration:** none.

### 17. A-MEM
- **Title:** A-MEM: Agentic Memory for LLM Agents.
- **First arXiv:** 2025-02. **Venue:** NeurIPS 2025.
- **arXiv:** [2502.12110](https://arxiv.org/abs/2502.12110). Verified, HTML read.
- **Code:** two verified repos, both maintained:
  - https://github.com/WujiangXu/A-mem-sys (the system): MIT, last push 2026-03-15.
  - https://github.com/WujiangXu/A-mem (evaluation; `WujiangXu/AgenticMemory` redirects here): MIT, last push 2026-10-22.
- **Mechanism:** Plain cosine **top-k** over note embeddings, with k = 10 by default. k is tuned per backbone (Table 8). There is no threshold.
- **Extra cost:** none beyond the search itself.
- **Reported results:** the abstract claims gains across six foundation models. No numbers were extracted.
- **Landable:** n/a, because there is no gate.
- **Calibration:** none.

### 18. Mem0
- **Title:** Mem0: Building Production-Ready AI Agents with Scalable Long-Term Memory.
- **First arXiv:** 2025-04. **Venue:** none.
- **arXiv:** [2504.19413](https://arxiv.org/abs/2504.19413). Verified, HTML read.
- **Code:** https://github.com/mem0ai/mem0. Verified: Apache-2.0, last push 2026-10-26, 67,994 stars. Maintained.
- **Mechanism:** The current `Memory.search` defaults in `mem0/memory/main.py` (main branch, fetched 2026-10-29) are **`top_k=20, threshold=0.1, rerank=False`**. A reranker is optional, via `RerankerFactory`. The file imports helpers named `detect_scale_threshold_from_top_k` and `display_scale_threshold_notice`; I did not inspect them.
- **Extra cost:** none by default.
- **Reported results:** On LOCOMO, +26% relative LLM-as-a-Judge over OpenAI memory. Against full context, 91% lower p95 latency and more than 90% token savings.
- **Landable:** n/a. **The current 0.1 floor in agent-memory-service is exactly Mem0's default.**
- **Calibration:** none.

---

## D. Evaluation handle

### 19. LongMemEval
- **Title:** LongMemEval: Benchmarking Chat Assistants on Long-Term Interactive Memory.
- **First arXiv:** 2024-10. **Venue:** ICLR 2025.
- **arXiv:** [2410.10813](https://arxiv.org/abs/2410.10813). Verified, abstract.
- **Code:** not checked.
- **Mechanism:** 500 questions covering five abilities, one of which is **abstention**: questions whose answer is not in the history.
- **Extra cost:** none (offline).
- **Reported results:** not used here.
- **Landable:** **yes, as calibration and test data.** The abstention items supply ready-made "nothing relevant exists" cases for tuning the thresholds of Rules 1 and 4. Neither the Zep nor the Hindsight HTML mentions abstention.
- **Calibration:** it supplies labels.

## Background LLM-side gates (not landable: each needs the answering LLM or a trained LM on the read path)

| Name | arXiv (verified) | First | Decision signal (from the abstract) | Why not landable |
|------|------------------|-------|-------------------------------------|------------------|
| Self-RAG | 2310.11511 | 2023-10 | A trained LM emits reflection tokens to retrieve on demand and critique passages | Requires a fine-tuned answering LM |
| SKR | 2310.05002 | 2023-10 | The LLM's self-knowledge, judged against previously seen questions, decides whether to call retrieval | Uses the answering LLM |
| DRAGIN | 2403.10081 | 2024-03 | When and what to retrieve, from the LLM's real-time information needs during generation | Token-level LLM internals |
| CRAG | 2401.15884 | 2024-01 | A "lightweight retrieval evaluator" returns a confidence that triggers retrieval actions, plus web search and decompose-then-recompose | Trained evaluator not available via an OpenAI-compatible endpoint; web search on the read path |
| UAR | 2406.12534 | 2024-06 | Four criteria cast as plug-and-play classification tasks for retrieval timing | Trained classifiers tied to an LLM (body not read) |
| SeaKR | 2406.19215 | 2024-06 | Self-aware uncertainty from LLM internal states triggers retrieval and reranks snippets | Needs LLM hidden states |

---

## Decision rules worth porting

No single paper covers gate, truncate and dedupe, so the useful port is a stack. The read path stays at **one query embedding plus at most one rerank call**. Everything else below is O(N log N) arithmetic on ≤ 100 scores, plus ≤ 20² dot products on stored embeddings, which costs microseconds.

### Rule 1: distributional gate on the dense pool (fixes problem 1)
Source: Magnitude Mirage (row 1). Compute this on the **raw dense cosine list**, before BM25/entity fusion: the paper studies single-retriever score lists, and RRF scores are rank-based.

```
k      = min(25, N)                     # N = dense pool size (≥ 60 normally)
Gap@k  = s1 - s_k
NQC@k  = std(s1..s_k) / mean(|s1..s_k|)     # scale-free secondary signal
if Gap@k < tau_gap[embedder]:  inject nothing, skip the reranker (Rule 4)
```

- **Why this one:** in the paper, switching from magnitude (s₁, which is what a cosine floor effectively thresholds) to *any* distributional statistic gained up to +.158 AUROC. The differences between distributional statistics were ≤ .017.
- **Calibration:** one τ per embedding model. Two ways to set it:
  - (a) A small labelled set from your own hook logs: prompts tagged "a relevant memory exists / does not exist", padded with LongMemEval abstention items. Choose τ at a target false-injection rate.
  - (b) Label-free rate control: τ = the q-quantile of Gap@k over recent prompts, which hits a target injection rate but gives no precision guarantee.

  Re-run whenever the embedder changes. A reranker swap does not affect this rule.
- **Optional label-free variant (row 5):** an EVT tail p-value for s₁ relative to the query's own score tail, with only α to choose. The details are unverified, so prototype it offline before trusting it.

### Rule 2: per-reranker calibrated floor (makes thresholds survive bge ↔ Qwen)
Source: conformal context engineering (row 7), with CONFLARE (row 8) as background.

```
A(q,m)        = -rerank_score(q,m)
tau_hat[r,α]  = Quantile_{1-α}( { A(q,m) : (q,m) in D_cal, m relevant to q } )   # per reranker r
keep m  iff  rerank_score(q,m) >= -tau_hat[r,α]
```

- **Guarantee:** about (1−α) of truly relevant memories survive (marginal coverage).
- **Calibration:** **small**, and it needs **positive pairs only**. Store τ̂ keyed by (reranker id, version) and recompute it as an explicit `calibrate` step whenever the reranker changes. The raw scores differ by design: bge is a raw logit (sigmoid when normalised), Qwen3 is a yes/no softmax (row 9).
- **Alternative:** Platt scaling per reranker, p = σ(a·z + b), fitted on positives and negatives, after which one probability floor can be shared across rerankers *(our inference; Platt calibration is what row 2 uses for its harm probabilities)*.
- **Limit:** coverage control does not cap false injections when nothing is relevant. Rule 1 does that job.

### Rule 3: largest-gap truncation after rerank (fixes token waste)
Source: Adaptive-k (row 4).

```
z      = reranked scores of survivors, descending (M items)
g_i    = z_i - z_{i+1}  for i < ceil(0.9*M)
k*     = argmax_i g_i + 1   (count of items before the biggest drop; our reading of the 0-indexed Algorithm 1)
return top k* (+B; paper B=5 for 1000-chunk contexts; use B=0 here), then cap at 5 items / 400 tokens
```

- **Calibration:** none.
- **Scale choice:** use one scale consistently, raw logits for bge or the yes/no probability for Qwen. A sigmoid compresses the top end and can move the argmax.
- **Placement:** it always returns at least 1 item, so it must sit **behind** Rules 1–2.

### Rule 4: skip the rerank call when the gate abstains (fixes latency)
Source: per-query reranker gating (row 2).

- If Rule 1 abstains, return empty without calling the reranker. That saves the largest single read-path cost on every irrelevant prompt.
- Optional upper threshold: if Gap@k ≥ τ_high (one dominant match), skip the rerank and serve the cosine order.
- **Calibration:**
  - τ_gap reuses Rule 1's set.
  - τ_high needs "did the rerank change the kept set?" labels, which can be generated offline by running both paths, with no human judging *(our inference)*.
  - A learned gate in the style of row 2 (score statistics plus query-embedding PCA) needs per-query lift labels.
- **Expected price:** row 2 skipped 51% of calls at −1.2 pp, but harmful skips outnumbered beneficial ones 190:136. A gate costs some recall.

### Rule 5: greedy redundancy suppression, then token packing (fixes tokens)
Source: MMR as used in Graphiti (row 14), token budgets as in Hindsight (row 16), ID-dedup as in SimpleMem (row 12).

```
S = []
while candidates and budget left:
    m* = argmax_{m not in S} [ λ·rel(m) − (1−λ)·max_{m' in S} cos(m, m') ]     # λ = 0.5 (Graphiti default)
    (or hard rule: skip m if max_{m' in S} cos(m, m') ≥ δ)
    append m* if it fits the 400-token budget
```

- **Greedy, not one-shot:** Graphiti's one-shot variant demotes both copies of a duplicate. Use greedy MMR.
- **Inputs:** rel(m) should be the calibrated [0,1] score from Rule 2, so that it mixes sensibly with cosine. Stored embeddings mean no extra model call.
- **Calibration:** none for λ. δ needs a quick per-embedder check on a handful of known near-duplicates.

**Order on the hot path:** embed → dense pool → **Rule 1** (abstain → empty, no rerank) → BM25/entity fusion → rerank → **Rule 2** floor → **Rule 3** cut → **Rule 5** dedupe and pack.

**Calibration burden overall:**
- Rule 1: one small labelled or rate-controlled set per embedder.
- Rule 2: one small positive-pair set per reranker.
- Rules 3 and 5: none.

---

## Rejected and why
- **LLM-side gates** (Self-RAG, SKR, DRAGIN, SeaKR, UAR, CRAG; rows 10 and 11, Sufficient Context and AdaRAGUE): they need the answering model's internals, an LLM autorater, or a trained evaluator on the read path. The hook cannot reach Claude's or Codex's logits.
- **SimpleMem's planner and MIRIX's topic generation:** each adds a generative call per prompt.
- **LightMem**, *Lightweight LLM Agent Memory with Small Language Models* ([2604.07798](https://arxiv.org/abs/2604.07798), ACL 2026; abstract only): small-model-driven online memory operations are still a generative call and would need a new runtime.
- **HUOZIIME** ([2604.14159](https://arxiv.org/abs/2604.14159), 2026-03; abstract only): an on-device IME with a post-trained LLM and hierarchical memory. Any learned retrieval trigger requires training, and the body was not read.
- **Supervised ranked-list truncation** (Choppy, AttnCut, MtCut, LeCut): needs training, and row 6 shows no significant gain over Fixed-k(20) in LLM re-rank pipelines.
- **Learned reranker gate in the style of row 2:** kept only as an optional later step, because it needs lift labels and a training fold.
- **Graphiti's `OpenAIRerankerClient`** (True/False logprobs): an LLM call per candidate.
- **Conformal factuality**, meaning claim-level, post-generation filtering:
  - Conformal-RAG ([2506.20978](https://arxiv.org/abs/2506.20978), SIGIR 2025 short).
  - Agentic RAG conformal factuality ([2610.34872](https://arxiv.org/abs/2610.34872), 2026-10).

  Both operate on generated claims with LLM scoring, after generation.
- **LLM-driven memory read paths, 2026** (abstracts only):
  - RIME ([2609.34438](https://arxiv.org/abs/2609.34438)): self-questions, and falls back to the source dialogue when compressed memory is insufficient.
  - REALM ([2609.16053](https://arxiv.org/abs/2609.16053)): adaptively composed graph-search atoms.
  - QUMem ([2608.16168](https://arxiv.org/abs/2608.16168)): query-conditioned user-state inference.
- **Reader-side predictors** (PerpA in row 3): they need an LLM forward pass, and correlations of about 0.3–0.4 are too weak for a hard gate.
- **Fieldbus auto-configuration's abstention gate** ([2608.08618](https://arxiv.org/abs/2608.08618), 2026-08; abstract only): a reranker-score magnitude threshold judged on a 28-query gold set. The evidence is too thin, and it is the magnitude pattern row 1 warns against.
- **Absolute cosine floors** (Graphiti 0.6, Mem0 0.1): they shift with the embedder, language and query form. Row 1 shows magnitude near-random on temporal queries.

## Caveats and evidence gaps
- **No verified 2025–26 paper tests an LLM-free gate on "no relevant memory exists" in agent or dialogue memory.** Rule 1's evidence is about predicting retrieval failure on IR benchmarks. Validate it on your own hook logs plus LongMemEval abstention items before turning injection off.
- **Adaptive-k was evaluated with thousands of candidates.** On a reranked list of 5–20 items, the largest gap is noisier. Measure the precision of the chosen k offline.
- **Adaptive-k indexing is ambiguous.** Algorithm 1 is 0-indexed (`for i=0 to |s|-2`, `k ← argmax g`), so the returned k is an index. Rule 3's "count of items before the biggest drop" (argmax index + 1) is *our reading* of the pseudocode.
- **No dedicated redundancy paper.** Within budget I found no dedicated 2024-10 to 2026-10 paper on MMR or near-duplicate suppression for RAG or memory context. Rule 5 is assembled from production defaults (Graphiti MMR λ = 0.5, Hindsight token budgets, SimpleMem ID-dedup), not from a paper that measured its token savings.
- **SessionStart has no query.** Rules 1–4 do not apply to the newest-facts injection. Rule 5 (greedy dedupe plus token packing) does, and it is the only rule here that trims that path.
- **Conformal coverage is marginal and assumes exchangeability.** It assumes calibration prompts look like live prompts: a mix of coding prompts and languages.
- **Hosted Qwen text-rerank API:** whether its returned score equals the report's yes/no softmax was not verified. The Hugging Face card fetch failed with an SSL error.
- **Surprise (EVT):** taken from the abstract only, because the ar5iv HTML timed out.
- **Memory systems:** only the papers and the defaults listed above were read. Newer releases may add gating that is not covered here.

## Sources opened
- **arXiv HTML:**
  - Gates and QPP: 2609.15578, 2609.22880, 2601.14546, 2507.10411, 2501.12835
  - Truncation and calibration: 2506.08479, 2404.18185, 2511.17908, 2506.05176, 2411.06037
  - Memory systems: 2601.02553, 2506.06326, 2501.13956, 2507.07957, 2512.12818, 2502.12110, 2504.19413
- **arXiv API abstracts:** all IDs cited above, plus 2010.09797, 2404.04287, 2410.10813, 2604.07798, 2604.14159, 2608.08618, 2608.16168, 2609.16053, 2609.34438, 2506.20978, 2610.34872, 2310.11511, 2310.05002, 2403.10081, 2401.15884, 2406.12534 and 2406.19215.
- **Model card:** https://huggingface.co/BAAI/bge-reranker-v2-m3 (raw README).
- **Source code:**
  - Graphiti: https://github.com/getzep/graphiti, files `graphiti_core/search/search_config.py`, `graphiti_core/search/search_utils.py` and `graphiti_core/cross_encoder/openai_reranker_client.py`.
  - Mem0: https://github.com/mem0ai/mem0, file `mem0/memory/main.py`.
- **Repo metadata:** `gh api` for every code URL listed in the rows.
