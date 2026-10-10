#!/usr/bin/env python3
"""Summarize a complete conv-26 cohort, preserving the physical-main retention failure."""
import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--results-dir',type=Path,required=True)
    args=parser.parse_args();root=args.results_dir.resolve()
    summaries={key:json.loads((root/path/'summary-unified.json').read_text()) for key,path in [
        ('service_longterm','memory-en/longterm-only'),('service_buffer','memory-en/buffer'),('mem0','mem0/k30')]}
    for value in summaries.values():
        assert value['scope']['verified'] and value['scope']['retrieval_complete'] and value['scope']['answering_complete']
        assert value['n_answered']==152
    assert len({s['scope']['qid_cohort_hash'] for s in summaries.values()})==1
    progress=[json.loads(x) for x in (root/'memory-en/progress.jsonl').read_text().splitlines()]
    sessions=[r for r in progress if r['stage']=='session']
    assert len(sessions)==19 and sum(r['turns'] for r in sessions)==214
    assert all(r['wait_idle']['failedTurns']==r['wait_idle']['failedRequests']==r['wait_idle']['failedExperiences']==0 for r in sessions)
    responses=json.loads((root/'final-llm_responses.json').read_text())['records']
    usage=json.loads((root/'final-llm_usage.json').read_text())['records']
    assert len(responses)==len(usage)
    assert {r['mid'] for r in responses}=={r['responseLogId'] for r in usage}
    assert all(json.loads(r['rawBody'])['choices'][0]['message']['content']==r['rawContent'] for r in responses)
    joint=[r for r in responses if r['stage']=='P1+P2']
    assert len(joint)==75 and all(r['validOutputShape'] and r['attempt']==1 for r in joint)
    stats=json.loads((root/'final-stats.json').read_text())
    context=[json.loads(x) for x in (root/'memory-en/longterm-only/contexts.jsonl').read_text().splitlines()]
    assert all(r['n_turns']==0 and not r['turn_ids'] and 'Recent conversation turns:' not in r['context'] for r in context)
    protocol=json.loads((root/'comparison-protocol.json').read_text())
    baseline=Path(protocol['baseline'])
    assert hashlib.sha256((root/'mem0/k30/summary-unified.json').read_bytes()).hexdigest()==protocol['baseline_summary_sha256']
    ingestion_seconds=sum(r['seconds'] for r in sessions)
    jobs_seconds=sum(r.get('seconds',0) for r in progress if r['stage']=='jobs')
    a=summaries['service_longterm'];b=summaries['service_buffer'];m=summaries['mem0']
    deltas={'judge_percentage_points':round((a['overall']['J']-m['overall']['J'])*100,2),
            'context_token_reduction_percent':round((1-a['overall']['context_tokens_mean']/m['overall']['context_tokens_mean'])*100,2),
            'ingestion_call_reduction_percent':round((1-a['usage']['ingest']['calls']/m['usage']['ingest']['calls'])*100,2)}
    audit={'complete_sessions':19,'paired_turns':214,'joint_batches':len(joint),'joint_first_attempt_success_percent':100,
           'all_service_raw_responses':len(responses),'all_service_usage_records':len(usage),
           'all_service_validation_failures':sum(r.get('validOutputShape') is False for r in responses),
           'stage_calls':dict(Counter(r['stage'] for r in responses)), 'failed_turns':0,
           'remaining_source_turns':stats['counts']['turns'],'remaining_source_vectors':stats['index']['turnVectors'],
           'index_tasks':stats['counts']['index_tasks'],'memories':stats['memories']}
    note='Physical-clear main blocked by 53 source turns retained for unfinished tasks. longterm-only is a context ablation excluding recent-turn text; it is not the physical-clear main setting. Its copied search time includes recent-turn retrieval, so no dedicated longterm-only search latency is reported.'
    result={'date':'2026-10-10','conversation':'conv-26','questions':152,'model':'qwen3.8-flash',
            'original_main_complete':False,'complete_context_ablation':True,'method_note':note,
            'audit':audit,'summaries':summaries,'deltas_vs_mem0':deltas,
            'service_ingestion_seconds':round(ingestion_seconds,1),'service_profile_jobs_seconds':jobs_seconds,
            'mem0_baseline_reused':str(baseline),'mem0_baseline_sha256':protocol['baseline_summary_sha256']}
    (root/'comparison-summary.json').write_text(json.dumps(result,ensure_ascii=False,indent=2))
    lines=['conv-26 联合抽取修复与评测汇总（2026-10-10）','',
        '修复：联合抽取使用严格 json_schema；每批限定合法来源轮编号、来源列表长度、1024 字符证据段和最多三条经验；结构化调用取消 max_tokens 上限。解析和结构校验错误携带原返回及具体错误进行纠错重试。每次 HTTP 原始返回在解析前保存到 llm_responses，并与用量、抽取日志关联。','',
        '验证：21 项 Rust 服务测试、Rust 规则/全文/模型测试、94 项 Python 评测测试、12 项模型工具测试及服务 selftest 通过。故障注入确认错误反馈、原返回持久化和重启恢复。最终小样本：首两个会话 18 轮、6 次调用一次成功；长上下文故障重放 11.478 秒、815 输出 token，完整返回。','',
        '完整写入：19 会话、214 轮、75 个联合抽取批次全部首次成功，最终运行无结构校验失败；79 次服务模型返回和79条用量记录逐条匹配。形成296条当前事实、6条被替代事实、2条画像/项目摘要；索引任务为零。',
        f'写入阶段耗时：{ingestion_seconds:.1f} 秒；画像/项目摘要任务：{jobs_seconds:.1f} 秒。','',
        '方法边界：清理仍保留53轮未完成任务来源，因此原物理清空 main 未完成。longterm-only 从已检索上下文移除近期原始对话，保留已检索记忆、画像及其顺序；152题输入均无近期轮次。保留缓冲的结果单列，不混入长期记忆消融。该消融的检索耗时沿用完整 buffer 检索（包含近期轮次检索），未独立测量纯长期检索延迟。','',
        '模型与口径：英文 LoCoMo 配置；Qwen qwen3.8-flash，temperature=0、非思考；真实 Metal BGE-M3/BGE-Reranker。复用本次 Mem0 OSS 2.2.1 结果，向量检索，无 fastembed/spaCy；双方使用同一152题与统一回答/评分提示词。服务 top10/2000 token 预算，Mem0 对两位说话者各 top30。','',
        '质量与上下文（全部152题）：','设置 | 判题准确率 | F1 | BLEU-1 | 平均上下文token | 正确题数']
    for key,label in [('service_longterm','服务 longterm-only（上下文消融）'),('service_buffer','服务 buffer（含近期轮次）'),('mem0','Mem0 k30×2')]:
        s=summaries[key];o=s['overall'];path={'service_longterm':'memory-en/longterm-only','service_buffer':'memory-en/buffer','mem0':'mem0/k30'}[key]
        answers={r['qid']:r for r in map(json.loads,(root/path/'answers-unified.jsonl').read_text().splitlines()) if r.get('judge_label') in ['CORRECT','WRONG']}
        correct=sum(r['judge_label']=='CORRECT' for r in answers.values())
        lines.append(f'{label} | {100*o["J"]:.2f}% | {100*o["F1"]:.2f}% | {100*o["BLEU1"]:.2f}% | {o["context_tokens_mean"]:.1f} | {correct}/152')
    lines+=['','分类判题准确率：','类别 | 题数 | 服务长期记忆消融 | 服务缓冲 | Mem0']
    for category in m['by_category']:
        lines.append(f'{category} | {m["by_category"][category]["n"]} | {100*a["by_category"][category]["J"]:.2f}% | {100*b["by_category"][category]["J"]:.2f}% | {100*m["by_category"][category]["J"]:.2f}%')
    lines+=['','检索性能：',f'- 服务完整检索：均值 {b["overall"]["search_ms_mean"]} ms，P95 {b["overall"]["search_ms_p95"]} ms。',f'- Mem0 两位说话者检索合计：均值 {m["overall"]["search_ms_mean"]} ms，P95 {m["overall"]["search_ms_p95"]} ms。','- 服务 longterm-only 是上下文消融，无独立的纯长期检索耗时。','', '模型用量：','阶段 | 调用次数 | 输入token（含缓存） | 缓存命中token | 输出token']
    for label,v in [('服务写入及画像',a['usage']['ingest']),('Mem0写入',m['usage']['ingest']),('服务长期消融回答',a['usage']['answer']),('服务长期消融评分',a['usage']['judge']),('服务缓冲回答',b['usage']['answer']),('服务缓冲评分',b['usage']['judge']),('Mem0回答',m['usage']['answer']),('Mem0评分',m['usage']['judge'])]:
        lines.append(f'{label} | {v["calls"]} | {v["hit"]+v["miss"]:,} | {v["hit"]:,} | {v["out"]:,}')
    lines+=['',f'观察：长期记忆消融比 Mem0 低 {abs(deltas["judge_percentage_points"]):.2f} 个百分点，平均上下文减少 {deltas["context_token_reduction_percent"]:.2f}%；写入及画像调用减少 {deltas["ingestion_call_reduction_percent"]:.2f}%。服务的写入输出token较多，不能把调用少直接理解为写入更快。分类差距主要在单跳和多跳，时间题差距较小。','',
        '修复过程的首轮全量运行因连续截断而中止，保留在 conv26-schema-full-20261010；两个小样本和故障重放也各有独立目录。它们均不混入最终质量分数或上述最终运行用量。中止时尚有在途请求，未收到的用量无法从本地确认。','',
        '证据：comparison-protocol.json、comparison-summary.json、final-llm_responses.json、final-llm_usage.json、final-extract_log.json、final-stats.json、usage_ledger.jsonl、memory-en/longterm-only/protocol.json、各设置的contexts/answers/summary文件。']
    (root/'汇总结果.txt').write_text('\n'.join(lines)+'\n')
    # Annotate the generic report so copied buffer timing is never presented as a separate measurement.
    report_path=root/'report.json';report=json.loads(report_path.read_text())
    for row in report['rows']:
        if row['setting']=='longterm-only':
            row.update(source_search_ms_mean=row['search_ms_mean'],source_search_ms_p95=row['search_ms_p95'],
                       search_ms_mean=None,search_ms_p95=None,latency_note=note)
    report['comparison_warnings'].append(note)
    report_path.write_text(json.dumps(report,indent=2))
    path=root/'report.md';text=path.read_text();updated=[]
    for line in text.splitlines():
        if line.startswith('| memory-en | longterm-only |'):
            cells=line.split('|');cells[-3]=' – ';cells[-2]=' – ';line='|'.join(cells)
        updated.append(line)
    path.write_text('\n'.join(updated)+'\n\n'+note+'\n')
    print(json.dumps({'audit':audit,'deltas':deltas,'report':str(root/'汇总结果.txt')},ensure_ascii=False))

if __name__=='__main__':main()
