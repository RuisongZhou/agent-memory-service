//! Node SDK 3.3.1 semantic candidate recall with keyword and entity score fusion.
use super::config::Mem0Config;
use super::repo::{self, InputFilter};
use anyhow::{anyhow, Result};
use memory_config::MemoryStrategy;
use memory_domain::types::*;
use memory_engine::runtime::{engine::Engine, instance::Instance};
use memory_rules::selection::RERANK_POOL;
use memory_rules::util;
use memory_store::schema;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Instant,
};

/// Shared vector recall for formation and query retrieval. Scores are normalized cosine.
pub async fn dense(
    eng: &Arc<Engine>,
    t: &Arc<Instance>,
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
    t: &Arc<Instance>,
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
            hits = t
                .store
                .nearest_memories(query, k)
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

/// The existing facts visible to `project` and `platform` nearest to `text`, by pure vector recall as formation
/// recalls existing facts.
pub async fn nearest_facts(
    eng: &Arc<Engine>,
    t: &Arc<Instance>,
    project: &str,
    platform: &str,
    text: &str,
    limit: usize,
) -> Result<Vec<(MemoryRec, f32)>> {
    dense(
        eng,
        t,
        text,
        project,
        platform,
        &[T_FACT.into()],
        -1.0,
        limit,
    )
    .await
}

pub async fn search(
    eng: &Arc<Engine>,
    t: &Arc<Instance>,
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
    // The project `""` is no project: only global and platform memories, as for a session with no project folder.
    let project = if scope.is_global()? {
        ""
    } else {
        scope.project.as_deref().unwrap_or("*")
    };
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
    let candidates = dense_with_vector(
        t,
        &q,
        project,
        platform,
        &types,
        f32::NEG_INFINITY,
        (k * 4).max(60),
        eng.cfg.brute_force_limit,
    )
    .await?;
    // Keyword hits contribute scores only; they never expand the semantic pool.
    let documents = keyword_documents(t, project, platform, &types).await?;
    let query_lemmatized = memory_rules::mem0_hybrid::lemmatize(query);
    let bm25 =
        memory_rules::mem0_hybrid::bm25_scores(&documents, &query_lemmatized, (k * 4).max(60));
    // Selective recall reranks the fused top of the pool and returns only what clears the reranker's floor. Without a
    // reranker, or when the request turns reranking off, search keeps Mem0's hybrid top-k with its entity boost.
    let rerank = req.rerank.unwrap_or(mem0.rerank);
    let selection = mem0.recall.filter(|_| rerank && eng.models.has_reranker());
    let explaining = req.explain.unwrap_or(false);
    let entity = if selection.is_none() || explaining {
        super::entities::boosts(eng, t, query, project, platform).await?
    } else {
        HashMap::new()
    };
    let no_entity = HashMap::new();
    let mut ranked = memory_rules::mem0_hybrid::score_and_rank(
        candidates
            .iter()
            .map(|(m, score)| (m.id.clone(), *score))
            .collect(),
        &bm25,
        if selection.is_some() {
            &no_entity
        } else {
            &entity
        },
        mem0.threshold,
        usize::MAX,
    );
    let explained = if explaining {
        let signals = Signals {
            pool: &candidates,
            fused: &ranked,
            bm25: &bm25,
            entity: &entity,
        };
        Some(explain(eng, t, query, &q, signals, project, platform, &types).await?)
    } else {
        None
    };
    ranked.truncate(if selection.is_some() { RERANK_POOL } else { k });
    let records: std::collections::HashMap<_, _> = candidates
        .into_iter()
        .map(|(m, _)| (m.id.clone(), m))
        .collect();
    let mut candidates: Vec<_> = ranked
        .into_iter()
        .filter_map(|(id, score)| records.get(&id).cloned().map(|m| (m, score)))
        .collect();
    let recall_ms = tick.elapsed().as_secs_f64() * 1000.0;
    let tick = Instant::now();
    let mut rerank_scores = HashMap::new();
    let mut selected = Value::Null;
    if let Some(rule) = selection {
        let docs = candidates
            .iter()
            .map(|(m, _)| m.text.clone())
            .collect::<Vec<_>>();
        // The reranker vouches for what selective search returns: when it fails, the search fails.
        let scores = eng.models.rerank(query, &docs).await?.unwrap_or_default();
        let top = scores.iter().copied().reduce(f32::max);
        let scored: Vec<_> = candidates.into_iter().zip(scores).collect();
        let admitted = memory_rules::selection::select(&scored, rule);
        selected = json!({"floor":rule.floor,"margin":rule.margin,"reranked":scored.len(),"top":top,
            "admitted":admitted.len()});
        candidates = admitted
            .into_iter()
            .map(|((m, fused), score)| {
                rerank_scores.insert(m.id.clone(), score);
                (m, fused)
            })
            .collect();
    }
    let rerank_ms = tick.elapsed().as_secs_f64() * 1000.0;
    candidates.truncate(k);
    let tick = Instant::now();
    // Read authoritative state as one batch after optional model work. Keep the
    // selected order and scores; concurrent corrections/deletions are filtered
    // by current primary records rather than trusting the earlier recall snapshot.
    let selected_ids: Vec<_> = candidates.iter().map(|(m, _)| m.id.clone()).collect();
    let mut authoritative: HashMap<_, _> = t
        .store
        .get_memories(&selected_ids)
        .await?
        .into_iter()
        .map(|m| (m.id.clone(), m))
        .collect();
    let mut used = 0;
    let mut results = Vec::new();
    for (selected, score) in candidates {
        let Some(m) = authoritative.remove(&selected.id) else {
            continue;
        };
        if !matches!(m.status.as_str(), S_CURRENT | S_CONFLICT)
            || !visible(&m.scope, project, platform)
        {
            continue;
        }
        let cost = util::est_tokens(&m.text) + 8;
        if used + cost > budget {
            continue;
        }
        used += cost;
        let mut item = json!({"memory":m.public(), "score":score, "slot":results.len()});
        if let Some(score) = rerank_scores.get(&m.id) {
            item["rerankScore"] = json!(score);
        }
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
    // Shaped like `get_context`'s rows, so the recall log can filter both by who searched and where.
    let ts = util::now_str();
    let retrieval_id = t
        .store
        .log_retrieval(
            json!({"kind":"search","platform":c.platform,"client":c.client,"project":project,"session":scope.session,
        "query":query,"strategy":MemoryStrategy::Mem0,"selection":selected,
        "returned":{"memories":returned,"tokens":used,"gap":results.is_empty()},"latency":lat,
        "createdAt":ts,"createdTs":util::ts_secs(&ts)}),
        )
        .await?;
    let mut out = json!({"retrievalId":retrieval_id,"strategy":"mem0","results":results,"profiles":[],
        "recentTurns":[],"uncertainties":[],"gap":results.is_empty(),"tokensUsed":used,"latencyMs":lat,
        "selection":selected});
    if let Some(explained) = explained {
        out["explain"] = explained;
    }
    Ok(out)
}

/// Candidates reranked for `explain`: the fused top of the pool.
const EXPLAIN_RERANK: usize = 30;

struct Signals<'a> {
    pool: &'a [(MemoryRec, f32)],
    fused: &'a [(String, f32)],
    bm25: &'a HashMap<String, f32>,
    entity: &'a HashMap<String, f32>,
}

/// Eval diagnostics: each pool candidate's cosine, keyword, entity and fused scores (fused order first), cross-encoder
/// scores for the fused top `EXPLAIN_RERANK` when a reranker is configured, and the query's cosine over every visible
/// candidate memory, so that selection rules can be compared offline from one search.
#[allow(clippy::too_many_arguments)]
async fn explain(
    eng: &Arc<Engine>,
    t: &Arc<Instance>,
    query: &str,
    q: &[f32],
    s: Signals<'_>,
    project: &str,
    platform: &str,
    types: &[String],
) -> Result<Value> {
    let fused: HashMap<&str, f32> = s.fused.iter().map(|(id, v)| (id.as_str(), *v)).collect();
    let mut pool: Vec<&(MemoryRec, f32)> = s.pool.iter().collect();
    pool.sort_by(|a, b| {
        let (fa, fb) = (fused.get(a.0.id.as_str()), fused.get(b.0.id.as_str()));
        fb.partial_cmp(&fa)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.1.total_cmp(&a.1))
    });
    let top: Vec<String> = pool
        .iter()
        .filter(|(m, _)| fused.contains_key(m.id.as_str()))
        .take(EXPLAIN_RERANK)
        .map(|(m, _)| m.text.clone())
        .collect();
    let rerank = if eng.models.has_reranker() && !top.is_empty() {
        eng.models.rerank(query, &top).await?
    } else {
        None
    };
    let candidates: Vec<Value> = pool
        .iter()
        .enumerate()
        .map(|(i, (m, cosine))| {
            json!({"id":m.id,"text":m.text,"tokens":util::est_tokens(&m.text) + 8,"cosine":cosine,
                "bm25":s.bm25.get(&m.id),"entity":s.entity.get(&m.id),"fused":fused.get(m.id.as_str()),
                "rerank":rerank.as_ref().and_then(|r| r.get(i))})
        })
        .collect();
    let metadata = t.store.active_memory_meta().await?;
    let cache = t.vecs.read().await;
    let mut all = Vec::new();
    for meta in metadata.iter() {
        let scope: MemScope = serde_json::from_value(meta["scope"].clone())?;
        if visible(&scope, project, platform)
            && meta["status"] != S_CANDIDATE
            && types.iter().any(|kind| meta["type"] == kind.as_str())
        {
            if let Some(v) = meta["mid"].as_str().and_then(|id| cache.memories.get(id)) {
                all.push(util::cosine(q, v) as f64);
            }
        }
    }
    let n = all.len().max(1) as f64;
    let mean = all.iter().sum::<f64>() / n;
    let std = (all.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n).sqrt();
    Ok(json!({"visible":all.len(),"cosineMean":mean,"cosineStd":std,"candidates":candidates}))
}

/// Default SQLite SDK keyword search scans every filtered record. Its corpus
/// statistics must not be restricted to the semantic over-fetch pool.
async fn keyword_documents(
    t: &Arc<Instance>,
    project: &str,
    platform: &str,
    types: &[String],
) -> Result<Vec<(String, String)>> {
    let meta = t.store.active_memory_meta().await?;
    let mut ids = Vec::new();
    for row in meta.iter() {
        let scope: MemScope = serde_json::from_value(row["scope"].clone())?;
        if visible(&scope, project, platform)
            && matches!(row["status"].as_str(), Some(S_CURRENT | S_CONFLICT))
            && types.iter().any(|kind| row["type"] == kind.as_str())
        {
            if let Some(id) = row["mid"].as_str() {
                ids.push(id.to_owned());
            }
        }
    }
    ids.sort();
    let records = t.store.get_memories(&ids).await?;
    Ok(records
        .into_iter()
        .filter(|m| {
            matches!(m.status.as_str(), S_CURRENT | S_CONFLICT)
                && visible(&m.scope, project, platform)
        })
        .map(|m| {
            let normalized = memory_rules::mem0_hybrid::lemmatize(&m.text);
            let text = if normalized.is_empty() {
                m.text
            } else {
                normalized
            };
            (m.id, text)
        })
        .collect())
}

fn visible(scope: &MemScope, project: &str, platform: &str) -> bool {
    (project == "*" && scope.kind == "project")
        || (platform == "*" && scope.kind == "platform")
        || memory_engine::retrieval::visibility::visible(scope, project, platform)
}
/// Inspect complete raw inputs independently of whether extraction succeeded.
pub async fn list_inputs(
    eng: &Arc<Engine>,
    t: &Arc<Instance>,
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
            // A global session's requests are filed under the project `""`.
            project: if req.scope.is_global()? {
                Some("")
            } else {
                req.scope.project.as_deref().filter(|s| *s != "*")
            },
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
