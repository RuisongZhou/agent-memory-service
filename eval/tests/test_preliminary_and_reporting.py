import json
from pathlib import Path

import pytest

import answer_and_judge
import usage_stats
import preliminary_summary
from common import jsonl, locomo
from common.usage_records import usage_entries
from common.llm import Ledger
from common.reporting import scope_for


def usage():
    return {"mid": "zh-call", "stage": "P1-b", "model": "qwen3.8-flash",
            "createdAt": "2026-10-09T02:30:00Z", "promptTokens": 120,
            "cacheHitTokens": 20, "cacheMissTokens": 100, "completionTokens": 30,
            "validJson": False, "attempt": 2}


def test_shared_preliminary_includes_example_preflight_failed_and_unfinished(tmp_path):
    ledger = Ledger(tmp_path / "usage_ledger.jsonl")
    ledger.record("preflight", 0, 44, 5)
    ledger.record("answer.mem0-pilot/k30", 0, 100, 20, qid="unfinished", parse_error=True)
    export = tmp_path / "zh-example-llm-usage.json"
    export.write_text(json.dumps({"table": "llm_usage", "records": [usage()]}))
    result = preliminary_summary.summarize(tmp_path, usage_exports=[("zh-example-zh", export)])
    assert result["recorded_calls"] == 3
    assert "example.zh-example-zh.daemon/P1-b" in result["components"]
    assert result["total"]["miss"] == 244
    assert "recorded_usd" not in result and "api_balance_reconciliation" not in result
    assert result["model_calls_by_this_tool"] == 0


def test_example_export_deduplicates_existing_paid_record(tmp_path):
    ledger = Ledger(tmp_path / "usage_ledger.jsonl")
    ledger.record_many(usage_entries([usage()], "zh-example-zh", "zh-example"))
    export = tmp_path / "llm_usage.json"
    export.write_text(json.dumps([usage()]))
    result = preliminary_summary.summarize(tmp_path, usage_exports=[("zh-example-zh", export)])
    assert result["recorded_calls"] == 1 and result["n_deduplicated_records"] == 1


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


def test_total_usage_retains_unfinished_requests_while_completed_usage_is_scoped(tmp_path):
    path = tmp_path / "mem0/main/answers-unified.jsonl"
    rec = {"system": "mem0", "setting": "main", "conv": "conv-26", "qid": "conv-26-q0", "category": 2,
           "question": "Q?", "answer": "A", "context": "context"}
    source_hash = answer_and_judge.fingerprint(rec, "unified")
    jsonl.append(path, {**rec, "source_hash": source_hash, "judge_label": "CORRECT", "J": 1, "f1": 1, "bleu1": 1})
    ledger = Ledger(tmp_path / "usage_ledger.jsonl")
    answer_component, judge_component = answer_and_judge.components("mem0", "main", "unified")
    for component, qid, sha in [(answer_component, rec["qid"], source_hash), (judge_component, rec["qid"], source_hash),
                                (answer_component, "unfinished", "old")]:
        ledger.record(component, 0, 100, 20, qid=qid, source_hash=sha)
    summary = answer_and_judge.summarize(path, [rec], "mem0", "main", "unified", ledger.path)
    assert summary["usage"]["answer"]["calls"] == 2
    assert summary["usage"]["completed_current_context_requests"]["calls"] == 2
    assert summary["usage"]["n_fully_recorded_completed_questions"] == 1
    assert "usd" not in summary["usage"]["answer"]
    assert "cost" not in summary
