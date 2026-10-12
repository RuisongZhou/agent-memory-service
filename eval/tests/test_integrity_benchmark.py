from integrity_benchmark import select_questions, semantic_hash, reuse_answers
from answer_and_judge import fingerprint
from common import jsonl
import pytest


def conversation():
    return {"sample_id": "test", "qa": [
        {"category": category, "question": f"{category}-{i}", "answer": "a"}
        for category in (1, 2, 3, 4, 5) for i in range(3)]}


def test_subset_is_fixed_by_category_and_original_index():
    rows = select_questions(conversation(), 2)
    assert [row.qid for row in rows] == ["test-q0", "test-q1", "test-q3", "test-q4", "test-q6", "test-q7", "test-q9", "test-q10"]
    assert all(row.category != 5 for row in rows)
    with pytest.raises(ValueError):
        select_questions(conversation(), 4)


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
    assert len(reused) == 1
    assert reused[0]["source_hash"] == fingerprint(new, "unified")
    assert reused[0]["search_ms"] == 2
    assert reuse_answers([{**new, "context": "Changed evidence"}], tmp_path) == []
    jsonl.append(tmp_path / "answers-unified.jsonl", {**row, "source_hash": fingerprint(row, "unified"), "judge_label": "UNKNOWN"})
    assert reuse_answers([new], tmp_path) == []
