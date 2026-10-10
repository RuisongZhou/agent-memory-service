#!/usr/bin/env python3
"""Run five explicitly requested, real-model extraction comparisons in isolated stores.

Both systems receive the same three-turn batch. This is an illustrative extraction
comparison, not a LoCoMo score or a test that runs automatically in CI.
"""
from __future__ import annotations

import argparse
from datetime import datetime, timedelta, timezone
import importlib.metadata
import json
import logging
import os
from pathlib import Path
import socket
import subprocess
import time
from urllib.request import Request, urlopen

from common import env
from common.llm import MODEL, QWEN_BASE_URL, api_key, set_ledger_path
import mem0_run


CASES = [
    {"id": "01-preferences", "topic": "长期偏好与要求", "turns": [
        ["我叫陈雨，是一名后端工程师。以后回答我的技术问题，请优先使用中文。", "好的，我会使用中文。"],
        ["我习惯先看结论，再看解释；给代码时优先用 Rust。", "明白，你偏好结论在前，代码优先使用 Rust。"],
        ["以后修改我的项目代码，都要先运行相关测试，再告诉我结果。", "收到，我会遵循这个要求。"],
    ]},
    {"id": "02-transient", "topic": "纯临时排查指令", "turns": [
        ["帮我看一下当前终端里的报错，先只检查这一条日志。", "这一条日志看起来是连接超时，还不能确定原因。"],
        ["这次排查先重试一次，不要把这次的步骤当成以后固定的做法。", "已重试，仍然超时；可能是临时网络波动，尚未验证。"],
        ["先停在这里，我暂时没有新的结论。谢谢。", "好的，排查暂停，目前原因未确定。"],
    ]},
    {"id": "03-project-update", "topic": "项目配置变更与已完成结果", "turns": [
        ["星桥项目之前使用 deepseek-flash 进行记忆抽取。", "了解，这是星桥项目原来的抽取配置。"],
        ["现在把星桥项目的记忆抽取模型改成 qwen3.8-flash，保留本地 BGE-M3 向量模型。", "配置已修改：记忆抽取使用 qwen3.8-flash，向量模型仍为本地 BGE-M3。"],
        ["星桥项目的离线测试已经通过，以后按这个配置继续。", "确认，星桥项目采用上述配置，离线测试通过。"],
    ]},
    {"id": "04-personal-event", "topic": "个人经历、情绪与未来计划", "turns": [
        ["我叫陈雨，2026年9月20日在杭州完成了自己的第一次半程马拉松。", "祝贺你完成第一次半程马拉松。"],
        ["那天我的姐姐陈欣也到场支持我，我跑完后特别自豪。", "姐姐的支持和完成比赛的自豪感，对你很有意义。"],
        ["我打算在2027年春天尝试一次全程马拉松，但目前还没决定具体比赛。", "明白，你有这个计划，具体比赛尚未确定。"],
    ]},
    {"id": "05-reference-suggestion", "topic": "指代消解与未采纳建议", "turns": [
        ["星桥项目的向量模型目前是 BGE-M3。", "我建议改用 Qwen3-Embedding-0.6B，不过这只是建议，尚未执行。"],
        ["先不换，星桥项目仍然用它。", "明白，星桥项目继续使用 BGE-M3。"],
        ["它要继续在本地运行，不要把星桥项目的向量生成迁移到云端。", "确认，BGE-M3 继续在本地生成星桥项目的向量。"],
    ]},
]


def save(path, value):
    Path(path).write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args()
    output = args.output_dir.resolve()
    output.mkdir(parents=True, exist_ok=False)
    logging.basicConfig(level=logging.ERROR)
    set_ledger_path(output / "usage_ledger.jsonl")
    save(output / "cases.json", CASES)
    protocol = {
        "model": MODEL, "base_url": QWEN_BASE_URL, "temperature": 0,
        "enable_thinking": False, "real_generation": True,
        "mem0_version": importlib.metadata.version("mem0ai"),
        "service_prompt": "p1_zh facts + session-end p2_zh", "mem0_prompt": "ADDITIVE_EXTRACTION_PROMPT + existing LoCoMo custom_instructions",
        "service_output_format": "facts-object-v1", "service_source_span_limit_chars": 1024,
        "same_input_batch": "three complete turns / six messages, one extraction per system per case",
        "initial_memory": "empty and isolated per case", "embedding": "real local BGE-M3 shared via daemon",
        "not_formal_benchmark": True,
    }
    report = {"protocol": protocol, "cases": [], "status": "running"}
    save(output / "comparison.json", report)
    with socket.socket() as port_socket:
        port_socket.bind(("127.0.0.1", 0))
        port = port_socket.getsockname()[1]
    base = f"http://127.0.0.1:{port}"

    def api(path, body=None, tenant=None):
        headers = {"Content-Type": "application/json", "X-Memory-Platform": "extraction-compare"}
        if tenant:
            headers["X-Memory-Tenant"] = tenant
        request = Request(base + path, headers=headers,
                          data=json.dumps(body).encode() if body is not None else None)
        with urlopen(request, timeout=300) as response:
            return json.load(response)

    child_env = dict(os.environ, QWEN_API_KEY=api_key(), QWEN_BASE_URL=QWEN_BASE_URL,
                     LLM_MODEL=MODEL, MEMORY_LLM_MAX_CALLS="15")
    child_env.pop("MEMORY_MOCK_MODELS", None)
    command = [str(env.ROOT_DIR / "target/fast/agent-memoryd"), "serve",
               "--data-dir", str(output / "service-store"),
               "--port", str(port), "--no-reranker", "--llm-concurrency", "1"]
    with (output / "daemon.log").open("w") as log_file:
        process = subprocess.Popen(command, env=child_env, stdout=log_file, stderr=log_file)
        try:
            for _ in range(180):
                if process.poll() is not None:
                    raise RuntimeError("isolated daemon exited; inspect local daemon.log")
                try:
                    health = api("/health")
                    break
                except OSError:
                    time.sleep(.5)
            else:
                raise RuntimeError("isolated daemon startup timed out")
            save(output / "health-before.json", health)
            print("Real local embedding service ready", flush=True)
            Memory, QwenLLM = mem0_run._install_mem0_patches(True)
            observed = datetime(2026, 10, 10, 10, 0, tzinfo=timezone(timedelta(hours=8)))
            for case in CASES:
                result = {"id": case["id"], "topic": case["topic"], "input": case["turns"]}
                report["cases"].append(result)
                case_dir = output / case["id"]
                case_dir.mkdir()
                tenant = case["id"]
                for i, (user, assistant) in enumerate(case["turns"]):
                    api("/v1/add_memories", {"scope": {"project": "星桥", "session": case["id"]},
                        "turnId": f"turn-{i+1}", "messages": [
                            {"role": "user", "content": user, "timestamp": (observed + timedelta(seconds=i*2)).isoformat()},
                            {"role": "assistant", "content": assistant, "timestamp": (observed + timedelta(seconds=i*2+1)).isoformat()},
                        ]}, tenant)
                idle = api("/admin/wait_idle", {}, tenant)
                if idle.get("status") != "idle" or idle.get("failedTurns") or idle.get("pendingTurns"):
                    raise RuntimeError(f"{tenant}: service extraction did not complete")
                for name, body in [("memories", {}), ("extract", {"table": "extract_log"}), ("usage", {"table": "llm_usage"})]:
                    value = api("/admin/export", body, tenant)
                    save(case_dir / f"service-{name}.json", value)
                    result[f"service_{name}"] = value
                result["service_texts"] = [m["text"] for m in result["service_memories"]["memories"] if m["type"] == "fact"]
                print(f"{case['id']}: service extracted {len(result['service_texts'])} facts", flush=True)
                config = {"version": "v1.1", "vector_store": {"provider": "qdrant", "config": {
                    "collection_name": "memories", "path": str(case_dir / "mem0-store"), "embedding_model_dims": 1024}},
                    "llm": {"provider": "openai", "config": {"model": MODEL, "openai_base_url": QWEN_BASE_URL,
                        "api_key": api_key(), "temperature": 0, "response_callback": mem0_run._ledger_callback(f"compare.{case['id']}.mem0")}},
                    "embedder": {"provider": "openai", "config": {"model": "bge-m3", "openai_base_url": base + "/v1",
                        "api_key": "unused", "embedding_dims": 1024}},
                    "history_db_path": str(case_dir / "mem0-history.db"),
                    "custom_instructions": mem0_run.mem0_custom_instructions()}
                memory = Memory.from_config(config)
                memory.llm.client.close()
                memory.llm = QwenLLM(memory.llm.config)
                original = memory.llm.generate_response
                trace = []

                def capture(messages, **kwargs):
                    record = {"messages": messages, "response_format": kwargs.get("response_format")}
                    trace.append(record)
                    response = original(messages, **kwargs)
                    record["response"] = response
                    save(case_dir / "mem0-trace.json", trace)
                    return response

                memory.llm.generate_response = capture
                mem0_run._tls.observation_date = "2026-10-10"
                messages = [{"role": role, "content": text} for turn in case["turns"]
                            for role, text in zip(["user", "assistant"], turn)]
                try:
                    result["mem0_result"] = memory.add(messages, user_id=case["id"], metadata={"timestamp": observed.isoformat()})
                    result["mem0_texts"] = [m["memory"] for m in result["mem0_result"]["results"]]
                finally:
                    memory.llm.client.close()
                    memory.embedding_model.client.close()
                    memory.vector_store.client.close()
                save(case_dir / "comparison.json", result)
                save(output / "comparison.json", report)
                print(f"{case['id']}: Mem0 extracted {len(result['mem0_texts'])} memories", flush=True)
            report["health_after"] = api("/health")
            report["status"] = "complete"
            save(output / "comparison.json", report)
            print(f"Complete: {output / 'comparison.json'}", flush=True)
        except Exception as error:
            report.update(status="failed", error=f"{type(error).__name__}: {str(error)[:300]}")
            save(output / "comparison.json", report)
            raise
        finally:
            process.terminate()
            try:
                process.wait(timeout=20)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()


if __name__ == "__main__":
    main()
