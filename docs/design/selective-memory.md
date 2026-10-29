# Selective recall and lean formation

> Status: implemented on branch `claude/selective-memory`. Evidence and the survey behind it, in Chinese:
> [evaluation report](../../eval/docs/benchmarks/selective-memory.md). Date: 2026-10-29.

## 1. Problem

Three paths put memory into an agent's context, and none of them asked whether the memory was relevant:

- **UserPromptSubmit** embeds every prompt, searches, and injects up to five memories. The search keeps candidates
  above a cosine of 0.1 (Mem0's default), which every candidate clears, so a prompt about anything gets five
  memories. In the live recall log, 17 of 29 searches returned all 15 candidates the hook asks for.
- **`search_memories`**, called by the agent, returns the same top-k.
- **SessionStart** injects the newest facts by time.

A cosine cutoff cannot fix this: cosine levels shift with the embedding model, the language and the query's form, and
the fused hybrid score is not comparable across queries at all (its divisor depends on whether any document matched
the keywords or entities).

Formation has the opposite problem. Every captured turn costs an extraction call that only adds facts; duplicates
and low-value facts accumulate; and the entity projection embeds every entity it extracts, most of them TOPIC word
runs (546 entity rows for 67 memories in the live store).

## 2. Read path: selective recall

With a reranker loaded (now the default), `search_memories` (and through it the hook):

1. embeds the query and recalls the semantic pool as before;
2. fuses cosine and BM25 (no entity boost) and reranks the top 20 in one call;
3. returns the memories whose reranker score clears the reranker's **floor** and lies within `mem0RecallMargin`
   (3) **logits** of the best, in reranker order, within topK and the token budget;
4. returns nothing when no candidate clears the floor (`gap: true`), and records `selection` (floor, margin, how many
   were reranked, the best score, how many cleared) in the response and the recall log.

The pure rule is [selection.rs](../../crates/memory-rules/src/selection.rs).

**Why a reranker floor and not a score-shape gate.** The survey's strongest LLM-free gates use the shape of the score
list (Gap@k, NQC; *Magnitude Mirage*, EMNLP 2026). They predict whether retrieval *found* an existing relevant
document. Our question is whether *any* relevant memory exists, and on it the reranker's best score separates
answerable from unanswerable queries far better (AUROC 0.97–0.98 over the LoCoMo calibration and report queries,
against 0.93 for the best cosine, 0.85 for Gap@25 and 0.49 for a corpus-standardized cosine). Largest-gap truncation (Adaptive-k) cut recall too much at 20 candidates, and read-time
near-duplicate suppression (MMR) cost recall for three tokens a query; neither is used.

**Floors are per reranker.** bge-reranker-v2-m3 returns a sigmoid of a logit; the hosted Qwen reranker a yes/no
softmax. Both lie in 0..1 but not on one scale: a floor of 0.3 injects for 1.8% of unrelated questions with bge and
41% with Qwen. Built-in floors were chosen on a LoCoMo calibration split by a fixed rule (the best hit rate whose
false-injection rate stays within 10%) and reported on a separate split: bge-reranker-v2-m3 0.30 (any quantization),
qwen3.7-text-rerank 0.72. Any other reranker needs `mem0RecallFloor`; startup fails without it rather than guess.
`eval/selective_recall.py` measures a floor for a new reranker (§6).

**Failure and fallback.** The reranker now vouches for what search returns, so its failure fails the search
(`MODEL_UNAVAILABLE`); the hook then injects nothing, which is its rule for every failure. Without a reranker
(`mem0Rerank:false` or `--no-reranker`), or when a request sends `rerank:false`, search keeps Mem0's hybrid top-k
with the entity boost, unchanged, and the daemon logs a warning at startup.

**SessionStart.** The hook asks `get_context` for its `lean` form: the explicit instructions and a one-line index
(how many project and shared facts exist). Per-prompt recall brings the facts a prompt needs; this is the shape of
Claude Code's auto memory (an index loaded at start, topics on demand). An agent that calls `get_context` itself
still gets every section.

## 3. Write path: lean formation

- **Segments.** One extraction call takes several queued turns of a session
  ([segments.rs](../../crates/memory-rules/src/segments.rs)): up to `mem0SegmentTurns` (4 by default), or fewer once
  their messages reach `mem0SegmentTokens`. A segment short of that is formed
  when the session's end marker arrives, when its oldest turn has waited `mem0SegmentWaitSecs`, when a caller waits
  for one of its requests (`sync`), or by `wait_idle`. A segment that cannot form yet, or failed, holds back its
  session's later segments, so a turn is never formed with context missing an earlier one. Facts name every request
  and turn of their segment. This is LightMem's and LycheeMemory's segment-level extraction and LangMem's debounce,
  without their compression models.
- **Turns in one prompt.** A segment of several turns lists its new messages under `[Turn i of n]` lines and adds a
  Turns note at the end of the prompt: extract from each turn what it would give alone, read every message of every
  turn, and count what a named speaker says about their own life in either role (the vendored prompt's multi-speaker
  rule, restated where a long input dilutes it). Given four turns as one run of messages, the model had passed over
  most of what the assistant-role speaker said about themselves; with the markers and the note, 4-turn segments
  covered as many LoCoMo questions as per-turn formation (§5). A single turn's prompt is unchanged byte for byte.
- **Recently Extracted Memories.** The vendored prompt asks for the facts already extracted in this session as its
  primary deduplication reference; the SDK sends an empty list. Formation now sends the current facts of the
  session's latest formed requests (up to 20).
- **Named entities only.** The projection keeps PROPER, QUOTED and IDENTIFIER rows. TOPIC rows (lowercase word runs)
  were most rows, each embedded at write time, and the only consumer of their scores, the entity boost, is no longer
  on the selective read path. Changing the rule version rebuilds existing projections.

**Not done, by evidence.** Write-time semantic deduplication: an LLM judge found that a third of the later facts in
pairs at cosine ≥ 0.95 add a detail (a changed date, a reason), so dropping them by similarity loses information.
Duplicates are left to the extraction prompt and to consolidation, which merges with a model call and keeps both
inputs' provenance.

## 4. Configuration

| Key | Default | Notes |
|---|---|---|
| `mem0Rerank` | `true` (was `false`) | Loads the reranker; selective recall depends on it |
| `mem0RecallFloor` | per reranker | Required for rerankers without a built-in floor |
| `mem0RecallMargin` | 3.0 | Logits below the best a returned memory may be |
| `mem0SegmentTurns` | 4 (was 1) | Turns one extraction call forms; 1 forms every turn alone (§5) |
| `mem0SegmentTokens` | 6000 | |
| `mem0SegmentWaitSecs` | 600 | |

An install whose configuration says `"mem0Rerank": false` keeps the old behaviour; `examples/service-config.json`
now says `true`.

## 5. Results

LoCoMo, five conversations, calibration on two and report on three, labels from an LLM over pooled candidates; the
[evaluation report](../../eval/docs/benchmarks/selective-memory.md) has the protocol, every number and its limits.

- **Recall** (bge-reranker-v2-m3, report split): questions about other conversations get memory 1.8% of the time
  instead of always, name-swapped hard negatives 20.8%; a store's own questions are hit as often as before (0.886
  against 0.884) with 65% fewer injected tokens and precision 0.55 instead of 0.19. Answer accuracy with the injected
  memories is unchanged (J 0.681 against 0.665, McNemar p = 0.40) with 2.4 memories a question instead of 5. On 25
  hand-labelled prompts from the live recall log, the hosted Qwen reranker injected nothing for the 12 that no memory
  answers, against five memories each before, and missed 3 of 13 answerable ones (Chinese questions about English
  memories).
- **Formation**: per-turn formation with Recently Extracted Memories cut memories with a near duplicate from 17.4% to
  9.9% and entity rows by 97% with no coverage loss. Segments of 4 or 8 turns, given as one run of messages, cut
  extraction calls by 72% or 83% but lost facts about the conversation's second speaker (the "assistant" role):
  own-question coverage fell from 94.9% to 88.7% or 82.0%, and answer accuracy fell significantly. Most of the loss
  came from the evaluation's custom instructions, whose rule "extract memories only from user messages" fits Mem0's
  harness (it forms each LoCoMo conversation once per speaker) but not this one (once, the first speaker as user):
  per-turn formation extracts the second speaker's facts anyway, four turns at once mostly do not. With turn
  markers and the Turns note, 4-turn segments covered 96.2% of the report split's questions (per turn 94.9%) and
  answered with J 0.707 (0.673, McNemar p = 0.085) at 29% of the extraction calls and 36% of the cost, so
  `mem0SegmentTurns` now defaults to 4, by a rule written down before the measurement. Eight-turn segments were not
  measured again.

## 6. Calibrating another reranker

Form the LoCoMo stores once (`eval/selective_recall.py --stage form`), search them with `explain` (`--stage search`),
label the pooled candidates (`--stage label`), score them with the new reranker the way the `hosted` stage scores
Qwen, and replay the rule (`--stage simulate`). Use the floor with the best calibration hit rate within the
false-injection ceiling you accept, then check it on the report split and on your own prompts.

## 7. Next

- Topic-boundary segmentation (embedding similarity between a turn and its segment), and admission rules for coding
  sessions (no plans, tool output or rejected options as facts), each needing coding transcripts to evaluate.
- Usage feedback: count whether an injected memory was used in the agent's next reply and fold it into SessionStart's
  index and consolidation's order (ACE's helpful/harmful counters, MemoryOS's heat).
- Scheduled consolidation ("sleep time"), within the existing per-run limits, now that formation adds fewer facts.
