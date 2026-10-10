#!/usr/bin/env python3
"""Explicit paid conv-26 pilot/full run; optionally reuse a fixed, completed Mem0 baseline."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import time
from urllib.request import Request, urlopen

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / 'eval'))
from common import env, locomo
from common.llm import MODEL, QWEN_BASE_URL, api_key

def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--output-dir', type=Path, required=True)
    ap.add_argument('--bin', type=Path, required=True)
    ap.add_argument('--sessions', type=int, help='Ingest-only small-sample validation')
    ap.add_argument('--baseline', type=Path, help='Completed Mem0 baseline, never rerun')
    a = ap.parse_args()
    out, binary = a.output_dir.resolve(), a.bin.resolve()
    out.mkdir(parents=True, exist_ok=True)
    if (out/'run-status.json').exists():
        raise SystemExit('Use a fresh output/store directory.')
    py = ROOT / '.venv/bin/python'
    now = lambda: datetime.now(timezone.utc).isoformat()
    state = {'status':'running', 'started_at':now(), 'stages':[]}
    def save():
        (out/'run-status.json').write_text(json.dumps(state,indent=2))
    def run(name, argv):
        record={'name':name,'started_at':now(),'command':list(map(str,argv))}
        state['stages'].append(record); save(); print('START',name,flush=True)
        start=time.perf_counter()
        with (out/f'{name}.log').open('w') as log:
            result=subprocess.run(list(map(str,argv)),cwd=ROOT,stdout=log,stderr=log)
        record.update(returncode=result.returncode,seconds=round(time.perf_counter()-start,3),finished_at=now())
        save(); print('END',name,record['returncode'],record['seconds'],flush=True)
        return result.returncode==0
    if a.baseline:
        baseline=a.baseline.resolve()
        summary=json.loads((baseline/'mem0/k30/summary-unified.json').read_text())
        if not summary['scope']['answering_complete'] or summary['scope']['n_expected_questions']!=152:
            raise SystemExit('Baseline must have a complete 152-question conv-26 result.')
        old_protocol=json.loads((baseline/'comparison-protocol.json').read_text())
        if old_protocol['generation_model']!=MODEL:
            raise SystemExit('Model differs from the fixed baseline.')
        shutil.copytree(baseline/'mem0',out/'mem0')
        lines=[line for line in (baseline/'usage_ledger.jsonl').read_text().splitlines()
               if json.loads(line)['component'].startswith(('mem0.', 'answer.mem0/', 'judge.mem0/'))]
        (out/'usage_ledger.jsonl').write_text('\n'.join(lines)+'\n')
    conv=locomo.select(locomo.load(),['conv-26'])[0]
    sources=[p for p in (ROOT/'crates').rglob('*') if p.is_file() and p.suffix in ['.rs','.txt','.toml']]
    sources += [p for p in (ROOT/'eval').rglob('*.py') if not any(x in p.parts for x in ['.cache','results','stores'])]
    protocol={'conversation':'conv-26','sessions':a.sessions,'question_count':152,'model':MODEL,
        'endpoint':QWEN_BASE_URL,'temperature':0,'enable_thinking':False,'output_format':'formation-object-v2',
        'response_format':'json_schema','strict':True,'source_turn_id_enums':True,'bounded_evidence_lists':True,'structured_max_tokens':None,'prompt_variant':'en','fresh_store':True,
        'dataset_sha256':hashlib.sha256(env.DATA_PATH.read_bytes()).hexdigest(),
        'git_head':subprocess.check_output(['git','rev-parse','HEAD'],cwd=ROOT,text=True).strip(),
        'diff_sha256':hashlib.sha256(subprocess.check_output(['git','diff'],cwd=ROOT)).hexdigest(),
        'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),
        'source_sha256':{str(p.relative_to(ROOT)):hashlib.sha256(p.read_bytes()).hexdigest() for p in sources},
        'baseline':str(a.baseline.resolve()) if a.baseline else None,
        'baseline_summary_sha256':hashlib.sha256((a.baseline/'mem0/k30/summary-unified.json').read_bytes()).hexdigest() if a.baseline else None}
    (out/'comparison-protocol.json').write_text(json.dumps(protocol,indent=2))
    with socket.socket() as s:
        s.bind(('127.0.0.1',0)); port=s.getsockname()[1]
    base=f'http://127.0.0.1:{port}'
    def api(path,body=None):
        req=Request(base+path,headers={'Content-Type':'application/json','X-Memory-Tenant':'conv-26-en','X-Memory-Platform':'locomo'},
                    data=json.dumps(body).encode() if body is not None else None)
        with urlopen(req,timeout=60) as response: return json.load(response)
    def audit(label):
        for table in ['llm_responses','llm_usage','extract_log']:
            data=api('/admin/export',{'table':table})
            (out/f'{label}-{table}.json').write_text(json.dumps(data,ensure_ascii=False,indent=2))
        (out/f'{label}-stats.json').write_text(json.dumps(api('/admin/stats',{}),indent=2))
    daemon_env=dict(os.environ,QWEN_API_KEY=api_key(),QWEN_BASE_URL=QWEN_BASE_URL,LLM_MODEL=MODEL,MEMORY_LLM_MAX_CALLS='512')
    daemon_env.pop('MEMORY_MOCK_MODELS',None)
    log=(out/'daemon.log').open('w')
    daemon=subprocess.Popen([str(binary),'serve','--data-dir',str(out/'service-store'),
        '--port',str(port),'--prompt-variant','en','--llm-concurrency','4','--log-level','warn'],
        cwd=ROOT,env=daemon_env,stdout=log,stderr=log)
    state.update(base_url=base,daemon_pid=daemon.pid); save()
    ready=False
    try:
        for _ in range(180):
            if daemon.poll() is not None: raise RuntimeError('daemon exited before readiness')
            try:
                health=api('/health'); ready=True; break
            except OSError: time.sleep(.5)
        if not ready: raise RuntimeError('daemon startup timeout')
        (out/'health-start.json').write_text(json.dumps(health,indent=2))
        if health['modelBackend']!='llama-server' or health['llm']['model']!=MODEL:
            raise RuntimeError('unexpected actual model runtime')
        command=[py,'eval/memory_run.py','--base-url',base,'--tag','memory-en','--prompt-variant','en',
                 '--convs','conv-26','--results-dir',out]
        if a.sessions: command+=['--stage','ingest','--sessions',str(a.sessions)]
        service_ok=run('service',command)
        audit('service')
        if a.sessions:
            state['status']='pilot_passed' if service_ok else 'pilot_failed'
        else:
            for setting in ['main','buffer']:
                context=out/'memory-en'/setting/'contexts.jsonl'
                if not context.exists(): continue
                for attempt in range(3):
                    run(f'answer-{setting}-{attempt}',[py,'eval/answer_and_judge.py','--contexts',context,'--results-dir',out])
                    summary=json.loads((context.parent/'summary-unified.json').read_text())
                    if summary['scope']['answering_complete']: break
            run('report',[py,'eval/report.py','--results-dir',out,'--expected-systems','memory-en,mem0'])
            run('usage',[py,'eval/usage_stats.py','--ledger',out/'usage_ledger.jsonl','--json'])
            state['status']='completed' if service_ok else 'completed_with_pipeline_error'
    except Exception as e:
        state.update(status='failed',error=str(e)); raise
    finally:
        if ready:
            try: audit('final')
            except Exception as e: state['audit_error']=str(e)
        state['finished_at']=now(); save()
        daemon.terminate()
        try: daemon.wait(timeout=15)
        except subprocess.TimeoutExpired: daemon.kill(); daemon.wait()
        log.close()

if __name__=='__main__': main()
