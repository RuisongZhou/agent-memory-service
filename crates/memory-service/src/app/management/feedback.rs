//! `feedback` (5.5): helpful/unhelpful signals on a retrieval, keyed by its `retrievalId`.
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::Arc;

use crate::domain::types::*;
use crate::runtime::engine::Engine;
use crate::runtime::tenant::Tenant;
use crate::storage::schema;
use crate::storage::store::{safe_id, Tx};
use memory_rules::util;

/// Helpful feedback count at which a candidate skill is enabled (4.5 / 5.5).
const SKILL_ENABLE_HELPFUL: i64 = 3;
/// Feedback window (5.5).
const FEEDBACK_WINDOW_HOURS: i64 = 24;

pub async fn feedback(eng: &Arc<Engine>, t: &Arc<Tenant>, req: FeedbackReq) -> ApiResult<Value> {
    let _ = eng;
    let _guard = t.lock_formation().await;
    let outcome = req.outcome.trim().to_string();
    if !["helpful", "irrelevant", "wrong"].contains(&outcome.as_str()) {
        return Err(ApiError::InvalidArgument(format!(
            "outcome must be helpful|irrelevant|wrong, got {outcome:?}"
        )));
    }
    if safe_id(&req.retrieval_id).is_err() {
        return Err(ApiError::NotFound(format!(
            "retrieval {} not found",
            req.retrieval_id
        )));
    }
    let Some(log) = t
        .store
        .get_doc(schema::RETRIEVAL_LOG, &req.retrieval_id)
        .await?
    else {
        return Err(ApiError::NotFound(format!(
            "retrieval {} not found",
            req.retrieval_id
        )));
    };
    // one feedback per retrievalId, within 24 hours
    if log.get("feedback").map(|v| !v.is_null()).unwrap_or(false) {
        return Ok(
            json!({"accepted": false, "reason": "feedback already recorded for this retrievalId"}),
        );
    }
    let created = log
        .get("createdAt")
        .and_then(|v| v.as_str())
        .and_then(util::parse_ts);
    if let Some(cr) = created {
        if util::now() - cr > chrono::Duration::hours(FEEDBACK_WINDOW_HOURS) {
            return Ok(json!({"accepted": false, "reason": "feedback window (24 h) has passed"}));
        }
    }
    // OPEN: feedback applies only to memories that this retrieval actually returned
    let returned: HashSet<String> = log
        .pointer("/returned/memories")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.get("id").and_then(|i| i.as_str()).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let mut unique = HashSet::new();
    let applied: Vec<String> = req
        .used
        .iter()
        .filter(|u| returned.contains(*u) && unique.insert((*u).clone()))
        .cloned()
        .collect();
    let ignored: Vec<String> = req
        .used
        .iter()
        .filter(|u| !returned.contains(*u))
        .cloned()
        .collect();

    let mut tx = Tx::new();
    let mut changed: Vec<Value> = Vec::new();
    if outcome != "irrelevant" {
        for id in &applied {
            let Some(m) = (if safe_id(id).is_ok() {
                t.store.get_memory(id).await?
            } else {
                None
            }) else {
                continue;
            };
            match outcome.as_str() {
                // wrong → candidate (only the status changes, never the text or the scoring)
                "wrong" if m.status == S_CURRENT || m.status == S_CONFLICT => {
                    let mut p = json!({"status": S_CANDIDATE});
                    if m.mtype == T_SKILL {
                        p["helpfulCount"] = json!(0);
                    }
                    tx.merge(schema::MEMORIES, &m.id, p)?;
                    tx.index_task(schema::MEMORIES, &m.id, "upsert")?;
                    changed.push(json!({"id": m.id, "from": m.status, "to": S_CANDIDATE}));
                }
                // helpful → candidate skills count up; enabled at 3
                "helpful" if m.mtype == T_SKILL && m.status == S_CANDIDATE => {
                    let n = m.helpful_count + 1;
                    let mut p = json!({"helpfulCount": n});
                    if n >= SKILL_ENABLE_HELPFUL {
                        p["status"] = json!(S_CURRENT);
                        tx.index_task(schema::MEMORIES, &m.id, "upsert")?;
                    }
                    tx.merge(schema::MEMORIES, &m.id, p)?;
                    changed.push(json!({"id": m.id, "helpfulCount": n, "enabled": n >= SKILL_ENABLE_HELPFUL}));
                }
                _ => {}
            }
        }
    }
    let now = util::now_str();
    tx.merge(
        schema::RETRIEVAL_LOG,
        &req.retrieval_id,
        json!({"feedback": {"used": req.used, "applied": applied, "outcome": outcome, "at": now}}),
    )?;
    t.store.commit(tx).await?;
    t.wake_indexer();
    Ok(json!({"accepted": true, "changed": changed, "ignored": ignored}))
}
