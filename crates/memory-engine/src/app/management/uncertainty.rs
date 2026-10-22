//! Explicit resolution of an open uncertainty between competing memories.
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::Arc;

use super::MAX_TEXT_BYTES;
use crate::runtime::instance::Instance;
use memory_domain::types::*;
use memory_rules::util;
use memory_store::schema;
use memory_store::store::{patch, safe_id, Tx};

/// Explicit user confirmation closes the competing assertion; original texts/evidence remain.
pub async fn resolve_uncertainty(
    t: &Arc<Instance>,
    caller: &Caller,
    req: ResolveUncertaintyReq,
) -> ApiResult<Value> {
    let _guard = t.lock_formation().await;
    safe_id(&req.id).map_err(|_| ApiError::InvalidArgument("invalid uncertainty id".into()))?;
    if req.reason.len() > MAX_TEXT_BYTES {
        return Err(ApiError::PayloadTooLarge("reason exceeds 4 KB".into()));
    }
    let record = t
        .store
        .get_doc(schema::UNCERTAINTIES, &req.id)
        .await?
        .ok_or_else(|| ApiError::NotFound("uncertainty not found".into()))?;
    if record["status"] != "open" {
        return Err(ApiError::InvalidArgument(
            "uncertainty is already resolved".into(),
        ));
    }
    let uncertainty: UncertaintyRec = memory_store::store::from_doc(record)?;
    if !uncertainty.memory_ids.contains(&req.keep_memory_id) {
        return Err(ApiError::InvalidArgument(
            "keepMemoryId is not a competing assertion".into(),
        ));
    }
    let records = t.store.get_memories(&uncertainty.memory_ids).await?;
    if records.len() != uncertainty.memory_ids.len()
        || records
            .iter()
            .any(|m| m.status != S_CONFLICT || m.scope != uncertainty.scope)
    {
        return Err(ApiError::InvalidArgument(
            "competing assertions have changed; inspect current state".into(),
        ));
    }
    // A competing assertion may participate in other unresolved questions. Do not silently resolve
    // those questions by declaring it current; keep that assertion disputed until they close too.
    let others = t
        .store
        .other_open_uncertainties(&req.id, &req.keep_memory_id)
        .await?;
    let discarded: HashSet<_> = uncertainty
        .memory_ids
        .iter()
        .filter(|id| *id != &req.keep_memory_id)
        .collect();
    let has_other_conflict = others.iter().any(|u| {
        u["memoryIds"].as_array().is_some_and(|ids| {
            !ids.iter()
                .filter_map(Value::as_str)
                .any(|id| discarded.iter().any(|removed| removed.as_str() == id))
        })
    });
    let mut tx = Tx::new();
    let now = util::now_str();
    for memory in records {
        if memory.id == req.keep_memory_id {
            let related: Vec<_> = memory
                .related
                .iter()
                .filter(|edge| {
                    edge.relation != "conflicts"
                        || !discarded.iter().any(|id| id.as_str() == edge.id)
                })
                .collect();
            tx.merge(schema::MEMORIES, &memory.id, json!({"related":related}))?;
            if !has_other_conflict {
                tx.merge(schema::MEMORIES, &memory.id, json!({"status":S_CURRENT}))?;
            }
        } else {
            tx.merge(
                schema::MEMORIES,
                &memory.id,
                patch(json!({"status":S_SUPERSEDED,"validUntil":now})),
            )?;
            tx.clear_fields(schema::MEMORIES, &memory.id, &["embedding"])?;
        }
        tx.index_task(schema::MEMORIES, &memory.id, "upsert")?;
    }
    tx.merge(schema::UNCERTAINTIES, &req.id, json!({"status":"resolved","resolvedAt":now,
        "resolution":{"keepMemoryId":req.keep_memory_id,"platform":caller.platform,"reason":req.reason}}))?;
    t.store.commit(tx).await?;
    t.wake_indexer();
    Ok(json!({"id":req.id,"status":"resolved","keepMemoryId":req.keep_memory_id}))
}
