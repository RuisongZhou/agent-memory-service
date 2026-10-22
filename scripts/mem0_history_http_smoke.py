#!/usr/bin/env python3
"""Offline HTTP regression for inferred conversation history and prompt views."""
import time

from smoke_support import arguments, chat_stub, daemon, write_report


def reply(body, calls):
    assert body['enable_thinking'] is False
    prompt = body['messages'][1]['content']
    if 'FAIL_INPUT' in prompt.split('## New Messages\n')[1].split('## Observation Date')[0]:
        # A non-retryable model HTTP response exercises the five durable job
        # attempts without multiplying the transport retry backoff.
        return 400
    return {'memory': []}


def main():
    args = arguments(__doc__)
    with chat_stub(reply) as stub, daemon(args.bin, stub) as service:
        calls = stub.calls
        request = service.request
        scope = {'project': 'a', 'session': 's'}

        def add(turn, contents, selected_scope=scope, **extra):
            return request('/v1/add_memories', {'scope': selected_scope, 'turnId': turn,
                'messages': [{'role': 'user', 'content': c,
                'timestamp': f'2023-05-08T13:56:{i:02d}Z'}
                for i, c in enumerate(contents)], 'sync': extra.pop('sync', True), **extra})

        def latest_sections():
            prompt = calls[-1]['messages'][1]['content']
            history = prompt.split('## Last k Messages\n')[1].split('## Recently Extracted Memories')[0]
            current = prompt.split('## New Messages\n')[1].split('## Observation Date')[0]
            return history, current

        try:
            add('blank', [' ', '\n'])
        except AssertionError as error:
            assert ': 400:' in str(error)
        else:
            raise AssertionError('all-blank messages accepted')
        assert not calls
        long_text = '😀汉' * 160 + 'UNICODE_TAIL'
        zero = add('unicode', [long_text])
        assert zero['status'] == 'completed' and zero['memoryIds'] == []
        assert latest_sections()[1] == 'user: ' + long_text + '\n\n'
        add('cross-session', ['OTHER_SESSION_INPUT'], {'project': 'a', 'session': 'other'})
        add('raw', ['RAW_INPUT'], infer=False)
        add('probe', ['PROBE'])
        history, _ = latest_sections()
        assert history.strip() == 'user: ' + '😀汉' * 100 + '...', history
        turns = request('/v1/list_turns', {'scope': scope})['turns']
        assert next(t for t in turns if t['turnId'] == 'unicode')['messages'][0]['content'] == long_text
        boundary = 'a' * 299 + '😀tail'
        add('surrogate-boundary', [boundary])
        assert latest_sections()[1] == 'user: ' + boundary + '\n\n'
        add('surrogate-probe', ['SURROGATE_PROBE'])
        assert ('user: ' + 'a' * 299 + '...\n') in latest_sections()[0]
        assert '\ufffd' not in latest_sections()[0]
        add('batch1', [f'B1-{i}' for i in range(7)])
        add('batch2', [f'B2-{i}' for i in range(7)])
        add('ten-probe', ['TEN_PROBE'])
        history, _ = latest_sections()
        assert history.strip().splitlines() == [f'user: B1-{i}' for i in range(4, 7)] + [f'user: B2-{i}' for i in range(7)], history
        service.restart()
        add('restart-probe', ['RESTART_PROBE'])
        history, _ = latest_sections()
        assert history.strip().splitlines() == [f'user: B1-{i}' for i in range(5, 7)] + [f'user: B2-{i}' for i in range(7)] + ['user: TEN_PROBE'], history

        # Actual exhausted formation failure must never supply historical context.
        failed = add('failed', ['FAIL_INPUT'], sync=False)
        deadline = time.monotonic() + 110
        while time.monotonic() < deadline:
            task = request('/v1/get_task', {'requestId': failed['requestId']})
            if task['status'] == 'failed':
                break
            time.sleep(.25)
        else:
            raise AssertionError(f'failure did not exhaust retries: {task}')
        assert not task.get('formed', False)
        add('after-failure', ['AFTER_FAILURE'])
        history, _ = latest_sections()
        assert 'FAIL_INPUT' not in history and 'RAW_INPUT' not in history and 'OTHER_SESSION_INPUT' not in history, history
        service.restart()
        add('after-failure-restart', ['FINAL_PROBE'])
        history, _ = latest_sections()
        assert 'AFTER_FAILURE' in history and 'FAIL_INPUT' not in history
        report = {'ok': True, 'realLlmCalls': 0, 'stubCalls': len(calls),
                  'checks': ['new Unicode message complete', 'history UTF16 clipping', 'surrogate boundary safe', 'all-blank rejection',
                             'successful zero-memory context', 'session isolation', 'raw isolation',
                             'ten messages across batches', 'original inputs unchanged',
                             'failed formation isolation', 'restart context persistence']}
        write_report(report, args.output)


if __name__ == '__main__':
    main()
