#!/usr/bin/env python
"""Memory memory daemon on LoCoMo: ingestion, daily jobs, retrieval in two settings (SPEC §4, §5).

Stages (default --stage all runs them in this order; each skips work already recorded):
  ingest         per conversation = tenant (<sample_id>-en/-zh with --prompt-variant, otherwise <sample_id>);
                 X-Memory-Platform: locomo;
                 conversations in parallel, sessions sequential: POST /v1/add_memories per turn,
                 then {"sessionEnded": true, "scope": ...}, then POST /admin/wait_idle.
  jobs           POST /admin/run_jobs {"jobs":["profile","project_notes"], "asOf": last session + 1 day}.
  search-buffer  setting "buffer" (short-term buffer retained): POST /v1/search_memories
                 {query, scope:{project}, asOf, touch:false, topK:10, tokenBudget:2000} per question.
  purge          POST /admin/run_jobs {"jobs":["purge"], "retentionDays":0, "asOf": ...} (+ wait_idle).
  search-main    setting "main" (buffer purged): same search.
Searches run one at a time (--search-concurrency 1) after all ingestion so latencies are not
contaminated by background extraction.

The daemon's tenant-persistent llm_usage table is exported and appended to the usage ledger
as '<tag>.daemon/<source stage>', deduplicated by tenant and record id across restarts.

  .venv/bin/python memory_run.py --base-url http://127.0.0.1:8765 --tag memory-en --prompt-variant en
  .venv/bin/python memory_run.py --base-url http://127.0.0.1:8765 --tag memory-zh --prompt-variant zh --convs conv-26
"""
from __future__ import annotations

import argparse
import asyncio
import json
import logging
import sys
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Optional

sys.path.insert(0, str(Path(__file__).resolve().parent))

from common import env  # noqa: E402,F401
from common import jsonl, locomo  # noqa: E402
from common.env import RESULTS_DIR  # noqa: E402
from common.llm import default_ledger, set_ledger_path  # noqa: E402
from common.usage_records import UsageRecordError, usage_entries  # noqa: E402
from common.metrics import TOKEN_COUNTER, count_tokens  # noqa: E402
from common.protocol import guard  # noqa: E402
from common import model_metadata  # noqa: E402

log = logging.getLogger("memory_run")
STAGES = ["ingest", "jobs", "search-buffer", "purge", "search-main"]


class DaemonError(RuntimeError):
    pass


# ----------------------------------------------------------------------------- HTTP

class Daemon:
    def __init__(self, base_url: str, platform: str = "locomo", timeout: float = 300.0):
        import httpx

        self.base = base_url.rstrip("/")
        self.platform = platform
        self.client = httpx.AsyncClient(timeout=httpx.Timeout(timeout, connect=10.0), trust_env=False)
        self.httpx = httpx

    async def post(self, path: str, tenant: str, body: Optional[dict] = None, *, timeout: Optional[float] = None,
                   retries: int = 8) -> Any:
        headers = {"X-Memory-Tenant": tenant, "X-Memory-Platform": self.platform}
        last = None
        for attempt in range(1, retries + 1):
            try:
                r = await self.client.post(self.base + path, json=body if body is not None else {},
                                           headers=headers, timeout=timeout or self.client.timeout)
            except (self.httpx.TransportError,) as e:
                last = f"{type(e).__name__}: {e}"
            else:
                if r.status_code < 400:
                    return r.json() if r.content else {}
                if r.status_code >= 500 and r.status_code != 501:
                    last = f"HTTP {r.status_code}: {r.text[:300]}"
                else:
                    raise DaemonError(f"POST {path} [{tenant}] -> HTTP {r.status_code}: {r.text[:500]}")
            log.warning("POST %s [%s] attempt %d failed: %s", path, tenant, attempt, last)
            await asyncio.sleep(min(60, 2 ** attempt))
        raise DaemonError(f"POST {path} [{tenant}] failed after {retries} attempts: {last}")

    async def health(self) -> dict:
        r = await self.client.get(self.base + "/health", timeout=30)
        r.raise_for_status()
        return r.json()

    async def aclose(self):
        await self.client.aclose()


class UsagePoller:
    """Collect returned responses from each tenant's persistent llm_usage records."""

    def __init__(self, daemon: Daemon, tag: str, interval: float, checkpoint: Optional[Path] = None):
        self.d, self.tag, self.interval = daemon, tag, interval
        self.stage = "ingest"
        self.tenants: list[str] = []
        self._task: Optional[asyncio.Task] = None
        self._lock = asyncio.Lock()
        # Legacy constructor parameter retained for existing callers. The ledger
        # itself is now the checkpoint; process counters are never used for bills.

    async def flush(self) -> None:
        async with self._lock:
            entries = []
            for tenant in self.tenants:
                exported = await self.d.post("/admin/export", tenant, {"table": "llm_usage"}, retries=1)
                try:
                    entries.extend(usage_entries(exported.get("records"), tenant, self.tag))
                except UsageRecordError as e:
                    raise DaemonError(str(e)) from e
            default_ledger().record_many(entries)

    async def set_stage(self, stage: str) -> None:
        await self.flush()
        self.stage = stage

    async def _loop(self):
        while True:
            await asyncio.sleep(self.interval)
            await self.flush()

    async def start(self):
        await self.flush()  # recover returned calls missed by an earlier process
        self._task = asyncio.create_task(self._loop())

    async def stop(self):
        if self._task:
            self._task.cancel()
            try:
                await self._task
            except asyncio.CancelledError:
                pass
        await self.flush()


# ----------------------------------------------------------------------------- formatting

def fmt_time(s: Optional[str]) -> str:
    if not s:
        return "?"
    s = str(s)
    if "T" in s and len(s) >= 16:
        return s[:10] + " " + s[11:16]
    return s


def _time_key(s: Optional[str]) -> str:
    return str(s or "")


def format_context(resp: dict, order: str = "time") -> tuple[str, dict]:
    """Memory search response -> context string: profiles, memories as [effectiveAt] text, recent turns."""
    results = resp.get("results") or []
    profiles, mems = list(resp.get("profiles") or []), []
    profile_ids = {p.get("id") for p in profiles if isinstance(p, dict)}
    for r in results:
        m = r.get("memory", r) if isinstance(r, dict) else {}
        if not isinstance(m, dict):
            continue
        item = dict(m)
        item["_score"] = r.get("score")
        if m.get("type") == "profile":
            if m.get("id") not in profile_ids:
                profiles.append(item)
                profile_ids.add(m.get("id"))
        else:
            mems.append(item)
    if order == "evidence":
        context, info = format_context(resp, "score")
        blocks = ["Source dates indicate when a statement was made, not when an event occurred. Resolve relative time against the source date; event dates are in the facts."]
        for result in results:
            m = result.get("memory", {})
            if m.get("type") == "profile":
                continue
            mark = " (conflicting)" if m.get("status") == "conflict" else ""
            if m.get("status") == "candidate":
                mark = " (unconfirmed)"
            elif m.get("status") == "superseded" or m.get("validUntil"):
                mark = f" (historical, valid until {fmt_time(m.get('validUntil'))})"
            block = f"Fact{mark}: {m.get('text', '')}"
            if not result.get("evidence"):
                block += f" (effective from {fmt_time(m.get('effectiveAt'))})"
            for e in result.get("evidence", []):
                if e.get("context"):
                    block += f"\nSource question at {fmt_time(e['context'].get('statedAt'))}: {e['context'].get('span', '')}"
                block += f"\nSource at {fmt_time(e.get('statedAt'))}: {e.get('span', '')}"
            for old in result.get("history", []):
                block += f"\nHistorical value (valid until {fmt_time(old.get('validUntil'))}): {old.get('text', '')}"
            blocks.append(block)
        for t in resp.get("recentTurns", []):
            blocks.append(f"Conversation at {fmt_time(t.get('timestamp'))}: {t.get('digest', '')}")
        for p in profiles:
            blocks.append(f"Summary (use the specific facts and source quotes above for details): {p.get('text', '')}")
        return "\n\n".join(blocks), info
    lines = []
    for p in profiles:
        kind = (p.get("scope") or {}).get("kind") if isinstance(p.get("scope"), dict) else None
        label = "Project notes" if kind == "project" else "User profile"
        lines.append(f"{label}: {p.get('text', '')}")
    seq = list(enumerate(mems))
    if order == "time":
        seq.sort(key=lambda im: (_time_key(im[1].get("effectiveAt")), im[0]))
    for _, m in seq:
        status = m.get("status")
        mark = ""
        if status == "conflict":
            mark = " (conflicting)"
        elif status == "superseded" or m.get("validUntil"):
            mark = f" (old value, valid until {fmt_time(m['validUntil'])})" if m.get("validUntil") else " (old value)"
        elif status == "candidate":
            mark = " (unconfirmed)"
        lines.append(f"[{fmt_time(m.get('effectiveAt'))}] {m.get('text', '')}{mark}")
    turns = resp.get("recentTurns") or []
    if turns:
        lines.append("Recent conversation turns:")
        for t in sorted(turns, key=lambda t: _time_key(t.get("timestamp"))):
            lines.append(f"[{fmt_time(t.get('timestamp'))}] {t.get('digest', '')}")
    info = {
        "n_profiles": len(profiles), "n_memories": len(mems), "n_turns": len(turns),
        "memory_ids": [m.get("id") for m in mems], "memory_types": [m.get("type") for m in mems],
        "memory_status": [m.get("status") for m in mems], "scores": [m.get("_score") for m in mems],
        "turn_ids": [t.get("turnId") for t in turns], "gap": resp.get("gap"),
        "retrieval_id": resp.get("retrievalId"), "daemon_tokens_used": resp.get("tokensUsed"),
    }
    return "\n".join(lines), info


# ----------------------------------------------------------------------------- stages

class Runner:
    def __init__(self, args, daemon: Daemon, poller: UsagePoller, data: list[dict]):
        self.a, self.d, self.poller, self.data = args, daemon, poller, data
        self.root = Path(args.results_dir) / args.tag
        self.progress = self.root / "progress.jsonl"
        self.root.mkdir(parents=True, exist_ok=True)
        guard(self.root / "protocol.json", {"sessions": args.sessions, "top_k": args.top_k,
              "token_budget": args.token_budget, "memory_order": args.memory_order,
              "retention_days": args.retention_days, "token_counter": TOKEN_COUNTER,
              "prompt_variant": getattr(args, "actual_prompt_variant", getattr(args, "prompt_variant", None)),
              "tenant_mode": "variant-suffix" if getattr(args, "prompt_variant", None) else "legacy",
              "as_of_mode": "last-ingested-session-plus-one-day",
              "model_runtime": getattr(args, "actual_model_runtime", None)})

    def tenant_for(self, conv: dict) -> str:
        sid = conv["sample_id"]
        variant = getattr(self.a, "prompt_variant", None)
        return f"{sid}-{variant}" if variant else sid

    def validate_prompt_health(self, health: dict) -> None:
        # Historical control daemons may still report the original current algorithm label.
        if health.get("memoryStrategy", "memos") not in ("memos", "current"):
            raise DaemonError("legacy evaluation requires MEMORY_STRATEGY=memos")
        if getattr(self.a, "actual_model_runtime", None) is not None:
            try:
                model_metadata.assert_same(self.a.actual_model_runtime, health)
            except ValueError as error:
                raise DaemonError(str(error)) from error
        variant = getattr(self.a, "actual_prompt_variant", None)
        if not getattr(self.a, "prompt_variant", None) and health.get("promptVariant") != variant:
            raise DaemonError("daemon default prompt changed during this run")
        tenant_variants = health.get("tenantPromptVariants")
        if getattr(self.a, "prompt_variant", None) and not isinstance(tenant_variants, dict):
            raise DaemonError("daemon lacks tenantPromptVariants; variant-suffix routing cannot be verified")
        for conv in self.data:
            tenant = self.tenant_for(conv)
            if isinstance(tenant_variants, dict) and tenant in tenant_variants and tenant_variants[tenant] != variant:
                raise DaemonError(f"tenant {tenant} uses {tenant_variants[tenant]!r}, expected {variant!r}")

    def _done(self) -> set:
        return {(r["conv"], r["stage"], r.get("session")) for r in jsonl.read(self.progress) if r.get("ok")}

    def _mark(self, conv: str, stage: str, session: Optional[str] = None, **extra) -> None:
        jsonl.append(self.progress, {"ts": datetime.now(timezone.utc).isoformat(timespec="seconds"),
                                     "conv": conv, "stage": stage, "session": session, "ok": True, **extra})

    def selected_qas(self, conv: dict) -> list[locomo.QA]:
        qs = locomo.qas(conv)
        if self.a.qids:
            qs = [q for q in qs if q.qid in set(self.a.qids)]
        if self.a.limit:
            qs = qs[: self.a.limit]
        return qs

    def as_of(self, conv: dict) -> str:
        selected = locomo.sessions(conv)[: self.a.sessions or None]
        if not selected:
            raise DaemonError(f"{conv['sample_id']}: no sessions selected")
        from datetime import timedelta
        return locomo.rfc3339(selected[-1].start + timedelta(days=1))

    async def wait_idle(self, tenant: str) -> dict:
        deadline = time.monotonic() + self.a.wait_timeout
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise DaemonError(f"{tenant}: wait_idle returned 'timeout'; overall wait deadline reached")
            idle = await self.d.post("/admin/wait_idle", tenant, {}, timeout=remaining)
            if idle.get("status") != "timeout":
                break
            log.info("%s: daemon wait window elapsed; pending work remains, continuing within overall deadline", tenant)
            await asyncio.sleep(min(0.1, max(0, deadline - time.monotonic())))
        if idle.get("status") != "idle":
            raise DaemonError(f"{tenant}: wait_idle returned {idle.get('status')!r}")
        for key in ("failedTurns", "failedRequests", "failedExperiences", "pendingTurns", "pendingJobs", "indexTasks"):
            value = idle.get(key)
            if not isinstance(value, int) or isinstance(value, bool) or value != 0:
                raise DaemonError(f"{tenant}: wait_idle {key}={value!r}; ingestion is not complete")
        return idle

    async def ensure_empty_buffer(self, conv: dict) -> None:
        stats = await self.d.post("/admin/stats", self.tenant_for(conv), {}, retries=1)
        remaining = (stats.get("counts") or {}).get("turns")
        if remaining != 0 or isinstance(remaining, bool):
            raise DaemonError(f"{conv['sample_id']}: main requires remainingTurns=0, found {remaining!r}")

    # -- ingest
    async def ingest_conv(self, conv: dict, sem: asyncio.Semaphore) -> None:
        sid = conv["sample_id"]
        tenant = self.tenant_for(conv)
        async with sem:
            done = self._done()
            for s in locomo.sessions(conv)[: self.a.sessions or None]:
                if (sid, "session", s.key) in done:
                    continue
                if getattr(self.a, "actual_model_runtime", None) is not None:
                    self.validate_prompt_health(await self.d.health())
                scope = {"project": sid, "session": s.key}
                t0 = time.perf_counter()
                dup = 0
                ts = locomo.turns(s)
                for t in ts:
                    body = {
                        "scope": scope, "turnId": t.turn_id,
                        "messages": [
                            {"role": "user", "content": t.user.content, "timestamp": locomo.rfc3339(t.user.timestamp)},
                            {"role": "assistant", "content": t.assistant.content,
                             "timestamp": locomo.rfc3339(t.assistant.timestamp)},
                        ],
                    }
                    r = await self.d.post("/v1/add_memories", tenant, body)
                    dup += 1 if isinstance(r, dict) and r.get("duplicate") else 0
                await self.d.post("/v1/add_memories", tenant, {"sessionEnded": True, "scope": scope})
                idle = await self.wait_idle(tenant)
                self._mark(sid, "session", s.key, turns=len(ts), duplicates=dup,
                           wait_idle=idle,
                           seconds=round(time.perf_counter() - t0, 1))
                log.info("%s %s: %d turns ingested (%d duplicates) in %.0fs", sid, s.key, len(ts), dup,
                         time.perf_counter() - t0)

    async def stage_ingest(self):
        await self.poller.set_stage("ingest")
        sem = asyncio.Semaphore(self.a.conv_concurrency)
        await asyncio.gather(*(self.ingest_conv(c, sem) for c in self.data))

    # -- jobs / purge
    async def run_jobs(self, stage: str, jobs: list[str], extra: dict):
        await self.poller.set_stage(stage)
        sem = asyncio.Semaphore(self.a.conv_concurrency)

        async def one(conv):
            sid = conv["sample_id"]
            if (sid, stage, None) in self._done():
                return
            if stage == "purge" and not self.a.force_purge:
                missing = self._missing_contexts(conv, "buffer")
                if missing:
                    raise SystemExit(f"{sid}: refusing to purge, {len(missing)} buffer-setting questions not "
                                     f"retrieved yet (run --stage search-buffer, or --force-purge)")
            async with sem:
                body = {"jobs": jobs, "asOf": self.as_of(conv), **extra}
                t0 = time.perf_counter()
                r = await self.d.post("/admin/run_jobs", self.tenant_for(conv), body, timeout=self.a.wait_timeout)
                if r.get("status") != "completed":
                    raise DaemonError(f"{sid}: run_jobs did not complete")
                await self.wait_idle(self.tenant_for(conv))
                if stage == "purge":
                    remaining = (r.get("jobs", {}).get("purge") or {}).get("remainingTurns")
                    if remaining != 0 or isinstance(remaining, bool):
                        raise DaemonError(f"{sid}: purge remainingTurns={remaining!r}, requires 0")
                    await self.ensure_empty_buffer(conv)
                self._mark(sid, stage, None, request=body, response=r, seconds=round(time.perf_counter() - t0, 1))
                log.info("%s %s done in %.0fs", sid, stage, time.perf_counter() - t0)

        await asyncio.gather(*(one(c) for c in self.data))

    # -- search
    def contexts_path(self, setting: str) -> Path:
        return self.root / setting / "contexts.jsonl"

    def _missing_contexts(self, conv: dict, setting: str) -> list[str]:
        have = jsonl.keys(self.contexts_path(setting))
        return [q.qid for q in self.selected_qas(conv) if q.qid not in have]

    async def stage_search(self, setting: str):
        await self.poller.set_stage(f"search-{setting}")
        out = self.contexts_path(setting)
        have = jsonl.keys(out)
        done = self._done()
        sem = asyncio.Semaphore(self.a.search_concurrency)

        async def one(conv, q: locomo.QA):
            async with sem:
                if getattr(self.a, "actual_model_runtime", None) is not None:
                    self.validate_prompt_health(await self.d.health())
                body = {"query": q.question, "scope": {"project": conv["sample_id"]},
                        "asOf": self.as_of(conv), "touch": False,
                        "topK": self.a.top_k, "tokenBudget": self.a.token_budget}
                t0 = time.perf_counter()
                resp = await self.d.post("/v1/search_memories", self.tenant_for(conv), body)
                ms = (time.perf_counter() - t0) * 1000
            context, info = format_context(resp, self.a.memory_order)
            jsonl.append(out, {
                "system": self.a.tag, "setting": setting, "conv": q.conv, "qid": q.qid, "category": q.category,
                "tenant": self.tenant_for(conv), "prompt_variant": self.a.actual_prompt_variant,
                "model_runtime": getattr(self.a, "actual_model_runtime", None),
                "category_name": q.category_name, "question": q.question, "answer": q.answer,
                "context": context, "context_tokens": count_tokens(context), "token_counter": TOKEN_COUNTER,
                "n_memories": info["n_memories"], "n_turns": info["n_turns"], "search_ms": round(ms, 2),
                **{k: v for k, v in info.items() if k not in ("n_memories", "n_turns")},
            })

        for conv in self.data:
            sid = conv["sample_id"]
            if setting == "buffer" and (sid, "purge", None) in done:
                missing = self._missing_contexts(conv, "buffer")
                if missing:
                    raise DaemonError(f"{sid} already purged: cannot retrieve {len(missing)} buffer-setting questions")
                continue
            if setting == "main" and (sid, "purge", None) not in done:
                raise DaemonError(f"{sid} not purged yet: run --stage purge before main-setting retrieval")
            if setting == "main":
                await self.ensure_empty_buffer(conv)
            todo = [q for q in self.selected_qas(conv) if q.qid not in have]
            await asyncio.gather(*(one(conv, q) for q in todo))
            log.info("%s search-%s: %d questions", sid, setting, len(todo))

    async def save_stats(self):
        for conv in self.data:
            sid = conv["sample_id"]
            try:
                st = await self.d.post("/admin/stats", self.tenant_for(conv), {}, retries=1)
            except Exception as e:
                log.info("stats unavailable for %s: %s", sid, e)
                continue
            p = self.root / "stats" / f"{sid}.json"
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text(json.dumps(st, indent=2, ensure_ascii=False))


async def amain(args) -> None:
    data = locomo.select(locomo.load(), args.convs)
    if args.qids:
        allowed = {q.qid for c in data for q in locomo.qas(c)}
        unknown = set(args.qids) - allowed
        if unknown:
            raise SystemExit(f"unknown or excluded qids: {sorted(unknown)}; use <sample_id>-q<original QA index>")
    for name in ("sessions", "limit"):
        value = getattr(args, name)
        if value is not None and value <= 0:
            raise SystemExit(f"--{name} must be positive")
    daemon = Daemon(args.base_url, timeout=args.timeout)
    poller = None
    started = False
    try:
        health = await daemon.health()
        default_variant = health.get("promptVariant")
        if default_variant not in ("en", "zh"):
            raise DaemonError(f"daemon returned an invalid default prompt variant: {default_variant!r}")
        args.actual_prompt_variant = getattr(args, "prompt_variant", None) or default_variant
        args.actual_model_runtime = model_metadata.capture(health)
        poller = UsagePoller(daemon, args.tag, args.usage_poll,
                             Path(args.results_dir) / args.tag / "usage_checkpoint.json")
        runner = Runner(args, daemon, poller, data)
        poller.tenants = [runner.tenant_for(c) for c in data]
        runner.validate_prompt_health(health)
        (runner.root / "run_config.json").write_text(json.dumps(vars(args), indent=2, default=str))
        model_metadata.snapshot(runner.root / "model-runtime.json", args.actual_model_runtime, args.base_url + "/health")
        stages = STAGES if args.stage == "all" else [args.stage]
        await poller.start()
        started = True
        for st in stages:
            if st == "ingest":
                await runner.stage_ingest()
            elif st == "jobs":
                await runner.run_jobs("jobs", ["profile", "project_notes"], {})
            elif st == "search-buffer":
                await runner.stage_search("buffer")
            elif st == "purge":
                await runner.run_jobs("purge", ["purge"], {"retentionDays": args.retention_days})
            elif st == "search-main":
                await runner.stage_search("main")
            runner.validate_prompt_health(await daemon.health())
        await runner.save_stats()
    finally:
        try:
            if started:
                await poller.stop()
        finally:
            await daemon.aclose()


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--base-url", required=True)
    ap.add_argument("--tag", required=True, help="system name, e.g. memory-en / memory-zh")
    ap.add_argument("--prompt-variant", choices=["en", "zh"], default=None,
                    help="route to <sample_id>-en/-zh tenant; omitted = legacy tenant with daemon default prompt")
    ap.add_argument("--stage", choices=["all", *STAGES], default="all")
    ap.add_argument("--convs", type=lambda s: s.split(","), default=None)
    ap.add_argument("--sessions", type=int, default=None, help="only the first N sessions (testing)")
    ap.add_argument("--limit", type=int, default=None, help="questions per conversation (testing)")
    ap.add_argument("--qids", type=lambda s: s.split(","), default=None)
    ap.add_argument("--conv-concurrency", type=int, default=10)
    ap.add_argument("--search-concurrency", type=int, default=1)
    ap.add_argument("--top-k", type=int, default=10)
    ap.add_argument("--token-budget", type=int, default=2000)
    ap.add_argument("--retention-days", type=int, default=0)
    ap.add_argument("--memory-order", choices=["time", "score"], default="time")
    ap.add_argument("--force-purge", action="store_true")
    ap.add_argument("--timeout", type=float, default=300.0, help="per-request timeout (s)")
    ap.add_argument("--wait-timeout", type=float, default=7200.0, help="timeout for wait_idle / run_jobs (s)")
    ap.add_argument("--usage-poll", type=float, default=60.0, help="seconds between /health usage polls")
    ap.add_argument("--results-dir", default=str(RESULTS_DIR))
    ap.add_argument("-v", "--verbose", action="store_true")
    args = ap.parse_args()
    logging.basicConfig(level=logging.DEBUG if args.verbose else logging.INFO,
                        format="%(asctime)s %(levelname)s %(name)s: %(message)s")
    logging.getLogger("httpx").setLevel(logging.WARNING)
    if Path(args.results_dir).resolve() != RESULTS_DIR.resolve():
        set_ledger_path(Path(args.results_dir) / "usage_ledger.jsonl")
    asyncio.run(amain(args))


if __name__ == "__main__":
    main()
