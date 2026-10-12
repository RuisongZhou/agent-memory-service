//! Mem0 persistence: the input job queue lives in `requests` with `kind = 'mem0'`. All of this
//! algorithm's SurrealQL is kept here.
use anyhow::Result;
use serde_json::{json, Value};

use crate::runtime::tenant::Tenant;
use memory_store::schema;

/// Raw-input filters for `list_inputs`; `None` leaves the dimension unconstrained.
pub struct InputFilter<'a> {
    pub project: Option<&'a str>,
    pub platform: Option<&'a str>,
    pub session: Option<&'a str>,
    pub turn_ids: Option<Vec<String>>,
}

/// The request id already created for the turn with record key `key`.
pub async fn request_id_for_turn_key(t: &Tenant, key: &str) -> Result<Option<String>> {
    let existing = t
        .store
        .select(
            "SELECT requestId FROM requests WHERE turnKeys CONTAINS $key LIMIT 1",
            vec![("key", json!(key))],
        )
        .await?;
    Ok(existing
        .first()
        .and_then(|v| v["requestId"].as_str())
        .map(str::to_string))
}

/// Queued, running and indexing jobs in input order.
pub async fn unfinished_jobs(t: &Tenant) -> Result<Vec<Value>> {
    t.store.select("SELECT * OMIT id FROM requests WHERE kind = 'mem0' AND status IN ['queued','running','indexing'] ORDER BY inputSeq ASC", vec![]).await
}

/// Number of queued, running and indexing jobs.
pub async fn unfinished_job_count(t: &Tenant) -> Result<i64> {
    t.store
        .count(
            schema::REQUESTS,
            Some("kind = 'mem0' AND status IN ['queued','running','indexing']"),
        )
        .await
}

/// Number of jobs that exhausted their retries.
pub async fn failed_job_count(t: &Tenant) -> Result<i64> {
    t.store
        .count(
            schema::REQUESTS,
            Some("kind = 'mem0' AND status = 'failed'"),
        )
        .await
}

/// Unfinished work that belongs to the other algorithm (pending extraction turns, memos requests).
pub async fn foreign_unfinished_job_count(t: &Tenant) -> Result<i64> {
    Ok(t.store
        .count(schema::TURNS, Some("extractState = 'pending'"))
        .await?
        + t.store
            .count(
                schema::REQUESTS,
                Some("kind IN ['text','session_end'] AND status IN ['queued','running']"),
            )
            .await?)
}

/// The ten latest non-empty inputs of the same platform and project before `seq`, newest first.
pub async fn previous_inputs(
    t: &Tenant,
    platform: &str,
    project: &str,
    seq: Value,
) -> Result<Vec<Value>> {
    t.store.select("SELECT inputMessages, inputSeq FROM requests WHERE kind = 'mem0' AND platform = $p AND project = $r AND array::len(inputMessages) > 0 AND inputSeq < $seq ORDER BY inputSeq DESC LIMIT 10",
        vec![("p",json!(platform)),("r",json!(project)),("seq",seq)]).await
}

/// Raw input jobs matching `filter`, in input order.
pub async fn list_inputs(t: &Tenant, filter: InputFilter<'_>) -> Result<Vec<Value>> {
    let mut sql = "SELECT * OMIT id FROM requests WHERE kind = 'mem0'".to_string();
    let mut binds = Vec::new();
    if let Some(project) = filter.project {
        sql.push_str(" AND project = $project");
        binds.push(("project", json!(project)));
    }
    if let Some(platform) = filter.platform {
        sql.push_str(" AND platform = $platform");
        binds.push(("platform", json!(platform)));
    }
    if let Some(session) = filter.session {
        sql.push_str(" AND session = $session_filter");
        binds.push(("session_filter", json!(session)));
    }
    if let Some(ids) = filter.turn_ids {
        sql.push_str(" AND turnId IN $ids");
        binds.push(("ids", json!(ids)));
    }
    sql.push_str(" ORDER BY inputSeq ASC");
    t.store.select(&sql, binds).await
}
