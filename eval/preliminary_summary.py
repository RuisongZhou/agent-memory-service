#!/usr/bin/env python
"""Local-only pilot usage: ledger files and exported example responses."""
from __future__ import annotations

import argparse
import json
from datetime import datetime, timezone
from pathlib import Path

from common import usage as usage_ledger
from common.usage_records import usage_entries
from common.llm import Ledger


def load_usage(path):
    path = Path(path)
    if path.suffix == ".jsonl":
        from common.jsonl import read
        return list(read(path))
    payload = json.loads(path.read_text(encoding="utf-8"))
    return payload.get("records") if isinstance(payload, dict) else payload


def summarize(root, *, ledgers=None, usage_exports=None):
    root = Path(root)
    paths = list(dict.fromkeys(Path(p).resolve() for p in (ledgers if ledgers is not None else root.rglob("*ledger*.jsonl"))))
    rows = [row for path in paths for row in usage_ledger.load_rows(path)]
    exports = []
    for tenant, path in usage_exports or []:
        converted = [Ledger.row(**entry) for entry in usage_entries(load_usage(path), tenant, f"example.{tenant}")]
        rows.extend(converted)
        exports.append({"tenant": tenant, "path": str(path), "usage_records": len(converted)})
    merged = usage_ledger.merge_rows(rows)
    measured = usage_ledger.summarize(merged)
    return {"generated_at": datetime.now(timezone.utc).isoformat(timespec="seconds"),
            "ledger_files": [str(p) for p in paths], "example_usage_exports": exports,
            "n_deduplicated_records": len(rows) - len(merged),
            "recorded_calls": measured["total"]["calls"], "total": measured["total"],
            "components": measured["components"],
            "notes": ["Returned failures, retries, unfinished questions and explicitly supplied examples remain included."],
            "model_calls_by_this_tool": 0}


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--results-dir", default="results/local-pilot")
    ap.add_argument("--ledger", action="append", default=None)
    ap.add_argument("--usage-export", action="append", default=[], help="actual-tenant=exported-llm-usage.json (repeatable)")
    ap.add_argument("--output", default=None)
    args = ap.parse_args()
    exports = []
    for value in args.usage_export:
        tenant, separator, path = value.partition("=")
        if not separator or not tenant or not path:
            ap.error("--usage-export must be actual-tenant=path")
        exports.append((tenant, path))
    data = summarize(args.results_dir, ledgers=args.ledger, usage_exports=exports)
    rendered = json.dumps(data, indent=2, ensure_ascii=False)
    if args.output:
        path = Path(args.output)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(rendered + "\n", encoding="utf-8")
    print(rendered)


if __name__ == "__main__":
    main()
