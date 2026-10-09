import json
from pathlib import Path

import pytest

import answer_and_judge
import costs
import preliminary_summary
from common import jsonl, locomo
from common.billing import usage_entries
from common.llm import Ledger
from common.projections import project
from common.reporting import scope_for


def usage():
    return {"mid": "zh-call", "stage": "P1-b", "model": "deepseek-flash",
            "createdAt": "2026-10-09T02:30:00Z", "promptTokens": 120,
            "cacheHitTokens": 20, "cacheMissTokens": 100, "completionTokens": 30,
            "validJson": False, "attempt": 2}


def test_shared_preliminary_includes_example_preflight_failed_and_unfinished(tmp_path):
    ledger = Ledger(tmp_path / "cost_ledger.jsonl")
    ledger.record("preflight", 0, 44, 5)
    ledger.record("answer.mem0-pilot/k30", 0, 100, 20, qid="unfinished", parse_error=True)
    export = tmp_path / "zh-example-llm-usage.json"
    export.write_text(json.dumps({"table": "llm_usage", "records": [usage()]}))
    result = preliminary_summary.summarize(tmp_path, usage_exports=[("zh-example-zh", export)])
    assert result["recorded_calls"] == 3
    assert "example.zh-example-zh.daemon/P1-b" in result["components"]
    assert result["recorded_cny_estimate"] == result["recorded_usd"] * 7.2
    assert result["api_balance_reconciliation"] is None
    assert result["paid_calls_by_this_tool"] == 0


def test_example_export_deduplicates_existing_paid_record(tmp_path):
    ledger = Ledger(tmp_path / "cost_ledger.jsonl")
    ledger.record_many(usage_entries([usage()], "zh-example-zh", "zh-example"))
    export = tmp_path / "llm_usage.json"
    export.write_text(json.dumps([usage()]))
    result = preliminary_summary.summarize(tmp_path, usage_exports=[("zh-example-zh", export)])
    assert result["recorded_calls"] == 1 and result["n_deduplicated_records"] == 1


def test_balance_reconciliation_keeps_actual_currency_separate(tmp_path):
    result = preliminary_summary.summarize(tmp_path, balance_before="25.32", balance_after="25.30", balance_currency="CNY")
    assert result["api_balance_reconciliation"]["observed_cny_delta"] == .02
    assert result["conservative_preliminary_target"]["spent_cny"] == .02
    assert result["recorded_usd"] == 0  # only known ledger spend, not a fabricated conversion
    usd = preliminary_summary.summarize(tmp_path, balance_before="2", balance_after="1.9", balance_currency="USD")
    assert usd["api_balance_reconciliation"]["observed_cny_delta"] is None
    assert usd["conservative_preliminary_target"]["over_target"] is True
    assert "estimate" in usd["conservative_preliminary_target"]["basis"]
    with pytest.raises(ValueError, match="currency"):
        preliminary_summary.summarize(tmp_path, balance_before="2", balance_after="1.9")


def test_pilot_scope_partial_and_full_scope_are_distinguished(tmp_path):
    path = tmp_path / "system/main/answers-unified.jsonl"
    path.parent.mkdir(parents=True)
    config = {"convs": ["conv-26"], "sessions": 2, "qids": ["conv-26-q0", "conv-26-q1"], "limit": None}
    (path.parent.parent / "run_config.json").write_text(json.dumps(config))
    contexts = [{"conv": "conv-26", "qid": "conv-26-q0"}, {"conv": "conv-26", "qid": "conv-26-q1"}]
    scope = scope_for(path, contexts, contexts[:1])
    assert scope["kind"] == "session-and-question-subset"
    assert scope["retrieval_complete"] is True and scope["answering_complete"] is False
    assert scope["n_expected_questions"] == 2 and scope["n_full_dataset_questions"] == 1540
    config.update(sessions=None, qids=None)
    (path.parent.parent / "run_config.json").write_text(json.dumps(config))
    full_conv = [{"conv": "conv-26", "qid": q.qid} for q in locomo.qas(locomo.load()[0])]
    full_scope = scope_for(path, full_conv, full_conv)
    assert full_scope["kind"] == "conversation-subset" and full_scope["answering_complete"]
    assert full_scope["qid_cohort_hash"] != scope["qid_cohort_hash"]


def test_cost_scenarios_do_not_invent_missing_memory_stages_or_scores():
    summary = {"system": "memory-en-pilot", "setting": "main", "cost": {}}
    rows = [{"component": "memory-en-pilot.daemon/P1", "source": "daemon/llm_usage", "source_stage": "P1", "usd": .001}]
    result = project(summary, rows, {"tenant_mode": "variant-suffix"})
    for scenario in result["scenarios"]:
        assert scenario["ingest_usd"] is None and scenario["answer_judge_usd"] is None
        assert scenario["total_usd"] is None
        assert set(scenario["ingest_basis"]["unknown_stages"]) == {"P2", "P3", "P3-project"}
    assert result["scenarios"][1]["questions"] == 1540
    assert result["automatic_full_run"] is False


def test_total_cost_retains_unfinished_requests_while_unit_cost_is_scoped(tmp_path):
    path = tmp_path / "mem0/main/answers-unified.jsonl"
    rec = {"system": "mem0", "setting": "main", "conv": "conv-26", "qid": "conv-26-q0", "category": 2,
           "question": "Q?", "answer": "A", "context": "context"}
    source_hash = answer_and_judge.fingerprint(rec, "unified")
    jsonl.append(path, {**rec, "source_hash": source_hash, "judge_label": "CORRECT", "J": 1, "f1": 1, "bleu1": 1})
    ledger = Ledger(tmp_path / "cost_ledger.jsonl")
    answer_component, judge_component = answer_and_judge.components("mem0", "main", "unified")
    for component, qid, sha in [(answer_component, rec["qid"], source_hash), (judge_component, rec["qid"], source_hash),
                                (answer_component, "unfinished", "old")]:
        ledger.record(component, 0, 100, 20, qid=qid, source_hash=sha)
    summary = answer_and_judge.summarize(path, [rec], "mem0", "main", "unified", ledger.path)
    assert summary["cost"]["answer"]["calls"] == 2
    assert summary["cost"]["completed_current_context_requests"]["calls"] == 2
    assert summary["cost"]["n_fully_costed_completed_questions"] == 1
    assert summary["cost"]["answer_judge_usd_per_question"] > summary["cost"]["completed_current_context_usd_per_question"]
