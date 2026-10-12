"""Qwen client (OpenAI-compatible) with concurrency limit, retries and a usage ledger.

Every request: model qwen3.8-flash, temperature 0, thinking disabled via
extra_body={"enable_thinking": False} (qwen3.8-flash defaults to thinking mode).

Ledger: eval/results/usage_ledger.jsonl, one JSON line per response:
  {ts, component, model, hit, miss, out, latency_ms, ...}
"""
from __future__ import annotations

import asyncio
import fcntl
import json
import os
import random
import re
import threading
import time
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Optional

from . import env  # noqa: F401  (environment + key loading)
from .env import RESULTS_DIR

QWEN_BASE_URL = os.environ.get("QWEN_BASE_URL", "https://maas.qianwenaiapi.com/compatible-mode/v1")
MODEL = os.environ.get("EVAL_MODEL", os.environ.get("LLM_MODEL", "qwen3.8-flash"))
NO_THINKING: dict = {"enable_thinking": False}
LEDGER_PATH = Path(os.environ.get("EVAL_LEDGER", RESULTS_DIR / "usage_ledger.jsonl"))


def usage_tokens(usage: Any) -> tuple[int, int, int, int]:
    """(hit, miss, out, reasoning) from an OpenAI-style usage object or dict, including cached-token details."""
    if usage is None:
        return 0, 0, 0, 0
    get = (lambda k: usage.get(k)) if isinstance(usage, dict) else (lambda k: getattr(usage, k, None))
    prompt = int(get("prompt_tokens") or 0)
    out = int(get("completion_tokens") or 0)
    hit = get("prompt_cache_hit_tokens")
    if hit is None:
        details = get("prompt_tokens_details")
        if details is not None:
            hit = details.get("cached_tokens") if isinstance(details, dict) else getattr(details, "cached_tokens", None)
    miss = get("prompt_cache_miss_tokens")
    hit = int(hit) if hit is not None else 0
    miss = int(miss) if miss is not None else max(prompt - hit, 0)
    details = get("completion_tokens_details")
    reasoning = 0
    if details is not None:
        r = details.get("reasoning_tokens") if isinstance(details, dict) else getattr(details, "reasoning_tokens", None)
        reasoning = int(r or 0)
    return hit, miss, out, reasoning


# ----------------------------------------------------------------------------- ledger

class Ledger:
    """Append-only JSONL ledger, safe across threads and processes."""

    def __init__(self, path: Path | str = LEDGER_PATH):
        self.path = Path(path)
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self._lock = threading.Lock()
        self.calls = 0

    @staticmethod
    def row(component: str, hit: int, miss: int, out: int, *, ts: Optional[datetime] = None,
               model: str = MODEL, latency_ms: Optional[float] = None, **extra: Any) -> dict:
        ts = ts or datetime.now(timezone.utc)
        row = {
            "ts": ts.isoformat(timespec="seconds"),
            "component": component,
            "model": model,
            "hit": int(hit),
            "miss": int(miss),
            "out": int(out),
        }
        if latency_ms is not None:
            row["latency_ms"] = round(latency_ms, 1)
        row.update({k: v for k, v in extra.items() if v is not None})
        return row

    def record(self, component: str, hit: int, miss: int, out: int, **kwargs: Any) -> dict:
        row = self.row(component, hit, miss, out, **kwargs)
        self.append_rows([row])
        return row

    def record_many(self, entries: list[dict]) -> list[dict]:
        return self.append_rows([self.row(**entry) for entry in entries])

    def append_rows(self, rows: list[dict]) -> list[dict]:
        """Atomically append a batch, deduplicating persistent event_id across processes."""
        if not rows:
            return []
        added = []
        with self._lock:
            with open(self.path, "a+", encoding="utf-8") as f:
                fcntl.flock(f, fcntl.LOCK_EX)
                f.seek(0)
                content = f.read()
                seen = set()
                lines = content.splitlines(keepends=True)
                for i, line in enumerate(lines):
                    if not line.strip():
                        continue
                    try:
                        previous = json.loads(line)
                    except json.JSONDecodeError as e:
                        if i == len(lines) - 1 and not line.endswith("\n"):
                            f.seek(0)
                            f.truncate(len("".join(lines[:i]).encode("utf-8")))
                            break
                        raise ValueError(f"corrupt ledger record in {self.path}") from e
                    if previous.get("event_id"):
                        seen.add(previous["event_id"])
                else:
                    if content and not content.endswith("\n"):
                        f.write("\n")
                for row in rows:
                    event_id = row.get("event_id")
                    if event_id and event_id in seen:
                        continue
                    if event_id:
                        seen.add(event_id)
                    f.write(json.dumps(row, ensure_ascii=False) + "\n")
                    added.append(row)
                f.flush()
                fcntl.flock(f, fcntl.LOCK_UN)
            self.calls += sum(int(row.get("calls", 1)) for row in added)
        return added


_default_ledger: Optional[Ledger] = None


def default_ledger() -> Ledger:
    global _default_ledger
    if _default_ledger is None:
        _default_ledger = Ledger()
    return _default_ledger


def set_ledger_path(path: Path | str) -> Ledger:
    """Point the process-wide ledger elsewhere (tests / alternative results dirs)."""
    global _default_ledger
    _default_ledger = Ledger(path)
    return _default_ledger


# ----------------------------------------------------------------------------- client

class LLMOutputError(RuntimeError):
    pass


class FatalAPIError(RuntimeError):
    """Non-retryable API error (auth, insufficient balance, bad request)."""


@dataclass
class LLMResult:
    text: str
    json: Any = None
    hit: int = 0
    miss: int = 0
    out: int = 0
    latency_ms: float = 0.0
    finish_reason: Optional[str] = None
    attempts: int = 1
    extra: dict = field(default_factory=dict)


def extract_json(text: str) -> str:
    """Same logic as mem0.memory.utils.extract_json (used by mem0's llm_judge)."""
    text = text.strip()
    match = re.search(r"```(?:json)?\s*(.*?)\s*```", text, re.DOTALL)
    if match:
        return match.group(1)
    start, end = text.find("{"), text.rfind("}")
    if start != -1 and end != -1 and end > start:
        return text[start:end + 1]
    return text


def api_key() -> str:
    key = os.environ.get("QWEN_API_KEY")
    if not key:
        raise RuntimeError("QWEN_API_KEY not set; add it to the project root .env or process environment")
    return key


class LLM:
    """Async Qwen chat client. One instance per process is enough."""

    def __init__(self, *, concurrency: int = 8, max_attempts: int = 6, timeout: float = 180.0,
                 ledger: Optional[Ledger] = None, model: str = MODEL):
        from openai import AsyncOpenAI

        self.model = model
        self.client = AsyncOpenAI(api_key=api_key(), base_url=QWEN_BASE_URL, max_retries=0, timeout=timeout)
        self.sem = asyncio.Semaphore(concurrency)
        self.max_attempts = max_attempts
        self.ledger = ledger or default_ledger()

    async def chat(self, messages: list[dict], *, component: str, json_mode: bool = False,
                   max_tokens: Optional[int] = None, meta: Optional[dict] = None) -> LLMResult:
        import openai

        last_err: Optional[BaseException] = None
        for attempt in range(1, self.max_attempts + 1):
            kwargs: dict[str, Any] = dict(model=self.model, messages=messages, temperature=0.0, extra_body=NO_THINKING)
            if max_tokens:
                kwargs["max_tokens"] = max_tokens
            if json_mode:
                kwargs["response_format"] = {"type": "json_object"}
            t0 = time.perf_counter()
            try:
                async with self.sem:
                    resp = await self.client.chat.completions.create(**kwargs)
            except (openai.RateLimitError, openai.APIConnectionError, openai.InternalServerError) as e:
                last_err = e
            except openai.APIStatusError as e:
                if e.status_code >= 500 or e.status_code in (408, 409, 429):
                    last_err = e
                else:  # 400 bad request, 401 auth, 402 insufficient balance, 422 ...
                    raise FatalAPIError(f"Qwen API {e.status_code}: {str(e)[:300]}") from e
            else:
                latency_ms = (time.perf_counter() - t0) * 1000
                hit, miss, out, reasoning = usage_tokens(resp.usage)
                choice = resp.choices[0] if resp.choices else None
                msg = choice.message if choice else None
                text = (msg.content if msg else None) or ""
                reasoning_content = getattr(msg, "reasoning_content", None) if msg else None
                finish = choice.finish_reason if choice else None
                parsed, parse_ok = None, True
                if json_mode:
                    try:
                        parsed = json.loads(extract_json(text)) if text.strip() else None
                        parse_ok = parsed is not None
                    except json.JSONDecodeError:
                        parse_ok = False
                self.ledger.record(
                    component, hit, miss, out, latency_ms=latency_ms, model=self.model,
                    reasoning_tokens=reasoning or None,
                    reasoning_chars=len(reasoning_content) if reasoning_content else None,
                    finish=finish if finish != "stop" else None,
                    parse_error=True if not parse_ok else None,
                    **(meta or {}),
                )
                if parse_ok:
                    return LLMResult(text=text, json=parsed, hit=hit, miss=miss, out=out,
                                     latency_ms=latency_ms, finish_reason=finish, attempts=attempt,
                                     extra={"reasoning_tokens": reasoning,
                                            "reasoning_chars": len(reasoning_content or "")})
                last_err = LLMOutputError(f"unparsable JSON output (finish={finish}): {text[:200]!r}")
            if attempt < self.max_attempts:
                await asyncio.sleep(min(60.0, 2 ** (attempt - 1)) + random.random())
        if isinstance(last_err, LLMOutputError):
            raise last_err
        raise RuntimeError(f"LLM call failed after {self.max_attempts} attempts: {last_err!r}") from last_err

    async def aclose(self) -> None:
        await self.client.close()
