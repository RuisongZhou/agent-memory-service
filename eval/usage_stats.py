#!/usr/bin/env python
"""Summarise results/usage_ledger.jsonl by component (calls and tokens).

  .venv/bin/python usage_stats.py                       # all components
  .venv/bin/python usage_stats.py --prefix answer.      # only answer.* components
  .venv/bin/python usage_stats.py --since 2026-10-09T00:00:00 --json
"""
from __future__ import annotations

import argparse
import json
import sys
from collections import defaultdict
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from common import jsonl  # noqa: E402
from common.llm import LEDGER_PATH  # noqa: E402


def load_rows(ledger: Path | str = LEDGER_PATH, since: str | None = None) -> list[dict]:
    rows = list(jsonl.read(ledger))
    if since:
        rows = [r for r in rows if r.get("ts", "") >= since]
    return rows


def merge_rows(rows: list[dict]) -> list[dict]:
    """Keep all requests; deduplicate only explicit stable event/response identities."""
    seen, merged = set(), []
    for row in rows:
        identity = row.get("event_id")
        if not identity and row.get("response_id"):
            identity = f"response/{row.get('model', '')}/{row['response_id']}"
        if identity and identity in seen:
            continue
        if identity:
            seen.add(identity)
        merged.append(row)
    return merged


def summarize(rows: list[dict], prefix: str | None = None, exact: str | None = None) -> dict:
    by = defaultdict(lambda: {"calls": 0, "hit": 0, "miss": 0, "out": 0,
                              "parse_errors": 0, "reasoning_tokens": 0})
    for r in rows:
        comp = r.get("component", "?")
        if prefix and not comp.startswith(prefix):
            continue
        if exact and comp != exact:
            continue
        b = by[comp]
        b["calls"] += int(r.get("calls", 1))
        for k in ("hit", "miss", "out"):
            b[k] += int(r.get(k, 0))
        b["parse_errors"] += 1 if r.get("parse_error") else 0
        b["reasoning_tokens"] += int(r.get("reasoning_tokens") or 0)
    total = {"calls": 0, "hit": 0, "miss": 0, "out": 0}
    for b in by.values():
        for k in total:
            total[k] += b[k]
    return {"components": dict(sorted(by.items())), "total": total}


def summarize_adds(rows: list[dict], component: str) -> dict:
    """Each logical mem0 add, including all returned retry attempts."""
    grouped = defaultdict(list)
    unattributed = []
    for row in rows:
        if row.get("component") != component:
            continue
        if row.get("add_id"):
            grouped[row["add_id"]].append(row)
        else:
            unattributed.append(row)
    adds = []
    for add_id, records in sorted(grouped.items()):
        value = summarize(records)["total"]
        first = records[0]
        adds.append({"add_id": add_id, **{k: first.get(k) for k in ("conv", "user_id", "session", "batch")},
                     **value, "parse_errors": sum(bool(r.get("parse_error")) for r in records)})
    return {"n_adds": len(adds), "adds": adds, "unattributed": summarize(unattributed)["total"]}


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--ledger", action="append", default=None, help="repeat to aggregate multiple ledgers")
    ap.add_argument("--prefix", default=None)
    ap.add_argument("--since", default=None, help="ISO timestamp lower bound (UTC)")
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--add-component", default=None, help="include per-add mem0 usage for this exact component")
    a = ap.parse_args()
    paths = dict.fromkeys(Path(p).resolve() for p in (a.ledger or [LEDGER_PATH]))
    rows = merge_rows([r for path in paths for r in load_rows(path, a.since)])
    s = summarize(rows, a.prefix)
    if a.add_component:
        s["mem0_add"] = summarize_adds(rows, a.add_component)
    if a.json:
        print(json.dumps(s, indent=2))
        return
    hdr = f"{'component':<34}{'calls':>7}{'hit':>11}{'miss':>11}{'out':>10}{'perr':>6}"
    print(hdr)
    for comp, b in s["components"].items():
        print(f"{comp:<34}{b['calls']:>7}{b['hit']:>11}{b['miss']:>11}{b['out']:>10}{b['parse_errors']:>6}")
    print(json.dumps(s["total"]))
    if a.add_component:
        for add in s["mem0_add"]["adds"]:
            print(f"add {add['add_id']}: {add['calls']} calls")



if __name__ == "__main__":
    main()
