from argparse import Namespace
from types import SimpleNamespace
import hashlib
import json

import pytest

import mem0_run
from common import jsonl, locomo
from common.llm import set_ledger_path


def test_mem0_real_class_disables_thinking_and_records_usage(tmp_path, monkeypatch):
    monkeypatch.setenv("DEEPSEEK_API_KEY", "test-not-a-real-key")
    monkeypatch.delenv("OPENROUTER_API_KEY", raising=False)
    ledger = set_ledger_path(tmp_path / "ledger.jsonl")
    _, cls = mem0_run._install_mem0_patches(True)
    calls = []
    response = SimpleNamespace(usage={"prompt_tokens": 120, "completion_tokens": 10,
                                     "prompt_cache_hit_tokens": 20},
                               choices=[SimpleNamespace(message=SimpleNamespace(content='{"memory":[]}',
                                                        reasoning_content=None), finish_reason="stop")])
    llm = cls({"model": "deepseek-flash", "openai_base_url": "https://api.deepseek.com",
               "api_key": "test-not-a-real-key", "temperature": 0,
               "response_callback": mem0_run._ledger_callback("offline.add")})
    def fake_create(**kwargs):
        calls.append(kwargs)
        return response
    monkeypatch.setattr(llm.client.chat.completions, "create", fake_create)
    output = llm.generate_response([{"role": "user", "content": "Return JSON"}],
                                  response_format={"type": "json_object"},
                                  extra_body={"thinking": {"type": "enabled"}})
    assert output == '{"memory":[]}'
    assert calls[0]["extra_body"] == {"thinking": {"type": "disabled"}}
    assert calls[0]["temperature"] == 0
    assert calls[0]["model"] == "deepseek-flash"
    assert ledger.calls == 1
    assert list(jsonl.read(ledger.path))[0]["miss"] == 100
    llm.client.close()


def test_observation_date_patch_uses_session_date():
    mm, _ = mem0_run._install_mem0_patches(True)
    import mem0.memory.main as main
    mem0_run._tls.observation_date = "2023-05-08"
    prompt = main.generate_additive_extraction_prompt(existing_memories=[], new_messages="Yesterday I ran",
                                                       last_k_messages=[], custom_instructions=None)
    assert "2023-05-08" in prompt
    mem0_run._tls.observation_date = None


def test_mem0_protocol_uses_two_perspectives_and_odd_batch():
    conv = locomo.load()[0]
    ps = mem0_run.perspectives(conv)
    assert len(ps) == 2
    a = list(mem0_run.batches_for(conv, ps[0], 2, 2))
    b = list(mem0_run.batches_for(conv, ps[1], 2, 2))
    assert len(a) == len(b)
    for ba, bb in zip(a, b):
        assert ba[0].key == bb[0].key
        assert all(x["content"] == y["content"] and x["role"] != y["role"]
                   for x, y in zip(ba[2], bb[2]))
        assert len(ba[2]) in (1, 2)


def test_search_refuses_partially_ingested_session(tmp_path):
    args = Namespace(setting=None, top_k=30, results_dir=str(tmp_path), tag="mem0-offline",
                     perspectives="both", sessions=2, batch_size=2)
    progress = tmp_path / args.tag / "ingest_progress.jsonl"
    p = mem0_run.perspectives(locomo.load()[0])[0]
    jsonl.append(progress, {"user_id": p.user_id, "session": "session_1", "batch": 0, "ok": True})
    with pytest.raises(RuntimeError, match="batches not ingested"):
        mem0_run.run_search(args, locomo.load()[:1], None)


def test_real_mem0_qdrant_pipeline_two_sessions_ten_questions(tmp_path, monkeypatch):
    """Real mem0 and Qdrant; every model response is intercepted in-process."""
    monkeypatch.setenv("DEEPSEEK_API_KEY", "offline-test-key")
    monkeypatch.delenv("OPENROUTER_API_KEY", raising=False)
    from openai.resources.chat.completions import Completions
    from openai.resources.embeddings import Embeddings
    calls = []
    def fake_chat(self, **kwargs):
        assert kwargs["extra_body"]["thinking"]["type"] == "disabled"
        calls.append(kwargs)
        text = "Offline fact " + hashlib.sha256(json.dumps(kwargs["messages"]).encode()).hexdigest()[:12]
        content = json.dumps({"memory": [{"text": text}]})
        return SimpleNamespace(usage={"prompt_tokens": 100, "completion_tokens": 10},
                               choices=[SimpleNamespace(message=SimpleNamespace(content=content,
                                                       reasoning_content=None), finish_reason="stop")])
    def fake_embed(self, **kwargs):
        assert kwargs["model"] == "bge-m3" and kwargs["dimensions"] == 1024
        # Distinct unit-ish vectors are enough to verify persistence and retrieval.
        data = []
        for i, text in enumerate(kwargs["input"]):
            vector = [0.] * 1024
            vector[int(hashlib.sha256(text.encode()).hexdigest()[:4], 16) % 1024] = 1.
            vector[0] = 1.
            data.append(SimpleNamespace(index=i, embedding=vector))
        return SimpleNamespace(data=data)
    monkeypatch.setattr(Completions, "create", fake_chat)
    monkeypatch.setattr(Embeddings, "create", fake_embed)
    ledger = set_ledger_path(tmp_path / "cost_ledger.jsonl")
    args = Namespace(observation_date="session", stores_dir=str(tmp_path / "stores"), tag="offline",
                     embed_url="http://127.0.0.1:8773/v1", no_custom_instructions=False,
                     sessions=2, batch_size=2, perspectives="both", workers=2, retries=1,
                     max_usd=None, results_dir=str(tmp_path), top_k=30, setting=None,
                     limit=10, qids=None)
    data = locomo.load()[:1]
    stores = mem0_run.Mem0Stores(args)
    try:
        mem0_run.run_ingest(args, data, stores)
        mem0_run.run_search(args, data, stores)
        count = len(calls)
        mem0_run.run_ingest(args, data, stores)
        mem0_run.run_search(args, data, stores)
        assert len(calls) == count  # completed batches/questions are not duplicated
    finally:
        stores.close()
    rows = list(jsonl.read(tmp_path / "offline/k30/contexts.jsonl"))
    assert len(rows) == 10
    assert all(r["n_memories"] > 0 for r in rows)
    assert ledger.calls == len(calls)
    recorded = list(jsonl.read(ledger.path))
    assert all(r.get("add_id") and r.get("session") and r.get("user_id") and r.get("attempt") for r in recorded)
    progress = list(jsonl.read(tmp_path / "offline/ingest_progress.jsonl"))
    assert all(r["cost"]["llm_calls"] == 1 and r["cost"]["usd"] > 0 for r in progress)
    assert all(r["speaker_1"] == "Caroline" and r["speaker_2"] == "Melanie" for r in rows)
    assert all(r["token_counter"] == "tiktoken/cl100k_base" for r in rows)
    reopened = mem0_run.Mem0Stores(args)
    try:
        p = mem0_run.perspectives(data[0])[0]
        assert reopened.get(p).get_all(filters={"user_id": p.user_id})["results"]
    finally:
        reopened.close()
