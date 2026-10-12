#!/usr/bin/env python3
"""Run the real daemon against a localhost-only deterministic LLM stub. No paid calls.

Exercises HTTP formation, persisted retry usage, profiles, purge, restart and idempotency.
Mock output validates transport/storage only and must never enter the LoCoMo effectiveness report.
"""
import argparse
from datetime import datetime, timedelta, timezone
import json
import os
from pathlib import Path
import re
import select
import socket
import subprocess
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.request import Request, urlopen
from urllib.error import HTTPError


class Stub(BaseHTTPRequestHandler):
    calls = []
    malformed_once = True
    wrong_shape_once = True

    def log_message(self, *_):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        assert body['model'] == 'qwen3.8-flash'
        assert body['enable_thinking'] is False and 'thinking' not in body
        assert body['temperature'] == 0
        system, user = [m['content'] for m in body['messages'][:2]]
        if '<current batch>' in user:
            stage = 'P1'
            facts = []
            batch = user.split('<current batch>')[1].split('</current batch>')[0]
            for turn_id, date, messages in re.findall(r'\[([^|]+) \| ([^\]]+)\]\n([^\[]+)', batch):
                for line in messages.strip().splitlines():
                    if not line.strip():
                        continue
                    facts.append([line, 'global', [line.split(':')[0]], date[:10], 'user',
                                  turn_id.strip(), line, [], None])
            out = {'facts': [{'text':r[0], 'scope':r[1], 'entities':r[2], 'effectiveAt':r[3], 'origin':r[4], 'source':{'turnId':r[5],'span':r[6]}, 'relations':[]} for r in facts]}
        elif '<content to remember' in user:
            stage = 'P1-b'
            text = re.search(r'<content to remember[^>]*>\s*(.*?)\s*</content to remember>', user, re.S).group(1)
            out = {'facts':[{'text':text,'scope':'project','entities':['team'],
                'effectiveAt':datetime.now(timezone.utc).date().isoformat(),'origin':'explicit',
                'source':{'turnId':'text','span':text},'relations':[]}]}
        elif 'experience extractor' in system:
            stage = 'P2'
            out = {'analysis': [], 'experiences': [], 'summary': {
                'goal': 'offline integration', 'outcome': 'completed', 'decisions': [], 'openIssues': []}}
        elif 'profile generator' in system or 'project-notes generator' in system:
            stage = 'P3'
            label = re.search(r'\bG\d+ \|', user).group(0).split()[0]
            fact = re.search(r'\bG\d+ \| [^|]+ \| ([^\n]+)', user).group(1)
            labels=re.findall(r'\bG\d+(?= \|)',user)
            out = {'profile': fact, 'memoryIds': [label], 'conflicts': [labels[:2]] if len(labels)>1 else []}
        elif system.startswith('You are the de-duplication reviewer'):
            stage = 'P4'
            out = {'pairs':[{'pair':'P1','decision':'keep','reason':'Different personal facts.'}]}
        elif system.startswith('You are the skill inducer'):
            stage = 'P5'
            out = {'skill':{'name':'Verify deployment','preconditions':'When a deployment needs verification',
                'steps':[{'text':'Inspect service status.','sourceMemoryIds':['E1','E2']},
                         {'text':'Compare logs with expected status.','sourceMemoryIds':['E1','E2']}],
                'cautions':[{'text':'Avoid assuming a deployment succeeded.','sourceMemoryIds':['E3']}],
                'extends':None},'reason':'Two verified successful approaches share these steps.'}
        else:
            raise AssertionError('unexpected stub stage')
        assert body['response_format'] == {'type': 'json_object'}
        if stage in ('P1','P1-b'):
            assert 'facts-object-v2' in system
            for fact in out['facts']:
                source = fact.pop('source')
                fact['sourceTurnId'], fact['sourceSpan'] = source['turnId'], source['span']
            assert body['max_tokens'] > 0
            assert 'analysis' not in out and 'summary' not in out
        if len(body['messages']) > 2:
            assert 'failed output validation' in body['messages'][-1]['content']
            assert body['messages'][-2]['role'] == 'assistant'
        Stub.calls.append(stage)
        # One returned, malformed completion must be retained before the client's retry.
        if stage == 'P1' and Stub.malformed_once:
            Stub.malformed_once = False
            content = 'malformed'
        elif stage == 'P1' and Stub.wrong_shape_once:
            Stub.wrong_shape_once = False
            content = '{"facts": "wrong"}'  # Local validation must trigger correction without provider schema.
        else:
            content = json.dumps(out)
        response = {'choices': [{'message': {'content': content}, 'finish_reason': 'stop'}],
                    'usage': {'prompt_tokens': 100, 'completion_tokens': 20, 'prompt_cache_hit_tokens': 30}}
        payload = json.dumps(response).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--bin', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    binary = args.bin.resolve()
    stub = ThreadingHTTPServer(('127.0.0.1', 0), Stub)
    threading.Thread(target=stub.serve_forever, daemon=True).start()
    proc = None
    with tempfile.TemporaryDirectory(prefix='memory-http-smoke-') as temp:
        temp = Path(temp)
        with socket.socket() as port_socket:
            port_socket.bind(('127.0.0.1', 0))
            port = port_socket.getsockname()[1]
        base = f'http://127.0.0.1:{port}'
        env = dict(os.environ, MEMORY_MOCK_MODELS='1', QWEN_API_KEY='offline-placeholder',
                   MEMORY_LLM_MAX_CALLS='60')
        logfile = open(temp / 'daemon.log', 'w')

        def request(path, body=None, tenant='http-smoke-en'):
            headers = {'Content-Type': 'application/json', 'X-Memory-Tenant': tenant,
                       'X-Memory-Platform': 'locomo'}
            req = Request(base + path, data=json.dumps(body).encode() if body is not None else None, headers=headers)
            with urlopen(req, timeout=150) as response:
                return json.load(response)

        def wait_tasks(receipts, tenant='http-smoke-en'):
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline:
                tasks = [request('/v1/get_task', {'requestId': r['requestId']}, tenant) for r in receipts]
                if all(t['status'] == 'completed' for t in tasks):
                    return tasks
                assert not any(t['status'] == 'failed' for t in tasks), tasks
                time.sleep(0.1)
            raise AssertionError(f'formation did not complete: {tasks}')

        def start(strategy='memos'):
            p = subprocess.Popen([str(binary), 'serve', '--memory-strategy', strategy, '--data-dir', str(temp/strategy),
                                  '--port', str(port), '--llm-base-url', f'http://127.0.0.1:{stub.server_port}',
                                  '--log-level', 'warn'],
                                 env=env, stdout=logfile, stderr=logfile)
            for _ in range(100):
                if p.poll() is not None:
                    raise RuntimeError((temp/'daemon.log').read_text())
                try:
                    request('/health')
                    return p
                except OSError:
                    time.sleep(0.1)
            raise RuntimeError('daemon startup timed out')

        try:
            for extra, expected in [
                (['--host', '0.0.0.0'], 'loopback'),
                (['--llm-concurrency', '0'], 'positive'),
            ]:
                failed = subprocess.run([str(binary), 'serve', '--memory-strategy', 'memos', *extra], env=env,
                                        capture_output=True, text=True)
                assert failed.returncode != 0 and expected in failed.stderr, failed.stderr
            for value, expected in [([], 'JSON object'), ({'batchTurns': 0}, 'batchTurns'),
                                    ({'batchTruns': 3}, 'unknown field')]:
                config = temp/'invalid-config.json'
                config.write_text(json.dumps(value))
                failed = subprocess.run([str(binary), 'serve', '--memory-strategy', 'memos', '--config', str(config)],
                                        env=env, capture_output=True, text=True)
                assert failed.returncode != 0 and expected in failed.stderr, failed.stderr
            failed = subprocess.run([str(binary), 'reindex'], env=env, capture_output=True, text=True)
            assert failed.returncode != 0 and 'select --vector' in failed.stderr
            proc = start()
            for value in [42, [], ['valid', 42], ['item']*129]:
                try:
                    request('/v1/embeddings', {'input': value})
                    raise AssertionError('invalid embedding input accepted')
                except HTTPError as error:
                    assert error.code == 400
            failed = subprocess.run([str(binary), 'reindex', '--url', base, '--all', '--tenant', 'bad/name'],
                                    env=env, capture_output=True, text=True)
            assert failed.returncode != 0 and '400' in failed.stderr
            scope = {'project': 'http-smoke', 'session': 'session_1'}
            utterances = [('Caroline: I joined a support group.', 'Melanie: I like painting sunsets.'),
                          ('Caroline: I volunteer at a library.', 'Melanie: I have a dog named Oliver.'),
                          ('Caroline: I live near a lake.', 'Melanie: I work at a cafe.')]
            now = datetime.now(timezone.utc) - timedelta(minutes=1)
            payloads, receipts = [], []
            for i, (u, a) in enumerate(utterances):
                def ts(offset):
                    return (now + timedelta(seconds=offset)).isoformat()
                payload = {'scope': scope, 'turnId': f'session_1-{i+1}', 'messages': [
                    {'role': 'user', 'content': u, 'timestamp': ts(2*i)},
                    {'role': 'assistant', 'content': a, 'timestamp': ts(2*i+1)}]}
                payloads.append(payload)
                receipts.append(request('/v1/add_memories', payload))
            # A full batch forms facts only while the session is open.
            wait_tasks(receipts)
            assert 'P2' not in Stub.calls
            calls_before_end = len(Stub.calls)
            request('/v1/add_memories', {'scope': scope, 'sessionEnded': True})
            idle = request('/admin/wait_idle', {})
            assert idle['status'] == 'idle' and idle['failedTurns'] == 0 and idle['failedExperiences'] == 0, idle
            assert len(Stub.calls) == calls_before_end + 1 and Stub.calls[-1] == 'P2'
            request('/v1/add_memories', {'scope': scope, 'sessionEnded': True})
            assert request('/admin/wait_idle', {})['status'] == 'idle'
            assert len(Stub.calls) == calls_before_end + 1  # Duplicate end does not re-run P2.
            tasks = [request('/v1/get_task', {'requestId': r['requestId']}) for r in receipts]
            assert all(t['status'] == 'completed' for t in tasks), tasks
            explicit = request('/v1/add_memories', {'scope': scope, 'text': 'The team service port is 8100.'})
            assert explicit['status'] == 'completed', explicit
            stats = request('/admin/stats', {})
            assert stats['counts']['turns'] == 3 and stats['counts']['memories'] == 7, stats
            extraction_records = request('/admin/export', {'table': 'extract_log'})
            fact_stages = [r for r in extraction_records['records'] if r.get('stage') in ('P1','P1-b')]
            assert {r['stage'] for r in fact_stages} == {'P1','P1-b'}, fact_stages
            assert len([r for r in extraction_records['records'] if r.get('stage') == 'P2']) == 1
            for record in fact_stages:
                assert record['outputFormat'] == 'facts-object-v2'
                assert isinstance(record['modelOutput'], dict)
                assert len(record['modelOutput']['facts']) == len(record['output']['facts'])
                for raw, formed in zip(record['modelOutput']['facts'], record['output']['facts']):
                    assert formed['source'] == {'turnId': raw['sourceTurnId'], 'span': raw['sourceSpan']}
                    assert 'source' not in raw and 'sourceSpan' not in formed
                assert set(record['output']) == {'facts'}
            response_records = request('/admin/export', {'table':'llm_responses'})['records']
            formation_responses = [r for r in response_records if r['stage']=='P1']
            assert len(formation_responses) == 3
            assert {r['rawContent'] for r in formation_responses if not r['validOutputShape']} == {'malformed', '{"facts": "wrong"}'}
            assert all(json.loads(r['rawBody'])['choices'][0]['message']['content'] == r['rawContent'] for r in formation_responses)
            corrected = sorted(formation_responses, key=lambda r:r['attempt'])[-1]
            assert '$.facts' in corrected['request']['messages'][-1]['content']
            assert corrected['request']['messages'][-2]['content'] == '{"facts": "wrong"}'
            request('/admin/run_jobs', {'jobs': ['profile','project_notes']})
            fixtures=[]
            for i,result in enumerate(['success: verified','partial：partly verified','failure: unverified']):
                fixtures.append({'id':f'mexp{i}', 'type':'experience','text':f'deployment verification lesson {i}',
                    'scope':{'kind':'global'},'status':'current','effectiveAt':now.date().isoformat(),
                    'recordedAt':now.isoformat(),'source':{'platform':'test','origin':'inferred','turnIds':[]},
                    'detail':{'situation':'deployment verification','approach':'Inspect service status; compare logs.',
                              'result':result,'lesson':f'Confirm the observed status {i}'}})
            request('/admin/seed',{'memories':fixtures})
            skills=request('/admin/run_jobs',{'jobs':['skills']})
            assert len(skills['jobs']['skills']['created']) == 1, skills
            skill=request('/v1/get_memory',{'id':skills['jobs']['skills']['created'][0]})
            assert skill['memory']['status'] == 'candidate', skill
            # Same source group does not generate another skill or another model call.
            calls_before=len(Stub.calls)
            assert request('/admin/run_jobs',{'jobs':['skills']})['jobs']['skills']['created'] == []
            assert len(Stub.calls)==calls_before
            search = request('/v1/search_memories', {'query': 'Caroline support group', 'scope': scope,
                                                    'touch': False, 'includeRecent': True})
            assert search['results'] and search['profiles'], search
            health = request('/health')
            assert health['memoryStrategy'] == 'memos', health
            assert health['tenantPromptVariants']['http-smoke-en'] == 'en', health
            assert health['llm']['calls'] == len(Stub.calls)
            assert not {'estimatedUsd', 'maxEstimatedUsd', 'offPeakOnly'} & health['llm'].keys()
            usage_records = request('/admin/export', {'table': 'llm_usage'})
            assert usage_records['count'] == len(Stub.calls), usage_records
            assert all(not {'estimatedUsd', 'pricingPeakUtc'} & row.keys() for row in usage_records['records'])
            traced_search = request('/v1/search_memories', {'query': 'Caroline support group', 'scope': scope,
                                    'types': ['fact'], 'includeRecent': False, 'touch': True})
            retrieval_records = request('/admin/export', {'table': 'retrieval_log'})
            logged_search = next(row for row in retrieval_records['records'] if row['mid'] == traced_search['retrievalId'])
            assert logged_search['candidates'] and logged_search['returned']['memories']
            assert {row['id'] for row in logged_search['returned']['memories']} <= {row['id'] for row in logged_search['candidates']}
            assert sum(r['validJson'] is False for r in usage_records['records']) == 1, usage_records
            assert sum(r['validOutputShape'] is False for r in usage_records['records']) == 2, usage_records
            assert request('/admin/export',{'table':'proposals'})['count'] == 1
            evidence_before=request('/admin/stats',{})['counts']['evidence']
            purge = request('/admin/run_jobs', {'jobs': ['purge'], 'retentionDays': 0})
            assert purge['jobs']['purge']['remainingTurns'] == 0, purge
            assert request('/admin/stats',{})['counts']['evidence']==evidence_before
            # Exercise each partial-batch trigger without shortening the configured five minutes.
            edge_tenant = 'formation-edge-en'
            for trigger in ('manual', 'end', 'timeout'):
                edge_scope = {'project': 'edge', 'session': trigger}
                stamp = datetime.now(timezone.utc).isoformat()
                tid = f'{trigger}-1'
                u, a = 'Caroline: I use the service.', 'Melanie: I verify deployments.'
                payload = {'scope': edge_scope, 'turnId': tid, 'messages': [
                    {'role': 'user', 'content': u, 'timestamp': stamp},
                    {'role': 'assistant', 'content': a, 'timestamp': stamp}]}
                if trigger == 'end':
                    payload['sessionEnded'] = True
                before = len(Stub.calls)
                receipt = request('/v1/add_memories', payload, edge_tenant)
                if trigger == 'manual':
                    assert request('/v1/get_task', {'requestId': receipt['requestId']}, edge_tenant)['status'] == 'queued'
                    assert request('/admin/wait_idle', {}, edge_tenant)['status'] == 'idle'
                elif trigger == 'timeout':
                    # Seed only the queued record's age; source timestamps and default 300 s rule stay intact.
                    old = (datetime.now(timezone.utc) - timedelta(seconds=301)).isoformat()
                    turn = {'key': f'locomo::{tid}', 'turnId': tid, 'platform': 'locomo',
                            'project': 'edge', 'session': trigger, 'userMessage': u, 'assistantMessage': a,
                            'userTs': stamp, 'assistantTs': stamp, 'startedAt': stamp, 'endedAt': stamp,
                            'recordedAt': old, 'digest': u+'\n'+a, 'seq': 1,
                            'requestId': receipt['requestId'], 'extractState': 'pending'}
                    request('/admin/seed', {'turns': [turn]}, edge_tenant)
                wait_tasks([receipt], edge_tenant)
                assert request('/admin/wait_idle', {}, edge_tenant)['status'] == 'idle'
                assert len(Stub.calls) == before + (2 if trigger == 'end' else 1), (trigger, Stub.calls)
                edge_logs = request('/admin/export', {'table': 'extract_log'}, edge_tenant)['records']
                p2_logs = [r for r in edge_logs if r.get('stage') == 'P2_validation' and r.get('session') == trigger]
                assert len(p2_logs) == (1 if trigger == 'end' else 0), (trigger, p2_logs)
            edge_usage = request('/admin/export', {'table': 'llm_usage'}, edge_tenant)['count']
            assert edge_usage == 4
            duplicate = request('/v1/add_memories', payloads[0])
            assert duplicate['duplicate'] and duplicate['requestId'] == receipts[0]['requestId'], duplicate
            proc.terminate(); proc.wait(timeout=15)
            proc = start()
            reloaded = request('/v1/search_memories', {'query': 'Caroline support group', 'scope': scope, 'touch': False})
            assert reloaded['results'] and not reloaded['recentTurns'], reloaded
            stats = request('/admin/stats', {})
            assert stats['index']['memoryVectors'] == 11, stats
            assert request('/v1/add_memories', payloads[0])['duplicate']
            # Exercise the actual stdio MCP bridge, including tenant and platform forwarding.
            bridge=subprocess.Popen([str(binary),'mcp','--url',base,'--platform','locomo'],
                env=dict(env,MEMORY_TENANT='http-smoke-en'),stdin=subprocess.PIPE,stdout=subprocess.PIPE,
                stderr=logfile,text=True)
            def rpc(value,reply=True):
                bridge.stdin.write(json.dumps(value)+'\n');bridge.stdin.flush()
                if reply:
                    if not select.select([bridge.stdout],[],[],10)[0]:
                        raise RuntimeError('MCP response timeout')
                    return json.loads(bridge.stdout.readline())
            try:
                initialized=rpc({'jsonrpc':'2.0','id':1,'method':'initialize','params':{
                    'protocolVersion':'2025-06-18','capabilities':{},'clientInfo':{'name':'offline-smoke','version':'1'}}})
                assert initialized['result']['capabilities']['tools'] is not None
                rpc({'jsonrpc':'2.0','method':'notifications/initialized'},False)
                listing=rpc({'jsonrpc':'2.0','id':2,'method':'tools/list','params':{}})
                names={tool['name'] for tool in listing['result']['tools']}
                assert names=={'add_memories','search_memories','get_context','get_task','list_turns',
                               'get_memory','correct_memory','feedback'},names
                def call(tool, arguments, identifier):
                    response = rpc({'jsonrpc':'2.0','id':identifier,'method':'tools/call',
                                    'params':{'name':tool,'arguments':arguments}})['result']
                    assert not response.get('isError', False), response
                    return json.loads(response['content'][0]['text'])
                mid=reloaded['results'][0]['memory']['id']
                assert call('get_memory', {'id':mid,'includeEvidence':True}, 3)['memory']['id']==mid
                assert call('get_task', {'requestId':receipts[0]['requestId']}, 4)==tasks[0]
                # An instruction is synchronous and never invokes the generation stub.
                instruction = call('add_memories', {'scope':scope, 'memoryType':'instruction',
                                   'text':'Keep source evidence with project decisions.'}, 5)
                context = call('get_context', {'scope':scope,'tokenBudget':1500}, 6)
                assert instruction['memoryIds'][0] in {m['id'] for m in context['instructions']}, context
                listed = call('list_turns', {'scope':scope,'tokenBudget':2000}, 7)
                assert listed == request('/v1/list_turns', {'scope':scope,'tokenBudget':2000}), listed
                invalid = rpc({'jsonrpc':'2.0','id':8,'method':'tools/call','params':{
                    'name':'get_task','arguments':{}}})['result']
                assert invalid['isError'] and 'HTTP 400' in invalid['content'][0]['text'], invalid
            finally:
                bridge.stdin.close()
                try:
                    bridge.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    bridge.terminate();bridge.wait(timeout=10)
            assert request('/health')['llm']['calls'] == 0  # Restart retrieval needs no generation.
            # Run the same public bridge against Mem0 using raw inputs, with no LLM calls.
            proc.terminate(); proc.wait(timeout=15)
            proc = start('mem0')
            bridge = subprocess.Popen([str(binary),'mcp','--url',base,'--platform','locomo'],
                env=dict(env,MEMORY_TENANT='http-smoke-en'),stdin=subprocess.PIPE,stdout=subprocess.PIPE,
                stderr=logfile,text=True)
            try:
                rpc({'jsonrpc':'2.0','id':1,'method':'initialize','params':{
                    'protocolVersion':'2025-06-18','capabilities':{},'clientInfo':{'name':'offline-smoke','version':'1'}}})
                rpc({'jsonrpc':'2.0','method':'notifications/initialized'},False)
                raw = dict(payloads[0], infer=False, sync=True)
                receipt = call('add_memories', raw, 2)
                task = call('get_task', {'requestId':receipt['requestId']}, 3)
                assert task['status']=='completed' and task['formed'] and task['searchable'], task
                listed = call('list_turns', {'scope':scope,'turnIds':[raw['turnId']],
                              'since':raw['messages'][-1]['timestamp'],'tokenBudget':2000}, 4)
                assert len(listed['turns'])==1 and listed['turns'][0]['messages']==raw['messages'], listed
                assert listed['turns'][0]['complete'], listed
                instruction = call('add_memories', {'scope':scope,'memoryType':'instruction',
                                   'text':'Keep source evidence with project decisions.'}, 5)
                context = call('get_context', {'scope':scope,'tokenBudget':1500}, 6)
                assert instruction['memoryIds'][0] in {m['id'] for m in context['instructions']}, context
                invalid = rpc({'jsonrpc':'2.0','id':7,'method':'tools/call','params':{
                    'name':'list_turns','arguments':{'scope':scope,'since':'not-a-date'}}})['result']
                assert invalid['isError'] and 'HTTP 400' in invalid['content'][0]['text'], invalid
            finally:
                bridge.stdin.close()
                try:
                    bridge.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    bridge.terminate();bridge.wait(timeout=10)
            assert request('/health')['llm']['calls'] == 0
            report = {'ok': True, 'mockModels': True, 'realLlmCalls': 0, 'stubCalls': len(Stub.calls),
                      'stages': Stub.calls, 'persistedUsageRows': usage_records['count'] + edge_usage, 'restartMemoryVectors': 11,
                      'checks': ['json_object with local validation and raw response persistence', 'independent fact and session-end experience commits',
                                 'three-turn/manual/session-end/300-second triggers', 'no redundant session-end P2 call',
                                 'P2 summary', 'P3 profiles', 'P4 proposal', 'P5 candidate/idempotency', 'span validation',
                                 'receipt idempotency', 'purge keeps evidence', 'tenant prompt',
                                 'retry usage', 'restart vectors', 'MCP eight tools/tenant/platform',
                                 'MCP context/task/turns/errors in memos and mem0', 'zero-generation retrieval',
                                 'loopback and config guards', 'strict embedding input', 'reindex failure exit status']}
            args.output.parent.mkdir(parents=True, exist_ok=True)
            args.output.write_text(json.dumps(report, indent=2) + '\n')
            print(json.dumps(report))
        finally:
            if proc and proc.poll() is None:
                proc.terminate(); proc.wait(timeout=15)
            logfile.close()
            stub.shutdown(); stub.server_close()


if __name__ == '__main__':
    main()
