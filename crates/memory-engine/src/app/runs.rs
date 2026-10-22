//! Steps shared by the run-based use cases, consolidation and import: reading a run, committing one op per
//! transaction with its proposal row, and recording an error on a run that was interrupted.
use serde_json::{json, Value};

use crate::runtime::instance::Instance;
use memory_domain::types::*;
use memory_rules::util;
use memory_store::schema;
use memory_store::store::{safe_id, Tx};

/// The run document `run_id` of `table`; `what` names the kind of run in the not-found error.
pub(crate) async fn load(t: &Instance, table: &str, what: &str, run_id: &str) -> ApiResult<Value> {
    safe_id(run_id).map_err(|_| ApiError::InvalidArgument("invalid runId".into()))?;
    t.store
        .get_doc(table, run_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("{what} run {run_id} not found")))
}

/// Commit one op's changes in `tx` together with the bookkeeping fields of its proposal row.
pub(crate) async fn commit_op(
    t: &Instance,
    mut tx: Tx,
    proposal_id: &str,
    bookkeeping: Value,
) -> anyhow::Result<()> {
    tx.merge(schema::PROPOSALS, proposal_id, bookkeeping)?;
    t.store.commit(tx).await
}

/// Record an error on the run before returning it; the run keeps its in-progress status and resumes.
pub(crate) async fn interrupted(
    t: &Instance,
    table: &str,
    run_id: &str,
    error: anyhow::Error,
) -> ApiError {
    let message = format!("{error:#}");
    if let Err(e) = t
        .store
        .merge_doc(
            table,
            run_id,
            json!({"error": message, "updatedAt": util::now_str()}),
        )
        .await
    {
        tracing::error!(run = run_id, table, "recording a run error: {e:#}");
    }
    ApiError::Internal(message)
}
