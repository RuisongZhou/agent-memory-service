#!/usr/bin/env python3
"""Start the daemon with the local .env key and LLM settings; never echo them or execute .env shell commands."""
import argparse
import os
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parent.parent
DOTENV_KEYS = ('QWEN_API_KEY', 'QWEN_BASE_URL', 'LLM_MODEL', 'MEMORY_STRATEGY')

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--bin', type=Path, default=ROOT/'target/fast/agent-memoryd')
    parser.add_argument('--data-dir', type=Path, default=ROOT/'daemon-data')
    parser.add_argument('--embed-url', default='http://127.0.0.1:8081', help='llama-server embedding endpoint (scripts/llama_servers.sh)')
    parser.add_argument('--rerank-url', default='http://127.0.0.1:8082', help='llama-server rerank endpoint')
    parser.add_argument('--memory-strategy', choices=['mem0','current'])
    parser.add_argument('--config', type=Path)
    parser.add_argument('--port', type=int, default=8765)
    parser.add_argument('--mock', action='store_true')
    parser.add_argument('--max-calls', type=int, default=60, help='attempt guard, including retries; use 0 for unlimited after pilot approval')
    parser.add_argument('--llm-concurrency',type=int,default=1)
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
    else:
        env.pop('MEMORY_MOCK_MODELS', None)
    if args.max_calls < 0:
        parser.error('max-calls must be nonnegative')
    if args.max_calls:
        env['MEMORY_LLM_MAX_CALLS'] = str(args.max_calls)
    else:
        env.pop('MEMORY_LLM_MAX_CALLS', None)
    if args.llm_concurrency<1: parser.error('llm-concurrency must be positive')
    command = [str(args.bin.resolve()), 'serve', '--data-dir', str(args.data_dir), '--embed-url', args.embed_url,
               '--rerank-url', args.rerank_url,
               '--port', str(args.port),'--llm-concurrency',str(args.llm_concurrency)]
    # Let the daemon apply environment/config/default precedence when no CLI override is given.
    if args.memory_strategy:
        command += ['--memory-strategy', args.memory_strategy]
    if args.config:
        command += ['--config', str(args.config)]
    raise SystemExit(subprocess.call(command, env=env))

if __name__ == '__main__':
    main()
