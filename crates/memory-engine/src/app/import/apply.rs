//! `apply`: the accepted proposals of a proposed run become records (or links to the current record of the same
//! text), one proposal per transaction under the formation lock; declined ones are dropped. Every unit the run
//! proposed gets a ledger row of its live links, so proposing the same text again finds it unchanged. Accepted
//! session turns are only marked queued: the client submits them through the dialogue outbox (`turns`).
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::Arc;

use super::{
    count, count_turns, interrupted, invalid, ledger, ledger_id, ledger_write, load_run,
    other_units_naming, run_proposals, LedgerRow, DECLINED, LEDGER_APPLIED,
};
use crate::app::consolidation::write_patch;
use crate::app::runs;
use crate::runtime::instance::Instance;
use memory_domain::consolidation::apply::FieldPatch;
use memory_domain::import::apply::{self as plan, EVIDENCE_ROLE};
use memory_domain::import::{
    is_session_turn, ImportProposal, ImportRun, ImportRunReq, UnitOutcome, ACCEPT, APPLIED,
    DROPPED, MODE_SESSION_TURN, PROPOSED, REJECT, RUN_APPLIED, RUN_APPLYING, RUN_PROPOSED, SKIPPED,
    UNIT_CHANGED, UNIT_NEW, UNIT_RECONSIDERED,
};
use memory_domain::types::*;
use memory_rules::import::units;
use memory_rules::util;
use memory_store::schema;
use memory_store::store::{to_doc, Tx};

/// `ingestionMode` of every record an import writes.
const INGESTION_MODE: &str = "imported";

/// Why `p`'s unit may no longer be applied by `run_id`: another run imported it (or rolled its import
/// back) after this run was proposed. `None` while the row is what propose saw or this run's own.
fn ledger_moved(row: Option<&LedgerRow>, seen: &UnitOutcome, run_id: &str) -> Option<String> {
    if row.is_some_and(|row| row.run_id == run_id) {
        return None;
    }
    let now = row.map(|row| (row.run_id.as_str(), row.unit_hash.as_str()));
    let then = seen
        .ledger_run_id
        .as_deref()
        .zip(Some(seen.ledger_unit_hash.as_deref().unwrap_or("")));
    if now == then {
        return None;
    }
    Some(match row {
        Some(row) => format!(
            "the unit was imported by run {} after this run was proposed",
            row.run_id
        ),
        None => "the unit's import was rolled back after this run was proposed".into(),
    })
}

enum Accepted {
    Applied { memory_id: String, applied: Value },
    Skipped(String),
}

/// Retire `old_id`, the record the unit's earlier import was, for `new`, the record the unit holds now
/// (written by this proposal or linked to). Left alone while another unit still holds it or when the plan
/// refuses (a person or a later run changed it). Returns the patch written, or why not.
async fn supersede(
    t: &Arc<Instance>,
    p: &ImportProposal,
    old_id: &str,
    new: &MemoryRec,
    tx: &mut Tx,
    now: &str,
) -> anyhow::Result<Result<FieldPatch, String>> {
    let others = other_units_naming(t, old_id, &p.project, &p.unit).await?;
    if !others.is_empty() {
        return Ok(Err(format!(
            "{old_id} is also imported by {}",
            others.join(", ")
        )));
    }
    let patch = match t.store.get_memory(old_id).await? {
        Some(old) => plan::supersede_patch(&old, new, now),
        None => Err(format!("{old_id} was deleted")),
    };
    if let Ok(patch) = &patch {
        write_patch(tx, patch)?;
    }
    Ok(patch)
}

/// The writes of one accepted proposal, added to `tx`.
async fn accept(
    t: &Arc<Instance>,
    run: &ImportRun,
    p: &ImportProposal,
    tx: &mut Tx,
) -> anyhow::Result<Accepted> {
    let unit = run
        .units
        .iter()
        .find(|u| u.index == p.cluster)
        .ok_or_else(|| anyhow::anyhow!("proposal {} without its unit outcome", p.proposal_id))?;
    let lid = ledger_id(&run.project, &p.unit);
    let row = ledger(t, &lid).await?;
    if let Some(reason) = ledger_moved(row.as_ref().map(|(_, r)| r), unit, &run.run_id) {
        return Ok(Accepted::Skipped(reason));
    }
    let now = util::now_str();
    let key = units::import_key(&p.project, &p.text);
    // Exact deduplication: the unit is linked to the current record of the same text instead of writing
    // another one.
    let existing = t.store.current_imported(&p.project, &key).await?;
    let (mut target, written) = match existing {
        Some(m) => (m, false),
        None => (plan::imported_record(p, &util::new_id("m"), &now), true),
    };
    let mut applied = if written {
        json!({"memoryId": target.id, "importKey": key})
    } else {
        json!({"dedupedInto": target.id})
    };
    // The record the unit's earlier import was: replaced by the one it holds now, and no longer the unit's.
    let released = p.supersedes.as_deref().filter(|old| *old != target.id);
    let mut replaced = None;
    if let Some(old_id) = released {
        match supersede(t, p, old_id, &target, tx, &now).await? {
            Ok(patch) => {
                applied["superseded"] = serde_json::to_value(&patch)?;
                replaced = Some(old_id.to_string());
            }
            Err(reason) => applied["supersedeSkipped"] = json!(reason),
        }
    }
    if written {
        // A linked record keeps its own lineage; a written one names the record it replaced.
        target.supersedes = replaced;
        let mut doc = to_doc(&target)?;
        doc["importKey"] = json!(key);
        doc["contentHash"] = json!(units::content_hash(&target.text));
        doc["ingestionMode"] = json!(INGESTION_MODE);
        tx.put(schema::MEMORIES, &target.id, doc)?;
        tx.index_task(schema::MEMORIES, &target.id, "upsert")?;
        let evidence = EvidenceRec {
            id: util::new_id("e"),
            memory_ids: vec![target.id.clone()],
            platform: p.host.clone(),
            turn_id: p.unit.clone(),
            role: EVIDENCE_ROLE.into(),
            span: p.evidence.clone(),
            stated_at: p.stated_at.clone(),
            created_at: now.clone(),
            context: None,
        };
        tx.put(schema::EVIDENCE, &evidence.id, to_doc(&evidence)?)?;
    }
    ledger_write(
        t,
        tx,
        &lid,
        row,
        run,
        unit,
        Some(&target.id),
        released,
        LEDGER_APPLIED,
        &now,
    )
    .await?;
    Ok(Accepted::Applied {
        memory_id: target.id,
        applied,
    })
}

/// Apply a `proposed` run (or resume an interrupted apply). Each `proposed` proposal takes its review
/// decision, or `accept` without one: `reject` drops it; `accept` rechecks the unit's ledger row (another
/// run imported the unit since: `skipped`) and the project's current records (same text: the unit is linked
/// to that record), and otherwise writes the record, its evidence and index task. Either way the record the
/// unit's earlier import was (`supersedes`) is retired while no other unit holds it, and the unit's ledger row
/// and the proposal's bookkeeping commit in the same transaction. A unit whose proposals were all declined
/// gets a `declined` row, so proposing the same text again finds it unchanged unless `reconsider` is asked.
pub async fn apply(t: &Arc<Instance>, req: ImportRunReq) -> ApiResult<Value> {
    let id = req.run_id.as_str();
    load_run(t, id).await?;
    let ids: HashSet<String> = run_proposals(t, id)
        .await?
        .into_iter()
        .map(|p| p.proposal_id)
        .collect();
    for (pid, decision) in &req.decisions {
        if decision != ACCEPT && decision != REJECT {
            return Err(invalid(format!(
                "the decision for {pid} must be accept or reject, not {decision:?}"
            )));
        }
        if !ids.contains(pid) {
            return Err(invalid(format!("{pid} is not a proposal of run {id}")));
        }
    }
    let _guard = t.lock_formation().await;
    let run = load_run(t, id).await?;
    if run.status != RUN_PROPOSED && run.status != RUN_APPLYING {
        return Err(invalid(format!(
            "run {id} is {}; only a proposed run can be applied",
            run.status
        )));
    }
    let now = util::now_str();
    t.store
        .merge_doc(
            schema::IMPORT_RUNS,
            id,
            json!({"status": RUN_APPLYING, "applyStartedAt": now, "updatedAt": now, "error": null}),
        )
        .await?;
    for p in run_proposals(t, id).await? {
        if p.status != PROPOSED {
            continue;
        }
        let decision = req
            .decisions
            .get(&p.proposal_id)
            .map(String::as_str)
            .unwrap_or(ACCEPT);
        let mut tx = Tx::new();
        let bookkeeping = if decision == REJECT {
            json!({"status": DROPPED, "decision": REJECT})
        } else if is_session_turn(&p) {
            // Queued for the client to put into the dialogue outbox; formation writes its facts.
            json!({"status": APPLIED, "decision": ACCEPT, "applied": {"queued": true},
                "appliedAt": util::now_str()})
        } else {
            match accept(t, &run, &p, &mut tx).await {
                Ok(Accepted::Applied { memory_id, applied }) => json!({"status": APPLIED,
                    "decision": ACCEPT, "memoryId": memory_id, "applied": applied,
                    "appliedAt": util::now_str()}),
                Ok(Accepted::Skipped(reason)) => {
                    tx = Tx::new();
                    json!({"status": SKIPPED, "decision": ACCEPT, "skipReason": reason})
                }
                Err(e) => return Err(interrupted(t, id, e).await),
            }
        };
        if let Err(e) = runs::commit_op(t, tx, &p.proposal_id, bookkeeping).await {
            return Err(interrupted(t, id, e).await);
        }
    }
    // Units of which nothing was applied are recorded as declined, keeping their earlier records.
    let mut tx = Tx::new();
    let now = util::now_str();
    for unit in run.units.iter().filter(|u| {
        u.mode != MODE_SESSION_TURN
            && [UNIT_NEW, UNIT_CHANGED, UNIT_RECONSIDERED].contains(&u.outcome.as_str())
    }) {
        let lid = ledger_id(&run.project, &unit.unit);
        let row = match ledger(t, &lid).await {
            Ok(row) => row,
            Err(e) => return Err(interrupted(t, id, e).await),
        };
        let moved = ledger_moved(row.as_ref().map(|(_, r)| r), unit, id).is_some();
        if moved || row.as_ref().is_some_and(|(_, r)| r.run_id == id) {
            continue;
        }
        let written = ledger_write(
            t, &mut tx, &lid, row, &run, unit, None, None, DECLINED, &now,
        );
        if let Err(e) = written.await {
            return Err(interrupted(t, id, e).await);
        }
    }
    if let Err(e) = t.store.commit(tx).await {
        return Err(interrupted(t, id, e).await);
    }
    let proposals = run_proposals(t, id).await?;
    let mut counts = run.counts.clone();
    counts.applied = count(&proposals, APPLIED);
    counts.dropped = count(&proposals, DROPPED);
    counts.skipped = count(&proposals, SKIPPED);
    counts.turns_queued = count_turns(&proposals, APPLIED);
    let now = util::now_str();
    t.store
        .merge_doc(
            schema::IMPORT_RUNS,
            id,
            json!({"status": RUN_APPLIED, "appliedAt": now, "updatedAt": now, "counts": counts}),
        )
        .await?;
    t.wake_indexer();
    Ok(json!({"run": load_run(t, id).await?, "proposals": proposals}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seen(run: Option<&str>, hash: Option<&str>) -> UnitOutcome {
        UnitOutcome {
            ledger_run_id: run.map(str::to_string),
            ledger_unit_hash: hash.map(str::to_string),
            ..Default::default()
        }
    }

    fn row(run: &str, hash: &str) -> LedgerRow {
        LedgerRow {
            run_id: run.into(),
            unit_hash: hash.into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_unit_moves_when_another_run_imports_it_after_propose() {
        // Unchanged since propose, or already this run's own row.
        assert_eq!(
            ledger_moved(
                Some(&row("ir1", "h1")),
                &seen(Some("ir1"), Some("h1")),
                "ir2"
            ),
            None
        );
        assert_eq!(
            ledger_moved(Some(&row("ir2", "h2")), &seen(None, None), "ir2"),
            None
        );
        // Imported by another run, or rolled back, since propose.
        assert!(ledger_moved(
            Some(&row("ir3", "h3")),
            &seen(Some("ir1"), Some("h1")),
            "ir2"
        )
        .unwrap()
        .contains("ir3"));
        assert!(ledger_moved(None, &seen(Some("ir1"), Some("h1")), "ir2")
            .unwrap()
            .contains("rolled back"));
    }
}
