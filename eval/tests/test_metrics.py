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
    assert {n.name for n in nodes} == wanted
    mod = ast.Module(body=nodes, type_ignores=[])
    ns = {}
    exec("import nltk\nfrom nltk.translate.bleu_score import SmoothingFunction, sentence_bleu\n"
         "from typing import Dict, List, Union\n", ns)
    exec(compile(mod, str(MEM0_UTILS), "exec"), ns)
    return ns


PAIRS = [
    ("7 May 2023", "7 May 2023"),
    ("Caroline went to the LGBTQ support group on May 7, 2023.", "7 May 2023"),
    ("The week before 9 June 2023", "The week before 9 June 2023"),
    ("Adoption agencies", "Adoption agencies, LGBTQ+ counseling"),
    ("She painted a sunset!", "a sunset painting"),
    ("Psychology, counseling certification", "Psychology, counseling certification"),
    ("I don't know", "Transgender woman"),
    ("", "something"),
    ("2022", "2022"),
    ("Yes, likely.", "Likely yes"),
    ("Running, pottery, camping, painting, swimming", "pottery, camping, painting, swimming"),
]


@pytest.mark.parametrize("pred,ref", PAIRS)
def test_f1_matches_mem0(mem0_fns, pred, ref):
    assert metrics.f1_score(pred, ref) == pytest.approx(mem0_fns["calculate_metrics"](pred, ref)["f1"], abs=1e-12)


@pytest.mark.parametrize("pred,ref", PAIRS)
def test_bleu1_matches_mem0(mem0_fns, pred, ref):
    assert metrics.bleu1(pred, ref) == pytest.approx(mem0_fns["calculate_bleu_scores"](pred, ref)["bleu1"], abs=1e-12)


def test_simple_tokenize_matches(mem0_fns):
    for p, r in PAIRS:
        assert metrics.simple_tokenize(p) == mem0_fns["simple_tokenize"](p)


def test_known_values():
    assert metrics.f1_score("7 May 2023", "7 May 2023") == 1.0
    assert metrics.f1_score("a b", "c d") == 0.0
    assert metrics.bleu1("7 May 2023", "7 May 2023") == pytest.approx(1.0)
    assert metrics.percentile([1, 2, 3, 4, 5], 95) == pytest.approx(4.8)
    assert metrics.count_tokens("hello world") == 2
