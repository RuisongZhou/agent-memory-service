<!-- Survey record behind the [selective-memory report](../selective-memory.md), compiled 2026-10-29 by research agents from primary sources (arXiv metadata and HTML, official repositories); every arXiv id and repository was opened. Kept as written. -->

# Write-time efficiency and reflection/consolidation: survey for agent-memory-service

Compiled 2026-10-29. Scope: work first released 2024-10 to 2026-10. Our problems: (1) retrieval always injects something; (2) duplicate and low-value facts accumulate; (3) one LLM extraction call per captured turn; (4) heavy entity projection.

How things were verified:

- arXiv IDs, first-version month, current title and the comment/journal-ref field (venue) come from the arXiv export API (`export.arxiv.org/api/query`, `id_list` and `search_query`). That is the same metadata record that `arxiv.org/abs/<id>` renders. Every ID below resolved to the expected title.
- Mechanism details that go beyond the abstract come from keyword excerpts of `arxiv.org/html/<id>`. Where the HTML was unavailable or did not cover a point, the row says "per abstract" or "not verified".
- Repos were checked through the GitHub API (`gh api`, `gh repo view`) on 2026-10-29. "License: none detected" means GitHub found no license file, so do not assume the code can be reused.
- A venue that is missing from the arXiv record but stated in the official repo README is marked "(README)".
- Mechanisms are paraphrased. The one direct quote is from the ForgetEval abstract.

## Summary table

| # | Name | arXiv (first version) | Venue | Code (license; last commit) | Landable | Problems |
|---|---|---|---|---|---|---|
| 1 | Mem0 (paper, V3 additive, Dream) | 2504.19413 (2025-04) | none | mem0ai/mem0 (Apache-2.0; 2026-10-26; ts-v3.3.1 2026-09-25) | yes | 2, 3 |
| 2 | LightMem | 2510.18866 (2025-10) | ICLR 2026 | zjunlp/LightMem (MIT; 2026-10-26) | partial | 3, 2 |
| 3 | Reproducing LightMem | 2607.29104 (2026-07) | SIGIR-AP 2026 | ielab/Reproducing-LightMem (MIT; 2026-10-21) | evidence | 3, 1 |
| 4 | SeCom | 2502.05589 (2025-02) | ICLR 2025 (README) | microsoft/SeCom (MIT; 2025-03-01, dormant) | partial | 3, 2 |
| 5 | LycheeMemory V2 | 2608.12990 (2026-08) | none | none found | partial | 3 |
| 6 | Nemori (now titled "What Deserves Memory") | 2508.03341 (2025-08) | none | nemori-ai/nemori (license: none detected; pushed 2026-04-16) | partial | 2 |
| 7 | SimpleMem | 2601.02553 (2026-01) | none | aiming-lab/SimpleMem (MIT; 2026-07-24; v0.3.0) | partial | 2, 3 |
| 8 | LangMem | no paper | none | langchain-ai/langmem (MIT; 2026-10-02) | yes | 3 |
| 9 | Letta sleep-time compute / Dreaming | 2504.13171 (2025-04) | none | letta-ai/sleep-time-compute (MIT; 2025-04-30); letta-ai/letta (Apache-2.0; 2026-10-18) | partial | 2, 3 |
| 10 | ACE (Agentic Context Engineering) | 2510.04618 (2025-10) | ICLR 2026 | ace-agent/ace (Apache-2.0; 2026-08-24) | partial (high fit) | 2, 1 |
| 11 | MemoryOS | 2506.06326 (2025-05) | EMNLP 2025 main (README) | BAI-LAB/MemoryOS (Apache-2.0; 2026-07-07) | partial | 2, 1 |
| 12 | RMM (Reflective Memory Management) | 2503.08026 (2025-03) | ACL 2025 | none found | partial | 1, 2 |
| 13 | Hindsight | 2512.12818 (2025-12) | none | vectorize-io/hindsight (license: none detected; 2026-10-28) | partial | 2 |
| 14 | SLICE-Mem | 2610.33015 (2026-10) | none | none found | partial | 3, 2 |
| 15 | MemFit | 2610.00872 (2026-10) | none | none listed (HTML unavailable) | yes | 3, 4 |
| 16 | Selection vs extraction (Jev, pre-registered) | 2609.34227 (2026-09) | none | ris3abh/Engram (license: none detected; 2026-09-29) | evidence / partial | 3, 1 |
| 17 | PreCoMem | 2610.29467 (2026-10) | NeurIPS 2026 | ZhongWishing/PreCoMem (license: none detected; 2026-10-12) | partial | 2, 1 |
| 18 | Stashbird | 2609.34242 (2026-09) | none | none found | yes (bookkeeping) | 2 |
| 19 | Control-plane placement / ForgetEval | 2606.15903 (2026-06) | none | deeplethe/lethe (abstract says MIT; GitHub detects none; 2026-06-16) | yes (design rule) | 2 |
| 20 | RSPM (Let Them Sleep) | 2610.25561 (2026-10) | BayLearn 2026 (oral) | none found | yes | 2, 3 |
| 21 | Tether (dependency-tethered memory) | 2610.25123 (2026-10) | none | none found | yes (code facts) | 2, 1 |
| 22 | Multi-factor memory value model | 2606.12945 (2026-06) | none | zhibao-dev/Learning-Multi-Factor-Memory (license: none detected; 2026-06-30) | partial | 2, 1 |

## Rows

### 1. Mem0: paper, V3 additive pipeline, and the Dream background layer
- **Title:** Mem0: Building Production-Ready AI Agents with Scalable Long-Term Memory. First released 2025-04. No venue on the arXiv record.
- **arXiv:** 2504.19413 (verified). **Code:** github.com/mem0ai/mem0, Apache-2.0, last commit 2026-10-26, latest release ts-v3.3.1 (2026-09-25). Actively maintained.
- **Mechanism:**
  - Paper: an extraction step proposes candidate facts. The update step retrieves similar memories, and an LLM function call picks ADD, UPDATE, DELETE or NOOP.
  - V3: the SDK changelog (PR #4805) says the two-call pipeline was replaced by a single additive extraction. Memories now accumulate through `linked_memory_ids`, and no UPDATE or DELETE events are emitted. The add-API docs describe it as one LLM call that only adds.
  - The platform Dream docs (`docs/platform/features/dream.mdx`) add the cleanup back as a separate layer:
    - Supersede: when a newer memory contradicts an older one, the older one is marked superseded and linked to the newer. Runs on add.
    - Merge: a duplicate is folded into one canonical memory. A barer duplicate is marked merged and hidden from reads, and most near-duplicates are caught on add. Runs on add.
    - Synthesis: on a schedule, it writes pattern memories alongside their sources, linked back to them. It is additive and idempotent.
  - Every Dream action keeps the original record, so nothing is erased. Default reads return active and superseded memories (the superseded ones labelled) and hide merged ones. `latest_only=true` returns only active memories.
  - In the OSS repo, "superseded" appears only in docs and in the platform-client SDK files. That suggests Dream is platform-only and not part of OSS 3.3.1, though this is inferred from file paths.
- **LLM calls:**
  - Paper pipeline: extraction plus the update decision (the two-call pipeline).
  - V3: 1 per add.
  - Dream: the decision mechanism is not documented.
  - Read: 0.
- **Results (paper, LoCoMo):** 26% relative LLM-as-judge improvement over OpenAI memory. The graph variant scores about 2% higher overall. Against full context: 91% lower p95 latency and more than 90% token savings.
- **Landable:** yes. We already run V3. Dream's status-plus-link model and its read filter port directly. Its decision logic has to be designed by us.
- **Problems:** (2), (3).

### 2. LightMem
- **Title:** LightMem: Lightweight and Efficient Memory-Augmented Generation. First released 2025-10. ICLR 2026.
- **arXiv:** 2510.18866 (verified). **Code:** github.com/zjunlp/LightMem, MIT, last commit 2026-10-26, no releases. Maintained.
- **Mechanism:**
  - Sensory stage: LLMLingua-2 pre-compresses each input, dropping low-value tokens, into a buffer.
  - When the buffer is full, a hybrid topic segmentation runs. A boundary is kept only where attention-based boundaries (from the compression model) and embedding-similarity boundaries agree.
  - Short-term stage: each topic segment is summarized or extracted into entries, one LLM pass per segment.
  - Long-term stage: entries are inserted at once with soft updates. Each entry keeps an update queue of its top-k most similar earlier entries, and the real merge and update runs offline at sleep time, decoupled from online inference.
- **LLM calls:** about 1 per topic segment online, plus batched offline update calls. Read: 0 for memory.
- **Results (abstract v4, LongMemEval / LoCoMo):**
  - QA accuracy up to +7.7% / +29.3%.
  - Total tokens down up to 38x / 20.9x, and API calls down up to 30x / 55.5x.
  - Online-only cost: 106x / 117x fewer tokens and 159x / 310x fewer API calls.
- **Landable:** partial.
  - The segment buffer, similarity-based boundaries and offline update queue port to Rust.
  - LLMLingua-2 is a token-classification model that our generation, embedding and rerank endpoints cannot serve. Drop pre-compression and use only the similarity half of the boundary rule.
- **Problems:** (3), (2).

### 3. Reproducing LightMem
- **Title:** Reproducing LightMem: When Does Memory Construction Help Over Raw-Turn Retrieval? First released 2026-07. SIGIR-AP 2026.
- **arXiv:** 2607.29104 (verified). **Code:** github.com/ielab/Reproducing-LightMem, MIT, last commit 2026-10-21.
- **Mechanism:** reproduces LightMem and compares it with Naive RAG over raw user turns across several retrievers.
- **LLM calls:** not applicable (an evaluation study).
- **Results:**
  - On the same LightMem store, changing only the retriever moves answer accuracy from 58.1% to 75.5%.
  - At matched retrieval depth, Naive RAG is generally more accurate. LightMem wins under tight answering-token budgets.
  - Oracle evaluation suggests that memory construction can drop or distort information needed for answers.
- **Landable:** evidence only. Keep raw evidence. Constructed memory pays off mainly when the budget is tight, and our hook (5 items) is a tight budget.
- **Problems:** (3), (1).

### 4. SeCom
- **Title:** On Memory Construction and Retrieval for Personalized Conversational Agents. First released 2025-02. ICLR 2025 according to the official README; the arXiv comment names no venue.
- **arXiv:** 2502.05589 (verified). **Code:** github.com/microsoft/SeCom, MIT, last commit 2025-03-01. Dormant.
- **Mechanism:** the memory unit is a topically coherent segment. The paper uses GPT-4 as the conversation segmentation model. LLMLingua-2 compression is applied to memory units as a denoising step that improves retrieval.
- **LLM calls:** LLM (GPT-4) segmentation at write; the call count was not verified. The compression model also runs at write. Read: 0.
- **Results:** segment-level memory beats turn-level, session-level and summary-level memory on LOCOMO and Long-MT-Bench+. The abstract gives no headline number. The segmentation also does well on DialSeg711, TIAGE and SuperDialSeg.
- **Landable:** partial. Segment-level units, yes. LLM segmentation is allowed at write time but costs more than an embedding-based boundary. LLMLingua-2 cannot be served.
- **Problems:** (3), (2).

### 5. LycheeMemory V2
- **Title:** LycheeMemory V2: Efficient Long-Term Memory for LLM Agents via Semantic Segment-Level Consolidation. First released 2026-08. No venue.
- **arXiv:** 2608.12990 (verified). **Code:** none found (the HTML links only to cited repos).
- **Mechanism:**
  - Replaces eager turn-level consolidation, where an LLM runs after every interaction, with segment-level consolidation.
  - Exchanges are batched into segments using semantic boundary detection instead of fixed windows.
  - Each finished segment is encoded into context-independent typed records by one segment-encoding prompt, with bounded cross-segment disambiguation.
  - Retrieval uses lightweight structured indexes plus an LLM query-planning prompt (appendix E.2).
- **LLM calls:** about 1 per segment at write. Read: 1 query-planner call.
- **Results (GPT-4.1-mini):**
  - 89.22% on LoCoMo and 92.20% on LongMemEval-S.
  - Construction tokens 86.0% (LoCoMo) and 75.9% (LongMemEval-S) lower than A-Mem, with no increase in query-time tokens.
- **Landable:** partial. Write-side segment batching and typed records, yes. The query planner breaks our no-LLM-on-read rule. The paper includes an ablation without the query planner; I did not extract its number.
- **Problems:** (3).

### 6. Nemori
- **Title:** the current (v4) title is "What Deserves Memory: Adaptive Memory Distillation for LLM Agents". An arXiv title search for "Nemori" no longer finds it. First released 2025-08. No venue.
- **arXiv:** 2508.03341 (verified). **Code:** github.com/nemori-ai/nemori, license: none detected, last push 2026-04-16.
- **Mechanism:**
  - Episodic integration: an LLM partitions local messages at topic boundaries, writes narrative episodes with temporal anchors, and integrates them with associated memories.
  - Semantic distillation, the predict-calibrate step: from existing knowledge, an LLM writes an anticipatory schema that predicts the episode. Only the prediction error, meaning what existing memory failed to predict, is distilled into new facts. The underlying assumption is that anything already predictable is redundant.
- **LLM calls:** about 6 prompt types per episode (appendix D.1.1 to D.1.6: partitioning, episode generation, candidate identification, integration, schema synthesis, prediction-error distillation). Read: not verified.
- **Results:** the v4 abstract claims strong performance, efficiency and storage reduction but gives no numbers.
- **Landable:** partial. Predict-calibrate is a principled write-time novelty filter, but at several calls per episode it only fits inside a batched background pass.
- **Problems:** (2). It raises the cost in (3).

### 7. SimpleMem
- **Title:** SimpleMem: Efficient Lifelong Memory for LLM Agents. First released 2026-01. No venue.
- **arXiv:** 2601.02553 (verified). **Code:** github.com/aiming-lab/SimpleMem, MIT, last commit 2026-07-24, release v0.3.0 (2026-05-21). Maintained.
- **Mechanism:**
  - Semantic Structured Compression: dialogue is cut into overlapping fixed-length sliding windows. Each window is distilled into compact, multi-view indexed memory units. Entropy-aware filtering drops low-information content such as repetitive logs and chit-chat.
  - Online Semantic Synthesis: an intra-session step merges related units into unified abstract entries on the fly, removing redundancy.
  - Intent-Aware Retrieval Planning: infers search intent to choose retrieval scope and query forms.
- **LLM calls:** about 1 per window plus synthesis at write. Read: at least 1 planner call.
- **Results:** average F1 on LoCoMo up 26.4%. Inference-time tokens down up to 30x.
- **Landable:** partial. Window or segment extraction, the entropy filter and intra-session merging, yes. The read-path planner, no.
- **Problems:** (2), (3).

### 8. LangMem (LangChain; SDK, no paper)
- **Title:** none (an SDK). **arXiv:** none. **Code:** github.com/langchain-ai/langmem, MIT, last commit 2026-10-02 (pushed 2026-10-26), no releases.
- **Mechanism:**
  - Offers in-conversation (hot-path) memory tools plus a background memory manager that extracts, consolidates and updates memories.
  - `ReflectionExecutor` debounces work per thread with `submit(..., after_seconds=delay)`. When a new message arrives, the pending task is cancelled and rescheduled, so extraction runs once over the complete conversation.
  - The docs suggest a 30 to 60 minute delay in production.
- **LLM calls:** 1 manager run per debounced window instead of per message. Read: 0 unless the agent calls the hot-path tools.
- **Results:** none published. The docs justify debouncing by avoiding redundant work, avoiding extraction from incomplete mid-conversation context, and saving tokens.
- **Landable:** yes. It is a per-session tokio timer.
- **Problems:** (3).

### 9. Letta: sleep-time compute and Dreaming
- **Title:** Sleep-time Compute: Beyond Inference Scaling at Test-time. First released 2025-04. No venue.
- **arXiv:** 2504.13171 (verified). **Code:** github.com/letta-ai/sleep-time-compute (MIT, last commit 2025-04-30, a paper artifact) and github.com/letta-ai/letta (Apache-2.0, last commit 2026-10-18, release 0.16.8 on 2026-05-14).
- **Mechanism:**
  - Paper: the model reasons offline about a context before queries arrive and precomputes a re-represented context, whose cost is amortized across related queries.
  - Current Letta docs (the sleeptime page now redirects to "Dreaming"): background helper agents review recent chats, distill lessons and revise memory, which lives in MemFS, a git-backed filesystem.
  - A run is triggered after a set number of completed agent steps or when the context window is compacted. It does not interrupt the active agent.
  - An optional setting has the agent review proposed updates before applying them, which costs extra tokens. The docs name no parameter defaults.
- **LLM calls:** one agent loop per trigger, in the background. Read: 0 extra.
- **Results (paper):**
  - About 5x less test-time compute at the same accuracy on Stateful GSM-Symbolic and Stateful AIME.
  - Up to +13% and +18% accuracy with more sleep-time compute.
  - 2.5x lower average cost per query when amortized across related queries. The benefit correlates with how predictable the query is.
- **Landable:** partial. The triggers (every N steps, on compaction, on idle) port. A full agentic loop is more than we need.
- **Problems:** (2), (3).

### 10. ACE (Agentic Context Engineering)
- **Title:** Agentic Context Engineering: Evolving Contexts for Self-Improving Language Models. First released 2025-10. ICLR 2026.
- **arXiv:** 2510.04618 (verified). **Code:** github.com/ace-agent/ace, Apache-2.0, last commit 2026-08-24.
- **Mechanism:**
  - Context is a playbook of itemized bullets. Each bullet carries metadata: an id and counters of how often it was marked helpful or harmful.
  - The flow is Generator, then Reflector (an LLM that extracts lessons from execution feedback), then Curator (an LLM that emits compact delta entries).
  - Plain code, not an LLM, merges the deltas into the existing context deterministically.
  - Grow-and-refine: redundant bullets are pruned by comparing their embeddings.
  - Incremental deltas avoid both context collapse from repeated full rewrites and brevity bias.
- **LLM calls:** reflector plus curator per task or episode. Merge and dedup: 0. Read: 0.
- **Results:** +10.6% on agent benchmarks and +8.6% on finance. Lower adaptation latency and rollout cost. Matches the top AppWorld production agent using a smaller open model.
- **Landable:** partial, with high fit. Delta updates, non-LLM merge, embedding dedup and helpful/harmful counters all port. The feedback signal has to come from our hook (was an injected memory used?).
- **Problems:** (2), and (1) through the counters.

### 11. MemoryOS
- **Title:** Memory OS of AI Agent. First released 2025-05. EMNLP 2025 main conference according to the README.
- **arXiv:** 2506.06326 (verified). **Code:** github.com/BAI-LAB/MemoryOS, Apache-2.0, last commit 2026-07-07, release V1.2 (2025-07-18).
- **Mechanism:**
  - Short-term memory (dialogue pages) moves to mid-term memory (topic segments) on a dialogue-chain FIFO.
  - Mid-term segments are either promoted to long-term personal memory or deleted according to a segment Heat score. Heat is a weighted sum of retrieval visits, the number of dialogue pages in the segment, and a recency factor.
  - Visits and recency are updated after each retrieval. After a transition the page count resets, so heat decays.
- **LLM calls:** summary and profile updates happen on transitions; I did not verify the exact count. Read: retrieval plus the answer.
- **Results (LoCoMo, GPT-4o-mini):** on average +49.11% F1 and +46.18% BLEU-1 over the baselines.
- **Landable:** partial. The heat score and the promotion/eviction rule are deterministic and port. The three-tier layout duplicates what the agent's own context already holds.
- **Problems:** (2), (1).

### 12. RMM (Reflective Memory Management)
- **Title:** In Prospect and Retrospect: Reflective Memory Management for Long-term Personalized Dialogue Agents. First released 2025-03. ACL 2025.
- **arXiv:** 2503.08026 (verified). **Code:** none found.
- **Mechanism:**
  - Prospective reflection: after a session, an LLM extracts topic-based memories and decides for each one whether to add it or merge it with an existing memory into an updated one.
  - Retrospective reflection: a lightweight reranker is trained online with REINFORCE and Gumbel sampling. The reward is which memories the answering LLM cited in its response.
- **LLM calls:** at write, extraction plus the add/merge decision per session. At read, 0 extra, because citations come from the answer call. The reranker gets gradient updates.
- **Results:** more than 10% accuracy improvement on LongMemEval over the baseline without memory management.
- **Landable:** partial. The prospective add/merge step is the Mem0 pattern. The online RL reranker breaks our no-training rule, but the "was it cited or used" signal can feed counters instead.
- **Problems:** (1), (2).

### 13. Hindsight
- **Title:** Hindsight is 20/20: Building Agent Memory that Retains, Recalls, and Reflects. First released 2025-12. No venue.
- **arXiv:** 2512.12818 (verified). **Code:** github.com/vectorize-io/hindsight, license: none detected by GitHub, last commit 2026-10-28.
- **Mechanism:**
  - Memory is split into four networks: world facts, agent experiences, entity summaries (observations) and beliefs (opinions).
  - Retain: LLM structured extraction (Pydantic schemas), entity resolution, and temporal and causal links.
  - Recall: four-way parallel retrieval (vector, BM25, graph spreading activation, temporal), then reciprocal rank fusion, then a cross-encoder rerank under a token budget. No LLM.
  - Observations are synthesized in the background and rebuilt whenever the facts beneath them change. Opinions are updated by reinforcement during the reflect operation. The paper also has a section on background merging.
- **LLM calls:** at write, at least 1 extraction per retain plus background synthesis. Recall: 0. Reflect: LLM.
- **Results:**
  - With a 20B open model, accuracy rises from 39% (full context, same backbone) to 83.6%.
  - Up to 91.4% on LongMemEval and 89.61% on LoCoMo, against 75.78% for the strongest prior open system.
- **Landable:** partial. The recall path already fits our constraints, and background regeneration of entity summaries is a model for consolidation. Four networks plus spreading activation are more than we need.
- **Problems:** (2).

### 14. SLICE-Mem
- **Title:** SLICE-Mem: Salience-Gated Lightweight Information Curation for Efficient LLM Agent Memory. First released 2026-10. No venue.
- **arXiv:** 2610.33015 (verified). **Code:** none found.
- **Mechanism:**
  - A compact classifier, the Gate, admits only user turns worth keeping.
  - A predicate-centred extractor based on dependency parsing turns admitted turns into short entries.
  - The online path makes no generative LLM calls at all. Reconciliation across entries is deferred to an offline consolidation stage (deterministic alignment plus LLM, Qwen 8B or 30B).
- **LLM calls:** 0 generative calls online; consolidation calls offline. Read: not verified.
- **Results (against MemOS, Qwen3-30B):**
  - FAMA 46.60 vs 46.68 on Memora Weekly.
  - 56.8% vs 59.3% on LongMemEval-S.
  - Total LLM tokens down 92.4% and 99.5%.
- **Landable:** partial. The shape fits us exactly (no LLM online, offline reconciliation). The trained gate and the dependency parser are models our endpoints cannot serve, so they need substitutes such as an embedding novelty gate and entity heuristics.
- **Problems:** (3), (2).

### 15. MemFit
- **Title:** MemFit: Efficient Long-Term Agentic Memory. First released 2026-10. No venue.
- **arXiv:** 2610.00872 (verified). **Code:** none listed; the HTML returns 404, so this row is per abstract.
- **Mechanism:**
  - Every turn is stored verbatim in an append-only store with near-instant, LLM-free insertion.
  - Turns are indexed by segment summaries, which point at the turns and do not replace them.
  - Retrieval is LLM-free and multi-path: lexical plus semantic signals, then a cross-encoder rerank over episodes augmented with captions.
- **LLM calls:** 0 per turn at write. Whether segment summaries use an LLM is not verified. Read: 0.
- **Results:** state of the art on LoCoMo, MemGallery and LongMemEval-S; the abstract gives no numbers. Construction time and cost fall by a multiple, with no exact figure in the abstract.
- **Landable:** yes. This is the published design closest to ours. The difference is that we inject extracted facts, not raw turns.
- **Problems:** (3), (4).

### 16. When Does Selection Replace Extraction? (Jev, pre-registered)
- **Title:** When Does Selection Replace Extraction? A Pre-Registered Test of Agent Memory with a Typed Decision Model. First released 2026-09. No venue (pre-registered on Zenodo).
- **arXiv:** 2609.34227 (verified). **Code:** github.com/ris3abh/Engram, license: none detected, last push 2026-09-29.
- **Mechanism:**
  - Compares two memory designs at matched context budgets on held-out LoCoMo and LongMemEval: raw turns picked by a selector, and LLM-extracted facts.
  - The selector, Jev, is a commercial typed decision model from the vendor TypeSafe.
- **LLM calls:** raw-turn arm: 0 at write, 1 selection call at read.
- **Results:**
  - At a tight budget on LoCoMo, selected raw turns are non-inferior to LLM-extraction memory (one-sided 95% bound of -3.0 points against a -5-point margin).
  - Raw turns are 3,061x cheaper to write.
  - Reranking adds +17.4 (LoCoMo) and +9.1 (LongMemEval) when keeping 3 of 30 candidates, but only +1.5 and +1.1 at generous budgets, where extraction systems are more accurate.
  - With reranking, the system correctly abstains less often.
- **Landable:** evidence, partial. Jev is not available through our endpoints, but the cost and budget findings carry over. The abstention finding is a caution for problem (1).
- **Problems:** (3), (1).

### 17. PreCoMem
- **Title:** PreCoMem: Predictive Cognitive Memory for Self-Evolving Long-Term Dialogue Agents. First released 2026-10. NeurIPS 2026.
- **arXiv:** 2610.29467 (verified). **Code:** github.com/ZhongWishing/PreCoMem, license: none detected, last push 2026-10-12.
- **Mechanism:**
  - Memories are confidence-weighted beliefs.
  - For each turn, an Effective Surprisal score combines semantic deviation from the recalled evidence with an NLI-verifier contradiction score, and is down-weighted when retrieval is diffuse (high entropy).
  - Two thresholds map the turn to one of three deterministic updates:
    - MAINTAIN reinforces a supported belief.
    - PROFILE parks an ambiguous cue as a low-confidence hypothesis until more evidence arrives.
    - CORRECT soft-decays a contradicted fact.
  - Retrieval multiplies relevance by per-item confidence, which works as an implicit reranker and needs no extra LLM call.
- **LLM calls:** update decisions use an NLI model and embeddings, no LLM. Read: 0 extra.
- **Results:** state of the art on LoCoMo, LongMemEval-S and PersonaMem-v2 (no numbers in the abstract). Their DiMoBench shows it adapts to real belief drift without over-reacting to noise.
- **Landable:** partial. The gate and the confidence-times-relevance ranking port. NLI is not one of our endpoint types, so the contradiction flag would have to come from the batched write-time LLM.
- **Problems:** (2), (1).

### 18. Stashbird
- **Title:** Stashbird: Episode-Grounded Lifecycle Reconciliation for Conversational Agent Memory. First released 2026-09. No venue.
- **arXiv:** 2609.34242 (verified). **Code:** none found.
- **Mechanism:** every derived record keeps the set of episodes that support it.
  - When the same evidence appears again, it is added to the existing record's support set instead of creating a new record.
  - Corrections supersede the prior active relation.
  - Deleting an episode removes only the state that has no remaining support.
- **LLM calls:** semantic decisions use an LLM (count not extracted). Reconciliation is deterministic.
- **Results:**
  - Correct final state in 20 of 20 update sequences, against 8 of 20 without reconciliation. It passes 263 deletion interventions.
  - On LoCoMo, 76.4x fewer ingestion prompt tokens than Graphiti and 8.1x fewer retrieval prompt tokens than a reproduced Hindsight.
- **Landable:** yes for the bookkeeping: support sets, supersede links and support-aware delete.
- **Problems:** (2).

### 19. Control-plane placement / ForgetEval
- **Title:** Control-Plane Placement Shapes Forgetting: An Architectural Study of Agent Memory Across Thirteen System Configurations. First released 2026-06. No venue.
- **arXiv:** 2606.15903 (verified). **Code:** github.com/deeplethe/lethe. The abstract says MIT; GitHub detects no license file. Last push 2026-06-16.
- **Mechanism:** compares 13 configurations by where the LLM sits:
  - deterministic primitives only;
  - an LLM at write time (inscribe-time);
  - an LLM hook at mutation time that runs on supersede, release and purge.
- **Results:**
  - Deterministic primitives fail canonicalization: 5% on identifier obfuscation and 0% cross-lingual.
  - The write-time LLM reaches 100% on canonicalization but 0% on intent-aware deletion.
  - The mutation-time hook reaches 78–85% on intent-aware deletion and 91.7–93.2% overall. It costs $0.17 per 385-case run and adds 2.3 s per case of mutation latency, against 64–191 ms for deterministic primitives. The recall path is unchanged.
  - The abstract's summary of production behaviour: "Production failures are predominantly forgetting failures rather than recall failures."
- **Landable:** yes, as a design rule. Spend the LLM at write and mutation time and keep recall deterministic.
- **Problems:** (2).

### 20. RSPM: Let Them Sleep
- **Title:** Let Them Sleep: Recursive Self-Pruning Memory for Long-Horizon LLM Agents. First released 2026-10. BayLearn 2026, oral.
- **arXiv:** 2610.25561 (verified). **Code:** none found.
- **Mechanism:**
  - A sleep runs every 10 episodes or on a failure spike.
  - It clusters recent failures, distills one rule per cluster with an LLM, and deletes the raw episodes it learned from.
  - Rules decay with a half-life unless they are reinforced.
  - A rule is promoted to category level once it holds in 3 contexts, and to universal at 5.
- **LLM calls:** 1 per cluster per sleep. Read: 0.
- **Results:**
  - +9.1 points on a SWE-bench simulation, +11.9 on temporal consistency across 12 conversational benchmarks, and +7.6 on LoCoMo-10.
  - About 93% fewer retry loops and about 30% fewer stored entries.
- **Landable:** yes (scheduler, clustering and decay). This is a short workshop paper, so the evidence is weak.
- **Problems:** (2), (3).

### 21. Tether (dependency-tethered memory)
- **Title:** Memories That Outlive Their Reasons: Dependency-Tethered Memory for Tool-Using LLM Agents. First released 2026-10. No venue.
- **arXiv:** 2610.25123 (verified). **Code:** none found.
- **Mechanism:**
  - At write time, each stored verdict records the exact versions of the premises it was derived from.
  - At read time, any memory whose support has failed anywhere in the store is withdrawn deterministically, even when the invalidating event never shows up in the same retrieval window.
- **LLM calls:** at write, extraction must also capture dependencies. Read: 0.
- **Results:**
  - Share of upstream-failing objects the agent still published: Tether 0.3%, Mem0 42%, A-Mem and Graphiti 9%.
  - 37% fewer verification calls than checking everything.
  - Degrades to 9.4–22.1% when dependencies arrive in separate sessions.
- **Landable:** yes, for code facts. Tether a memory to its file path and content hash or commit, and withdraw or flag it when the hook reports the file has changed. This requires the hook to pass repo state.
- **Problems:** (2), (1).

### 22. Multi-factor memory value model
- **Title:** Learning What to Remember: A Cognitively Grounded Multi-Factor Value Model for Agentic Memory. First released 2026-06. No venue.
- **arXiv:** 2606.12945 (verified). **Code:** github.com/zhibao-dev/Learning-Multi-Factor-Memory, license: none detected, last push 2026-06-30.
- **Mechanism:**
  - Memory value is V(m) = sum of w_i * f_i(m) over seven factors: emotional intensity, goal relevance, value alignment, self/user relevance, task utility, reliability and usage history.
  - The weights are fitted by a gradient-free optimizer against downstream retention of gold evidence.
  - The single scalar drives encoding depth, forget risk and retrieval rank.
- **LLM calls:** the authors report CPU-only experiments with no API calls. How the factors are scored was not extracted.
- **Results (LongMemEval, blind regime):** keeps 0.770 ± 0.011 of the gold evidence, against 0.657 with uniform weights, 0.518 for the best single factor and 0.368 for recency. The learned weights favour reliability, emotional intensity and self/user relevance.
- **Landable:** partial. Fitting 7 weights is calibration, not model training, but it needs a labelled objective. For us that would be memories later used versus never used.
- **Problems:** (2), (1).

## Also checked (opened; short rows)

| Name | arXiv (first) | Venue | Code | Relevance and verdict |
|---|---|---|---|---|
| Mem++ | 2610.02002 (2026-10) | none | AIDAChip-Inc/mem-plus-plus (license: none detected; 2026-10-26) | Stores whole documents with date and author and makes no generative-model calls at write. At read it filters by date and fuses lexical and semantic rankings. Best average LLM-judge score on LoCoMo; second on LongMemEval-S, behind its own entity-graph variant. Evidence for an LLM-free raw store. |
| Naive RAG Is Stronger Than We Thought | 2610.30755 (2026-10) | none | not checked | Raw-RAG (dense retrieval plus rerank over raw history) ranks first in 6 of 7 benchmark-model settings on LoCoMo, LongMemEval, MEME and MemoryAgentBench, by 0.13–5.53 points. Even with full access to the transformed memory store, accuracy stays below what the raw evidence gives. Evidence. |
| Do Conversational Agents Need Explicit Memory? | 2610.24671 (2026-10) | none | not checked | Compares six explicit memory systems with raw-history RAG on LongMemEval-S and BEAM-100K. Gains are only local (some question types, tight budgets), and construction takes hours per conversation. Evidence. |
| MemoryTax | 2610.29823 (2026-10) | none | not checked | Uses one LoCoMo conversation and one run per cell. Mem0 beats full context only with GPT-4o-mini, and its build alone costs more than a full-context rollout. Memory quality is capped by the model that writes it; a stronger reader cannot recover what was left out. Weak but pointed evidence. |
| Retain or Consolidate? | 2607.17545 (2026-07) | none | not checked | On LongMemEval, consolidation improves absolute accuracy by up to 48% under tight budgets, while keeping raw records wins under loose budgets. Cross-note merging and abstraction beat local rewriting. Supports consolidating for our tight hook budget. |
| Supersede | 2606.27472 (2026-06) | none | Vrin-cloud/supersede (not checked) | On LongMemEval knowledge updates, a bounded, self-maintained memory drops accuracy from 92% to 77% even with gpt-5.4, and more memory does not recover it. The fix offered is a GRPO environment, so: evidence only, training rejected. |
| Manufactured Confidence | 2606.29279 (2026-06) | none | collapseindex/manufactured-confidence (license: none detected) | When memory products rewrite a hedged remark as a flat fact, agents later obey it as if it were verified. The fix in the store is to keep the tentative phrasing; one redundant source restores correct decisions. Use as an extraction-prompt rule. |
| What Eviction Destroys | 2609.08279 (2026-09) | none | not checked | Most eviction losses are irreversible: an irreversible share of 0.67–0.73 for FIFO, random and redundancy-aware eviction, and 0.60 for LLM importance, at an 80k budget; 1.00 at 8k. Argues for archiving instead of deleting. |
| Gated Memory | 2610.11270 (2026-10) | none | none found | An admission gate runs before extraction and checks: permanent attribute or transient situation, stated or inferred, scope, and grounded time and place. +2.6% relative LLM-judge on LoCoMo-10. Port as rules in the extraction prompt. |
| AMU | 2609.36976 (2026-09) | NORA 2026 workshop | UnicusT11/AMU-memory (license: none detected) | Admission plus a small-model choice between store, discard as duplicate, and fuse as update. Controlled evaluation only. Shares mechanism 2. |
| ConsistencyGate | 2607.22962 (2026-07) | none | not checked | Admits a fact only if the average of K LLM support scores clears a threshold; a log-prob variant needs one forward pass. Reduces contamination; the cost falls on implicitly stated facts. Optional if our endpoint returns logprobs. |
| CASK (scope-aware commit rules) | 2610.09008 (2026-10) | NeurIPS 2026 workshop LP4FM | not checked | Commit only shared-world facts; plans, simulated tool results and others' beliefs stay scoped. The model-internal signal does not port; the rule does, as a prompt and typing rule. It matters for coding transcripts. |
| Dual-Layer Agentic Memory | 2608.22215 (2026-08) | none | promised after acceptance | Routes each item to non-write, write-new or write-update through a 1.7B-to-8B cascade with a learned escalation gate. Prunes up to 68% of redundant memory while escalating under 50% of inputs and keeping over 98% of EM. The SFT write-back breaks our no-training rule. Split verdict: the small-first triage idea ports; the gate and SFT do not. |
| MemoryLACE | 2609.03201 (2026-09) | none | not checked | Sparse merge, supersession and contradiction relations among atomic memories with provenance. Runtime on BEAM is 66.6% lower than Hindsight. HTML fetch failed, so per abstract. |
| EngramRAG | 2609.32049 (2026-09) | none | bpoti001/EngramRAG (license: none detected) | Usage-modulated PageRank, decay scaled by load-bearing weight, and a SUPERSEDES DAG. The SUPERSEDES DAG cut contradictory old-and-new answers from 70% to 0%. LoCoMo Recall@5 is 53.21% vs 38.29% for dense retrieval. |
| REM (coding agents) | 2610.27377 (2026-10) | NeurIPS 2026 workshop CLEA | not checked | Stores a phase-structured document about 100x smaller than the trajectory, and holds back the costly implementation record until an in-loop check sees the agent stuck, then evicts it after one exposure. SWE-ContextBench Lite: 27.6% vs 24.9% resolved at 17% lower cost. Needs in-loop signals we do not have. |
| Stratum (coding agents) | 2610.36153 (2026-10) | none | Zenodo package | A sketch of repository memory. 21 of 24 vs 6 of 24 root-cause identifications, at 4.5x lower token cost. Supports project-scoped memory. |
| ReasoningBank | 2509.25140 (2025-09) | ICLR 2026 | google-research/reasoning-bank (Apache-2.0; 2026-05-19) | Distills strategies from successes and failures that the model judges itself. Write-time LLM is fine, but MaTTS needs multiple rollouts. Partial, for procedural memory only. |
| PREMem | 2509.10852 (2025-09) | EMNLP 2025 Findings | sangyeop-kim/PREMem (license: none detected; 2025-09-10) | Moves reasoning to storage time: fragments typed as factual, experiential or subjective, with cross-session relations (extension, transformation, implication). More write cost, so a background-only option. |
| MOOM | 2509.11860 (2025-09) | none | not checked | Forgetting inspired by competition-inhibition caps memory growth, with fewer LLM invocations. Role-play domain. |
| Mnemosyne | 2510.08601 (2025-10) | none | not checked | Substance and redundancy filters, commit and prune, and recall with temporal decay and refresh. 54.6% average on LoCoMo. Edge or healthcare domain. |
| SEDM | 2509.09498 (2025-09) | none | not checked | Admission by paired execution tests. Calibrated Memory Gating injects memory only when the predicted gain beats the context cost (18–31% fewer input tokens). Relevant to (1), but needs outcome labels. |
| Hippocam (Use and Disuse) | 2610.12124 (2026-10) | none | not checked | Recursive prefix consolidation: unused experience becomes progressively more abstract, and recalled experience is reconsolidated. No numbers in the abstract. |

## Mechanisms worth porting

Ordered by expected payoff under our constraints: no LLM on the read path, no new service, no training.

### M1. LLM-free capture, then debounced segment-level extraction
- **What we would build:**
  - Append every captured turn verbatim to an episode log in SurrealDB, with session, time and provenance. Embed it, which retrieval already does. No LLM call.
  - Flush a segment to the extraction LLM when any one of these happens:
    - the cosine between the new turn and the running segment centroid drops below a boundary threshold (the similarity half of LightMem's rule; LycheeMemory's semantic boundaries);
    - the session goes idle for T (LangMem's cancel-and-reschedule debounce);
    - the segment hits a token cap, or the session ends.
  - Make one extraction call per segment.
- **Cost removed:** one LLM call per turn becomes one per segment.
  - LightMem: API calls down up to 30x on LongMemEval and 55.5x on LoCoMo overall, and 159x / 310x online.
  - LycheeMemory V2: construction tokens 86.0% and 75.9% lower than A-Mem while reaching 89.22% on LoCoMo and 92.20% on LongMemEval-S.
  - The Jev study puts raw-turn writes at 3,061x cheaper than extraction.
- **Why keep the raw log:** the Jun–Oct 2026 controlled studies (2610.30755, 2610.24671, 2607.29104) find that raw-turn retrieval matches or beats constructed memory at generous budgets. Constructed or consolidated memory wins at tight budgets (2607.17545: up to +48% absolute; 2607.29104; 2609.34227). Our hook injects at most 5 items, which is a tight budget. So: keep injecting compact facts, and keep the raw segments as provenance and for re-extraction.
- **Caveats:**
  - A session's memories become retrievable only after its flush. That is acceptable, because the live session is already in the agent's context.
  - The embedding-only boundary rule has not been evaluated on its own in these papers. That part is my inference.

### M2. Single-call reconciliation with non-destructive status links
This restores Mem0's dropped UPDATE/DELETE without a second LLM call.
- **What we would build:**
  - Inside the M1 segment call, include the top-k existing active memories nearest the segment, with their IDs. They come from the cosine pool we already compute.
  - Ask the LLM to label each output fact `new`, `duplicate_of(id)`, `refines(id)` or `supersedes(id)`.
  - Apply the labels deterministically:
    - duplicate: add the segment to the existing memory's support set and increment its support count, with no new row (Stashbird).
    - refines: insert the fuller fact and mark the old one `merged`, hidden from reads (Mem0 Dream's Merge).
    - supersedes: mark the old one `superseded` with a link to the new one. The hook injects only `active` memories (Dream's `latest_only`).
    - new: insert.
  - Keep exact content-hash dedup. Add a high-cosine near-duplicate pre-check so verbatim restatements never reach the insert step.
  - Ask the extractor to keep hedges and to mark facts as stated or inferred (Manufactured Confidence; Gated Memory).
- **Cost removed:** no extra LLM call, since this rides on the M1 call, and no store growth from restatements. The read path changes only by a status filter.
- **Evidence:**
  - Mem0 dropped its two-call pipeline for cost in V3, then put Supersede and Merge back, running on add, only in its closed Dream layer. OSS 3.3.1, which we follow, has neither.
  - Stashbird: 20 of 20 correct final states vs 8 of 20 without reconciliation, and 76.4x fewer ingestion tokens than Graphiti.
  - ForgetEval: an LLM at mutation time reaches 91.7–93.2% where deterministic primitives fail, and recall is untouched. Its abstract finds that production failures are mostly forgetting failures.
  - Supersede: maintenance, not comprehension, costs 15 points (92% to 77%) on knowledge updates.
  - EngramRAG's SUPERSEDES DAG: contradictory old-and-new answers from 70% to 0%.
  - PreCoMem's PROFILE state suggests a low-confidence holding status for ambiguous facts.
- **Caveat:** the LLM sees only the k nearest neighbours, so it misses invalidations that are not local. M3 and M5 cover those.

### M3. Background consolidation ("dream") pass, off the hot path
- **What we would build:**
  - A tokio job that runs every N new memories per scope, on idle, or nightly.
  - It clusters active memories by embedding and shared IDENTIFIER entities.
  - For each cluster above a size or similarity threshold, it makes one LLM call to:
    - (a) fold duplicates into a canonical memory, marking the others `merged`;
    - (b) resolve contradictions the write path missed, marking the losers `superseded`;
    - (c) optionally write a pattern memory linked to its sources (Mem0 Synthesis).
  - Apply results with deterministic delta merge and embedding dedup (ACE's non-LLM curator merge).
  - Never hard-delete: archive. "What Eviction Destroys" finds 60–100% of eviction losses irreversible.
  - Optionally decay unreinforced memories and promote patterns seen in 3 or more contexts (RSPM).
- **Cost removed:** duplicate checking leaves the per-turn path and runs in batches. The store shrinks: RSPM reports about 30% fewer entries, and Dual-Layer prunes up to 68% of redundant memory while keeping over 98% of EM.
- **Evidence:**
  - LightMem's offline update queue underlies its 159x / 310x online API-call reduction.
  - Hindsight regenerates observations in the background.
  - Letta's Dreaming triggers on N steps or on compaction.
  - Sleep-time compute's amortization: 2.5x lower cost per query.
  - Retain-or-Consolidate: cross-note merging beats local rewriting under tight budgets.
- **Caveat:** consolidation can manufacture confidence (2606.29279). Keep hedges and source links, and write merges additively with provenance.

### M4. Usage feedback plus a value (heat) score, used as a ranking prior and for archiving
- **What we would build:**
  - When the hook injects memory m, log it. When the next assistant turn is captured, mark m as used if the response hits one of its IDENTIFIER or QUOTED entities, or if its cosine to a response sentence clears a threshold. This overlap test is my heuristic; RMM uses LLM citations instead.
  - Keep `injected`, `used` and `last_used` counters and the support count from M2.
  - Compute V = w · [used/injected, recency, support, stated-vs-inferred reliability, ...]. This combines MemoryOS heat (visits, page count, recency), ACE's helpful/harmful counters and the structure of the 7-factor value model.
  - That paper fits its weights gradient-free against retention of gold evidence on LongMemEval. Our proposed target is different: whether a memory was later used. That target is untested.
  - Use V to:
    - multiply relevance in fusion, as PreCoMem does with confidence, at no LLM cost;
    - archive memories with many injections and zero uses;
    - choose which clusters M3 processes first.
- **Cost removed:** no LLM. Fewer repeat injections of useless memories, which also helps problem (1). SEDM saved 18–31% of tokens by gating injection on expected gain.
- **Evidence:**
  - RMM: more than 10% on LongMemEval from retrospective reflection. That used an online RL reranker; counters are our untested substitute.
  - The value model keeps 0.770 of gold evidence vs 0.368 for recency.
  - MemoryOS heat eviction: +49% F1 on LoCoMo for the full system.
- **Caveat:** use detection by overlap is a heuristic. Validate it on our own captured transcripts before letting it drive archiving.

### M5. Admission and commit rules at extraction, plus dependency tethering for code facts
- **What we would build:**
  - In the M1 prompt, require each fact to be durable rather than transient, tagged stated or inferred, and scoped to user, project or session.
  - Have the prompt drop provisional content: plans that were considered, simulated or expected tool outputs, rejected options, and other speakers' beliefs. These are the CASK commit rule and Gated Memory's admission checks, both common in coding transcripts.
  - Skip a segment before any LLM call when it is not novel: its max cosine to active memories is above a threshold and it contains no new IDENTIFIER entity. This applies SLICE-Mem's no-LLM-online principle with a heuristic in place of its trained gate.
  - For code facts, store the file path(s) and the content hash or commit they rest on (Tether). The hook passes HEAD or changed paths, and the read path drops or flags tethered memories whose support changed. Deterministic, no LLM.
- **Cost removed:** extraction calls on non-novel segments. SLICE-Mem cut LLM tokens by 92.4–99.5% at near-par accuracy, though with a trained gate. Stale code facts get withdrawn: Tether published 0.3% of upstream-failing objects vs 42% for Mem0.
- **Caveat:** Gated Memory's measured gain is small (+2.6% relative). The novelty gate is untested in these papers; that part is my inference.

### Note on problem (4), entity projection
Among write-side papers, the strongest LLM-free systems (MemFit, Raw-RAG, the Mem++ base) reach top results using lexical plus dense retrieval with a cross-encoder and no per-entity embeddings. Mem++'s entity-graph variant did score higher on LongMemEval-S, so entity signals carry some value.

My inference, not tested in any paper: keep IDENTIFIER and QUOTED entities as exact-match keys for BM25 and the boost, which is cheap. Stop embedding every PROPER or TOPIC entity at write time. Compute entity embeddings in M3 only for entities shared by 2 or more memories.

## Rejected and why
- **Memory-R1** (2508.19828, 2025-08): the memory manager (ADD/UPDATE/DELETE/NOOP) and the answer agent are fine-tuned with PPO and GRPO. Requires training; no official checkpoint verified.
- **Mem-α** (2509.25911, 2025-09): a memory-construction agent trained with RL. Repo wangyu-ustc/Mem-alpha (license: none detected; last commit 2025-12-25). Requires training; no checkpoint verified.
- **MemDream** (2609.34545, 2026-09): a GRPO-trained repair policy plus three agents per dream cycle. Requires training. Its soft-decay idea is already covered by M3 and M4.
- **Hindsight Memory-PRM** (2608.29605, 2026-08): trains a memory-utility critic and an 8B policy. Requires training. Its insight that retrieval hits and answer citations form an audit trail informs M4.
- **Supersede** (2606.27472): a GRPO environment. Kept only as evidence.
- **Zep / Graphiti** (2501.13956, 2025-01): a temporal knowledge graph that keeps the history of relationships, with LLM-driven entity and edge processing. It needs a graph database, which is a new runtime service. DMR 94.8% vs 93.4% for MemGPT; LongMemEval up to +18.5% accuracy with 90% lower latency. Repo getzep/graphiti (Apache-2.0; v0.31.2, 2026-10-20). Only the "supersede, do not delete" idea is kept, through M2.
- **MemOS** (2507.03724 and 2505.22101, 2025-05/07): a memory OS spanning plaintext, activation (KV) and parameter memories through MemCubes, scheduling and governance. Far beyond a single daemon, and parameter memory implies training. Repo MemTensor/MemOS (Apache-2.0; v2.0.34, 2026-09-23).
- **MIRIX** (2507.07957, 2025-07): six memory types run by a multi-agent framework that coordinates both updates and retrieval, which puts agents on the read path. LOCOMO 85.4%. Repo Mirix-AI/MIRIX (Apache-2.0).
- **EverMemOS** (2601.02163, 2026-01): segment boundaries are detected by prompting an LLM, which is fine at write. But retrieval is an agentic loop guided by MemScenes, with an LLM checking whether the context is sufficient: an LLM on the read path. The repo has been renamed to EverMind-AI/EverOS (Apache-2.0; v1.5.1, 2026-10-22).
- **A-MEM** (2502.12110, NeurIPS 2025): each write makes several LLM calls (note construction, link generation, and an evolution step that rewrites older notes), which raises exactly the cost we are trying to cut. LycheeMemory uses it as the expensive construction baseline. Code at github.com/WujiangXu/A-mem (license: none detected; pushed 2026-10-22) and agiresearch/A-mem. The A-mem-sys repo named in the paper was not re-verified.
- **MemInsight** (2503.21760, 2025-03): LLM-generated attribute augmentation of every memory. +34% recall over RAG on LoCoMo, but write-heavy, and it does not address our problems. Repo amazon-science/MemInsight (license NOASSERTION; 2025-09-15).
- **Rejected sub-mechanisms:**
  - LightMem and SeCom's LLMLingua-2 compression: a token-classification model our endpoints cannot serve.
  - SimpleMem and LycheeMemory's read-time query planners: LLM on the read path.
  - Dual-Layer's SFT write-back: training.
  - RMM's online REINFORCE reranker: training.
  - SLICE-Mem's trained gate and dependency parser: these would be new in-process models; replaced by heuristics in M5.

## Gaps and caveats in this survey
- Nemori v1's original title was not fetched.
- I did not open the Mem0 Dream decision model, which is not documented, or the Mem0 changelog date.
- MemFit and MemoryLACE details are per abstract only.
- Letta's Dreaming page gives no parameter names or defaults.
- SeCom's segmentation call count was not verified.
- I did not extract the ablation numbers for running without LightMem's pre-compression or without LycheeMemory's query planner. Those are needed to know how much accuracy the parts we cannot port contribute.
