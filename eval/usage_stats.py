#!/usr/bin/env python
"""Summarise eval/results/usage_ledger.jsonl by component (calls and tokens).

  .venv/bin/python usage_stats.py                       # all components
  .venv/bin/python usage_stats.py --prefix answer.      # only answer.* components
  .venv/bin/python usage_stats.py --since 2026-10-09T00:00:00 --json
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from common.llm import LEDGER_PATH  # noqa: E402
from common.usage import load_rows, merge_rows, summarize, summarize_adds  # noqa: E402,F401


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
