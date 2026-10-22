"""Process environment for every eval entry point.

Import this module first (before mem0 / nltk / tiktoken / fastembed): it pins caches to eval/.cache,
disables mem0 telemetry and loads QWEN_API_KEY from the project root .env when it is not
already exported. The key is never printed or written anywhere.
"""
from __future__ import annotations

import os
import warnings
from pathlib import Path

EVAL_DIR = Path(__file__).resolve().parent.parent
ROOT_DIR = EVAL_DIR.parent
RESULTS_DIR = EVAL_DIR / "results"
STORES_DIR = EVAL_DIR / "stores"
CACHE_DIR = EVAL_DIR / ".cache"
DATA_PATH = EVAL_DIR / "data" / "locomo10.json"
DOTENV_PATH = ROOT_DIR / ".env"

os.environ["MEM0_TELEMETRY"] = "False"
os.environ.setdefault("MEM0_DIR", str(CACHE_DIR / "mem0"))
os.environ.setdefault("NLTK_DATA", str(CACHE_DIR / "nltk_data"))
os.environ.setdefault("TIKTOKEN_CACHE_DIR", str(CACHE_DIR / "tiktoken"))
os.environ.setdefault("FASTEMBED_CACHE_PATH", str(CACHE_DIR / "fastembed"))
os.environ.setdefault("HF_HUB_DISABLE_TELEMETRY", "1")
os.environ.setdefault("TOKENIZERS_PARALLELISM", "false")
# en_core_web_sm 3.7.1 (the wheel reachable from this sandbox) under spaCy 3.8 emits W095 on load.
warnings.filterwarnings("ignore", message=r".*\[W095\].*")


def load_dotenv_key(name: str = "QWEN_API_KEY") -> None:
    """Populate os.environ[name] from ROOT/.env if unset (value never echoed)."""
    if os.environ.get(name) or not DOTENV_PATH.exists():
        return
    for line in DOTENV_PATH.read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        k, v = line.split("=", 1)
        k = k.strip()
        if k.startswith("export "):
            k = k[len("export "):].strip()
        if k == name:
            os.environ[name] = v.strip().strip('"').strip("'")
            return


for name in ("QWEN_API_KEY", "QWEN_BASE_URL", "LLM_MODEL", "EVAL_MODEL"):
    load_dotenv_key(name)
