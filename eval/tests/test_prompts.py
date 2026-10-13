from common import prompts


def test_unified_prompt_only_replaces_memory_blocks():
    orig = prompts.answer_prompt_original()
    uni = prompts.answer_prompt_unified()
    head = orig[: orig.index("    Memories for user {{speaker_1_user_id}}:")]
    tail = orig[orig.index("    Question: {{question}}"):]
    assert uni == head + "    Memories:\n\n    {{memories}}\n\n" + tail


def test_render_mem0_original_matches_jinja_semantics():
    p = prompts.render_mem0_original("Caroline", ["1:56 pm on 8 May, 2023: x"], "Melanie", [], "Q?")
    assert 'Memories for user Caroline:\n\n    [\n    "1:56 pm on 8 May, 2023: x"\n]' in p
    assert "Memories for user Melanie:\n\n    []" in p
    assert p.rstrip().endswith("Question: Q?\n\n    Answer:")


def test_judge_prompt():
    p = prompts.render_judge("Q", "gold", "gen")
    assert "Question: Q\nGold answer: gold\nGenerated answer: gen" in p
    assert '"label"' in p and "json" in p.lower()
