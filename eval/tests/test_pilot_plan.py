import costs
import pilot_plan


def test_pilot_manifest_only_uses_ingested_evidence():
    plan = pilot_plan.build()
    assert plan["utterances"] == 35 and plan["memory_turns"] == 18 and plan["mem0_adds"] == 36
    assert len(plan["qids"]) == 10
    assert set(plan["qids"]) >= {"conv-26-q82", "conv-26-q83", "conv-26-q84"}
    assert "conv-26-q7" not in plan["qids"]
    assert plan["mem0_ingest_extrapolation"] is None
    assert plan["paid_calls_performed_by_this_script"] == 0


def test_per_add_cost_includes_retries_and_extrapolation_is_report_only():
    rows = [{"component": "mem0-pilot.add", "add_id": "a", "usd": .001, "hit": 10, "miss": 100, "out": 20},
            {"component": "mem0-pilot.add", "add_id": "a", "usd": .002, "hit": 20, "miss": 120, "out": 30},
            {"component": "mem0-pilot.add", "add_id": "b", "usd": .001, "hit": 0, "miss": 150, "out": 40}]
    adds = costs.summarize_adds(rows, "mem0-pilot.add")
    assert adds["n_adds"] == 2 and adds["usd_per_add"] == .002
    assert adds["adds"][0]["calls"] == 2
    plan = pilot_plan.build(ledger_rows=rows)
    ex = plan["mem0_ingest_extrapolation"]
    assert ex["conv_adds"] == 428 and ex["dataset_adds"] == 6022
    assert ex["conv_ingest_usd"] == .002 * 428
    assert ex["automatic_full_run"] is False and ex["includes_answers_and_judges"] is False
