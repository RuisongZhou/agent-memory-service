"""Fixed offline message scenarios shared by Python/Node SDK comparisons."""
import json

def message(text, role="user"):
    return {"role": role, "content": text, "timestamp": "2026-10-14T00:00:00Z"}


def cases():
    long = "前🙂" * 1600 + "TAIL_FACT_完整尾部"
    return [
        {"id": "long", "steps": [("s", [message(long)], True),
                                     ("s", [message("PROBE_LONG")], True)]},
        {"id": "session", "steps": [("s1", [message("POISON_SESSION")], True),
                                        ("s2", [message("PROBE_SESSION")], True)]},
        {"id": "raw", "steps": [("s", [message("POISON_RAW")], False),
                                    ("s", [message("PROBE_RAW")], True)]},
        {"id": "empty", "steps": [("s", [message("ZERO_MEMORY_CONTEXT")], True),
                                      ("s", [message("PROBE_EMPTY")], True)]},
        {"id": "window", "steps": [("s", [message(f"WINDOW_{i:02}"),
                                                  message(f"WINDOW_{i+1:02}", "assistant")], True)
                                       for i in range(0, 14, 2)] +
                                      [("s", [message("PROBE_WINDOW")], True)]},
        {"id": "reference", "steps": [("s", [message("REFERENCE_BGE_M3")], True),
                                          ("s", [message("PROBE_REFERENCE")], True)]},
        {"id": "empty_window", "steps": [("s", [message("EXPIRED")] + [message("")] * 9 +
                                                        [message("LAST_NONEMPTY")], True),
                                             ("s", [message("PROBE_EMPTY_WINDOW")], True)]},
    ]


def section(prompt, title):
    marker = f"## {title}\n"
    return prompt.split(marker, 1)[1].split("\n\n## ", 1)[0]


def extract(prompt):
    new = section(prompt, "New Messages")
    history = section(prompt, "Last k Messages")
    texts = []
    if "TAIL_FACT_完整尾部" in new:
        texts.append("TAIL_FACT_完整尾部")
    for name in ("SESSION", "RAW"):
        if f"PROBE_{name}" in new and f"POISON_{name}" in history:
            texts.append(f"CONTAMINATED_{name}")
    if "PROBE_REFERENCE" in new and "REFERENCE_BGE_M3" in history:
        texts.append("Resolved reference: BGE-M3")
    return {"memory": [{"id": str(i), "text": text, "attributed_to": "user",
                         "linked_memory_ids": []} for i, text in enumerate(texts)]}
