"""Usage-ledger aggregation shared by the usage CLI, answer/judge summaries and reports."""
from __future__ import annotations

from collections import defaultdict
from pathlib import Path

from . import jsonl
from .llm import LEDGER_PATH


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
