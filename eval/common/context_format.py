"""Pack retrieved records into an answer context under a token budget."""
from __future__ import annotations

from .metrics import count_tokens


def pack_blocks(blocks: list[str], budget: int) -> str:
    """Keep whole records in ranking order; count delimiters in the shared tokenizer."""
    kept = []
    for block in blocks:
        candidate = "\n\n".join([*kept, block])
        if count_tokens(candidate) <= budget:
            kept.append(block)
    return "\n\n".join(kept)
