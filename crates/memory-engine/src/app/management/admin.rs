//! Admin inspection (`stats`, `export`) and the test-only `seed` action.
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::runtime::engine::Engine;
use crate::runtime::indexer;
use crate::runtime::tenant::Tenant;
use memory_domain::types::*;
use memory_store::schema;
use memory_store::store::{safe_id, session_rid, to_doc, turn_rid, Tx};

pub async fn stats(eng: &Arc<Engine>, t: &Arc<Tenant>) -> ApiResult<Value> {
    let mut counts = Map::new();
    let owned = eng.algorithm().owned_tables();
    for tb in schema::SHARED_TABLES.iter().chain(owned) {
        counts.insert((*tb).into(), json!(t.store.count(tb, None).await?));
    }
    let rows = t.store.memory_counts_by_type_status().await?;
    let mut by: BTreeMap<String, BTreeMap<String, i64>> = BTreeMap::new();
    for r in rows {
        let ty = r
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string();
        let st = r
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string();
        let n = r.get("n").and_then(|v| v.as_i64()).unwrap_or(0);
        *by.entry(ty).or_default().entry(st).or_default() += n;
    }
    // persisted LLM usage rows: sum every numeric field (schema owned by the formation modules)
    let usage_rows = t.store.table_docs(schema::LLM_USAGE).await?;
    let mut usage_sum: BTreeMap<String, f64> = BTreeMap::new();
    for r in &usage_rows {
        if let Some(o) = r.as_object() {
            for (k, v) in o {
                if let Some(x) = v.as_f64() {
                    if !k.ends_with("Ts") && k != "createdTs2" {
                        *usage_sum.entry(k.clone()).or_default() += x;
                    }
                }
            }
        }
    }
    let (vm, vt, vc) = {
        let g = t.vecs.read().await;
        let chunks: usize = g.turn_chunks.values().map(|c| c.len()).sum();
        (g.memories.len(), g.turns.len(), chunks)
    };
    let gap_searches = t.store.gap_search_count().await?;
    Ok(json!({
        "tenant": t.name,
        "counts": counts,
        "memories": by,
        "llm": {"process": eng.llm.usage_snapshot(), "persisted": usage_sum, "rows": usage_rows.len()},
        "index": {"fulltextDocs": t.ft.num_docs(), "memoryVectors": vm, "turnVectors": vt, "turnChunkVectors": vc,
                  "pendingTasks": indexer::pending(t).await?},
        "retrieval": {"gapSearches": gap_searches},
        "jobs": indexer::stats_json(t),
    }))
}

/// All memories (debug). Optional filters: `status`, `type`, `project` (strings or arrays of strings).
pub async fn export(eng: &Arc<Engine>, t: &Arc<Tenant>, body: Value) -> ApiResult<Value> {
    // Read-only evaluation extensions for request-level billing and proposal/discard audits.
    if let Some(table) = body.get("table").and_then(Value::as_str) {
        if !(matches!(
            table,
            schema::LLM_USAGE
                | schema::LLM_RESPONSES
                | schema::EXTRACT_LOG
                | schema::RETRIEVAL_LOG
                | schema::PROPOSALS
                | schema::UNCERTAINTIES
                | schema::REQUESTS
        ) || eng.algorithm().owned_tables().contains(&table))
        {
            return Err(ApiError::InvalidArgument("unsupported export table".into()));
        }
        let records = t.store.table_docs(table).await?;
        return Ok(json!({"table":table,"count":records.len(),"records":records}));
    }
    let as_list = |k: &str| -> Option<Vec<String>> {
        match body.get(k) {
            Some(Value::String(s)) => Some(vec![s.clone()]),
            Some(Value::Array(a)) => Some(
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect(),
            ),
            _ => None,
        }
    };
    let mut conds = vec!["true".to_string()];
    let mut binds: Vec<(&str, Value)> = vec![];
    if let Some(s) = as_list("status") {
        conds.push("status IN $st".into());
        binds.push(("st", json!(s)));
    }
    if let Some(s) = as_list("type") {
        conds.push("type IN $ty".into());
        binds.push(("ty", json!(s)));
    }
    if let Some(s) = as_list("project") {
        conds.push("scope.project IN $pr".into());
        binds.push(("pr", json!(s)));
    }
    let clause = format!("{} ORDER BY recordedTs ASC", conds.join(" AND "));
    let mems = t.store.find_memories(&clause, binds).await?;
    Ok(json!({"count": mems.len(), "memories": mems}))
}

/// Test-only admin action: insert records directly (bypassing extraction) and index them synchronously.
/// Body: `{memories: [MemoryRec], turns: [TurnRec], sessions: [SessionRec], evidence: [EvidenceRec],
/// entities: [EntityRec]}` in the API's camelCase JSON.
pub async fn seed(eng: &Arc<Engine>, t: &Arc<Tenant>, body: Value) -> ApiResult<Value> {
    if !eng.models.is_mock() {
        return Err(ApiError::InvalidArgument(
            "seed is available only with mock models".into(),
        ));
    }
    let _guard = t.lock_formation().await;
    fn list<T: serde::de::DeserializeOwned>(body: &Value, k: &str) -> ApiResult<Vec<T>> {
        match body.get(k) {
            None | Some(Value::Null) => Ok(vec![]),
            Some(v) => serde_json::from_value(v.clone())
                .map_err(|e| ApiError::InvalidArgument(format!("{k}: {e}"))),
        }
    }
    let mems: Vec<MemoryRec> = list(&body, "memories")?;
    let turns: Vec<TurnRec> = list(&body, "turns")?;
    let sessions: Vec<SessionRec> = list(&body, "sessions")?;
    let evidence: Vec<EvidenceRec> = list(&body, "evidence")?;
    let entities: Vec<EntityRec> = list(&body, "entities")?;
    let mut tx = Tx::new();
    for m in &mems {
        let mut m = m.clone();
        if m.last_accessed_at.is_empty() {
            m.last_accessed_at = m.recorded_at.clone();
        }
        safe_id(&m.id)?;
        tx.put(schema::MEMORIES, &m.id, to_doc(&m)?)?;
        tx.index_task(schema::MEMORIES, &m.id, "upsert")?;
    }
    for tr in &turns {
        let rid = turn_rid(&tr.platform, &tr.turn_id);
        let mut d = to_doc(tr)?;
        d["mid"] = json!(rid);
        tx.put(schema::TURNS, &rid, d)?;
        tx.index_task(schema::TURNS, &rid, "upsert")?;
    }
    for s in &sessions {
        let rid = session_rid(&s.platform, &s.session);
        let mut d = to_doc(s)?;
        d["mid"] = json!(rid);
        tx.put(schema::SESSIONS, &rid, d)?;
    }
    for e in &evidence {
        tx.put(schema::EVIDENCE, &e.id, to_doc(e)?)?;
    }
    for e in &entities {
        tx.put(schema::ENTITIES, &e.id, to_doc(e)?)?;
    }
    t.store.commit(tx).await?;
    {
        let mut d = t.entities.write().await;
        for e in &entities {
            let mut names = vec![e.name.clone()];
            names.extend(e.aliases.iter().cloned());
            d.upsert(&e.id, &names);
        }
    }
    indexer::drain(eng, t).await?;
    // sanity: every seeded record can be read back
    let back: Vec<MemoryRec> = t
        .store
        .get_memories(&mems.iter().map(|m| m.id.clone()).collect::<Vec<_>>())
        .await?;
    Ok(json!({
        "memories": back.len(), "turns": turns.len(), "sessions": sessions.len(),
        "evidence": evidence.len(), "entities": entities.len(), "fulltextDocs": t.ft.num_docs(),
    }))
}
