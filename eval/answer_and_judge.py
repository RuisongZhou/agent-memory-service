#!/usr/bin/env python
"""Answer + judge + metrics for one contexts.jsonl (any system / setting).

Answer: mem0's ANSWER_PROMPT sent as one system message, temperature 0, qwen3.8-flash non-thinking.
  --answer-prompt unified        (default) ANSWER_PROMPT with one "Memories:" block = record["context"]
  --answer-prompt mem0-original  mem0's two-block template (needs speaker_1/2 lists; mem0 records only)
Judge: mem0's ACCURACY_PROMPT (user message, JSON mode) -> label CORRECT/WRONG -> J in {0,1}.
F1 / BLEU-1: mem0's metrics code (common/metrics.py).

Writes next to the contexts file:
  answers-<prompt>.jsonl   one record per question (incremental; reruns skip finished qids)
  summary-<prompt>.json    overall + per category (n, J, F1, BLEU-1, context tokens, search latency
                           mean/p95) and usage per stage from the ledger.

  .venv/bin/python eval/answer_and_judge.py --contexts eval/results/mem0/k30/contexts.jsonl
  .venv/bin/python eval/answer_and_judge.py --contexts eval/results/mem0/k30/contexts.jsonl --answer-prompt mem0-original
  .venv/bin/python eval/answer_and_judge.py --contexts eval/results/memory-en/main/contexts.jsonl --summary-only
"""
from __future__ import annotations

import argparse
import asyncio
import json
import hashlib
import logging
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from common import env  # noqa: E402,F401
from common import jsonl, prompts  # noqa: E402
from common.answers import build_answer_prompt, fingerprint, summary_stats  # noqa: E402
from common.env import RESULTS_DIR  # noqa: E402
from common.llm import LLM, LLMOutputError, FatalAPIError, default_ledger, set_ledger_path  # noqa: E402
from common.locomo import CATEGORY_NAMES, EVAL_CATEGORIES  # noqa: E402
from common.metrics import bleu1, f1_score  # noqa: E402
from common.reporting import scope_for  # noqa: E402

from common import usage as usage_ledger  # noqa: E402

log = logging.getLogger("answer_and_judge")


def components(system: str, setting: str, prompt: str) -> tuple[str, str]:
    suffix = "" if prompt == "unified" else f"/{prompt}"
    return f"answer.{system}/{setting}{suffix}", f"judge.{system}/{setting}{suffix}"


async def process(llm: LLM, rec: dict, args, comp_answer: str, comp_judge: str, out: Path) -> None:
    meta = {"qid": rec["qid"]}
    # Keep paid answers even when judging fails; resuming only repeats the judge.
    pending = out.with_name(f"pending-{args.answer_prompt}.jsonl")
    saved = {r["qid"]: r for r in jsonl.read(pending)}.get(rec["qid"])
    source_hash = fingerprint(rec, args.answer_prompt)
    meta["source_hash"] = source_hash
    if saved and saved.get("source_hash") != source_hash:
        saved = None
    if saved:
        response = saved["response"]
        answer_usage = {k: v for k, v in saved["answer_usage"].items() if k in ("hit", "miss", "out", "finish")}
    else:
        a = await llm.chat([{"role": "system", "content": build_answer_prompt(rec, args.answer_prompt)}],
                           component=comp_answer, max_tokens=args.answer_max_tokens, meta=meta)
        response = a.text
        answer_usage = {"hit": a.hit, "miss": a.miss, "out": a.out, "finish": a.finish_reason}
        jsonl.append(pending, {"qid": rec["qid"], "response": response, "answer_usage": answer_usage,
                              "source_hash": source_hash})
    judge_prompt = prompts.render_judge(rec["question"], rec["answer"], response)
    judge_model = getattr(llm, "model", None)
    judge_key = hashlib.sha256(json.dumps({"prompt": judge_prompt, "model": judge_model,
        "max_tokens": args.judge_max_tokens}, sort_keys=True).encode()).hexdigest()
    cache_path = getattr(args, "judge_cache", None)
    cached = next((r for r in jsonl.read(Path(cache_path)) if r.get("key") == judge_key), None) if cache_path else None
    if cached:
        label = cached["label"]
        judge_usage = {"hit": 0, "miss": 0, "out": 0}
    else:
        j = await llm.chat([{"role": "user", "content": judge_prompt}],
                           component=comp_judge, json_mode=True, max_tokens=args.judge_max_tokens, meta=meta)
        label = j.json.get("label") if isinstance(j.json, dict) else None
        judge_usage = {"hit": j.hit, "miss": j.miss, "out": j.out}
    if label not in ("CORRECT", "WRONG"):
        raise LLMOutputError(f"invalid judge label {label!r} for {rec['qid']}")
    if cache_path and not cached:
        jsonl.append(Path(cache_path), {"key": judge_key, "label": label, "qid": rec["qid"],
            "model": judge_model, "source": str(out), "usage": judge_usage})
    pred, gt = str(response), str(rec["answer"])
    jsonl.append(out, {
        "system": rec.get("system"), "setting": rec.get("setting"), "answer_prompt": args.answer_prompt,
        "source_hash": source_hash,
        "conv": rec["conv"], "qid": rec["qid"], "category": rec["category"],
        "category_name": rec.get("category_name") or CATEGORY_NAMES.get(rec["category"]),
        "question": rec["question"], "answer": rec["answer"], "response": response,
        "judge_label": label, "J": 1 if label == "CORRECT" else 0,
        "f1": f1_score(pred, gt), "bleu1": bleu1(pred, gt),
        "context_tokens": rec.get("context_tokens"), "search_ms": rec.get("search_ms"),
        "n_memories": rec.get("n_memories"), "n_turns": rec.get("n_turns"),
        "answer_usage": answer_usage,
        "judge_usage": judge_usage,
        "judge_raw": None,
        "judge_cache_key": judge_key, "judge_reused_from": cached.get("source") if cached else None,
    })


def summarize(answers_path: Path, contexts: list[dict], system: str, setting: str, prompt: str,
              ledger_path: Path) -> dict:
    rows = {r["qid"]: r for r in jsonl.read(answers_path)}  # last record per qid wins
    context_hashes = {r["qid"]: fingerprint(r, prompt) for r in contexts}
    rows = [r for qid, r in rows.items() if qid in context_hashes
            and r.get("source_hash") == context_hashes[qid] and r.get("judge_label") in ("CORRECT", "WRONG")]
    by_cat = {}
    for c in EVAL_CATEGORIES:
        rs = [r for r in rows if int(r["category"]) == c]
        if rs:
            by_cat[f"{c}-{CATEGORY_NAMES[c]}"] = summary_stats(rs)
    comp_answer, comp_judge = components(system, setting, prompt)
    ledger_rows = usage_ledger.load_rows(ledger_path)
    usage = {
        "ingest": usage_ledger.summarize(ledger_rows, prefix=f"{system}.")["total"],  # mem0: <tag>.add; memory: <tag>.ingest/jobs/purge
        "answer": usage_ledger.summarize(ledger_rows, exact=comp_answer)["total"],
        "judge": usage_ledger.summarize(ledger_rows, exact=comp_judge)["total"],
        "ledger_available": ledger_path.exists(),
    }
    completed_hashes = {r["qid"]: r["source_hash"] for r in rows}
    matched_usage_rows = [r for r in ledger_rows if r.get("component") in (comp_answer, comp_judge)
                         and r.get("qid") in completed_hashes
                         and r.get("source_hash") == completed_hashes[r["qid"]]]
    matched = usage_ledger.summarize(matched_usage_rows)["total"]
    matched_components = {}
    for record in matched_usage_rows:
        matched_components.setdefault(record["qid"], set()).add(record["component"])
    fully_recorded = {qid for qid, component_set in matched_components.items()
                    if component_set == {comp_answer, comp_judge}}
    usage["completed_current_context_requests"] = matched
    usage["n_fully_recorded_completed_questions"] = len(fully_recorded)
    usage["usage_note"] = "system totals include failed, unfinished, retried and old-context requests; matched usage only uses completed current-context questions"
    return {
        "system": system, "setting": setting, "answer_prompt": prompt,
        "n_contexts": len(contexts), "n_answered": len(rows),
        "overall": summary_stats(rows), "by_category": by_cat, "usage": usage,
        "token_counter": contexts[0].get("token_counter") if contexts else None,
        "scope": scope_for(answers_path, contexts, rows),
    }


async def amain(args) -> None:
    ctx_path = Path(args.contexts)
    contexts = list({r["qid"]: r for r in jsonl.read(ctx_path)}.values())
    if not contexts:
        raise SystemExit(f"no records in {ctx_path}")
    system, setting = contexts[0]["system"], contexts[0]["setting"]
    if any(r["system"] != system or r["setting"] != setting for r in contexts):
        raise SystemExit("contexts file mixes different systems or settings")
    out = ctx_path.parent / f"answers-{args.answer_prompt}.jsonl"
    summ = ctx_path.parent / f"summary-{args.answer_prompt}.json"
    comp_answer, comp_judge = components(system, setting, args.answer_prompt)
    ledger_path = Path(args.results_dir) / "usage_ledger.jsonl"

    if not args.summary_only:
        done = {r["qid"]: r for r in jsonl.read(out) if r.get("judge_label") in ("CORRECT", "WRONG")}
        todo = [r for r in contexts if done.get(r["qid"], {}).get("source_hash") != fingerprint(r, args.answer_prompt)]
        if args.qids:
            todo = [r for r in todo if r["qid"] in set(args.qids)]
        if args.limit:
            todo = todo[: args.limit]
        log.info("%s/%s [%s]: %d to answer (%d done)", system, setting, args.answer_prompt, len(todo), len(done))
        llm = LLM(concurrency=args.concurrency)
        failed = 0

        async def guarded(rec):
            nonlocal failed
            try:
                await process(llm, rec, args, comp_answer, comp_judge, out)
            except FatalAPIError:
                raise
            except Exception as e:
                failed += 1
                log.warning("%s failed: %s", rec["qid"], e)

        try:
            # chunked gather keeps memory bounded
            for i in range(0, len(todo), 200):
                await asyncio.gather(*(guarded(r) for r in todo[i:i + 200]))
        finally:
            await llm.aclose()
        log.info("calls this run: %d; failed: %d", default_ledger().calls, failed)

    s = summarize(out, contexts, system, setting, args.answer_prompt, ledger_path)
    summ.write_text(json.dumps(s, indent=2))
    o = s["overall"]
    print(f"{system}/{setting} [{args.answer_prompt}] n={o['n']} J={o['J']} F1={o['F1']} BLEU1={o['BLEU1']} "
          f"ctx={o['context_tokens_mean']} lat_p95={o['search_ms_p95']}ms -> {summ}")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--contexts", required=True)
    ap.add_argument("--answer-prompt", choices=["unified", "mem0-original"], default="unified")
    ap.add_argument("--concurrency", type=int, default=8)
    ap.add_argument("--limit", type=int, default=None)
    ap.add_argument("--qids", type=lambda s: s.split(","), default=None)
    ap.add_argument("--answer-max-tokens", type=int, default=512)
    ap.add_argument("--judge-max-tokens", type=int, default=256)
    ap.add_argument("--judge-cache", type=Path, help="optional shared exact-prompt/model judge cache for fair comparisons")
    ap.add_argument("--summary-only", action="store_true")
    ap.add_argument("--results-dir", default=str(RESULTS_DIR), help="where usage_ledger.jsonl lives")
    ap.add_argument("-v", "--verbose", action="store_true")
    args = ap.parse_args()
    logging.basicConfig(level=logging.DEBUG if args.verbose else logging.INFO,
                        format="%(asctime)s %(levelname)s %(name)s: %(message)s")
    for noisy in ("httpx", "openai"):
        logging.getLogger(noisy).setLevel(logging.WARNING)
    if Path(args.results_dir).resolve() != RESULTS_DIR.resolve():
        set_ledger_path(Path(args.results_dir) / "usage_ledger.jsonl")
    asyncio.run(amain(args))


if __name__ == "__main__":
    main()
