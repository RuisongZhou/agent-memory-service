//! 纠正、删除、反馈 (2.5, 5.5) and admin inspection (stats, export) plus the test-only `seed` admin action.
use anyhow::Result;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use super::engine::Engine;
use super::indexer;
use super::store::{patch, safe_id, session_rid, to_doc, turn_rid, Tx};
use super::tenant::Tenant;
use super::types::*;
use super::util;

/// Helpful feedback count at which a candidate skill is enabled (4.5 / 5.5).
const SKILL_ENABLE_HELPFUL: i64 = 3;
/// Feedback window (5.5).
const FEEDBACK_WINDOW_HOURS: i64 = 24;
/// `text` limit for a correction (same as `add_memories.text`, 2.3).
const MAX_TEXT_BYTES: usize = 4 * 1024;
const MAX_CHAIN_HOPS: usize = 64;

async fn must_get(t: &Tenant, id: &str) -> ApiResult<MemoryRec> {
    if safe_id(id).is_err() {
        return Err(ApiError::NotFound(format!("memory {id} not found")));
    }
    t.store
        .get_memory(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("memory {id} not found")))
}

/// The current value of `m`'s chain: walk supersededBy forward; the first active node, else the last node.
async fn chain_current(t: &Tenant, m: &MemoryRec) -> Result<String> {
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
    t: &Arc<Tenant>,
    c: &Caller,
    req: CorrectMemoryReq,
) -> ApiResult<Value> {
    let _ = eng;
    let _guard = t.extract_lock.lock().await;
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
            tx.put("memories", &new.id, to_doc(&new)?)?;
            tx.merge(
                "memories",
                &old.id,
                patch(json!({"status": S_SUPERSEDED, "supersededBy": new.id, "validUntil": eff})),
            )?;
            let ev = EvidenceRec {
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
            tx.put("evidence", &ev.id, to_doc(&ev)?)?;
            tx.index_task("memories", &old.id, "upsert")?;
            tx.index_task("memories", &new.id, "upsert")?;
            json!({"memory": new.public(), "correctedId": old.id})
        }
        None => {
            tx.merge(
                "memories",
                &old.id,
                patch(json!({"status": S_SUPERSEDED, "validUntil": eff})),
            )?;
            let ev = EvidenceRec {
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
            tx.put("evidence", &ev.id, to_doc(&ev)?)?;
            tx.index_task("memories", &old.id, "upsert")?;
            let mut m = old.clone();
            m.status = S_SUPERSEDED.into();
            m.valid_until = Some(eff);
            json!({"memory": m.public(), "correctedId": old.id, "retracted": true})
        }
    };
    t.store.commit(tx).await?;
    t.index_notify.notify_one();
    Ok(out)
}

// =====================================================================================================
// delete_memory (2.5, 3.2, 5.2)
// =====================================================================================================

/// Physical delete: the record, its index entries (full-text delete committed before returning), evidence
/// rows that only reference it, edges pointing at it, and its id in derived-turn lists. An update chain is
/// relinked prev ↔ next. OPEN: deleting the head of a chain does not revive the previous value (it stays
/// superseded with no successor); a `conflict` memory whose only conflict partner was deleted returns to
/// `current`.
pub async fn delete_memory(eng: &Arc<Engine>, t: &Arc<Tenant>, req: IdReq) -> ApiResult<Value> {
    let _guard = t.extract_lock.lock().await;
    must_get(t, &req.id).await?;
    // Profiles and skills copy their source memories. Delete these derived records transitively;
    // otherwise a deleted fact would still be disclosed through the next context injection.
    let mut pending = vec![req.id.clone()];
    let mut visited = HashSet::new();
    let mut derived = Vec::new();
    let mut root = Value::Null;
    while let Some(id) = pending.pop() {
        if !visited.insert(id.clone()) {
            continue;
        }
        let dependents = t
            .store
            .find_memories(
                "type IN ['profile','skill'] AND usedMemoryIds CONTAINS $id",
                vec![("id", json!(id))],
            )
            .await?;
        pending.extend(dependents.into_iter().map(|m| m.id));
        let out = delete_one(eng, t, &id).await?;
        if id == req.id {
            root = out;
        } else {
            derived.push(id);
        }
    }
    root["derivedDeleted"] = json!(derived);
    Ok(root)
}

async fn delete_one(eng: &Arc<Engine>, t: &Arc<Tenant>, memory_id: &str) -> ApiResult<Value> {
    let m = must_get(t, memory_id).await?;
    let id = m.id.clone();
    let mut tx = Tx::new();
    tx.delete("memories", &id)?;

    // chain relink
    let prev = match &m.supersedes {
        Some(p) if safe_id(p).is_ok() => t.store.get_memory(p).await?,
        _ => None,
    };
    let next = match &m.superseded_by {
        Some(n) if safe_id(n).is_ok() => t.store.get_memory(n).await?,
        _ => None,
    };
    let mut relinked = json!(null);
    match (&prev, &next) {
        (Some(p), Some(n)) => {
            tx.merge(
                "memories",
                &p.id,
                json!({"supersededBy": n.id, "validUntil": n.effective_at}),
            )?;
            tx.merge("memories", &n.id, json!({"supersedes": p.id}))?;
            relinked = json!({"prev": p.id, "next": n.id});
        }
        (Some(p), None) => {
            tx.raw(
                &format!(
                    "UPDATE memories:{} SET supersededBy = NONE, validUntil = NONE RETURN NONE",
                    safe_id(&p.id)?
                ),
                vec![],
            );
        }
        (None, Some(n)) => {
            tx.raw(
                &format!(
                    "UPDATE memories:{} SET supersedes = NONE RETURN NONE",
                    safe_id(&n.id)?
                ),
                vec![],
            );
        }
        (None, None) => {}
    }

    // edges in other memories
    let others = t
        .store
        .find_memories("related.id CONTAINS $id", vec![("id", json!(id))])
        .await?;
    let mut edges_removed = 0usize;
    for o in &others {
        if o.id == id {
            continue;
        }
        let rel: Vec<RelEdge> = o.related.iter().filter(|e| e.id != id).cloned().collect();
        edges_removed += o.related.len() - rel.len();
        let mut p = json!({"related": rel});
        if o.status == S_CONFLICT && !rel.iter().any(|e| e.relation == "conflicts") {
            p["status"] = json!(S_CURRENT);
        }
        tx.merge("memories", &o.id, p)?;
        tx.index_task("memories", &o.id, "upsert")?;
    }

    // evidence: rows only referencing this memory are deleted, others lose the reference
    let ev: Vec<EvidenceRec> = t
        .store
        .select_as(
            "SELECT * OMIT id FROM evidence WHERE memoryIds CONTAINS $id",
            vec![("id", json!(id))],
        )
        .await?;
    let (mut ev_deleted, mut ev_updated) = (0usize, 0usize);
    for e in &ev {
        let rest: Vec<String> = e.memory_ids.iter().filter(|x| **x != id).cloned().collect();
        if rest.is_empty() {
            tx.delete("evidence", &e.id)?;
            ev_deleted += 1;
        } else {
            tx.merge("evidence", &e.id, json!({"memoryIds": rest}))?;
            ev_updated += 1;
        }
    }

    // derived content in buffer turns (the fact text merged into the turn's full-text key, 3.4)
    let turns: Vec<TurnRec> = t
        .store
        .select_as(
            "SELECT * OMIT id, embedding FROM turns WHERE derivedMemoryIds CONTAINS $id",
            vec![("id", json!(id))],
        )
        .await?;
    for tr in &turns {
        let ids: Vec<String> = tr
            .derived_memory_ids
            .iter()
            .filter(|x| **x != id)
            .cloned()
            .collect();
        let remaining = t.store.get_memories(&ids).await?;
        let by_id: BTreeMap<&str, &str> = remaining
            .iter()
            .map(|m| (m.id.as_str(), m.text.as_str()))
            .collect();
        let text: Vec<&str> = ids
            .iter()
            .filter_map(|id| by_id.get(id.as_str()).copied())
            .collect();
        let rid = turn_rid(&tr.platform, &tr.turn_id);
        tx.merge(
            "turns",
            &rid,
            json!({"derivedMemoryIds": ids, "derivedText": text.join("\n")}),
        )?;
        tx.index_task("turns", &rid, "upsert")?;
    }
    tx.index_task("memories", &id, "delete")?;
    t.store.commit(tx).await?;
    // 5.2: the full-text delete is done and committed before returning
    indexer::drain(eng, t).await?;
    Ok(json!({
        "deleted": id,
        "relinked": relinked,
        "edgesRemoved": edges_removed,
        "evidenceDeleted": ev_deleted,
        "evidenceUpdated": ev_updated,
        "turnsUpdated": turns.len(),
    }))
}

// =====================================================================================================
// feedback (5.5)
// =====================================================================================================

pub async fn feedback(eng: &Arc<Engine>, t: &Arc<Tenant>, req: FeedbackReq) -> ApiResult<Value> {
    let _ = eng;
    let _guard = t.extract_lock.lock().await;
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
    let Some(log) = t.store.get_doc("retrieval_log", &req.retrieval_id).await? else {
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
                    tx.merge("memories", &m.id, p)?;
                    tx.index_task("memories", &m.id, "upsert")?;
                    changed.push(json!({"id": m.id, "from": m.status, "to": S_CANDIDATE}));
                }
                // helpful → candidate skills count up; enabled at 3
                "helpful" if m.mtype == T_SKILL && m.status == S_CANDIDATE => {
                    let n = m.helpful_count + 1;
                    let mut p = json!({"helpfulCount": n});
                    if n >= SKILL_ENABLE_HELPFUL {
                        p["status"] = json!(S_CURRENT);
                        tx.index_task("memories", &m.id, "upsert")?;
                    }
                    tx.merge("memories", &m.id, p)?;
                    changed.push(json!({"id": m.id, "helpfulCount": n, "enabled": n >= SKILL_ENABLE_HELPFUL}));
                }
                _ => {}
            }
        }
    }
    let now = util::now_str();
    tx.merge(
        "retrieval_log",
        &req.retrieval_id,
        json!({"feedback": {"used": req.used, "applied": applied, "outcome": outcome, "at": now}}),
    )?;
    t.store.commit(tx).await?;
    t.index_notify.notify_one();
    Ok(json!({"accepted": true, "changed": changed, "ignored": ignored}))
}

// =====================================================================================================
// admin: stats, export, seed
// =====================================================================================================

const TABLES: [&str; 12] = [
    "turns",
    "sessions",
    "evidence",
    "memories",
    "entities",
    "index_tasks",
    "index_meta",
    "requests",
    "retrieval_log",
    "extract_log",
    "llm_usage",
    "proposals",
];

pub async fn stats(eng: &Arc<Engine>, t: &Arc<Tenant>) -> ApiResult<Value> {
    let mut counts = Map::new();
    for tb in TABLES {
        counts.insert(tb.into(), json!(t.store.count(tb, None).await?));
    }
    let rows = t
        .store
        .select(
            "SELECT type, status, count() AS n FROM memories GROUP BY type, status",
            vec![],
        )
        .await?;
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
    let usage_rows = t
        .store
        .select("SELECT * OMIT id FROM llm_usage", vec![])
        .await?;
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
    let (vm, vt) = {
        let g = t.vecs.read().await;
        (g.memories.len(), g.turns.len())
    };
    let gap_rows = t
        .store
        .select("SELECT count() AS n FROM retrieval_log WHERE kind = 'search' AND returned.gap = true GROUP ALL", vec![])
        .await?;
    Ok(json!({
        "tenant": t.name,
        "counts": counts,
        "memories": by,
        "llm": {"process": eng.llm.usage_snapshot(), "persisted": usage_sum, "rows": usage_rows.len()},
        "index": {"fulltextDocs": t.ft.num_docs(), "memoryVectors": vm, "turnVectors": vt,
                  "pendingTasks": indexer::pending(t).await?},
        "retrieval": {"gapSearches": gap_rows.first().and_then(|r| r.get("n")).cloned().unwrap_or(json!(0))},
        "jobs": indexer::stats_json(t),
    }))
}

/// All memories (debug). Optional filters: `status`, `type`, `project` (strings or arrays of strings).
pub async fn export(_eng: &Arc<Engine>, t: &Arc<Tenant>, body: Value) -> ApiResult<Value> {
    // Read-only evaluation extensions for request-level billing and proposal/discard audits.
    if let Some(table) = body.get("table").and_then(Value::as_str) {
        if !matches!(
            table,
            "llm_usage" | "extract_log" | "retrieval_log" | "proposals"
        ) {
            return Err(ApiError::InvalidArgument("unsupported export table".into()));
        }
        let records = t
            .store
            .select(&format!("SELECT * OMIT id FROM {table}"), vec![])
            .await?;
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
    if eng.models.embed_model_id != "mock-bow-1024" {
        return Err(ApiError::InvalidArgument(
            "seed is available only with mock models".into(),
        ));
    }
    let _guard = t.extract_lock.lock().await;
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
        tx.put("memories", &m.id, to_doc(&m)?)?;
        tx.index_task("memories", &m.id, "upsert")?;
    }
    for tr in &turns {
        let rid = turn_rid(&tr.platform, &tr.turn_id);
        let mut d = to_doc(tr)?;
        d["mid"] = json!(rid);
        tx.put("turns", &rid, d)?;
        tx.index_task("turns", &rid, "upsert")?;
    }
    for s in &sessions {
        let rid = session_rid(&s.platform, &s.session);
        let mut d = to_doc(s)?;
        d["mid"] = json!(rid);
        tx.put("sessions", &rid, d)?;
    }
    for e in &evidence {
        tx.put("evidence", &e.id, to_doc(e)?)?;
    }
    for e in &entities {
        tx.put("entities", &e.id, to_doc(e)?)?;
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
