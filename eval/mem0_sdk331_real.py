#!/usr/bin/env python3
"""Explicit paid small LoCoMo comparison of the service and Node SDK 3.3.1.

Never invoked by CI. Protocol preparation is offline; run stages require user authorization.
"""
from __future__ import annotations
import argparse
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import asdict
import hashlib
import json
import os
from pathlib import Path
import re
import statistics
import time

from common import env, locomo, prompts
from common.context_format import pack_blocks
from common.mem0_adapter import mem0_custom_instructions
from common.metrics import count_tokens, f1_score, percentile
from common.sdk331_live import Gateway, Service, SDK, save, append, rows


def digest(path): return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def prepare(root, sdk_root):
    if (root / 'protocol.json').exists(): return json.loads((root / 'protocol.json').read_text())
    root.mkdir(parents=True, exist_ok=False)
    conv = locomo.select(locomo.load(), ['conv-26'])[0]
    sessions = locomo.sessions(conv)[:4]
    supported = [q for q in locomo.qas(conv) if q.evidence and all(
        re.fullmatch(r'D(\d+):\d+', str(e)) and int(re.fullmatch(r'D(\d+):\d+', str(e)).group(1)) <= 4 for e in q.evidence)]
    assert len(supported) == 35
    names = locomo.speakers(conv)
    speakers = [{'id': f'conv-26-{letter}-en', 'name': name} for letter, name in zip('ab', names)]
    batches = []
    source = []
    for session in sessions:
        dates = locomo.utterance_times(session)
        source.extend({'id': u['dia_id'], 'date': session.start.date().isoformat(), 'text': locomo.utterance_text(u)}
                      for u in session.utterances)
        for offset in range(0, len(session.utterances), 8):
            for speaker in speakers:
                messages = [{'role': 'user' if u['speaker'] == speaker['name'] else 'assistant',
                             'content': locomo.utterance_text(u), 'timestamp': locomo.rfc3339(dates[offset + j])}
                            for j, u in enumerate(session.utterances[offset:offset + 8])]
                batches.append({'id': f"{speaker['id']}-{session.key}-{offset}", 'speaker': speaker['id'],
                                'session': session.key, 'date': session.start.date().isoformat(), 'messages': messages})
    binary = env.ROOT_DIR / 'target/fast/agent-memoryd'
    code = [Path(__file__), Path(__file__).with_suffix('.cjs'), Path(__file__).parent / 'common/sdk331_live.py',
            Path(__file__).parent / 'common/context_format.py', Path(__file__).parent / 'common/prompts.py']
    protocol = {'name': 'explicit-real-sdk331-pilot', 'model': os.environ.get('LLM_MODEL', 'qwen3.8-flash'),
                'sdk': 'mem0ai@3.3.1', 'sdkLockSha256': digest(sdk_root / 'package-lock.json'),
                'customInstructions': mem0_custom_instructions(), 'binary': str(binary), 'binarySha256': digest(binary),
                'codeHashes': {str(p.resolve()): digest(p) for p in code}, 'datasetSha256': digest(env.DATA_PATH),
                'speakers': speakers, 'batches': batches, 'questions': [asdict(q) for q in supported],
                'source': source, 'sessions': 4, 'utterances': len(source), 'passes': 3,
                'contextCap': 2000, 'topKPerPerspective': 30, 'semanticThreshold': .1, 'rerank': False,
                'temperature': 0, 'enableThinking': False, 'extractMaxTokens': 8192, 'answerMaxTokens': 1024,
                'maxProviderCalls': 320, 'embedding': 'real BGE-M3 Q8_0 via common daemon, 512-token input limit',
                'nlp': 'official built-in fallback without natural/compromise',
                'dateAdapter': 'gateway replaces only Observation Date with original session date in both arms',
                'scopeMapping': 'service tenant=perspective/project=session; SDK user_id=perspective/run_id=session; search across sessions',
                'contextPacking': 'same score/text stable sort, exact text dedup, whole records, cl100k_base',
                'ingestOrder': 'alternate arm first by batch index; each perspective remains chronological',
                'searchTiming': 'fresh daemon cache per arm; three sequential passes; includes HTTP/IPC, excludes startup/export',
                'quality': 'LLM-judged source support, QA fact coverage, retrieved-context support, unified answer J/F1',
                'noTuning': True, 'developmentCohort': True}
    save(root / 'protocol.json', protocol)
    print(json.dumps({'stage': 'prepared', 'utterances': len(source), 'questions': len(supported),
                      'batchesPerArm': len(batches), 'model': protocol['model']}, ensure_ascii=False), flush=True)
    return protocol


def export_corpus(root, protocol, service, sdk):
    corpus = {'service': {}, 'sdk': {}}
    for person in protocol['speakers']:
        who = person['id']
        corpus['service'][who] = service.post('/admin/export', {}, who)['memories']
        corpus['sdk'][who] = sdk.call('export', who)['result']['memories']['results']
    save(root / 'corpus.json', corpus)
    return corpus


def ingest(root, protocol, service, sdk, gateway):
    done = {(r['arm'], r['batch']) for r in rows(root / 'ingestion.jsonl')}
    for i, batch in enumerate(protocol['batches']):
        for arm in (['service', 'sdk'] if i % 2 == 0 else ['sdk', 'service']):
            if (arm, batch['id']) in done: continue
            gateway.meta = {'batch': batch['id'], 'date': batch['date']}
            start = time.perf_counter()
            if arm == 'service':
                result = service.post('/v1/add_memories', {'scope': {'project': batch['session'], 'session': batch['session']},
                    'turnId': batch['id'], 'messages': batch['messages'], 'sync': True}, batch['speaker'])
                if result.get('status') != 'completed' or not result.get('searchable'):
                    raise RuntimeError(f'formation unfinished: {result}')
            else:
                result = sdk.call('add', batch['speaker'], session=batch['session'],
                                  messages=[{'role': m['role'], 'content': m['content']} for m in batch['messages']])
            append(root / 'ingestion.jsonl', {'arm': arm, 'batch': batch['id'], 'elapsedMs': (time.perf_counter() - start) * 1000,
                                             'result': result})
            print(f"ingest {i+1}/{len(protocol['batches'])} {arm} complete", flush=True)
    for person in protocol['speakers']:
        idle = service.post('/admin/wait_idle', {}, person['id'])
        if idle.get('status') != 'idle': raise RuntimeError(str(idle))
    export_corpus(root, protocol, service, sdk)


def corpus_texts(corpus, arm):
    return [{'id': f'{who}:{m["id"]}', 'text': m['text'] if arm == 'service' else m['memory']}
            for who, items in corpus[arm].items() for m in items]


def search(root, protocol, service, sdk, gateway):
    gateway.formation_enabled = False
    old = rows(root / 'retrieval.jsonl')
    done = {(r['arm'], r['pass'], r['qid']) for r in old}
    before = export_corpus(root, protocol, service, sdk)
    for arm in ['service', 'sdk']:
        service.stop(); service.start(generation=False)
        # Initialize tenant caches outside the client timing boundary.
        for person in protocol['speakers']:
            service.post('/admin/export', {}, person['id'])
            sdk.call('export', person['id'])
        for pass_id in range(3):
            for q in protocol['questions']:
                if (arm, pass_id, q['qid']) in done: continue
                start = time.perf_counter(); outputs = []; blocks = []
                for person in protocol['speakers']:
                    if arm == 'service':
                        result = service.post('/v1/search_memories', {'scope': {'project': '*'}, 'query': q['question'],
                            'topK': 30, 'tokenBudget': 100000, 'rerank': False, 'touch': False}, person['id'])
                        blocks.extend((r['score'], r['memory']['text']) for r in result['results'])
                    else:
                        result = sdk.call('search', person['id'], query=q['question'])
                        blocks.extend((r['score'], r['memory']) for r in result['result']['results'])
                    outputs.append(result)
                elapsed = (time.perf_counter() - start) * 1000
                ordered = []; seen = set()
                for score, text in sorted(blocks, key=lambda item: (-item[0], item[1])):
                    if text not in seen: ordered.append(text); seen.add(text)
                context = pack_blocks(ordered, protocol['contextCap'])
                append(root / 'retrieval.jsonl', {'arm': arm, 'pass': pass_id, 'qid': q['qid'], 'elapsedMs': elapsed,
                    'context': context, 'contextTokens': count_tokens(context), 'responses': outputs})
            print(f'search {arm} pass {pass_id+1}/3 complete', flush=True)
    after = export_corpus(root, protocol, service, sdk)
    assert before == after, 'retrieval mutated primary corpus'
    data = rows(root / 'retrieval.jsonl')
    for arm in ['service', 'sdk']:
        known = {m['id'] for values in after[arm].values() for m in values}
        for record in (r for r in data if r['arm'] == arm):
            for response in record['responses']:
                items = response['results'] if arm == 'service' else response['result']['results']
                ids = {item['memory']['id'] if arm == 'service' else item['id'] for item in items}
                assert ids <= known, 'retrieved memory absent from corpus export'
        for q in protocol['questions']:
            contexts = [r['context'] for r in data if r['arm'] == arm and r['qid'] == q['qid']]
            assert len(contexts) == 3 and len(set(contexts)) == 1, 'unstable or incomplete retrieval'
    save(root / 'retrieval-audit.json', {'primaryUnchanged': True, 'threePassContextsStable': True,
                                       'maxContextTokens': max(r['contextTokens'] for r in data)})


def evaluate(root, protocol, gateway):
    corpus = json.loads((root / 'corpus.json').read_text())
    retrieval = {(r['arm'], r['qid']): r for r in rows(root / 'retrieval.jsonl') if r['pass'] == 0}
    pending = {(r['arm'], r['qid']): r for r in rows(root / 'answers.jsonl')}
    finished = {(r['arm'], r['qid']) for r in rows(root / 'judgments.jsonl')}
    def qa_task(arm, q):
        key = (arm, q['qid'])
        if key in finished: return
        if key in pending: answer = pending[key]['answer']
        else:
            answer = gateway.chat([{'role': 'system', 'content': prompts.render_unified(retrieval[key]['context'], q['question'])}],
                                   {'stage': 'answer', 'arm': arm, 'qid': q['qid']})
            append(root / 'answers.jsonl', {'arm': arm, 'qid': q['qid'], 'answer': answer})
        judged = gateway.chat([{'role': 'user', 'content': prompts.render_judge(q['question'], q['answer'], answer)}],
                              {'stage': 'answer-judge', 'arm': arm, 'qid': q['qid']}, json_mode=True)
        if judged.get('label') not in ('CORRECT', 'WRONG'): raise RuntimeError(f'invalid judge {judged}')
        append(root / 'judgments.jsonl', {'arm': arm, 'qid': q['qid'], 'label': judged['label'],
                                          'F1': f1_score(answer, q['answer']), 'answer': answer})
    jobs = [(arm, q) for q in protocol['questions'] for arm in ['service', 'sdk']]
    with ThreadPoolExecutor(max_workers=4) as pool:
        futures = [pool.submit(qa_task, arm, q) for arm, q in jobs]
        for i, future in enumerate(as_completed(futures), 1):
            future.result()
            if i % 10 == 0: print(f'answer/judge {i}/{len(jobs)} complete', flush=True)
    # Blind source-grounded memory support audit; no system label goes into the judge prompt.
    source = '\n'.join(f"[{r['id']}; {r['date']}] {r['text']}" for r in protocol['source'])
    for arm in ['service', 'sdk']:
        memories = corpus_texts(corpus, arm)
        for offset in range(0, len(memories), 12):
            file = root / 'support' / f'{arm}-{offset}.json'
            if file.exists(): continue
            group = memories[offset:offset+12]
            prompt = ('Audit memory fidelity against the SOURCE only. For each memory, classify SUPPORTED if every material claim is '
                      'stated or unambiguously entailed by the source; CONTRADICTED if source disagrees; otherwise UNSUPPORTED. '
                      'Allow faithful paraphrase and correctly resolved relative dates. Do not infer missing identity/date details. '
                      'Return JSON {"items":[{"id":"exact id","label":"SUPPORTED|CONTRADICTED|UNSUPPORTED","reason":"brief"}]}.\n'
                      + 'SOURCE:\n' + source + '\nMEMORIES:\n' + json.dumps(group))
            result = gateway.chat([{'role': 'user', 'content': prompt}], {'stage': 'memory-support', 'arm': arm, 'offset': offset}, True, 4096)
            assert {r['id'] for r in result['items']} == {r['id'] for r in group}
            assert all(r['label'] in ('SUPPORTED', 'CONTRADICTED', 'UNSUPPORTED') for r in result['items'])
            save(file, result)
        for offset in range(0, 35, 7):
            file = root / 'coverage' / f'{arm}-{offset}.json'
            if file.exists(): continue
            group = [{'qid': q['qid'], 'question': q['question'], 'reference': q['answer'],
                      'retrieved': retrieval[arm, q['qid']]['context']} for q in protocol['questions'][offset:offset+7]]
            prompt = ('For each question, independently decide whether ALL_MEMORIES contains sufficient explicit facts to derive the '
                      'REFERENCE answer, and whether RETRIEVED contains sufficient facts. Use only the supplied memory text, not world '
                      'knowledge or the reference itself as evidence. Partial answers do not count. Return JSON '
                      '{"items":[{"qid":"exact qid","storedSupported":true,"retrievedSupported":true,"reason":"brief"}]}.\n'
                      + 'ALL_MEMORIES:\n' + json.dumps([m['text'] for m in memories]) + '\nQUESTIONS:\n' + json.dumps(group))
            result = gateway.chat([{'role': 'user', 'content': prompt}], {'stage': 'coverage', 'arm': arm, 'offset': offset}, True, 4096)
            assert {r['qid'] for r in result['items']} == {r['qid'] for r in group}
            assert all(isinstance(r.get(k), bool) for r in result['items'] for k in ('storedSupported', 'retrievedSupported'))
            save(file, result)
        print(f'memory support / coverage {arm} complete', flush=True)


def report(root, protocol):
    corpus = json.loads((root / 'corpus.json').read_text())
    retrieval, ingestion, judgments = rows(root / 'retrieval.jsonl'), rows(root / 'ingestion.jsonl'), rows(root / 'judgments.jsonl')
    outputs = {}
    for arm in ['service', 'sdk']:
        memories = corpus_texts(corpus, arm)
        support = [r for f in sorted((root / 'support').glob(arm+'-*.json')) for r in json.loads(f.read_text())['items']]
        coverage = [r for f in sorted((root / 'coverage').glob(arm+'-*.json')) for r in json.loads(f.read_text())['items']]
        answers = [r for r in judgments if r['arm'] == arm]
        assert len(support) == len(memories) and len(coverage) == len(answers) == 35
        ing = [r['elapsedMs'] for r in ingestion if r['arm'] == arm]
        assert len(ing) == len(protocol['batches'])
        cold = [r['elapsedMs'] for r in retrieval if r['arm'] == arm and r['pass'] == 0]
        warm = [r['elapsedMs'] for r in retrieval if r['arm'] == arm and r['pass'] > 0]
        assert len(cold)==35 and len(warm)==70
        outputs[arm] = {'memoryCount': len(memories), 'supportedMemories': sum(r['label']=='SUPPORTED' for r in support),
            'contradictedMemories': sum(r['label']=='CONTRADICTED' for r in support),
            'unsupportedMemories': sum(r['label']=='UNSUPPORTED' for r in support),
            'storedAnswerCoverage': sum(r['storedSupported'] for r in coverage),
            'retrievedAnswerCoverage': sum(r['retrievedSupported'] for r in coverage),
            'answerCorrect': sum(r['label']=='CORRECT' for r in answers), 'questions':35,
            'F1': statistics.mean(r['F1'] for r in answers), 'saveSumSeconds': sum(ing)/1000,
            'saveBatchMeanMs': statistics.mean(ing), 'saveBatchP95Ms': percentile(ing,95),
            'firstSearchMeanMs': statistics.mean(cold), 'firstSearchP95Ms': percentile(cold,95),
            'warmSearchMeanMs':statistics.mean(warm), 'warmSearchP95Ms':percentile(warm,95),
            'meanContextTokens': statistics.mean(r['contextTokens'] for r in retrieval if r['arm']==arm and r['pass']==0)}
    provider = [json.loads(f.read_text()) for f in (root/'provider').glob('*.json')]
    usage = {}
    for record in provider:
        key = record['meta'].get('arm','?')+'.'+record['meta']['stage']
        entry = usage.setdefault(key,{'attempts':0,'promptTokens':0,'completionTokens':0,'latencyMs':0})
        entry['attempts']+=1; entry['latencyMs']+=record.get('latencyMs',0)
        body = json.loads(record.get('rawBody','{}'))
        tokens = body.get('usage',{})
        entry['promptTokens']+=tokens.get('prompt_tokens',0);entry['completionTokens']+=tokens.get('completion_tokens',0)
    summary = {'status':'complete','protocol':'protocol.json','metrics':outputs,'usage':usage,'realProviderCalls':len(provider),
               'contextCap':2000,'allModelsReal':True,'judgeModel':protocol['model'],'developmentCohort':True}
    save(root/'summary.json',summary)
    print(json.dumps(summary,ensure_ascii=False,indent=2),flush=True)


def main():
    ap=argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--root',type=Path,required=True);ap.add_argument('--sdk-root',type=Path,required=True)
    ap.add_argument('--stage',choices=['prepare','ingest','search','evaluate','report','all'],default='prepare')
    args=ap.parse_args();root=args.root.resolve();protocol=prepare(root,args.sdk_root)
    if args.stage=='prepare': return
    assert digest(protocol['binary'])==protocol['binarySha256'],'binary changed after protocol freeze'
    for file,expected in protocol['codeHashes'].items(): assert digest(file)==expected,f'source changed: {file}'
    if args.stage=='report': report(root,protocol);return
    gateway=Gateway(root,protocol);service=None;sdk=None
    try:
        if args.stage in ('all','ingest','search'):
            service=Service(root,protocol,gateway);health=service.start(generation=args.stage!='search')
            save(root/'health.json',health)
            sdk=SDK(root,args.sdk_root,service,gateway)
            if args.stage in ('all','ingest'):ingest(root,protocol,service,sdk,gateway)
            if args.stage in ('all','search'):search(root,protocol,service,sdk,gateway)
        if args.stage in ('all','evaluate'):evaluate(root,protocol,gateway);report(root,protocol)
    finally:
        if sdk:sdk.close()
        if service:service.close()
        gateway.close()


if __name__=='__main__':main()
