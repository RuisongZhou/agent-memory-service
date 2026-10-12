"""Render retrieval results into answer contexts and pack them under a token budget."""
from __future__ import annotations

from typing import Optional

from .metrics import count_tokens


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


def pack_blocks(blocks: list[str], budget: int) -> str:
    """Keep whole records in ranking order; count delimiters in the shared tokenizer."""
    kept = []
    for block in blocks:
        candidate = "\n\n".join([*kept, block])
        if count_tokens(candidate) <= budget:
            kept.append(block)
    return "\n\n".join(kept)
