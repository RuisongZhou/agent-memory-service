"""F1 and BLEU-1 exactly as mem0's evaluation computes them (metrics/utils.py + evals.py).

evals.py:  metrics = calculate_metrics(pred, gt)       -> f1_score  = metrics["f1"]
           bleu    = calculate_bleu_scores(pred, gt)  -> bleu_score = bleu["bleu1"]
with pred = str(response), gt = str(answer). tests/test_metrics.py checks equality against mem0's own
functions loaded from third_party/mem0_evaluation/metrics/utils.py.
"""
from __future__ import annotations

import statistics
from typing import Iterable

from . import env  # noqa: F401  (NLTK_DATA)


def simple_tokenize(text) -> list[str]:
    text = str(text)
    return text.lower().replace(".", " ").replace(",", " ").replace("!", " ").replace("?", " ").split()


def f1_score(prediction, reference) -> float:
    """Token-set F1 from mem0's calculate_metrics (0 when either side is empty)."""
    if not prediction or not reference:
        return 0.0
    prediction = str(prediction).strip()
    reference = str(reference).strip()
    pred_tokens = set(simple_tokenize(prediction))
    ref_tokens = set(simple_tokenize(reference))
    common = pred_tokens & ref_tokens
    if not pred_tokens or not ref_tokens:
        return 0.0
    precision = len(common) / len(pred_tokens)
    recall = len(common) / len(ref_tokens)
    return 2 * precision * recall / (precision + recall) if (precision + recall) > 0 else 0.0


def bleu1(prediction, reference) -> float:
    """sentence_bleu with weights (1,0,0,0) and SmoothingFunction().method1 on nltk word tokens."""
    import nltk
    from nltk.translate.bleu_score import SmoothingFunction, sentence_bleu

    pred_tokens = nltk.word_tokenize(str(prediction).lower())
    ref_tokens = [nltk.word_tokenize(str(reference).lower())]
    try:
        return float(sentence_bleu(ref_tokens, pred_tokens, weights=(1, 0, 0, 0),
                                   smoothing_function=SmoothingFunction().method1))
    except Exception:
        return 0.0


def mean(xs: Iterable[float]) -> float | None:
    xs = list(xs)
    return statistics.mean(xs) if xs else None


def percentile(xs: Iterable[float], p: float) -> float | None:
    """Nearest-rank-free linear interpolation percentile (numpy 'linear' method)."""
    xs = sorted(xs)
    if not xs:
        return None
    if len(xs) == 1:
        return xs[0]
    k = (len(xs) - 1) * p / 100.0
    lo = int(k)
    hi = min(lo + 1, len(xs) - 1)
    return xs[lo] + (xs[hi] - xs[lo]) * (k - lo)


# ----------------------------------------------------------------------------- token counting

_ENC = None


def count_tokens(text: str) -> int:
    """Context size in tiktoken cl100k_base tokens (same counter for every system)."""
    global _ENC
    if _ENC is None:
        import tiktoken

        _ENC = tiktoken.get_encoding("cl100k_base")
    return len(_ENC.encode(text or "", disallowed_special=()))


TOKEN_COUNTER = "tiktoken/cl100k_base"
