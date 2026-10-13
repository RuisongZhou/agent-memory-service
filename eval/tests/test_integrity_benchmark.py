from integrity_benchmark import select_questions, reuse_answers
from common.answers import fingerprint
from common.corpus import semantic_hash
from common import jsonl
from common import env
from integrity_benchmark import report
from argparse import Namespace
import json
import pytest


def conversation():
    return {"sample_id": "test", "qa": [
        {"category": category, "question": f"{category}-{i}", "answer": "a"}
        for category in (1, 2, 3, 4, 5) for i in range(3)]}


def test_subset_is_fixed_by_category_and_original_index():
    rows = select_questions(conversation(), 2)
    assert [row.qid for row in rows] == ["test-q0", "test-q1", "test-q3", "test-q4", "test-q6", "test-q7", "test-q9", "test-q10"]


def test_corpus_audit_ignores_order_and_index_fields_but_detects_semantic_mutation():
    rows = [{"id": "a", "text": "one", "status": "current", "embeddingModel": "old"},
            {"id": "b", "text": "two", "status": "current"}]
    assert semantic_hash(rows) == semantic_hash(list(reversed(rows)))
    assert semantic_hash(rows) == semantic_hash([{**rows[0], "embeddingModel": "new"}, rows[1]])
    assert semantic_hash(rows) != semantic_hash([{**rows[0], "status": "stale"}, rows[1]])


def test_answer_reuse_requires_exact_prompt_and_valid_judge(tmp_path):
    row = {"system": "old", "setting": "buffer", "qid": "q1", "question": "Question?", "answer": "Answer",
           "context": "Evidence", "search_ms": 1, "context_tokens": 1, "n_memories": 1, "n_turns": 0}
    jsonl.append(tmp_path / "contexts.jsonl", row)
    jsonl.append(tmp_path / "answers-unified.jsonl", {**row, "source_hash": fingerprint(row, "unified"), "judge_label": "CORRECT"})
    new = {**row, "system": "new", "search_ms": 2}
    reused = reuse_answers([new], tmp_path)
    assert reused[0]["source_hash"] == fingerprint(new, "unified") and reused[0]["search_ms"] == 2
    assert reuse_answers([{**new, "context": "Changed evidence"}], tmp_path) == []


@pytest.mark.parametrize("historical_path", [
    "results/historical",
    "/previous-checkout/agent-memory-service/results/historical",
])
def test_report_reads_relocated_evidence_without_rewriting_manifest(tmp_path, monkeypatch, historical_path):
    results = tmp_path / "eval/results"
    monkeypatch.setattr(env, "RESULTS_DIR", results)
    root = results / "replay"
    root.mkdir(parents=True)
    manifest = {"qids": ["q1"], "historical_root": historical_path}
    frozen = json.dumps(manifest).encode()
    (root / "manifest.json").write_bytes(frozen)
    paths = [results / "historical/memory-en/buffer", results / "historical/mem0/k30",
             root / "current/retained-snapshot", root / "improved/retained-snapshot"]
    for path in paths:
        row = {"system": path.name, "setting": "fixture", "qid": "q1", "question": "Question?",
               "answer": "Answer", "context": "Evidence", "category": 1, "search_ms": 1,
               "context_tokens": 1, "J": 1, "f1": 1, "bleu1": 1}
        jsonl.append(path / "contexts.jsonl", row)
        jsonl.append(path / "answers-unified.jsonl",
                     {**row, "source_hash": fingerprint(row, "unified"), "judge_label": "CORRECT"})
        jsonl.append(path / "retrieval.jsonl", {"qid": "q1", "pass": 0, "search_ms": 1,
                                               "response": {"results": [], "profiles": [], "recentTurns": []}})
    report(Namespace(root=str(root)))
    result = json.loads((root / "comparison.json").read_text())
    assert len(result["systems"]) == 4
    assert (root / "manifest.json").read_bytes() == frozen
