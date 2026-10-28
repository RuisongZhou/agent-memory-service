<!-- Survey record behind the [selective-memory report](../selective-memory.md), compiled 2026-10-29 by research agents from primary sources (arXiv metadata and HTML, official repositories); every arXiv id and repository was opened. Kept as written. -->

# Agent-memory benchmarks and the 2026 landscape

Survey for agent-memory-service: changing formation and retrieval so that (1) nothing is injected when nothing is relevant, (2) fewer tokens are injected, (3) fewer duplicate or low-value facts and fewer extraction calls are produced. Checked 2026-10-29.

**Scope.** This covers work first released between 2024-10 and 2026-10. Older work is included only when a newer item builds on it, and is marked (background).

**How things were verified.**
- arXiv: every id below was fetched from `https://arxiv.org/abs/<id>`. Titles, dates and quoted numbers come from that page. The date shown is the page's `citation_date`. Where the id's month differs from that date, both are shown.
- GitHub repos: checked with `gh api repos/...`. Licenses come from the repo's license file.
- Hugging Face datasets: checked through the HF API. Licenses come from the dataset card.
- Websites: counted as verified when they returned HTTP 200.
- "No link" means the arXiv abstract and comments gave no code link, and I found none whose README or card cites the paper.
- UNVERIFIED marks anything I could not confirm from a primary page.
- Numbers quoted from abstracts are the authors' own claims, and they are not comparable across papers (different judges, readers and protocols).

---

## Benchmarks

### Cost model used below
- **Price.** qwen3.7-flash at the China-mainland list price, for requests up to 32K input tokens: **0.2 CNY per M input tokens, 0.8 CNY per M output tokens**. Batch costs 50% of the real-time price. Source: help.aliyun.com/zh/model-studio/model-pricing, read 2026-10-29. For comparison, qwen-flash is 0.15/1.5 (up to 128K) and qwen3.5-flash is 0.2/2.
- **Extractor assumptions (replace with your harness's measured averages):**
  - *Per-turn* (today's Mem0-V3-style path): one call per captured turn, 2,000 input tokens of overhead (instructions, recent context, existing memories) plus the turn text, and 150 output tokens.
  - *Per-session* (batched): one call per session, 1,500 tokens of overhead plus the session text, and 400 output tokens.
- **Formula:** `CNY ≈ 0.2 × (calls × overhead + corpus_tokens) / 1e6 + 0.8 × calls × output_tokens / 1e6`.
- **QA:** answering plus an LLM judge for about 2,000 questions with prompts of at most 1.5K tokens costs about 1 CNY in total.

### Established benchmarks

| Benchmark | What it measures | Negatives / abstention | Size | License | Download | Formation cost (assumptions above) |
|---|---|---|---|---|---|---|
| **LoCoMo** [2402.17753](https://arxiv.org/abs/2402.17753), 2024-02 (background; your harness) | QA, event summarization and multimodal dialogue over very long two-speaker conversations, with image sharing | Category 5 (adversarial) is expected to test answer-level abstention. The topic is probably present in the conversation, so it is likely not a clean "nothing relevant" case (confirm by reading items). Every QA item has an `evidence` list of dialog ids, so retrieval precision and recall can be scored without an LLM. | 10 conversations (README: the longest of the 50 in the March 2024 release). Paper average: 300 turns and 9K tokens, up to 35 sessions (for the original set). Total and per-category QA counts are UNVERIFIED (1,986 is the figure usually quoted); recount in the harness. | CC BY-NC 4.0 (LICENSE.txt) | [github.com/snap-research/locomo](https://github.com/snap-research/locomo) `data/locomo10.json`; [site](https://snap-research.github.io/locomo/) | Upper bound, assuming ≤7,000 turns, ≤0.3M tokens and about 300 sessions: per-turn **≈3.7 CNY**, per-session **≈0.25 CNY** |
| **LongMemEval** [2410.10813](https://arxiv.org/abs/2410.10813), ICLR 2025 | Five abilities: information extraction, multi-session reasoning, temporal reasoning, knowledge updates, abstention | Abstention questions are the ones whose `question_id` ends in `_abs` (README); their count is UNVERIFIED, so recount. `answer_session_ids` gives session-level evidence, and the README also describes turn-level evidence marking. | 500 questions. **S:** about 115K tokens (about 40 sessions) per history (Llama-3 tokens). **M:** about 500 sessions per history. **Oracle:** evidence sessions only. The cleaned release (2025-09) has files of 277.4 MB (S), 2,737.1 MB (M) and 15.4 MB (oracle). | MIT (repo and HF card) | [HF xiaowu0162/longmemeval-cleaned](https://huggingface.co/datasets/xiaowu0162/longmemeval-cleaned); [github.com/xiaowu0162/LongMemEval](https://github.com/xiaowu0162/LongMemEval) | **S, all 500, per-session:** 20,000 calls and about 87.5M input tokens, **≈24 CNY** (≈12 with Batch). **S per-turn** (assuming 10 user turns per session): **≈115 CNY**, over budget. **S, 100-question subset, per-session:** ≈4.8 CNY. **Oracle** (assumed about 3.5M tokens), per-session: ≈1.5 CNY. **M:** over budget. |
| **MemoryAgentBench** [2507.05257](https://arxiv.org/abs/2507.05257) (ICLR 2026 per repo) | Accurate retrieval, test-time learning, long-range understanding and selective forgetting, with long contexts fed in incremental chunks | No abstention set. Selective forgetting (FactConsolidation) gives stale-vs-current conflicts, which suit supersede and dedup tests. | Varies by subset (not checked). [2610.36308](https://arxiv.org/abs/2610.36308) uses FactConsolidation with 6K and 32K histories. | MIT (repo and HF) | [HF ai-hyz/MemoryAgentBench](https://huggingface.co/datasets/ai-hyz/MemoryAgentBench); [github.com/HUST-AI-HYZ/MemoryAgentBench](https://github.com/HUST-AI-HYZ/MemoryAgentBench) | FactConsolidation 6K and 32K: <1 CNY (assuming tens of histories) |
| **HaluMem** [2511.03506](https://arxiv.org/abs/2511.03506) | Hallucination at each memory operation: extraction (memory integrity = recall, memory accuracy = precision of memory points), updating, QA | Extraction precision directly scores fabricated or unsupported memories, and the data includes distractor memories (README). Best fit for "low-value facts". | HaluMem-Medium and HaluMem-Long: about 15k memory points and 3.5k questions; 1.5k and 2.6k turns per user on average; Long exceeds 1M tokens. User count UNVERIFIED. | **CC BY-NC-ND 4.0** (repo and HF). Fine for internal evaluation; do not publish derived negatives. | [HF IAAR-Shanghai/HaluMem](https://huggingface.co/datasets/IAAR-Shanghai/HaluMem); [github.com/MemTensor/HaluMem](https://github.com/MemTensor/HaluMem) | Per-turn, assuming 100 tokens per turn: ≈0.9 CNY per Medium user, ≈1.6 CNY per Long user. Run 3–5 users. |
| **MemBench** [2506.21605](https://arxiv.org/abs/2506.21605) | Factual vs reflective memory and participation vs observation settings, scored on effectiveness, efficiency and capacity | Not described in the abstract | Not checked | No license file (default: all rights reserved) | [github.com/import-myself/Membench](https://github.com/import-myself/Membench) | Not estimated |
| **BEAM** [2510.27246](https://arxiv.org/abs/2510.27246) (ICLR 2026 per repo) | Long-term memory in coherent conversations of up to 10M tokens, covering general, coding and math topics | The ability list was not checked; abstention is UNVERIFIED | 100 conversations and 2,000 questions, split into 128K (20 chats), 500K (35), 1M (35) and 10M (10). A 128K chat has 144 user and 144 assistant messages. | Data CC BY-SA 4.0 (HF card); code MIT | [HF Mohammadta/BEAM](https://huggingface.co/datasets/Mohammadta/BEAM) (and Mohammadta/BEAM-10M); [github.com/mohammadtavakoli78/BEAM](https://github.com/mohammadtavakoli78/BEAM) | 128K split (2.56M tokens, 2,880 user turns), per-turn: **≈2 CNY**. 1M and 10M: over budget. |
| **MEMTRACK** [2510.01353](https://arxiv.org/abs/2510.01353) | Memory and state tracking across interleaved Slack, Linear and Git timelines, including codebase exploration; scored on Correctness, Efficiency and Redundancy | Noisy, conflicting and cross-referring events | Not given in the abstract. Best model (GPT-5) reaches 60% Correctness. | n/a | No public link found (availability UNVERIFIED) | n/a |
| **Evo-Memory** [2511.20857](https://arxiv.org/abs/2511.20857) | Streaming task sequences where memory is searched and updated after each task; 10 datasets; ExpRAG and ReMem baselines | None stated | 10 datasets | n/a | No link | n/a |
| **MemoryBench (THUIR)** [2510.17281](https://arxiv.org/abs/2510.17281) | Continual learning from simulated user feedback across domains, languages and task types | None stated | The HF data includes LoCoMo and DialSim corpora | MIT | [HF THUIR/MemoryBench](https://huggingface.co/datasets/THUIR/MemoryBench); [github.com/THUIR/MemoryBench](https://github.com/THUIR/MemoryBench); [memorybench.thuir.cn](https://memorybench.thuir.cn) | n/a |
| **PersonaMem** [2504.14225](https://arxiv.org/abs/2504.14225) (COLM 2025 per repo) | Dynamic user profiling and personalized responses | None stated | Not checked | MIT (repo) | [github.com/bowen-upenn/PersonaMem](https://github.com/bowen-upenn/PersonaMem); HF bowen-upenn/PersonaMem (its card cites the paper) | n/a |

### 2026 benchmarks
Most of these have no verified code yet. Formation costs are omitted where sizes are unknown.

| Benchmark | Date, id | What it measures | Negatives / abstention | Size | Code / data (license) |
|---|---|---|---|---|---|
| **MemoryArena** | 2026-02-18, [2602.16313](https://arxiv.org/abs/2602.16313) (ICML 2026) | Multi-session agent tasks with interdependent subtasks: web navigation, preference-constrained planning, progressive search, formal reasoning. Agents near-saturated on LoCoMo do poorly here. | None stated | Not given | [memoryarena.github.io](https://memoryarena.github.io/); [github.com/ZexueHe/MemoryArena](https://github.com/ZexueHe/MemoryArena) (no license file) |
| **Memora** | 2026-04-21, [2604.20006](https://arxiv.org/abs/2604.20006) (ACL 2026 Findings) | Conversations spanning weeks to months; remembering, reasoning, recommending. Its FAMA metric penalizes use of obsolete memory. | Obsolete or invalidated memory | Not given | No link |
| **LongMemEval-V2** | 2026-05-12, [2605.12493](https://arxiv.org/abs/2605.12493) | Web-agent environment experience: static state recall, dynamic state tracking, workflow knowledge, environment gotchas, premise awareness | Premise-awareness questions | 451 questions; histories of up to 500 trajectories and 115M tokens | [HF xiaowu0162/longmemeval-v2](https://huggingface.co/datasets/xiaowu0162/longmemeval-v2) (card cites the paper; license not read) |
| **EvoMemBench** | 2026-05-18, [2605.18421](https://arxiv.org/abs/2605.18421) | In-episode vs cross-episode, and knowledge vs execution memory; 15 methods. Long-context baselines remain competitive. | None stated | 15 methods | [github.com/DSAIL-Memory/EvoMemBench](https://github.com/DSAIL-Memory/EvoMemBench) (no license file) |
| **Momento** | [2606.00832](https://arxiv.org/abs/2606.00832) (page date 2026-05-30) | Tool-mediated multi-session service tasks. Agents fail by treating stale history as current state. | Stale prior-session state | Not given | No link |
| **MemTrace** (benchmark) | 2026-06-15, [2606.17328](https://arxiv.org/abs/2606.17328) | Scores each knowledge point (one typed user fact) by memory age and question type (current, earlier, trajectory) | Evidence **present / missing / contradicted by a false premise**: ready-made negatives. Safe abstention does not imply correcting a false premise. | 13 system configurations | No verified link. HF zjunlp/MemTraceBench exists, but its card does not cite the paper. |
| **GateMem** | 2026-06-17, [2606.18829](https://arxiv.org/abs/2606.18829) | Shared memory with several principals: utility, access control, and active forgetting after deletion | Unauthorized and deleted information serve as leak targets | 4 domains | [github.com/rzhub/GateMem](https://github.com/rzhub/GateMem) (MIT); [HF Ray368/GateMem](https://huggingface.co/datasets/Ray368/GateMem) (CC BY 4.0) |
| **DynamicMem** | 2026-06-22, [2606.22877](https://arxiv.org/abs/2606.22877) | 15 months of synthetic multi-app activity with drifting attributes, habits and preferences | None stated | About 2.2M tokens and 1,772 events per user; 16 apps | No link |
| **MEMPROBE** (arXiv title "MemAudit") | 2026-06-23, [2606.24595](https://arxiv.org/abs/2606.24595) | Reconstructs a hidden user state from the memory store, with full-store and top-k access | None stated | 50 users × 31 hidden dimensions | No link |
| **AgentMemBench** | [2608.00009](https://arxiv.org/abs/2608.00009) (page date 2026-06-16) | Five strategies on LoCoMo, MultiDoc2Dial and MSC, scored with Recall@k, MRR, nDCG, F1, faithfulness, **memory footprint** and **latency**. Qwen2.5-7B (4-bit) serves as both generator and judge. | None stated | 491 annotated question turns | The abstract says it is released, but no link is given |
| **LTP** (LoCoMo Temporal Plus, from A-TMA) | 2026-07-02, [2607.01935](https://arxiv.org/abs/2607.01935) | "Ghost memory": old, current and transition facts mixed together in the store | Conflict-heavy | Not given | No link |
| **Ground Truth First** | 2026-07-24, [2607.21962](https://arxiv.org/abs/2607.21962) | A script-first corpus: facts with validity intervals, as-of-date question sets and injection probes, plus a no-memory control | Every question is checked for answerability; as-of-date questions | About 380 questions of 15 types | [github.com/veracium-ai/Veracium](https://github.com/veracium-ai/Veracium) (MIT) |
| **MERIT** ("When Does Memory Help?") | [2609.05441](https://arxiv.org/abs/2609.05441) (page date 2026-07-26) | Marginal value of memory for tool-using agents, with full token and dollar metering, leak-checked dependence and controlled corruption | Leak-verified floor of 0.00 without memory | 23,440 scored episodes cost the authors $42.57 | No link |
| **TANGLE** | 2026-08-14, [2608.13921](https://arxiv.org/abs/2608.13921) | Memory conflicts that cannot be resolved; scores calibration and clarification-seeking | Underdetermined queries | 541 instances, 40 personas | No link |
| **UTILMEM** | 2026-08-31, [2608.30508](https://arxiv.org/abs/2608.30508) | Evidence use when plausible distractors are present | Distractors | Not given | [github.com/peijunallin/UtilMem](https://github.com/peijunallin/UtilMem) (no license file) |
| **LOCOMO-CONV** ("When Users Don't Ask") | 2026-09-03, [2609.03467](https://arxiv.org/abs/2609.03467) | LoCoMo-derived conversational use with four query styles (dialog, implicit, counterfactual, composed), plus `supportive_memory` annotations. **Closest to a hook that sees prompts, not questions.** | Counterfactual style | Not given | No link |
| **VibeMemBench** | 2026-09-20, [2609.23570](https://arxiv.org/abs/2609.23570) | Memory systems for coding agents on real repository tasks, scored by executable tests. In 11 of 12 solver/system pairings, the memory system does not beat memory-off. | Memory-off baseline for every target | 111 targets from 90 SWE-rebench V2 repos; 3,634 history trajectories | No link |
| **DolphinBench** | 2026-09-21, [2609.24971](https://arxiv.org/abs/2609.24971) | Memory measured through task completion; **cost and latency must be reported** | Every task is verified to fail without the history | 3 personas, each with about 500K tokens of user messages; 200 tasks per persona | [dolphinbench.ai](https://dolphinbench.ai); [HF mem0ai/dolphinbench](https://huggingface.co/datasets/mem0ai/dolphinbench) (Apache-2.0; hosted under the Mem0 org) |
| **MemCalib** | 2026-09-21, [2609.24259](https://arxiv.org/abs/2609.24259) | Whether the model gives each in-context memory the right weight (over-use vs under-use) | Over-use of memory | Not given | No link |
| **CPB** (epistemic admission) | 2026-09-25, [2609.30813](https://arxiv.org/abs/2609.30813) | Whether candidate claims should be admitted to shared memory | False claims, verbatim copies, paraphrases | Not given | No link |
| **"Beyond Downstream QA"** | 2026-10-15, [2610.22209](https://arxiv.org/abs/2610.22209) | Direct scoring of memory operations (create, update, reinforce, supersede). The evaluated system gets the operation right only 20% of the time. | None stated | Not given | github.com/amazon-science/feb-30-benchmark returned **404** on 2026-10-29 |
| **MemDev-Bench** | 2026-10-20, [2610.27524](https://arxiv.org/abs/2610.27524) | Memory systems for code agents over real repository histories. Raw trajectories save more agent steps than the memory systems do. | Relevance of prior tasks is controlled | Not given | No link |

**Diagnostic protocols that are not datasets:**
- **MemDelta** ([2606.29914](https://arxiv.org/abs/2606.29914)): changes one variable at a time on LongMemEval-S.
- **EvalMem** ([2609.22231](https://arxiv.org/abs/2609.22231), page date 2026-09-03): examines each operation separately.
- **"Diagnosing Retrieval vs. Utilization"** ([2603.02473](https://arxiv.org/abs/2603.02473); [github.com/boqiny/memory-probe](https://github.com/boqiny/memory-probe), no license file).

---

## How to build negatives

Two kinds of negative need to be scored separately:

- **N1, gate negatives.** No stored memory is relevant, so the hook should inject nothing. These are scored on retrieval output alone, at no cost.
- **N2, answer negatives.** Related memories exist, but the asked-about fact does not (adversarial question or false premise). Injecting the related memories is acceptable; the answerer should abstain. These are scored with an LLM judge.

LoCoMo category 5 and LongMemEval `_abs` are *expected* to be mainly N2: a false premise asked about a topic that is present. I did not read the items themselves, so read about 20 of each in the harness before deciding how to score them. If they are N2, treating them as N1 would punish a gate for correctly injecting on-topic memories.

Constructions, cheapest first:

1. **Cross-store swap (N1; free; guaranteed negatives).**
   - Retrieve question q from conversation A against the store built from conversation B.
   - LoCoMo has 10 stores, so each question has 9 candidate negative stores. Sample 1–2 per question to keep the classes balanced.
   - For LongMemEval-oracle, use question j's store for question i, after checking that the two share no sessions.
   - The plain version is easy, because the speaker names differ. For a **hard** version, replace A's speaker names in the question with B's so the lexical overlap survives.
2. **Prefix negatives, i.e. "not said yet" (N1; hard; matches real use).**
   - Build the store only from sessions before q's earliest evidence session. For LoCoMo, map the `evidence` dialog ids to sessions; check the id format in the harness. LongMemEval has `answer_session_ids` and `haystack_dates`.
   - The persona and topics are the same, but the fact is absent.
   - Leave out multi-evidence questions whose earlier evidence is already present, or label them "partial".
3. **Evidence ablation (N1 or N2).**
   - Remove the evidence turns (LoCoMo) or answer sessions (LongMemEval) before forming memories, then ask q.
   - Use the reranker plus a manual spot check to confirm that no paraphrased evidence survives elsewhere. The LongMemEval-cleaned release was cleaned for exactly this kind of interference.
4. **Built-in abstention items (N2):**
   - LoCoMo category 5 and LongMemEval `_abs`.
   - MemTrace's missing-evidence and false-premise conditions (no verified code).
   - LongMemEval-V2 premise-awareness questions.
   - Ground Truth First's as-of-date questions (Veracium, MIT).
   - LOCOMO-CONV's implicit and counterfactual query styles, if the data is released.
5. **Workload negatives (N1; the real target distribution).** Label a few hundred of your own agents' prompts in three classes:
   - (a) Ordinary coding prompts that need no long-term memory, such as "run the tests" or "fix this type error".
   - (b) Cross-project: a prompt from repo A run against repo B's memories.
   - (c) Cross-agent scope: memory written by Codex in one repo, prompt from Claude Code in another.

   Report the false-injection rate for each class. GateMem and "Right Memory, Wrong Context" ([2610.07309](https://arxiv.org/abs/2610.07309), MIT code) show how to score scope and admissibility leaks.
6. **Formation negatives (duplicates and low-value facts):**
   - HaluMem's extraction precision and its distractor memories.
   - MemoryAgentBench FactConsolidation for superseded facts.
   - [2610.36308](https://arxiv.org/abs/2610.36308): an obsolete competing statement hurts more than unrelated evidence of the same size.
   - A direct near-duplicate count: bge-m3 cosine ≥ 0.92, confirmed by bge-reranker-v2-m3.

**Metrics:**
- Gate quality:
  - **false-injection rate** (FIR) on each N1 type;
  - **positive coverage**: the share of positive prompts whose injected set holds a memory sourced from an evidence turn;
  - **injection precision**;
  - **AUROC** of the gate score.
- Token cost: **injected tokens per prompt** (mean and p95), on positives and on negatives.
- Answers: accuracy and abstention accuracy on N2.

This needs provenance: store the source dialog or session ids with each memory in the harness. Without them, precision needs a judge.

**Gate calibration.** Gate on a bge-reranker-v2-m3 score threshold, the top-1 margin, or both. Fit it on LoCoMo conversations 0–4, then report on 5–9 and on the LongMemEval subset. LoCoMo prompts are explicit questions, which is why the workload negatives are needed too.

---

## Recommended evaluation within 50 CNY

**Ground rules from 2026 evaluation papers:**
- Keep bge-m3 and bge-reranker-v2-m3 fixed across all variants. MemDelta found that swapping only the embedding model moves LongMemEval-S accuracy by 6.2 points and can reverse a conclusion.
- Report write-path cost (calls, tokens, CNY) alongside accuracy.
- Always include a **raw-turn RAG baseline with zero LLM calls at write time** ([2603.02473](https://arxiv.org/abs/2603.02473), [2610.30755](https://arxiv.org/abs/2610.30755), [2610.24671](https://arxiv.org/abs/2610.24671)).
- Use a judge that differs from the answer model, or hand-check about 100 judgments. Same-model self-grading inflates scores; for example, SodaMem's reported numbers are self-graded.

**Variants:**
- **A**: today's path (per-turn additive extraction, always injects top-5).
- **A+gate**: A with the relevance gate added.
- **B+gate**: new formation (batched or salience-gated extraction plus dedup) with the gate.
- **Raw-RAG+gate**: zero write cost.

Comparing these separates the effect of the formation change from the effect of the gate.

| # | Step | Paid? | Est. CNY |
|---|---|---|---|
| 1 | **LoCoMo-10 formation** with A (per-turn) and B. Log calls, tokens, memories per 100 turns, near-duplicate rate, and the share of memories never retrieved by any question. Keep the image-caption setting the same for all variants: [2610.29823](https://arxiv.org/abs/2610.29823) found that photo captions explain part of the gap between systems. | yes | ≤3.7 + ≤1 |
| 2 | **LoCoMo gate evaluation.** Positives are categories 1–4, scored with evidence ids. N1 are the cross-store (easy and hard) and prefix negatives. Run for A, B and Raw-RAG, sweeping the threshold. | no | 0 |
| 3 | **LoCoMo QA plus judge** on all questions including category 5, for A, A+gate, B+gate and Raw-RAG+gate | yes | ≈4 |
| 4 | **LongMemEval-S, 100 questions**: all `_abs` questions plus a sample stratified by `question_type`. Form with B (per-session) and Raw-RAG; gate evaluation is free; then QA plus judge. | yes | ≈4.8 (≈2.4 with Batch) + 0.5 |
| 5 | **BEAM-128K** (20 chats, some with coding and math topics): A and B, then QA plus judge | yes | ≈2 + 0.6 + 0.4 |
| 6 | **Workload negatives**: about 300 hand-labelled prompts from your own agents, gate only | no (≈0.2 if a judge does first-pass labels) | ≈0.2 |
| 7 | *Optional:* **LongMemEval-oracle**, all 500, with A and B, to compare calls and duplicates on clean evidence and to get cross-question negatives | yes | ≈1.5 (B) + ≈7 (A per-turn; ≈3.6 with Batch) |
| | **Total** | | **≈17 without step 7, ≈26 with it**, leaving 24–33 CNY for reruns and prompt fixes |

**Report one row per variant with:**
- FIR on each negative type (N1-easy, N1-hard, N1-prefix, and each workload class);
- positive coverage, injection precision and AUROC;
- injected tokens (mean and p95);
- the empty-injection rate on positives;
- QA accuracy by category, plus abstention accuracy on category 5 and `_abs`;
- extraction calls, tokens and CNY per 100 turns;
- memories per 100 turns, near-duplicate rate, and never-retrieved share.

**Out of budget or the wrong shape for now:**
- LongMemEval-M, and BEAM-1M or BEAM-10M.
- DynamicMem (about 2.2M tokens per user).
- LongMemEval-V2 formation (115M tokens).
- Agent-loop benchmarks: DolphinBench, VibeMemBench, MemDev-Bench, MemoryArena. Their agent runs, not formation, dominate the cost. Revisit DolphinBench or VibeMemBench later, on a small subset, for coding-agent evidence.
- HaluMem beyond 3–5 users, because of the ND license and judge cost.

All estimates assume the extractor sizes stated above. The LoCoMo QA count and LongMemEval turns per session are UNVERIFIED, and costs scale linearly with both.

---

## Jun–Oct 2026 landscape

**Volume.** An arXiv API query for title "memory" AND abstract "agent", submitted 2026-06-01 to 2026-10-31, hit its 400-result cap while only reaching back to 2026-08-30. The field is producing hundreds of papers per month, so the tables below are a selection by relevance to this decision. Every id was fetched from arxiv.org/abs. The last column says whether the abstract reports LoCoMo or LongMemEval (LME) numbers.

### Systems and methods

| Date | arXiv | Name | Code | Mechanism (one line) | LoCoMo / LME |
|---|---|---|---|---|---|
| 2026-06-04 | [2606.06036](https://arxiv.org/abs/2606.06036) | MRAgent (ICML 2026) | No link | Cue-Tag-Content graph; the LLM explores and prunes retrieval paths step by step | Both, but the numbers were cut off in the abstract I read |
| 2026-06-04 | [2606.05894](https://arxiv.org/abs/2606.05894) | EMBER | No link | Learned pre-query retention policy that keeps verbatim "evidence capsules" within a token budget | LME-derived LongMemEval-RR: F1 0.3017 vs 0.1765 at 8,192 retained tokens |
| 2026-06-18 | [2606.19847](https://arxiv.org/abs/2606.19847) | AtomMem | No link | Selects high-value atomic facts, then builds hierarchical events, temporal profiles and an associative graph | LoCoMo state of the art claimed (numbers not in the abstract) |
| 2026-07-02 | [2607.01935](https://arxiv.org/abs/2607.01935) | A-TMA | No link | Overlay that labels records as current, historical or transition | LoCoMo temporal F1 0.0295 → 0.1705 (on Graphiti) |
| 2026-07-17 | [2607.22690](https://arxiv.org/abs/2607.22690) | **LazyMem** | [allacnobug/LazyMem](https://github.com/allacnobug/LazyMem) (no license file) | No write-time compression. At query time a 4B model (SFT then RL) filters and compresses a broad retrieved pool in parallel windows. | **LME judge 0.85 using 213 answer-context tokens, 21× fewer than the strongest non-oracle baseline**; also generalizes to LoCoMo |
| 2026-07-24 | [2607.21962](https://arxiv.org/abs/2607.21962) | Veracium (Ground Truth First) | [veracium-ai/Veracium](https://github.com/veracium-ai/Veracium) (MIT) | Layered, provenance-typed memory. Rankings flip with history length (a budgeted curated map drops from 96% to 72% by nine weeks). | Own corpus only |
| 2026-08-08 | [2608.08055](https://arxiv.org/abs/2608.08055) | SodaMem | [SodaMem/SodaMem](https://github.com/SodaMem/SodaMem) (Apache-2.0) | Typed FactEvents with mandatory provenance spans and SUPERSEDES / CONTRADICTS / UPDATES edges; planner-reader loop | LME-S 92.8% (best of 3, self-graded) at $0.00161 and about 18.3K tokens per question, using deepseek-v4-flash |
| 2026-08-24 | [2608.22767](https://arxiv.org/abs/2608.22767) | EARM | No link | Reuses past LLM relevance scores through online matrix completion, so fewer candidates need LLM reranking | Long-term conversational memory: up to +6.62% over semantic retrieval with only 17.5% of candidates scored by the LLM |
| 2026-08-30 | [2608.29606](https://arxiv.org/abs/2608.29606) | Agent Zero Memory | No link | Three parallel stores. An **intent gate** means self-contained turns add no latency. A citation lock makes it abstain rather than guess. | **Self-reported** LME 95.60% and LoCoMo 93.60%; across 8 backbones, cost varies about 30× while accuracy varies 3.4 points |
| 2026-09-02 | [2609.29545](https://arxiv.org/abs/2609.29545) | ERRAND | No link | Treats rechecking a stored item as a priced, budgeted action | — |
| 2026-09-03 | [2609.05553](https://arxiv.org/abs/2609.05553) | **EdgeMem** | [Soullesskid/edgemem](https://github.com/Soullesskid/edgemem) (Apache-2.0) | Keeps original turns in a multi-anchor hypergraph index; **no generative-LLM calls at construction or retrieval** | LoCoMo strict judge 61.01 vs 58.70 (best of 7 reproduced systems) |
| 2026-10-01 | [2610.00872](https://arxiv.org/abs/2610.00872) | MemFit | No link | Cheap writes; lexical plus semantic retrieval with cross-encoder reranking | LoCoMo, MemGallery and LME-S: state of the art claimed, with several-fold lower construction time and cost |
| 2026-10-01 | [2610.02472](https://arxiv.org/abs/2610.02472) | APDMem | No link | Progressive disclosure over four granularity levels; simple queries stop early | LME: strong results while accessing 8% of the conversations |
| 2026-10-04 | [2610.04838](https://arxiv.org/abs/2610.04838) | MemTrace (coding agents; not the 2606.17328 benchmark) | [Homy-Xu/MemTrace](https://github.com/Homy-Xu/MemTrace) (Apache-2.0 text) | Checks old evidence against the current repo state; retrieves only what the next action needs | DeepSWE +21.2 pass@1, SWE-EVO +4.4, SWE-Milestone +17.8 under Codex CLI |
| 2026-10-05 | [2610.07100](https://arxiv.org/abs/2610.07100) | Category-conditioned retention | NeurIPS 2026 workshop (no code) | Applies a write-time confidence threshold per assertion category instead of one global threshold | — |
| 2026-10-05 | [2610.07309](https://arxiv.org/abs/2610.07309) | Retrieval admissibility | [ziwang11112/right-memory-wrong-context](https://github.com/ziwang11112/right-memory-wrong-context) (MIT) | Checks separately whether a retrieved memory is supported, admissible (owner, policy, lifecycle), exposed in the prompt, and disclosed in the answer | — |
| 2026-10-06 | [2610.09008](https://arxiv.org/abs/2610.09008) | CASK ("Whose Memory Is It?") | No link | Scope-aware commit rules, so provisional reasoning does not become a durable "fact" | — |
| 2026-10-08 | [2610.33015](https://arxiv.org/abs/2610.33015) | **SLICE-Mem** | No link | A **compact classifier admits only salient user turns**, followed by predicate-centered extraction | **LME-S 56.8% vs 59.3% with 99.5% fewer LLM tokens**; Memora FAMA 46.60 vs 46.68 with 92.4% fewer (Qwen3-30B) |
| 2026-10-16 | [2610.23812](https://arxiv.org/abs/2610.23812) | Personalized memory gating | No link | Per-user, per-fact gate learned from delayed one-sided feedback | — |
| 2026-10-19 | [2610.27377](https://arxiv.org/abs/2610.27377) | **REM** (coding agents) | No link | A policy decides what experience to store, when to inject it and what to forget | Task resolution 27.6% vs 24.9% at 17% lower cost. Injecting a summary or raw trajectory, even an oracle-retrieved one, does not help. |
| 2026-10-23 | [2610.33274](https://arxiv.org/abs/2610.33274) | FlexA-Mem | [WujiangXu/Flex-Amem](https://github.com/WujiangXu/Flex-Amem) (MIT) | Budget-adaptive timeline plus recovery | LME and LoCoMo: matches a keyword retriever that writes no memory; LLM-written memories fall behind at tight budgets |
| 2026-10-26 | [2610.34673](https://arxiv.org/abs/2610.34673) | **AgentMemRec** | No link | Treats memory selection as **budgeted slate recommendation** (candidate generation, learning to rank, slate construction) **with no LLM calls** | LME multi-session +10 to 31 points over A-Mem, HippoRAG 2 and others; matches a 64-call LLM reranker at 3.2× lower serving cost; beats Mem0 and Zep on LoCoMo without any write-time LLM call |
| 2026-10-26 | [2610.36153](https://arxiv.org/abs/2610.36153) | Stratum (coding agents) | [Zenodo 10.5281/zenodo.23615180](https://doi.org/10.5281/zenodo.23615180) | Persistent, experience-aware repository memory | Root-cause file found in 21/24 trials vs 6/24 for retrieval-only, at 4.5× lower median token cost |

### Evaluations and analyses that bear on the decision

| Date | arXiv | Title | Code | Finding |
|---|---|---|---|---|
| 2026-06-23 | [2606.24775](https://arxiv.org/abs/2606.24775) | Are We Ready For An Agent-Native Memory System? | [OpenDataBox/MemoryData](https://github.com/OpenDataBox/MemoryData) (no license file) | 12 systems on 5 workloads (11 datasets). No architecture wins everywhere, and localized maintenance is cheaper than global reorganization. |
| 2026-06-29 | [2606.29914](https://arxiv.org/abs/2606.29914) | MemDelta | No link | Swapping only the embedding model moves LME-S accuracy by 6.2 points. Mem0 beats MiniLM-RAG by 11 points but loses to cloud RAG by 1.2. On 2 of 6 question types, Mem0 matches cloud RAG at 50× the cost. |
| 2026-09-03 | [2609.22231](https://arxiv.org/abs/2609.22231) | EvalMem | No link | Retrieval is the most common failure layer. On LoCoMo, retrieval defects are 22.1%, vs 7.7% for encoding and 6.5% for generation. |
| 2026-09-20 | [2609.23790](https://arxiv.org/abs/2609.23790) | Total Cost of Agency | [vsingh45/tca-compiler](https://github.com/vsingh45/tca-compiler) (MIT) | Injected memory is about 12% of the billed cost in multi-agent workflows, rising to 27.6% at depth six. |
| 2026-10-04 | [2610.05124](https://arxiv.org/abs/2610.05124) | Memory Canonicalization | No link | The same stored memory can be read differently by different LLMs. Results are exploratory and not significant after correction. Relevant to one store shared by Claude Code, Codex and WorkBuddy. |
| 2026-10-17 | [2610.24671](https://arxiv.org/abs/2610.24671) | Do Conversational Agents Need Explicit Memory? | No link | Compares six explicit memory systems with raw-history RAG on LME-S and BEAM-100K. Retrieval remains the main bottleneck. |
| 2026-10-21 | [2610.29823](https://arxiv.org/abs/2610.29823) | MemoryTax: The Bitter Lesson of Agent Memory | No link | Uses one LoCoMo conversation (419 turns, 152 questions). Coding agents that search the raw history (Pi, Codex, Claude Code) are compared with Mem0, Zep, BM25 and full context. A written memory caps what a stronger reader can recover. |
| 2026-10-22 | [2610.30755](https://arxiv.org/abs/2610.30755) | Is Complex Agentic Memory Always Needed? | No link | Raw-RAG (dense retrieval plus reranking over the raw history) does well across LoCoMo, LME, MEME and MemoryAgentBench. Even full access to transformed stores trails raw evidence. |
| 2026-10-25 | [2610.34073](https://arxiv.org/abs/2610.34073) | Memory Tax: Dissecting the Net Value of Agent Memory (a different paper from 2610.29823) | [project page](https://hexuan21.github.io/MemoryTax/) | Many dedicated memory systems do worse than naive RAG, i.e. they have negative net value. |
| 2026-10-26 | [2610.36308](https://arxiv.org/abs/2610.36308) | When Retrieval Is Not Enough: Obsolete Facts | No link | A larger retrieved-evidence budget lowers accuracy. With 32K histories, a 64-unit BM25 budget scores 69% vs 58% at 512. Stale competing statements do more harm than unrelated text. |

**Earlier in 2026, for context:**
- [2603.02473](https://arxiv.org/abs/2603.02473): on LoCoMo, retrieval method spans 20 accuracy points but write strategy only 3–8. Raw chunks with **zero LLM calls** match or beat Mem0-style extraction.
- [2603.04814](https://arxiv.org/abs/2603.04814): Mem0 vs long-context LLMs. At 100K context, memory becomes cheaper after about 10 turns.
- [2604.04853](https://arxiv.org/abs/2604.04853) MemMachine: stores whole episodes and reports about 80% fewer input tokens than Mem0. Scores are LoCoMo 0.9169 (gpt-4.1-mini) and LME-S 93.0%.
- [2604.11628](https://arxiv.org/abs/2604.11628) Back to Basics: turn-isolation retrieval with query-driven pruning.

**Seen only in the arXiv API listing, not opened (UNVERIFIED):** 2609.33226 (rethinking memory access), 2610.22668 (from relevant memories to sufficient evidence), 2610.09348 (relevance is not sufficiency), 2610.23313 (query-conditioned memory compilation), 2609.29144 (scope before you persist), 2610.18052 (memory unlearning benchmark), 2610.14431 (MemForm, formation poisoning), 2609.34422 (coding-agent memory post-training).

### Memory features shipped by coding agents (primary docs)

**Claude Code auto memory** ([code.claude.com/docs/en/memory](https://code.claude.com/docs/en/memory), read 2026-10-29):
- Claude writes its own notes. Each note's kind is recorded as a `type` field in the file's frontmatter (four kinds).
- Storage is per repository at `~/.claude/projects/<project>/memory/`, shared across worktrees. It holds a `MEMORY.md` index (one line per memory) and one topic file per memory.
- The index is loaded into every session, up to the **first 200 lines or 25KB**.
- On by default in local sessions. Controls: the `/memory` toggle (`autoMemoryEnabled`), `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1`, and `autoMemoryDirectory`. Subagents can keep their own.
- *Takeaway:* it uses an index-first, progressive-disclosure design with no per-prompt retrieval injection.

**Codex memories** ([learn.chatgpt.com/docs/customization/memories](https://learn.chatgpt.com/docs/customization/memories?surface=app), redirected from developers.openai.com/codex/memories):
- **Off by default.** Enable it in the ChatGPT desktop app or with `[features] memories = true`.
- Memories are generated **in the background from eligible past chats once they have been idle**. Active and short-lived sessions are skipped, and secrets are redacted.
- Two models are involved: a per-chat **extraction** model (`memories.extract_model`) and a global **consolidation** model (`memories.consolidation_model`).
- Other keys: `memories.use_memories`, `memories.generate_memories`, `memories.disable_on_external_context`, `memories.min_rate_limit_remaining_percent`. Storage is under `~/.codex/memories/`, and `/memories` gives per-chat control.
- The page says must-follow rules belong in AGENTS.md.
- *Takeaway:* extraction runs once per finished chat, not per turn, followed by a separate consolidation pass. The page gives no default values for the keys.

---

## Surveys

| Date | arXiv | Title | Code | Notes |
|---|---|---|---|---|
| 2024-04-21 | [2404.13501](https://arxiv.org/abs/2404.13501) | A Survey on the Memory Mechanism of Large Language Model based Agents (background) | [nuster1128/LLM_Agent_Memory_Survey](https://github.com/nuster1128/LLM_Agent_Memory_Survey) | Covers what agent memory is, why it is needed, and how to design and evaluate it |
| 2025-04-22 | [2504.15965](https://arxiv.org/abs/2504.15965) | From Human Memory to AI Memory: A Survey on Memory Mechanisms in the Era of LLMs | No link | Relates LLM memory to human memory |
| 2025-05-01 | [2505.00675](https://arxiv.org/abs/2505.00675) | Rethinking Memory in LLM based Agents: Representations, Operations, and Emerging Topics (the current arXiv title; the user-facing name was "Rethinking Memory in AI") | [Elvin-Yiming-Du/Survey_Memory_in_AI](https://github.com/Elvin-Yiming-Du/Survey_Memory_in_AI) (MIT) | Parametric vs contextual memory; six operations (consolidation, updating, indexing, forgetting, retrieval, condensation) |
| 2025-12-15 | [2512.13564](https://arxiv.org/abs/2512.13564) | Memory in the Age of AI Agents | No link | Separates agent memory from LLM memory, RAG and context engineering; analyzes memory by forms, functions and dynamics |
| 2026-01-14 (id 2602.06052) | [2602.06052](https://arxiv.org/abs/2602.06052) | A Survey of Agent Memory in the Second Half: Towards Self-Evolving and Long-Horizon Agents (TMLR, Survey Certification) | Project page (link not checked) | Organizes memory by substrate, cognitive mechanism and subject (user-centric vs agent-centric) |
| 2026-03-08 | [2603.07670](https://arxiv.org/abs/2603.07670) | Memory for Autonomous LLM Agents: Mechanisms, Evaluation, and Emerging Frontiers | No link | Write–manage–read loop; covers write-path filtering, contradiction handling, latency budgets, and coding agents |
| 2026-06-29 | [2606.30306](https://arxiv.org/abs/2606.30306) | Always-On Agents: A Survey of Persistent Memory, State, and Governance in LLM Agents | No link | Codes 435 works and finds the field focused on accumulating and retrieving memory more than on governing or relinquishing it; proposes the AOEP-v0 evaluation protocol |
