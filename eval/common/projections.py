"""Conditional cost scenarios from measured stages; no metric or missing-cost extrapolation."""
from . import locomo
from .llm import CNY_PER_USD


def nominal_jobs(conversations):
    sessions = [s for c in conversations for s in locomo.sessions(c)]
    return {"P1": sum((len(locomo.turns(s)) + 2) // 3 for s in sessions), "P2": len(sessions),
            "P3": len(conversations), "P3-project": len(conversations)}


def project(summary, ledger_rows, protocol=None):
    import costs
    system = summary["system"]
    all_data = locomo.load()
    conv26 = [c for c in all_data if c["sample_id"] == "conv-26"]
    targets = [("conv-26", conv26), ("full-dataset", all_data)]
    system_rows = [r for r in ledger_rows if r.get("component", "").startswith(f"{system}.")]
    mem0_rows = [r for r in system_rows if r.get("component") == f"{system}.add"]
    is_mem0 = (protocol or {}).get("mem0_version") == "2.2.1" or bool(mem0_rows)
    daemon_rows = [r for r in system_rows if r.get("source") == "daemon/llm_usage"]
    kind = "mem0" if is_mem0 else "memory" if daemon_rows or "tenant_mode" in (protocol or {}) else "unknown"
    add_cost = costs.summarize_adds(mem0_rows, f"{system}.add")
    stage_means = {}
    for stage in nominal_jobs(conv26):
        records = [r for r in daemon_rows if r.get("source_stage") == stage]
        if records:
            stage_means[stage] = sum(r["usd"] for r in records) / len(records)
    qa_unit = summary.get("cost", {}).get("completed_current_context_usd_per_question")
    scenarios = []
    for name, data in targets:
        n_questions = sum(len(locomo.qas(c)) for c in data)
        jobs = nominal_jobs(data)
        known_stage_total, missing = None, []
        if kind == "mem0":
            n_adds = 2 * sum(len(locomo.all_turns(c)) for c in data)
            ingest = add_cost["usd_per_add"] * n_adds if add_cost["usd_per_add"] is not None else None
            ingest_basis = {"nominal_adds": n_adds, "observed_adds": add_cost["n_adds"],
                            "observed_usd_per_add_including_retries": add_cost["usd_per_add"]}
        elif kind == "memory":
            missing = [stage for stage in jobs if stage not in stage_means]
            known_stage_total = sum(jobs[s] * mean for s, mean in stage_means.items()) if stage_means else None
            ingest = known_stage_total if not missing else None
            ingest_basis = {"nominal_stage_calls": jobs, "observed_usd_per_response": stage_means,
                            "unknown_stages": missing, "known_stage_subtotal_usd": known_stage_total}
        else:
            ingest, ingest_basis = None, {"reason": "system protocol/ingest costs unavailable"}
        qa_cost = qa_unit * n_questions if qa_unit is not None else None
        total = ingest + qa_cost if ingest is not None and qa_cost is not None else None
        scenarios.append({"target": name, "questions": n_questions, "ingest_usd": ingest,
                          "answer_judge_usd": qa_cost, "total_usd": total,
                          "total_cny_estimate": total * CNY_PER_USD if total is not None else None,
                          "ingest_basis": ingest_basis,
                          "answer_judge_observed_usd_per_completed_question": qa_unit})
    return {"system": system, "setting": summary["setting"], "kind": kind, "scenarios": scenarios,
            "assumptions": ["No accuracy extrapolation. These are conditional cost scenarios, not invoices or upper bounds.",
                            "Future tokens, cache hit rates, retry rates, temporal prices and larger conversation histories can change cost.",
                            "Memory assumes one P1 response per nominal three-turn batch, one P2 per session and at most one P3 per scope; absent P3 measurements remain unknown.",
                            "Ingest is shared across buffer/main settings and must not be summed twice.",
                            "USD × 7.2 is only a CNY estimate; actual account balance reconciliation is separate."],
            "automatic_full_run": False}
