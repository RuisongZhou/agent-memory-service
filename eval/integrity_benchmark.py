#!/usr/bin/env python3
"""Replay a fixed LoCoMo subset against an isolated retained-store snapshot.

No ingestion, maintenance, paid calls, purge or retrieval tuning. Historical answers
are reused only when the complete rendered answer prompt has the same fingerprint.
"""
from __future__ import annotations

import argparse
from collections import Counter
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time
from urllib.parse import urlsplit

import httpx

from common import jsonl, locomo
from common.metrics import TOKEN_COUNTER, count_tokens
from common.protocol import guard
from memory_run import format_context
from answer_and_judge import fingerprint, _stats


def select_questions(conv: dict, per_category: int = 8) -> list:
    if per_category < 1:
        raise ValueError("per-category must be positive")
    questions = locomo.qas(conv)
    selected = []
    for category in locomo.EVAL_CATEGORIES:
        group = [q for q in questions if q.category == category]
        if len(group) < per_category:
            raise ValueError(f"category {category} has only {len(group)} questions")
        selected.extend(group[:per_category])
    return selected


def semantic_hash(records: list[dict]) -> str:
    fields = ("id", "mid", "type", "text", "status", "scope", "effectiveAt", "validUntil",
              "supersedes", "supersededBy", "usedMemoryIds", "derivation")
    rows = [{key: row[key] for key in fields if key in row} for row in records]
    rows.sort(key=lambda row: row.get("id", row.get("mid", "")))
    return hashlib.sha256(json.dumps(rows, sort_keys=True).encode()).hexdigest()


def reuse_answers(contexts: list[dict], source: Path) -> list[dict]:
    old_contexts = {row["qid"]: row for row in jsonl.read(source / "contexts.jsonl")}
    old_answers = {row["qid"]: row for row in jsonl.read(source / "answers-unified.jsonl")}
    reused = []
    for row in contexts:
        old = old_contexts.get(row["qid"])
        answer = old_answers.get(row["qid"])
        if old and answer and answer.get("judge_label") in ("CORRECT", "WRONG"):
            original = dict(row, system=old["system"], setting=old["setting"])
            if fingerprint(original, "unified") == answer.get("source_hash"):
                reused.append({**answer, "system": row["system"], "setting": row["setting"],
                               "source_hash": fingerprint(row, "unified"), "search_ms": row["search_ms"],
                               "context_tokens": row["context_tokens"], "n_memories": row["n_memories"],
                               "n_turns": row["n_turns"], "reused_from": str(source / "answers-unified.jsonl")})
    return reused


def collect(args) -> None:
    root = Path(args.root)
    manifest = json.loads((root / "manifest.json").read_text())
    conv = locomo.select(locomo.load(), [manifest["conversation"]])[0]
    by_id = {q.qid: q for q in locomo.qas(conv)}
    questions = [by_id[qid] for qid in manifest["qids"]]
    target = root / args.system / "retained-snapshot"
    target.mkdir(parents=True, exist_ok=True)
    protocol = {"manifest_sha256": hashlib.sha256((root / "manifest.json").read_bytes()).hexdigest(),
                "setting": "retained-snapshot", "top_k": 10, "token_budget": 2000,
                "touch": False, "passes": args.passes, "generation": False,
                "note": "First pass is cold; subsequent passes are warm; corpus is a historical post-purge retained snapshot, not the original buffer or physically cleared main."}
    guard(target / "protocol.json", protocol)
    contexts_path = target / "contexts.jsonl"
    raw_path = target / "retrieval.jsonl"
    if contexts_path.exists() or raw_path.exists():
        raise ValueError("use a fresh run directory; do not mix completed replays")
    headers = {"X-Memory-Tenant": "conv-26-en", "X-Memory-Platform": "locomo"}
    with httpx.Client(base_url=args.base_url, headers=headers, timeout=180, trust_env=False) as client:
        def post(path, body):
            response = client.post(path, json=body)
            response.raise_for_status()
            return response.json()

        health = client.get('/health').json()
        if health.get('memoryStrategy', 'memos') not in ('memos', 'current'):
            raise ValueError('retained-snapshot replay requires MEMORY_STRATEGY=memos')
        # Warm/open the store and finish only rebuildable indexes. Do not invoke wait_idle:
        # legacy formation receipts do not describe the current session-end contract.
        before = post("/admin/export", {})["memories"]
        deadline = time.monotonic() + 180
        while True:
            stats = post("/admin/stats", {})
            if stats["index"]["pendingTasks"] == 0:
                break
            if time.monotonic() > deadline:
                raise TimeoutError("index rebuild did not complete")
            time.sleep(.2)
        health = client.get("/health").json()
        (target / "health.json").write_text(json.dumps(health, indent=2))
        (target / "stats-before.json").write_text(json.dumps(stats, indent=2))
        for pass_index in range(args.passes):
            for question in questions:
                body = {"query": question.question, "scope": {"project": conv["sample_id"]},
                        "asOf": locomo.rfc3339(locomo.as_of(conv)), "touch": False,
                        "topK": 10, "tokenBudget": 2000}
                started = time.perf_counter()
                response = post("/v1/search_memories", body)
                elapsed = (time.perf_counter() - started) * 1000
                context, info = format_context(response, "time")
                jsonl.append(raw_path, {"qid": question.qid, "pass": pass_index,
                                      "search_ms": elapsed, "request": body, "response": response})
                if pass_index == 0:
                    row = {"system": args.system, "setting": "retained-snapshot", "conv": question.conv,
                           "qid": question.qid, "category": question.category, "category_name": question.category_name,
                           "question": question.question, "answer": question.answer, "context": context,
                           "context_tokens": count_tokens(context), "token_counter": TOKEN_COUNTER,
                           "search_ms": elapsed, **info}
                    jsonl.append(contexts_path, row)
            print(f"{args.system}: pass {pass_index + 1}/{args.passes}, {len(questions)} queries", flush=True)
        after = post("/admin/export", {})["memories"]
        audit = {"memory_hash_before": semantic_hash(before), "memory_hash_after": semantic_hash(after),
                 "memory_statuses": dict(Counter(row["status"] for row in after)),
                 "count": len(after), "llm": client.get("/health").json()["llm"]}
        (target / "corpus-audit.json").write_text(json.dumps(audit, indent=2))
        if audit["memory_hash_before"] != audit["memory_hash_after"]:
            raise ValueError("background work changed the measured corpus")

    historical = Path(manifest["historical_root"])
    sources = [historical / "memory-en/buffer"]
    if args.system == "improved":
        sources.append(root / "current/retained-snapshot")
    if args.reuse_root:
        sources += [args.reuse_root / system / "retained-snapshot" for system in ("current", "improved")]
    reused = {}
    for source in sources:
        for answer in reuse_answers(list(jsonl.read(contexts_path)), source):
            reused[answer["qid"]] = answer
    for answer in reused.values():
        jsonl.append(target / "answers-unified.jsonl", answer)
    print(f"{args.system}: {len(reused)} exact historical prompt matches; remaining answers require explicit evaluation", flush=True)


def report(args) -> None:
    root = Path(args.root)
    manifest = json.loads((root / "manifest.json").read_text())
    expected = set(manifest["qids"])
    historical = Path(manifest["historical_root"])
    sources = {"historical-service": historical / "memory-en/buffer", "historical-mem0": historical / "mem0/k30",
               "current": root / "current/retained-snapshot", "improved": root / "improved/retained-snapshot"}
    result = {"manifest": manifest, "systems": {}, "notes": [
        "Historical and live retained-snapshot cohorts share question IDs, but differ in retained turns, model runtime and formation version.",
        "Current versus improved uses identical stored primary records and the same llama-server models; no formation is re-run.",
        "LoCoMo measures QA; instructions/conflict resolution/invalidation are verified separately by offline integration tests."]}
    for name, path in sources.items():
        contexts = {row["qid"]: row for row in jsonl.read(path / "contexts.jsonl") if row["qid"] in expected}
        answers = {row["qid"]: row for row in jsonl.read(path / "answers-unified.jsonl") if row["qid"] in expected}
        matched = [row for qid, row in answers.items() if qid in contexts
                   and row.get("source_hash") == fingerprint(contexts[qid], "unified")
                   and row.get("judge_label") in ("CORRECT", "WRONG")]
        if set(row["qid"] for row in matched) != expected:
            raise ValueError(f"{name}: incomplete or incompatible answer cohort")
        entry = {"overall": _stats(matched), "by_category": {
            str(cat): _stats([row for row in matched if row["category"] == cat]) for cat in locomo.EVAL_CATEGORIES}}
        if name in ("current", "improved"):
            raw = list(jsonl.read(path / "retrieval.jsonl"))
            entry["latency_by_pass"] = {str(i): _stats([{"J": 0, "f1": 0, "bleu1": 0, "search_ms": r["search_ms"]}
                for r in raw if r["pass"] == i]) for i in sorted({r["pass"] for r in raw})}
            entry["warm_latency"] = _stats([{"J": 0, "f1": 0, "bleu1": 0, "search_ms": r["search_ms"]}
                                            for r in raw if r["pass"] > 0])
            entry["contexts_stable_across_passes"] = all(
                len({format_context(row["response"], "time")[0] for row in raw if row["qid"] == qid}) == 1
                for qid in expected)
        result["systems"][name] = entry
    (root / "comparison.json").write_text(json.dumps(result, indent=2))
    lines = ["# Fixed 32-question LoCoMo comparison", "", "| Setting | n | J % | F1 % | Context tokens | Cold P95 ms |", "|---|---:|---:|---:|---:|---:|"]
    for name, entry in result["systems"].items():
        row = entry["overall"]
        lines.append(f"| {name} | {row['n']} | {100 * row['J']:.2f} | {100 * row['F1']:.2f} | {row['context_tokens_mean']} | {row['search_ms_p95']} |")
    lines += ["", *result["notes"]]
    lines += ["", "| Live setting | Warm mean ms | Warm P95 ms |", "|---|---:|---:|"]
    for name in ("current", "improved"):
        timing = result["systems"][name]["warm_latency"]
        lines.append(f"| {name} | {timing['search_ms_mean']} | {timing['search_ms_p95']} |")
    (root / "comparison.md").write_text("\n".join(lines) + "\n")
    print("\n".join(lines))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", required=True)
    parser.add_argument("--system", choices=["current", "improved"])
    parser.add_argument("--base-url", default="http://127.0.0.1:8771")
    parser.add_argument("--passes", type=int, default=3)
    parser.add_argument("--report", action="store_true")
    parser.add_argument("--bin", type=Path, help="launch this binary for the replay; store required")
    parser.add_argument("--store", type=Path, help="isolated copied store; never the original")
    parser.add_argument("--reuse-root", type=Path, help="reuse answers from an earlier replay only for exactly equal prompts")
    args = parser.parse_args()
    if args.passes < 1 or not args.report and not args.system:
        parser.error("select --system and positive --passes, or --report")
    if args.report:
        report(args)
        return
    if not args.bin:
        collect(args)
        return
    if not args.store or not args.store.is_dir():
        parser.error("--bin requires an existing isolated --store")
    root = Path(args.root)
    if args.store.resolve().parent != root.resolve() or args.store.name != f"{args.system}-store":
        parser.error("launched replays require <root>/<system>-store; copy the original there first")
    config = root / "replay-config.json"
    # Preserve primary records during the experiment. Generation is disabled by removing its key;
    # unsuccessful legacy session jobs may update receipts but must not change semantic memory.
    config.write_text(json.dumps({"bufferRetentionDays": 36500, "sessionIdleMinutes": 100000000,
                                  "batchWaitSecs": 1000000000}))
    environment = dict(os.environ)
    environment.pop("QWEN_API_KEY", None)
    environment.pop("MEMORY_MOCK_MODELS", None)
    environment["MEMORY_LLM_MAX_CALLS"] = "1"
    # Keep the archived control binary's launch contract; the rebuilt arm uses the renamed algorithm.
    environment["MEMORY_STRATEGY"] = "current" if args.system == "current" else "memos"
    port = urlsplit(args.base_url).port
    if not port:
        parser.error("base-url must include a localhost port")
    with (root / f"{args.system}-service.log").open("w") as log:
        process = subprocess.Popen([str(args.bin.resolve()), "serve", "--data-dir", str(args.store.resolve()),
                                    "--port", str(port), "--prompt-variant", "en", "--config", str(config.resolve())],
                                   env=environment, stdout=log, stderr=subprocess.STDOUT)
        try:
            deadline = time.monotonic() + 120
            with httpx.Client(trust_env=False, timeout=2) as client:
                while True:
                    if process.poll() is not None:
                        raise RuntimeError("daemon exited; inspect local service log")
                    try:
                        response = client.get(args.base_url + "/health")
                        if response.is_success:
                            break
                    except httpx.HTTPError:
                        pass
                    if time.monotonic() > deadline:
                        raise TimeoutError("daemon startup timed out")
                    time.sleep(.2)
            collect(args)
        finally:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()


if __name__ == "__main__":
    main()
