#!/usr/bin/env python3
"""Render the fixed cohort's source/formation/retrieval/answer audit, including successful QAs.

Human interpretations are supplied separately as annotations, never used by retrieval.
All generated source excerpts and raw artifacts belong in the ignored results directory.
"""
import argparse
import json
from pathlib import Path

from common import jsonl, locomo
from common.env import RESULTS_DIR


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--diagnosis", type=Path, required=True)
    parser.add_argument("--fair", type=Path, required=True)
    args = parser.parse_args()
    root = args.diagnosis
    notes = json.loads((root / "analysis-notes.json").read_text())
    manifest = json.loads((args.fair / "manifest.json").read_text())
    conv = locomo.select(locomo.load(), [manifest["conversation"]])[0]
    questions = {q.qid: q for q in locomo.qas(conv)}
    sources = {u["dia_id"]: {"date": s.date_str, **u} for s in locomo.sessions(conv) for u in s.utterances}
    memories = {m["id"]: m for m in json.loads((root / "memories.json").read_text())["memories"]}
    evidence = {x["memory"]["id"]: x["evidence"] for x in json.loads((root / "memory-evidence.json").read_text())}
    traces = {x["query"]: x for x in json.loads((root / "retrieval_log.json").read_text())["records"]}
    historical = RESULTS_DIR / "conv26-schema-full-20261010-b"
    paths = {"原快照": RESULTS_DIR / "memory-integrity-final-20261011/current/retained-snapshot",
        "历史 Mem0": historical / "mem0/k30", **{s: args.fair / s for s in ("baseline", "retrieval", "repaired", "mem0")}}
    answers = {name: {row["qid"]: row for row in jsonl.read(path / "answers-unified.jsonl")} for name, path in paths.items()}
    old_contexts = {row["qid"]: row for row in jsonl.read(paths["原快照"] / "contexts.jsonl")}
    formation = json.loads((historical / "final-extract_log.json").read_text())["records"]
    lines = ["# 32 题逐题证据审计", "", "开发样本：解释这些题并验证修复，不代表独立测试集泛化。完整响应、上下文、形成日志及原始源数据保留在同目录链接的 JSON 文件中。", "",
        "原比较使用清理后的服务快照和完整 Mem0 形成结果，且上下文预算不同；新比较禁止 recentTurns，统一 2,000 tokens。J 是模型评分，不能替代逐项证据完整性检查。", ""]
    rows = []
    for qid in sorted(manifest["qids"], key=lambda x: int(x.rsplit("q", 1)[1])):
        q = questions[qid]
        carriers = {f"session_{e.split(':')[0][1:]}-{(int(e.split(':')[1])+1)//2}" for e in q.evidence}
        trace = traces[q.question]
        candidates = {x["id"]: x for x in trace["candidates"]}
        selected = old_contexts[qid]["memory_ids"]
        relevant = [{"memory": m, "evidence": [e for e in evidence[mid] if e["turnId"] in carriers],
            "candidate": candidates.get(mid), "selected": mid in selected} for mid, m in memories.items()
            if any(e["turnId"] in carriers for e in evidence[mid])]
        logs = [x for x in formation if x.get("stage") == "P1_validation" and set(x.get("sourceTurnIds", [])) & carriers]
        row = {"qid": qid, "question": q.question, "gold": q.answer, "evidence_ids": q.evidence,
            "annotation": notes[qid], "answers": {name: rs.get(qid) for name, rs in answers.items()},
            "source_carrier_memories": relevant, "formation_validation": logs,
            "original_selected": [memories[mid] for mid in selected]}
        rows.append(row)
        lines += [f"## {qid}", "", f"**问题**：{q.question}", f"**金答案**：{q.answer}",
            f"**定位**：{notes[qid]['classification']}。{notes[qid]['analysis']}", "",
            "| 系统 | J | 原始回答 |", "|---|---:|---|"]
        for name, rs in answers.items():
            a = rs.get(qid)
            if a:
                response = a["response"].replace("\n", " ").replace("|", "\\|")
                lines.append(f"| {name} | {a['J']} | {response} |")
        lines += ["", "原始证据及邻近话轮（QA 标注本身可能遗漏实际支持句）：", ""]
        surrounding = []
        for eid in q.evidence:
            session, index = eid.split(":")
            for n in range(max(1, int(index) - 1), int(index) + 3):
                key = f"{session}:{n}"
                if key in sources and key not in surrounding:
                    surrounding.append(key)
        for eid in surrounding:
            source = sources[eid]
            lines.append(f"- {eid}，{source['date']}：{locomo.utterance_text(source)}")
        if not surrounding:
            lines.append("- 该题没有标注具体 evidence；应审计实际支持行为及推断边界。")
        lines += ["", "形成及检索链：", ""]
        for item in relevant:
            m, candidate = item["memory"], item["candidate"]
            rank = f"召回通道={candidate['ranks']}；rr={candidate['rerank']}；passed={candidate['passed']}" if candidate else "未进入召回候选"
            lines.append(f"- {m['id']} / {m['status']} / 返回={item['selected']}：{m['text']}。{rank}。")
        for log in logs:
            lines.append(f"- P1 {', '.join(log['sourceTurnIds'])}：accepted={log['accepted']}；rejected={json.dumps(log['rejected'], ensure_ascii=False)}")
        lines += ["", "原快照实际返回的记忆：", ""]
        lines.extend(f"- {memories[mid]['id']}：{memories[mid]['text']}" for mid in selected)
        lines += ["", f"完整原上下文见 [contexts.jsonl]({paths['原快照'].resolve() / 'contexts.jsonl'})；原始检索响应见 [retrieval.jsonl]({paths['原快照'].resolve() / 'retrieval.jsonl'})。", ""]
    (root / "analysis.json").write_text(json.dumps(rows, ensure_ascii=False, indent=2))
    (root / "analysis.md").write_text("\n".join(lines))
    print(f"audited {len(rows)} questions -> {root / 'analysis.md'}")


if __name__ == "__main__":
    main()
