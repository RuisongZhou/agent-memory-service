//! Mem0 persistence: the input job queue lives in `requests` with `kind = 'mem0'`. All of this
//! algorithm's SurrealQL is kept here.
use anyhow::Result;
use serde_json::{json, Value};

use memory_engine::runtime::instance::Instance;
use memory_store::schema;

/// Raw-input filters for `list_inputs`; `None` leaves the dimension unconstrained.
pub struct InputFilter<'a> {
    pub project: Option<&'a str>,
    pub platform: Option<&'a str>,
    pub session: Option<&'a str>,
    pub turn_ids: Option<Vec<String>>,
}

/// The request id already created for the turn with record key `key`.
pub async fn request_id_for_turn_key(t: &Instance, key: &str) -> Result<Option<String>> {
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
pub async fn unfinished_jobs(t: &Instance) -> Result<Vec<Value>> {
    t.store.select("SELECT * OMIT id FROM requests WHERE kind = 'mem0' AND status IN ['queued','running','indexing'] ORDER BY inputSeq ASC", vec![]).await
}

/// Number of queued, running and indexing jobs.
pub async fn unfinished_job_count(t: &Instance) -> Result<i64> {
    t.store
        .count(
            schema::REQUESTS,
            Some("kind = 'mem0' AND status IN ['queued','running','indexing']"),
        )
        .await
}

/// Number of jobs that exhausted their retries.
pub async fn failed_job_count(t: &Instance) -> Result<i64> {
    t.store
        .count(
            schema::REQUESTS,
            Some("kind = 'mem0' AND status = 'failed'"),
        )
        .await
}

/// Unfinished legacy work: reject these stores rather than silently abandoning their jobs.
pub async fn foreign_unfinished_job_count(t: &Instance) -> Result<i64> {
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

/// Up to ten latest non-empty, durably formed inferred batches in the same conversation.
/// Missing `infer` retains the legacy formation default of true. `formed` is committed
/// with primary memories and sources, including successful extraction of zero memories.
/// Index publication need not finish before a batch supplies extraction context.
pub async fn previous_inputs(
    t: &Instance,
    platform: &str,
    project: &str,
    session: &str,
    seq: Value,
) -> Result<Vec<Value>> {
    t.store.select("SELECT inputMessages, inputSeq FROM requests WHERE kind = 'mem0' AND platform = $p AND project = $r AND session = $s AND (infer = NONE OR infer != false) AND formed = true AND status IN ['indexing', 'completed'] AND array::len(inputMessages) > 0 AND inputSeq < $seq ORDER BY inputSeq DESC LIMIT 10",
        vec![("p",json!(platform)),("r",json!(project)),("s",json!(session)),("seq",seq)]).await
}

/// Raw input jobs matching `filter`, in input order.
pub async fn list_inputs(t: &Instance, filter: InputFilter<'_>) -> Result<Vec<Value>> {
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

/// Primary facts used to reconcile the rebuildable entity projection. This deliberately
/// reads the authoritative table rather than eventual vector/full-text indexes.
pub async fn entity_memory_inputs(t: &Instance) -> Result<Vec<Value>> {
    t.store.select("SELECT mid, text, type, scope, status, ingestionMode, supersedes, recordedAt, formationPosition FROM memories WHERE type = 'fact' AND status IN ['current','conflict'] ORDER BY mid ASC", vec![]).await
}

pub async fn entity_rows(t: &Instance) -> Result<Vec<Value>> {
    t.store
        .select(
            "SELECT * OMIT id FROM mem0_entities ORDER BY mid ASC",
            vec![],
        )
        .await
}

/// Content keys that currently deduplicate into `memory_id`.
pub async fn content_keys_of(t: &Instance, memory_id: &str) -> Result<Vec<String>> {
    Ok(t.store
        .select(
            "SELECT mid FROM mem0_hashes WHERE memoryId = $id ORDER BY mid ASC",
            vec![("id", json!(memory_id))],
        )
        .await?
        .iter()
        .filter_map(|row| row["mid"].as_str().map(str::to_string))
        .collect())
}

/// The input requests formation made the fact `memory_id` from (its source batch); `None` without one, as for
/// a record formation did not write.
pub async fn source_requests(t: &Instance, memory_id: &str) -> Result<Option<Vec<String>>> {
    let Some(source) = t.store.get_doc(super::SOURCES, memory_id).await? else {
        return Ok(None);
    };
    Ok(Some(serde_json::from_value(
        source["inputRequestIds"].clone(),
    )?))
}

/// Canonical entity rows without their vectors: `{mid, entityType, linkedMemoryIds}`.
pub async fn entity_links(t: &Instance) -> Result<Vec<Value>> {
    t.store
        .select(
            "SELECT mid, entityType, linkedMemoryIds FROM mem0_entities ORDER BY mid ASC",
            vec![],
        )
        .await
}

/// Extraction intermediates are rebuildable; a stored text/version must match the
/// authoritative fact before its extracted entities are reused.
pub async fn entity_extractions(t: &Instance) -> Result<Vec<Value>> {
    t.store
        .select("SELECT * OMIT id FROM mem0_entity_inputs", vec![])
        .await
}

pub async fn entity_memory_inputs_by_id(
    t: &Instance,
    ids: &std::collections::BTreeSet<String>,
) -> Result<Vec<Value>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let records = ids
        .iter()
        .map(|id| Ok(format!("memories:{}", memory_store::store::safe_id(id)?)))
        .collect::<Result<Vec<_>>>()?
        .join(",");
    t.store.select(&format!("SELECT mid, text, type, scope, status, ingestionMode, supersedes, recordedAt, formationPosition FROM {records} WHERE type = 'fact' AND status IN ['current','conflict'] ORDER BY mid ASC"), vec![]).await
}

#[cfg(test)]
pub async fn reject_entity_manifest(t: &Instance, reject: bool) -> Result<()> {
    if reject {
        t.store
            .query(
                "DEFINE FIELD version ON mem0_entity_state TYPE string ASSERT $value = 'reject'",
                vec![],
            )
            .await?;
    } else {
        t.store
            .query("REMOVE FIELD version ON mem0_entity_state", vec![])
            .await?;
    }
    Ok(())
}

#[cfg(test)]
pub fn purge_entity_projection(tx: &mut memory_store::store::Tx) {
    tx.raw("DELETE mem0_entities RETURN NONE", vec![]);
}
