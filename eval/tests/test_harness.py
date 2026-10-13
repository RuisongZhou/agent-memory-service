import asyncio
import json
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


@pytest.fixture
def daemon(tmp_path):
    srv, state = start()
    set_ledger_path(tmp_path / "usage_ledger.jsonl")
    yield f"http://127.0.0.1:{srv.server_port}", state
    srv.shutdown()
    srv.server_close()


def test_full_fake_http_pipeline_and_resume(tmp_path, daemon):
    url, state = daemon
    args = args_for(tmp_path, url)
    asyncio.run(memory_run.amain(args))
    assert not state.errors
    expected = sum(len(locomo.turns(s)) for s in locomo.sessions(locomo.load()[0])[:2])
    assert len(state.turns["conv-26"]) == expected
    assert state.sessions_ended["conv-26"] == ["session_1", "session_2"]
    root = tmp_path / args.tag
    buffer = list(jsonl.read(root / "buffer/contexts.jsonl"))
    main = list(jsonl.read(root / "main/contexts.jsonl"))
    assert len(buffer) == len(main) == 3
    assert all(r["n_turns"] == 2 for r in buffer) and all(r["n_turns"] == 0 for r in main)
    assert "User profile: PROFILE TEXT" in main[0]["context"]
    # A completed run resumes without repeating any mutating request.
    mutating = {"/v1/add_memories", "/v1/search_memories", "/admin/run_jobs", "/admin/wait_idle"}
    before = sum(path in mutating for path, *_ in state.calls)
    asyncio.run(memory_run.amain(args))
    assert sum(path in mutating for path, *_ in state.calls) == before


def test_two_prompt_variants_are_isolated_on_one_daemon(tmp_path, daemon):
    url, state = daemon
    for variant in ("en", "zh"):
        args = args_for(tmp_path, url)
        args.tag = f"fake-{variant}"
        args.prompt_variant = variant
        asyncio.run(memory_run.amain(args))
        protocol = json.loads((tmp_path / args.tag / "protocol.json").read_text())
        assert protocol["prompt_variant"] == variant
    assert set(state.turns) == set(state.purged) == {"conv-26-en", "conv-26-zh"}
    assert not state.errors


@pytest.mark.parametrize("override", [{"failedTurns": 1}, {"indexTasks": 1}])
def test_failed_or_pending_ingestion_is_not_marked_complete(tmp_path, override):
    class Daemon:
        async def post(self, path, tenant, body, **kwargs):
            if path == "/admin/wait_idle":
                return {"status": "idle", "failedTurns": 0, "failedRequests": 0, "failedExperiences": 0,
                        "pendingTurns": 0, "pendingJobs": 0, "indexTasks": 0, **override}
            return {"status": "queued", "duplicate": False}
    conv = locomo.load()[0]
    runner = memory_run.Runner(args_for(tmp_path, "http://unused"), Daemon(), None, [conv])
    with pytest.raises(memory_run.DaemonError):
        asyncio.run(runner.ingest_conv(conv, asyncio.Semaphore(1)))
    assert not runner._done()


def test_wait_idle_repolls_window_timeout_until_real_idle(tmp_path):
    class Daemon:
        calls = 0
        async def post(self, path, tenant, body, **kwargs):
            self.calls += 1
            if self.calls == 1:
                return {"status": "timeout", "pendingTurns": 3}
            return {"status": "idle", "failedTurns": 0, "failedRequests": 0, "failedExperiences": 0,
                    "pendingTurns": 0, "pendingJobs": 0, "indexTasks": 0}
    daemon = Daemon()
    runner = memory_run.Runner(args_for(tmp_path, "http://unused"), daemon, None, locomo.load()[:1])
    assert asyncio.run(runner.wait_idle("conv-26"))["status"] == "idle"
    assert daemon.calls == 2
