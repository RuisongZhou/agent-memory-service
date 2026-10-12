"""Answer-record helpers shared by the answer/judge CLI, replay and migration tools.

Prompt rendering for a context record, the fingerprint that ties an answer to its exact rendered
prompt, and per-cohort quality statistics.
"""
from __future__ import annotations

import hashlib
import json

from . import prompts
from .metrics import mean, percentile


def build_answer_prompt(rec: dict, variant: str) -> str:
    if variant == "unified":
        return prompts.render_unified(rec["context"], rec["question"])
    if "speaker_1_memories" not in rec:
        raise SystemExit("--answer-prompt mem0-original needs mem0 records (speaker_1_memories / speaker_2_memories)")
    return prompts.render_mem0_original(rec["speaker_1"], rec["speaker_1_memories"], rec["speaker_2"],
                                        rec.get("speaker_2_memories", []), rec["question"])


def fingerprint(rec: dict, variant: str) -> str:
    value = {k: rec.get(k) for k in ("system", "setting", "qid", "question", "answer")}
    value["prompt"] = build_answer_prompt(rec, variant)
    return hashlib.sha256(json.dumps(value, sort_keys=True).encode()).hexdigest()


def summary_stats(rows: list[dict]) -> dict:
    lat = [r["search_ms"] for r in rows if r.get("search_ms") is not None]
    ctx = [r["context_tokens"] for r in rows if r.get("context_tokens") is not None]
    rnd = lambda x, n=4: None if x is None else round(x, n)  # noqa: E731
    return {
        "n": len(rows),
        "J": rnd(mean(r["J"] for r in rows)),
        "F1": rnd(mean(r["f1"] for r in rows)),
        "BLEU1": rnd(mean(r["bleu1"] for r in rows)),
        "context_tokens_mean": rnd(mean(ctx), 1),
        "n_memories_mean": rnd(mean(r["n_memories"] for r in rows if r.get("n_memories") is not None), 2),
        "n_turns_mean": rnd(mean(r["n_turns"] for r in rows if r.get("n_turns") is not None), 2),
        "search_ms_mean": rnd(mean(lat), 1),
        "search_ms_p95": rnd(percentile(lat, 95), 1),
    }
