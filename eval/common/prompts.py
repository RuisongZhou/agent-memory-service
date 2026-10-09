"""Answer and judge prompts, taken verbatim from mem0's evaluation code.

The constants are read with `ast` (no code execution) from
  third_party/mem0_evaluation/prompts.py          -> ANSWER_PROMPT
  third_party/mem0_evaluation/metrics/llm_judge.py -> ACCURACY_PROMPT

Answer prompt variants:
- "unified" (default, all systems): ANSWER_PROMPT's text with its two per-speaker blocks
  ("Memories for user X: ... Memories for user Y: ...") replaced by one block
  "Memories:\n\n    {{memories}}"; everything else is byte-identical.
- "mem0-original": ANSWER_PROMPT itself (two speaker blocks, JSON-dumped lists, as in
  mem0's src/memzero/search.py) — mem0 reference run only.
Both are sent like mem0 does: a single system message, temperature 0.
"""
from __future__ import annotations

import ast
import json
import re
from functools import lru_cache
from pathlib import Path

from .env import EVAL_DIR

MEM0_EVAL_DIR = EVAL_DIR / "third_party" / "mem0_evaluation"


def _string_constant(path: Path, name: str) -> str:
    tree = ast.parse(path.read_text(encoding="utf-8"))
    for node in tree.body:
        if isinstance(node, ast.Assign) and any(isinstance(t, ast.Name) and t.id == name for t in node.targets):
            if isinstance(node.value, ast.Constant) and isinstance(node.value.value, str):
                return node.value.value
    raise KeyError(f"{name} not found in {path}")


@lru_cache(maxsize=None)
def answer_prompt_original() -> str:
    return _string_constant(MEM0_EVAL_DIR / "prompts.py", "ANSWER_PROMPT")


@lru_cache(maxsize=None)
def accuracy_prompt() -> str:
    return _string_constant(MEM0_EVAL_DIR / "metrics" / "llm_judge.py", "ACCURACY_PROMPT")


@lru_cache(maxsize=None)
def answer_prompt_unified() -> str:
    p = answer_prompt_original()
    start = p.index("    Memories for user {{speaker_1_user_id}}:")
    end = p.index("    Question: {{question}}")
    return p[:start] + "    Memories:\n\n    {{memories}}\n\n" + p[end:]


def _render(template: str, **values: str) -> str:
    # mem0 renders with jinja2.Template; for plain {{var}} substitutions without filters this is identical.
    return re.sub(r"\{\{(\w+)\}\}", lambda m: values.get(m.group(1), m.group(0)), template)


def render_unified(memories: str, question: str) -> str:
    return _render(answer_prompt_unified(), memories=memories, question=question)


def render_mem0_original(speaker_1: str, speaker_1_memories: list[str], speaker_2: str,
                         speaker_2_memories: list[str], question: str) -> str:
    return _render(
        answer_prompt_original(),
        speaker_1_user_id=speaker_1,
        speaker_2_user_id=speaker_2,
        speaker_1_memories=json.dumps(speaker_1_memories, indent=4),
        speaker_2_memories=json.dumps(speaker_2_memories, indent=4),
        question=question,
    )


def render_judge(question: str, gold_answer: str, generated_answer: str) -> str:
    return accuracy_prompt().format(question=question, gold_answer=gold_answer, generated_answer=generated_answer)
