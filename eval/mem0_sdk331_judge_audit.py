#!/usr/bin/env python3
"""Audit identical judge inputs after the paid pilot; preserve original judgments.

Conflicting labels for an identical request get one extra identical-request vote,
then the same majority label is applied to both arms. No extraction/search rerun.
"""
import argparse
from collections import Counter, defaultdict
import hashlib
import json
from pathlib import Path
from common.sdk331_live import Gateway, rows, save


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, required=True)
    args = parser.parse_args(); root = args.root.resolve()
    protocol = json.loads((root / 'protocol.json').read_text())
    original = json.loads((root / 'summary.json').read_text())
    groups = defaultdict(list)
    for path in sorted((root / 'provider').glob('*.json')):
        item = json.loads(path.read_text())
        if item['meta']['stage'] != 'answer-judge': continue
        key = hashlib.sha256(json.dumps(item['request'], sort_keys=True).encode()).hexdigest()
        item['vote'] = json.loads(json.loads(item['rawBody'])['choices'][0]['message']['content'])['label']
        groups[key].append(item)
    gateway = Gateway(root, protocol)
    labels = {}; disputes = []; coverage_repairs = []
    try:
        for key, items in groups.items():
            votes = [r['vote'] for r in items]
            if len(set(votes)) > 1:
                cached = root / 'judge-consistency' / f'{key}.json'
                if cached.exists(): extra = json.loads(cached.read_text())
                else:
                    request = items[0]['request']
                    extra = gateway.chat(request['messages'], {'stage': 'answer-judge-audit', 'arm': 'shared',
                        'requestHash': key}, json_mode=True, max_tokens=request['max_tokens'])
                    assert extra.get('label') in ('CORRECT', 'WRONG')
                    save(cached, extra)
                votes.append(extra['label'])
                counts = Counter(votes)
                assert counts['CORRECT'] != counts['WRONG'], 'unresolved tie'
                canonical = counts.most_common(1)[0][0]
                disputes.append({'requestHash': key, 'originalVotes': [r['vote'] for r in items],
                                 'extraVote': extra['label'], 'canonical': canonical,
                                 'originalProviderIds': [r['index'] for r in items]})
            else: canonical = votes[0]
            for item in items: labels[item['meta']['arm'], item['meta']['qid']] = canonical
        corpus = json.loads((root / 'corpus.json').read_text())
        contexts = {(r['arm'], r['qid']): r['context'] for r in rows(root / 'retrieval.jsonl') if r['pass'] == 0}
        questions = {q['qid']: q for q in protocol['questions']}
        for arm in ('service', 'sdk'):
            coverage = [item for p in (root / 'coverage').glob(arm+'-*.json') for item in json.loads(p.read_text())['items']]
            inconsistent = [item for item in coverage if item['retrievedSupported'] and not item['storedSupported']]
            if not inconsistent: continue
            file = root / 'judge-consistency' / f'{arm}-coverage.json'
            if file.exists(): repaired = json.loads(file.read_text())
            else:
                memories = [m['text'] if arm == 'service' else m['memory'] for items in corpus[arm].values() for m in items]
                group = [{'qid': item['qid'], 'question': questions[item['qid']]['question'],
                          'reference': questions[item['qid']]['answer'], 'retrieved': contexts[arm, item['qid']]}
                         for item in inconsistent]
                prompt = ('Re-evaluate evidence coverage. A previous evaluation violated this invariant: RETRIEVED consists '
                    'only of complete memory texts selected from ALL_MEMORIES, so retrievedSupported=true and storedSupported=false '
                    'cannot both be correct. Re-examine both judgments; do not automatically assume either earlier judgment was right. '
                    'Use explicit facts and supported inferences, not world knowledge or the reference itself as evidence. '
                    'Return JSON {"items":[{"qid":"exact qid","storedSupported":true,"retrievedSupported":true,"reason":"brief"}]}.\n'
                    + 'ALL_MEMORIES:\n' + json.dumps(memories) + '\nQUESTIONS:\n' + json.dumps(group))
                error = 'Previous output failed validation: required top-level items array was missing. Return the exact requested JSON object.'
                for attempt in range(1, 3):
                    repaired = gateway.chat([{'role': 'user', 'content': prompt + '\n\nLOCAL VALIDATION ERROR: ' + error}],
                        {'stage': 'coverage-consistency-audit-repair', 'arm': arm, 'repair': attempt}, json_mode=True, max_tokens=4096)
                    try:
                        assert isinstance(repaired.get('items'), list), 'items must be an array'
                        assert len(repaired['items']) == len(inconsistent), 'wrong item count'
                        assert {i['qid'] for i in repaired['items']} == {i['qid'] for i in inconsistent}, 'qid mismatch'
                        assert all(isinstance(i[k], bool) for i in repaired['items'] for k in ('storedSupported','retrievedSupported')), 'flags must be booleans'
                        assert all(not i['retrievedSupported'] or i['storedSupported'] for i in repaired['items']), 'retrieved cannot be supported while stored is unsupported'
                        break
                    except (AssertionError, KeyError, TypeError) as exc:
                        error = str(exc)
                else: raise RuntimeError('bounded coverage consistency repair failed')
                save(file, repaired)
            coverage_repairs.append({'arm': arm, 'original': inconsistent, 'repaired': repaired['items']})
    finally: gateway.close()
    audited = []
    for item in rows(root / 'judgments.jsonl'):
        new = dict(item, originalLabel=item['label'], label=labels[item['arm'], item['qid']])
        audited.append(new)
    summary = json.loads(json.dumps(original))
    for arm in ('service', 'sdk'):
        group = [r for r in audited if r['arm'] == arm]
        summary['metrics'][arm]['answerCorrectOriginalJudge'] = summary['metrics'][arm]['answerCorrect']
        summary['metrics'][arm]['answerCorrect'] = sum(r['label'] == 'CORRECT' for r in group)
        # Independently inspected source/gold conflict: original utterance says Saturday, gold says Sunday.
        clean = [r for r in group if r['qid'] != 'conv-26-q5']
        summary['metrics'][arm]['goldConflictExcludedSensitivity'] = {'correct': sum(r['label'] == 'CORRECT' for r in clean), 'n': len(clean)}
        coverage = {item['qid']: item for p in (root/'coverage').glob(arm+'-*.json') for item in json.loads(p.read_text())['items']}
        for repair in coverage_repairs:
            if repair['arm'] == arm:
                coverage.update({item['qid']: item for item in repair['repaired']})
        summary['metrics'][arm]['storedAnswerCoverageOriginalJudge'] = summary['metrics'][arm]['storedAnswerCoverage']
        summary['metrics'][arm]['retrievedAnswerCoverageOriginalJudge'] = summary['metrics'][arm]['retrievedAnswerCoverage']
        summary['metrics'][arm]['storedAnswerCoverage'] = sum(i['storedSupported'] for i in coverage.values())
        summary['metrics'][arm]['retrievedAnswerCoverage'] = sum(i['retrievedSupported'] for i in coverage.values())
    summary['realProviderCalls'] = len(list((root / 'provider').glob('*.json')))
    summary['judgeConsistency'] = {'disputedIdenticalInputs': len(disputes), 'disputes': disputes, 'coverageRepairs': coverage_repairs}
    summary['goldSourceConflict'] = {'qid': 'conv-26-q5', 'sourceEvidence': 'D2:1',
                                    'sourceRelativeDay': 'last Saturday', 'goldRelativeDay': 'Sunday before 25 May 2023'}
    save(root / 'judgments-audited.json', audited)
    save(root / 'summary-audited.json', summary)
    save(root / 'protocol-amendment-judge-consistency.json', {
        'reason': 'identical judge requests received contradictory labels',
        'policy': 'one additional identical-request vote; share majority label; original judgments untouched',
        'changedFormationOrRetrieval': False, 'disputes': disputes, 'coverageRepairs': coverage_repairs,
        'goldConflictHandling': 'retain all 35 in primary; show 34-question exclusion only as sensitivity'})
    print(json.dumps(summary, ensure_ascii=False, indent=2))


if __name__ == '__main__': main()
