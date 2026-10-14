"""Replay guard: fixed responses stay arm-local and search cannot generate."""
import json
import httpx
from mem0_sdk331_optimization import ReplayGateway


def test_captured_replay_is_independent_per_arm_and_blocks_extra_generation(tmp_path):
    original = tmp_path / 'original'
    (original / 'provider').mkdir(parents=True)
    for index, content in enumerate(('invalid-first', 'repaired-second'), 1):
        (original / 'provider' / f'{index:04}.json').write_text(json.dumps({
            'index': index, 'meta': {'stage': 'extract', 'arm': 'service', 'batch': 'b1'},
            'rawBody': json.dumps({'content': content}),
        }))
    run = tmp_path / 'run'; run.mkdir()
    gateway = ReplayGateway(run, original)
    gateway.meta = {'batch': 'b1'}
    try:
        with httpx.Client(trust_env=False) as client:
            for arm in ('before', 'after'):
                for expected in ('invalid-first', 'repaired-second'):
                    response = client.post(gateway.url + '/' + arm + '/service', json={})
                    assert response.status_code == 200
                    assert response.json()['content'] == expected
            gateway.formation_enabled = False
            assert client.post(gateway.url + '/after/service', json={}).status_code == 500
        replayed = [json.loads(line) for line in (run/'local-model-replay.jsonl').read_text().splitlines()]
        assert [(r['arm'], r['originalProviderId']) for r in replayed] == [
            ('before', 1), ('before', 2), ('after', 1), ('after', 2)]
    finally:
        gateway.close()
