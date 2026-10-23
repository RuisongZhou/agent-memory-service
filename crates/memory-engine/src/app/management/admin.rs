//! Admin inspection: `stats` and `export`.
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::runtime::engine::Engine;
use crate::runtime::indexer;
use crate::runtime::instance::Instance;
use memory_domain::types::*;
use memory_store::schema;

pub async fn stats(eng: &Arc<Engine>, t: &Arc<Instance>) -> ApiResult<Value> {
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
    let vm = t.vecs.read().await.memories.len();
    let gap_searches = t.store.gap_search_count().await?;
    Ok(json!({
        "counts": counts,
        "memories": by,
        "llm": {"process": eng.llm.usage_snapshot(), "persisted": usage_sum, "rows": usage_rows.len()},
        "index": {"memoryVectors": vm, "pendingTasks": indexer::pending(t).await?},
        "retrieval": {"gapSearches": gap_searches},
        "jobs": {"pendingJobs": t.store.count(schema::REQUESTS, Some("status IN ['queued','running','indexing']")).await?},
    }))
}

/// All memories (debug). Optional filters: `status`, `type`, `project` (strings or arrays of strings).
pub async fn export(eng: &Arc<Engine>, t: &Arc<Instance>, body: Value) -> ApiResult<Value> {
    // Read-only extensions for request-level billing, proposal/discard, consolidation-run and import audits.
    if let Some(table) = body.get("table").and_then(Value::as_str) {
        if !(matches!(
            table,
            schema::LLM_USAGE
                | schema::LLM_RESPONSES
                | schema::EXTRACT_LOG
                | schema::RETRIEVAL_LOG
                | schema::PROPOSALS
                | schema::CONSOLIDATION_RUNS
                | schema::IMPORT_RUNS
                | schema::IMPORT_LEDGER
                | schema::IMPORT_EXTRACTIONS
                | schema::UNCERTAINTIES
                | schema::REQUESTS
                | schema::STORE_REVISIONS
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
