from types import SimpleNamespace

from mem0_migration import select_questions, service_blocks
from fair_replay import pack_blocks
import json
import pytest
from common import jsonl
from common.metrics import count_tokens
from answer_and_judge import fingerprint, _stats
from mem0_migration import ARMS, corpus_hash, report


def test_service_blocks_preserve_evidence_as_its_own_source_date():
    response = {'results': [{'memory': {'id': 'a', 'text': 'event on May 7'}, 'score': .9,
        'evidence': [{'span': 'yesterday', 'statedAt': '2023-05-08',
                      'context': {'span': 'when?', 'statedAt': '2023-05-08'}}]}]}
    result = service_blocks(response, evidence=True)
    assert result[0][2] == 'event on May 7\nSource question at 2023-05-08: when?\nSource at 2023-05-08: yesterday'
    assert service_blocks(response)[0][2] == 'event on May 7'


def test_full_record_packing_can_skip_an_oversized_record():
    records = ['large ' * 300, 'small fact']
    assert pack_blocks(records, 10) == 'small fact'


def test_selected_questions_follow_protocol_not_dataset_order(monkeypatch):
    questions = [SimpleNamespace(qid='a'), SimpleNamespace(qid='b')]
    monkeypatch.setattr('mem0_migration.locomo.qas', lambda _: questions)
    assert [q.qid for q in select_questions({}, {'qids': ['b', 'a']})] == ['b', 'a']


def report_fixture(root):
    protocol = {'conversation': 'synthetic', 'sessions': ['session_1'], 'utterances': 2,
                'qids': ['q1', 'q2'], 'passes': 3, 'context_cap': 2000}
    for arm in ARMS:
        target = root / arm
        target.mkdir()
        (target / 'save-summary.json').write_text(json.dumps({'save_wall_ms': 1000}))
        answers = []
        for qid in protocol['qids']:
            context = {'qid': qid, 'system': arm, 'setting': 'synthetic', 'question': 'Question?',
                       'answer': 'Fact', 'context': 'Supporting fact', 'context_tokens': count_tokens('Supporting fact')}
            jsonl.append(target / 'contexts.jsonl', context)
            correct = not (arm == 'mem0-strategy' and qid == 'q2')
            answer = {**context, 'source_hash': fingerprint(context, 'unified'),
                      'judge_label': 'CORRECT' if correct else 'WRONG', 'J': int(correct),
                      'f1': .5, 'bleu1': .5}
            answers.append(answer)
            jsonl.append(target / 'answers-unified.jsonl', answer)
            for i, ms in enumerate([100, 10, 20]):
                jsonl.append(target / 'retrieval.jsonl', {'qid': qid, 'pass': i, 'search_ms': ms, 'context': context['context']})
        (target / 'summary-unified.json').write_text(json.dumps({
            'overall': _stats(answers), 'n_contexts': 2, 'n_answered': 2}))
        (target / 'provider.jsonl').write_text(json.dumps({'ms': 500, 'response': {'usage': {}}}) + '\n')
        corpus = {'store': [{'id': 'm', 'text': 'Fact'}]}
        for name in ('corpus-before-search.json', 'corpus-after-search.json'):
            (target / name).write_text(json.dumps(corpus))
        hash_value = corpus_hash(corpus['store'])
        (target / 'retrieval-audit.json').write_text(json.dumps({
            'store': {'before': hash_value, 'after': hash_value, 'count': 1}}))
    return protocol


def test_report_reads_complete_evidence_and_generates_current_results(tmp_path):
    protocol = report_fixture(tmp_path)
    report(tmp_path, protocol)
    result = json.loads((tmp_path / 'comparison.json').read_text())
    assert result['systems']['current']['provider_attempts'] == 1
    assert result['systems']['current']['warm_p95_ms'] == 20
    assert result['systems']['mem0-strategy']['correct'] == 1
    text = (tmp_path / 'comparison.md').read_text()
    assert 'mem0-strategy: 1/2; wrong qids: q2; stored memories: 1' in text
    assert 'current: 2/2; wrong qids: none' in text
    assert '33/35' not in text and '52-record' not in text
    assert '1 chronological synthetic sessions; 2 utterances' in text


@pytest.mark.parametrize('defect', ['missing_audit', 'empty_audit', 'changed_corpus',
    'bad_hash', 'missing_pass', 'duplicate_pass', 'changed_context', 'missing_answer',
    'invalid_judge', 'stale_fingerprint', 'stale_summary'])
def test_report_rejects_missing_or_inconsistent_evidence(tmp_path, defect):
    protocol = report_fixture(tmp_path)
    target = tmp_path / 'mem0-strategy'
    if defect == 'missing_audit':
        (target / 'retrieval-audit.json').unlink()
    elif defect == 'empty_audit':
        (target / 'retrieval-audit.json').write_text('{}')
    elif defect == 'changed_corpus':
        (target / 'corpus-after-search.json').write_text(json.dumps({'store': [{'id': 'm', 'text': 'Changed'}]}))
    elif defect == 'bad_hash':
        (target / 'retrieval-audit.json').write_text(json.dumps({'store': {'before': 'bad', 'after': 'bad', 'count': 1}}))
    elif defect in ('missing_pass', 'duplicate_pass', 'changed_context'):
        path = target / 'retrieval.jsonl'
        rows = list(jsonl.read(path))
        if defect == 'missing_pass': rows.pop()
        elif defect == 'duplicate_pass': rows.append(rows[0])
        else: rows[-1]['context'] = 'Changed'
        path.write_text(''.join(json.dumps(row) + '\n' for row in rows))
    elif defect == 'stale_summary':
        (target / 'summary-unified.json').write_text(json.dumps({'overall': {'J': 1.}, 'n_answered': 2, 'n_contexts': 2}))
    else:
        path = target / 'answers-unified.jsonl'
        rows = list(jsonl.read(path))
        if defect == 'missing_answer': rows.pop()
        elif defect == 'invalid_judge': rows[0]['judge_label'] = 'INVALID'
        else: rows[0]['source_hash'] = 'stale'
        path.write_text(''.join(json.dumps(row) + '\n' for row in rows))
    with pytest.raises(ValueError):
        report(tmp_path, protocol)
    assert not (tmp_path / 'comparison.md').exists()
    assert not (tmp_path / 'comparison.json').exists()
