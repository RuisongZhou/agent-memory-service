#!/usr/bin/env python3
"""Offline V3 message-boundary comparison; real stores, deterministic model doubles.

This measures input/persistence semantics, not natural-language extraction quality.
Each arm gets fresh stores and the same frozen cases. No paid model is contacted.
"""
from __future__ import annotations

import argparse
from contextlib import ExitStack
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import importlib.metadata
import json
import os
from pathlib import Path
import socket
import subprocess
import threading
import time
from unittest.mock import patch
from urllib.request import Request, urlopen

os.environ["MEM0_TELEMETRY"] = "False"
os.environ["QWEN_API_KEY"] = "offline"
from common import env  # noqa: E402


from common.mem0_context_cases import message, cases, section, extract


def save(path, data):
    path.write_text(json.dumps(data, ensure_ascii=False, indent=2) + "\n")


def evaluate(case, traces, inputs):
    prompts = [trace["messages"][-1]["content"] for trace in traces]
    last = prompts[-1]
    history = section(last, "Last k Messages")
    case_id = case["id"]
    checks = {}
    if case_id == "long":
        original = case["steps"][0][1][0]["content"]
        checks["new_message_full_over_8kb"] = section(prompts[0], "New Messages") == f"user: {original}\n"
        checks["history_300_unicode_chars"] = history == f"user: {original[:300]}...\n"
        checks["stored_input_full"] = any(m["content"] == original for m in inputs)
        checks["tail_fact_extracted"] = traces[0]["response"]["memory"][0]["text"] == "TAIL_FACT_完整尾部"
    elif case_id in ("session", "raw"):
        checks[f"{case_id}_history_isolated"] = history == ""
        checks[f"{case_id}_no_context_contamination"] = traces[-1]["response"]["memory"] == []
        if case_id == "raw":
            checks["raw_bypasses_llm"] = len(traces) == 1
    elif case_id == "empty":
        checks["zero_memory_success_keeps_history"] = history == "user: ZERO_MEMORY_CONTEXT\n"
    elif case_id == "window":
        expected = "".join(f"{'user' if i % 2 == 0 else 'assistant'}: WINDOW_{i:02}\n"
                           for i in range(4, 14))
        checks["latest_ten_messages_chronological"] = history == expected
    elif case_id == "reference":
        checks["same_session_reference_context"] = history == "user: REFERENCE_BGE_M3\n"
        checks["same_session_reference_resolved"] = traces[-1]["response"]["memory"][0]["text"] == "Resolved reference: BGE-M3"
    elif case_id == "empty_window":
        checks["empty_messages_consume_window_but_not_prompt"] = history == "user: LAST_NONEMPTY\n"
    return checks


def service(binary, output, workload):
    traces = []

    class Stub(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            result = extract(body["messages"][-1]["content"])
            traces.append({"messages": body["messages"], "response": result})
            data = json.dumps({"choices": [{"message": {"content": json.dumps(result)},
                                            "finish_reason": "stop"}],
                               "usage": {"prompt_tokens": 0, "completion_tokens": 0}}).encode()
            self.send_response(200)
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

    stub = ThreadingHTTPServer(("127.0.0.1", 0), Stub)
    threading.Thread(target=stub.serve_forever, daemon=True).start()
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    child_env = dict(os.environ, MEMORY_MOCK_MODELS="1", QWEN_API_KEY="offline",
                     MEMORY_LLM_MAX_CALLS="100", MEMORY_STRATEGY="mem0")
    rows = []
    with (output / "daemon.log").open("w") as log:
        proc = subprocess.Popen([str(binary.resolve()), "serve", "--data-dir", str(output / "store"),
                                 "--port", str(port), "--llm-base-url", f"http://127.0.0.1:{stub.server_port}",
                                 "--prompt-variant", "en", "--memory-strategy", "mem0", "--log-level", "warn"],
                                env=child_env, stdout=log, stderr=log)

        def api(path, body=None, tenant="context"):
            req = Request(f"http://127.0.0.1:{port}{path}",
                          data=None if body is None else json.dumps(body).encode(),
                          headers={"Content-Type": "application/json", "X-Memory-Tenant": tenant,
                                   "X-Memory-Platform": "context"})
            with urlopen(req, timeout=30) as response:
                return json.load(response)

        try:
            for _ in range(200):
                if proc.poll() is not None:
                    raise RuntimeError("daemon exited; inspect daemon.log")
                try:
                    api("/health")
                    break
                except OSError:
                    time.sleep(.05)
            else:
                raise RuntimeError("startup timeout")
            for case in workload:
                start = len(traces)
                receipts = []
                for i, (session, messages, infer) in enumerate(case["steps"]):
                    before = time.perf_counter()
                    receipt = api("/v1/add_memories", {"scope": {"project": "p", "session": session},
                                  "turnId": f"{case['id']}-{i}", "messages": messages, "infer": infer,
                                  "sync": True}, case["id"])
                    receipt["clientMs"] = (time.perf_counter() - before) * 1000
                    receipts.append(receipt)
                    assert receipt["status"] == "completed" and receipt["searchable"], receipt
                raw = api("/v1/list_turns", {"scope": {"project": "p"}, "tokenBudget": 20000}, case["id"])
                inputs = [m for turn in raw["turns"] for m in turn["messages"]]
                memories = api("/admin/export", {}, case["id"])
                checks = evaluate(case, traces[start:], inputs)
                if case["id"] in ("session", "raw"):
                    checks["no_contaminated_memory_persisted"] = not any(
                        m["text"].startswith("CONTAMINATED_") for m in memories["memories"])
                if case["id"] in ("long", "reference"):
                    query = "TAIL_FACT_完整尾部" if case["id"] == "long" else "Resolved reference: BGE-M3"
                    found = api("/v1/search_memories", {"scope": {"project": "p"}, "query": query,
                                                       "includeEvidence": True}, case["id"])
                    checks["formed_memory_searchable"] = any(r["memory"]["text"] == query for r in found["results"])
                    save(output / f"{case['id']}-search.json", found)
                row = {"id": case["id"], "checks": checks, "receipts": receipts,
                       "traces": traces[start:], "inputs": raw, "memories": memories}
                rows.append(row)
                save(output / f"{case['id']}.json", row)
        finally:
            proc.terminate()
            proc.wait(timeout=15)
            stub.shutdown()
            stub.server_close()
    return rows


def sdk(output, workload):
    if importlib.metadata.version("mem0ai") != "2.2.1":
        raise RuntimeError("requires protocol-pinned mem0ai==2.2.1")
    from mem0 import Memory
    from mem0.memory.main import _build_session_scope
    rows = []
    with ExitStack() as patches:
        patches.enter_context(patch("mem0.memory.main.extract_entities_batch", side_effect=lambda texts: [[] for _ in texts]))
        patches.enter_context(patch("mem0.memory.main.lemmatize_for_bm25", side_effect=lambda text: text))
        for case in workload:
            path = output / case["id"]
            path.mkdir()
            memory = Memory.from_config({"vector_store": {"provider": "qdrant", "config": {
                "collection_name": "context", "path": str(path / "qdrant"), "embedding_model_dims": 8}},
                "llm": {"provider": "openai", "config": {"api_key": "offline", "openai_base_url": "http://127.0.0.1:1/v1"}},
                "embedder": {"provider": "openai", "config": {"api_key": "offline", "embedding_dims": 8,
                                                                               "openai_base_url": "http://127.0.0.1:1/v1"}},
                "history_db_path": str(path / "history.db")})
            traces = []

            def response(messages, **_):
                result = extract(messages[-1]["content"])
                traces.append({"messages": messages, "response": result})
                return json.dumps(result)

            memory.llm.generate_response = response
            memory.embedding_model.embed = lambda *_args, **_kw: [1.] + [0.] * 7
            memory.embedding_model.embed_batch = lambda texts, *_args, **_kw: [[1.] + [0.] * 7 for _ in texts]
            receipts = []
            stored_inputs = []
            try:
                for session, messages, infer in case["steps"]:
                    receipts.append(memory.add([{k: v for k, v in m.items() if k != "timestamp"} for m in messages],
                                               user_id="p", run_id=session, infer=infer))
                    scope = _build_session_scope({"user_id": "p", "run_id": session})
                    stored_inputs.extend(memory.db.get_last_messages(scope, 10))
                checks = evaluate(case, traces, stored_inputs)
                row = {"id": case["id"], "checks": checks, "receipts": receipts, "traces": traces,
                       "storedInputs": stored_inputs}
                save(output / f"{case['id']}.json", row)
                rows.append(row)
            finally:
                for resource in [memory, memory.vector_store.client, memory.entity_store.client,
                                 memory.llm.client, memory.embedding_model.client]:
                    resource.close()
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--arm", choices=["service", "sdk"], required=True)
    parser.add_argument("--bin", type=Path, default=Path("target/fast/agent-memoryd"))
    parser.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args()
    output = args.output_dir.resolve()
    output.mkdir(parents=True, exist_ok=False)
    workload = cases()
    save(output / "protocol.json", {"cases": workload, "realModelCalls": 0,
         "scopeMapping": "SDK user_id=project, run_id=session; service tenant per case, fixed platform",
         "generation": "deterministic prompt-sensitive double; not an accuracy benchmark",
         "retrieval": "mock vectors; latency is not model/database performance",
         "sourceInspectionTokenBudget": 20000,
         "sdkOptionalFeatures": "entity extraction disabled, lemmatizer identity",
         "sourceSha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
         "binarySha256": hashlib.sha256(args.bin.read_bytes()).hexdigest() if args.arm == "service" else None})
    rows = service(args.bin, output, workload) if args.arm == "service" else sdk(output, workload)
    checks = {f"{row['id']}.{key}": val for row in rows for key, val in row["checks"].items()}
    summary = {"arm": args.arm, "passed": sum(checks.values()), "total": len(checks), "checks": checks,
               "realModelCalls": 0, "status": "complete"}
    save(output / "summary.json", summary)
    print(json.dumps(summary, ensure_ascii=False))


if __name__ == "__main__":
    main()
