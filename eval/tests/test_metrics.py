"""Our F1 / BLEU-1 must equal mem0's own functions (loaded from third_party via ast, not imported:
the module pulls bert_score / sentence_transformers at import time)."""
import ast

import pytest

from common import metrics
from common.env import EVAL_DIR

MEM0_UTILS = EVAL_DIR / "third_party" / "mem0_evaluation" / "metrics" / "utils.py"


@pytest.fixture(scope="module")
def mem0_fns():
    tree = ast.parse(MEM0_UTILS.read_text())
    wanted = {"simple_tokenize", "calculate_bleu_scores", "calculate_metrics"}
    nodes = [n for n in tree.body if isinstance(n, ast.FunctionDef) and n.name in wanted]
    mod = ast.Module(body=nodes, type_ignores=[])
    ns = {}
    exec("import nltk\nfrom nltk.translate.bleu_score import SmoothingFunction, sentence_bleu\n"
         "from typing import Dict, List, Union\n", ns)
    exec(compile(mod, str(MEM0_UTILS), "exec"), ns)
    return ns


@pytest.mark.parametrize("pred,ref", [
    ("Caroline went to the LGBTQ support group on May 7, 2023.", "7 May 2023"),
    ("Adoption agencies", "Adoption agencies, LGBTQ+ counseling"),
    ("She painted a sunset!", "a sunset painting"),
    ("I don't know", "Transgender woman"),
])
def test_f1_and_bleu1_match_mem0(mem0_fns, pred, ref):
    assert metrics.f1_score(pred, ref) == pytest.approx(mem0_fns["calculate_metrics"](pred, ref)["f1"], abs=1e-12)
    assert metrics.bleu1(pred, ref) == pytest.approx(mem0_fns["calculate_bleu_scores"](pred, ref)["bleu1"], abs=1e-12)
