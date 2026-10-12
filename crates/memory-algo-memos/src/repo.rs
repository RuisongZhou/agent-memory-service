//! Memos persistence: every SurrealQL statement of this algorithm over the shared tables (memories,
//! turns, sessions, requests, evidence, uncertainties) is kept here.
use anyhow::Result;
use memory_rules::util;
use serde_json::{json, Value};

use memory_domain::types::*;
use memory_engine::runtime::tenant::Tenant;
use memory_store::schema;
use memory_store::store::{safe_id, Tx};

/// Which turns of the buffer a query covers.
pub enum TurnScope<'a> {
    All,
    Project(&'a str),
    Platform(&'a str),
}

// ------------------------------------------------------------------------------------- memories

/// Every current memory.
pub async fn current_memories(t: &Tenant) -> Result<Vec<MemoryRec>> {
    t.store.find_memories("status = 'current'", vec![]).await
}

/// Current facts.
pub async fn current_facts(t: &Tenant) -> Result<Vec<MemoryRec>> {
    t.store
        .find_memories("type = 'fact' AND status = 'current'", vec![])
        .await
}

/// Current facts scoped to a project.
pub async fn current_project_facts(t: &Tenant) -> Result<Vec<MemoryRec>> {
    t.store
        .find_memories(
            "type = 'fact' AND status = 'current' AND scope.kind = 'project'",
            vec![],
        )
        .await
}

/// Current profile memories.
pub async fn current_profiles(t: &Tenant) -> Result<Vec<MemoryRec>> {
    t.store
        .find_memories("type = 'profile' AND status = 'current'", vec![])
        .await
}

/// Current facts and experiences, the candidates for exact merging.
pub async fn current_facts_and_experiences(t: &Tenant) -> Result<Vec<MemoryRec>> {
    t.store
        .find_memories(
            "status = 'current' AND type IN ['fact','experience']",
            vec![],
        )
        .await
}

/// Current experiences.
pub async fn current_experiences(t: &Tenant) -> Result<Vec<MemoryRec>> {
    t.store
        .find_memories("type = 'experience' AND status = 'current'", vec![])
        .await
}

/// Skills that are current or still candidates.
pub async fn live_skills(t: &Tenant) -> Result<Vec<MemoryRec>> {
    t.store
        .find_memories(
            "type = 'skill' AND status IN ['current','candidate']",
            vec![],
        )
        .await
}

/// Queue clearing the embedding of memory `id` (it leaves the vector index when its status changes).
pub fn clear_embedding(tx: &mut Tx, id: &str) -> Result<()> {
    tx.clear_fields(schema::MEMORIES, id, &["embedding"])?;
    Ok(())
}

/// Queue `lastAccessedAt` / `lastAccessedTs` updates for the memories that were returned; ids that
/// are not valid record ids are skipped.
pub fn touch_memories(tx: &mut Tx, memory_ids: &[String], ts: &str) {
    for memory_id in memory_ids {
        if let Ok(id) = safe_id(memory_id) {
            tx.raw(
                &format!(
                    "UPDATE memories:{id} SET lastAccessedAt = $t, lastAccessedTs = $n RETURN NONE"
                ),
                vec![("t", json!(ts)), ("n", json!(util::ts_secs(ts)))],
            );
        }
    }
}

// ------------------------------------------------------------------------------------- evidence

/// Every evidence row.
pub async fn all_evidence(t: &Tenant) -> Result<Vec<EvidenceRec>> {
    t.store
        .select_as("SELECT * OMIT id FROM evidence", vec![])
        .await
}

/// Raw evidence documents that support memory `id`.
pub async fn evidence_docs_for_memory(t: &Tenant, id: &str) -> Result<Vec<Value>> {
    t.store
        .select(
            "SELECT * OMIT id FROM evidence WHERE memoryIds CONTAINS $id",
            vec![("id", json!(id))],
        )
        .await
}

// ------------------------------------------------------------------------------------- uncertainties

/// Open uncertainties that involve any of the memories `ids`.
pub async fn open_uncertainties_for(t: &Tenant, ids: &[String]) -> Result<Vec<UncertaintyRec>> {
    t.store.select_as(
        "SELECT * OMIT id FROM uncertainties WHERE status = 'open' AND memoryIds CONTAINSANY $ids",
        vec![("ids", json!(ids))]).await
}

// ------------------------------------------------------------------------------------- maintenance state

/// Stored profile conflict groups (`{pairs}` rows).
pub async fn profile_conflict_groups(t: &Tenant) -> Result<Vec<Value>> {
    t.store
        .select(
            "SELECT pairs FROM index_meta WHERE kind = 'profile_conflicts'",
            vec![],
        )
        .await
}

// ------------------------------------------------------------------------------------- sessions

/// Every session.
pub async fn all_sessions(t: &Tenant) -> Result<Vec<SessionRec>> {
    t.store
        .select_as("SELECT * OMIT id FROM sessions", vec![])
        .await
}

/// Sessions of one platform and project.
pub async fn sessions_of_project(
    t: &Tenant,
    platform: &str,
    project: &str,
) -> Result<Vec<SessionRec>> {
    t.store
        .select_as(
            "SELECT * OMIT id FROM sessions WHERE platform = $p AND project = $r",
            vec![("p", json!(platform)), ("r", json!(project))],
        )
        .await
}

// ------------------------------------------------------------------------------------- turns

/// Turns whose facts were extracted (candidates for the buffer purge).
pub async fn extracted_turns(t: &Tenant) -> Result<Vec<TurnRec>> {
    t.store
        .select_as(
            "SELECT * OMIT id, embedding, chunkEmbeddings FROM turns WHERE extractState = 'done'",
            vec![],
        )
        .await
}

/// Number of turns waiting for fact extraction.
pub async fn pending_turn_count(t: &Tenant) -> Result<i64> {
    t.store
        .count(schema::TURNS, Some("extractState = 'pending'"))
        .await
}

/// Number of turns whose extraction failed.
pub async fn failed_turn_count(t: &Tenant) -> Result<i64> {
    t.store
        .count(schema::TURNS, Some("extractState = 'failed'"))
        .await
}

/// Turns inside `scope`, optionally narrowed to those started in `[start, end)` (unix seconds).
pub async fn turns_in_scope(
    t: &Tenant,
    scope: TurnScope<'_>,
    started: Option<(i64, i64)>,
) -> Result<Vec<TurnRec>> {
    let (clause, mut binds): (&str, Vec<(&str, Value)>) = match scope {
        TurnScope::Project(p) => ("project = $p", vec![("p", json!(p))]),
        TurnScope::All => ("true", vec![]),
        TurnScope::Platform(pl) => ("platform = $pl", vec![("pl", json!(pl))]),
    };
    let mut sql = format!("SELECT * OMIT id, embedding, chunkEmbeddings FROM turns WHERE {clause}");
    if let Some((start, end)) = started {
        // time words narrow the candidates by startedAt
        sql.push_str(" AND startedTs >= $ws AND startedTs < $we");
        binds.push(("ws", json!(start)));
        binds.push(("we", json!(end)));
    }
    t.store.select_as(&sql, binds).await
}

/// The turns with the given ids, oldest first; `scope` and `session` narrow them further.
pub async fn turns_by_id(
    t: &Tenant,
    ids: &[String],
    scope: TurnScope<'_>,
    session: Option<&str>,
) -> Result<Vec<TurnRec>> {
    let mut sql =
        "SELECT * OMIT id, embedding, chunkEmbeddings FROM turns WHERE turnId IN $ids".to_string();
    let mut binds: Vec<(&str, Value)> = vec![("ids", json!(ids))];
    match scope {
        TurnScope::Project(p) => {
            sql.push_str(" AND project = $p");
            binds.push(("p", json!(p)));
        }
        TurnScope::Platform(pl) => {
            sql.push_str(" AND platform = $pl");
            binds.push(("pl", json!(pl)));
        }
        TurnScope::All => {}
    }
    if let Some(s) = session {
        sql.push_str(" AND session = $s");
        binds.push(("s", json!(s)));
    }
    sql.push_str(" ORDER BY startedTs ASC");
    t.store.select_as(&sql, binds).await
}

/// Turns inside `scope` (and `session`, and started at or after `since`, unix seconds), newest first.
pub async fn turns_newest_first(
    t: &Tenant,
    scope: TurnScope<'_>,
    session: Option<&str>,
    since: Option<i64>,
) -> Result<Vec<TurnRec>> {
    let mut conds: Vec<String> = vec!["true".into()];
    let mut binds: Vec<(&str, Value)> = vec![];
    match scope {
        TurnScope::Project(p) => {
            conds.push("project = $p".into());
            binds.push(("p", json!(p)));
        }
        TurnScope::Platform(pl) => {
            conds.push("platform = $pl".into());
            binds.push(("pl", json!(pl)));
        }
        TurnScope::All => {}
    }
    if let Some(s) = session {
        conds.push("session = $s".into());
        binds.push(("s", json!(s)));
    }
    if let Some(since) = since {
        conds.push("startedTs >= $since".into());
        binds.push(("since", json!(since)));
    }
    let sql = format!(
        "SELECT * OMIT id, embedding, chunkEmbeddings FROM turns WHERE {} ORDER BY startedTs DESC",
        conds.join(" AND ")
    );
    t.store.select_as(&sql, binds).await
}

// ------------------------------------------------------------------------------------- requests

/// The request id already created for the turn with record key `key`.
pub async fn request_id_for_turn_key(t: &Tenant, key: &str) -> Result<Option<String>> {
    let rows = t
        .store
        .select(
            "SELECT requestId FROM requests WHERE turnKeys CONTAINS $key LIMIT 1",
            vec![("key", json!(key))],
        )
        .await?;
    Ok(rows
        .first()
        .and_then(|r| r["requestId"].as_str())
        .map(str::to_string))
}

/// Text and session-end requests that are queued or running.
pub async fn unfinished_request_count(t: &Tenant) -> Result<i64> {
    t.store
        .count(
            schema::REQUESTS,
            Some("kind IN ['text','session_end'] AND status IN ['queued','running']"),
        )
        .await
}

/// Number of requests that exhausted their retries.
pub async fn failed_request_count(t: &Tenant) -> Result<i64> {
    t.store
        .count(schema::REQUESTS, Some("status = 'failed'"))
        .await
}

/// Text requests that are queued or running.
pub async fn unfinished_text_requests(t: &Tenant) -> Result<Vec<Value>> {
    t.store
        .select(
            "SELECT * OMIT id FROM requests WHERE kind = 'text' AND status IN ['queued','running']",
            vec![],
        )
        .await
}

/// `{requestId}` rows of the queued session-end requests of one session.
pub async fn queued_session_end_requests(
    t: &Tenant,
    platform: &str,
    session: &str,
) -> Result<Vec<Value>> {
    t.store.select("SELECT requestId FROM requests WHERE kind = 'session_end' AND platform = $p AND session = $s AND status = 'queued'",
        vec![("p",json!(platform)),("s",json!(session))]).await
}

/// Queue dropping the retry payload of text request `id` once its facts are stored.
pub fn clear_request_text(tx: &mut Tx, id: &str) -> Result<()> {
    tx.clear_fields(schema::REQUESTS, id, &["text"])?;
    Ok(())
}

/// Number of mem0 jobs that are queued, running or indexing (the other algorithm's work).
pub async fn foreign_unfinished_job_count(t: &Tenant) -> Result<i64> {
    t.store
        .count(
            schema::REQUESTS,
            Some("kind = 'mem0' AND status IN ['queued','running','indexing']"),
        )
        .await
}
