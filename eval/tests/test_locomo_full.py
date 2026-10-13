from common import locomo
from locomo_full import batches, questions, protocol


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
    assert sum(c['utterances'] for c in spec['conversations']) == 5882
    assert sum(c['batches'] for c in spec['conversations']) == 1696
    assert len({q['qid'] for q in rows}) == 1986
    assert sum(q['category'] == 5 for q in rows) == 446


def test_adversarial_distractor_never_becomes_gold():
    conv = {'sample_id': 'example', 'qa': [
        {'category': 5, 'question': 'Unsupported?', 'adversarial_answer': 'wrong distractor'},
        {'category': 4, 'question': 'Supported?', 'answer': 'correct fact'}]}
    rows = questions(conv)
    assert rows[0]['answer'] == 'No information available in the memories.'
    assert rows[1]['answer'] == 'correct fact'


def test_each_perspective_sees_every_utterance_once_in_order():
    conv = locomo.load()[0]
    rows = list(batches(conv, 8))
    for speaker in locomo.speakers(conv):
        selected = [(s, i, m) for p, s, i, m in rows if p.speaker == speaker]
        assert [(s.n, i) for s, i, _ in selected] == sorted((s.n, i) for s, i, _ in selected)
        assert sum(len(m) for _, _, m in selected) == 419
        s, offset, messages = selected[0]
        assert messages[0]['content'] == locomo.utterance_text(s.utterances[offset])
        assert messages[0]['role'] == ('user' if s.utterances[offset]['speaker'] == speaker else 'assistant')
