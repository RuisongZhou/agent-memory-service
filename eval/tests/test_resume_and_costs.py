import asyncio
from argparse import Namespace
from datetime import datetime, timezone
from types import SimpleNamespace

import pytest

import answer_and_judge
import costs
import report
from common import jsonl
from common.llm import LLM, BudgetExceeded, LLMOutputError, LLMResult, Ledger, cost_usd, is_peak, usage_tokens
from common.protocol import guard


def test_torn_jsonl_is_repaired_before_append(tmp_path):
    path = tmp_path / "results.jsonl"
    jsonl.append(path, {"qid": "q1", "text": "中文"})
    with path.open("a") as f:
        f.write('{"qid":"q2"')
    assert jsonl.keys(path) == {"q1"}
    jsonl.append(path, {"qid": "q3"})
    assert jsonl.keys(path) == {"q1", "q3"}


def test_jsonl_keeps_complete_row_without_final_newline(tmp_path):
    path = tmp_path / "results.jsonl"
    path.write_text('{"qid":"q1"}')
    jsonl.append(path, {"qid": "q2"})
    assert jsonl.keys(path) == {"q1", "q2"}


def test_jsonl_does_not_hide_middle_corruption(tmp_path):
    path = tmp_path / "results.jsonl"
    path.write_text('{broken}\n{"qid":"q2"}\n')
    with pytest.raises(ValueError, match="corrupt"):
        list(jsonl.read(path))


def test_changed_conditions_cannot_resume(tmp_path):
    path = tmp_path / "protocol.json"
    guard(path, {"sessions": 2})
    guard(path, {"sessions": 2})
    with pytest.raises(RuntimeError, match="sessions"):
        guard(path, {"sessions": None})


def test_usage_fallback_and_peak_boundaries(tmp_path):
    assert usage_tokens({"prompt_tokens": 120, "prompt_cache_hit_tokens": 20,
                         "completion_tokens": 30}) == (20, 100, 30, 0)
    weekday = datetime(2026, 10, 9, 1, tzinfo=timezone.utc)
    assert is_peak(weekday)
    assert not is_peak(weekday.replace(hour=4))
    assert not is_peak(weekday.replace(day=10))
    assert cost_usd(100, 200, 30, weekday) == 2 * cost_usd(100, 200, 30, weekday.replace(hour=4))
    ledger = Ledger(tmp_path / "cost.jsonl")
    ledger.record("fake.ingest", 100, 200, 30, ts=weekday, calls=3)
    summary = costs.summarize(list(jsonl.read(ledger.path)))
    assert summary["components"]["fake.ingest"]["peak_calls"] == 3
    assert summary["total"]["calls"] == 3


def test_invalid_judge_label_retries_only_judge(tmp_path):
    class FakeLLM:
        def __init__(self):
            self.calls = []
            self.label = "MAYBE"
        async def chat(self, messages, *, component, **kwargs):
            self.calls.append(component)
            if component == "answer":
                return LLMResult(text="7 May 2023", miss=20, out=5, usd=.001)
            return LLMResult(text='{"label":"CORRECT"}', json={"label": self.label})
    llm = FakeLLM()
    args = Namespace(answer_prompt="unified", answer_max_tokens=20, judge_max_tokens=20)
    rec = {"qid": "q1", "conv": "conv-26", "category": 2, "question": "When?",
           "answer": "7 May 2023", "context": "Some context"}
    out = tmp_path / "answers-unified.jsonl"
    with pytest.raises(LLMOutputError, match="invalid judge"):
        asyncio.run(answer_and_judge.process(llm, rec, args, "answer", "judge", out))
    assert not list(jsonl.read(out))
    llm.label = "CORRECT"
    asyncio.run(answer_and_judge.process(llm, rec, args, "answer", "judge", out))
    assert llm.calls == ["answer", "judge", "judge"]
    assert list(jsonl.read(out))[0]["J"] == 1


def test_queued_llm_calls_recheck_budget_and_disable_thinking(tmp_path, monkeypatch):
    monkeypatch.setenv("DEEPSEEK_API_KEY", "offline-test-key")
    ledger = Ledger(tmp_path / "costs.jsonl")
    calls = []
    async def fake_create(**kwargs):
        calls.append(kwargs)
        await asyncio.sleep(0)
        return SimpleNamespace(usage={"prompt_tokens": 1000, "completion_tokens": 100},
                               choices=[SimpleNamespace(message=SimpleNamespace(content='{"ok":true}',
                                                        reasoning_content=None), finish_reason="stop")])
    async def run():
        llm = LLM(concurrency=1, ledger=ledger, budget_usd=1e-8)
        monkeypatch.setattr(llm.client.chat.completions, "create", fake_create)
        try:
            results = await asyncio.gather(*(llm.chat([{"role": "user", "content": "JSON please"}],
                        component="offline", json_mode=True) for _ in range(3)), return_exceptions=True)
            assert sum(isinstance(r, BudgetExceeded) for r in results) == 2
        finally:
            await llm.aclose()
    asyncio.run(run())
    assert len(calls) == ledger.calls == 1
    assert calls[0]["extra_body"] == {"thinking": {"type": "disabled"}}
    assert calls[0]["temperature"] == 0
    assert calls[0]["response_format"] == {"type": "json_object"}


def test_changed_context_does_not_reuse_paid_answer(tmp_path):
    class FakeLLM:
        def __init__(self):
            self.calls = []
        async def chat(self, messages, *, component, **kwargs):
            self.calls.append(component)
            return LLMResult(text="answer", json={"label": "CORRECT"})
    llm = FakeLLM()
    args = Namespace(answer_prompt="unified", answer_max_tokens=20, judge_max_tokens=20)
    rec = {"qid": "q1", "conv": "conv-26", "category": 2, "question": "When?",
           "answer": "7 May 2023", "context": "old context"}
    out = tmp_path / "answers.jsonl"
    asyncio.run(answer_and_judge.process(llm, rec, args, "answer", "judge", out))
    rec["context"] = "changed context"
    asyncio.run(answer_and_judge.process(llm, rec, args, "answer", "judge", out))
    assert llm.calls == ["answer", "judge", "answer", "judge"]


def test_summary_and_report_exclude_stale_answers(tmp_path, monkeypatch):
    class FakeLLM:
        async def chat(self, messages, *, component, **kwargs):
            return LLMResult(text="7 May 2023", json={"label": "CORRECT"})
    args = Namespace(answer_prompt="unified", answer_max_tokens=20, judge_max_tokens=20)
    rec = {"system": "offline", "setting": "main", "qid": "q1", "conv": "conv-26", "category": 2,
           "question": "When?", "answer": "7 May 2023", "context": "old context",
           "context_tokens": 3, "search_ms": 5, "n_memories": 1, "n_turns": 0}
    path = tmp_path / "offline/main/answers-unified.jsonl"
    asyncio.run(answer_and_judge.process(FakeLLM(), rec, args, "answer", "judge", path))
    summary = answer_and_judge.summarize(path, [rec], "offline", "main", "unified", tmp_path / "ledger.jsonl")
    assert summary["overall"]["J"] == summary["overall"]["F1"] == 1
    assert summary["by_category"]["2-temporal"]["n"] == 1
    import json
    (path.parent / "summary-unified.json").write_text(json.dumps(summary))
    monkeypatch.setattr("sys.argv", ["report.py", "--results-dir", str(tmp_path)])
    report.main()
    assert (tmp_path / "report.md").exists() and (tmp_path / "report.json").exists()
    rec["context"] = "new context"
    stale = answer_and_judge.summarize(path, [rec], "offline", "main", "unified", tmp_path / "ledger.jsonl")
    assert stale["n_answered"] == 0
