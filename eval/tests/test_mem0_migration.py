from common import migration
from common.migration import service_blocks
import json
import pytest
from common import jsonl
from common.metrics import count_tokens
from common.answers import fingerprint, summary_stats
from common.corpus import corpus_hash
from mem0_migration import ARMS, report


def test_service_blocks_preserve_evidence_as_its_own_source_date():
    response = {'results': [{'memory': {'id': 'a', 'text': 'event on May 7'}, 'score': .9,
        'evidence': [{'span': 'yesterday', 'statedAt': '2023-05-08',
                      'context': {'span': 'when?', 'statedAt': '2023-05-08'}}]}]}
    result = service_blocks(response, evidence=True)
    assert result[0][2] == 'event on May 7\nSource question at 2023-05-08: when?\nSource at 2023-05-08: yesterday'
    assert service_blocks(response)[0][2] == 'event on May 7'


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
            'overall': summary_stats(answers), 'n_contexts': 2, 'n_answered': 2}))
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
    assert result['systems']['current']['warm_p95_ms'] == 20
    assert result['systems']['mem0-strategy']['correct'] == 1
    assert 'mem0-strategy: 1/2; wrong qids: q2; stored memories: 1' in (tmp_path / 'comparison.md').read_text()


def test_report_rejects_a_corpus_changed_by_search(tmp_path):
    protocol = report_fixture(tmp_path)
    changed = {'store': [{'id': 'm', 'text': 'Changed'}]}
    (tmp_path / 'mem0-strategy/corpus-after-search.json').write_text(json.dumps(changed))
    with pytest.raises(ValueError):
        report(tmp_path, protocol)
    assert not (tmp_path / 'comparison.json').exists()


@pytest.mark.parametrize('arm,expected', [('current', 'current'), ('mem0-strategy', 'mem0')])
def test_migration_control_overrides_inherited_strategy(monkeypatch, tmp_path, arm, expected):
    monkeypatch.setenv('MEMORY_STRATEGY', 'mem0' if arm == 'current' else 'memos')
    class Captured(Exception):
        pass
    def launch(command, *, env, **kwargs):
        assert env['MEMORY_STRATEGY'] == expected
        raise Captured
    monkeypatch.setattr(migration.subprocess, 'Popen', launch)
    with pytest.raises(Captured):
        with migration.daemon(tmp_path, arm):
            pytest.fail('must stop before any HTTP requests')
