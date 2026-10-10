import usage_stats
import pilot_plan


def test_pilot_manifest_only_uses_ingested_evidence():
    plan = pilot_plan.build()
    assert plan["utterances"] == 35 and plan["memory_turns"] == 18 and plan["mem0_adds"] == 36
    assert len(plan["qids"]) == 10
    assert set(plan["qids"]) >= {"conv-26-q82", "conv-26-q83", "conv-26-q84"}
    assert "conv-26-q7" not in plan["qids"]
    assert plan["model_calls_performed_by_this_script"] == 0


def test_per_add_usage_includes_retries_without_extrapolation():
    rows = [{"component": "mem0-pilot.add", "add_id": "a", "hit": 10, "miss": 100, "out": 20},
            {"component": "mem0-pilot.add", "add_id": "a", "hit": 20, "miss": 120, "out": 30},
            {"component": "mem0-pilot.add", "add_id": "b", "hit": 0, "miss": 150, "out": 40}]
    adds = usage_stats.summarize_adds(rows, "mem0-pilot.add")
    assert adds["n_adds"] == 2
    assert adds["adds"][0]["calls"] == 2 and adds["adds"][0]["miss"] == 220
    assert "usd" not in adds and "usd_per_add" not in adds
    plan = pilot_plan.build()
    assert "budget_usd" not in plan and "mem0_ingest_extrapolation" not in plan
