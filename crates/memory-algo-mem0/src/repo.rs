//! Mem0 persistence: the input job queue lives in `requests` with `kind = 'mem0'`. All of this
//! algorithm's SurrealQL is kept here.
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;

use memory_domain::entities::{EntityDetail, EntityFilter, EntityPage, EntityRec, MemoryEntities};
use memory_engine::runtime::instance::Instance;
use memory_store::schema;
use memory_store::store::{conditions, from_doc};

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

/// Up to ten latest non-empty, durably formed inferred batches in the same conversation, newest first, with the
/// facts each formed. `formed` is committed with primary memories and sources, including successful extraction of
/// zero memories. Index publication need not finish before a batch supplies extraction context.
pub async fn previous_inputs(
    t: &Instance,
    platform: &str,
    project: &str,
    session: &str,
    seq: Value,
) -> Result<Vec<Value>> {
    t.store.select("SELECT inputMessages, inputSeq, memoryIds FROM requests WHERE kind = 'mem0' AND platform = $p AND project = $r AND session = $s AND infer = true AND formed = true AND status IN ['indexing', 'completed'] AND array::len(inputMessages) > 0 AND inputSeq < $seq ORDER BY inputSeq DESC LIMIT 10",
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
/// reads the authoritative table rather than eventual vector index.
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

/// An entity row as the web UI reads it: no vector, the memory count in place of the linked ids.
const ENTITY_LIST: &str =
    "SELECT mid, text, entityType, scope, array::len(linkedMemoryIds) AS memoryCount FROM mem0_entities";

/// When the entity projection was last written; `None` before it was ever built. Reads show the projection
/// as stored: bringing it up to date embeds new entity texts, which is search's and formation's work.
async fn projection_updated_at(t: &Instance) -> Result<Option<String>> {
    Ok(t.store
        .get_doc(super::ENTITY_STATE, "projection")
        .await?
        .and_then(|state| state["updatedAt"].as_str().map(str::to_string)))
}

fn entity_recs(rows: Vec<Value>) -> Result<Vec<EntityRec>> {
    rows.into_iter().map(from_doc).collect()
}

/// One page of entities, `memoryCount DESC, mid DESC` after the `(memoryCount, mid)` keyset `after`, with the
/// entity types of the filter's project.
pub async fn entity_page(
    t: &Instance,
    filter: &EntityFilter,
    after: Option<(i64, String)>,
    limit: usize,
) -> Result<EntityPage> {
    const IN_PROJECT: &str = "scope.project = $project";
    let project = || filter.project.as_ref().map(|v| json!(v));
    let (mut conds, mut binds) = conditions([
        (IN_PROJECT, "project", project()),
        (
            "entityType = $type",
            "type",
            filter.entity_type.as_ref().map(|v| json!(v)),
        ),
        (
            "(string::lowercase(text) CONTAINS $text OR normalizedText CONTAINS $text)",
            "text",
            filter.text.as_ref().map(|v| json!(v)),
        ),
    ]);
    if let Some((count, mid)) = after {
        conds.push(
            "(array::len(linkedMemoryIds) < $at \
              OR (array::len(linkedMemoryIds) = $at AND mid < $mid))",
        );
        binds.push(("at", json!(count)));
        binds.push(("mid", json!(mid)));
    }
    // One more row than the page tells whether another page follows.
    let sql = format!(
        "{ENTITY_LIST} WHERE {} ORDER BY memoryCount DESC, mid DESC LIMIT {}",
        conds.join(" AND "),
        limit + 1
    );
    let mut entities = entity_recs(t.store.select(&sql, binds).await?)?;
    let more = entities.len() > limit;
    entities.truncate(limit);
    let next = entities
        .last()
        .filter(|_| more)
        .map(|e| (e.memory_count as i64, e.id.clone()));
    let (conds, binds) = conditions([(IN_PROJECT, "project", project())]);
    let counted = t
        .store
        .select(
            &format!(
                "SELECT entityType AS name, count() AS n FROM mem0_entities WHERE {} GROUP BY name",
                conds.join(" AND ")
            ),
            binds,
        )
        .await?;
    let types: BTreeMap<String, i64> = counted
        .iter()
        .filter_map(|r| Some((r["name"].as_str()?.to_string(), r["n"].as_i64()?)))
        .collect();
    Ok(EntityPage {
        entities,
        types,
        next,
        projection_updated_at: projection_updated_at(t).await?,
    })
}

/// The entity `id` with the ids of the memories it links, `None` when there is no such entity.
pub async fn entity(t: &Instance, id: &str) -> Result<Option<EntityDetail>> {
    let Some(mut row) = t.store.get_doc(super::ENTITIES, id).await? else {
        return Ok(None);
    };
    let linked: Vec<String> =
        serde_json::from_value(row["linkedMemoryIds"].take()).context("linkedMemoryIds")?;
    let normalized_text = row["normalizedText"]
        .as_str()
        .context("normalizedText")?
        .to_string();
    row["memoryCount"] = json!(linked.len());
    Ok(Some(EntityDetail {
        entity: from_doc(row)?,
        normalized_text,
        linked_memory_ids: linked,
        projection_updated_at: projection_updated_at(t).await?,
    }))
}

/// The entities that link the memory `memory_id`, by text.
pub async fn memory_entities(t: &Instance, memory_id: &str) -> Result<MemoryEntities> {
    let sql =
        format!("{ENTITY_LIST} WHERE linkedMemoryIds CONTAINS $id ORDER BY text ASC, mid ASC");
    Ok(MemoryEntities {
        entities: entity_recs(t.store.select(&sql, vec![("id", json!(memory_id))]).await?)?,
        projection_updated_at: projection_updated_at(t).await?,
    })
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
