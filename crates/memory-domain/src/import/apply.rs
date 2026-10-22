//! Writing and rolling back imported records as pure plans (design §6.8, §6.9 and §6.16): the record an
//! accepted proposal becomes, the supersede of the record a changed verbatim unit replaces, and the logical
//! rollback of a written record. Changes to existing records are `FieldPatch`es that keep the values they
//! replaced, as consolidation's are. No I/O: the engine loads the records, keeps the import ledger and
//! writes each proposal in its own transaction; algorithm-private tables are the algorithm's.
use serde_json::{json, Value};

use super::ImportProposal;
use crate::consolidation::apply::FieldPatch;
use crate::types::*;

/// The item kind written as an explicit instruction; every other kind becomes an imported fact.
pub const INSTRUCTION: &str = "instruction";
/// `role` of the evidence row that quotes the unit an imported record came from.
pub const EVIDENCE_ROLE: &str = "import";

/// Whether `p` writes an instruction rather than a fact.
pub fn is_instruction(p: &ImportProposal) -> bool {
    p.item_kind == INSTRUCTION
}

/// Whether import wrote `m`: an imported fact, or an instruction a person accepted from an import.
pub fn is_import_record(m: &MemoryRec) -> bool {
    m.source.origin == ORIGIN_IMPORTED
        || (m.mtype == T_INSTRUCTION
            && m.detail
                .as_ref()
                .is_some_and(|detail| detail.get("import").is_some()))
}

/// `detail.import` of a record written from `p`: where it came from and how it was proposed.
fn provenance(p: &ImportProposal) -> Value {
    json!({
        "runId": p.run_id,
        "proposalId": p.proposal_id,
        "unit": p.unit,
        "host": p.host,
        "mode": p.mode,
        "itemKind": p.item_kind,
        "scopeHint": p.scope_hint,
        "evidence": p.evidence,
        "headingPath": p.heading_path,
        "unitHash": p.unit_hash,
        "fileHash": p.file_hash,
        "source": p.source,
        "responseLogId": p.response_log_id,
    })
}

/// The record an accepted proposal writes, with id `id`, at `now`.
///
/// - Facts, preferences and references become a `fact` with origin `imported`, scoped to the project
///   (a `global` scope hint is kept in the provenance only), stated when the source states it. The effective
///   time is unknown, as for Mem0 facts. `supersedes` is the proposal's: the record a changed verbatim unit
///   replaces.
/// - An instruction a person accepted becomes an explicit `instruction`, the project's unless the person
///   chose `global` (a `global` scope hint alone never widens it), in effect from now like any explicit
///   instruction; the source's own statement time stays in `detail.import.statedAt`.
pub fn imported_record(p: &ImportProposal, id: &str, now: &str, global: bool) -> MemoryRec {
    let mut detail = provenance(p);
    if is_instruction(p) {
        detail["statedAt"] = json!(p.stated_at);
        let scope = if global {
            MemScope::global()
        } else {
            MemScope::project(&p.project)
        };
        return MemoryRec {
            id: id.into(),
            mtype: T_INSTRUCTION.into(),
            text: p.text.clone(),
            scope,
            status: S_CURRENT.into(),
            effective_at: now.into(),
            recorded_at: now.into(),
            stated_at: now.into(),
            last_accessed_at: now.into(),
            source: Source {
                platform: p.host.clone(),
                turn_ids: vec![],
                origin: ORIGIN_EXPLICIT.into(),
            },
            detail: Some(json!({ "import": detail })),
            ..Default::default()
        };
    }
    MemoryRec {
        id: id.into(),
        mtype: T_FACT.into(),
        text: p.text.clone(),
        scope: MemScope::project(&p.project),
        status: S_CURRENT.into(),
        effective_at: String::new(),
        recorded_at: now.into(),
        stated_at: p.stated_at.clone(),
        last_accessed_at: now.into(),
        source: Source {
            platform: p.host.clone(),
            turn_ids: vec![],
            origin: ORIGIN_IMPORTED.into(),
        },
        supersedes: p.supersedes.clone(),
        detail: Some(json!({ "import": detail })),
        ..Default::default()
    }
}

/// Retire `old`, the record a changed unit was imported as, for `new`: the record its new text became, or
/// the current record of the same text it is linked to. `old` becomes `superseded`, linked to `new`, valid
/// until `new` was stated (`now` when that is unknown). Only a current record import wrote, of the same type
/// as `new`, is replaced; one a person corrected, a consolidation run merged or a question disputes is left
/// alone (`Err` is the reason, and `new` is then written without `supersedes`). Whether another unit still
/// holds `old` is the caller's check: this plan only sees the records.
pub fn supersede_patch(old: &MemoryRec, new: &MemoryRec, now: &str) -> Result<FieldPatch, String> {
    if !is_import_record(old) {
        return Err(format!(
            "{} is not an imported record (origin {})",
            old.id, old.source.origin
        ));
    }
    if old.mtype != new.mtype {
        return Err(format!(
            "{} has type {}, not {}",
            old.id, old.mtype, new.mtype
        ));
    }
    if old.status != S_CURRENT {
        return Err(match &old.superseded_by {
            Some(next) => format!("{} was replaced by {next}", old.id),
            None => format!("{} is {}", old.id, old.status),
        });
    }
    let until = if new.stated_at.is_empty() {
        now
    } else {
        new.stated_at.as_str()
    };
    Ok(FieldPatch::over(
        old,
        &[
            ("status", json!(S_SUPERSEDED)),
            ("supersededBy", json!(new.id)),
            ("validUntil", json!(until)),
        ],
    ))
}

/// The logical rollback of `rec`, a record that run `run_id` wrote with `expected_text`. `predecessor` is the
/// record that apply superseded for it, as stored now (`None` when apply replaced nothing, or when that record
/// was deleted since).
///
/// `rec` becomes `superseded`, marked `importRollback {runId, at}`, and `predecessor` returns to `current`
/// (without `supersededBy` and `validUntil`) while `rec` is still what supersedes it. Refused, with the reason,
/// when a person or a later run changed `rec` since apply: it is no longer current (corrected, merged,
/// disputed), something supersedes it, its text differs, or it now replaces another record.
pub fn rollback_patches(
    rec: &MemoryRec,
    expected_text: &str,
    run_id: &str,
    predecessor: Option<&MemoryRec>,
    now: &str,
) -> Result<Vec<FieldPatch>, String> {
    if rec.status != S_CURRENT {
        return Err(match &rec.superseded_by {
            Some(next) => format!("{} was replaced by {next}", rec.id),
            None => format!("{} is {}", rec.id, rec.status),
        });
    }
    if let Some(next) = &rec.superseded_by {
        return Err(format!("{} is superseded by {next}", rec.id));
    }
    if rec.text != expected_text {
        return Err(format!("{} text changed since apply", rec.id));
    }
    let replaced = predecessor.map(|m| m.id.as_str());
    if rec.supersedes.as_deref() != replaced {
        return Err(match &rec.supersedes {
            Some(other) => format!("{} now replaces {other}", rec.id),
            None => format!("{} no longer replaces {}", rec.id, replaced.unwrap_or("")),
        });
    }
    let mut patches = vec![FieldPatch::over(
        rec,
        &[
            ("status", json!(S_SUPERSEDED)),
            ("importRollback", json!({"runId": run_id, "at": now})),
        ],
    )];
    patches.extend(predecessor.and_then(|previous| restore_patch(previous, &rec.id)));
    Ok(patches)
}

/// The logical rollback of `rec`, a fact formed from a session turn that run `run_id` imported, when every
/// request it was formed from is the run's (the caller's check: this plan only sees the record). `rec` becomes
/// `superseded`, marked `importRollback {runId, at}`, as an imported record does. Refused, with the reason, once
/// it is no longer the current fact formation wrote: corrected, merged, disputed, deleted or replaced since.
pub fn turn_fact_rollback_patch(
    rec: &MemoryRec,
    run_id: &str,
    now: &str,
) -> Result<FieldPatch, String> {
    if rec.status != S_CURRENT {
        return Err(match &rec.superseded_by {
            Some(next) => format!("{} was replaced by {next}", rec.id),
            None => format!("{} is {}", rec.id, rec.status),
        });
    }
    if let Some(next) = &rec.superseded_by {
        return Err(format!("{} is superseded by {next}", rec.id));
    }
    if rec.mtype != T_FACT {
        return Err(format!("{} is a {}, not a formed fact", rec.id, rec.mtype));
    }
    Ok(FieldPatch::over(
        rec,
        &[
            ("status", json!(S_SUPERSEDED)),
            ("importRollback", json!({"runId": run_id, "at": now})),
        ],
    ))
}

/// Return `previous` to `current` (without `supersededBy` and `validUntil`) while `successor` is still what
/// supersedes it; `None` once a person or a later run changed that.
pub fn restore_patch(previous: &MemoryRec, successor: &str) -> Option<FieldPatch> {
    (previous.status == S_SUPERSEDED && previous.superseded_by.as_deref() == Some(successor)).then(
        || {
            FieldPatch::over(
                previous,
                &[
                    ("status", json!(S_CURRENT)),
                    ("supersededBy", Value::Null),
                    ("validUntil", Value::Null),
                ],
            )
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::{default_decision, KIND, MODE_VERBATIM, PROPOSED};

    const NOW: &str = "2026-10-22T10:00:00Z";

    fn proposal(kind: &str, scope_hint: &str) -> ImportProposal {
        ImportProposal {
            proposal_id: "ip1".into(),
            run_id: "ir1".into(),
            kind: KIND.into(),
            project: "proj".into(),
            host: "claude-code".into(),
            unit: "claude-code:memory%2Fdeploy.md#deploy".into(),
            mode: MODE_VERBATIM.into(),
            item_kind: kind.into(),
            scope_hint: scope_hint.into(),
            text: "Deploys of proj go through the release job.".into(),
            evidence: "Deploys go through the release job.".into(),
            stated_at: "2026-05-03T09:00:00Z".into(),
            heading_path: vec!["Deploy".into()],
            unit_hash: "uh".into(),
            file_hash: "fh".into(),
            source: json!({"path": "memory/deploy.md"}),
            status: PROPOSED.into(),
            default_decision: default_decision(kind).into(),
            created_at: NOW.into(),
            ..Default::default()
        }
    }

    fn imported(id: &str, text: &str) -> MemoryRec {
        imported_record(
            &ImportProposal {
                text: text.into(),
                ..proposal("fact", "project")
            },
            id,
            NOW,
            false,
        )
    }

    /// `m` after `patch` is written: null clears a field.
    fn patched(m: &MemoryRec, patch: &FieldPatch) -> MemoryRec {
        let mut doc = serde_json::to_value(m).unwrap();
        for (k, v) in &patch.after {
            if v.is_null() {
                doc.as_object_mut().unwrap().remove(k);
            } else {
                doc[k] = v.clone();
            }
        }
        serde_json::from_value(doc).unwrap()
    }

    #[test]
    fn facts_preferences_and_references_become_imported_project_facts() {
        for kind in ["fact", "preference", "reference"] {
            let mut p = proposal(kind, "global");
            p.supersedes = Some("mold".into());
            let m = imported_record(&p, "mnew", NOW, true);
            assert_eq!(m.mtype, T_FACT, "{kind}");
            assert_eq!(m.source.origin, ORIGIN_IMPORTED);
            assert_eq!(m.source.platform, "claude-code");
            assert!(m.source.turn_ids.is_empty());
            // A global hint is provenance only: facts stay in the project.
            assert_eq!(m.scope, MemScope::project("proj"));
            assert_eq!(m.status, S_CURRENT);
            assert_eq!(m.stated_at, "2026-05-03T09:00:00Z");
            assert!(m.effective_at.is_empty());
            assert_eq!(
                (m.recorded_at.as_str(), m.last_accessed_at.as_str()),
                (NOW, NOW)
            );
            assert_eq!(m.supersedes.as_deref(), Some("mold"));
            let detail = &m.detail.as_ref().unwrap()["import"];
            assert_eq!(detail["runId"], "ir1");
            assert_eq!(detail["proposalId"], "ip1");
            assert_eq!(detail["itemKind"], kind);
            assert_eq!(detail["scopeHint"], "global");
            assert_eq!(detail["headingPath"], json!(["Deploy"]));
            assert_eq!(detail["source"]["path"], "memory/deploy.md");
            assert!(detail.get("statedAt").is_none());
        }
    }

    #[test]
    fn accepted_instructions_are_explicit_and_take_effect_now() {
        let m = imported_record(&proposal(INSTRUCTION, "project"), "mi", NOW, false);
        assert_eq!(m.mtype, T_INSTRUCTION);
        assert_eq!(m.source.origin, ORIGIN_EXPLICIT);
        assert_eq!(m.scope, MemScope::project("proj"));
        assert_eq!((m.effective_at.as_str(), m.stated_at.as_str()), (NOW, NOW));
        assert_eq!(m.supersedes, None);
        // The note's own time is kept with the provenance.
        assert_eq!(
            m.detail.as_ref().unwrap()["import"]["statedAt"],
            "2026-05-03T09:00:00Z"
        );
        // A global hint alone keeps the instruction in the project; only a person's choice widens it.
        let hinted = imported_record(&proposal(INSTRUCTION, "global"), "mh", NOW, false);
        assert_eq!(hinted.scope, MemScope::project("proj"));
        let global = imported_record(&proposal(INSTRUCTION, "project"), "mg", NOW, true);
        assert_eq!(global.scope, MemScope::global());
        assert!(is_import_record(&m) && is_import_record(&global));
    }

    #[test]
    fn instructions_supersede_instructions_only() {
        let old = imported_record(&proposal(INSTRUCTION, "project"), "mi1", NOW, false);
        let new = imported_record(&proposal(INSTRUCTION, "project"), "mi2", NOW, false);
        let patch = supersede_patch(&old, &new, NOW).unwrap();
        assert_eq!(patch.after["supersededBy"], "mi2");
        let fact = imported("mf", "Deploys use Nomad.");
        assert!(supersede_patch(&old, &fact, NOW)
            .unwrap_err()
            .contains("has type instruction, not fact"));
        // An explicit instruction a person wrote is not import's to replace.
        let typed = MemoryRec {
            detail: None,
            ..old.clone()
        };
        assert!(supersede_patch(&typed, &new, NOW)
            .unwrap_err()
            .contains("not an imported record"));
        // A supersede by a linked record can be undone while that record still supersedes it.
        let retired = patched(&old, &patch);
        assert!(restore_patch(&retired, "mi2").is_some());
        assert!(restore_patch(&retired, "mi9").is_none());
        assert!(restore_patch(&old, "mi2").is_none());
    }

    #[test]
    fn supersede_retires_only_a_current_imported_record() {
        let old = imported("mold", "Deploys use Ansible.");
        let new = imported("mnew", "Deploys use Nomad.");
        let patch = supersede_patch(&old, &new, NOW).unwrap();
        assert_eq!(patch.id, "mold");
        assert_eq!(patch.after["status"], S_SUPERSEDED);
        assert_eq!(patch.after["supersededBy"], "mnew");
        assert_eq!(patch.after["validUntil"], "2026-05-03T09:00:00Z");
        assert_eq!(patch.before["status"], S_CURRENT);
        assert!(patch.before["supersededBy"].is_null());
        // Without a statement time the replacement holds from now.
        let undated = MemoryRec {
            stated_at: String::new(),
            ..new.clone()
        };
        assert_eq!(
            supersede_patch(&old, &undated, NOW).unwrap().after["validUntil"],
            NOW
        );
        let user = MemoryRec {
            source: Source {
                origin: ORIGIN_USER.into(),
                ..old.source.clone()
            },
            ..old.clone()
        };
        assert!(supersede_patch(&user, &new, NOW)
            .unwrap_err()
            .contains("not an imported record"));
        let corrected = MemoryRec {
            status: S_SUPERSEDED.into(),
            superseded_by: Some("mfix".into()),
            ..old.clone()
        };
        assert!(supersede_patch(&corrected, &new, NOW)
            .unwrap_err()
            .contains("replaced by mfix"));
        let disputed = MemoryRec {
            status: S_CONFLICT.into(),
            ..old
        };
        assert!(supersede_patch(&disputed, &new, NOW)
            .unwrap_err()
            .contains("is conflict"));
    }

    #[test]
    fn rollback_retires_an_untouched_record() {
        let rec = imported("m1", "Deploys use Nomad.");
        let patches = rollback_patches(&rec, "Deploys use Nomad.", "ir1", None, NOW).unwrap();
        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0].id, "m1");
        assert_eq!(patches[0].after["status"], S_SUPERSEDED);
        assert_eq!(
            patches[0].after["importRollback"],
            json!({"runId": "ir1", "at": NOW})
        );
        assert!(patches[0].before["importRollback"].is_null());
        assert_eq!(patches[0].before["status"], S_CURRENT);
    }

    #[test]
    fn rollback_returns_the_replaced_record_to_current() {
        let old = imported("mold", "Deploys use Ansible.");
        let new = MemoryRec {
            supersedes: Some("mold".into()),
            ..imported("mnew", "Deploys use Nomad.")
        };
        let old = patched(&old, &supersede_patch(&old, &new, NOW).unwrap());
        let patches = rollback_patches(&new, "Deploys use Nomad.", "ir2", Some(&old), NOW).unwrap();
        assert_eq!(patches.len(), 2);
        let restored = patched(&old, &patches[1]);
        assert_eq!(restored.status, S_CURRENT);
        assert_eq!((restored.superseded_by, restored.valid_until), (None, None));
        // A predecessor that something else replaced meanwhile stays as it is.
        let elsewhere = MemoryRec {
            superseded_by: Some("mother".into()),
            ..old.clone()
        };
        assert_eq!(
            rollback_patches(&new, "Deploys use Nomad.", "ir2", Some(&elsewhere), NOW)
                .unwrap()
                .len(),
            1
        );
        // A predecessor deleted since: deletion cleared `supersedes`, and only the record itself is retired.
        let unlinked = MemoryRec {
            supersedes: None,
            ..new.clone()
        };
        assert_eq!(
            rollback_patches(&unlinked, "Deploys use Nomad.", "ir2", None, NOW)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn rollback_keeps_later_changes() {
        let rec = imported("m1", "Deploys use Nomad.");
        let corrected = MemoryRec {
            status: S_SUPERSEDED.into(),
            superseded_by: Some("mfix".into()),
            ..rec.clone()
        };
        assert!(
            rollback_patches(&corrected, "Deploys use Nomad.", "ir1", None, NOW)
                .unwrap_err()
                .contains("replaced by mfix")
        );
        let disputed = MemoryRec {
            status: S_CONFLICT.into(),
            ..rec.clone()
        };
        assert!(
            rollback_patches(&disputed, "Deploys use Nomad.", "ir1", None, NOW)
                .unwrap_err()
                .contains("is conflict")
        );
        let pointed = MemoryRec {
            superseded_by: Some("mnext".into()),
            ..rec.clone()
        };
        assert!(
            rollback_patches(&pointed, "Deploys use Nomad.", "ir1", None, NOW)
                .unwrap_err()
                .contains("superseded by mnext")
        );
        assert!(
            rollback_patches(&rec, "Deploys use Ansible.", "ir1", None, NOW)
                .unwrap_err()
                .contains("text changed")
        );
        // A later consolidation run made the record replace another one.
        let relinked = MemoryRec {
            supersedes: Some("mx".into()),
            ..rec
        };
        assert!(
            rollback_patches(&relinked, "Deploys use Nomad.", "ir1", None, NOW)
                .unwrap_err()
                .contains("now replaces mx")
        );
    }

    #[test]
    fn a_formed_fact_is_retired_unless_changed_since() {
        let fact = MemoryRec {
            id: "mf".into(),
            mtype: T_FACT.into(),
            text: "Releases go to the staging bucket.".into(),
            status: S_CURRENT.into(),
            source: Source {
                platform: "claude-code".into(),
                turn_ids: vec!["proj/s/p1".into()],
                origin: ORIGIN_USER.into(),
            },
            ..Default::default()
        };
        let patch = turn_fact_rollback_patch(&fact, "ir1", NOW).unwrap();
        assert_eq!(patch.after["status"], S_SUPERSEDED);
        assert_eq!(
            patch.after["importRollback"],
            json!({"runId": "ir1", "at": NOW})
        );
        assert_eq!(patch.before["status"], S_CURRENT);
        let merged = MemoryRec {
            status: S_SUPERSEDED.into(),
            superseded_by: Some("mmerged".into()),
            ..fact.clone()
        };
        assert!(turn_fact_rollback_patch(&merged, "ir1", NOW)
            .unwrap_err()
            .contains("replaced by mmerged"));
        let disputed = MemoryRec {
            status: S_CONFLICT.into(),
            ..fact.clone()
        };
        assert!(turn_fact_rollback_patch(&disputed, "ir1", NOW)
            .unwrap_err()
            .contains("is conflict"));
        let instruction = MemoryRec {
            mtype: T_INSTRUCTION.into(),
            ..fact
        };
        assert!(turn_fact_rollback_patch(&instruction, "ir1", NOW)
            .unwrap_err()
            .contains("not a formed fact"));
    }
}
