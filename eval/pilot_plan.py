#!/usr/bin/env python
"""Prepare an evidence-scoped pilot manifest; never calls APIs."""
from __future__ import annotations

import argparse
import json
import re
from pathlib import Path

from common import locomo


def build(conversation="conv-26", sessions=2, questions=10):
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
    return {"conversation": conversation, "sessions": [s.key for s in chosen],
            "utterances": sum(len(s.utterances) for s in chosen), "memory_turns": pilot_turns,
            "mem0_perspectives": 2, "mem0_adds": pilot_turns * 2,
            "qid_format": "<sample_id>-q<original qa index>", "qids": [q.qid for q in selected],
            "qids_csv": ",".join(q.qid for q in selected),
            "questions": [{"qid": q.qid, "category": q.category, "evidence": q.evidence} for q in selected],
            "model_calls_performed_by_this_script": 0}


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--conversation", default="conv-26")
    ap.add_argument("--sessions", type=int, default=2)
    ap.add_argument("--questions", type=int, default=10)
    ap.add_argument("--output", default=None)
    args = ap.parse_args()
    plan = build(args.conversation, args.sessions, args.questions)
    rendered = json.dumps(plan, indent=2, ensure_ascii=False)
    if args.output:
        path = Path(args.output)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(rendered + "\n", encoding="utf-8")
    print(rendered)


if __name__ == "__main__":
    main()
