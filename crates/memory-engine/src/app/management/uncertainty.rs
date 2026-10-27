//! Uncertainties between competing memories: listing them for review, and their explicit resolution.
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use super::{page_limit, with_ids, MAX_TEXT_BYTES};
use crate::runtime::instance::Instance;
use memory_domain::types::*;
use memory_rules::util;
use memory_store::schema;
use memory_store::store::{patch, safe_id, Tx};

const DEFAULT_LIST: usize = 50;
const MAX_LIST: usize = 200;

/// Questions of one status (open by default), newest first, as stored, each with its member records in
/// `memoryIds` order as `get_memory` shows them; a member that no longer exists is left out. Questions come
/// only from consolidation conflicts and the startup repair, so the list is truncated at `limit`, not paged.
pub async fn list_uncertainties(t: &Arc<Instance>, req: ListUncertaintiesReq) -> ApiResult<Value> {
    let limit = page_limit(req.limit, DEFAULT_LIST, MAX_LIST)?;
    let status = req.status.as_deref().unwrap_or("open");
    if !matches!(status, "open" | "resolved" | "withdrawn") {
        return Err(ApiError::InvalidArgument(
            "status must be open, resolved or withdrawn".into(),
        ));
    }
    let mut questions = with_ids(
        t.store
            .uncertainties(status, req.project.as_deref(), limit)
            .await?,
    )?;
    let member_ids = |q: &Value| -> Vec<String> {
        q["memoryIds"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|id| id.as_str().map(str::to_string))
            .collect()
    };
    let ids: BTreeSet<String> = questions.iter().flat_map(member_ids).collect();
    let records: HashMap<String, MemoryRec> = t
        .store
        .get_memories(&ids.into_iter().collect::<Vec<_>>())
        .await?
        .into_iter()
        .map(|m| (m.id.clone(), m))
        .collect();
    for q in &mut questions {
        let members: Vec<Value> = member_ids(q)
            .iter()
            .filter_map(|id| records.get(id))
            .map(MemoryRec::public_with_stated_at)
            .collect();
        q["memories"] = json!(members);
    }
    Ok(json!({ "uncertainties": questions }))
}

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

    /// An open question is listed with its members; resolving it keeps the chosen record, supersedes the other
    /// and lists the question as resolved, with the resolution.
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
        let list = |req: Value| list_uncertainties(&t, serde_json::from_value(req).unwrap());
        // Open by default, each question with its members in `memoryIds` order, as `get_memory` shows them.
        let open = list(json!({})).await.unwrap()["uncertainties"].clone();
        assert_eq!(open.as_array().unwrap().len(), 1, "{open}");
        let question = &open[0];
        let id = question["id"].as_str().unwrap().to_string();
        assert!(question.get("mid").is_none());
        assert_eq!(
            (&question["status"], &question["memoryIds"]),
            (&json!("open"), &json!(["newport", "port"]))
        );
        assert_eq!(
            question["scope"],
            json!({"kind": "project", "project": "project"})
        );
        let members: Vec<&Value> = question["memories"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| &m["id"])
            .collect();
        assert_eq!(members, ["newport", "port"]);
        assert_eq!(question["memories"][1]["text"], "Port is 8200");
        assert!(question["memories"][1].get("statedAt").is_some());
        for (filter, want) in [
            (json!({"project": "other"}), 0),
            (json!({"status": "resolved"}), 0),
            (json!({"project": "project", "limit": 1}), 1),
        ] {
            let found = list(filter.clone()).await.unwrap();
            assert_eq!(
                found["uncertainties"].as_array().unwrap().len(),
                want,
                "{filter}"
            );
        }
        for bad in [
            json!({"status": "closed"}),
            json!({"limit": 0}),
            json!({"limit": 201}),
        ] {
            assert!(
                matches!(list(bad.clone()).await, Err(ApiError::InvalidArgument(_))),
                "{bad}"
            );
        }
        resolve_uncertainty(
            &t,
            &caller,
            ResolveUncertaintyReq {
                id: id.clone(),
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
        // Resolved, with the resolution; a member that no longer exists is left out.
        let mut tx = Tx::new();
        tx.delete(schema::MEMORIES, &port.id).unwrap();
        t.store.commit(tx).await.unwrap();
        assert_eq!(list(json!({})).await.unwrap()["uncertainties"], json!([]));
        let resolved =
            list(json!({"status": "resolved"})).await.unwrap()["uncertainties"][0].clone();
        assert_eq!(resolved["id"], id);
        assert_eq!(
            resolved["resolution"],
            json!({"keepMemoryId": "newport", "platform": "agent", "reason": "Verified the configured port"})
        );
        assert!(resolved["resolvedAt"].is_string());
        let members: Vec<(&Value, &Value)> = resolved["memories"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| (&m["id"], &m["status"]))
            .collect();
        assert_eq!(members, [(&json!("newport"), &json!(S_CURRENT))]);
        assert_eq!(llm_calls(&eng), 0);
        drop(t);
        let _ = std::fs::remove_dir_all(dir);
    }
}
