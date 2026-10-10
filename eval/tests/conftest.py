import os
import sys
from pathlib import Path

EVAL = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(EVAL))
sys.path.insert(0, str(EVAL / "tests"))
os.environ.setdefault("QWEN_API_KEY", "offline-test-key")
