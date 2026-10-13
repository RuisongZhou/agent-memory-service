from common import usage as usage_ledger
import pilot_plan


def test_pilot_manifest_only_uses_ingested_evidence():
    plan = pilot_plan.build()
    assert plan["utterances"] == 35 and plan["memory_turns"] == 18 and plan["mem0_adds"] == 36
    assert len(plan["qids"]) == 10
    assert "conv-26-q7" not in plan["qids"]


def test_per_add_usage_includes_retries():
    rows = [{"component": "mem0-pilot.add", "add_id": "a", "hit": 10, "miss": 100, "out": 20},
            {"component": "mem0-pilot.add", "add_id": "a", "hit": 20, "miss": 120, "out": 30},
            {"component": "mem0-pilot.add", "add_id": "b", "hit": 0, "miss": 150, "out": 40}]
    adds = usage_ledger.summarize_adds(rows, "mem0-pilot.add")
    assert adds["n_adds"] == 2
    assert adds["adds"][0]["calls"] == 2 and adds["adds"][0]["miss"] == 220
