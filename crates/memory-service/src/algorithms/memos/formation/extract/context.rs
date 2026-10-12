//! Visible recall and prompt context for extraction: old memories, known entities and the conversation.
use super::super::prompts;
use crate::algorithms::memos::repo;
use crate::retrieval::visibility::visible;
use crate::runtime::{engine::Engine, tenant::Tenant};
use anyhow::Result;
use memory_domain::types::*;
use memory_fulltext::FtFilter;
use memory_rules::util;
use std::collections::HashMap;
use std::sync::Arc;

/// Three independent ranked channels, fused with RRF k=60; re-check authoritative scope/status.
pub async fn recall(
    eng: &Arc<Engine>,
    t: &Arc<Tenant>,
    query: &str,
    project: &str,
    platform: &str,
    mtype: Option<&str>,
    limit: usize,
) -> Result<Vec<MemoryRec>> {
    let all = repo::current_memories(t).await?;
    let candidates: HashMap<String, MemoryRec> = all
        .into_iter()
        .filter(|m| {
            visible(&m.scope, project, platform) && mtype.map(|v| m.mtype == v).unwrap_or(true)
        })
        .map(|m| (m.id.clone(), m))
        .collect();
    let q = eng.models.embed_one(query).await?;
    // Earlier batches have committed their primary records, but their asynchronous index tasks may
    // still be pending. Fill only the missing vectors so formation sees those records immediately.
    let missing: Vec<_> = {
        let cache = t.vecs.read().await;
        candidates
            .values()
            .filter(|m| !cache.memories.contains_key(&m.id))
            .filter_map(|m| {
                crate::runtime::indexer::vector_text(m).map(|text| (m.id.clone(), text))
            })
            .collect()
    };
    if !missing.is_empty() {
        let texts = missing
            .iter()
            .map(|(_, text)| text.clone())
            .collect::<Vec<_>>();
        let vectors = eng.models.embed(&texts).await?;
        let mut cache = t.vecs.write().await;
        for ((id, _), vector) in missing.into_iter().zip(vectors) {
            cache.memories.insert(id, vector);
        }
    }
    let cache = t.vecs.read().await;
    let mut dense: Vec<(String, f32)> = candidates
        .keys()
        .filter_map(|id| {
            cache
                .memories
                .get(id)
                .map(|v| (id.clone(), util::cosine(&q, v)))
        })
        .collect();
    drop(cache);
    dense.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    dense.truncate(30);
    let filter = FtFilter {
        kinds: vec!["memory".into()],
        statuses: vec![S_CURRENT.into()],
        types: mtype.map(|v| vec![v.into()]).unwrap_or_default(),
        visible: vec![
            ("scope_kind".into(), "global".into()),
            ("project".into(), project.into()),
            ("platform".into(), platform.into()),
        ],
        ..Default::default()
    };
    let sparse = t.ft.search(query, &filter, 30)?;
    let hits = t.entities.read().await.longest_match(query);
    let mut entity: Vec<(String, usize)> = candidates
        .values()
        .map(|m| {
            (
                m.id.clone(),
                m.entity_ids.iter().filter(|id| hits.contains(id)).count(),
            )
        })
        .filter(|(_, n)| *n > 0)
        .collect();
    entity.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut scores: HashMap<String, f64> = HashMap::new();
    for channel in [
        dense.iter().map(|x| x.0.clone()).collect::<Vec<_>>(),
        sparse.into_iter().map(|x| x.0).collect(),
        entity.into_iter().take(30).map(|x| x.0).collect(),
    ] {
        for (rank, id) in channel.iter().enumerate() {
            if candidates.contains_key(id) {
                *scores.entry(id.clone()).or_default() += 1.0 / (61.0 + rank as f64);
            }
        }
    }
    let mut ranked: Vec<_> = scores.into_iter().collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    Ok(ranked
        .into_iter()
        .take(limit)
        .filter_map(|(id, _)| candidates.get(&id).cloned())
        .collect())
}

pub(super) async fn headers(
    t: &Arc<Tenant>,
    old: &[MemoryRec],
    query: &str,
    project: &str,
    platform: &str,
    p: &prompts::PromptSet,
) -> Result<String> {
    let mut ids = t.entities.read().await.longest_match(query);
    for m in old {
        for id in &m.entity_ids {
            if !ids.contains(id) {
                ids.push(id.clone());
            }
        }
    }
    let ents: Vec<EntityRec> = t.store.entities().await?;
    let known = ents
        .iter()
        .filter(|e| ids.contains(&e.id) && visible(&e.scope, project, platform))
        .map(|e| format!("{} | {}", e.name, e.aliases.join(", ")))
        .collect::<Vec<_>>()
        .join("\n");
    let rows = old
        .iter()
        .enumerate()
        .map(|(i, m)| {
            format!(
                "M{} | {} | {} | {} | {}",
                i + 1,
                m.effective_at,
                m.scope.kind,
                m.source.origin,
                m.text
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(format!(
        "{}\n\n{}",
        prompts::block(
            p.l.known_entities,
            if known.is_empty() { p.l.none } else { &known }
        ),
        prompts::block(
            p.l.old_memories,
            if rows.is_empty() { p.l.none } else { &rows }
        )
    ))
}

pub fn conversation(turns: &[TurnRec], p: &prompts::PromptSet) -> String {
    turns
        .iter()
        .map(|tr| {
            format!(
                "[{} | {}]\n{}{}\n{}{}",
                tr.turn_id,
                tr.user_ts,
                p.l.user,
                tr.user_message,
                p.l.assistant,
                tr.assistant_message
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}
