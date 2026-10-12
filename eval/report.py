#!/usr/bin/env python
"""Collect eval/results/<system>/<setting>/summary-<prompt>.json into one table (markdown + JSON).

  .venv/bin/python eval/report.py            # -> eval/results/report.md, eval/results/report.json
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from common.env import RESULTS_DIR  # noqa: E402
from common.locomo import CATEGORY_NAMES, EVAL_CATEGORIES  # noqa: E402

from common import usage as usage_ledger  # noqa: E402


def fmt(x, nd=3, pct=False):
    if x is None:
        return "–"
    return f"{100 * x:.1f}" if pct else f"{x:.{nd}f}"


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--results-dir", default=str(RESULTS_DIR))
    ap.add_argument("--expected-systems", default=None, help="comma-separated systems expected for a comparison")
    a = ap.parse_args()
    root = Path(a.results_dir)
    summaries = []
    for p in sorted(root.glob("*/*/summary-*.json")):
        s = json.loads(p.read_text())
        s["_path"] = str(p.relative_to(root))
        summaries.append(s)
    if not summaries:
        raise SystemExit(f"no summary-*.json under {root}")

    cat_keys = [f"{c}-{CATEGORY_NAMES[c]}" for c in EVAL_CATEGORIES]
    head = (["system", "setting", "prompt", "answered/expected", "scope", "complete", "J", *[f"J {CATEGORY_NAMES[c]}" for c in EVAL_CATEGORIES],
             "F1", "BLEU-1", "ctx tok", "lat mean ms", "lat p95 ms"])
    lines = ["| " + " | ".join(head) + " |", "|" + "---|" * len(head)]
    table = []
    for s in summaries:
        o, bc = s["overall"], s["by_category"]
        scope = s.get("scope") or {}
        row = {
            "system": s["system"], "setting": s["setting"], "answer_prompt": s["answer_prompt"],
            "n_expected": scope.get("n_expected_questions"), "n_contexts": s.get("n_contexts"),
            "scope": scope, "complete": bool(scope.get("verified") and scope.get("retrieval_complete") and scope.get("answering_complete")),
            "n": o["n"], "J": o["J"], "J_by_category": {k: bc.get(k, {}).get("J") for k in cat_keys},
            "n_by_category": {k: bc.get(k, {}).get("n") for k in cat_keys},
            "F1": o["F1"], "BLEU1": o["BLEU1"], "context_tokens_mean": o["context_tokens_mean"],
            "search_ms_mean": o["search_ms_mean"], "search_ms_p95": o["search_ms_p95"],
            "source": s["_path"],
        }
        table.append(row)
        lines.append("| " + " | ".join([
            row["system"], row["setting"], row["answer_prompt"], f"{row['n']}/{row['n_expected'] if row['n_expected'] is not None else '?'}",
            scope.get("kind", "scope-unverified"), "yes" if row["complete"] else "no", fmt(row["J"], pct=True),
            *[fmt(row["J_by_category"][k], pct=True) for k in cat_keys],
            fmt(row["F1"], pct=True), fmt(row["BLEU1"], pct=True), fmt(row["context_tokens_mean"], 0),
            fmt(row["search_ms_mean"], 0), fmt(row["search_ms_p95"], 0)]) + " |")
    ledger_rows = usage_ledger.merge_rows([r for path in root.rglob("*ledger*.jsonl") for r in usage_ledger.load_rows(path)])
    total = usage_ledger.summarize(ledger_rows)["total"]
    warnings = []
    if any(not row["complete"] for row in table):
        warnings.append("Incomplete or unverified rows are observed partial results; they do not establish a full benchmark result.")
    cohorts = {s.get("scope", {}).get("qid_cohort_hash") for s in summaries}
    if len(cohorts) > 1 or None in cohorts:
        warnings.append("Question/session cohorts differ or are unverified; do not directly rank these rows as a shared comparison.")
    if a.expected_systems:
        missing = set(a.expected_systems.split(",")) - {s["system"] for s in summaries}
        if missing:
            warnings.append("Missing system results: " + ", ".join(sorted(missing)))
    warnings.append("Run tags label outputs and usage components; they do not isolate daemon tenants/stores. Inspect the protocol and data-dir before interpreting a reused store as a standalone full run.")
    md = ["# LoCoMo observed results (categories 1-4, J/F1/BLEU-1 in %)", "",
          *lines, "",
          "J = LLM-judge accuracy (mem0 ACCURACY_PROMPT, model recorded in each run, non-thinking); F1 / BLEU-1 = mem0 "
          "metrics code; ctx tok = mean retrieved-context size (tiktoken cl100k); latency = client-side search "
          "time per question (mem0: both speakers' searches summed).", "",
          f"Recorded usage: {total['calls']} calls, {total['hit'] + total['miss']} input tokens, {total['out']} output tokens. Returned retries and unfinished requests remain included.",
          "", *warnings]
    (root / "report.md").write_text("\n".join(md) + "\n")
    (root / "report.json").write_text(json.dumps({"rows": table, "usage_total": total,
                                                "comparison_warnings": warnings}, indent=2))
    print("\n".join(md))


if __name__ == "__main__":
    main()
