#!/usr/bin/env python
"""Local-only shared pilot spend: ledger files + exported example usage + optional API balance snapshots."""
from __future__ import annotations

import argparse
import json
from datetime import datetime, timezone
from decimal import Decimal
from pathlib import Path

import costs
from common.billing import usage_entries
from common.llm import CNY_PER_USD, Ledger


def load_usage(path):
    path = Path(path)
    if path.suffix == ".jsonl":
        from common.jsonl import read
        return list(read(path))
    payload = json.loads(path.read_text(encoding="utf-8"))
    return payload.get("records") if isinstance(payload, dict) else payload


def summarize(root, *, ledgers=None, usage_exports=None, balance_before=None, balance_after=None,
              balance_currency=None, target_cny=.5):
    root = Path(root)
    paths = list(dict.fromkeys(Path(p).resolve() for p in (ledgers if ledgers is not None else root.rglob("*ledger*.jsonl"))))
    rows = [row for path in paths for row in costs.load_rows(path)]
    exports = []
    for tenant, path in usage_exports or []:
        converted = [Ledger.row(**entry) for entry in usage_entries(load_usage(path), tenant, f"example.{tenant}")]
        rows.extend(converted)
        exports.append({"tenant": tenant, "path": str(path), "usage_records": len(converted)})
    merged = costs.merge_rows(rows)
    measured = costs.summarize(merged)
    usd = measured["total"]["usd"]
    cny_estimate = usd * CNY_PER_USD
    balance = None
    if (balance_before is None) != (balance_after is None):
        raise ValueError("provide both balance snapshots, with their API currency")
    if balance_before is not None:
        if balance_currency not in ("CNY", "USD"):
            raise ValueError("balance currency must be explicitly CNY or USD")
        delta = Decimal(str(balance_before)) - Decimal(str(balance_after))
        balance = {"currency": balance_currency, "before": str(balance_before), "after": str(balance_after),
                   "delta": float(delta), "includes_any_other_account_activity": True,
                   "pilot_attribution_verified": False,
                   "observed_cny_delta": float(delta) if balance_currency == "CNY" else None,
                   "cny_delta_estimate": float(delta) * CNY_PER_USD if balance_currency == "USD" else None,
                   "balance_minus_ledger_estimate_cny": float(delta) - cny_estimate if balance_currency == "CNY" else None}
    actual = balance["observed_cny_delta"] if balance else None
    basis = "max(API-account-CNY-difference,token-ledger-CNY-estimate)" if actual is not None and actual >= 0 else "token-ledger-CNY-estimate"
    # Balance snapshots can be rounded to cents. Keep the larger observed/estimated
    # value for a conservative workflow target while displaying both separately.
    spent_for_target = max(actual, cny_estimate) if actual is not None and actual >= 0 else cny_estimate
    if balance and balance_currency == "USD" and balance["delta"] >= 0:
        basis = "max(API-USD-difference-converted-CNY-estimate,token-ledger-CNY-estimate)"
        spent_for_target = max(balance["cny_delta_estimate"], cny_estimate)
    components = {name: {**{k: v for k, v in values.items() if k != "cny"}, "cny_estimate": values["cny"]}
                  for name, values in measured["components"].items()}
    return {"generated_at": datetime.now(timezone.utc).isoformat(timespec="seconds"),
            "ledger_files": [str(p) for p in paths], "example_usage_exports": exports,
            "n_deduplicated_records": len(rows) - len(merged),
            "recorded_calls": measured["total"]["calls"], "recorded_usd": usd,
            "cny_per_usd_assumption": CNY_PER_USD, "recorded_cny_estimate": cny_estimate,
            "components": components, "api_balance_reconciliation": balance,
            "conservative_preliminary_target": {"cny": target_cny, "basis": basis,
                                                "spent_cny": spent_for_target,
                                                "remaining_cny": max(0, target_cny - spent_for_target),
                                                "over_target": spent_for_target > target_cny},
            "notes": ["Totals retain failures, retries, unfinished questions, old contexts, preflight and explicitly supplied examples.",
                      "USD token costs use verified unit prices; USD × 7.2 is a CNY estimate, not an API balance charge.",
                      "Missing/ongoing requests without returned usage require balance reconciliation; no missing cost or score is invented.",
                      "The 0.5 CNY shared preliminary target is conservative workflow scope; earlier formation and mem0 acceptance limits remain separate."],
            "paid_calls_by_this_tool": 0}


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--results-dir", default="results/local-pilot")
    ap.add_argument("--ledger", action="append", default=None)
    ap.add_argument("--usage-export", action="append", default=[], help="actual-tenant=exported-llm-usage.json (repeatable)")
    ap.add_argument("--balance-before", default=None)
    ap.add_argument("--balance-after", default=None)
    ap.add_argument("--balance-currency", choices=["CNY", "USD"], default=None)
    ap.add_argument("--target-cny", type=float, default=.5)
    ap.add_argument("--output", default=None)
    args = ap.parse_args()
    exports = []
    for value in args.usage_export:
        tenant, separator, path = value.partition("=")
        if not separator or not tenant or not path:
            ap.error("--usage-export must be actual-tenant=path")
        exports.append((tenant, path))
    data = summarize(args.results_dir, ledgers=args.ledger, usage_exports=exports,
                     balance_before=args.balance_before, balance_after=args.balance_after,
                     balance_currency=args.balance_currency, target_cny=args.target_cny)
    rendered = json.dumps(data, indent=2, ensure_ascii=False)
    if args.output:
        path = Path(args.output)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(rendered + "\n", encoding="utf-8")
    print(rendered)


if __name__ == "__main__":
    main()
