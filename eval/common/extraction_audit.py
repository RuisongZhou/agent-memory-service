#!/usr/bin/env python3
"""Summarize formation rule checks without making generative-model requests.

Read saved /admin/export {"table":"extract_log"} JSON or JSONL files with --input;
optionally inspect a running daemon using --base-url and repeatable --tenant. Default
output contains counts, protocol metadata and timestamp checks, never source excerpts.
Rule-valid output is not a measurement of factual correctness or QA effectiveness.
"""
from __future__ import annotations

import argparse
from collections import Counter, defaultdict
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
from urllib.request import Request, urlopen

VALIDATION_STAGES = {"P1_validation": "P1", "P1-b_validation": "P1-b", "P2_validation": "P2"}


def parse_time(value):
    if not isinstance(value, str) or not value:
        return None
    try:
        result = datetime.fromisoformat(value.replace("Z", "+00:00"))
        return result.replace(tzinfo=timezone.utc) if result.tzinfo is None else result.astimezone(timezone.utc)
    except ValueError:
        return None


def read_json(path):
    text = Path(path).read_text(encoding="utf-8")
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        return [json.loads(line) for line in text.splitlines() if line.strip()]


def records(payload, field="records"):
    if isinstance(payload, list):
        return payload
    if isinstance(payload, dict):
        if isinstance(payload.get(field), list):
            return payload[field]
        return [payload]
    raise ValueError("expected an object or list of objects")


def api(base_url, tenant, body=None, path="/admin/export"):
    headers = {"X-Memory-Tenant": tenant, "X-Memory-Platform": "audit", "Content-Type": "application/json"}
    request = Request(base_url.rstrip("/") + path, headers=headers,
                      data=None if body is None else json.dumps(body).encode("utf-8"))
    with urlopen(request, timeout=60) as response:
        return json.load(response)


def discarded_kind(stage, rejection):
    """Support older coarse log labels while separating relation removal from memory discard."""
    if rejection.get("kind"):
        return rejection["kind"]
    reason = str(rejection.get("reason", "unknown"))
    if "non-destructive reference removed" in reason:
        return "relation_removed"
    return "experience_discarded" if stage == "P2" else "fact_discarded"


def summarize(logs, memories=(), as_of=None, since=None):
    if since and parse_time(since) is None:
        raise ValueError("since must be an ISO date or RFC3339 timestamp")
    since = parse_time(since) if since else None
    reference = parse_time(as_of) if as_of else None
    if as_of and reference is None:
        raise ValueError("asOf must be an ISO date or RFC3339 timestamp")
    unique, missing_ids = {}, []
    for entry in logs:
        if not isinstance(entry, dict):
            raise ValueError("log entries must be objects")
        created = parse_time(entry.get("createdAt"))
        if since and (created is None or created < since):
            continue
        uid = entry.get("mid", entry.get("id"))
        if uid:
            unique[str(uid)] = entry  # export retries may repeat a row; IDs remain authoritative.
        else:
            missing_ids.append(entry)
    logs = list(unique.values()) + missing_ids
    stats = defaultdict(lambda: {
        "model_completed": 0, "model_failed": 0, "model_candidate_array_missing": 0, "raw_candidates": 0,
        "validation_groups": 0, "validated_candidates": 0,
        "facts_discarded": 0, "experiences_discarded": 0, "relations_removed": 0,
        "exact_dedupe_merged": 0, "new_experiences": 0, "same_experiences": 0,
        "discard_reasons": Counter(), "removed_relation_reasons": Counter(),
        "analysis_results": Counter(), "groups": [],
    })
    action_counts, committed_facts = Counter(), set()
    variants, input_hashes, system_hashes = Counter(), set(), defaultdict(set)
    for row in logs:
        raw_stage = row.get("stage", "unknown")
        variant = row.get("promptVariant", row.get("variant"))
        if variant:
            variants[str(variant)] += 1
        if row.get("inputHash"):
            input_hashes.add(row["inputHash"])
        if row.get("systemHash"):
            system_hashes[str(raw_stage)].add(row["systemHash"])
        if raw_stage == "relations":
            uid = row.get("mid", row.get("id", hashlib.sha256(json.dumps(row, sort_keys=True).encode()).hexdigest()))
            for action in row.get("actions", []):
                action_counts[str(action.get("action", "unknown"))] += 1
                committed_facts.add((str(uid), str(action.get("fact"))))
            continue
        stage = VALIDATION_STAGES.get(raw_stage, raw_stage)
        if stage not in ("P1", "P1-b", "P2"):
            continue
        target = stats[stage]
        if row.get("status") == "model_completed":
            target["model_completed"] += 1
            output = row.get("output", {})
            field = "experiences" if stage == "P2" else "facts"
            if isinstance(output, dict) and isinstance(output.get(field), list):
                target["raw_candidates"] += len(output[field])
            else:
                target["model_candidate_array_missing"] += 1
        elif row.get("status") == "failed":
            target["model_failed"] += 1
        if raw_stage not in VALIDATION_STAGES:
            continue
        target["validation_groups"] += 1
        inputs = row.get("inputCounts", {})
        accepted = row.get("accepted", inputs.get("afterDedupe", inputs.get("accepted")))
        if accepted is not None:
            target["validated_candidates"] += int(accepted)
        target["exact_dedupe_merged"] += int(row.get("dedupe", {}).get("mergedFacts", 0))
        target["new_experiences"] += int(row.get("created", 0))
        target["same_experiences"] += int(row.get("same", 0))
        target["analysis_results"].update(row.get("analysisResults", {}))
        rejected, removed = 0, 0
        for rejection in row.get("rejected", []):
            kind = discarded_kind(stage, rejection)
            reason = str(rejection.get("reason", "unknown"))
            if kind == "relation_removed":
                target["relations_removed"] += 1
                target["removed_relation_reasons"][reason] += 1
                removed += 1
            else:
                field = "experiences_discarded" if kind == "experience_discarded" else "facts_discarded"
                target[field] += 1
                target["discard_reasons"][reason] += 1
                rejected += 1
        target["groups"].append({
            "group": row.get("groupLabel", row.get("session")), "variant": variant,
            "sourceTurnIds": row.get("sourceTurnIds", []), "outputHash": row.get("outputHash"),
            "inputCounts": inputs, "accepted": accepted, "discarded": rejected,
            "removedRelations": removed, "dedupeMerged": row.get("dedupe", {}).get("mergedFacts"),
        })
    for value in stats.values():
        value["model_outputs_without_validation_row"] = max(0, value["model_completed"] - value["validation_groups"])
        for key in ("discard_reasons", "removed_relation_reasons", "analysis_results"):
            value[key] = dict(sorted(value[key].items()))
        value["groups"].sort(key=lambda g: (str(g["group"]), str(g["outputHash"])))
    future_access, future_recorded, future_stated, invalid_dates = 0, 0, 0, 0
    memory_types, statuses, origins = Counter(), Counter(), Counter()
    for memory in memories:
        memory_types[str(memory.get("type", "unknown"))] += 1
        statuses[str(memory.get("status", "unknown"))] += 1
        origins[str(memory.get("source", {}).get("origin", "unknown"))] += 1
        if parse_time(memory.get("effectiveAt")) is None:
            invalid_dates += 1
        if reference:
            for field, target in (("lastAccessedAt", "access"), ("recordedAt", "recorded"), ("statedAt", "stated")):
                stamp = parse_time(memory.get(field))
                if stamp and stamp > reference:
                    if target == "access":
                        future_access += 1
                    elif target == "recorded":
                        future_recorded += 1
                    else:
                        future_stated += 1
    return {
        "rule_validity_only": True, "log_rows": len(logs), "variants_in_logs": dict(sorted(variants.items())),
        "distinct_input_hashes": len(input_hashes), "system_hashes": {stage: sorted(hashes) for stage, hashes in sorted(system_hashes.items())},
        "formation": dict(sorted(stats.items())),
        "committed_relation_actions": dict(sorted(action_counts.items())), "committed_fact_items": len(committed_facts),
        "memories": {"count": len(memories), "types": dict(memory_types), "statuses": dict(statuses),
                     "origins": dict(origins), "invalid_effective_dates": invalid_dates},
        "clock": {"asOf": as_of, "future_lastAccessedAt": future_access, "future_recordedAt": future_recorded,
                  "future_statedAt": future_stated,
                  "recency_note": "Future lastAccessedAt is clamped to zero age by the existing scoring rule; this may flatten long-term recency during historical replay."},
        "notes": ["Rule validation does not establish factual correctness, P2 task classification correctness, or QA effectiveness.",
                  "A validation log precedes the memory commit for P1/P1-b; committed relation-action rows are the commit evidence.",
                  "Older P1_validation rows can include P1-b and have coarse reasons; counts are not retroactively relabelled.",
                  "No gate, timestamp, memory, or model configuration is changed by this audit."],
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", action="append", default=[], help="Saved extract_log JSON/JSONL; repeatable")
    parser.add_argument("--memory-input", action="append", default=[], help="Saved memory export JSON; repeatable")
    parser.add_argument("--base-url", help="Read-only local daemon endpoint; only health and export are requested")
    parser.add_argument("--tenant", action="append", default=[], help="Tenant to audit from base-url; repeatable")
    parser.add_argument("--as-of", help="Frozen evaluation reference time for detecting future access/write timestamps")
    parser.add_argument("--since", help="Keep log rows created on/after this ISO timestamp")
    parser.add_argument("--output", help="Write the count-only report JSON to this path; default stdout")
    args = parser.parse_args()
    if not args.input and not (args.base_url and args.tenant):
        parser.error("provide --input, or --base-url with at least one --tenant")
    if args.base_url and not args.tenant:
        parser.error("--base-url requires --tenant")
    reports = {}
    if args.input:
        logs = [row for path in args.input for row in records(read_json(path))]
        memories = [row for path in args.memory_input for row in records(read_json(path), "memories")]
        reports["files"] = summarize(logs, memories, args.as_of, args.since)
    health = None
    if args.base_url:
        health = api(args.base_url, args.tenant[0], path="/health")
        opened = health.get("tenants")
        if not isinstance(opened, list):
            raise ValueError("daemon cannot confirm opened tenants; audit saved export files instead")
        for tenant in args.tenant:
            if tenant not in opened:
                raise ValueError(f"refusing to open tenant {tenant!r}: opening can resume background LLM jobs; initialize it explicitly before auditing")
            logs = records(api(args.base_url, tenant, {"table": "extract_log"}))
            memories = records(api(args.base_url, tenant, {}), "memories")
            reports[tenant] = summarize(logs, memories, args.as_of, args.since)
    result = {"health": health, "reports": reports}
    output = json.dumps(result, ensure_ascii=False, indent=2) + "\n"
    if args.output:
        path = Path(args.output)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(output, encoding="utf-8")
    else:
        print(output, end="")


if __name__ == "__main__":
    main()
