#!/usr/bin/env python3
"""Explicit before/after pilot: captured-response replay or authorized real-model calls.

Never run by CI. Both arms are service binaries, not an additional SDK baseline.
"""
import argparse
from concurrent.futures import ThreadPoolExecutor
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import statistics
import threading
import time
from types import SimpleNamespace

from common import prompts
from common.context_format import pack_blocks
from common.metrics import count_tokens, f1_score, percentile
from common.sdk331_live import Gateway, Service, append, rows, save
from common.sdk331_replay_vectors import FrozenEmbeddings

ARMS = ('before', 'after')


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


class ReplayGateway:
    """Only serves recorded extraction responses locally; has no provider client."""
    def __init__(self, root, original):
        self.root, self.meta, self.index = root, {}, {}
        self.url = ''; self.formation_enabled = True
        self.responses = {}
        for p in sorted((original / 'provider').glob('*.json')):
            item = json.loads(p.read_text())
            if item['meta']['stage'] == 'extract' and item['meta']['arm'] == 'service':
                self.responses.setdefault(item['meta']['batch'], []).append(item)
        owner = self
        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_): pass
            def do_POST(self):
                try:
                    assert owner.formation_enabled
                    arm = self.path.strip('/').split('/')[0]
                    body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                    batch = owner.meta['batch']; key = (arm, batch)
                    index = owner.index.get(key, 0)
                    record = owner.responses[batch][index]
                    owner.index[key] = index + 1
                    append(root / 'local-model-replay.jsonl', {'arm': arm, 'batch': batch, 'attempt': index,
                        'request': body, 'originalProviderId': record['index']})
                    content = record['rawBody'].encode(); status = 200
                except Exception as exc:
                    content = json.dumps({'error': str(exc)}).encode(); status = 500
                self.send_response(status); self.send_header('Content-Length', str(len(content)))
                self.send_header('Content-Type', 'application/json'); self.end_headers(); self.wfile.write(content)
        self.server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        self.url = f'http://127.0.0.1:{self.server.server_port}'
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def close(self):
        self.server.shutdown(); self.server.server_close()


def prepare(args):
    root = args.root.resolve()
    if (root / 'protocol.json').exists():
        protocol = json.loads((root / 'protocol.json').read_text())
        assert protocol['mode'] == args.mode
    else:
        root.mkdir(parents=True, exist_ok=False)
        base = json.loads((args.original / 'protocol.json').read_text())
        keep = ('model', 'customInstructions', 'speakers', 'batches', 'questions', 'source', 'sessions',
                'utterances', 'contextCap', 'topKPerPerspective', 'semanticThreshold', 'rerank', 'temperature',
                'enableThinking', 'extractMaxTokens', 'answerMaxTokens', 'embedding', 'contextPacking', 'dateAdapter',
                'scopeMapping', 'datasetSha256', 'developmentCohort')
        protocol = {k: base[k] for k in keep}
        protocol.update(name='sdk331-service-optimization', mode=args.mode, passes=3, maxProviderCalls=320,
            binaries={arm: str(getattr(args, arm).resolve()) for arm in ARMS},
            binaryHashes={arm: digest(getattr(args, arm)) for arm in ARMS},
            codeHashes={str(p.resolve()): digest(p) for p in (Path(__file__), Path(__file__).parent/'common/sdk331_live.py', Path(__file__).parent/'common/sdk331_replay_vectors.py')},
            original=str(args.original.resolve()),
            ingestOrder='alternate before/after first by batch; chronological per perspective',
            searchTiming='fresh model/cache process per arm; 3 sequential passes; two-perspective HTTP; startup/export and common packing excluded',
            judgePolicy='same full requests share cached answer/judge result; no independent conflicting labels for identical answer',
            supportAudit='short local item IDs, strict schema, source only, model identity blinded',
            replayControls='reuse real embedding vectors per exact input; map generated memory IDs by text/scope; live bypasses shared vector cache',
            corrections='retain all 35 questions; q5/q94 posthoc exclusion only sensitivity', noTuning=True)
        save(root / 'protocol.json', protocol)
    for arm in ARMS:
        assert digest(protocol['binaries'][arm]) == protocol['binaryHashes'][arm], 'binary changed'
    for name, sha in protocol['codeHashes'].items(): assert digest(name) == sha, 'source changed'
    return root, protocol


def make_service(root, protocol, gateway, arm):
    folder = root / arm; folder.mkdir(exist_ok=True)
    return Service(folder, dict(protocol, binary=protocol['binaries'][arm]),
                   SimpleNamespace(url=gateway.url + '/' + arm))


def ingest(root, protocol, gateway):
    services = {arm: make_service(root, protocol, gateway, arm) for arm in ARMS}
    done = {(r['arm'], r['batch']) for r in rows(root/'ingestion.jsonl')}
    try:
        for arm, svc in services.items(): save(root/(arm+'-health.json'), svc.start(generation=True))
        for index, batch in enumerate(protocol['batches']):
            gateway.meta = {'batch': batch['id'], 'date': batch['date']}
            for arm in (ARMS if index % 2 == 0 else ARMS[::-1]):
                if (arm, batch['id']) in done: continue
                start = time.perf_counter()
                result = services[arm].post('/v1/add_memories', {'scope': {'project': batch['session'], 'session': batch['session']},
                    'turnId': batch['id'], 'messages': batch['messages'], 'sync': True}, batch['speaker'])
                assert result.get('status') == 'completed' and result.get('searchable'), result
                append(root/'ingestion.jsonl', {'arm': arm, 'batch': batch['id'], 'elapsedMs': (time.perf_counter()-start)*1000, 'result': result})
            print('ingest', index+1, '/', len(protocol['batches']), flush=True)
        corpus = {}
        for arm, svc in services.items():
            corpus[arm] = {}
            for person in protocol['speakers']:
                assert svc.post('/admin/wait_idle', {}, person['id'])['status'] == 'idle'
                corpus[arm][person['id']] = svc.post('/admin/export', {}, person['id'])['memories']
        save(root/'corpus.json', corpus)
    finally:
        for svc in services.values(): svc.close()


def search(root, protocol, gateway):
    gateway.formation_enabled = False
    corpus = json.loads((root/'corpus.json').read_text())
    done = {(r['arm'], r['pass'], r['qid']) for r in rows(root/'retrieval.jsonl')}
    for arm in ARMS:
        svc = make_service(root, protocol, gateway, arm)
        try:
            svc.start(generation=False)
            for person in protocol['speakers']: svc.post('/admin/export', {}, person['id'])
            for pass_id in range(protocol['passes']):
                for q in protocol['questions']:
                    if (arm, pass_id, q['qid']) in done: continue
                    responses = []; blocks = []; start = time.perf_counter()
                    for person in protocol['speakers']:
                        response = svc.post('/v1/search_memories', {'scope': {'project': '*'}, 'query': q['question'],
                            'topK':30, 'tokenBudget':100000, 'rerank':False, 'touch':False}, person['id'])
                        responses.append(response)
                        blocks.extend((r['score'], r['memory']['text']) for r in response['results'])
                        known = {m['id'] for m in corpus[arm][person['id']]}
                        assert all(r['memory']['id'] in known for r in response['results'])
                    elapsed = (time.perf_counter()-start)*1000
                    ordered = []; seen = set()
                    for score, text in sorted(blocks, key=lambda item: (-item[0], item[1])):
                        if text not in seen: ordered.append(text); seen.add(text)
                    context = pack_blocks(ordered, protocol['contextCap'])
                    append(root/'retrieval.jsonl', {'arm':arm, 'pass':pass_id, 'qid':q['qid'], 'elapsedMs':elapsed,
                        'context':context, 'contextTokens':count_tokens(context), 'responses':responses})
                print('search', arm, pass_id+1, '/', protocol['passes'], flush=True)
        finally: svc.close()
    data = rows(root/'retrieval.jsonl')
    for arm in ARMS:
        for q in protocol['questions']:
            items = [r for r in data if r['arm']==arm and r['qid']==q['qid']]
            assert len(items)==3 and len({r['context'] for r in items})==1
    assert max(r['contextTokens'] for r in data) <= protocol['contextCap']
    if protocol['mode']=='replay':
        requests = rows(root/'local-model-replay.jsonl')
        bodies = {arm: {(r['batch'],r['attempt']):r['request'] for r in requests if r['arm']==arm} for arm in ARMS}
        assert bodies['before']==bodies['after'], 'replay extraction request mismatch'
        signatures = {}
        for arm in ARMS:
            signatures[arm] = [(r['qid'],r['pass'],r['context'],
                [sorted((i['memory']['text'],json.dumps(i['memory']['scope'],sort_keys=True),i['score']) for i in response['results']) for response in r['responses']])
                for r in data if r['arm']==arm]
        assert signatures['before']==signatures['after'], 'replay retrieval semantic mismatch'
        for person in protocol['speakers']:
            def facts(arm): return sorted((m['text'],m['status'],json.dumps(m['scope'],sort_keys=True)) for m in corpus[arm][person['id']])
            assert facts('before')==facts('after'), 'replay formed memory mismatch'
        save(root/'semantic-audit.json', {'sameMemoryTextsScopesStatuses':True, 'same105RetrievalLogicalCandidatesScoresContexts':True,
            'sameExtractionRequests':True, 'externalProviderCalls':0, 'same35QAInputs':True})


def evaluate(root, protocol, gateway):
    assert protocol['mode']=='live'
    corpus = json.loads((root/'corpus.json').read_text())
    retrieval = {(r['arm'],r['qid']):r for r in rows(root/'retrieval.jsonl') if r['pass']==0}
    locks = {}; guard = threading.Lock()
    def chat(messages, meta, json_mode=False, max_tokens=1024, validator=None):
        key = hashlib.sha256(json.dumps([messages,json_mode,max_tokens],sort_keys=True).encode()).hexdigest()
        with guard: lock=locks.setdefault(key,threading.Lock())
        with lock:
            path=root/'judge-cache'/(key+'.json')
            if path.exists(): result=json.loads(path.read_text())['result']
            else:
                error=''
                for repair in range(3):
                    actual = messages + ([{'role':'user','content':'Local validation error: '+error}] if error else [])
                    try:
                        result=gateway.chat(actual,dict(meta,repair=repair),json_mode,max_tokens)
                        if validator: validator(result)
                        break
                    except (AssertionError, KeyError, TypeError, json.JSONDecodeError, RuntimeError) as exc:
                        error=str(exc) or 'Return the exact requested schema and item IDs.'
                        if isinstance(exc,RuntimeError) and 'truncated evaluation' not in error: raise
                else: raise RuntimeError('bounded evaluation validation failed: '+error)
                save(path,{'result':result,'requestHash':key})
            if validator:validator(result)
            return result
    def valid_label(r): assert r.get('label') in ('CORRECT','WRONG'), 'label must be CORRECT or WRONG'
    finished={(r['arm'],r['qid']) for r in rows(root/'judgments.jsonl')}
    def qa(job):
        arm,q=job
        if (arm,q['qid']) in finished:return
        answer=chat([{'role':'system','content':prompts.render_unified(retrieval[arm,q['qid']]['context'],q['question'])}],
            {'stage':'answer','arm':arm,'qid':q['qid']})
        judged=chat([{'role':'user','content':prompts.render_judge(q['question'],q['answer'],answer)}],
            {'stage':'answer-judge','arm':arm,'qid':q['qid']},True,1024,valid_label)
        with guard:append(root/'judgments.jsonl',{'arm':arm,'qid':q['qid'],'answer':answer,'label':judged['label'],'F1':f1_score(answer,q['answer'])})
    with ThreadPoolExecutor(max_workers=4) as pool:
        for index,_ in enumerate(pool.map(qa,[(arm,q) for q in protocol['questions'] for arm in ARMS]),1):
            if index%10==0:print('QA',index,'/70',flush=True)
    source='\n'.join(f"[{r['id']}; {r['date']}] {r['text']}" for r in protocol['source'])
    def validate_items(result, group, coverage):
        items=result['items']; assert len(items)==len(group), 'wrong item count'
        field='qid' if coverage else 'id'
        assert {i[field] for i in items}=={i[field] for i in group}, 'wrong item IDs'
        if coverage:
            assert all(isinstance(i.get(k),bool) for i in items for k in ('storedSupported','retrievedSupported')), 'support flags must be boolean'
            assert all(not i['retrievedSupported'] or i['storedSupported'] for i in items), 'retrieved is a subset of all memories'
        else:assert all(i.get('label') in ('SUPPORTED','CONTRADICTED','UNSUPPORTED') for i in items), 'invalid support label'
    for arm in ARMS:
        memories=[{'key':who+':'+m['id'],'text':m['text']} for who,items in corpus[arm].items() for m in items]
        for offset in range(0,len(memories),12):
            path=root/'support'/f'{arm}-{offset}.json'
            if path.exists():continue
            group=[{'id':str(i),'text':m['text']} for i,m in enumerate(memories[offset:offset+12])]
            prompt=('Audit memory fidelity against the SOURCE only. For each memory, classify SUPPORTED if every material claim is '
                'stated or unambiguously entailed by the source; CONTRADICTED if source disagrees; otherwise UNSUPPORTED. '
                'Allow faithful paraphrase and correctly resolved relative dates. Do not infer missing identity/date details. '
                'Return JSON {"items":[{"id":"exact id","label":"SUPPORTED|CONTRADICTED|UNSUPPORTED","reason":"brief"}]}.\n'
                +'SOURCE:\n'+source+'\nMEMORIES:\n'+json.dumps(group))
            result=chat([{'role':'user','content':prompt}],{'stage':'memory-support','arm':arm,'offset':offset},True,4096,
                lambda result:validate_items(result,group,False))
            save(path,dict(result,idMap={str(i):m['key'] for i,m in enumerate(memories[offset:offset+12])}))
        for offset in range(0,35,7):
            path=root/'coverage'/f'{arm}-{offset}.json'
            if path.exists():continue
            group=[{'qid':q['qid'],'question':q['question'],'reference':q['answer'],'retrieved':retrieval[arm,q['qid']]['context']}
                   for q in protocol['questions'][offset:offset+7]]
            prompt=('For each question, independently decide whether ALL_MEMORIES contains sufficient explicit facts to derive the '
                'REFERENCE answer, and whether RETRIEVED contains sufficient facts. Use only the supplied memory text, not world '
                'knowledge or the reference itself as evidence. Partial answers do not count. Return JSON '
                '{"items":[{"qid":"exact qid","storedSupported":true,"retrievedSupported":true,"reason":"brief"}]}. '
                'RETRIEVED is a verbatim subset of ALL_MEMORIES; retrievedSupported=true implies storedSupported=true.\n'
                +'ALL_MEMORIES:\n'+json.dumps([m['text'] for m in memories])+'\nQUESTIONS:\n'+json.dumps(group))
            result=chat([{'role':'user','content':prompt}],{'stage':'coverage','arm':arm,'offset':offset},True,4096,
                lambda result:validate_items(result,group,True))
            save(path,result)
        print('support/coverage',arm,'complete',flush=True)


def report(root, protocol):
    corpus=json.loads((root/'corpus.json').read_text());ing=rows(root/'ingestion.jsonl');ret=rows(root/'retrieval.jsonl')
    summary={'mode':protocol['mode'],'metrics':{},'providerCalls':len(list((root/'provider').glob('*.json')))}
    for arm in ARMS:
        writes=[r['elapsedMs'] for r in ing if r['arm']==arm];assert len(writes)==24
        cold=[r['elapsedMs'] for r in ret if r['arm']==arm and r['pass']==0]
        warm=[r['elapsedMs'] for r in ret if r['arm']==arm and r['pass']>0]
        assert len(cold)==35 and len(warm)==70
        m={'memoryCount':sum(map(len,corpus[arm].values())), 'saveSumSeconds':sum(writes)/1000,
           'saveBatchMeanMs':statistics.mean(writes),'saveBatchP95Ms':percentile(writes,95),
           'firstSearchMeanMs':statistics.mean(cold),'firstSearchP95Ms':percentile(cold,95),
           'warmSearchMeanMs':statistics.mean(warm),'warmSearchP95Ms':percentile(warm,95)}
        if protocol['mode']=='live':
            labels=[r for r in rows(root/'judgments.jsonl') if r['arm']==arm];assert len(labels)==35
            support=[i for p in (root/'support').glob(arm+'-*.json') for i in json.loads(p.read_text())['items']]
            coverage=[i for p in (root/'coverage').glob(arm+'-*.json') for i in json.loads(p.read_text())['items']]
            assert len(support)==m['memoryCount'] and len(coverage)==35
            m.update(supportedMemories=sum(i['label']=='SUPPORTED' for i in support),
                storedCoverage=sum(i['storedSupported'] for i in coverage),retrievedCoverage=sum(i['retrievedSupported'] for i in coverage),
                answerCorrect=sum(i['label']=='CORRECT' for i in labels),F1=statistics.mean(i['F1'] for i in labels),
                annotationSensitivity={'n':33,'correct':sum(i['label']=='CORRECT' for i in labels if i['qid'] not in ('conv-26-q5','conv-26-q94'))})
        summary['metrics'][arm]=m
    save(root/'summary.json',summary);print(json.dumps(summary,indent=2))


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--root',type=Path,required=True);p.add_argument('--original',type=Path,required=True)
    p.add_argument('--before',type=Path,required=True);p.add_argument('--after',type=Path,required=True)
    p.add_argument('--mode',choices=['live','replay'],required=True)
    p.add_argument('--stage',choices=['prepare','ingest','search','evaluate','report','all'],default='prepare')
    args=p.parse_args();root,protocol=prepare(args)
    if args.stage=='prepare':return
    if args.stage=='report':report(root,protocol);return
    gateway=Gateway(root,protocol) if args.mode=='live' else ReplayGateway(root,args.original)
    embeddings=FrozenEmbeddings(root) if args.mode=='replay' else None
    if embeddings: protocol=dict(protocol,embedBaseUrl=embeddings.url)
    try:
        if args.stage in ('all','ingest'):ingest(root,protocol,gateway)
        if args.stage in ('all','search'):search(root,protocol,gateway)
        if args.mode=='live' and args.stage in ('all','evaluate'):evaluate(root,protocol,gateway)
        if args.stage=='all' or args.stage=='evaluate':report(root,protocol)
    finally:
        gateway.close()
        if embeddings:embeddings.close()


if __name__=='__main__':main()
