//! Memory import runs (docs/design/memory-import.md), in the shape of consolidation runs: `propose` takes a plan's
//! units in batches of at most `MAX_BATCH_UNITS` and offers proposals without writing a memory record; `apply` and
//! `rollback` then change records one proposal per transaction under the formation lock, through the pure plans of
//! `memory_domain::import::apply`. The import ledger keeps one row per unit of a project: the run that last imported
//! it, its hash and the records it became, with the row it replaced kept for rollback. Session turns (`turns`) are
//! rows of the run too, but go through the algorithm's formation instead: they have no ledger rows, since the daemon
//! deduplicates turns by `platform + turnId`. This module holds the ledger and run reads the steps share.
//!
//! Import knows no algorithm and no algorithm knows import: notes are extracted here (`extract`) and written as
//! plain records, and the algorithm is asked only generic questions, the facts nearest to a text
//! (`MemoryAlgorithm::nearest_facts`) and the requests a fact was formed from (`MemoryAlgorithm::source_requests`).
mod apply;
mod extract;
mod propose;
mod rollback;
mod turns;

pub use apply::apply;
pub use propose::propose;
pub use rollback::rollback;
pub use turns::{proposal_id as turn_proposal_id, submit as submit_turn};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::runtime::instance::Instance;
use memory_domain::import::{
    is_session_turn, ImportProposal, ImportRun, ImportRunReq, ImportRunsReq, UnitOutcome, APPLIED,
    KIND,
};
use memory_domain::types::*;
use memory_rules::util;
use memory_store::schema;
use memory_store::store::{hashed_id, safe_id, Tx};

/// `import_runs` page size: default and largest.
const DEFAULT_RUNS: usize = 20;
const MAX_RUNS: usize = 200;

/// Ledger status of a unit whose last import wrote or linked a record. The other value, `DECLINED`, means
/// every proposal of that import was declined or rejected, or nothing was extracted: the text is recorded so a
/// rerun does not offer it again, unless proposing with `reconsider`.
const LEDGER_APPLIED: &str = APPLIED;
const DECLINED: &str = "declined";

/// One import ledger row: the latest import of `unit` into `project`. `memory_ids` are the unit's live links,
/// the current records that hold its content: records the unit was written as and records of the same text
/// it was linked to, carried forward from import to import until a later import of the unit supersedes them
/// or they stop being current.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LedgerRow {
    project: String,
    unit: String,
    host: String,
    unit_hash: String,
    file_hash: String,
    run_id: String,
    #[serde(default)]
    memory_ids: Vec<String>,
    status: String,
    updated_at: String,
    /// The row this one replaced, as stored; rollback puts it back.
    #[serde(default)]
    previous: Option<Value>,
}

fn ledger_id(project: &str, unit: &str) -> String {
    hashed_id("il", &format!("{project}\n{unit}"))
}

/// A ledger row as stored (for `previous`) and parsed.
async fn ledger(t: &Instance, id: &str) -> anyhow::Result<Option<(Value, LedgerRow)>> {
    match t.store.get_doc(schema::IMPORT_LEDGER, id).await? {
        Some(doc) => Ok(Some((doc.clone(), serde_json::from_value(doc)?))),
        None => Ok(None),
    }
}

/// The current records among `ids`, in their order.
async fn live_records(t: &Instance, ids: &[String]) -> anyhow::Result<Vec<MemoryRec>> {
    let mut found = t.store.get_memories(ids).await?;
    found.retain(|m| m.status == S_CURRENT);
    found.sort_by_key(|m| ids.iter().position(|id| *id == m.id));
    Ok(found)
}

/// The other units of `project` whose ledger rows hold `memory_id`: a record they hold is not one unit's to
/// replace or retire.
async fn other_units_naming(
    t: &Instance,
    memory_id: &str,
    project: &str,
    unit: &str,
) -> anyhow::Result<Vec<String>> {
    Ok(t.store
        .ledger_rows_naming(memory_id)
        .await?
        .iter()
        .filter(|row| row["project"] != project || row["unit"] != unit)
        .filter_map(|row| row["unit"].as_str().map(str::to_string))
        .collect())
}

/// Write the unit's ledger row after one of its proposals linked `linked` (when any) and released
/// `released` (the record it superseded or no longer holds). The first change a run makes to a unit starts a
/// row from the previous one's live links, keeping that row under `previous` for rollback; later changes of
/// the same run edit it. `status` is the row's status when the run starts it.
#[allow(clippy::too_many_arguments)]
async fn ledger_write(
    t: &Instance,
    tx: &mut Tx,
    id: &str,
    row: Option<(Value, LedgerRow)>,
    run: &ImportRun,
    unit: &UnitOutcome,
    linked: Option<&str>,
    released: Option<&str>,
    status: &str,
    now: &str,
) -> anyhow::Result<()> {
    let mut next = match row {
        Some((_, row)) if row.run_id == run.run_id => row,
        previous => {
            let carried = match &previous {
                Some((_, row)) => live_records(t, &row.memory_ids).await?,
                None => vec![],
            };
            LedgerRow {
                project: run.project.clone(),
                unit: unit.unit.clone(),
                host: run.host.clone(),
                unit_hash: unit.unit_hash.clone(),
                file_hash: unit.file_hash.clone(),
                run_id: run.run_id.clone(),
                memory_ids: carried.into_iter().map(|m| m.id).collect(),
                status: status.into(),
                updated_at: now.into(),
                previous: previous.map(|(doc, _)| doc),
            }
        }
    };
    if let Some(released) = released {
        next.memory_ids.retain(|m| m != released);
    }
    if let Some(linked) = linked {
        if !next.memory_ids.iter().any(|m| m == linked) {
            next.memory_ids.push(linked.into());
        }
        next.status = LEDGER_APPLIED.into();
    }
    next.updated_at = now.into();
    tx.put(schema::IMPORT_LEDGER, id, serde_json::to_value(next)?)?;
    Ok(())
}

fn invalid(message: impl Into<String>) -> ApiError {
    ApiError::InvalidArgument(message.into())
}

async fn load_run(t: &Instance, run_id: &str) -> ApiResult<ImportRun> {
    safe_id(run_id).map_err(|_| invalid("invalid runId"))?;
    let doc = t
        .store
        .get_doc(schema::IMPORT_RUNS, run_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("import run {run_id} not found")))?;
    Ok(serde_json::from_value(doc).map_err(anyhow::Error::from)?)
}

/// The run's proposals in proposal order: unit index, then position in the unit.
async fn run_proposals(t: &Instance, run_id: &str) -> ApiResult<Vec<ImportProposal>> {
    let mut proposals = Vec::new();
    for row in t.store.proposals_for_run(run_id).await? {
        if row["kind"] == KIND {
            proposals.push(serde_json::from_value(row).map_err(anyhow::Error::from)?);
        }
    }
    Ok(proposals)
}

/// Record an error on the run before returning it; the run keeps its in-progress status and resumes.
async fn interrupted(t: &Instance, run_id: &str, error: anyhow::Error) -> ApiError {
    let message = format!("{error:#}");
    if let Err(e) = t
        .store
        .merge_doc(
            schema::IMPORT_RUNS,
            run_id,
            json!({"error": message, "updatedAt": util::now_str()}),
        )
        .await
    {
        tracing::error!(run = run_id, "recording import error: {e:#}");
    }
    ApiError::Internal(message)
}

/// Proposals with `status`, session turns left out: they are counted on their own.
fn count(proposals: &[ImportProposal], status: &str) -> usize {
    proposals
        .iter()
        .filter(|p| p.status == status && !is_session_turn(p))
        .count()
}

/// Session turns with `status`.
fn count_turns(proposals: &[ImportProposal], status: &str) -> usize {
    proposals
        .iter()
        .filter(|p| p.status == status && is_session_turn(p))
        .count()
}

/// One run with its proposals.
pub async fn get_run(t: &Arc<Instance>, req: ImportRunReq) -> ApiResult<Value> {
    let run = load_run(t, &req.run_id).await?;
    let proposals = run_proposals(t, &req.run_id).await?;
    Ok(json!({"run": run, "proposals": proposals}))
}

/// Runs, newest first, optionally of one project (default 20, at most 200).
pub async fn list_runs(t: &Arc<Instance>, req: ImportRunsReq) -> ApiResult<Value> {
    let limit = req.limit.unwrap_or(DEFAULT_RUNS);
    if limit == 0 || limit > MAX_RUNS {
        return Err(invalid(format!("limit must be between 1 and {MAX_RUNS}")));
    }
    let project = req
        .project
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty() && *p != "*");
    let mut runs = Vec::new();
    for doc in t.store.import_runs(project, limit).await? {
        runs.push(serde_json::from_value::<ImportRun>(doc).map_err(anyhow::Error::from)?);
    }
    Ok(json!({ "runs": runs }))
}
