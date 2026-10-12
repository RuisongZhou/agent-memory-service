#!/usr/bin/env python3
"""Explicit recovery wrapper with a transparent loopback relay for this host's egress.

For an isolated copied store only. Requests/responses contain source data and stay local;
Authorization headers and environment values are never logged. No dataset/QA selection here.
"""
import argparse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import subprocess
import threading

import httpx

from common import env, jsonl
from common.llm import QWEN_BASE_URL, api_key


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--bin", type=Path, default=env.ROOT_DIR / "target/fast/examples/replay_facts")
    parser.add_argument("--start", type=int, default=0, help="first archived batch index")
    parser.add_argument("--limit", type=int, help="optional small contract-validation sample")
    args = parser.parse_args()
    root = args.root.resolve()
    if args.start < 0 or args.limit is not None and args.limit < 1:
        parser.error("start must be nonnegative and limit must be positive")
    batches = json.loads((root / "repair-batches.json").read_text())
    selected = batches[args.start:args.start + args.limit if args.limit else None]
    if not selected:
        parser.error("no batches selected")
    input_path = root / f"repair-input-{args.start}-{len(selected)}.json"
    input_path.write_text(json.dumps(selected, ensure_ascii=False, indent=2))
    attempts = 0
    lock = threading.Lock()

    class Relay(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_POST(self):
            nonlocal attempts
            if self.path != "/chat/completions":
                self.send_error(404)
                return
            raw = self.rfile.read(int(self.headers["Content-Length"]))
            with lock:
                if attempts >= 60:
                    self.send_error(429, "explicit recovery attempt cap reached")
                    return
                attempts += 1
                number = attempts
            record = {"attempt": number, "request": json.loads(raw)}
            try:
                with httpx.Client(trust_env=False, timeout=180) as client:
                    response = client.post(QWEN_BASE_URL.rstrip("/") + "/chat/completions", content=raw,
                        headers={"Authorization": "Bearer " + api_key(), "Content-Type": "application/json"})
                payload, status = response.content, response.status_code
                record.update(status=status, response=json.loads(payload))
            except Exception as error:
                status = 502
                payload = json.dumps({"error": {"type": type(error).__name__}}).encode()
                record.update(status=status, error_type=type(error).__name__)
            with lock:
                jsonl.append(root / "repair-relay.jsonl", record)
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

    relay = ThreadingHTTPServer(("127.0.0.1", 0), Relay)
    thread = threading.Thread(target=relay.serve_forever, daemon=True)
    thread.start()
    child = dict(os.environ, QWEN_API_KEY=api_key(), QWEN_BASE_URL=f"http://127.0.0.1:{relay.server_port}",
        MEMORY_LLM_MAX_CALLS="60", NO_PROXY="*", no_proxy="*")
    child.pop("MEMORY_MOCK_MODELS", None)
    try:
        with (root / "repair-relayed.log").open("a") as log:
            result = subprocess.run([str(args.bin.resolve()), str(root / "repaired-store"), "conv-26-en",
                str(input_path)], env=child, stdout=log, stderr=subprocess.STDOUT)
        print(f"recovery exit={result.returncode}; relayed attempts={attempts}", flush=True)
        raise SystemExit(result.returncode)
    finally:
        relay.shutdown()
        relay.server_close()


if __name__ == "__main__":
    main()
