#!/usr/bin/env python3
"""Install evaluation dependencies and public tokenizers; no model or paid API calls."""
from pathlib import Path
import os
import subprocess
import sys
import venv

ROOT = Path(__file__).resolve().parent.parent

def main():
    venv.EnvBuilder(with_pip=True).create(ROOT / '.venv')
    py = ROOT / '.venv' / ('Scripts/python.exe' if os.name == 'nt' else 'bin/python')
    subprocess.run([str(py), '-m', 'pip', 'install', '-r', str(ROOT/'eval/requirements.txt')], check=True)
    subprocess.run([str(py), str(ROOT/'scripts/download_data.py')], check=True)
    subprocess.run([str(py), '-c', "import sys; sys.path.insert(0,'eval'); from common import env; import nltk,tiktoken; nltk.download('punkt_tab',download_dir=str(env.CACHE_DIR/'nltk_data'),raise_on_error=True); tiktoken.get_encoding('cl100k_base')"],cwd=ROOT, check=True)

if __name__ == '__main__':
    main()
