//! Turn-buffer retrieval: the recent-turn channel of `search_memories` (3.4) and `list_turns`.
use anyhow::Result;
use chrono::{DateTime, Utc};
use memory_fulltext::FtFilter;
use memory_rules::timeparse::TimeWindow;
use memory_rules::util;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::query::{Analysis, Params};
use super::round4;
use crate::repo::{self, TurnScope};
use memory_domain::types::*;
use memory_engine::retrieval::{
    scoring::{decay, rrf_fuse, term_overlap_ok},
    visibility::{Lvl, Vis},
};
use memory_engine::runtime::engine::Engine;
use memory_engine::runtime::tenant::Tenant;
use memory_store::store::turn_rid;

struct TurnHit {
    turn: TurnRec,
    score: f64,
    /// Window of a long message that matched better than the digest.
    chunk: Option<usize>,
}

/// Turns of the buffer inside the request scope. OPEN: a turn is in scope when its project equals the
/// request project ("*" = all projects); without a project, the caller platform's turns are used.
fn turn_scope_clause<'a>(vis: &'a Vis, c: &'a Caller) -> (TurnScope<'a>, Vec<(String, String)>) {
    match &vis.project {
        Lvl::Is(p) => (TurnScope::Project(p), vec![("project".into(), p.clone())]),
        Lvl::Any => (TurnScope::All, vec![]),
        Lvl::No if vis.platform == Lvl::Any => (TurnScope::All, vec![]),
        Lvl::No => (
            TurnScope::Platform(&c.platform),
            vec![("platform".into(), c.platform.clone())],
        ),
    }
}

#[allow(clippy::too_many_arguments)]
async fn recent_turns(
    eng: &Arc<Engine>,
    t: &Arc<Tenant>,
    c: &Caller,
    scope: &ReqScope,
    vis: &Vis,
    now: DateTime<Utc>,
    window: Option<&TimeWindow>,
    qvec: &[f32],
    query: &str,
    qterms: &[String],
    exclude: &HashSet<String>,
) -> Result<Vec<TurnHit>> {
    let cfg = &eng.cfg;
    let (turn_scope, ft_vis) = turn_scope_clause(vis, c);
    let turns: Vec<TurnRec> = repo::turns_in_scope(
        t,
        turn_scope,
        window.map(|w| (w.start.timestamp(), w.end.timestamp())),
    )
    .await?;
    if turns.is_empty() {
        return Ok(vec![]);
    }
    let by_rid: HashMap<String, TurnRec> = turns
        .into_iter()
        .map(|tr| (turn_rid(&tr.platform, &tr.turn_id), tr))
        .collect();

    // BM25 over the whole turn text (derived facts merged in after extraction)
    let filter = FtFilter {
        kinds: vec!["turn".into()],
        types: vec![],
        statuses: vec![],
        visible: ft_vis,
        time_range: window.map(|w| (w.start.timestamp(), w.end.timestamp() - 1)),
    };
    let bm25: Vec<String> =
        t.ft.search(query, &filter, cfg.recent_bm25_k)?
            .into_iter()
            .map(|(id, _)| id)
            .filter(|id| by_rid.contains_key(id))
            .collect();
    // digest vectors, plus window vectors of long messages: a turn scores its best-matching view
    // (turns without a vector yet only go through BM25)
    let mut best_chunk: HashMap<String, usize> = HashMap::new();
    let mut vs: Vec<(String, f32)> = {
        let g = t.vecs.read().await;
        by_rid
            .keys()
            .filter_map(|rid| {
                let mut best = g.turns.get(rid).map(|e| (util::cosine(qvec, e), None));
                let n = by_rid[rid].chunks.len();
                for (i, e) in g
                    .turn_chunks
                    .get(rid)
                    .into_iter()
                    .flat_map(|c| c.iter())
                    .enumerate()
                    .take(n)
                {
                    let s = util::cosine(qvec, e);
                    if best.map(|(b, _)| s > b).unwrap_or(true) {
                        best = Some((s, Some(i)));
                    }
                }
                let (s, chunk) = best?;
                if let Some(i) = chunk {
                    best_chunk.insert(rid.clone(), i);
                }
                Some((rid.clone(), s))
            })
            .collect()
    };
    let vsim: HashMap<String, f32> = vs.iter().cloned().collect();
    vs.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });
    let vlist: Vec<String> = vs.into_iter().take(cfg.recent_vec_k).map(|x| x.0).collect();
    let fused = rrf_fuse(&[bm25, vlist], cfg.rrf_k);

    let cur_project = vis.current_project();
    let mut hits: Vec<TurnHit> = Vec::new();
    for (rid, rrf) in fused {
        let tr = &by_rid[&rid];
        if tr.derived_memory_ids.iter().any(|m| exclude.contains(m)) {
            continue;
        }
        // OPEN: the design gives no relevance threshold for turns; the no-reranker gate of 5.3 is applied
        // (digest similarity ≥ 0.45 or > half of the query terms in the turn) so that `gap` stays meaningful.
        let s = vsim.get(&rid).copied().unwrap_or(0.0);
        let ok = s >= cfg.no_rerank_vec_threshold || {
            let terms: HashSet<String> = t.ft.analyze(&tr.full_text()).into_iter().collect();
            term_overlap_ok(qterms, &terms)
        };
        if !ok {
            continue;
        }
        let days = util::parse_ts(&tr.started_at)
            .map(|st| util::days_between(&now, &st))
            .unwrap_or(0.0);
        let mut score = rrf * decay(days, cfg.recent_half_life_days);
        if scope
            .session
            .as_deref()
            .is_some_and(|s| !s.is_empty() && s == tr.session)
            && tr.platform == c.platform
        {
            score *= 1.2;
        }
        if cur_project.is_some_and(|p| p == tr.project) {
            score *= 1.1;
        }
        hits.push(TurnHit {
            turn: tr.clone(),
            score,
            chunk: best_chunk.get(&rid).copied(),
        });
    }
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    hits.truncate(cfg.recent_return);
    Ok(hits)
}

/// Recent turns that fit the remaining budget, as response items and retrieval-log entries.
#[allow(clippy::too_many_arguments)]
pub(super) async fn place_recent(
    eng: &Arc<Engine>,
    t: &Arc<Tenant>,
    c: &Caller,
    vis: &Vis,
    params: &Params,
    an: &Analysis,
    qvec: &[f32],
    returned_ids: &[String],
    used: &mut usize,
) -> ApiResult<(Vec<Value>, Vec<Value>)> {
    let mut turns_out: Vec<Value> = Vec::new();
    let mut turn_log: Vec<Value> = Vec::new();
    let exclude: HashSet<String> = returned_ids.iter().cloned().collect();
    let hits = recent_turns(
        eng,
        t,
        c,
        &params.scope,
        vis,
        params.now_utc,
        an.window.as_ref(),
        qvec,
        &params.query,
        &an.qterms,
        &exclude,
    )
    .await?;
    let digest_max = eng.cfg.digest_max_bytes;
    for (rank, h) in hits.into_iter().enumerate() {
        // A long message contributes the window that matched instead of its head.
        let window = h
            .chunk
            .and_then(|i| Some((h.turn.chunk_view(i, digest_max)?, &h.turn.chunks[i])));
        let text = window.as_ref().map_or(&h.turn.digest, |(view, _)| view);
        let cost = util::est_tokens(text);
        let fit = *used + cost <= params.budget;
        turn_log.push(
            json!({"turnId": h.turn.turn_id, "platform": h.turn.platform, "rank": rank + 1,
                                 "score": round4(h.score), "placed": fit, "chunk": h.chunk}),
        );
        if fit {
            *used += cost;
            let mut item = json!({
                "turnId": h.turn.turn_id,
                "platform": h.turn.platform,
                "session": h.turn.session,
                "timestamp": h.turn.started_at,
                "digest": text,
                "complete": h.turn.digest_complete(digest_max),
            });
            if let Some((_, c)) = &window {
                item["excerpt"] = json!({"role": c.role, "start": c.start, "end": c.end});
            }
            turns_out.push(item);
        }
    }
    Ok((turns_out, turn_log))
}

// =====================================================================================================
// list_turns (3.4 按时间)
// =====================================================================================================

pub async fn list_turns(
    eng: &Arc<Engine>,
    t: &Arc<Tenant>,
    c: &Caller,
    req: ListTurnsReq,
) -> ApiResult<Value> {
    let budget = req.token_budget.unwrap_or(eng.cfg.default_token_budget);
    if budget == 0 {
        return Err(ApiError::InvalidArgument("tokenBudget must be > 0".into()));
    }
    let project = req
        .scope
        .project
        .clone()
        .filter(|p| !p.is_empty() && p != "*");
    let session = req
        .scope
        .session
        .clone()
        .filter(|s| !s.is_empty() && s != "*");

    // with turnIds: the full text of those turns
    if let Some(ids) = req.turn_ids.as_ref().filter(|v| !v.is_empty()) {
        let scope = if let Some(p) = &project {
            TurnScope::Project(p)
        } else if req.scope.project.as_deref() != Some("*")
            && req.scope.platform.as_deref() != Some("*")
        {
            TurnScope::Platform(&c.platform)
        } else {
            TurnScope::All
        };
        let turns = repo::turns_by_id(t, ids, scope, session.as_deref()).await?;
        return Ok(json!({
            "turns": turns.iter().map(|tr| json!({
                "turnId": tr.turn_id, "platform": tr.platform, "project": tr.project, "session": tr.session,
                "startedAt": tr.started_at, "endedAt": tr.ended_at,
                "messages": [
                    {"role": "user", "content": tr.user_message, "timestamp": tr.user_ts},
                    {"role": "assistant", "content": tr.assistant_message, "timestamp": tr.assistant_ts},
                ],
            })).collect::<Vec<_>>()
        }));
    }

    // OPEN: the platform level is not used as a filter here — list_turns exists to continue work started
    // on another platform; scope = project (+ session when given), plus `since`.
    let scope = if let Some(p) = &project {
        TurnScope::Project(p)
    } else if req.scope.project.as_deref() != Some("*")
        && req.scope.platform.as_deref() != Some("*")
    {
        TurnScope::Platform(&c.platform)
    } else {
        TurnScope::All
    };
    let mut since_ts = None;
    if let Some(since) = req.since.as_deref().filter(|s| !s.trim().is_empty()) {
        let ts = util::parse_ts(since)
            .ok_or_else(|| ApiError::InvalidArgument(format!("since is not RFC3339: {since:?}")))?;
        since_ts = Some(ts.timestamp());
    }
    let turns = repo::turns_newest_first(t, scope, session.as_deref(), since_ts).await?;

    // group by (platform, session), most recently active session first
    let mut order: Vec<(String, String)> = Vec::new();
    let mut groups: HashMap<(String, String), Vec<&TurnRec>> = HashMap::new();
    for tr in &turns {
        let k = (tr.platform.clone(), tr.session.clone());
        if !groups.contains_key(&k) {
            order.push(k.clone());
        }
        groups.entry(k).or_default().push(tr);
    }
    let mut used = 0usize;
    let mut summaries: HashMap<(String, String), Value> = HashMap::new();
    // session summaries first (newest session first) …
    for k in &order {
        if let Some(s) = t.store.get_session(&k.0, &k.1).await? {
            let v = json!({"goal": s.goal, "outcome": s.outcome, "decisions": s.decisions,
                           "openIssues": s.open_issues, "turnCount": s.turn_count, "updatedAt": s.updated_at,
                           "endedAt": s.ended_at});
            let cost = util::est_tokens(&v.to_string());
            if used + cost <= budget {
                used += cost;
                summaries.insert(k.clone(), v);
            }
        }
    }
    // … then digests from the newest turn backwards until the budget is used (non-fitting ones skipped)
    let mut kept: HashSet<String> = HashSet::new();
    for tr in &turns {
        let cost = util::est_tokens(&tr.digest);
        if used + cost <= budget {
            used += cost;
            kept.insert(turn_rid(&tr.platform, &tr.turn_id));
        }
    }
    let sessions: Vec<Value> = order
        .iter()
        .map(|k| {
            let ts = &groups[k];
            let mut items: Vec<Value> = ts
                .iter()
                .filter(|tr| kept.contains(&turn_rid(&tr.platform, &tr.turn_id)))
                .map(|tr| json!({"turnId": tr.turn_id, "timestamp": tr.started_at, "digest": tr.digest}))
                .collect();
            items.reverse(); // chronological inside a session
            json!({
                "platform": k.0, "session": k.1, "project": ts.first().map(|t| t.project.clone()),
                "summary": summaries.get(k).cloned(),
                "turns": items,
                "totalTurns": ts.len(),
            })
        })
        .collect();
    Ok(json!({"sessions": sessions, "tokensUsed": used}))
}
