//! `rollback`: an applied run's writes are undone as logical inverses, newest proposal first, under the formation
//! lock; a record changed since apply stays, with the reason.
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

use super::{
    count, count_turns, interrupted, invalid, ledger, ledger_id, load_run, other_units_naming,
    run_proposals, turns, LedgerRow,
};
use crate::app::consolidation::write_patch;
use crate::runtime::engine::Engine;
use crate::runtime::instance::Instance;
use memory_domain::import::apply as plan;
use memory_domain::import::{
    is_session_turn, ImportProposal, ImportRun, ImportRunReq, APPLIED, ROLLED_BACK, RUN_APPLIED,
    RUN_APPLYING, RUN_PARTIALLY_ROLLED_BACK, RUN_ROLLED_BACK, RUN_ROLLING_BACK,
};
use memory_domain::types::*;
use memory_rules::util;
use memory_store::schema;
use memory_store::store::Tx;

/// Drop `memory_id` from the unit's ledger row while the row is this run's.
fn ledger_drop(
    tx: &mut Tx,
    id: &str,
    row: Option<(Value, LedgerRow)>,
    memory_id: &str,
    now: &str,
) -> anyhow::Result<()> {
    if let Some((_, mut row)) = row {
        row.memory_ids.retain(|m| m != memory_id);
        row.updated_at = now.into();
        tx.put(schema::IMPORT_LEDGER, id, serde_json::to_value(row)?)?;
    }
    Ok(())
}

/// The inverse of one applied proposal, added to `tx`, or the reason to leave it applied.
async fn undo(
    t: &Arc<Instance>,
    run: &ImportRun,
    p: &ImportProposal,
    tx: &mut Tx,
) -> anyhow::Result<Result<Value, String>> {
    let lid = ledger_id(&run.project, &p.unit);
    let row = ledger(t, &lid).await?;
    if let Some((_, row)) = &row {
        if row.run_id != run.run_id {
            return Ok(Err(format!(
                "the unit was imported again by run {}; roll that run back first",
                row.run_id
            )));
        }
    }
    let now = util::now_str();
    let applied = p.applied.clone().unwrap_or(Value::Null);
    let predecessor = match applied["superseded"]["id"].as_str() {
        Some(old) => t.store.get_memory(old).await?,
        None => None,
    };
    if let Some(target) = applied["dedupedInto"].as_str() {
        // The record the link replaced returns to current while that link is still what replaces it.
        let restored = predecessor
            .as_ref()
            .and_then(|old| plan::restore_patch(old, target));
        if let Some(patch) = &restored {
            write_patch(tx, patch)?;
        }
        ledger_drop(tx, &lid, row, target, &now)?;
        return Ok(Ok(json!({ "unlinked": target, "restored": restored })));
    }
    let Some(memory_id) = p.memory_id.as_deref() else {
        return Ok(Err("the proposal names no record".into()));
    };
    let Some(rec) = t.store.get_memory(memory_id).await? else {
        return Ok(Err(format!("{memory_id} was deleted")));
    };
    let others = other_units_naming(t, memory_id, &run.project, &p.unit).await?;
    if !others.is_empty() {
        return Ok(Err(format!(
            "{memory_id} is also imported by {}",
            others.join(", ")
        )));
    }
    let patches =
        match plan::rollback_patches(&rec, &p.text, &run.run_id, predecessor.as_ref(), &now) {
            Ok(patches) => patches,
            Err(reason) => return Ok(Err(reason)),
        };
    for patch in &patches {
        write_patch(tx, patch)?;
    }
    ledger_drop(tx, &lid, row, memory_id, &now)?;
    Ok(Ok(json!({ "patches": patches })))
}

/// Undo an applied run, newest proposal first, as logical inverses (design §6.16): a written record becomes
/// `superseded` with `importRollback`, the record it replaced returns to `current`, a linked duplicate is
/// unlinked, and each unit's ledger row returns to the row it replaced once none of the unit's proposals
/// stays applied. A proposal stays `applied` with `rollbackSkipReason` when its record was corrected,
/// merged, disputed or deleted since, when another run's ledger row also names the record, or when a later
/// run imported the unit again; the run is then `partially_rolled_back` and rollback can be retried. Session
/// turns are closed first (`turns`): the run admits none from then on, and each submitted turn's facts formed
/// only from the run's own requests are retired; a turn still forming stays applied until a retry.
pub async fn rollback(eng: &Arc<Engine>, t: &Arc<Instance>, req: ImportRunReq) -> ApiResult<Value> {
    let _guard = t.lock_formation().await;
    let id = req.run_id.as_str();
    let run = load_run(t, id).await?;
    if ![
        RUN_APPLIED,
        RUN_APPLYING,
        RUN_ROLLING_BACK,
        RUN_PARTIALLY_ROLLED_BACK,
    ]
    .contains(&run.status.as_str())
    {
        return Err(invalid(format!(
            "run {id} is {}; only an applied run can be rolled back",
            run.status
        )));
    }
    let now = util::now_str();
    {
        // From here on the run admits no more session turns; one admitted before is already a request.
        let _closed = t.close_import_turns().await;
        t.store
            .merge_doc(
                schema::IMPORT_RUNS,
                id,
                json!({"status": RUN_ROLLING_BACK, "rollbackStartedAt": now, "updatedAt": now, "error": null}),
            )
            .await?;
    }
    let mut proposals = run_proposals(t, id).await?;
    proposals.reverse();
    let owned = if proposals.iter().any(is_session_turn) {
        match turns::owned_requests(t, id).await {
            Ok(owned) => owned,
            Err(e) => return Err(interrupted(t, id, e).await),
        }
    } else {
        HashMap::new()
    };
    for p in proposals.iter().filter(|p| p.status == APPLIED) {
        let mut tx = Tx::new();
        let now = util::now_str();
        let undone = match is_session_turn(p) {
            true => turns::undo(eng, t, &run, p, &owned, &mut tx).await,
            false => undo(t, &run, p, &mut tx).await,
        };
        let bookkeeping = match undone {
            Ok(Ok(inverse)) => json!({"status": ROLLED_BACK, "rollback": inverse,
                "rolledBackAt": now, "rollbackSkipReason": null}),
            Ok(Err(reason)) => {
                tx = Tx::new();
                json!({"rollbackSkipReason": reason, "rollbackCheckedAt": now})
            }
            Err(e) => return Err(interrupted(t, id, e).await),
        };
        let committed = match tx.merge(schema::PROPOSALS, &p.proposal_id, bookkeeping) {
            Ok(_) => t.store.commit(tx).await,
            Err(e) => Err(e),
        };
        if let Err(e) = committed {
            return Err(interrupted(t, id, e).await);
        }
    }
    // Ledger rows of units none of whose proposals stays applied return to the rows they replaced.
    let proposals = run_proposals(t, id).await?;
    let mut tx = Tx::new();
    for unit in &run.units {
        let lid = ledger_id(&run.project, &unit.unit);
        let row = match ledger(t, &lid).await {
            Ok(row) => row,
            Err(e) => return Err(interrupted(t, id, e).await),
        };
        let Some((_, row)) = row.filter(|(_, row)| row.run_id == id) else {
            continue;
        };
        if proposals
            .iter()
            .any(|p| p.cluster == unit.index && p.status == APPLIED)
        {
            continue;
        }
        let restored = match row.previous {
            Some(previous) => tx.put(schema::IMPORT_LEDGER, &lid, previous).map(|_| ()),
            None => tx.delete(schema::IMPORT_LEDGER, &lid).map(|_| ()),
        };
        if let Err(e) = restored {
            return Err(interrupted(t, id, e).await);
        }
    }
    if let Err(e) = t.store.commit(tx).await {
        return Err(interrupted(t, id, e).await);
    }
    let remaining = count(&proposals, APPLIED);
    let mut counts = run.counts.clone();
    counts.rolled_back = count(&proposals, ROLLED_BACK);
    counts.turns_rolled_back = count_turns(&proposals, ROLLED_BACK);
    let remaining = remaining + count_turns(&proposals, APPLIED);
    let status = if remaining == 0 {
        RUN_ROLLED_BACK
    } else {
        RUN_PARTIALLY_ROLLED_BACK
    };
    let now = util::now_str();
    t.store
        .merge_doc(
            schema::IMPORT_RUNS,
            id,
            json!({"status": status, "rolledBackAt": now, "updatedAt": now, "counts": counts,
                "remainingApplied": remaining}),
        )
        .await?;
    t.wake_indexer();
    Ok(json!({"run": load_run(t, id).await?, "proposals": proposals}))
}
