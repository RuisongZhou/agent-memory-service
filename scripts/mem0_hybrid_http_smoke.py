#!/usr/bin/env python3
"""Offline entity projection, scope, correction/delete and restart regression."""
from smoke_support import arguments, chat_stub, daemon, write_report


def reply(body, calls):
    prompt = body['messages'][-1]['content']
    fact = prompt.split('## New Messages\n')[1].split('\n\n## Observation Date')[0].split(': ', 1)[1]
    return {'memory': [{'id': '0', 'text': fact, 'attributed_to': 'user', 'linked_memory_ids': []}]}


def main():
    args = arguments(__doc__)
    with chat_stub(reply) as stub, daemon(args.bin, stub) as service:
        calls = stub.calls
        api = service.request
        evidence = {}

        def add(turn, text, project='p', infer=True):
            return api('/v1/add_memories', {'scope': {'project': project, 'session': 's'}, 'turnId': turn,
                'messages': [{'role': 'user', 'content': text, 'timestamp': '2026-10-14T00:00:00Z'}],
                'infer': infer, 'sync': True})['memoryIds'][0]

        def search(project='p'):
            return api('/v1/search_memories', {'query': 'Alice Johnson', 'scope': {'project': project},
                       'topK': 20, 'tokenBudget': 10000})['results']

        def entities():
            return api('/admin/export', {'table': 'mem0_entities'})['records']

        old = add('original', 'Alice Johnson moved to New York.')
        raw = add('raw', 'Bob Smith moved to Boston.', infer=False)
        foreign = add('foreign', 'Alice Johnson moved to Paris.', project='other')
        before = entities()
        assert before and any(old in e['linkedMemoryIds'] for e in before)
        assert all(raw not in e['linkedMemoryIds'] for e in before)
        found = search()
        assert old in {r['memory']['id'] for r in found}
        assert foreign not in {r['memory']['id'] for r in found}
        fixed = api('/v1/correct_memory', {'id': old, 'text': 'Alice Johnson moved to London.'})['memory']['id']
        api('/admin/wait_idle', {})
        found = search()
        assert fixed in {r['memory']['id'] for r in found} and old not in {r['memory']['id'] for r in found}
        after = entities()
        assert all(old not in e['linkedMemoryIds'] for e in after)
        assert any(fixed in e['linkedMemoryIds'] for e in after)
        service.restart()
        assert fixed in {r['memory']['id'] for r in search()}
        api('/v1/delete_memory', {'id': fixed})
        assert fixed not in {r['memory']['id'] for r in search()}
        remaining = entities()
        assert all(fixed not in e['linkedMemoryIds'] and old not in e['linkedMemoryIds'] for e in remaining)
        assert any(foreign in e['linkedMemoryIds'] for e in remaining)
        assert len(calls) == 2, len(calls)
        evidence.update(before=before, afterCorrection=after, afterDelete=remaining)
        report = {'ok': True, 'realModelCalls': 0, 'stubCalls': len(calls),
                  'checks': ['inferred entity persistence', 'raw excludes entity extraction',
                             'project isolation', 'correction reprojects links',
                             'restart projection', 'delete prunes links', 'no generation during search/recovery'],
                  'evidence': evidence}
        write_report(report, args.output, hidden=('evidence',))


if __name__ == '__main__':
    main()
