#!/usr/bin/env python3
"""Run the documented Chinese P1 example through the actual daemon with real Qwen P1/P2.

Only embeddings are mocked; SurrealDB, Tantivy, entity recall, prompt construction, rule
validation and relation commits are real. A transparent localhost relay records the
actual generated request bodies without headers or credentials. This isolated example
is not a LoCoMo effectiveness result. Existing nonempty stores are never overwritten.
"""
from __future__ import annotations

import argparse
from datetime import datetime, timedelta, timezone
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import re
import socket
import ssl
import subprocess
import sys
import threading
import time
from urllib.error import HTTPError
from urllib.request import Request, urlopen
from urllib.parse import urlparse

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "eval"))
from common import env as eval_env  # Loads only the named key without printing it.
sys.path.insert(0, str(ROOT / "scripts"))
from extraction_audit import summarize

OLD = [
    ("moldport", "2026-09-20", "project", "user", "共享记忆库项目测试环境的 SurrealDB 端口是 8000", ["ememory", "esurreal", "etest"]),
    ("moldhost", "2026-09-30", "project", "user", "2026-10-14 的设计评审由王经理主持", ["ereview", "ewang"]),
    ("moldchinese", "2026-09-02", "global", "explicit", "用户要求生成的文档使用简体中文", []),
    ("moldtantivy", "2026-09-05", "project", "user", "共享记忆库的全文索引使用 Tantivy", ["ememory", "etantivy"]),
]
TURNS = [
    ("T1", "2026-10-08T10:02:00+08:00", "测试环境的 SurrealDB 端口从 8000 改成 8100 了，昨天运维刚改的，你把配置也同步一下。", "已把 config/test.toml 里的 surreal.port 改为 8100，连接测试通过。"),
    ("T2", "2026-10-08T10:15:00+08:00", "另外下周三的设计评审可能换李经理主持。以后生成的文档都用简体中文，别加表情符号。", "好的，记下了。"),
    ("T3", "2026-10-08T10:31:00+08:00", "检索最近有点慢，是不是 HNSW 参数的问题？你先看下日志。", "看了日志，慢查询都集中在全文检索。我推测是 Tantivy 每次写入都提交，段文件太多，还没验证。"),
]
ENTITIES = [("ememory", "共享记忆库", ["记忆库", "memory-memory"]), ("esurreal", "SurrealDB", ["surreal"]),
            ("etantivy", "Tantivy", []), ("etest", "测试环境", []), ("ereview", "设计评审", []),
            ("ewang", "王经理", []), ("eli", "李经理", [])]


def saved(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin", type=Path, default=ROOT / "target/fast/agent-memoryd")
    parser.add_argument("--data-dir", type=Path, default=ROOT / "results/local-pilot/zh-example-store")
    parser.add_argument("--output", type=Path, default=ROOT / "results/local-pilot/zh-example.json")
    parser.add_argument("--port", type=int, default=8771)
    parser.add_argument("--max-calls", type=int, default=12)
    parser.add_argument("--allow-local-stub", action="store_true", help="Test-only loopback upstream; report is explicitly simulated, never effectiveness evidence")
    args = parser.parse_args()
    if args.max_calls > 12 or args.max_calls < 1:
        parser.error("this example is authorized for at most 12 attempts")
    binary = args.bin.resolve()
    sources = [ROOT / "crates/memory-service/src" / name for name in ("algorithms/memos/formation/extract.rs", "algorithms/memos/formation/experience.rs", "runtime/llm.rs")]
    if not binary.exists() or any(binary.stat().st_mtime < path.stat().st_mtime for path in sources):
        raise SystemExit("binary predates formation/accounting source; root must rebuild before this real example")
    if b"systemHash" not in binary.read_bytes():
        raise SystemExit("binary does not contain systemHash audit metadata")
    if args.data_dir.exists() and any(args.data_dir.iterdir()):
        raise SystemExit("data-dir already contains a store; refusing to replay or reset its attempt guard")
    key = os.environ.get("QWEN_API_KEY", "")
    if not key:
        raise SystemExit("QWEN_API_KEY is not configured; eval.common.env loaded no key")
    model = os.environ.get("EVAL_MODEL", os.environ.get("LLM_MODEL", "qwen3.8-flash"))
    upstream = os.environ.get("QWEN_BASE_URL", "https://maas.qianwenaiapi.com/compatible-mode/v1").rstrip("/")
    host=urlparse(upstream).hostname
    if args.allow_local_stub:
        if host not in {"127.0.0.1","localhost","::1"}:
            raise SystemExit("local-stub mode refuses any external model destination")
    elif host!="maas.qianwenaiapi.com":
        raise SystemExit("real example is scoped to the official maas.qianwenaiapi.com destination")
    ca = os.environ.get("MEMORY_CA_BUNDLE") or os.environ.get("SSL_CERT_FILE")
    tls = ssl.create_default_context(cafile=ca) if ca else ssl.create_default_context()
    result = {"status": "started", "protocol": {"language": "zh", "model": model,
        "tenant": "documented-example-zh", "platform": "codex", "project": "共享记忆库",
        "turns": 3, "oldFacts": 4, "realGenerativeModel": not args.allow_local_stub, "mockEmbeddings": True,
        "destinationHostname":host,
        "maxAttempts": args.max_calls,
        "source": "documented synthetic extraction example; P2 sees T1-T3 only, without its later T4-T6 verification",
        "notLoCoMoEffectiveness": True, "transparentRelay": True}, "binary": {"path":str(binary),
        "mtime":binary.stat().st_mtime,"systemHashCompiled":True}, "startedAt":datetime.now(timezone.utc).isoformat()}
    # Authenticated models listing is a connectivity/configuration check, never a generation call.
    try:
        request = Request(upstream + "/models", headers={"Authorization": "Bearer " + key})
        with urlopen(request, timeout=30, context=tls) as response:
            available = json.load(response)
        model_ids = [row.get("id") for row in available.get("data", [])]
        result["connectivity"] = {"modelsListingSucceeded": True,"selectedModelPresent":model in model_ids}
        if model not in model_ids:
            raise RuntimeError("selected Qwen model absent from authenticated /models listing")
    except Exception as exc:
        result.update(status="network_or_configuration_blocked", errorType=type(exc).__name__)
        if isinstance(exc, HTTPError): result["httpStatus"] = exc.code
        saved(args.output, result)
        print(json.dumps({"status":result["status"],"errorType":result["errorType"],"report":str(args.output)}))
        return 2

    traces = []
    trace_lock = threading.Lock()

    class Relay(BaseHTTPRequestHandler):
        def log_message(self, *_): pass
        def do_POST(self):
            raw = self.rfile.read(int(self.headers["Content-Length"]))
            body = json.loads(raw)
            system = body["messages"][0]["content"]
            stage = "P1+P2" if "formation-object-v2" in system else "P1" if "事实抽取器" in system else "P2" if "经验抽取器" in system else "unexpected"
            with trace_lock:
                if len(traces) >= args.max_calls:
                    self.send_error(429, "example request cap reached"); return
                trace = {"stage":stage,"sentAt":datetime.now(timezone.utc).isoformat(),"request":body}
                traces.append(trace)
            try:
                req = Request(upstream + "/chat/completions", data=raw,
                    headers={"Authorization":"Bearer " + key,"Content-Type":"application/json"})
                with urlopen(req, timeout=90, context=tls) as response:
                    payload, status = response.read(), response.status
                decoded = json.loads(payload)
                trace["response"] = decoded
            except HTTPError as exc:
                payload, status = exc.read(), exc.code
                trace["error"] = {"type":"HTTPError","status":status}
            except Exception as exc:
                payload, status = json.dumps({"error":{"message":"upstream connectivity failed","type":type(exc).__name__}}).encode(), 502
                trace["error"] = {"type":type(exc).__name__}
            trace["receivedAt"] = datetime.now(timezone.utc).isoformat()
            # Only protocol bodies are captured, never the Authorization header or process environment.
            self.send_response(status); self.send_header("Content-Type","application/json")
            self.send_header("Content-Length",str(len(payload))); self.end_headers(); self.wfile.write(payload)

    relay = ThreadingHTTPServer(("127.0.0.1",0),Relay)
    threading.Thread(target=relay.serve_forever,daemon=True).start()
    tenant = result["protocol"]["tenant"]
    base = f"http://127.0.0.1:{args.port}"
    def api(path, body=None):
        req = Request(base + path, data=None if body is None else json.dumps(body).encode(),
            headers={"Content-Type":"application/json","X-Memory-Tenant":tenant,"X-Memory-Platform":"codex"})
        with urlopen(req,timeout=180) as response:return json.load(response)
    process = None
    def sidecar(name):return args.output.with_name(f"{args.output.stem}-{name}.json")
    logfile = args.output.with_name(args.output.stem + "-daemon.log")
    logfile.parent.mkdir(parents=True,exist_ok=True)
    log = logfile.open("w",encoding="utf-8")
    try:
        with socket.socket() as check_socket:check_socket.bind(("127.0.0.1",args.port))
        child_env = dict(os.environ,MEMORY_MOCK_MODELS="1",
            MEMORY_LLM_MAX_CALLS=str(args.max_calls))
        process = subprocess.Popen([str(binary),"serve","--data-dir",str(args.data_dir),
            "--port",str(args.port),"--prompt-variant","zh","--no-reranker","--llm-model",model,
            "--llm-base-url",f"http://127.0.0.1:{relay.server_port}","--llm-concurrency","1","--log-level","warn"],
            env=child_env,stdout=log,stderr=log)
        for _ in range(100):
            if process.poll() is not None:raise RuntimeError("daemon exited before readiness; inspect redacted daemon log")
            try:result["healthBefore"]=api("/health");break
            except OSError:time.sleep(0.1)
        else:raise RuntimeError("daemon readiness timed out")
        project = result["protocol"]["project"]
        def scope(kind):return {"kind":"global"} if kind=="global" else {"kind":"project","project":project}
        memories = [{"id":mid,"type":"fact","text":text,"scope":scope(kind),"status":"current","effectiveAt":date,
            "recordedAt":date+"T00:00:00+08:00","statedAt":date+"T00:00:00+08:00","lastAccessedAt":date+"T00:00:00+08:00",
            "evidenceCount":1,"source":{"platform":"codex","origin":origin,"turnIds":["seed-"+mid]},"entityIds":entities}
            for mid,date,kind,origin,text,entities in OLD]
        entities = [{"id":eid,"name":name,"aliases":aliases,"scope":scope("project")} for eid,name,aliases in ENTITIES]
        result["seed"]=api("/admin/seed",{"memories":memories,"entities":entities})
        result["statsBefore"]=api("/admin/stats",{})
        receipts=[];payloads=[]
        context={"project":project,"session":"documented-session"}
        for tid,stamp,user,assistant in TURNS:
            ts=datetime.fromisoformat(stamp)
            payload={"scope":context,"turnId":tid,"messages":[{"role":"user","content":user,"timestamp":stamp},
                {"role":"assistant","content":assistant,"timestamp":(ts+timedelta(seconds=1)).isoformat()}]}
            payloads.append(payload);receipts.append(api("/v1/add_memories",payload))
        result["sessionEnd"]=api("/v1/add_memories",{"scope":context,"sessionEnded":True})
        idle=api("/admin/wait_idle",{});result["idle"]=idle
        result["tasks"]=[api("/v1/get_task",{"requestId":r["requestId"]}) for r in receipts]
        exported=api("/admin/export",{});extract=api("/admin/export",{"table":"extract_log"});usage=api("/admin/export",{"table":"llm_usage"})
        result["healthAfter"]=api("/health");result["statsAfter"]=api("/admin/stats",{})
        result["duplicateReceipt"]=api("/v1/add_memories",payloads[0])
        result["memories"]=exported["memories"]
        by_id={m["id"]:m for m in result["memories"]}
        result["oldRecords"]={mid:api("/v1/get_memory",{"id":mid,"includeHistory":True,"includeEvidence":True}) for mid,*_ in OLD}
        p1_trace=next((t for t in traces if t["stage"] in ("P1", "P1+P2")),None)
        recalled=[]
        if p1_trace:
            prompt=p1_trace["request"]["messages"][1]["content"]
            match=re.search(r"<旧记忆>\n(.*?)\n</旧记忆>",prompt,re.S)
            for line in (match.group(1).splitlines() if match else []):
                parts=line.split(" | ",4)
                if len(parts)==5:
                    recalled.append({"label":parts[0],"text":parts[4],"seedId":next((m[0] for m in OLD if m[4]==parts[4]),None)})
        current=[m for m in result["memories"] if m["status"]=="current"]
        new_port=by_id.get(by_id["moldport"].get("supersededBy"),{})
        result["oldRecall"]={"actualPromptMemories":recalled,"missingSeedIds":[m[0] for m in OLD if m[0] not in {r["seedId"] for r in recalled}]}
        result["checks"]={
            "allOldMemoriesActuallyRecalled":not result["oldRecall"]["missingSeedIds"],
            "oldPortSuperseded":by_id["moldport"]["status"]=="superseded",
            "newPort8100Current":new_port.get("status")=="current" and "8100" in new_port.get("text",""),
            "portEffectiveYesterday":new_port.get("effectiveAt")=="2026-10-07",
            "oldHostStillCurrent":by_id["moldhost"]["status"]=="current",
            "tentativeLiHostRetained":any("李经理" in m["text"] and "可能" in m["text"] and "2026-10-14" in m["text"] for m in current),
            "simplifiedChineseSameEvidencePlusOne":by_id["moldchinese"]["evidenceCount"]==2,
            "noUnverifiedTantivyCausePersisted":not any(m["id"] not in {x[0] for x in OLD} and
                ("段文件太多" in m["text"] or ("Tantivy" in m["text"] and ("逐条提交" in m["text"] or "每次写入都提交" in m["text"]))) for m in result["memories"]),
            "noTransientT3FactPersisted":not any(m["type"]=="fact" and "T3" in m.get("source",{}).get("turnIds",[]) for m in result["memories"]),
            "threeTurnP2CreatesNoReusableExperience":not any(m["type"]=="experience" for m in result["memories"]),
            "allRequestsCompleted":all(t["status"]=="completed" for t in result["tasks"]),
            "idleWithoutFailures":idle.get("status")=="idle" and all(idle.get(k,0)==0 for k in ("failedTurns","failedRequests","failedExperiences")),
            "duplicateIdempotent":result["duplicateReceipt"].get("duplicate") is True and result["duplicateReceipt"].get("requestId")==receipts[0]["requestId"],
            "boundedAttempts":result["healthAfter"]["llm"]["attempts"]<=args.max_calls,
        }
        result["deviations"]=[name for name,passed in result["checks"].items() if not passed]
        result["status"]="completed" if not result["deviations"] else "completed_with_deviations"
        saved(sidecar("extract"),{**extract,"protocol":result["protocol"]})
        saved(sidecar("usage"),{**usage,"protocol":result["protocol"]})
        saved(sidecar("audit"),{"protocol":result["protocol"],"audit":summarize(extract["records"],result["memories"],"2026-10-09T00:00:00+08:00")})
    except Exception as exc:
        result.update(status="runtime_failed",errorType=type(exc).__name__)
        if isinstance(exc,HTTPError):result["httpStatus"]=exc.code
        # Gather persisted bookkeeping when possible; never echo upstream headers or secret env.
        for name,body in (("extract",{"table":"extract_log"}),("usage",{"table":"llm_usage"})):
            try:saved(sidecar(name),{**api("/admin/export",body),"protocol":result["protocol"]})
            except Exception:pass
    finally:
        if process is not None:
            process.terminate()
            try:process.wait(timeout=15)
            except subprocess.TimeoutExpired:process.kill();process.wait()
        relay.shutdown();relay.server_close();log.close()
        # Redact defensively even though fixture prompts and captured protocol bodies contain no key.
        if logfile.exists():logfile.write_text(logfile.read_text().replace(key,"[REDACTED]"),encoding="utf-8")
        saved(sidecar("traffic"),{"protocol":result["protocol"],"requests":json.loads(json.dumps(traces,ensure_ascii=False).replace(key,"[REDACTED]"))})
        result["finishedAt"]=datetime.now(timezone.utc).isoformat()
        saved(args.output,json.loads(json.dumps(result,ensure_ascii=False).replace(key,"[REDACTED]")))
    print(json.dumps({"status":result["status"],"deviations":result.get("deviations"),
        "realGenerativeModel":not args.allow_local_stub,"guardUsage":result.get("healthAfter",{}).get("llm"),"report":str(args.output)},ensure_ascii=False))
    return 0 if result["status"]=="completed" else 1


if __name__=="__main__":
    raise SystemExit(main())
