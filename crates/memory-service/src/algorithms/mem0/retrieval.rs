//! Semantic-only search. No generative calls, broad summaries or implicit cross-encoder.
use super::config::Mem0Config;
use super::repo::{self, InputFilter};
use crate::domain::types::*;
use crate::runtime::{config::MemoryStrategy, engine::Engine, tenant::Tenant};
use crate::storage::schema;
use anyhow::{anyhow, Result};
use memory_rules::util;
use serde_json::{json, Value};
use std::{collections::HashSet, sync::Arc, time::Instant};

/// Shared vector recall for formation and query retrieval. Scores are normalized cosine.
pub async fn dense(
    eng: &Arc<Engine>,
    t: &Arc<Tenant>,
    query: &str,
    project: &str,
    platform: &str,
    types: &[String],
    threshold: f32,
    limit: usize,
) -> Result<Vec<(MemoryRec, f32)>> {
    let q = eng.models.embed_one(query).await?;
    dense_with_vector(
        t,
        &q,
        project,
        platform,
        types,
        threshold,
        limit,
        eng.cfg.brute_force_limit,
    )
    .await
}

pub async fn dense_with_vector(
    t: &Arc<Tenant>,
    query: &[f32],
    project: &str,
    platform: &str,
    types: &[String],
    threshold: f32,
    limit: usize,
    brute_force_limit: usize,
) -> Result<Vec<(MemoryRec, f32)>> {
    let metadata = t.store.active_memory_meta().await?;
    let mut ids = HashSet::new();
    for meta in metadata.iter() {
        let scope: MemScope = serde_json::from_value(meta["scope"].clone())?;
        if visible(&scope, project, platform)
            && meta["status"] != S_CANDIDATE
            && types.iter().any(|kind| meta["type"] == kind.as_str())
        {
            if let Some(id) = meta["mid"].as_str() {
                ids.insert(id.to_string());
            }
        }
    }
    let scored = if ids.len() > brute_force_limit {
        // Reuse the existing ANN index and bounded over-fetch policy; inactive/out-of-scope
        // records are never admitted just because the vector index still contains them.
        let mut k = limit * 2;
        let mut hits = Vec::new();
        for attempt in 0..3 {
            hits = crate::retrieval::vector::hnsw_search(t, query, k)
                .await?
                .into_iter()
                .filter(|(id, _)| ids.contains(id))
                .collect();
            if hits.len() >= limit || attempt == 2 {
                break;
            }
            k *= 2;
        }
        hits
    } else {
        let cache = t.vecs.read().await;
        ids.iter()
            .filter_map(|id| {
                cache
                    .memories
                    .get(id)
                    .map(|v| (id.clone(), util::cosine(query, v)))
            })
            .collect()
    };
    let ranked = memory_rules::mem0::rank(scored, threshold, limit);
    let ids: Vec<_> = ranked.iter().map(|(id, _)| id.clone()).collect();
    let records = t.store.get_memories(&ids).await?;
    let records: std::collections::HashMap<_, _> =
        records.into_iter().map(|m| (m.id.clone(), m)).collect();
    Ok(ranked
        .into_iter()
        .filter_map(|(id, score)| records.get(&id).cloned().map(|m| (m, score)))
        .filter(|(m, _)| {
            matches!(m.status.as_str(), S_CURRENT | S_CONFLICT)
                && visible(&m.scope, project, platform)
        })
        .collect())
}

pub async fn search(
    eng: &Arc<Engine>,
    t: &Arc<Tenant>,
    c: &Caller,
    req: SearchReq,
) -> ApiResult<Value> {
    let start = Instant::now();
    let query = req.query.trim();
    if query.is_empty() {
        return Err(ApiError::InvalidArgument("query is required".into()));
    }
    let mem0 = Mem0Config::from(&eng.cfg);
    let k = req.top_k.unwrap_or(mem0.top_k);
    let budget = req.token_budget.unwrap_or(eng.cfg.default_token_budget);
    if k == 0 || k > eng.cfg.max_top_k || budget == 0 {
        return Err(ApiError::InvalidArgument(
            "invalid topK or tokenBudget".into(),
        ));
    }
    let scope = req.scope.unwrap_or_default();
    if scope
        .platform
        .as_deref()
        .is_some_and(|p| p != "*" && p != c.platform)
    {
        return Err(ApiError::InvalidArgument(
            "platform is supplied by the connection".into(),
        ));
    }
    let project = scope.project.as_deref().unwrap_or("*");
    let platform = if scope.platform.as_deref() == Some("*") {
        "*"
    } else {
        &c.platform
    };
    let types = req.types.unwrap_or_else(|| vec![T_FACT.into()]);
    if types.iter().any(|kind| kind == T_INSTRUCTION) {
        return Err(ApiError::InvalidArgument(
            "instructions are selected by get_context".into(),
        ));
    }
    let tick = Instant::now();
    let q = eng.models.embed_one(query).await?;
    let embed_ms = tick.elapsed().as_secs_f64() * 1000.0;
    let tick = Instant::now();
    let mut candidates = dense_with_vector(
        t,
        &q,
        project,
        platform,
        &types,
        mem0.threshold,
        (k * 4).max(60),
        eng.cfg.brute_force_limit,
    )
    .await?;
    let recall_ms = tick.elapsed().as_secs_f64() * 1000.0;
    let tick = Instant::now();
    let rerank = req.rerank.unwrap_or(mem0.rerank);
    if rerank && !candidates.is_empty() {
        if !eng.models.has_reranker() {
            return Err(ApiError::InvalidArgument(
                "rerank needs mem0Rerank=true at startup".into(),
            ));
        }
        let docs = candidates
            .iter()
            .map(|(m, _)| m.text.clone())
            .collect::<Vec<_>>();
        if let Some(scores) = eng.models.rerank(query, &docs).await? {
            for (item, score) in candidates.iter_mut().zip(scores) {
                item.1 = score;
            }
            candidates.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.id.cmp(&b.0.id)));
        }
    }
    let rerank_ms = tick.elapsed().as_secs_f64() * 1000.0;
    candidates.truncate(k);
    let tick = Instant::now();
    let mut used = 0;
    let mut results = Vec::new();
    for (m, score) in candidates {
        let cost = util::est_tokens(&m.text) + 8;
        if used + cost > budget {
            continue;
        }
        used += cost;
        let mut item = json!({"memory":m.public(), "score":score, "slot":results.len()});
        if req.include_evidence.unwrap_or(false) {
            // A batch reference is explicitly distinguished from a verified verbatim claim span.
            let provenance = t.store.get_doc(super::SOURCES, &m.id).await?;
            item["sourceBatch"] = provenance.unwrap_or(Value::Null);
        }
        if req.touch.unwrap_or(false) {
            t.store
                .merge_doc(
                    schema::MEMORIES,
                    &m.id,
                    json!({"lastAccessedAt":util::now_str()}),
                )
                .await?;
        }
        results.push(item);
    }
    let assembly_ms = tick.elapsed().as_secs_f64() * 1000.0;
    let returned = results
        .iter()
        .enumerate()
        .map(|(i, r)| json!({"id":r["memory"]["id"],"rank":i+1}))
        .collect::<Vec<_>>();
    let lat = json!({"embed":embed_ms,"recall":recall_ms,"rerank":rerank_ms,
        "assemble":assembly_ms,"total":start.elapsed().as_secs_f64()*1000.0});
    let retrieval_id = t
        .store
        .log(
            schema::RETRIEVAL_LOG,
            json!({"query":query,"strategy":MemoryStrategy::Mem0,
        "createdAt":util::now_str(),"returned":{"memories":returned},"latency":lat}),
        )
        .await?;
    Ok(
        json!({"retrievalId":retrieval_id,"strategy":"mem0","results":results,"profiles":[],
        "recentTurns":[],"uncertainties":[],"gap":results.is_empty(),"tokensUsed":used,"latencyMs":lat}),
    )
}

fn visible(scope: &MemScope, project: &str, platform: &str) -> bool {
    (project == "*" && scope.kind == "project")
        || (platform == "*" && scope.kind == "platform")
        || crate::retrieval::visibility::visible(scope, project, platform)
}
/// Inspect complete raw inputs independently of whether extraction succeeded.
pub async fn list_inputs(
    eng: &Arc<Engine>,
    t: &Arc<Tenant>,
    c: &Caller,
    req: ListTurnsReq,
) -> ApiResult<Value> {
    let budget = req.token_budget.unwrap_or(eng.cfg.default_token_budget);
    if budget == 0 {
        return Err(ApiError::InvalidArgument(
            "tokenBudget must be positive".into(),
        ));
    }
    let since = req
        .since
        .as_deref()
        .map(str::trim)
        .filter(|since| !since.is_empty())
        .map(|since| {
            util::parse_ts_fixed(since).ok_or_else(|| {
                ApiError::InvalidArgument(format!("since is not RFC3339: {since:?}"))
            })
        })
        .transpose()?;
    let platform = if req.scope.platform.as_deref() != Some("*") {
        if req
            .scope
            .platform
            .as_deref()
            .is_some_and(|s| s != c.platform)
        {
            return Err(ApiError::InvalidArgument(
                "platform is supplied by the connection".into(),
            ));
        }
        Some(c.platform.as_str())
    } else {
        None
    };
    let rows = repo::list_inputs(
        t,
        InputFilter {
            project: req.scope.project.as_deref().filter(|s| *s != "*"),
            platform,
            session: req.scope.session.as_deref().filter(|s| *s != "*"),
            turn_ids: req.turn_ids,
        },
    )
    .await?;
    let mut turns = Vec::new();
    let mut used = 0;
    for row in rows {
        let messages: Vec<InMessage> =
            serde_json::from_value(row["inputMessages"].clone()).map_err(anyhow::Error::from)?;
        if messages.is_empty() {
            continue;
        }
        if let Some(since) = since {
            let ended = util::parse_ts_fixed(&messages.last().unwrap().timestamp)
                .ok_or_else(|| anyhow!("stored input timestamp is not RFC3339"))?;
            if ended < since {
                continue;
            }
        }
        let cost = messages
            .iter()
            .map(|m| util::est_tokens(&m.content))
            .sum::<usize>();
        if used + cost > budget {
            continue;
        }
        used += cost;
        turns.push(json!({"turnId":row["turnId"],"requestId":row["requestId"],"platform":row["platform"],
            "project":row["project"],"session":row["session"],"messages":messages,
            "startedAt":messages.first().unwrap().timestamp,"endedAt":messages.last().unwrap().timestamp,
            "complete":true,"extractState":row["status"]}));
    }
    Ok(json!({"turns":turns,"tokensUsed":used}))
}
