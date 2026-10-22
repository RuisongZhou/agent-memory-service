#!/usr/bin/env python3
"""Offline HTTP tests of additive formation, repair, provenance and restart visibility."""
import json

from smoke_support import arguments, chat_stub, daemon, write_report


def reply(body, calls):
    assert body['enable_thinking'] is False
    if len(calls) == 1:
        return {'memory': [{'id': '0', 'text': 'database port 8765', 'attributed_to': 'bad'}]}
    assert len(body['messages']) == 2
    assert 'failed output validation' in body['messages'][1]['content']
    assert 'memory[0].attributed_to must be user or assistant' in body['messages'][1]['content']
    return {'memory': [{'id': '0', 'text': 'database port 8765', 'attributed_to': 'user'}]}


def main():
    args = arguments(__doc__)
    with chat_stub(reply) as stub, daemon(args.bin, stub) as service:
        calls = stub.calls
        request = service.request
        scope = {'project': 'a', 'session': 's'}
        write = {'scope': scope, 'turnId': 't1', 'sync': True, 'messages': [
            {'role': 'user', 'content': 'database port 8765', 'timestamp': '2023-05-08T13:56:00Z'}]}
        saved = request('/v1/add_memories', write)
        assert saved['status'] == 'completed' and saved['searchable']
        assert len(calls) == 2 and 'attributed_to' in json.dumps(calls[1])
        raw = request('/admin/export', {'table': 'llm_responses'})['records']
        assert len(raw) == 2, raw
        assert request('/v1/add_memories', dict(write, messages=[dict(write['messages'][0], content='changed retry')]))['duplicate']
        assert len(calls) == 2
        found = request('/v1/search_memories', {'query': 'database port 8765', 'scope': scope, 'includeEvidence': True})
        assert found['results'][0]['memory']['id'] == saved['memoryIds'][0]
        assert found['results'][0]['sourceBatch']['verifiedClaimSpan'] is False
        source = request('/v1/list_turns', {'scope': scope})['turns'][0]
        assert source['messages'][0]['content'] == 'database port 8765'
        request('/v1/add_memories', {'scope': scope, 'turnId': 'raw', 'text': 'raw message', 'infer': False})
        assert len(calls) == 2
        assert not request('/v1/search_memories', {'query': 'database port 8765', 'scope': {'project': 'other'}})['results']
        request('/v1/add_memories', {'scope': scope, 'sessionEnded': True, 'sync': True})
        assert len(calls) == 2  # no implicit P2/P3/P4/P5
        service.restart()
        found = request('/v1/search_memories', {'query': 'database port 8765', 'scope': scope})
        assert found['results'][0]['memory']['id'] == saved['memoryIds'][0]
        assert len(calls) == 2
        report = {'ok': True, 'realLlmCalls': 0, 'stubCalls': len(calls),
                  'checks': ['contract repair/raw replies', 'idempotency', 'raw bypass', 'scope',
                             'complete source inputs', 'batch provenance', 'immediate search',
                             'no implicit maintenance', 'restart vectors']}
        write_report(report, args.output)


if __name__ == '__main__':
    main()
