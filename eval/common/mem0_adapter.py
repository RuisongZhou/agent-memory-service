"""mem0 OSS (2.2.1) adapter shared by the mem0 runners and the fresh-store comparison tools.

`common.env` is imported first on purpose: it sets MEM0_TELEMETRY and the cache directories, which must
be in place before the `mem0` package is imported. The package itself is only imported lazily, inside
`install_mem0_patches`.
"""
from __future__ import annotations

import ast
import json
import threading
import time
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path

from . import env  # noqa: F401  (must precede mem0 import: MEM0_TELEMETRY=False, caches)
from . import locomo
from .env import EVAL_DIR
from .llm import (QWEN_BASE_URL, MODEL, NO_THINKING, api_key,
                  LLMOutputError, default_ledger, extract_json, usage_tokens)

MEM0_EVAL_ADD = EVAL_DIR / "third_party" / "mem0_evaluation" / "src" / "memzero" / "add.py"


def mem0_custom_instructions() -> str:
    """`custom_instructions` string from mem0's LoCoMo add.py (read with ast, not executed)."""
    tree = ast.parse(MEM0_EVAL_ADD.read_text(encoding="utf-8"))
    for node in tree.body:
        if isinstance(node, ast.Assign) and any(getattr(t, "id", None) == "custom_instructions" for t in node.targets):
            return node.value.value
    raise KeyError("custom_instructions not found")


# Per-thread state shared with mem0's patched LLM: observation_date (extraction prompt), usage_meta and
# add_usage_rows (ledger attribution), llm_started_at/llm_started_perf (latency).
thread_context = threading.local()


def install_mem0_patches(observation_date: bool):
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
            obs = getattr(thread_context, "observation_date", None)
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
            thread_context.llm_started_at = datetime.now(timezone.utc)
            thread_context.llm_started_perf = time.perf_counter()
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


def ledger_callback(component: str):
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
                      ts=getattr(thread_context, "llm_started_at", None),
                      latency_ms=(time.perf_counter() - thread_context.llm_started_perf) * 1000
                                 if hasattr(thread_context, "llm_started_perf") else None,
                      source="mem0/response_callback", response_id=getattr(response, "id", None),
                      reasoning_tokens=reasoning or None,
                      reasoning_chars=len(reasoning_content) if reasoning_content else None,
                      finish=finish if finish != "stop" else None, parse_error=parse_error,
                      **getattr(thread_context, "usage_meta", {}))
        if hasattr(thread_context, "add_usage_rows"):
            thread_context.add_usage_rows.append(row)
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
        self.Memory, self.LLMClass = install_mem0_patches(args.observation_date == "session")
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
                    "temperature": 0.0, "response_callback": ledger_callback(f"{self.args.tag}.add")}},
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


