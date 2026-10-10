#!/usr/bin/env python3
"""Run one real LoCoMo excerpt through extraction and traced fact-only retrieval.

Explicit real-model example, not a full-dataset benchmark or an automatic CI test.
"""
from __future__ import annotations

import argparse
from datetime import timedelta
import hashlib
import importlib.metadata
import importlib.util
import json
import logging
import os
from pathlib import Path
import socket
import subprocess
import time
from urllib.request import Request, urlopen

from common import env, locomo
from common.llm import MODEL, QWEN_BASE_URL, api_key, set_ledger_path
from extraction_compare import save
import mem0_run


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--resume", action="store_true", help="reuse completed extraction in this example's store")
    args = parser.parse_args()
    output = args.output_dir.resolve()
    if not args.resume:
        output.mkdir(parents=True, exist_ok=False)
    logging.basicConfig(level=logging.ERROR)
    set_ledger_path(output / "usage_ledger.jsonl")
    conversation = locomo.select(locomo.load(), ["conv-26"])[0]
    original_session = locomo.sessions(conversation)[0]
    excerpt = locomo.Session(original_session.n, original_session.key, original_session.date_str,
                             original_session.start, original_session.utterances[:6])
    question = next(q for q in locomo.qas(conversation) if q.qid == "conv-26-q0")
    assert question.evidence == ["D1:3"]
    protocol = {"source": "https://github.com/snap-research/locomo/blob/main/data/locomo10.json",
                "dataset_sha256": hashlib.sha256(env.DATA_PATH.read_bytes()).hexdigest(),
                "conversation": "conv-26", "session": "session_1", "dialog_ids": [u["dia_id"] for u in excerpt.utterances],
                "session_date": excerpt.date_str, "qid": question.qid, "question": question.question,
                "reference_answer": question.answer, "evidence": question.evidence,
                "model": MODEL, "temperature": 0, "enable_thinking": False, "real_generation": True,
                "service_prompt": "p1_en + p2_en (unified formation, LoCoMo human-human adaptation)",
                "service_output_format": "formation-values-v1", "service_source_span_limit_chars": 1024,
                "service_prompt_sha256": hashlib.sha256(b"".join((env.ROOT_DIR / "crates/memory-service/src/formation/prompts" / n).read_bytes() for n in ("p1_en.txt", "p2_en.txt", "p12_structure_en.txt"))).hexdigest(),
                "mem0_prompt": "default additive + existing LoCoMo custom instructions",
                "mem0_version": importlib.metadata.version("mem0ai"),
                "service_batch": "3 paired turns", "mem0_batch": "2 messages, both speaker perspectives",
                "embedding": "shared real local BGE-M3", "service_reranker": "real local BGE-Reranker-v2-M3",
                "raw_dialogue_fallback": False, "answer_model_called": False, "not_full_benchmark": True,
                "mem0_optional_dependencies": {n: importlib.util.find_spec(n) is not None for n in ["spacy", "fastembed"]}}
    result = {"status": "running", "protocol": protocol, "dialogue": excerpt.utterances, "mem0": []}
    if args.resume:
        result = json.loads((output / "result.json").read_text())
        if result["protocol"] != protocol or result["mem0"]:
            raise RuntimeError("resume requires unchanged protocol and an unfinished Mem0 stage")
        result["status"] = "running"
        result.pop("error", None)
    save(output / "result.json", result)
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    base = f"http://127.0.0.1:{port}"
    tenant = "locomo-conv26-example-en"

    def api(path, body=None):
        request = Request(base + path, headers={"Content-Type": "application/json",
                          "X-Memory-Platform": "locomo", "X-Memory-Tenant": tenant},
                          data=json.dumps(body).encode() if body is not None else None)
        with urlopen(request, timeout=300) as response:
            return json.load(response)

    daemon_env = dict(os.environ, QWEN_API_KEY=api_key(), QWEN_BASE_URL=QWEN_BASE_URL,
                      LLM_MODEL=MODEL, MEMORY_LLM_MAX_CALLS="10")
    daemon_env.pop("MEMORY_MOCK_MODELS", None)
    command = [str(env.ROOT_DIR / "target/fast/agent-memoryd"), "serve", "--data-dir", str(output / "service-store"),
               "--models-dir", str(env.ROOT_DIR / "models"), "--port", str(port), "--model-device", "metal",
               "--prompt-variant", "en", "--llm-concurrency", "1"]
    memories = []
    with (output / "daemon.log").open("w") as logfile:
        process = subprocess.Popen(command, env=daemon_env, stdout=logfile, stderr=logfile)
        try:
            for _ in range(180):
                if process.poll() is not None:
                    raise RuntimeError("daemon exited; inspect daemon.log")
                try:
                    api("/health")
                    break
                except OSError:
                    time.sleep(.5)
            else:
                raise RuntimeError("daemon startup timeout")
            result["health_before"] = api("/health")
            print("Real embedding and reranking service ready", flush=True)
            if not args.resume:
                for turn in locomo.turns(excerpt):
                    api("/v1/add_memories", {"scope": {"project": "conv-26", "session": "session_1"},
                        "turnId": turn.turn_id, "messages": [
                            {"role": "user", "content": turn.user.content, "timestamp": locomo.rfc3339(turn.user.timestamp)},
                            {"role": "assistant", "content": turn.assistant.content, "timestamp": locomo.rfc3339(turn.assistant.timestamp)}]})
                result["service_idle"] = api("/admin/wait_idle", {})
                if result["service_idle"].get("status") != "idle" or result["service_idle"].get("failedTurns"):
                    raise RuntimeError("service extraction incomplete")
                result["service_memories"] = api("/admin/export", {})
                result["service_extraction"] = api("/admin/export", {"table": "extract_log"})
            result["service_search_request"] = {"query": question.question, "scope": {"project": "conv-26"},
                "types": ["fact"], "topK": 10, "includeRecent": False, "includeEvidence": True,
                "asOf": locomo.rfc3339(excerpt.start + timedelta(days=1)), "touch": True}
            if not args.resume:
                result["service_search"] = api("/v1/search_memories", result["service_search_request"])
            result["service_retrieval_trace"] = api("/admin/export", {"table": "retrieval_log"})
            print(f"Service saved {len(result['service_memories']['memories'])} memories and returned {len(result['service_search']['results'])}", flush=True)
            save(output / "result.json", result)
            Memory, QwenLLM = mem0_run._install_mem0_patches(True)
            for perspective in mem0_run.perspectives(conversation):
                directory = output / f"mem0-{perspective.speaker}"
                directory.mkdir()
                config = {"version": "v1.1", "vector_store": {"provider": "qdrant", "config": {
                    "collection_name": "memories", "path": str(directory / "store"), "embedding_model_dims": 1024}},
                    "llm": {"provider": "openai", "config": {"model": MODEL, "openai_base_url": QWEN_BASE_URL,
                        "api_key": api_key(), "temperature": 0, "response_callback": mem0_run._ledger_callback(f"locomo-example.{perspective.speaker}")}},
                    "embedder": {"provider": "openai", "config": {"model": "bge-m3", "openai_base_url": base + "/v1",
                        "api_key": "unused", "embedding_dims": 1024}}, "history_db_path": str(directory / "history.db"),
                    "custom_instructions": mem0_run.mem0_custom_instructions()}
                memory = Memory.from_config(config)
                memories.append(memory)
                memory.llm.client.close()
                memory.llm = QwenLLM(memory.llm.config)
                own = {"speaker": perspective.speaker, "user_id": perspective.user_id, "batches": [], "llm_trace": []}
                result["mem0"].append(own)
                original_generation = memory.llm.generate_response

                def generation(messages, **kwargs):
                    record = {"messages": messages, "response_format": kwargs.get("response_format")}
                    own["llm_trace"].append(record)
                    response = original_generation(messages, **kwargs)
                    record["response"] = response
                    return response

                memory.llm.generate_response = generation
                mem0_run._tls.observation_date = excerpt.start.date().isoformat()
                mapped = [{"role": "user" if u["speaker"] == perspective.speaker else "assistant",
                           "content": locomo.utterance_text(u)} for u in excerpt.utterances]
                for offset in range(0, len(mapped), 2):
                    added = memory.add(mapped[offset:offset+2], user_id=perspective.user_id,
                                       metadata={"timestamp": excerpt.date_str})
                    own["batches"].append({"dialog_ids": protocol["dialog_ids"][offset:offset+2], "result": added})
                own["saved_memories"] = memory.get_all(filters={"user_id": perspective.user_id})
                own["retrieval_trace"] = []
                original_embedding = memory.embedding_model.embed

                def embed(text, memory_action="add"):
                    vector = original_embedding(text, memory_action)
                    own["retrieval_trace"].append({"step": "embedding", "text": text, "action": memory_action, "dimensions": len(vector)})
                    return vector

                memory.embedding_model.embed = embed
                original_search = memory.vector_store.search

                def vector_search(query, vectors, top_k=5, filters=None):
                    found = original_search(query=query, vectors=vectors, top_k=top_k, filters=filters)
                    own["retrieval_trace"].append({"step": "vector_search", "query": query, "top_k": top_k,
                        "filters": filters, "candidates": [{"id": str(m.id), "score": m.score, "text": m.payload.get("data")} for m in found]})
                    return found

                memory.vector_store.search = vector_search
                original_keywords = memory.vector_store.keyword_search

                def keywords(query, top_k=5, filters=None):
                    found = original_keywords(query=query, top_k=top_k, filters=filters)
                    own["retrieval_trace"].append({"step": "keyword_search", "query": query,
                        "available": found is not None, "count": len(found) if found is not None else 0})
                    return found

                memory.vector_store.keyword_search = keywords
                own["search"] = memory.search(question.question, top_k=30, filters={"user_id": perspective.user_id}, explain=True)
                save(directory / "trace.json", own)
                save(output / "result.json", result)
                print(f"Mem0 {perspective.speaker}: saved {len(own['saved_memories']['results'])}, returned {len(own['search']['results'])}", flush=True)
            result["health_after"] = api("/health")
            result["status"] = "complete"
            save(output / "result.json", result)
            print(f"Complete: {output / 'result.json'}", flush=True)
        except Exception as error:
            result.update(status="failed", error=f"{type(error).__name__}: {str(error)[:300]}")
            save(output / "result.json", result)
            raise
        finally:
            for memory in memories:
                memory.llm.client.close()
                memory.embedding_model.client.close()
                memory.vector_store.client.close()
            process.terminate()
            try:
                process.wait(timeout=20)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()


if __name__ == "__main__":
    main()
