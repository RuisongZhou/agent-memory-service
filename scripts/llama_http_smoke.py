#!/usr/bin/env python3
"""Exercise the self-hosted model provider: the daemon with two real BGE llama-server models and a localhost-only Mem0 stub.

No .env is loaded, no provider key is inherited, and no external generative service is
contacted. Both llama-server processes must already run (scripts/self_hosted_models.sh start).
Temporary storage never replaces production stores. This verifies llama-server integration
and HTTP/storage/index plumbing;
it does not measure real generative extraction quality.
"""
from __future__ import annotations

import argparse
from collections import Counter
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timedelta, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import math
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import threading
import time
from urllib.error import HTTPError
from urllib.request import ProxyHandler, Request, build_opener

ROOT = Path(__file__).resolve().parents[1]
FACT_TEXTS = [
    "The demo API service port is 8200.",
    "The demo uses BAAI bge-m3 embeddings.",
    "The demo uses BAAI bge-reranker-v2-m3 for relevance reranking.",
]
EXPECTED_DEVICE = {"modelBackend": "llama-server"}


def validate_vectors(response, expected_rows):
    rows = response.get("data")
    if not isinstance(rows, list) or len(rows) != expected_rows:
        raise AssertionError("embedding response has the wrong row count")
    norms = []
    for index, row in enumerate(rows):
        vector = row.get("embedding")
        if row.get("index") != index or not isinstance(vector, list) or len(vector) != 1024:
            raise AssertionError("embedding must be an indexed 1024-dimensional vector")
        if not all(isinstance(value, (int, float)) and math.isfinite(value) for value in vector):
            raise AssertionError("embedding has a non-finite value")
        norm = math.sqrt(sum(value * value for value in vector))
        if abs(norm - 1.0) > 1e-4:
            raise AssertionError(f"embedding is not unit-normalized: {norm}")
        norms.append(norm)
    return {"rows": len(rows), "dim": 1024, "norms": norms, "allFinite": True, "model": response.get("model")}


def validate_search(response, expected_text=FACT_TEXTS[0]):
    rows = response.get("results")
    if response.get("gap") or not isinstance(rows, list) or not rows:
        raise AssertionError("search returned a gap for an indexed port fact")
    top = rows[0]
    memory = top.get("memory", {})
    if memory.get("text") != expected_text or memory.get("status") != "current":
        raise AssertionError("the most relevant search result is not the current port-8200 fact")
    if not isinstance(top.get("score"), (int, float)) or not math.isfinite(top["score"]):
        raise AssertionError("search score is not finite")
    return {"topMemoryId": memory["id"], "topText": memory["text"], "topScore": top["score"],
            "resultCount": len(rows), "serverLatency": response.get("latency", response.get("latencyMs"))}


def sanitized_environment():
    # Allow only process/runtime settings needed for a native local executable. Provider, proxy,
    # CA-bundle, mock-model and dotenv settings are deliberately absent rather than overwritten.
    allowed = {"PATH", "TMPDIR", "LANG", "LC_ALL", "LC_CTYPE", "SYSTEMROOT"}
    result = {key: value for key, value in os.environ.items() if key in allowed}
    result.update({"QWEN_API_KEY": "localhost-stub-placeholder",
                   "MEMORY_STRATEGY": "mem0",
                   "PYTHONDONTWRITEBYTECODE": "1"})
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin", type=Path, required=True)
    parser.add_argument("--embed-url", default="http://127.0.0.1:8081")
    parser.add_argument("--rerank-url", default="http://127.0.0.1:8082")
    parser.add_argument("--output", type=Path, default=ROOT / "results/service-smoke/llama-http-smoke.json")
    parser.add_argument("--startup-timeout", type=float, default=300)
    parser.add_argument("--request-timeout", type=float, default=240)
    parser.add_argument("--skip-concurrency", action="store_true", help="Skip the default parallel embedding/search check")
    args = parser.parse_args()
    binary = args.bin.resolve()
    embed_url, rerank_url = args.embed_url.rstrip("/"), args.rerank_url.rstrip("/")
    if not binary.is_file():
        parser.error("--bin must be a compiled daemon executable")
    for url in [embed_url, rerank_url]:
        try:
            build_opener(ProxyHandler({})).open(url + "/health", timeout=5).read()
        except OSError:
            parser.error(f"llama-server is not ready at {url}; run scripts/self_hosted_models.sh start")
    stub_calls, stub_errors = [], []
    stub_lock = threading.Lock()

    class Stub(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_POST(self):
            try:
                if self.path != "/chat/completions":
                    raise AssertionError("stub accepts only the local chat-completions path")
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                if body.get("model") != "qwen3.8-flash":
                    raise AssertionError("daemon must use the configured Qwen model")
                if body.get("temperature") != 0 or body.get("enable_thinking") is not False or "thinking" in body:
                    raise AssertionError("daemon changed the configured generation protocol")
                if body.get("stream") is not False:
                    raise AssertionError("daemon must request non-streaming output")
                if self.headers.get("Authorization") != "Bearer localhost-stub-placeholder":
                    raise AssertionError("daemon did not use the explicit local placeholder credential")
                messages = body["messages"]
                if [message["role"] for message in messages] != ["system", "user"]:
                    raise AssertionError("expected the Mem0 system prompt and user context")
                prompt = messages[1]["content"]
                with stub_lock:
                    index = len(stub_calls)
                if index >= len(FACT_TEXTS) or FACT_TEXTS[index] not in prompt:
                    raise AssertionError("unexpected Mem0 source batch or retry")
                stage = "mem0"
                output = {"memory": [{"id": "0", "text": FACT_TEXTS[index], "attributed_to": "user"}]}
                with stub_lock:
                    stub_calls.append(stage)
                payload = json.dumps({"choices": [{"message": {"content": json.dumps(output)}, "finish_reason": "stop"}],
                                      "usage": {"prompt_tokens": 0, "completion_tokens": 0, "prompt_cache_hit_tokens": 0}}).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)
            except Exception as error:
                with stub_lock:
                    stub_errors.append(type(error).__name__ + ": " + str(error))
                self.send_error(400, "local stub validation failed")

    report = {"status": "started", "realGenerativeCalls": 0, "generationBackend": "localhost-deterministic-stub",
              "realEmbeddingModel": "BAAI/bge-m3", "realRerankerModel": "BAAI/bge-reranker-v2-m3",
              "expectedDevice": EXPECTED_DEVICE,
              "numericalReferenceComparisonNotIncluded": True,
              "providerKeyInherited": False, "dotenvRead": False,
              "startedAt": datetime.now(timezone.utc).isoformat(), "binary": str(binary),
              "embedUrl": embed_url, "rerankUrl": rerank_url, "latencyMs": {}}
    stub = ThreadingHTTPServer(("127.0.0.1", 0), Stub)
    threading.Thread(target=stub.serve_forever, daemon=True).start()
    process, daemon_log_text = None, ""
    # Every HTTP opener ignores inherited proxies. The two possible destinations are hardcoded
    # loopback addresses, and the daemon receives only an explicit loopback LLM base URL.
    def request(base, path, body=None):
        headers = {"Content-Type": "application/json", "X-Memory-Platform": "llama-test"}
        req = Request(base + path, headers=headers, data=None if body is None else json.dumps(body).encode())
        return json.load(build_opener(ProxyHandler({})).open(req, timeout=args.request_timeout))

    def timed(key, func):
        started = time.perf_counter()
        value = func()
        report["latencyMs"][key] = round((time.perf_counter() - started) * 1000, 2)
        return value

    try:
        with tempfile.TemporaryDirectory(prefix="memory-llama-http-smoke-") as temporary:
            temp = Path(temporary)
            with socket.socket() as port_socket:
                port_socket.bind(("127.0.0.1", 0))
                port = port_socket.getsockname()[1]
            base = f"http://127.0.0.1:{port}"
            log_path = temp / "daemon.log"
            with log_path.open("w", encoding="utf-8") as logfile:
                try:
                    start = time.perf_counter()
                    config = temp / "config.json"
                    config.write_text(json.dumps({"mem0Rerank": True}))
                    process = subprocess.Popen([str(binary), "serve", "--data-dir", str(temp / "data"),
                        "--model-provider", "self-hosted", "--embed-url", embed_url, "--rerank-url", rerank_url, "--port", str(port),
                        "--prompt-variant", "en", "--config", str(config),
                        "--llm-base-url", f"http://127.0.0.1:{stub.server_port}", "--llm-concurrency", "1", "--log-level", "warn"],
                        env=sanitized_environment(), stdout=logfile, stderr=logfile)
                    while time.perf_counter() - start < args.startup_timeout:
                        if process.poll() is not None:
                            raise RuntimeError("daemon exited before readiness")
                        try:
                            health = request(base, "/health")
                            break
                        except OSError:
                            time.sleep(0.2)
                    else:
                        raise RuntimeError("daemon startup timeout")
                    report["latencyMs"]["startupIncludingBothModels"] = round((time.perf_counter() - start) * 1000, 2)
                    report["healthBefore"] = health
                    for key, expected in EXPECTED_DEVICE.items():
                        if health.get(key) != expected:
                            raise AssertionError(f"health {key} is {health.get(key)!r}, expected {expected!r}")
                    if health.get("modelDevice") != embed_url or not health.get("modelDtype"):
                        raise AssertionError("health must report the embedding server URL and GGUF file type")
                    if "mock" in str(health.get("embedModel", "")).lower() or not health.get("rerankModel"):
                        raise AssertionError("the daemon must load both real BGE models")
                    if "bge-m3" not in str(health.get("embedModel", "")).lower() or "bge-reranker-v2-m3" not in str(health.get("rerankModel", "")).lower():
                        raise AssertionError("the required BGE model identities were not reported")
                    report["device"] = {key: health[key] for key in ["modelBackend", "modelDevice", "modelDtype"]}
                    report["checks"] = {"realBgeLlamaServerModels": True}
                    scope = {"project": "llama-smoke", "session": "llama-session"}
                    now = datetime.now(timezone.utc) - timedelta(minutes=1)
                    receipts = []
                    for index, text in enumerate(FACT_TEXTS):
                        stamp = now + timedelta(seconds=index * 2)
                        receipts.append(request(base, "/v1/add_memories", {"scope": scope, "turnId": f"T{index + 1}", "sync": True,
                            "messages": [{"role": "user", "content": text, "timestamp": stamp.isoformat()},
                                         {"role": "assistant", "content": "Confirmed the stated configuration.", "timestamp": (stamp + timedelta(seconds=1)).isoformat()}]}))
                    request(base, "/v1/add_memories", {"scope": scope, "sessionEnded": True})
                    idle = timed("formationAndIndexDrain", lambda: request(base, "/admin/wait_idle", {}))
                    report["idle"] = idle
                    zero_fields = ["failedRequests", "pendingRequests", "indexTasks"]
                    if idle.get("status") != "idle" or any(idle.get(key) != 0 for key in zero_fields):
                        raise AssertionError("wait_idle did not confirm zero failures, pending jobs and index tasks")
                    tasks = [request(base, "/v1/get_task", {"requestId": receipt["requestId"]}) for receipt in receipts]
                    report["tasks"] = tasks
                    if not all(task.get("status") == "completed" and len(task.get("memoryIds", [])) == 1 for task in tasks):
                        raise AssertionError("each completed turn must have exactly one persisted configuration fact")
                    stats = request(base, "/admin/stats", {})
                    report["statsAfterIngest"] = stats
                    if stats["counts"]["index_tasks"] != 0 or stats["index"]["pendingTasks"] != 0 or stats["jobs"]["pendingJobs"] != 0:
                        raise AssertionError("real model indexing is still pending")
                    if stats["index"]["memoryVectors"] != 3 or stats["index"]["turnVectors"] != 0:
                        raise AssertionError("real vector index has unexpected counts")
                    exported = request(base, "/admin/export", {})["memories"]
                    if sorted(memory["text"] for memory in exported) != sorted(FACT_TEXTS):
                        raise AssertionError("the exact three facts were not persisted")
                    report["checks"].update({"formationTasksCompleted": True, "allIndexTasksDrained": True})
                    embedded = timed("embeddingTwoFreshTexts", lambda: request(base, "/v1/embeddings", {
                        "model": "bge-m3", "input": ["A service listens on port 8200.", "Dense embeddings encode semantic similarity."]}))
                    report["embeddingCheck"] = validate_vectors(embedded, 2)
                    query = "Which port does the demo API service use?"
                    search_body = {"query": query, "scope": scope, "topK": 3, "includeRecent": False, "touch": False}
                    search = timed("searchIncludingReranker", lambda: request(base, "/v1/search_memories", search_body))
                    report["searchCheck"] = validate_search(search)
                    report["checks"].update({"finite1024UnitVectors": True, "rerankedPortFactFirst": True})
                    if not args.skip_concurrency:
                        # Different texts and a new query bypass the embedding cache; search also invokes
                        # the real reranker. Both HTTP requests run concurrently against the shared model servers.
                        started = time.perf_counter()
                        with ThreadPoolExecutor(max_workers=2) as executor:
                            embed_future = executor.submit(request, base, "/v1/embeddings", {"input": ["Concurrent fresh vector for shared model-server scheduling and semantic retrieval."]})
                            search_future = executor.submit(request, base, "/v1/search_memories", {
                                **search_body, "query": "What is the configured network port of the demo API service?"})
                            concurrent_embedding, concurrent_search = embed_future.result(), search_future.result()
                        report["latencyMs"]["concurrentEmbeddingAndSearchWall"] = round((time.perf_counter() - started) * 1000, 2)
                        report["concurrencyCheck"] = {"embedding": validate_vectors(concurrent_embedding, 1), "search": validate_search(concurrent_search)}
                        report["checks"]["concurrentSharedDeviceCalls"] = True
                    report["healthAfter"] = request(base, "/health")
                    with stub_lock:
                        calls = list(stub_calls)
                    if Counter(calls) != {"mem0": 3} or stub_errors:
                        raise AssertionError("expected exactly three additive extraction calls")
                    if report["healthAfter"]["llm"]["calls"] != 3 or report["healthAfter"]["llm"]["attempts"] != 3:
                        raise AssertionError("unexpected generative stage retry or extra daemon model request")
                    for key, expected in EXPECTED_DEVICE.items():
                        if report["healthAfter"].get(key) != expected:
                            raise AssertionError("device metadata changed during concurrent model calls")
                    if any(report["healthAfter"].get(key) != health[key] for key in ["modelDevice", "modelDtype"]):
                        raise AssertionError("the model server or quantisation changed during the run")
                    report["checks"]["additiveFormationNoRealGenerativeCalls"] = True
                    report["status"] = "passed"
                finally:
                    if process is not None:
                        process.terminate()
                        try:
                            process.wait(timeout=20)
                        except subprocess.TimeoutExpired:
                            process.kill()
                            process.wait()
                    logfile.flush()
                    daemon_log_text = log_path.read_text(encoding="utf-8")
    except Exception as error:
        report["status"] = "failed"
        report["error"] = {"type": type(error).__name__, "message": str(error)}
        if isinstance(error, HTTPError):
            report["error"]["httpStatus"] = error.code
    finally:
        stub.shutdown()
        stub.server_close()
        with stub_lock:
            report["stubCalls"] = list(stub_calls)
            report["stubStageCounts"] = dict(Counter(stub_calls))
            report["stubErrors"] = list(stub_errors)
        report["finishedAt"] = datetime.now(timezone.utc).isoformat()
        report["realGenerativeCalls"] = 0
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        args.output.with_suffix(".daemon.log").write_text(daemon_log_text.replace("localhost-stub-placeholder", "[LOCAL_PLACEHOLDER]"), encoding="utf-8")
    print(json.dumps({"status": report["status"], "realGenerativeCalls": 0, "stubCalls": report["stubCalls"],
                      "device": report.get("device"), "latencyMs": report["latencyMs"], "report": str(args.output)}, ensure_ascii=False))
    return 0 if report["status"] == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
