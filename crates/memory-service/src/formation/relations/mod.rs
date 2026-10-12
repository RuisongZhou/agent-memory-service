//! 关系判定与写入 (4.3). The decision logic is the pure planner in [`plan`]; this module loads the update
//! chains the planner needs, turns the plan into records and commits one transaction per batch (memories,
//! edges, evidence, old-memory status changes, entities, turns.extractedAt, requests, index tasks).
pub use memory_rules::relations as plan;

use anyhow::Result;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

use super::engine::Engine;
use super::store::{self, to_doc, Tx};
use super::tenant::Tenant;
use super::types::*;
use super::util;
use plan::{Node, Rel, Target};

/// Where a fact's supporting span came from.
#[derive(Debug, Clone)]
pub struct FactSource {
    /// turnId (P1) or "text" (P1-b)
    pub turn_id: String,
    pub span: String,
    /// "user" | "assistant" | "text"
    pub role: String,
    /// 说出时间: timestamp of the message the span is in (RFC3339)
    pub stated_at: String,
    /// (unix seconds, position) — user message spans sort before assistant reply spans
    pub stated: (i64, i64),
}

/// A fact that passed the rule checks of prompts_ch4 (built by `extract`).
#[derive(Debug, Clone)]
pub struct NewFact {
    /// 1-based F index in the model output
    pub fidx: usize,
    pub text: String,
    pub scope: MemScope,
    pub entity_ids: Vec<String>,
    pub effective_at: String,
    pub origin: String,
    /// first = primary; more when identical texts of the batch were merged
    pub sources: Vec<FactSource>,
    pub relations: Vec<Rel>,
    pub reason: Option<String>,
}

impl NewFact {
    /// statedAt used for ordering: the latest of the merged sources.
    pub fn stated(&self) -> (i64, i64, String) {
        let s = self
            .sources
            .iter()
            .max_by_key(|s| s.stated)
            .expect("fact without source");
        (s.stated.0, s.stated.1, s.stated_at.clone())
    }
}

pub struct WriteCtx {
    pub platform: String,
    pub now: String,
    /// batch turns as (record id, turn) — empty for P1-b
    pub turns: Vec<(String, TurnRec)>,
    /// P1-b request to complete in the same transaction
    pub text_request: Option<String>,
    /// evidence turn id for P1-b spans (the request id)
    pub text_turn_id: Option<String>,
    pub new_entities: Vec<EntityRec>,
}

#[derive(Debug, Default)]
pub struct Applied {
    /// ids of the new memories
    pub created: Vec<String>,
    /// memory ids every fact ended up as (new or `same` target), de-duplicated, in processing order
    pub all_ids: Vec<String>,
    pub per_turn: HashMap<String, Vec<String>>,
    pub actions: Vec<Value>,
    pub superseded: Vec<String>,
}

pub fn source_context(turn: &TurnRec, role: &str, first_hand: bool) -> Option<EvidenceContext> {
    let question = turn.user_message.trim();
    (first_hand
        && role == "assistant"
        && question.ends_with(['?', '？'])
        && question.chars().count() <= 1024)
        .then(|| EvidenceContext {
            span: turn.user_message.clone(),
            stated_at: turn.user_ts.clone(),
        })
}

/// Explicit archive recovery only: enrich existing verified reply evidence, without generating
/// facts or using QA labels. Ordinary formation writes the same bounded question atomically.
pub async fn backfill_source_context(t: &Arc<Tenant>, turns: &[TurnRec]) -> Result<usize> {
    let rows: Vec<EvidenceRec> = t
        .store
        .select_as("SELECT * OMIT id FROM evidence", vec![])
        .await?;
    let mut tx = Tx::new();
    let mut count = 0;
    for evidence in rows
        .into_iter()
        .filter(|e| e.context.is_none() && e.role == "assistant")
    {
        let Some(turn) = turns
            .iter()
            .find(|tr| tr.platform == evidence.platform && tr.turn_id == evidence.turn_id)
        else {
            continue;
        };
        let body = turn
            .assistant_message
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let span = evidence
            .span
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if span.is_empty() || !body.contains(&span) {
            continue;
        }
        if let Some(context) = source_context(turn, &evidence.role, true) {
            tx.merge("evidence", &evidence.id, json!({"context":context}))?;
            count += 1;
        }
    }
    if !tx.is_empty() {
        t.store.commit(tx).await?;
    }
    Ok(count)
}

/// Load `id`, its successors up to the chain head and the head's predecessors (5.3 更新链).
pub async fn load_chain(t: &Tenant, id: &str, out: &mut HashMap<String, MemoryRec>) -> Result<()> {
    let mut cur = id.to_string();
    let mut head = None;
    for _ in 0..64 {
        let rec = match out.get(&cur) {
            Some(r) => r.clone(),
            None => match t.store.get_memory(&cur).await? {
                Some(r) => {
                    out.insert(cur.clone(), r.clone());
                    r
                }
                None => break,
            },
        };
        head = Some(rec.id.clone());
        match rec.superseded_by {
            Some(n) if n != cur => cur = n,
            _ => break,
        }
    }
    let Some(mut cur) = head else { return Ok(()) };
    for _ in 0..64 {
        let prev = out.get(&cur).and_then(|r| r.supersedes.clone());
        let Some(pv) = prev else { break };
        if !out.contains_key(&pv) {
            match t.store.get_memory(&pv).await? {
                Some(r) => {
                    out.insert(pv.clone(), r);
                }
                None => break,
            }
        }
        cur = pv;
    }
    Ok(())
}

fn stated_of(m: &MemoryRec) -> (i64, i64) {
    let s = if m.stated_at.is_empty() {
        &m.recorded_at
    } else {
        &m.stated_at
    };
    (util::ts_secs(s), m.stated_pos)
}

pub fn node_of(m: &MemoryRec) -> Node {
    Node {
        id: m.id.clone(),
        is_new: false,
        status: m.status.clone(),
        origin: m.source.origin.clone(),
        stated: stated_of(m),
        stated_at: if m.stated_at.is_empty() {
            m.recorded_at.clone()
        } else {
            m.stated_at.clone()
        },
        effective_at: m.effective_at.clone(),
        supersedes: m.supersedes.clone(),
        superseded_by: m.superseded_by.clone(),
        valid_until: m.valid_until.clone(),
        related: m
            .related
            .iter()
            .map(|e| plan::Edge {
                id: e.id.clone(),
                relation: e.relation.clone(),
            })
            .collect(),
        evidence_count: m.evidence_count,
        turn_ids: m.source.turn_ids.clone(),
        dirty: false,
    }
}

fn edges(n: &Node) -> Vec<RelEdge> {
    n.related
        .iter()
        .map(|e| RelEdge {
            id: e.id.clone(),
            relation: e.relation.clone(),
        })
        .collect()
}

/// Remove JSON nulls from a document so absent values stay NONE in SurrealDB (NULL != NONE in queries).
pub fn strip_nulls(v: Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(m.into_iter().filter(|(_, v)| !v.is_null()).collect()),
        other => other,
    }
}

/// Patch for an old memory changed by a plan.
pub fn old_patch(old: &MemoryRec, n: &Node) -> Value {
    let mut source = old.source.clone();
    for tid in &n.turn_ids {
        if !source.turn_ids.contains(tid) {
            source.turn_ids.push(tid.clone());
        }
    }
    let mut p = json!({
        "status": n.status,
        "related": edges(n),
        "evidenceCount": n.evidence_count,
        "statedAt": n.stated_at,
        "statedPos": n.stated.1,
        "source": source,
    });
    if let Some(s) = &n.supersedes {
        p["supersedes"] = json!(s);
    }
    if let Some(s) = &n.superseded_by {
        p["supersededBy"] = json!(s);
    }
    if let Some(s) = &n.valid_until {
        p["validUntil"] = json!(s);
    }
    store::patch(p)
}

/// Queue the record changes of an old memory into `tx` (status change → embedding cleared in the same
/// transaction, 5.2; the index task drops it from the full-text index).
pub fn write_old(tx: &mut Tx, old: &MemoryRec, n: &Node) -> Result<()> {
    tx.merge("memories", &old.id, old_patch(old, n))?;
    if n.status == S_SUPERSEDED && old.status != S_SUPERSEDED {
        tx.raw(
            &format!(
                "UPDATE memories:{} SET embedding = NONE RETURN NONE",
                store::safe_id(&old.id)?
            ),
            vec![],
        );
    }
    tx.index_task("memories", &old.id, "upsert")?;
    Ok(())
}

/// Apply 4.3 to the validated facts of one batch (or one P1-b call) and commit everything in one transaction.
pub async fn apply(
    eng: &Arc<Engine>,
    t: &Arc<Tenant>,
    facts: Vec<NewFact>,
    ctx: WriteCtx,
) -> Result<Applied> {
    apply_with(eng, t, facts, ctx, |_, _, _| Ok(())).await
}

/// Append experience/summary changes before the same atomic fact/turn/receipt commit.
pub async fn apply_with<F>(
    eng: &Arc<Engine>,
    t: &Arc<Tenant>,
    facts: Vec<NewFact>,
    ctx: WriteCtx,
    additional: F,
) -> Result<Applied>
where
    F: FnOnce(&mut Tx, &mut Applied, &mut HashMap<String, String>) -> Result<()> + Send,
{
    let _ = eng;
    // 1. old memories referenced by the model, with their chains
    let mut recs: HashMap<String, MemoryRec> = HashMap::new();
    for f in &facts {
        for r in &f.relations {
            if let Target::Old(id) = &r.target {
                if !recs.contains_key(id) {
                    load_chain(t, id, &mut recs).await?;
                }
            }
        }
    }
    // 2. plan
    let ids: HashMap<usize, String> = facts.iter().map(|f| (f.fidx, util::new_id("m"))).collect();
    let pfacts: Vec<plan::Fact> = facts
        .iter()
        .map(|f| {
            let (ts, pos, stated_at) = f.stated();
            let mut tids: Vec<String> = Vec::new();
            for s in &f.sources {
                if s.role != "text" && !tids.contains(&s.turn_id) {
                    tids.push(s.turn_id.clone());
                }
            }
            plan::Fact {
                fidx: f.fidx,
                id: ids[&f.fidx].clone(),
                origin: f.origin.clone(),
                stated: (ts, pos),
                stated_at,
                effective_at: f.effective_at.clone(),
                turn_ids: tids,
                n_sources: f.sources.len() as i64,
                relations: f.relations.clone(),
            }
        })
        .collect();
    let p = plan::plan_batch(recs.values().map(node_of).collect(), &pfacts);

    // 3. records
    let by_fidx: HashMap<usize, &NewFact> = facts.iter().map(|f| (f.fidx, f)).collect();
    let now = ctx.now.clone();
    let mut tx = Tx::new();
    let mut out = Applied::default();
    let mut texts: HashMap<String, String> = HashMap::new();
    let mut evidence: Vec<EvidenceRec> = Vec::new();
    let ev_turn = |s: &FactSource| -> String {
        if s.role == "text" {
            ctx.text_turn_id.clone().unwrap_or_else(|| "text".into())
        } else {
            s.turn_id.clone()
        }
    };
    for e in &ctx.new_entities {
        tx.put("entities", &e.id, strip_nulls(to_doc(e)?))?;
    }
    for fidx in &p.created {
        let f = by_fidx[fidx];
        let id = &p.resolved[fidx];
        let n = &p.nodes[id];
        let rec = MemoryRec {
            id: id.clone(),
            mtype: T_FACT.into(),
            text: f.text.clone(),
            scope: f.scope.clone(),
            status: n.status.clone(),
            effective_at: f.effective_at.clone(),
            recorded_at: now.clone(),
            source: Source {
                platform: ctx.platform.clone(),
                turn_ids: n.turn_ids.clone(),
                origin: f.origin.clone(),
            },
            supersedes: n.supersedes.clone(),
            superseded_by: n.superseded_by.clone(),
            related: edges(n),
            entity_ids: f.entity_ids.clone(),
            stated_at: n.stated_at.clone(),
            stated_pos: n.stated.1,
            detail: None,
            evidence_count: n.evidence_count,
            last_accessed_at: now.clone(),
            valid_until: n.valid_until.clone(),
            ..Default::default()
        };
        tx.put("memories", id, strip_nulls(to_doc(&rec)?))?;
        tx.index_task("memories", id, "upsert")?;
        texts.insert(id.clone(), f.text.clone());
        for s in &f.sources {
            evidence.push(EvidenceRec {
                context: ctx
                    .turns
                    .iter()
                    .find(|(_, tr)| tr.turn_id == s.turn_id)
                    .and_then(|(_, tr)| {
                        source_context(tr, &s.role, eng.prompts_for(t).both_speakers_first_hand)
                    }),
                id: util::new_id("e"),
                memory_ids: vec![id.clone()],
                platform: ctx.platform.clone(),
                turn_id: ev_turn(s),
                role: s.role.clone(),
                span: s.span.clone(),
                stated_at: s.stated_at.clone(),
                created_at: now.clone(),
            });
        }
        out.created.push(id.clone());
    }
    for (fidx, target) in &p.same {
        let f = by_fidx[fidx];
        for s in &f.sources {
            evidence.push(EvidenceRec {
                context: ctx
                    .turns
                    .iter()
                    .find(|(_, tr)| tr.turn_id == s.turn_id)
                    .and_then(|(_, tr)| {
                        source_context(tr, &s.role, eng.prompts_for(t).both_speakers_first_hand)
                    }),
                id: util::new_id("e"),
                memory_ids: vec![target.clone()],
                platform: ctx.platform.clone(),
                turn_id: ev_turn(s),
                role: s.role.clone(),
                span: s.span.clone(),
                stated_at: s.stated_at.clone(),
                created_at: now.clone(),
            });
        }
    }
    for (id, n) in &p.nodes {
        if n.is_new || !n.dirty {
            continue;
        }
        let Some(old) = recs.get(id) else { continue };
        texts.insert(id.clone(), old.text.clone());
        write_old(&mut tx, old, n)?;
        if n.status == S_SUPERSEDED && old.status != S_SUPERSEDED {
            out.superseded.push(id.clone());
        }
    }
    // Conflicts are durable questions with both assertions retained, not a generated winner.
    let scopes: HashMap<String, MemScope> = recs
        .values()
        .map(|m| (m.id.clone(), m.scope.clone()))
        .chain(
            facts
                .iter()
                .map(|f| (p.resolved[&f.fidx].clone(), f.scope.clone())),
        )
        .collect();
    for (id, node) in &p.nodes {
        if node.status != S_CONFLICT || !node.dirty {
            continue;
        }
        for edge in node
            .related
            .iter()
            .filter(|edge| edge.relation == "conflicts")
        {
            if scopes.get(id) != scopes.get(&edge.id) {
                continue;
            }
            let mut pair = vec![id.clone(), edge.id.clone()];
            pair.sort();
            let uncertainty_id = store::hashed_id("u", &serde_json::to_string(&pair)?);
            let rec = UncertaintyRec {
                id: uncertainty_id.clone(),
                memory_ids: pair,
                scope: scopes[id].clone(),
                reason: "Conflicting assertions require confirmation".into(),
                status: "open".into(),
                created_at: now.clone(),
            };
            tx.put("uncertainties", &uncertainty_id, to_doc(&rec)?)?;
        }
    }
    for ev in &evidence {
        tx.put("evidence", &ev.id, strip_nulls(to_doc(ev)?))?;
    }

    // derived memories per turn / per request
    for fidx in &p.order {
        let Some(mid) = p.resolved.get(fidx) else {
            continue;
        };
        if !out.all_ids.contains(mid) {
            out.all_ids.push(mid.clone());
        }
        for s in &by_fidx[fidx].sources {
            let v = out.per_turn.entry(s.turn_id.clone()).or_default();
            if !v.contains(mid) {
                v.push(mid.clone());
            }
        }
    }
    additional(&mut tx, &mut out, &mut texts)?;
    for (rid, tr) in &ctx.turns {
        let mut ids = if tr.extract_state == "done" {
            tr.derived_memory_ids.clone()
        } else {
            Vec::new()
        };
        for id in out.per_turn.get(&tr.turn_id).into_iter().flatten() {
            if !ids.contains(id) {
                ids.push(id.clone());
            }
        }
        let mut derived: Vec<String> = if tr.extract_state == "done" && !tr.derived_text.is_empty()
        {
            vec![tr.derived_text.clone()]
        } else {
            Vec::new()
        };
        for text in ids.iter().filter_map(|i| texts.get(i)) {
            if !derived.contains(text) {
                derived.push(text.clone());
            }
        }
        tx.merge(
            "turns",
            rid,
            store::patch(json!({
                "extractedAt": now,
                "extractedTs": util::ts_secs(&now),
                "extractState": "done",
                "derivedText": derived.join("\n"),
                "derivedMemoryIds": ids,
            })),
        )?;
        tx.index_task("turns", rid, "upsert")?;
        if !tr.request_id.is_empty() {
            tx.merge(
                "requests",
                &tr.request_id,
                store::patch(json!({"status": "completed", "memoryIds": ids, "updatedAt": now, "error": Value::Null})),
            )?;
        }
    }
    if let Some(rq) = &ctx.text_request {
        // The retry payload is transient; retain only copied evidence after successful formation.
        tx.raw(
            &format!(
                "UPDATE requests:{} SET text = NONE RETURN NONE",
                store::safe_id(rq)?
            ),
            vec![],
        );
        tx.merge(
            "requests",
            rq,
            store::patch(json!({"status": "completed", "memoryIds": out.all_ids, "updatedAt": now, "error": Value::Null})),
        )?;
    }
    // Decision provenance belongs to the same commit as the records it describes.
    let log_id = util::new_id("l");
    tx.put(
        "extract_log",
        &log_id,
        json!({"mid":log_id,"stage":"relations","createdAt":now,
        "actions":p.log.iter().map(|l|json!({"fact":format!("F{}",l.fidx),"action":l.action,
            "target":l.target,"memoryId":p.resolved.get(&l.fidx)})).collect::<Vec<_>>()}),
    )?;
    t.store.commit(tx).await?;

    // 4. after commit: entity dictionary, vector cache, index worker
    if !ctx.new_entities.is_empty() {
        let mut d = t.entities.write().await;
        for e in &ctx.new_entities {
            let mut names = vec![e.name.clone()];
            names.extend(e.aliases.iter().cloned());
            d.upsert(&e.id, &names);
        }
    }
    if !out.superseded.is_empty() {
        let mut c = t.vecs.write().await;
        for id in &out.superseded {
            c.memories.remove(id);
        }
    }
    t.index_notify.notify_one();

    out.actions = p
        .log
        .iter()
        .map(|l| {
            json!({
                "fact": format!("F{}", l.fidx),
                "action": l.action,
                "target": l.target,
                "memoryId": p.resolved.get(&l.fidx),
            })
        })
        .collect();
    Ok(out)
}
