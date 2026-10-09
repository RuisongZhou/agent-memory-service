#!/usr/bin/env python
"""Prepare a paid pilot manifest and optional measured mem0 cost extrapolation; never calls APIs."""
from __future__ import annotations

import argparse
import json
import re
from pathlib import Path

import costs
from common import locomo
from common.llm import CNY_PER_USD


def build(conversation="conv-26", sessions=2, questions=10, *, ledger_rows=None, add_component="mem0-pilot.add"):
    data = locomo.load()
    conv = locomo.select(data, [conversation])[0]
    chosen = locomo.sessions(conv)[:sessions]
    session_ids = {s.n for s in chosen}
    eligible = []
    for qa in locomo.qas(conv):
        matches = [re.fullmatch(r"D(\d+):\d+", str(e)) for e in qa.evidence]
        if matches and all(m and int(m.group(1)) in session_ids for m in matches):
            eligible.append(qa)
    if len(eligible) < questions:
        raise ValueError(f"only {len(eligible)} questions have all evidence in the selected sessions")
    selected = eligible[:questions]
    pilot_turns = sum(len(locomo.turns(s)) for s in chosen)
    conv_turns = len(locomo.all_turns(conv))
    all_turns = sum(len(locomo.all_turns(c)) for c in data)
    rows = costs.merge_rows(ledger_rows or [])
    measured = costs.summarize_adds(rows, add_component)
    extrapolation = None
    if measured["usd_per_add"] is not None:
        mean = measured["usd_per_add"]
        extrapolation = {"basis": "observed billed logical adds, including retries; unchanged prompt/token distribution assumed",
                         "conv_adds": conv_turns * 2, "dataset_adds": all_turns * 2,
                         "conv_ingest_usd": mean * conv_turns * 2,
                         "conv_ingest_cny": mean * conv_turns * 2 * CNY_PER_USD,
                         "dataset_ingest_usd": mean * all_turns * 2,
                         "dataset_ingest_cny": mean * all_turns * 2 * CNY_PER_USD,
                         "includes_answers_and_judges": False,
                         "automatic_full_run": False}
    paid = costs.summarize(rows)["total"]
    return {"conversation": conversation, "sessions": [s.key for s in chosen],
            "utterances": sum(len(s.utterances) for s in chosen), "memory_turns": pilot_turns,
            "mem0_perspectives": 2, "mem0_adds": pilot_turns * 2,
            "qid_format": "<sample_id>-q<original qa index>", "qids": [q.qid for q in selected],
            "qids_csv": ",".join(q.qid for q in selected),
            "questions": [{"qid": q.qid, "category": q.category, "evidence": q.evidence} for q in selected],
            "budget_cny": .5, "budget_usd": .5 / CNY_PER_USD,
            "recorded_cost": paid, "remaining_usd": max(0, .5 / CNY_PER_USD - paid["usd"]),
            "budget_note": "Shared pilot ceiling across daemon, mem0, answers, judges and preflight; caps are soft. Root dispatches each stage.",
            "paid_calls_performed_by_this_script": 0,
            "mem0_measured_adds": measured, "mem0_ingest_extrapolation": extrapolation}


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--conversation", default="conv-26")
    ap.add_argument("--sessions", type=int, default=2)
    ap.add_argument("--questions", type=int, default=10)
    ap.add_argument("--ledger", action="append", default=[])
    ap.add_argument("--add-component", default="mem0-pilot.add")
    ap.add_argument("--output", default=None)
    args = ap.parse_args()
    rows = [row for path in args.ledger for row in costs.load_rows(path)]
    plan = build(args.conversation, args.sessions, args.questions, ledger_rows=rows, add_component=args.add_component)
    rendered = json.dumps(plan, indent=2, ensure_ascii=False)
    if args.output:
        path = Path(args.output)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(rendered + "\n", encoding="utf-8")
    print(rendered)


if __name__ == "__main__":
    main()
