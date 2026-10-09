import asyncio
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone

import pytest

import memory_run
from common import jsonl
from common.llm import Ledger, cost_usd, set_ledger_path


def usage(mid, *, ts="2026-10-09T02:30:00Z", stage="P1", valid=True):
    return {"mid": mid, "createdAt": ts, "stage": stage, "model": "deepseek-flash",
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
        raise AssertionError("billing must never read process counters")


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
    assert all(r["source"] == "daemon/llm_usage" and r["calls"] == 1 for r in rows)
    assert rows[0]["peak"] and not rows[-1]["peak"]
    assert rows[0]["usd"] == 2 * rows[-1]["usd"]
    assert rows[1]["parse_error"] is True and rows[1]["attempt"] == 1
    assert rows[1]["component"] == "en.daemon/P3"


def test_ledger_event_id_dedup_is_atomic_across_writers(tmp_path):
    path = tmp_path / "ledger.jsonl"
    ledgers = [Ledger(path), Ledger(path)]
    entry = {"component": "en.daemon/P1", "hit": 20, "miss": 100, "out": 30,
             "event_id": "memory/conv-26-en/u1", "ts": datetime(2026, 10, 9, 2, tzinfo=timezone.utc)}
    with ThreadPoolExecutor(max_workers=2) as pool:
        lists = list(pool.map(lambda ledger: ledger.record_many([entry]), ledgers))
    assert sum(map(len, lists)) == len(list(jsonl.read(path))) == 1


@pytest.mark.parametrize("field,value", [("createdAt", "not-a-date"), ("mid", ""), ("cacheMissTokens", -1)])
def test_invalid_persisted_usage_stops_collection(tmp_path, field, value):
    set_ledger_path(tmp_path / "ledger.jsonl")
    record = usage("u1")
    record[field] = value
    collector = memory_run.UsagePoller(ExportDaemon({"conv-26-en": [record]}), "en", 60)
    collector.tenants = ["conv-26-en"]
    with pytest.raises(memory_run.DaemonError):
        asyncio.run(collector.flush())
    assert not list(jsonl.read(tmp_path / "ledger.jsonl"))


def test_source_price_matches_verified_rates(tmp_path):
    set_ledger_path(tmp_path / "ledger.jsonl")
    record = usage("u1")
    record.update(estimatedUsd=cost_usd(20, 100, 30, datetime(2026, 10, 9, 2, tzinfo=timezone.utc)),
                  pricingPeakUtc=True, receivedAt="2026-10-09T02:30:01Z")
    collector = memory_run.UsagePoller(ExportDaemon({"conv-26-en": [record]}), "en", 60)
    collector.tenants = ["conv-26-en"]
    asyncio.run(collector.flush())
    assert list(jsonl.read(tmp_path / "ledger.jsonl"))[0]["received_at"] == record["receivedAt"]
    record["mid"] = "u2"
    record["estimatedUsd"] = .1
    with pytest.raises(memory_run.DaemonError, match="pricing mismatch"):
        asyncio.run(collector.flush())
