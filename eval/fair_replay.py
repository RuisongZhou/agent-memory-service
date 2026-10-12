#!/usr/bin/env python3
"""Explicit retrieval benchmark on archived LoCoMo memories, with a common context cap.

No formation or answer calls. Rebuild Mem0 vectors outside timing, then call its real search.
The service daemon must use an isolated copied store. Raw results stay under results/.
"""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import time

import httpx

from common import env, jsonl, locomo
from common.metrics import TOKEN_COUNTER, count_tokens
from common.protocol import guard
from memory_run import format_context


def pack_blocks(blocks: list[str], budget: int) -> str:
    """Keep whole records in ranking order; count delimiters in the shared tokenizer."""
    kept = []
    for block in blocks:
        candidate = "\n\n".join([*kept, block])
        if count_tokens(candidate) <= budget:
            kept.append(block)
    return "\n\n".join(kept)


def legacy_blocks(context: str) -> list[str]:
    blocks = []
    for line in context.splitlines():
        if not blocks or line.startswith(("User profile:", "Project notes:", "[", "Fact:", "Fact (", "Conversation at", "Summary (")):
            blocks.append(line)
        else:
            blocks[-1] += "\n" + line
    return blocks


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--system", choices=["baseline", "retrieval", "repaired", "mem0"], required=True)
    parser.add_argument("--url", default="http://127.0.0.1:8771")
    parser.add_argument("--mem0-stores", type=Path)
    args = parser.parse_args()
    manifest = json.loads((args.root / "manifest.json").read_text())
    conv = locomo.select(locomo.load(), [manifest["conversation"]])[0]
    by_id = {q.qid: q for q in locomo.qas(conv)}
    questions = [by_id[qid] for qid in manifest["qids"]]
    target = args.root / args.system
    target.mkdir(exist_ok=True)
    guard(target / "protocol.json", {"manifest": hashlib.sha256((args.root / "manifest.json").read_bytes()).hexdigest(),
        "system": args.system, "context_cap": manifest["context_cap"], "token_counter": TOKEN_COUNTER,
        "passes": 3, "formation": False, "include_recent": False})
    if (target / "contexts.jsonl").exists():
        raise ValueError("use a new result directory")
    budget = manifest["context_cap"]
    with httpx.Client(base_url=args.url, timeout=180, trust_env=False,
                      headers={"X-Memory-Tenant": "conv-26-en", "X-Memory-Platform": "locomo"}) as client:
        deadline = time.monotonic() + 120
        while True:
            try:
                health = client.get("/health", timeout=2)
                health.raise_for_status()
                break
            except httpx.HTTPError:
                if time.monotonic() >= deadline:
                    raise TimeoutError("isolated daemon did not start")
                time.sleep(.2)
        def post(path, body):
            response = client.post(path, json=body)
            response.raise_for_status()
            return response.json()

        (target / "health.json").write_text(json.dumps(client.get("/health").json(), indent=2))
        if args.system != 'mem0' and health.json().get('memoryStrategy', 'current') != 'current':
            raise ValueError('archived service replay requires MEMORY_STRATEGY=current')
        stores = None
        if args.system == "mem0":
            if not args.mem0_stores:
                parser.error("mem0 needs copied --mem0-stores")
            from argparse import Namespace
            from qdrant_client import QdrantClient, models
            from mem0_run import Mem0Stores, perspectives
            ps = perspectives(conv)
            # Original vectors were Candle FP32. Every current record is embedded with the same
            # HTTP service used by our systems; payloads, formation and history are preserved.
            for perspective in ps:
                path = args.mem0_stores / "mem0" / conv["sample_id"] / perspective.user_id / "qdrant"
                qdrant = QdrantClient(path=str(path))
                points = []
                offset = None
                while True:
                    page, offset = qdrant.scroll("mem0", limit=256, offset=offset, with_payload=True)
                    points.extend(page)
                    if offset is None:
                        break
                for start in range(0, len(points), 32):
                    group = points[start:start + 32]
                    vectors = post("/v1/embeddings", {"input": [p.payload["data"] for p in group], "model": "bge-m3"})["data"]
                    qdrant.update_vectors("mem0", [models.PointVectors(id=p.id, vector=v["embedding"])
                                                  for p, v in zip(group, vectors)], wait=True)
                (target / f"{perspective.speaker}-corpus.json").write_text(json.dumps(
                    [{"id": p.id, "payload": p.payload} for p in points], indent=2))
                qdrant.close()
            stores = Mem0Stores(Namespace(stores_dir=args.mem0_stores, tag="mem0",
                observation_date="session", no_custom_instructions=False, embed_url=args.url + "/v1"))
            mems = [stores.get(p) for p in ps]
        try:
            if args.system != "mem0":
                before = post("/admin/export", {})
                (target / "corpus-before.json").write_text(json.dumps(before, indent=2))
                deadline = time.monotonic() + 180
                while post("/admin/stats", {})["index"]["pendingTasks"]:
                    if time.monotonic() >= deadline:
                        raise TimeoutError("index rebuild did not finish")
                    time.sleep(.1)
            for pass_index in range(3):
                for question in questions:
                    started = time.perf_counter()
                    if args.system == "mem0":
                        responses = [m.search(question.question, top_k=30, filters={"user_id": p.user_id})
                                     for p, m in zip(ps, mems)]
                        elapsed = 1000 * (time.perf_counter() - started)
                        records = [(p.speaker, x) for p, resp in zip(ps, responses) for x in resp["results"]]
                        records.sort(key=lambda item: (-item[1]["score"], item[1]["id"]))
                        blocks = [f"[{speaker}] {(x.get('metadata') or {}).get('timestamp', '')}: {x['memory']}"
                                  for speaker, x in records]
                        context = pack_blocks(blocks, budget)
                        info = {"n_memories": len(context.split("\n\n")) if context else 0, "n_turns": 0}
                        raw = responses
                    else:
                        raw = post("/v1/search_memories", {"query": question.question, "scope": {"project": conv["sample_id"]},
                            "asOf": locomo.rfc3339(locomo.as_of(conv)), "touch": False, "includeRecent": False,
                            "includeEvidence": args.system != "baseline", "topK": 10, "tokenBudget": budget})
                        elapsed = 1000 * (time.perf_counter() - started)
                        context, info = format_context(raw, "time" if args.system == "baseline" else "evidence")
                        blocks = legacy_blocks(context)
                        context = pack_blocks(blocks, budget)
                    jsonl.append(target / "retrieval.jsonl", {"qid": question.qid, "pass": pass_index,
                        "search_ms": elapsed, "response": raw, "context": context})
                    if pass_index == 0:
                        jsonl.append(target / "contexts.jsonl", {"system": args.system, "setting": "fair-2000",
                            "conv": question.conv, "qid": question.qid, "category": question.category,
                            "question": question.question, "answer": question.answer, "context": context,
                            "context_tokens": count_tokens(context), "token_counter": TOKEN_COUNTER, "search_ms": elapsed, **info})
                print(f"{args.system}: pass {pass_index + 1} complete", flush=True)
            if args.system != "mem0":
                after = post("/admin/export", {})
                (target / "corpus-after.json").write_text(json.dumps(after, indent=2))
                from integrity_benchmark import semantic_hash
                if semantic_hash(before["memories"]) != semantic_hash(after["memories"]):
                    raise ValueError("formation changed the measured corpus")
        finally:
            if stores:
                stores.close()


if __name__ == "__main__":
    main()
