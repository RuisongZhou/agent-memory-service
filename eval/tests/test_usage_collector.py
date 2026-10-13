import asyncio
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone

import memory_run
from common import jsonl
from common.llm import Ledger, set_ledger_path


def usage(mid, *, ts="2026-10-09T02:30:00Z", stage="P1", valid=True):
    return {"mid": mid, "createdAt": ts, "stage": stage, "model": "qwen3.8-flash",
            "promptTokens": 120, "cacheHitTokens": 20, "cacheMissTokens": 100,
            "completionTokens": 30, "attempt": 1, "latencyMs": 15, "validJson": valid}


class ExportDaemon:
    def __init__(self, records):
        self.records = records
        self.calls = []
    async def post(self, path, tenant, body, **kwargs):
        assert path == "/admin/export" and body == {"table": "llm_usage"}
        self.calls.append(tenant)
        return {"records": self.records[tenant]}
    async def health(self):
        raise AssertionError("usage must never read process counters")


def test_persisted_collector_recovers_restart_without_cross_tenant_mix(tmp_path):
    path = tmp_path / "ledger.jsonl"
    set_ledger_path(path)
    daemon = ExportDaemon({"conv-26-en": [usage("shared-id")],
                           "conv-26-zh": [usage("shared-id", ts="2026-10-09T12:30:00Z")]})
    first = memory_run.UsagePoller(daemon, "en", 60)
    first.tenants = ["conv-26-en"]
    asyncio.run(first.flush())
    daemon.records["conv-26-en"].append(usage("second", stage="P3", valid=False))
    # New harness and ledger instance after a daemon restart. Persisted records
    # survive; an old counter checkpoint deliberately contains unusable content.
    checkpoint = tmp_path / "old-checkpoint.json"
    checkpoint.write_text("obsolete counter snapshot")
    set_ledger_path(path)
    resumed = memory_run.UsagePoller(daemon, "en", 60, checkpoint)
    resumed.tenants = ["conv-26-en"]
    asyncio.run(resumed.flush())
    asyncio.run(resumed.flush())
    chinese = memory_run.UsagePoller(daemon, "zh", 60)
    chinese.tenants = ["conv-26-zh"]
    asyncio.run(chinese.flush())
    rows = list(jsonl.read(path))
    assert len(rows) == 3
    assert {r["event_id"] for r in rows} == {"memory/conv-26-en/shared-id", "memory/conv-26-en/second",
                                              "memory/conv-26-zh/shared-id"}
    assert rows[1]["parse_error"] is True and rows[1]["component"] == "en.daemon/P3"


def test_ledger_event_id_dedup_is_atomic_across_writers(tmp_path):
    path = tmp_path / "ledger.jsonl"
    ledgers = [Ledger(path), Ledger(path)]
    entry = {"component": "en.daemon/P1", "hit": 20, "miss": 100, "out": 30,
             "event_id": "memory/conv-26-en/u1", "ts": datetime(2026, 10, 9, 2, tzinfo=timezone.utc)}
    with ThreadPoolExecutor(max_workers=2) as pool:
        lists = list(pool.map(lambda ledger: ledger.record_many([entry]), ledgers))
    assert sum(map(len, lists)) == len(list(jsonl.read(path))) == 1
