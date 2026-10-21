#!/usr/bin/env python3
"""Start the daemon with the local .env keys, LLM and retrieval-model settings; never echo them or execute .env shell commands."""
import argparse
import os
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parent.parent
DOTENV_KEYS = ('QWEN_API_KEY', 'QWEN_BASE_URL', 'LLM_MODEL', 'MEMORY_STRATEGY', 'MEMORY_MODEL_PROVIDER',
               'MEMORY_MODEL_API_KEY', 'MEMORY_EMBED_URL', 'MEMORY_RERANK_URL', 'MEMORY_EMBED_MODEL',
               'MEMORY_RERANK_MODEL')

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--bin', type=Path, default=ROOT/'target/fast/agent-memoryd')
    parser.add_argument('--data-dir', type=Path, default=ROOT/'daemon-data')
    parser.add_argument('--model-provider', choices=['api', 'self-hosted'],
                        help='embedding/rerank source; the daemon defaults to api (docs/self-hosted-models.md for self-hosted)')
    parser.add_argument('--embed-url', help='embedding base URL; defaults depend on the model provider')
    parser.add_argument('--rerank-url', help='rerank base URL; defaults depend on the model provider')
    parser.add_argument('--memory-strategy', choices=['mem0'])
    parser.add_argument('--config', type=Path)
    parser.add_argument('--port', type=int, default=8765)
    parser.add_argument('--mock', action='store_true')
    parser.add_argument('--max-calls', type=int, default=60, help='attempt guard, including retries; use 0 for unlimited after pilot approval')
    parser.add_argument('--llm-concurrency',type=int,default=1)
    parser.add_argument('--outbox', nargs='?', const='', metavar='PATH',
                        help='also upload captured dialogue from this outbox (the default outbox without PATH)')
    args = parser.parse_args()
    env = dict(os.environ)
    if (ROOT/'.env').is_file():
        for line in (ROOT/'.env').read_text().splitlines():
            key, sep, value = line.strip().removeprefix('export ').partition('=')
            key = key.strip()
            if sep and key in DOTENV_KEYS and not env.get(key):
                env[key] = value.strip().strip('\"').strip("'")
    if args.mock:
        env['MEMORY_MOCK_MODELS'] = '1'
        env.pop('QWEN_API_KEY', None)
        env.pop('MEMORY_MODEL_API_KEY', None)
    else:
        env.pop('MEMORY_MOCK_MODELS', None)
    if args.max_calls < 0:
        parser.error('max-calls must be nonnegative')
    if args.max_calls:
        env['MEMORY_LLM_MAX_CALLS'] = str(args.max_calls)
    else:
        env.pop('MEMORY_LLM_MAX_CALLS', None)
    if args.llm_concurrency<1: parser.error('llm-concurrency must be positive')
    command = [str(args.bin.resolve()), 'serve', '--data-dir', str(args.data_dir),
               '--port', str(args.port),'--llm-concurrency',str(args.llm_concurrency)]
    # Let the daemon apply environment/config/default precedence when no CLI override is given.
    for flag, value in (('--model-provider', args.model_provider), ('--embed-url', args.embed_url),
                        ('--rerank-url', args.rerank_url)):
        if value:
            command += [flag, value]
    if args.memory_strategy:
        command += ['--memory-strategy', args.memory_strategy]
    if args.config:
        command += ['--config', str(args.config)]
    if args.outbox is not None:
        command += ['--outbox'] + ([str(Path(args.outbox).expanduser())] if args.outbox else [])
    raise SystemExit(subprocess.call(command, env=env))

if __name__ == '__main__':
    main()
