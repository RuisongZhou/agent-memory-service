import asyncio
from argparse import Namespace

import pytest

import memory_run
from common import jsonl, locomo
from common.llm import set_ledger_path
from fake_memory import start


def args_for(tmp_path, base_url):
    return Namespace(base_url=base_url, tag="fake-plumbing", stage="all", convs=["conv-26"],
                     sessions=2, limit=3, qids=None, conv_concurrency=2, search_concurrency=1,
                     top_k=10, token_budget=2000, retention_days=0, memory_order="time",
                     force_purge=False, timeout=10, wait_timeout=10, usage_poll=60,
                     results_dir=str(tmp_path))


def test_full_fake_http_pipeline_and_resume(tmp_path):
    srv, state = start()
    args = args_for(tmp_path, f"http://127.0.0.1:{srv.server_port}")
    set_ledger_path(tmp_path / "usage_ledger.jsonl")
    try:
        asyncio.run(memory_run.amain(args))
        assert not state.errors
        expected = sum(len(locomo.turns(s)) for s in locomo.sessions(locomo.load()[0])[:2])
        assert len(state.turns["conv-26"]) == expected
        assert state.sessions_ended["conv-26"] == ["session_1", "session_2"]
        root = tmp_path / args.tag
        buffer = list(jsonl.read(root / "buffer/contexts.jsonl"))
        main = list(jsonl.read(root / "main/contexts.jsonl"))
        assert len(buffer) == len(main) == 3
        assert all(r["n_profiles"] == 1 and r["n_turns"] == 2 for r in buffer)
        assert all(r["n_turns"] == 0 for r in main)
        assert "User profile: PROFILE TEXT" in main[0]["context"]
        assert "conflicting" in main[0]["context"] and "old value" in main[0]["context"]
        mutating = {"/v1/add_memories", "/v1/search_memories", "/admin/run_jobs", "/admin/wait_idle"}
        mutations_before = sum(p in mutating for p, *_ in state.calls)
        asyncio.run(memory_run.amain(args))
        assert sum(p in mutating for p, *_ in state.calls) == mutations_before
        assert len(list(jsonl.read(root / "main/contexts.jsonl"))) == 3
        ledger = list(jsonl.read(tmp_path / "usage_ledger.jsonl"))
        assert sum(r["calls"] for r in ledger) == state.llm["calls"]
    finally:
        srv.shutdown()
        srv.server_close()


def test_main_without_purge_is_an_error(tmp_path):
    args = args_for(tmp_path, "http://unused")
    class Poller:
        async def set_stage(self, stage):
            pass
    runner = memory_run.Runner(args, None, Poller(), locomo.select(locomo.load(), args.convs))
    with pytest.raises(memory_run.DaemonError, match="not purged"):
        asyncio.run(runner.stage_search("main"))


def test_purge_refuses_missing_buffer_questions(tmp_path):
    args = args_for(tmp_path, "http://unused")
    class Poller:
        async def set_stage(self, stage):
            pass
    runner = memory_run.Runner(args, None, Poller(), locomo.select(locomo.load(), args.convs))
    with pytest.raises(SystemExit, match="refusing to purge"):
        asyncio.run(runner.run_jobs("purge", ["purge"], {"retentionDays": 0}))


def test_profile_field_and_legacy_profile_are_deduplicated():
    profile = {"id": "p", "type": "profile", "text": "A profile", "scope": {"kind": "global"}}
    context, info = memory_run.format_context({"profiles": [profile], "results": [{"memory": profile}]})
    assert info["n_profiles"] == 1
    assert context == "User profile: A profile"


def test_two_prompt_variants_are_isolated_on_one_daemon(tmp_path):
    srv, state = start()
    set_ledger_path(tmp_path / "usage_ledger.jsonl")
    try:
        for variant in ("en", "zh"):
            args = args_for(tmp_path, f"http://127.0.0.1:{srv.server_port}")
            args.tag = f"fake-{variant}"
            args.prompt_variant = variant
            asyncio.run(memory_run.amain(args))
        assert set(state.turns) == {"conv-26-en", "conv-26-zh"}
        assert set(state.purged) == {"conv-26-en", "conv-26-zh"}
        assert len(state.turns["conv-26-en"]) == len(state.turns["conv-26-zh"]) > 0
        for path, tenant, _, body in state.calls:
            if path in ("/v1/add_memories", "/v1/search_memories"):
                assert body["scope"]["project"] == "conv-26"
                assert tenant in ("conv-26-en", "conv-26-zh")
        for variant in ("en", "zh"):
            root = tmp_path / f"fake-{variant}"
            import json
            protocol = json.loads((root / "protocol.json").read_text())
            assert protocol["prompt_variant"] == variant
            assert protocol["tenant_mode"] == "variant-suffix"
            rows = list(jsonl.read(root / "main/contexts.jsonl"))
            assert all(r["tenant"] == f"conv-26-{variant}" and r["prompt_variant"] == variant for r in rows)
        assert not state.errors
    finally:
        srv.shutdown()
        srv.server_close()


def test_legacy_resume_refuses_changed_daemon_default(tmp_path):
    srv, state = start()
    args = args_for(tmp_path, f"http://127.0.0.1:{srv.server_port}")
    set_ledger_path(tmp_path / "usage_ledger.jsonl")
    try:
        asyncio.run(memory_run.amain(args))
        before = len(state.calls)
        state.default_prompt = "zh"
        with pytest.raises(RuntimeError, match="prompt_variant"):
            asyncio.run(memory_run.amain(args))
        assert len(state.calls) == before
    finally:
        srv.shutdown()
        srv.server_close()


def test_reported_tenant_prompt_must_match_variant(tmp_path):
    args = args_for(tmp_path, "http://unused")
    args.prompt_variant = args.actual_prompt_variant = "en"
    runner = memory_run.Runner(args, None, None, locomo.select(locomo.load(), args.convs))
    with pytest.raises(memory_run.DaemonError, match="expected 'en'"):
        runner.validate_prompt_health({"promptVariant": "zh", "tenantPromptVariants": {"conv-26-en": "zh"}})
    with pytest.raises(memory_run.DaemonError, match="cannot be verified"):
        runner.validate_prompt_health({"promptVariant": "en"})


@pytest.mark.parametrize("override", [{"status": "timeout"}, {"failedTurns": 1}, {"failedRequests": 1},
                                      {"failedExperiences": 1}, {"indexTasks": 1}])
def test_http_200_with_failed_or_pending_ingestion_is_not_marked_complete(tmp_path, override):
    class Daemon:
        async def post(self, path, tenant, body, **kwargs):
            if path == "/admin/wait_idle":
                return {"status": "idle", "failedTurns": 0, "failedRequests": 0, "failedExperiences": 0,
                        "pendingTurns": 0, "pendingJobs": 0, "indexTasks": 0, **override}
            return {"status": "queued", "duplicate": False}
    args = args_for(tmp_path, "http://unused")
    if override.get("status") == "timeout":
        args.wait_timeout = 0.02
    conv = locomo.load()[0]
    runner = memory_run.Runner(args, Daemon(), None, [conv])
    with pytest.raises(memory_run.DaemonError):
        asyncio.run(runner.ingest_conv(conv, asyncio.Semaphore(1)))
    assert not runner._done()


def test_main_requires_actual_empty_store_even_with_purge_checkpoint(tmp_path):
    class Daemon:
        async def post(self, path, tenant, body, **kwargs):
            assert path == "/admin/stats"
            return {"counts": {"turns": 1}}
    class Poller:
        async def set_stage(self, stage):
            pass
    args = args_for(tmp_path, "http://unused")
    runner = memory_run.Runner(args, Daemon(), Poller(), locomo.load()[:1])
    runner._mark("conv-26", "purge")
    with pytest.raises(memory_run.DaemonError, match="remainingTurns=0"):
        asyncio.run(runner.stage_search("main"))


def test_pilot_as_of_uses_last_ingested_session(tmp_path):
    args = args_for(tmp_path, "http://unused")
    conv = locomo.load()[0]
    runner = memory_run.Runner(args, None, None, [conv])
    from datetime import timedelta
    expected = locomo.rfc3339(locomo.sessions(conv)[1].start + timedelta(days=1))
    assert runner.as_of(conv) == expected
    assert runner.as_of(conv) != locomo.rfc3339(locomo.as_of(conv))


def test_incompatible_qid_manifest_is_rejected_before_requests(tmp_path):
    args = args_for(tmp_path, "http://unused")
    args.qids = ["conv-26-0"]
    with pytest.raises(SystemExit, match="unknown or excluded qids"):
        asyncio.run(memory_run.amain(args))


def test_wait_idle_repolls_window_timeout_until_real_idle(tmp_path):
    class Daemon:
        calls = 0
        async def post(self, path, tenant, body, **kwargs):
            self.calls += 1
            if self.calls == 1:
                return {"status": "timeout", "pendingTurns": 3}
            return {"status": "idle", "failedTurns": 0, "failedRequests": 0, "failedExperiences": 0,
                    "pendingTurns": 0, "pendingJobs": 0, "indexTasks": 0}
    args = args_for(tmp_path, "http://unused")
    args.wait_timeout = 1
    daemon = Daemon()
    runner = memory_run.Runner(args, daemon, None, locomo.load()[:1])
    assert asyncio.run(runner.wait_idle("conv-26"))["status"] == "idle"
    assert daemon.calls == 2
