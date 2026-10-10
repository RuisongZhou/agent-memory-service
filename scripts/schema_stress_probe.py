#!/usr/bin/env python3
"""Explicit one-call replay of a saved truncation case with bounded dialogue-source schema."""
import argparse
import copy
from datetime import datetime, timezone
import json
from pathlib import Path
import re
import sys
import time

ROOT=Path(__file__).resolve().parent.parent
sys.path.insert(0,str(ROOT/'eval'))
from common.llm import MODEL,QWEN_BASE_URL,api_key,Ledger,usage_tokens

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--records',type=Path,required=True)
    parser.add_argument('--output-dir',type=Path,required=True)
    args=parser.parse_args()
    args.output_dir.mkdir(parents=True,exist_ok=False)
    records=json.loads(args.records.read_text())['records']
    record=next(x for x in sorted(records,key=lambda x:x['createdAt'])
                if x.get('finishReason')=='length' and x.get('attempt')==1 and 'session_13' in x['request']['messages'][1]['content'])
    body=copy.deepcopy(record['request']);body.pop('max_tokens',None)
    body['model']=MODEL
    user=body['messages'][1]['content']
    def turns(tag):
        text=re.search(rf'<{tag}>\n(.*?)\n</{tag}>',user,re.S).group(1)
        return re.findall(r'^\[([^|\]\n]+)\s*\|',text,re.M)
    current=[x.strip() for x in turns('current batch')]
    context=list(dict.fromkeys([x.strip() for x in turns('task context')]+current))
    schema=body['response_format']['json_schema']['schema']
    schema['properties']['facts']['items']['properties']['source']['properties']['turnId']['enum']=current
    schema['properties']['facts']['items']['properties']['source']['properties']['span']['maxLength']=1024
    for section in ['analysis','experiences']:
        refs=schema['properties'][section]['items']['properties']['turnIds']
        refs['items']['enum']=context;refs['maxItems']=len(context)
    schema['properties']['experiences']['maxItems']=3
    (args.output_dir/'request.json').write_text(json.dumps(body,ensure_ascii=False,indent=2))
    import httpx
    started=datetime.now(timezone.utc);clock=time.perf_counter()
    with httpx.Client(timeout=180,trust_env=False) as client:
        response=client.post(QWEN_BASE_URL+'/chat/completions',json=body,headers={'Authorization':'Bearer '+api_key()})
    (args.output_dir/'raw-response.txt').write_text(response.text)
    response.raise_for_status();parsed=response.json()
    hit,miss,out,reasoning=usage_tokens(parsed.get('usage',{}))
    Ledger(args.output_dir/'usage_ledger.jsonl').record('schema-stress',hit,miss,out,model=MODEL,ts=started,latency_ms=(time.perf_counter()-clock)*1000)
    content=parsed['choices'][0]['message']['content']
    (args.output_dir/'raw-content.txt').write_text(content)
    assert parsed['choices'][0]['finish_reason']=='stop'
    value=json.loads(content);assert set(value)=={'facts','analysis','experiences','summary'}
    for fact in value['facts']:
        assert fact['source']['turnId'] in current and len(fact['source']['span'])<=1024
    for section in ['analysis','experiences']:
        for row in value[section]:assert len(row['turnIds'])<=len(context) and all(x in context for x in row['turnIds'])
    assert len(value['experiences'])<=3
    report={'status':'passed','source_record':record['mid'],'current_turn_ids':current,'context_turn_ids':context,
        'latency_seconds':round(time.perf_counter()-clock,3),'input_tokens':hit+miss,'output_tokens':out,
        'facts':len(value['facts']),'analysis':len(value['analysis']),'raw_response_preserved':True}
    (args.output_dir/'result.json').write_text(json.dumps(report,indent=2))
    print(json.dumps(report))

if __name__=='__main__':main()
