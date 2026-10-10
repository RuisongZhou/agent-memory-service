"""Minimal stand-in for the Memory daemon HTTP API (SPEC §4) used to exercise memory_run.py.

Validates request shapes strictly and records every call; returns canned search results.
"""
from __future__ import annotations

import json
import re
import threading
from collections import defaultdict
from datetime import datetime
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

TURN_ID_RE = re.compile(r"^session_\d+-\d+$")


def _rfc3339(s: str) -> bool:
    try:
        datetime.fromisoformat(s.replace("Z", "+00:00"))
        return s.endswith("Z") or "+" in s
    except ValueError:
        return False


class State:
    def __init__(self):
        self.lock = threading.Lock()
        self.calls = []                         # (path, tenant, platform, body)
        self.turns = defaultdict(dict)          # tenant -> turnId -> body
        self.sessions_ended = defaultdict(list)
        self.jobs = defaultdict(list)
        self.purged = set()
        self.errors = []
        self.llm = {"calls": 0, "promptTokens": 0, "completionTokens": 0, "cacheHitTokens": 0}
        self.default_prompt = "en"
        self.usage = defaultdict(list)
        self.idle_overrides = {}
        self.purge_remaining = 0

    def add_usage(self, tenant, stage, hit, miss, out):
        self.llm["calls"] += 1
        self.llm["promptTokens"] += hit + miss
        self.llm["cacheHitTokens"] += hit
        self.llm["completionTokens"] += out
        self.usage[tenant].append({"mid": f"usage-{len(self.usage[tenant])+1}", "stage": stage,
                                  "model": "qwen3.8-flash", "createdAt": "2026-10-09T12:00:00Z",
                                  "promptTokens": hit + miss, "cacheHitTokens": hit, "cacheMissTokens": miss,
                                  "completionTokens": out, "latencyMs": 5, "attempt": 1, "validJson": True})

    def err(self, msg):
        self.errors.append(msg)
        return 400, {"code": "INVALID_ARGUMENT", "message": msg}

    def handle(self, path, tenant, platform, body):
        with self.lock:
            self.calls.append((path, tenant, platform, body))
            project = re.sub(r"-(en|zh)$", "", tenant)
            if platform != "locomo":
                return self.err(f"platform header {platform!r}")
            if path == "/v1/add_memories":
                scope = body.get("scope") or {}
                if scope.get("project") != project or not str(scope.get("session", "")).startswith("session_"):
                    return self.err(f"bad scope {scope}")
                if body.get("sessionEnded"):
                    self.sessions_ended[tenant].append(scope["session"])
                    return 200, {"requestId": "r", "status": "queued", "duplicate": False}
                tid = body.get("turnId", "")
                msgs = body.get("messages") or []
                if not TURN_ID_RE.match(tid) or not tid.startswith(scope["session"] + "-"):
                    return self.err(f"bad turnId {tid}")
                if [m.get("role") for m in msgs] != ["user", "assistant"]:
                    return self.err(f"bad roles {msgs}")
                if not all(isinstance(m.get("content"), str) and _rfc3339(m.get("timestamp", "")) for m in msgs):
                    return self.err(f"bad message {msgs}")
                dup = tid in self.turns[tenant]
                self.turns[tenant][tid] = body
                return 200, {"requestId": f"req-{tid}", "status": "queued", "duplicate": dup}
            if path == "/admin/wait_idle":
                self.add_usage(tenant, "P1", 500, 1000, 250)
                self.add_usage(tenant, "P2", 500, 1000, 250)
                return 200, {"idle": True, "status": "idle", "pendingTurns": 0, "pendingJobs": 0,
                             "indexTasks": 0, "failedTurns": 0, "failedRequests": 0,
                             "failedExperiences": 0, **self.idle_overrides}
            if path == "/admin/run_jobs":
                if not _rfc3339(body.get("asOf", "")):
                    return self.err("bad asOf")
                self.jobs[tenant].append(body)
                if "purge" in body.get("jobs", []):
                    self.purged.add(tenant)
                else:
                    self.add_usage(tenant, "P3", 0, 2000, 300)
                jobs = {j: ({"remainingTurns": self.purge_remaining} if j == "purge" else {"ok": True})
                        for j in body.get("jobs", [])}
                return 200, {"ok": True, "status": "completed", "jobs": jobs}
            if path == "/v1/search_memories":
                if body.get("touch") is not False or body.get("scope", {}).get("project") != project:
                    return self.err(f"bad search {body}")
                purged = tenant in self.purged
                results = [
                    {"memory": {"id": "m-prof", "type": "profile", "text": "PROFILE TEXT", "scope": {"kind": "global"},
                                "status": "current", "effectiveAt": "2023-10-01"}, "score": 1.0},
                    {"memory": {"id": "m2", "type": "fact", "text": "Later fact", "scope": {"kind": "project"},
                                "status": "current", "effectiveAt": "2023-06-01T10:00:00Z"}, "score": 0.9},
                    {"memory": {"id": "m1", "type": "fact", "text": "Earlier fact", "status": "conflict",
                                "effectiveAt": "2023-05-07"}, "score": 0.8},
                    {"memory": {"id": "m0", "type": "fact", "text": "Old fact", "status": "superseded",
                                "effectiveAt": "2023-05-01", "validUntil": "2023-05-20"}, "score": 0.7},
                ]
                recent = [] if purged else [
                    {"turnId": "session_2-1", "platform": "locomo", "session": "session_2",
                     "timestamp": "2023-05-25T13:14:00Z", "digest": "DIGEST B"},
                    {"turnId": "session_1-1", "platform": "locomo", "session": "session_1",
                     "timestamp": "2023-05-08T13:56:00Z", "digest": "DIGEST A"},
                ]
                return 200, {"retrievalId": "rt", "profiles": [results[0]["memory"]],
                             "results": results[1:], "recentTurns": recent, "gap": False}
            if path == "/admin/stats":
                remaining = self.purge_remaining if tenant in self.purged else len(self.turns[tenant])
                return 200, {"turns": remaining, "counts": {"turns": remaining}, "llm": dict(self.llm)}
            if path == "/admin/export" and body.get("table") == "llm_usage":
                return 200, {"table": "llm_usage", "records": list(self.usage[tenant]), "count": len(self.usage[tenant])}
            return 404, {"code": "NOT_FOUND", "message": path}


def make_handler(state: State):
    class H(BaseHTTPRequestHandler):
        def log_message(self, *a):
            pass

        def _send(self, code, obj):
            data = json.dumps(obj).encode()
            self.send_response(code)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def do_GET(self):
            if self.path == "/health":
                with state.lock:
                    tenant_variants = {tenant: (tenant.rsplit("-", 1)[1] if tenant.endswith(("-en", "-zh"))
                                                else state.default_prompt) for tenant in state.turns}
                    return self._send(200, {"ok": True, "promptVariant": state.default_prompt,
                                           "tenantPromptVariants": tenant_variants,
                                           "modelBackend": "mock", "modelDevice": "mock", "modelDtype": "f32",
                                           "embedModel": "mock-bow-1024", "rerankModel": None,
                                           "llm": dict(state.llm, model="qwen3.8-flash")})
            self._send(404, {})

        def do_POST(self):
            n = int(self.headers.get("Content-Length") or 0)
            try:
                body = json.loads(self.rfile.read(n) or b"{}")
            except json.JSONDecodeError:
                return self._send(400, {"code": "INVALID_ARGUMENT", "message": "json"})
            code, obj = state.handle(self.path, self.headers.get("X-Memory-Tenant", "default"),
                                     self.headers.get("X-Memory-Platform", "sdk"), body)
            self._send(code, obj)

    return H


def start(port: int = 0):
    state = State()
    srv = ThreadingHTTPServer(("127.0.0.1", port), make_handler(state))
    t = threading.Thread(target=srv.serve_forever, daemon=True)
    t.start()
    return srv, state
