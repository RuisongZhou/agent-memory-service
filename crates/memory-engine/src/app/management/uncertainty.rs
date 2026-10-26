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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{engine, fresh_store, llm_calls, memory};
    use memory_store::store::to_doc;

    #[tokio::test(flavor = "multi_thread")]
    async fn resolving_a_question_keeps_the_chosen_record_and_supersedes_the_other() {
        let eng = engine();
        let (t, caller, dir) = fresh_store(&eng, "uncertainty", "agent").await;
        // A legacy conflict pair: the store's startup repair opens a question for it.
        let edge = |id: &str| RelEdge {
            id: id.into(),
            relation: "conflicts".into(),
        };
        let port = MemoryRec {
            id: "port".into(),
            mtype: T_FACT.into(),
            text: "Port is 8200".into(),
            status: S_CONFLICT.into(),
            scope: MemScope::project("project"),
            related: vec![edge("newport")],
            ..Default::default()
        };
        let new_port = MemoryRec {
            id: "newport".into(),
            text: "Port is 8100".into(),
            related: vec![edge("port")],
            ..port.clone()
        };
        let mut tx = Tx::new();
        for rec in [&port, &new_port] {
            tx.put(schema::MEMORIES, &rec.id, to_doc(rec).unwrap())
                .unwrap();
        }
        t.store.commit(tx).await.unwrap();
        t.store.repair_integrity().await.unwrap();
        let open: Vec<UncertaintyRec> = t
            .store
            .select_as(
                "SELECT * OMIT id FROM uncertainties WHERE status = 'open'",
                vec![],
            )
            .await
            .unwrap();
        assert_eq!(open.len(), 1);
        resolve_uncertainty(
            &t,
            &caller,
            ResolveUncertaintyReq {
                id: open[0].id.clone(),
                keep_memory_id: new_port.id.clone(),
                reason: "Verified the configured port".into(),
            },
        )
        .await
        .unwrap();
        assert_eq!(memory(&t, &new_port.id).await.status, S_CURRENT);
        assert_eq!(memory(&t, &port.id).await.status, S_SUPERSEDED);
        assert_eq!(
            t.store
                .count(schema::UNCERTAINTIES, Some("status = 'open'"))
                .await
                .unwrap(),
            0
        );
        assert_eq!(llm_calls(&eng), 0);
        drop(t);
        let _ = std::fs::remove_dir_all(dir);
    }
}
