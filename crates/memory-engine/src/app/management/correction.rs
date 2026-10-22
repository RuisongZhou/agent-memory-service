//! `correct_memory` (2.5): a correction supersedes the original with an explicit-origin memory.
use anyhow::Result;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::Arc;

use super::{must_get, MAX_TEXT_BYTES};
use crate::runtime::engine::Engine;
use crate::runtime::instance::Instance;
use memory_domain::types::*;
use memory_rules::util;
use memory_store::schema;
use memory_store::store::{patch, to_doc, Tx};

const MAX_CHAIN_HOPS: usize = 64;

/// The current value of `m`'s chain: walk supersededBy forward; the first active node, else the last node.
async fn chain_current(t: &Instance, m: &MemoryRec) -> Result<String> {
    let mut cur = m.clone();
    let mut seen = HashSet::from([m.id.clone()]);
    for _ in 0..MAX_CHAIN_HOPS {
        if is_active(&cur.status) {
            return Ok(cur.id);
        }
        let Some(n) = cur.superseded_by.clone() else {
            break;
        };
        if !seen.insert(n.clone()) {
            break;
        }
        match t.store.get_memory(&n).await? {
            Some(x) => cur = x,
            None => break,
        }
    }
    Ok(cur.id)
}

fn valid_time(s: &str) -> bool {
    util::parse_ts(s).is_some()
}

// =====================================================================================================
// correct_memory (2.5)
// =====================================================================================================

/// The original record keeps its text: a new memory (origin = explicit) supersedes it. Without `text` the
/// memory simply stops holding (superseded, no successor).
///
/// The API contract requires `current`; superseded, conflicting and candidate records return 409.
/// The formation lock prevents a concurrent extraction from superseding the same record twice.
pub async fn correct_memory(
    eng: &Arc<Engine>,
    t: &Arc<Instance>,
    c: &Caller,
    req: CorrectMemoryReq,
) -> ApiResult<Value> {
    let _ = eng;
    let _guard = t.lock_formation().await;
    let old = must_get(t, &req.id).await?;
    if old.status != S_CURRENT {
        return Err(ApiError::NotCurrent(chain_current(t, &old).await?));
    }
    let now = util::now_str();
    let eff = match req
        .effective_at
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(e) if valid_time(e) => e.to_string(),
        Some(e) => {
            return Err(ApiError::InvalidArgument(format!(
                "effectiveAt is not a date/RFC3339: {e:?}"
            )))
        }
        None => now.clone(),
    };
    let reason = req.reason.clone().unwrap_or_default();
    if req
        .text
        .as_deref()
        .is_some_and(|text| text.trim().is_empty())
    {
        return Err(ApiError::InvalidArgument(
            "text must be nonempty when provided; omit text to retract a memory".into(),
        ));
    }
    let text = req
        .text
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    if let Some(tx) = &text {
        if tx.len() > MAX_TEXT_BYTES {
            return Err(ApiError::PayloadTooLarge(format!(
                "text exceeds {MAX_TEXT_BYTES} bytes"
            )));
        }
    }
    let mut tx = Tx::new();
    let out = match text {
        Some(text) => {
            let new = MemoryRec {
                id: util::new_id("m"),
                mtype: old.mtype.clone(),
                text,
                scope: old.scope.clone(),
                status: S_CURRENT.into(),
                effective_at: eff.clone(),
                recorded_at: now.clone(),
                source: Source {
                    platform: c.platform.clone(),
                    turn_ids: vec![],
                    origin: ORIGIN_EXPLICIT.into(),
                },
                supersedes: Some(old.id.clone()),
                superseded_by: None,
                // plain `related` edges carry over; conflict edges stay with the old value
                related: old
                    .related
                    .iter()
                    .filter(|e| e.relation == "related")
                    .cloned()
                    .collect(),
                entity_ids: old.entity_ids.clone(),
                stated_at: now.clone(),
                stated_pos: 0,
                // OPEN: the structured detail of an experience/skill no longer matches a corrected text
                detail: None,
                evidence_count: 1,
                last_accessed_at: now.clone(),
                profile_kind: old.profile_kind.clone(),
                ..Default::default()
            };
            tx.put(schema::MEMORIES, &new.id, to_doc(&new)?)?;
            tx.merge(
                schema::MEMORIES,
                &old.id,
                patch(json!({"status": S_SUPERSEDED, "supersededBy": new.id, "validUntil": eff})),
            )?;
            let ev = EvidenceRec {
                context: None,
                id: util::new_id("e"),
                memory_ids: vec![new.id.clone()],
                platform: c.platform.clone(),
                turn_id: String::new(),
                role: "correction".into(),
                span: if reason.is_empty() {
                    format!("correction of {}", old.id)
                } else {
                    reason.clone()
                },
                stated_at: now.clone(),
                created_at: now.clone(),
            };
            tx.put(schema::EVIDENCE, &ev.id, to_doc(&ev)?)?;
            tx.index_task(schema::MEMORIES, &old.id, "upsert")?;
            tx.index_task(schema::MEMORIES, &new.id, "upsert")?;
            json!({"memory": new.public(), "correctedId": old.id})
        }
        None => {
            tx.merge(
                schema::MEMORIES,
                &old.id,
                patch(json!({"status": S_SUPERSEDED, "validUntil": eff})),
            )?;
            let ev = EvidenceRec {
                context: None,
                id: util::new_id("e"),
                memory_ids: vec![old.id.clone()],
                platform: c.platform.clone(),
                turn_id: String::new(),
                role: "correction".into(),
                span: if reason.is_empty() {
                    "no longer holds".into()
                } else {
                    reason.clone()
                },
                stated_at: now.clone(),
                created_at: now.clone(),
            };
            tx.put(schema::EVIDENCE, &ev.id, to_doc(&ev)?)?;
            tx.index_task(schema::MEMORIES, &old.id, "upsert")?;
            let mut m = old.clone();
            m.status = S_SUPERSEDED.into();
            m.valid_until = Some(eff);
            json!({"memory": m.public(), "correctedId": old.id, "retracted": true})
        }
    };
    t.store.commit(tx).await?;
    t.wake_indexer();
    Ok(out)
}
