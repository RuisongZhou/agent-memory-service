from common import locomo
from locomo_full import batches, batch_key, questions, protocol


def test_full_dataset_coverage_and_frozen_workload(monkeypatch, tmp_path):
    # The protocol fingerprints the daemon binary; the Python CI job does not build it.
    import locomo_full
    binary = tmp_path / 'target/fast/agent-memoryd'
    binary.parent.mkdir(parents=True)
    binary.write_bytes(b'binary')
    monkeypatch.setattr(locomo_full, 'REPO', tmp_path)
    data = locomo.load()
    spec = protocol(data, 10)
    rows = [q for c in data for q in questions(c)]
    assert len(data) == 10
    assert sum(len(locomo.sessions(c)) for c in data) == 272
    assert sum(c['utterances'] for c in spec['conversations']) == 5882
    assert sum(c['batches'] for c in spec['conversations']) == 1696
    assert len(rows) == len({q['qid'] for q in rows}) == 1986
    assert sum(q['category'] < 5 for q in rows) == 1540
    assert sum(q['category'] == 5 for q in rows) == 446
    assert spec['context_cap'] == 2000
    assert spec['search_top_k_per_store'] == 30


def test_adversarial_distractor_never_becomes_gold():
    conv = {'sample_id': 'example', 'qa': [
        {'category': 5, 'question': 'Unsupported?', 'adversarial_answer': 'wrong distractor'},
        {'category': 4, 'question': 'Supported?', 'answer': 'correct fact'}]}
    rows = questions(conv)
    assert rows[0]['answer'] == 'No information available in the memories.'
    assert rows[1]['answer'] == 'correct fact'
    assert rows[1]['qid'] == 'example-q1'


def test_both_perspectives_preserve_every_utterance_once_and_batch_order():
    conv = locomo.load()[0]
    rows = list(batches(conv, 8))
    assert len({batch_key(p, s, i) for p, s, i, _ in rows}) == len(rows)
    speakers = locomo.speakers(conv)
    for speaker in speakers:
        selected = [(s, i, m) for p, s, i, m in rows if p.speaker == speaker]
        assert [(s.n, i) for s, i, _ in selected] == sorted((s.n, i) for s, i, _ in selected)
        assert sum(len(m) for _, _, m in selected) == 419
        for s, offset, messages in selected:
            assert 1 <= len(messages) <= 8
            for source, message in zip(s.utterances[offset:offset+8], messages):
                assert message['content'] == locomo.utterance_text(source)
                assert message['role'] == ('user' if source['speaker'] == speaker else 'assistant')


def test_sdk_relay_configuration_is_per_store_not_a_global_mutation(monkeypatch, tmp_path):
    from common import mem0_adapter
    from common.migration import sdk_stores
    original = mem0_adapter.QWEN_BASE_URL
    monkeypatch.setattr(mem0_adapter, 'Mem0Stores', lambda args: args)
    first = sdk_stores(tmp_path, 'http://embed', 'http://relay-1')
    second = sdk_stores(tmp_path, 'http://embed', 'http://relay-2')
    assert first.llm_base_url == 'http://relay-1'
    assert second.llm_base_url == 'http://relay-2'
    assert mem0_adapter.QWEN_BASE_URL == original


def test_resume_refuses_changed_context_packing_code(monkeypatch, tmp_path):
    import pytest
    import locomo_full
    repo = tmp_path / 'repo'
    names = ['eval/locomo_full.py', 'eval/mem0_migration.py', 'eval/mem0_run.py',
             'eval/answer_and_judge.py', 'eval/fair_replay.py', 'eval/common/metrics.py']
    for name in names:
        path = repo / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text('# frozen source\n')
    monkeypatch.setattr(locomo_full, 'REPO', repo)
    monkeypatch.setattr(locomo_full, 'protocol', lambda *_: {'binary_sha256': 'unused'})
    root = tmp_path / 'run'
    locomo_full.prepare(root, [], 1)
    locomo_full.prepare(root, [], 1)
    (repo / 'eval/fair_replay.py').write_text('# different context packing\n')
    with pytest.raises(RuntimeError, match='Source changed'):
        locomo_full.prepare(root, [], 1)
