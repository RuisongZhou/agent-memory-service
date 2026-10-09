"""Describe declared evaluation scope without turning pilot subsets into full results."""
import hashlib
import json

from . import locomo


def scope_for(answers_path, contexts, answered):
    config_path = answers_path.parent.parent / "run_config.json"
    config = json.loads(config_path.read_text()) if config_path.exists() else {}
    verified = all(k in config for k in ("convs", "sessions", "qids", "limit"))
    data = locomo.load()
    by_id = {c["sample_id"]: c for c in data}
    observed_convs = sorted({r["conv"] for r in contexts})
    convs = (config.get("convs") or sorted(by_id)) if verified else observed_convs
    if any(c not in by_id for c in convs):
        verified = False
    expected = []
    full_selected = []
    for cid in convs:
        if cid not in by_id:
            continue
        qs = locomo.qas(by_id[cid])
        full_selected.extend(q.qid for q in qs)
        if config.get("qids"):
            qs = [q for q in qs if q.qid in set(config["qids"])]
        if config.get("limit"):
            qs = qs[:config["limit"]]
        expected.extend(q.qid for q in qs)
    observed = {r["qid"] for r in contexts}
    expected_set = set(expected) if verified else observed
    answered_set = {r["qid"] for r in answered}
    session_limit = config.get("sessions") if verified else None
    question_subset = expected_set != set(full_selected)
    if not verified:
        kind = "scope-unverified"
    elif session_limit is not None:
        kind = "session-and-question-subset" if question_subset else "session-subset"
    elif question_subset:
        kind = "question-subset"
    else:
        kind = "full-dataset" if set(convs) == set(by_id) else "conversation-subset"
    signature = {"qids": sorted(expected_set), "session_limit": session_limit, "verified": verified}
    return {"kind": kind, "verified": verified, "conversations": convs,
            "session_limit": session_limit, "n_expected_questions": len(expected_set) if verified else None,
            "n_full_selected_questions": len(full_selected) if full_selected else None,
            "n_full_dataset_questions": sum(len(locomo.qas(c)) for c in data),
            "retrieval_complete": observed == expected_set if verified else None,
            "answering_complete": answered_set == expected_set,
            "missing_context_qids": sorted(expected_set - observed),
            "missing_answer_qids": sorted(expected_set - answered_set),
            "qid_cohort_hash": hashlib.sha256(json.dumps(signature, sort_keys=True).encode()).hexdigest()}
