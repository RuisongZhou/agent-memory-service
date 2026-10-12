from common import prompts


def test_unified_prompt_only_replaces_memory_blocks():
    orig = prompts.answer_prompt_original()
    uni = prompts.answer_prompt_unified()
    head = orig[: orig.index("    Memories for user {{speaker_1_user_id}}:")]
    tail = orig[orig.index("    Question: {{question}}"):]
    assert uni == head + "    Memories:\n\n    {{memories}}\n\n" + tail
    assert "Carefully analyze all provided memories from both speakers" in uni
    assert "{{speaker_1" not in uni


def test_render_unified():
    p = prompts.render_unified("[2023-05-07] Caroline went to a support group.", "When did Caroline go?")
    assert "Memories:\n\n    [2023-05-07] Caroline went to a support group.\n\n    Question: When did Caroline go?" in p
    assert "{{" not in p


def test_render_mem0_original_matches_jinja_semantics():
    p = prompts.render_mem0_original("Caroline", ["1:56 pm on 8 May, 2023: x"], "Melanie", [], "Q?")
    assert 'Memories for user Caroline:\n\n    [\n    "1:56 pm on 8 May, 2023: x"\n]' in p
    assert "Memories for user Melanie:\n\n    []" in p
    assert p.rstrip().endswith("Question: Q?\n\n    Answer:")


def test_judge_prompt():
    p = prompts.render_judge("Q", "gold", "gen")
    assert "Question: Q\nGold answer: gold\nGenerated answer: gen" in p
    assert '"label"' in p and "json" in p.lower()


def test_mem0_custom_instructions_loaded():
    from common import mem0_adapter

    ci = mem0_adapter.mem0_custom_instructions()
    assert "Extract memories only from user messages" in ci
