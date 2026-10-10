#!/usr/bin/env python3
"""Explicit context-only ablation: retain retrieved memories/profiles, exclude recent-turn text.

Does not purge sources or bypass memory_run's main-setting guard. Search latency is the original
buffer search (including recent-turn retrieval), not a separately measured longterm-only API.
"""
import argparse
import hashlib
import json
from pathlib import Path
import sys

ROOT=Path(__file__).resolve().parent.parent
sys.path.insert(0,str(ROOT/'eval'))
from common.metrics import count_tokens

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--contexts',type=Path,required=True)
    parser.add_argument('--retained-source-turns',type=int,required=True)
    args=parser.parse_args()
    rows=[json.loads(x) for x in args.contexts.read_text().splitlines()]
    if len({r['qid'] for r in rows})!=len(rows):raise SystemExit('Source cohort contains duplicate questions.')
    target=args.contexts.parent.parent/'longterm-only'
    target.mkdir(exist_ok=False)
    marker='Recent conversation turns:'
    transformed=[]
    removed=0
    for original in rows:
        row=dict(original);turns=row['n_turns'];context=row['context']
        if turns:
            assert context.count(marker)==1, 'Expected exactly the formatter-owned recent-turn section.'
            context=context.split(marker,1)[0].rstrip('\n')
        else:
            assert marker not in context
        removed+=turns
        row.update(setting='longterm-only',source_setting=original['setting'],context=context,
                   context_tokens=count_tokens(context),n_turns=0,turn_ids=[],gap=not bool(context),
                   source_recent_turn_count=turns,source_context_sha256=hashlib.sha256(original['context'].encode()).hexdigest(),
                   latency_note='Copied buffer search latency; includes recent-turn retrieval. No dedicated longterm-only search measured.')
        assert marker not in row['context'] and row['n_turns']==0
        transformed.append(row)
    assert {r['qid'] for r in transformed}=={r['qid'] for r in rows}
    (target/'contexts.jsonl').write_text(''.join(json.dumps(r,ensure_ascii=False)+'\n' for r in transformed))
    protocol={'setting':'longterm-only','method':'Remove only formatter-owned recent-turn section; preserve retrieved memories/profiles and their order.',
        'source':str(args.contexts.resolve()),'source_sha256':hashlib.sha256(args.contexts.read_bytes()).hexdigest(),
        'source_setting':'buffer','questions':len(rows),'excluded_turn_occurrences':removed,
        'physical_purge_main_completed':False,'retained_source_turns_after_purge':args.retained_source_turns,
        'latency_note':'Source buffer search includes recent-turn retrieval; not an isolated longterm-only search latency.'}
    (target/'protocol.json').write_text(json.dumps(protocol,indent=2))
    print(json.dumps(protocol))

if __name__=='__main__':main()
