#!/usr/bin/env python
"""mem0 OSS (mem0ai 2.2.1) on LoCoMo: ingestion + retrieval, following mem0's official LoCoMo protocol
(third_party/mem0_evaluation/src/memzero/{add,search}.py) as closely as OSS 2.2.1 allows.

  ingest : per conversation two user_ids (<speaker>_<sample_id>), one per speaker perspective
           (own utterances role=user, the other speaker's role=assistant); each session's utterances
           are added in batches of 2 with metadata {"timestamp": <session date_time string>};
           mem0's LoCoMo custom_instructions are set as Memory custom_instructions.
  search : for each question, Memory.search(question, top_k=30, filters={"user_id": ...}) for both
           speakers (top_k 30 = mem0's Makefile run-mem0-search); context = both lists concatenated,
           each line "[<speaker>] <timestamp>: <memory>".

LLM: provider "openai" pointed at Qwen (qwen3.8-flash, temperature 0); thinking is disabled by a
3-line OpenAILLM subclass that passes extra_body={"enable_thinking": False} through mem0's own
generate_response(**kwargs) path. Usage is logged to the usage ledger via mem0's response_callback hook.
Embedder: provider "openai" pointed at the Memory daemon's /v1/embeddings (bge-m3, 1024-d).
Vector store: local on-disk Qdrant, one store per (conversation, perspective).

Usage:
  .venv/bin/python mem0_run.py --stage all --embed-url http://127.0.0.1:8765/v1
  .venv/bin/python mem0_run.py --stage ingest --convs conv-26 --sessions 2 --embed-url http://127.0.0.1:8773/v1
  .venv/bin/python mem0_run.py --stage search --convs conv-26 --limit 10 --embed-url ...
"""
from __future__ import annotations

import argparse
import ast
import json
import logging
import sys
import threading
import time
from datetime import datetime, timezone
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass
from pathlib import Path
from typing import Optional

sys.path.insert(0, str(Path(__file__).resolve().parent))

from common import env  # noqa: E402,F401  (must precede mem0 import: MEM0_TELEMETRY=False, caches)
from common import jsonl, locomo  # noqa: E402
from common.env import EVAL_DIR, RESULTS_DIR, STORES_DIR  # noqa: E402
from common.llm import (QWEN_BASE_URL, MODEL, NO_THINKING, api_key,  # noqa: E402
                        LLMOutputError, default_ledger, extract_json, set_ledger_path, usage_tokens)
from common.metrics import TOKEN_COUNTER, count_tokens  # noqa: E402
from common.protocol import guard  # noqa: E402
from common import model_metadata  # noqa: E402

log = logging.getLogger("mem0_run")

MEM0_EVAL_ADD = EVAL_DIR / "third_party" / "mem0_evaluation" / "src" / "memzero" / "add.py"


def mem0_custom_instructions() -> str:
    """`custom_instructions` string from mem0's LoCoMo add.py (read with ast, not executed)."""
    tree = ast.parse(MEM0_EVAL_ADD.read_text(encoding="utf-8"))
    for node in tree.body:
        if isinstance(node, ast.Assign) and any(getattr(t, "id", None) == "custom_instructions" for t in node.targets):
            return node.value.value
    raise KeyError("custom_instructions not found")


# ----------------------------------------------------------------------------- mem0 plumbing

_tls = threading.local()  # per-thread observation date for the extraction prompt


def _install_mem0_patches(observation_date: bool):
    """Import mem0 and (optionally) feed the session date as the extraction prompt's Observation Date.

    mem0 OSS 2.2.1 builds the V3 extraction prompt with generate_additive_extraction_prompt(...) but
    never passes `timestamp`, so "Observation Date" defaults to today's system date (2026) and relative
    references ("yesterday") are resolved against it; Memory.add(timestamp=...) raises in OSS
    ("platform-only"). The platform resolves them against the given timestamp, which is what this patch
    reproduces. Disable with --observation-date system.
    """
    import mem0.memory.main as mm
    from mem0.llms.openai import OpenAILLM

    if observation_date and not getattr(mm, "_eval_obs_patch", False):
        orig = mm.generate_additive_extraction_prompt

        def patched(*args, **kwargs):
            obs = getattr(_tls, "observation_date", None)
            if obs and kwargs.get("timestamp") is None:
                kwargs["timestamp"] = obs
            return orig(*args, **kwargs)

        mm.generate_additive_extraction_prompt = patched
        mm._eval_obs_patch = True

    class QwenNoThinkLLM(OpenAILLM):
        """mem0 OpenAILLM that disables Qwen thinking on every request."""

        def __init__(self, config):
            from openai import OpenAI
            super().__init__(config)
            self.client.close()
            # The parent silently prefers OPENROUTER_API_KEY if present. Pin the
            # intended backend, and let the harness own retries and usage records.
            self.client = OpenAI(api_key=self.config.api_key,
                                 base_url=self.config.openai_base_url, max_retries=0)

        def generate_response(self, messages, response_format=None, tools=None, tool_choice="auto", **kwargs):
            _tls.llm_started_at = datetime.now(timezone.utc)
            _tls.llm_started_perf = time.perf_counter()
            kwargs["extra_body"] = {**(kwargs.get("extra_body") or {}), **NO_THINKING}
            output = super().generate_response(messages, response_format=response_format, tools=tools,
                                                tool_choice=tool_choice, **kwargs)
            if response_format and not tools:
                # OSS otherwise swallows malformed extraction JSON and reports an
                # empty successful add, making an incomplete baseline look valid.
                try:
                    parsed = json.loads(extract_json(output), strict=False)
                except (ValueError, TypeError) as e:
                    raise LLMOutputError("mem0 extraction returned invalid JSON") from e
                if not isinstance(parsed, dict) or not isinstance(parsed.get("memory"), list):
                    raise LLMOutputError("mem0 extraction requires a memory list")
            return output

    return mm.Memory, QwenNoThinkLLM


def _ledger_callback(component: str):
    ledger = default_ledger()

    def cb(llm, response, params):
        hit, miss, out, reasoning = usage_tokens(getattr(response, "usage", None))
        choice = response.choices[0] if getattr(response, "choices", None) else None
        msg = choice.message if choice else None
        content = (msg.content if msg else "") or ""
        parse_error = None
        if params.get("response_format"):
            try:
                parsed = json.loads(extract_json(content), strict=False)
                if not isinstance(parsed, dict) or not isinstance(parsed.get("memory"), list):
                    raise ValueError("mem0 extraction requires a memory list")
            except Exception:
                parse_error = True
        reasoning_content = getattr(msg, "reasoning_content", None) if msg else None
        finish = choice.finish_reason if choice else None
        row = ledger.record(component, hit, miss, out, model=params.get("model", MODEL),
                      ts=getattr(_tls, "llm_started_at", None),
                      latency_ms=(time.perf_counter() - _tls.llm_started_perf) * 1000
                                 if hasattr(_tls, "llm_started_perf") else None,
                      source="mem0/response_callback", response_id=getattr(response, "id", None),
                      reasoning_tokens=reasoning or None,
                      reasoning_chars=len(reasoning_content) if reasoning_content else None,
                      finish=finish if finish != "stop" else None, parse_error=parse_error,
                      **getattr(_tls, "usage_meta", {}))
        if hasattr(_tls, "add_usage_rows"):
            _tls.add_usage_rows.append(row)
    return cb


@dataclass
class Perspective:
    conv: str
    speaker: str        # whose memories (role=user)
    other: str
    user_id: str
    is_a: bool          # speaker == speaker_a


def perspectives(conv: dict) -> list[Perspective]:
    a, b = locomo.speakers(conv)
    sid = conv["sample_id"]
    return [Perspective(sid, a, b, f"{a}_{sid}", True), Perspective(sid, b, a, f"{b}_{sid}", False)]


class Mem0Stores:
    def __init__(self, args):
        from importlib.metadata import version
        if version("mem0ai") != "2.2.1":
            raise RuntimeError("this protocol requires mem0ai==2.2.1; install eval/requirements.txt")
        self.args = args
        self.Memory, self.LLMClass = _install_mem0_patches(args.observation_date == "session")
        self._mems: dict[str, object] = {}
        self._lock = threading.Lock()

    def store_dir(self, p: Perspective) -> Path:
        return Path(self.args.stores_dir) / self.args.tag / p.conv / p.user_id

    def get(self, p: Perspective):
        with self._lock:
            if p.user_id in self._mems:
                return self._mems[p.user_id]
            d = self.store_dir(p)
            d.mkdir(parents=True, exist_ok=True)
            cfg = {
                "version": "v1.1",
                "vector_store": {"provider": "qdrant", "config": {
                    "collection_name": "mem0", "path": str(d / "qdrant"), "embedding_model_dims": 1024,
                    "on_disk": True}},
                "llm": {"provider": "openai", "config": {
                    "model": MODEL, "openai_base_url": getattr(self.args, 'llm_base_url', QWEN_BASE_URL), "api_key": api_key(),
                    "temperature": 0.0, "response_callback": _ledger_callback(f"{self.args.tag}.add")}},
                "embedder": {"provider": "openai", "config": {
                    "model": "bge-m3", "openai_base_url": self.args.embed_url, "api_key": "unused",
                    "embedding_dims": 1024}},
                "history_db_path": str(d / "history.db"),
            }
            if not self.args.no_custom_instructions:
                cfg["custom_instructions"] = mem0_custom_instructions()
            m = self.Memory.from_config(cfg)
            m.llm.client.close()
            m.llm = self.LLMClass(m.llm.config)  # same config, thinking disabled
            self._mems[p.user_id] = m
            return m

    def close(self):
        for m in self._mems.values():
            # mem0's close() only closes SQLite. Release Qdrant's local directory
            # lock and HTTP clients too so a resumed runner can reopen the store.
            clients = [getattr(getattr(m, name, None), "client", None)
                       for name in ("vector_store", "llm", "embedding_model")]
            for resource in [m, *clients]:
                if resource is not None:
                    try:
                        resource.close()
                    except Exception:
                        pass
        self._mems.clear()


# ----------------------------------------------------------------------------- ingestion

def batches_for(conv: dict, p: Perspective, max_sessions: Optional[int], batch_size: int):
    """Yield (session, batch_index, messages, timestamp_str, observation_date) in mem0-eval order."""
    for s in locomo.sessions(conv)[: max_sessions or None]:
        msgs = []
        for u in s.utterances:
            role = "user" if u["speaker"] == p.speaker else "assistant"
            if u["speaker"] not in (p.speaker, p.other):
                raise ValueError(f"unknown speaker {u['speaker']}")
            msgs.append({"role": role, "content": locomo.utterance_text(u)})
        for bi, i in enumerate(range(0, len(msgs), batch_size)):
            yield s, bi, msgs[i: i + batch_size], s.date_str, s.start.date().isoformat()


class Stop(Exception):
    pass


def ingest_perspective(stores: Mem0Stores, conv: dict, p: Perspective, progress: Path, done: set,
                       stop: threading.Event) -> dict:
    a = stores.args
    m = stores.get(p)
    ledger = default_ledger()
    n_add = n_skip = n_fail = n_mem = 0
    for s, bi, msgs, ts_str, obs in batches_for(conv, p, a.sessions, a.batch_size):
        key = (p.user_id, s.key, bi)
        if key in done:
            n_skip += 1
            continue
        if getattr(a, "embedding_runtime", None) is not None:
            model_metadata.check_embedding_runtime(a.embed_url, a.embedding_runtime)
        if stop.is_set():
            raise Stop()
        _tls.observation_date = obs if a.observation_date == "session" else None
        t0 = time.perf_counter()
        err = None
        res = None
        _tls.add_usage_rows = []
        add_id = f"{p.user_id}/{s.key}/{bi}"
        for attempt in range(1, a.retries + 1):
            _tls.usage_meta = {"conv": p.conv, "user_id": p.user_id, "session": s.key, "batch": bi,
                                 "add_id": add_id, "attempt": attempt}
            try:
                res = m.add(msgs, user_id=p.user_id, metadata={"timestamp": ts_str})
                err = None
                break
            except Exception as e:  # mem0 raises LLMError on API failure
                err = f"{type(e).__name__}: {str(e)[:300]}"
                log.warning("add failed %s %s b%d attempt %d: %s", p.user_id, s.key, bi, attempt, err)
                if attempt < a.retries:
                    time.sleep(min(60, 2 ** attempt))
        ms = (time.perf_counter() - t0) * 1000
        batch_usage = {"add_id": add_id, "llm_calls": len(_tls.add_usage_rows),
                      "hit": sum(r["hit"] for r in _tls.add_usage_rows),
                      "miss": sum(r["miss"] for r in _tls.add_usage_rows),
                      "out": sum(r["out"] for r in _tls.add_usage_rows)}
        if err:
            n_fail += 1
            jsonl.append(progress, {"conv": p.conv, "user_id": p.user_id, "session": s.key, "batch": bi,
                                    "ok": False, "error": err, "usage": batch_usage})
            break  # preserve chronological extraction; resume this batch before later sessions
        added = len((res or {}).get("results", []))
        n_add += 1
        n_mem += added
        jsonl.append(progress, {"conv": p.conv, "user_id": p.user_id, "session": s.key, "batch": bi, "ok": True,
                                "n_messages": len(msgs), "n_added": added, "ms": round(ms, 1), "usage": batch_usage})
    return {"user_id": p.user_id, "added_batches": n_add, "skipped": n_skip, "failed": n_fail, "memories": n_mem}


def run_ingest(args, data: list[dict], stores: Mem0Stores) -> None:
    progress = Path(args.results_dir) / args.tag / "ingest_progress.jsonl"
    done = {(r["user_id"], r["session"], r["batch"]) for r in jsonl.read(progress) if r.get("ok")}
    jobs = [(c, p) for c in data for p in perspectives(c)
            if args.perspectives == "both" or p.is_a]
    stop = threading.Event()
    failures = []
    log.info("ingest: %d perspectives, %d batches already done", len(jobs), len(done))
    with ThreadPoolExecutor(max_workers=args.workers) as ex:
        futs = {ex.submit(ingest_perspective, stores, c, p, progress, done, stop): p for c, p in jobs}
        for f in as_completed(futs):
            p = futs[f]
            try:
                result = f.result()
                log.info("ingest done %s: %s", p.user_id, result)
                if result["failed"]:
                    failures.append(p.user_id)
            except Stop as e:
                log.warning("ingest stopped %s: %s", p.user_id, e)
                failures.append(p.user_id)
            except Exception:
                log.exception("ingest crashed %s", p.user_id)
                stop.set()
                failures.append(p.user_id)
    log.info("model calls this process: %d", default_ledger().calls)
    if failures:
        raise RuntimeError(f"incomplete ingestion for {failures}; rerun ingest to resume before retrieval")


# ----------------------------------------------------------------------------- search

def format_context(lists: list[tuple[str, list[dict]]]) -> str:
    lines = []
    for speaker, mems in lists:
        for x in mems:
            lines.append(f"[{speaker}] {x['timestamp']}: {x['memory']}")
    return "\n".join(lines)


def search_one(m, query: str, user_id: str, top_k: int) -> tuple[list[dict], float]:
    t0 = time.perf_counter()
    for attempt in range(1, 4):
        try:
            res = m.search(query, top_k=top_k, filters={"user_id": user_id})
            break
        except Exception:
            if attempt == 3:
                raise
            time.sleep(attempt)
    ms = (time.perf_counter() - t0) * 1000
    out = []
    for x in res.get("results", []):
        md = x.get("metadata") or {}
        out.append({"memory": x["memory"], "timestamp": md.get("timestamp", ""), "score": round(x.get("score") or 0, 4)})
    return out, ms


def run_search(args, data: list[dict], stores: Mem0Stores) -> None:
    setting = args.setting or f"k{args.top_k}"
    out_path = Path(args.results_dir) / args.tag / setting / "contexts.jsonl"
    guard(out_path.parent / "protocol.json", {"top_k": args.top_k, "token_counter": TOKEN_COUNTER})
    have = jsonl.keys(out_path)
    progress = Path(args.results_dir) / args.tag / "ingest_progress.jsonl"
    ok_batches = {(r["user_id"], r["session"], r["batch"]) for r in jsonl.read(progress) if r.get("ok")}
    for conv in data:
        ps = [p for p in perspectives(conv) if args.perspectives == "both" or p.is_a]
        for p in ps:
            expected = {(p.user_id, s.key, bi) for s, bi, *_ in
                        batches_for(conv, p, args.sessions, args.batch_size)}
            missing = expected - ok_batches
            if missing:
                raise RuntimeError(f"{p.user_id}: {len(missing)} batches not ingested; resume --stage ingest first")
        qas = locomo.qas(conv)
        if args.qids:
            qas = [q for q in qas if q.qid in set(args.qids)]
        if args.limit:
            qas = qas[: args.limit]
        mems = [stores.get(p) for p in ps]
        for q in qas:
            if q.qid in have:
                continue
            if getattr(args, "embedding_runtime", None) is not None:
                model_metadata.check_embedding_runtime(args.embed_url, args.embedding_runtime)
            per = []
            for p, m in zip(ps, mems):
                res, ms = search_one(m, q.question, p.user_id, args.top_k)
                per.append((p, res, ms))
            context = format_context([(p.speaker, res) for p, res, _ in per])
            rec = {
                "system": args.tag, "setting": setting, "conv": q.conv, "qid": q.qid, "category": q.category,
                "category_name": q.category_name, "question": q.question, "answer": q.answer,
                "context": context, "context_tokens": count_tokens(context), "token_counter": TOKEN_COUNTER,
                "n_memories": sum(len(r) for _, r, _ in per), "n_turns": 0,
                "search_ms": round(sum(ms for _, _, ms in per), 2),
                "search_ms_parts": [round(ms, 2) for _, _, ms in per],
                "speaker_1": per[0][0].speaker, "speaker_2": per[0][0].other,
                "speaker_1_memories": [f"{x['timestamp']}: {x['memory']}" for x in per[0][1]],
                "speaker_2_memories": [f"{x['timestamp']}: {x['memory']}" for x in per[1][1]] if len(per) > 1 else [],
                "scores": [[x["score"] for x in r] for _, r, _ in per],
                "top_k": args.top_k,
                "embedding_runtime": getattr(args, "embedding_runtime", None),
            }
            jsonl.append(out_path, rec)
        log.info("search done %s (%d questions)", conv["sample_id"], len(qas))


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--stage", choices=["ingest", "search", "all"], default="all")
    ap.add_argument("--embed-url", required=True, help="OpenAI-compatible embeddings base, e.g. http://127.0.0.1:8765/v1")
    ap.add_argument("--tag", default="mem0", help="system name: results/<tag>/, stores/<tag>/, ledger component <tag>.add")
    ap.add_argument("--setting", default=None, help="results subdir for contexts (default k<top_k>)")
    ap.add_argument("--convs", type=lambda s: s.split(","), default=None)
    ap.add_argument("--sessions", type=int, default=None, help="only the first N sessions of each conversation (testing)")
    ap.add_argument("--perspectives", choices=["both", "a"], default="both",
                    help="'a' = only speaker_a's user_id (halves ingestion calls; not mem0's protocol)")
    ap.add_argument("--batch-size", type=int, default=2)
    ap.add_argument("--top-k", type=int, default=30)
    ap.add_argument("--limit", type=int, default=None, help="questions per conversation (testing)")
    ap.add_argument("--qids", type=lambda s: s.split(","), default=None)
    ap.add_argument("--workers", type=int, default=20, help="parallel perspectives during ingestion")
    ap.add_argument("--retries", type=int, default=3)
    ap.add_argument("--observation-date", choices=["session", "system"], default="session")
    ap.add_argument("--no-custom-instructions", action="store_true")
    ap.add_argument("--results-dir", default=str(RESULTS_DIR))
    ap.add_argument("--stores-dir", default=str(STORES_DIR))
    ap.add_argument("-v", "--verbose", action="store_true")
    args = ap.parse_args()
    if Path(args.results_dir).resolve() != RESULTS_DIR.resolve():
        set_ledger_path(Path(args.results_dir) / "usage_ledger.jsonl")

    logging.basicConfig(level=logging.DEBUG if args.verbose else logging.INFO,
                        format="%(asctime)s %(levelname)s %(name)s: %(message)s")
    for noisy in ("httpx", "openai", "mem0", "qdrant_client", "urllib3", "httpcore"):
        logging.getLogger(noisy).setLevel(logging.WARNING)

    data = locomo.select(locomo.load(), args.convs)
    args.embedding_runtime = model_metadata.fetch_embedding_runtime(args.embed_url)
    (Path(args.results_dir) / args.tag).mkdir(parents=True, exist_ok=True)
    guard(Path(args.results_dir) / args.tag / "protocol.json", {
        "sessions": args.sessions, "batch_size": args.batch_size, "perspectives": args.perspectives,
        "observation_date": args.observation_date, "no_custom_instructions": args.no_custom_instructions,
        "model": MODEL, "embed_url": args.embed_url, "stores_dir": str(Path(args.stores_dir).resolve()),
        "mem0_version": "2.2.1", "embedding_runtime": args.embedding_runtime})
    cfg_path = Path(args.results_dir) / args.tag / "run_config.json"
    cfg_path.write_text(json.dumps({k: v for k, v in vars(args).items()}, indent=2, default=str))
    model_metadata.snapshot(cfg_path.parent / "embedding-runtime.json", args.embedding_runtime,
                            model_metadata.health_base(args.embed_url) + "/health")
    stores = Mem0Stores(args)
    try:
        if args.stage in ("ingest", "all"):
            run_ingest(args, data, stores)
        if args.stage in ("search", "all"):
            run_search(args, data, stores)
        model_metadata.check_embedding_runtime(args.embed_url, args.embedding_runtime)
    finally:
        stores.close()


if __name__ == "__main__":
    main()
