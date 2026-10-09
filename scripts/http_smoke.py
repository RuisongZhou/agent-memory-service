#!/usr/bin/env python3
"""Run the real daemon against a localhost-only deterministic LLM stub. No paid calls.

Exercises HTTP formation, persisted retry billing, profiles, purge, restart and idempotency.
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

    def log_message(self, *_):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        assert body['thinking'] == {'type': 'disabled'}
        assert body['temperature'] == 0
        assert body['response_format'] == {'type': 'json_object'}
        system, user = [m['content'] for m in body['messages']]
        if '<current batch>' in user:
            stage = 'P1'
            facts = []
            batch = user.split('<current batch>')[1].split('</current batch>')[0]
            for turn_id, date, messages in re.findall(r'\[([^|]+) \| ([^\]]+)\]\n([^\[]+)', batch):
                for line in messages.strip().splitlines():
                    if not line.strip():
                        continue
                    facts.append({'text': line, 'scope': 'global', 'entities': [line.split(':')[0]],
                                  'effectiveAt': date[:10], 'origin': 'user',
                                  'source': {'turnId': turn_id.strip(), 'span': line}, 'relations': []})
            out = {'facts': facts}
        elif '<content to remember' in user:
            stage = 'P1-b'
            text = re.search(r'<content to remember[^>]*>\s*(.*?)\s*</content to remember>', user, re.S).group(1)
            out = {'facts': [{'text': text, 'scope': 'project', 'entities': ['team'],
                             'effectiveAt': datetime.now(timezone.utc).date().isoformat(), 'origin': 'explicit',
                             'source': {'turnId': 'text', 'span': text}, 'relations': []}]}
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
        Stub.calls.append(stage)
        # One billed, malformed completion must be retained before the client's retry.
        if stage == 'P1' and Stub.malformed_once:
            Stub.malformed_once = False
            content = 'malformed'
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
        env = dict(os.environ, MEMORY_MOCK_MODELS='1', DEEPSEEK_API_KEY='offline-placeholder',
                   MEMORY_LLM_MAX_CALLS='60')
        logfile = open(temp / 'daemon.log', 'w')

        def request(path, body=None, tenant='http-smoke-en'):
            headers = {'Content-Type': 'application/json', 'X-Memory-Tenant': tenant,
                       'X-Memory-Platform': 'locomo'}
            req = Request(base + path, data=json.dumps(body).encode() if body is not None else None, headers=headers)
            with urlopen(req, timeout=150) as response:
                return json.load(response)

        def start():
            p = subprocess.Popen([str(binary), 'serve', '--data-dir', str(temp/'data'), '--models-dir', str(temp),
                                  '--port', str(port), '--llm-base-url', f'http://127.0.0.1:{stub.server_port}',
                                  '--llm-model', 'offline-stub', '--log-level', 'warn'],
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
                failed = subprocess.run([str(binary), 'serve', *extra], env=env,
                                        capture_output=True, text=True)
                assert failed.returncode != 0 and expected in failed.stderr, failed.stderr
            for value, expected in [([], 'JSON object'), ({'batchTurns': 0}, 'batchTurns'),
                                    ({'batchTruns': 3}, 'unknown field')]:
                config = temp/'invalid-config.json'
                config.write_text(json.dumps(value))
                failed = subprocess.run([str(binary), 'serve', '--config', str(config)],
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
            request('/v1/add_memories', {'scope': scope, 'sessionEnded': True})
            idle = request('/admin/wait_idle', {})
            assert idle['status'] == 'idle' and idle['failedTurns'] == 0 and idle['failedExperiences'] == 0, idle
            tasks = [request('/v1/get_task', {'requestId': r['requestId']}) for r in receipts]
            assert all(t['status'] == 'completed' for t in tasks), tasks
            explicit = request('/v1/add_memories', {'scope': scope, 'text': 'The team service port is 8100.'})
            assert explicit['status'] == 'completed', explicit
            stats = request('/admin/stats', {})
            assert stats['counts']['turns'] == 3 and stats['counts']['memories'] == 7, stats
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
            assert health['tenantPromptVariants']['http-smoke-en'] == 'en', health
            assert health['llm']['calls'] == len(Stub.calls)
            bills = request('/admin/export', {'table': 'llm_usage'})
            assert bills['count'] == len(Stub.calls), bills
            assert sum(r['validJson'] is False for r in bills['records']) == 1, bills
            assert request('/admin/export',{'table':'proposals'})['count'] == 1
            evidence_before=request('/admin/stats',{})['counts']['evidence']
            purge = request('/admin/run_jobs', {'jobs': ['purge'], 'retentionDays': 0})
            assert purge['jobs']['purge']['remainingTurns'] == 0, purge
            assert request('/admin/stats',{})['counts']['evidence']==evidence_before
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
                assert names=={'add_memories','search_memories','get_memory','correct_memory','feedback'},names
                mid=reloaded['results'][0]['memory']['id']
                result=rpc({'jsonrpc':'2.0','id':3,'method':'tools/call','params':{
                    'name':'get_memory','arguments':{'id':mid,'includeEvidence':True}}})
                assert not result['result'].get('isError',False),result
                assert json.loads(result['result']['content'][0]['text'])['memory']['id']==mid,result
            finally:
                bridge.stdin.close()
                try:
                    bridge.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    bridge.terminate();bridge.wait(timeout=10)
            assert request('/health')['llm']['calls'] == 0  # Restart retrieval needs no generation.
            report = {'ok': True, 'mockModels': True, 'realLlmCalls': 0, 'stubCalls': len(Stub.calls),
                      'stages': Stub.calls, 'persistedBillingRows': bills['count'], 'restartMemoryVectors': 11,
                      'checks': ['HTTP P1 retry', 'P1-b', 'P2 summary', 'P3 profiles', 'P4 proposal', 'P5 candidate/idempotency', 'span validation',
                                 'receipt idempotency', 'purge keeps evidence', 'tenant prompt',
                                 'retry billing', 'restart vectors', 'MCP initialize/list/call/tenant', 'zero-generation retrieval',
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
