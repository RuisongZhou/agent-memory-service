#!/usr/bin/env python3
"""Start the daemon with the local .env key and LLM settings; never echo them or execute .env shell commands."""
import argparse
import os
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parent.parent
DOTENV_KEYS = ('QWEN_API_KEY', 'QWEN_BASE_URL', 'LLM_MODEL')

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--bin', type=Path, default=ROOT/'target/fast/agent-memoryd')
    parser.add_argument('--data-dir', type=Path, default=ROOT/'daemon-data')
    parser.add_argument('--models-dir', type=Path, default=ROOT/'models')
    parser.add_argument('--port', type=int, default=8765)
    parser.add_argument('--device', choices=['auto','cpu','metal'], default='auto')
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
    command = [str(args.bin.resolve()), 'serve', '--data-dir', str(args.data_dir), '--models-dir', str(args.models_dir),
               '--port', str(args.port),'--llm-concurrency',str(args.llm_concurrency),
               '--model-device',args.device]
    raise SystemExit(subprocess.call(command, env=env))

if __name__ == '__main__':
    main()
