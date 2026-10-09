//! P1/P1-b: visible recall, documented prompt layout, deterministic validation and atomic formation.
use super::{engine::Engine, prompts, relations, tenant::Tenant, types::*, util};
use anyhow::{anyhow, Result};
use memory_fulltext::FtFilter;
use relations::{
    plan::{Rel, Target},
    Applied, FactSource, NewFact, WriteCtx,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub fn visible(scope: &MemScope, project: &str, platform: &str) -> bool {
    scope.kind == "global"
        || (scope.kind == "project" && scope.project.as_deref() == Some(project))
        || (scope.kind == "platform" && scope.platform.as_deref() == Some(platform))
}

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
    let all = t.store.find_memories("status = 'current'", vec![]).await?;
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
            .filter_map(|m| super::indexer::vector_text(m).map(|text| (m.id.clone(), text)))
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

/// Persist stage output/final failure; the LLM client durably accounts for every billed reply.
pub async fn call_logged(
    eng: &Arc<Engine>,
    t: &Arc<Tenant>,
    stage: &str,
    system: &str,
    user: &str,
    max_tokens: u32,
) -> Result<Value> {
    let reply = eng
        .llm
        .json_call_logged(system, user, max_tokens, &t.store, stage)
        .await;
    let now = util::now_str();
    match reply {
        Ok(r) => {
            t.store
                .log(
                    "extract_log",
                    json!({"stage":stage,"createdAt":now,"variant":eng.prompts_for(t).variant,
                "systemHash":blake3::hash(system.as_bytes()).to_hex().to_string(),"inputHash":blake3::hash(user.as_bytes()).to_hex().to_string(),
                "outputHash":blake3::hash(r.json.to_string().as_bytes()).to_hex().to_string(),"output":r.json,"status":"model_completed"}),
                )
                .await?;
            Ok(r.json)
        }
        Err(e) => {
            t.store
                .log(
                    "extract_log",
                    json!({"stage":stage,"createdAt":now,"status":"failed","error":e.to_string()}),
                )
                .await?;
            Err(e)
        }
    }
}

#[derive(Debug, Deserialize)]
struct RawFact {
    text: String,
    scope: String,
    entities: Vec<String>,
    #[serde(rename = "effectiveAt")]
    effective_at: String,
    origin: String,
    source: RawSource,
    #[serde(deserialize_with = "parse_raw_relations")]
    relations: Vec<RawRelation>,
    #[serde(default)]
    reason: Option<String>,
}
#[derive(Debug, Deserialize)]
struct RawSource {
    #[serde(rename = "turnId")]
    turn_id: String,
    span: String,
}
#[derive(Debug, Deserialize)]
struct RawRelation {
    // Both spellings identify the same reference. Real JSON-mode replies can use
    // target/type because the verbatim prompt describes relations without a JSON shape.
    // Serde rejects ambiguous objects containing both aliases; all semantic gates still apply.
    #[serde(rename = "memoryId", alias = "target")]
    memory_id: String,
    #[serde(alias = "type")]
    relation: String,
}

fn parse_raw_relations<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Vec<RawRelation>, D::Error> {
    let values = Vec::<Value>::deserialize(d)?;
    values
        .into_iter()
        .map(|v| {
            if let Value::String(label) = v {
                // Missing targets stay invalid: non-destructive edges are removed by the
                // existing reference gate, while updates/conflicts reject the fact.
                Ok(RawRelation {
                    memory_id: String::new(),
                    relation: label,
                })
            } else {
                serde_json::from_value(v).map_err(serde::de::Error::custom)
            }
        })
        .collect()
}

fn collapsed(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whitespace normalisation is the sole permitted change when matching a supporting span.
fn supporting_source(
    raw: &RawFact,
    turns: &[TurnRec],
    text: Option<(&str, &str)>,
    first_hand: bool,
) -> Option<FactSource> {
    let span = collapsed(&raw.source.span);
    if span.is_empty() {
        return None;
    }
    if let Some((content, now)) = text {
        if raw.origin != ORIGIN_EXPLICIT || raw.source.turn_id != "text" {
            return None;
        }
        let pos = collapsed(content).find(&span)?;
        return Some(FactSource {
            turn_id: "text".into(),
            span: raw.source.span.clone(),
            role: "text".into(),
            stated_at: now.into(),
            stated: (util::ts_secs(now), pos as i64),
        });
    }
    let tr = turns.iter().find(|tr| tr.turn_id == raw.source.turn_id)?;
    let choices = match raw.origin.as_str() {
        ORIGIN_EXPLICIT if first_hand => vec![
            ("user", &tr.user_message, &tr.user_ts, 0),
            (
                "assistant",
                &tr.assistant_message,
                &tr.assistant_ts,
                1_000_000,
            ),
        ],
        ORIGIN_EXPLICIT => vec![("user", &tr.user_message, &tr.user_ts, 0)],
        ORIGIN_USER if first_hand => vec![
            ("user", &tr.user_message, &tr.user_ts, 0),
            (
                "assistant",
                &tr.assistant_message,
                &tr.assistant_ts,
                1_000_000,
            ),
        ],
        ORIGIN_USER => vec![("user", &tr.user_message, &tr.user_ts, 0)],
        ORIGIN_ASSISTANT => vec![(
            "assistant",
            &tr.assistant_message,
            &tr.assistant_ts,
            1_000_000,
        )],
        _ => return None,
    };
    choices.into_iter().find_map(|(role, body, ts, offset)| {
        collapsed(body).find(&span).map(|pos| FactSource {
            turn_id: tr.turn_id.clone(),
            span: raw.source.span.clone(),
            role: role.into(),
            stated_at: ts.clone(),
            stated: (util::ts_secs(ts), offset + pos as i64),
        })
    })
}

async fn validate(
    t: &Arc<Tenant>,
    output: Value,
    old: &[MemoryRec],
    turns: &[TurnRec],
    text: Option<(&str, &str)>,
    project: &str,
    platform: &str,
    first_hand: bool,
) -> Result<(Vec<NewFact>, Vec<EntityRec>)> {
    let items = output
        .get("facts")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("P1 output requires facts array"))?;
    let mut rejected = Vec::new();
    let mut parsed: Vec<(usize, RawFact, FactSource)> = Vec::new();
    for (i, value) in items.iter().enumerate() {
        let mut r = match serde_json::from_value::<RawFact>(value.clone()) {
            Ok(r) => r,
            Err(_) => {
                rejected
                    .push(json!({"fact":i+1,"kind":"fact_discarded","reason":"schema_invalid"}));
                continue;
            }
        };
        // The prompt schema uses English labels; the design's older JSON example uses Chinese.
        r.origin = match r.origin.as_str() {
            "用户明确要求" => ORIGIN_EXPLICIT,
            "用户原话" => ORIGIN_USER,
            "助手回复" => ORIGIN_ASSISTANT,
            other => other,
        }
        .to_string();
        let reason = if r.text.trim().is_empty() {
            Some("empty_text")
        } else if !matches!(r.scope.as_str(), "global" | "project") {
            Some("scope_invalid")
        } else if r.source.span.chars().count() > 80 {
            Some("span_too_long")
        } else if r.relations.iter().any(|rel| {
            !matches!(
                rel.relation.as_str(),
                "same" | "updates" | "conflicts" | "related"
            )
        }) {
            Some("relation_label_invalid")
        } else if !util::is_valid_date(&r.effective_at) {
            Some("effective_date_invalid")
        } else if !matches!(
            r.origin.as_str(),
            ORIGIN_EXPLICIT | ORIGIN_USER | ORIGIN_ASSISTANT
        ) {
            Some("origin_invalid")
        } else if collapsed(&r.source.span).is_empty() {
            Some("empty_span")
        } else {
            None
        };
        if let Some(reason) = reason {
            rejected.push(json!({"fact":i+1,"kind":"fact_discarded","reason":reason}));
            continue;
        }
        if let Some(source) = supporting_source(&r, turns, text, first_hand) {
            parsed.push((i + 1, r, source));
        } else {
            let reason = if text.is_some() && r.origin != ORIGIN_EXPLICIT {
                "text_origin_not_explicit"
            } else if text.map(|_| r.source.turn_id != "text").unwrap_or(false)
                || (text.is_none() && !turns.iter().any(|tr| tr.turn_id == r.source.turn_id))
            {
                "source_turn_unknown"
            } else if text.is_none()
                && turns
                    .iter()
                    .find(|tr| tr.turn_id == r.source.turn_id)
                    .map(|tr| {
                        collapsed(&tr.user_message).contains(&collapsed(&r.source.span))
                            || collapsed(&tr.assistant_message).contains(&collapsed(&r.source.span))
                    })
                    .unwrap_or(false)
            {
                "origin_role_mismatch"
            } else {
                "span_not_verbatim"
            };
            rejected.push(json!({"fact":i+1,"kind":"fact_discarded","reason":reason}));
        }
    }
    let after_rules = parsed.len();
    // Reject destructive references to rejected facts transitively, before allocating entities.
    loop {
        let live: HashSet<usize> = parsed.iter().map(|x| x.0).collect();
        let before = parsed.len();
        parsed.retain(|(idx, r, _)| {
            let bad = r.relations.iter().any(|rel| {
                matches!(rel.relation.as_str(), "updates" | "conflicts")
                    && !valid_target(&rel.memory_id, *idx, &live, old)
            });
            if bad {
                rejected.push(json!({"fact":idx,"kind":"fact_discarded","reason":"destructive_reference_invalid"}));
            }
            !bad
        });
        if before == parsed.len() {
            break;
        }
    }
    let after_references = parsed.len();
    let mut canonical: HashMap<usize, usize> = HashMap::new();
    let mut text_indices: HashMap<String, usize> = HashMap::new();
    for (idx, r, _) in &parsed {
        let first = *text_indices.entry(r.text.clone()).or_insert(*idx);
        canonical.insert(*idx, first);
    }
    let live: HashSet<usize> = parsed.iter().map(|x| x.0).collect();
    let ents: Vec<EntityRec> = t
        .store
        .select_as("SELECT * OMIT id FROM entities", vec![])
        .await?;
    let mut names: HashMap<String, String> = HashMap::new();
    for e in ents.iter().filter(|e| visible(&e.scope, project, platform)) {
        for name in std::iter::once(&e.name).chain(e.aliases.iter()) {
            names.insert(memory_fulltext::normalize_name(name), e.id.clone());
        }
    }
    let mut new_entities = Vec::new();
    let mut facts: Vec<NewFact> = Vec::new();
    for (idx, r, source) in parsed {
        let fidx = canonical[&idx];
        let scope = if r.scope == "global" {
            MemScope::global()
        } else {
            MemScope::project(project)
        };
        let mut entity_ids = Vec::new();
        for rel in &r.relations {
            if !valid_target(&rel.memory_id, idx, &live, old) {
                rejected.push(json!({"fact":idx,"kind":"relation_removed","memoryId":rel.memory_id,"reason":"non_destructive_reference_invalid"}));
            }
        }
        for name in &r.entities {
            let normalized = memory_fulltext::normalize_name(name);
            if normalized.is_empty() {
                continue;
            }
            let id = names
                .entry(normalized)
                .or_insert_with(|| {
                    let id = util::new_id("ent");
                    new_entities.push(EntityRec {
                        id: id.clone(),
                        name: name.trim().into(),
                        scope: scope.clone(),
                        ..Default::default()
                    });
                    id
                })
                .clone();
            if !entity_ids.contains(&id) {
                entity_ids.push(id);
            }
        }
        let relations: Vec<Rel> = r
            .relations
            .iter()
            .filter(|rel| {
                matches!(
                    rel.relation.as_str(),
                    "same" | "updates" | "conflicts" | "related"
                ) && valid_target(&rel.memory_id, idx, &live, old)
            })
            .filter_map(|rel| {
                let target = if let Some(n) = rel
                    .memory_id
                    .strip_prefix('M')
                    .and_then(|n| n.parse::<usize>().ok())
                {
                    Target::Old(old[n - 1].id.clone())
                } else {
                    let n = rel.memory_id.strip_prefix('F')?.parse::<usize>().ok()?;
                    let n = canonical[&n];
                    if n == fidx {
                        rejected.push(json!({"fact":idx,"kind":"relation_removed","memoryId":rel.memory_id,"reason":"dedupe_self_reference"}));
                        return None;
                    }
                    Target::Batch(n)
                };
                Some(Rel {
                    target,
                    relation: rel.relation.clone(),
                })
            })
            .collect();
        if let Some(f) = facts.iter_mut().find(|f| f.fidx == fidx) {
            if origin_priority(&r.origin) > origin_priority(&f.origin) {
                f.origin = r.origin;
            }
            f.sources.push(source);
            f.relations.extend(relations);
            for id in entity_ids {
                if !f.entity_ids.contains(&id) {
                    f.entity_ids.push(id);
                }
            }
        } else {
            facts.push(NewFact {
                fidx,
                text: r.text,
                scope,
                entity_ids,
                effective_at: r.effective_at,
                origin: r.origin,
                sources: vec![source],
                relations,
                reason: r.reason,
            });
        }
    }
    let source_turn_ids = if text.is_some() {
        vec!["text".to_string()]
    } else {
        turns.iter().map(|tr| tr.turn_id.clone()).collect()
    };
    let groups=facts.iter().filter(|f|f.sources.len()>1).map(|f|json!({"retainedFact":f.fidx,
        "mergedFacts":canonical.iter().filter_map(|(idx,canon)|(*canon==f.fidx&&*idx!=f.fidx).then_some(*idx)).collect::<Vec<_>>(),
        "sourceCount":f.sources.len()})).collect::<Vec<_>>();
    t.store.log("extract_log",json!({"stage":if text.is_some(){"P1-b_validation"}else{"P1_validation"},
        "createdAt":util::now_str(),"outputHash":blake3::hash(output.to_string().as_bytes()).to_hex().to_string(),
        "groupLabel":format!("{}::{}::{}",platform,project,turns.first().map(|tr|tr.session.as_str()).unwrap_or("text")),
        "promptVariant":if first_hand{"en"}else{"zh"},"sourceTurnIds":source_turn_ids,
        "inputCounts":{"modelFacts":items.len(),"afterRules":after_rules,"afterReferences":after_references,"afterDedupe":facts.len()},
        "dedupe":{"mergedFacts":after_references-facts.len(),"groups":groups},
        "rejected":rejected,"accepted":facts.len()})).await?;
    Ok((facts, new_entities))
}
fn valid_target(id: &str, myself: usize, live: &HashSet<usize>, old: &[MemoryRec]) -> bool {
    if id.len() < 2 || !id.as_bytes()[1..].iter().all(|c| c.is_ascii_digit()) {
        return false;
    }
    if let Some(n) = id.strip_prefix('M').and_then(|n| n.parse::<usize>().ok()) {
        n > 0 && n <= old.len()
    } else if let Some(n) = id.strip_prefix('F').and_then(|n| n.parse::<usize>().ok()) {
        n != myself && live.contains(&n)
    } else {
        false
    }
}

async fn headers(
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
    let ents: Vec<EntityRec> = t
        .store
        .select_as("SELECT * OMIT id FROM entities", vec![])
        .await?;
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

/// Caller owns extract_lock; commit includes turns and receipts even when there are no accepted facts.
pub async fn batch(eng: &Arc<Engine>, t: &Arc<Tenant>, turns: Vec<TurnRec>) -> Result<Applied> {
    let first = turns
        .first()
        .ok_or_else(|| anyhow!("empty extraction batch"))?;
    let p = eng.prompts_for(t);
    let query = turns
        .iter()
        .map(TurnRec::full_text)
        .collect::<Vec<_>>()
        .join("\n");
    let old = recall(eng, t, &query, &first.project, &first.platform, None, 10).await?;
    let previous = t
        .store
        .session_turns(&first.platform, &first.session)
        .await?
        .into_iter()
        .filter(|tr| tr.seq < first.seq)
        .collect::<Vec<_>>();
    let prev = previous
        .iter()
        .rev()
        .take(2)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|tr| format!("[{} | {}]\n{}", tr.turn_id, tr.user_ts, tr.digest))
        .collect::<Vec<_>>()
        .join("\n");
    let user = format!(
        "{}\n\n{}\n\n{}",
        headers(t, &old, &query, &first.project, &first.platform, p).await?,
        prompts::block(
            p.l.previous,
            &format!(
                "{}\n{}",
                p.l.previous_note,
                if prev.is_empty() {
                    p.l.previous_none
                } else {
                    &prev
                }
            )
        ),
        prompts::block(p.l.batch, &conversation(&turns, p))
    );
    let output = call_logged(eng, t, "P1", p.p1, &user, 4096).await?;
    let (facts, new_entities) = validate(
        t,
        output,
        &old,
        &turns,
        None,
        &first.project,
        &first.platform,
        p.both_speakers_first_hand,
    )
    .await?;
    let platform = first.platform.clone();
    relations::apply(
        eng,
        t,
        facts,
        WriteCtx {
            platform,
            now: util::now_str(),
            turns: turns
                .into_iter()
                .map(|tr| (super::store::turn_rid(&tr.platform, &tr.turn_id), tr))
                .collect(),
            text_request: None,
            text_turn_id: None,
            new_entities,
        },
    )
    .await
}

pub async fn explicit(
    eng: &Arc<Engine>,
    t: &Arc<Tenant>,
    platform: &str,
    project: &str,
    text: &str,
    request: &str,
    now: &str,
) -> Result<Applied> {
    let p = eng.prompts_for(t);
    let old = recall(eng, t, text, project, platform, None, 10).await?;
    let user = format!(
        "{}\n\n<{} time=\"{}\">\n{}\n</{}>",
        headers(t, &old, text, project, platform, p).await?,
        p.l.remember,
        now,
        text,
        p.l.remember
    );
    let output = call_logged(eng, t, "P1-b", p.p1b, &user, 4096).await?;
    let (facts, new_entities) = validate(
        t,
        output,
        &old,
        &[],
        Some((text, now)),
        project,
        platform,
        p.both_speakers_first_hand,
    )
    .await?;
    relations::apply(
        eng,
        t,
        facts,
        WriteCtx {
            platform: platform.into(),
            now: util::now_str(),
            turns: vec![],
            text_request: Some(request.into()),
            text_turn_id: Some(request.into()),
            new_entities,
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    fn raw(origin: &str, span: &str) -> RawFact {
        RawFact {
            text: "fact".into(),
            scope: "global".into(),
            entities: vec![],
            effective_at: "2026-10-08".into(),
            origin: origin.into(),
            source: RawSource {
                turn_id: "T1".into(),
                span: span.into(),
            },
            relations: vec![],
            reason: None,
        }
    }
    #[test]
    fn provenance_checks_role_and_verbatim_span() {
        let tr = TurnRec {
            turn_id: "T1".into(),
            user_message: "port is 8100".into(),
            assistant_message: "port is 8200".into(),
            user_ts: "2026-10-08T00:00:00Z".into(),
            assistant_ts: "2026-10-08T00:00:01Z".into(),
            ..Default::default()
        };
        assert!(supporting_source(&raw("user", "8200"), &[tr.clone()], None, false).is_none());
        assert!(supporting_source(&raw("user", "8200"), &[tr.clone()], None, true).is_some());
        assert!(supporting_source(&raw("explicit", "8200"), &[tr.clone()], None, false).is_none());
        assert!(supporting_source(&raw("explicit", "8200"), &[tr.clone()], None, true).is_some());
        assert!(
            supporting_source(&raw("assistant", "is   8200"), &[tr.clone()], None, false).is_some()
        );
        assert!(supporting_source(&raw("assistant", "is 8201"), &[tr], None, false).is_none());
    }
    #[test]
    fn reference_validation() {
        let live = HashSet::from([1, 3]);
        let old = vec![MemoryRec::default()];
        assert!(valid_target("M1", 1, &live, &old));
        assert!(!valid_target("M0", 1, &live, &old));
        assert!(!valid_target("M+1", 1, &live, &old));
        assert!(!valid_target("中1", 1, &live, &old));
        assert!(valid_target("F3", 1, &live, &old));
        assert!(!valid_target("F2", 1, &live, &old));
        assert!(!valid_target("F1", 1, &live, &old));
    }
}

/// Deterministic formation/storage integration test. Uses supplied JSON output and mock embeddings;
/// the LLM client has no key and this test makes zero generative-model requests.
pub async fn formation_selftest() {
    let legacy: RawRelation =
        serde_json::from_value(json!({"target":"M1","type":"updates"})).unwrap();
    assert_eq!(legacy.memory_id, "M1");
    assert_eq!(legacy.relation, "updates");
    assert!(serde_json::from_value::<RawRelation>(
        json!({"target":"M1","memoryId":"M2","type":"updates"})
    )
    .is_err());
    assert!(serde_json::from_value::<RawRelation>(json!({"target":12,"type":"updates"})).is_err());
    use super::{
        config::ServiceConfig,
        indexer, ingest,
        llm::Llm,
        models::ModelHub,
        store::{self, to_doc, Tx},
    };
    assert_eq!(std::env::var("MEMORY_MOCK_MODELS").as_deref(), Ok("1"));
    super::experience::rule_selftest();
    let dir = std::env::temp_dir().join(format!("memory-formation-selftest-{}", util::new_id("t")));
    let eng = Engine::new(
        ServiceConfig {
            data_dir: dir.clone(),
            ..Default::default()
        },
        Llm::new("http://127.0.0.1:1", "", "disabled", 1, 1).unwrap(),
        ModelHub::load(std::path::Path::new("mock"), None).unwrap(),
    );
    let t = Arc::new(
        Tenant::open("formation-zh", &dir, eng.models.dim)
            .await
            .unwrap(),
    );
    let c = Caller {
        tenant: t.name.clone(),
        platform: "codex".into(),
    };
    let now = util::now_str();
    let scope = ReqScope {
        project: Some("p1".into()),
        session: Some("S1".into()),
        ..Default::default()
    };
    let req = AddMemoriesReq {
        scope: scope.clone(),
        turn_id: Some("T1".into()),
        messages: Some(vec![
            InMessage {
                role: "user".into(),
                content: "SurrealDB端口改成8100了，配置已同步。以后文档用简体中文。".into(),
                timestamp: "2026-10-08T10:00:00+08:00".into(),
            },
            InMessage {
                role: "assistant".into(),
                content: "配置已同步，测试通过。".into(),
                timestamp: "2026-10-08T10:00:01+08:00".into(),
            },
        ]),
        ..Default::default()
    };
    let receipt = ingest::add_memories(&eng, &t, &c, req.clone())
        .await
        .unwrap();
    assert_eq!(receipt["status"], "queued");
    let id = receipt["requestId"].as_str().unwrap();
    let duplicate = ingest::add_memories(&eng, &t, &c, req.clone())
        .await
        .unwrap();
    assert_eq!(duplicate["requestId"], id);
    assert_eq!(duplicate["duplicate"], true);
    assert_eq!(
        t.store
            .get_session("codex", "S1")
            .await
            .unwrap()
            .unwrap()
            .turn_count,
        1
    );
    let mut old = MemoryRec {
        id: "moldport".into(),
        mtype: T_FACT.into(),
        text: "SurrealDB端口为8000".into(),
        scope: MemScope::project("p1"),
        status: S_CURRENT.into(),
        effective_at: "2026-10-01".into(),
        recorded_at: now.clone(),
        stated_at: "2026-10-01T10:00:00+08:00".into(),
        evidence_count: 1,
        source: Source {
            platform: "codex".into(),
            origin: ORIGIN_USER.into(),
            turn_ids: vec!["T0".into()],
        },
        ..Default::default()
    };
    let mut tx = Tx::new();
    tx.put("memories", &old.id, to_doc(&old).unwrap()).unwrap();
    let entity = EntityRec {
        id: "entsurreal".into(),
        name: "SurrealDB".into(),
        scope: MemScope::project("p1"),
        ..Default::default()
    };
    tx.put("entities", &entity.id, to_doc(&entity).unwrap())
        .unwrap();
    let hidden = MemoryRec {
        id: "mhidden".into(),
        text: "SurrealDB hidden private port".into(),
        scope: MemScope::project("p2"),
        ..old.clone()
    };
    tx.put("memories", &hidden.id, to_doc(&hidden).unwrap())
        .unwrap();
    t.store.commit(tx).await.unwrap();
    let turns = t.store.session_turns("codex", "S1").await.unwrap();
    let harmless = json!({"facts":[{"text":"配置已同步","scope":"project","entities":[],"effectiveAt":"2026-10-08",
        "origin":"user","source":{"turnId":"T1","span":"配置已同步"},
        "relations":[{"memoryId":"M99","relation":"related"}]}]});
    let (accepted, _) = validate(
        &t,
        harmless,
        &[old.clone()],
        &turns,
        None,
        "p1",
        "codex",
        false,
    )
    .await
    .unwrap();
    assert_eq!(accepted.len(), 1);
    assert!(accepted[0].relations.is_empty());
    for label in ["related", "updates", "conflicts"] {
        let value = json!({"facts":[{"text":"配置已同步","scope":"project","entities":[],"effectiveAt":"2026-10-08",
            "origin":"user","source":{"turnId":"T1","span":"配置已同步"},"relations":[label]}]});
        let (facts, _) = validate(
            &t,
            value,
            &[old.clone()],
            &turns,
            None,
            "p1",
            "codex",
            false,
        )
        .await
        .unwrap();
        if label == "related" {
            assert_eq!(facts.len(), 1);
            assert!(facts[0].relations.is_empty());
        } else {
            assert!(facts.is_empty());
        }
    }
    let explicit = json!({"facts":[
        {"text":"SurrealDB端口为8100","scope":"project","entities":["ＳｕｒｒｅａｌＤＢ"],"effectiveAt":"2026-10-08",
            "origin":"explicit","source":{"turnId":"text","span":"端口8100"},"relations":[]},
        {"text":"错误来源不能写成主动记忆","scope":"global","entities":[],"effectiveAt":"2026-10-08",
            "origin":"user","source":{"turnId":"text","span":"端口8100"},"relations":[]}
    ]});
    let (accepted, entities) = validate(
        &t,
        explicit,
        &[old.clone()],
        &[],
        Some(("记住端口8100", &now)),
        "p1",
        "codex",
        false,
    )
    .await
    .unwrap();
    assert_eq!(accepted.len(), 1);
    assert_eq!(accepted[0].sources[0].stated_at, now);
    assert_eq!(accepted[0].entity_ids, vec!["entsurreal"]);
    assert!(entities.is_empty());
    let output = json!({"facts":[
        {"text":"SurrealDB端口为8100","scope":"project","entities":["SurrealDB"],"effectiveAt":"2026-10-07","origin":"用户原话",
            "source":{"turnId":"T1","span":"端口改成8100了"},"relations":[{"memoryId":"M1","relation":"updates"}]},
        {"text":"用户要求文档用简体中文","scope":"global","entities":[],"effectiveAt":"2026-10-08","origin":"explicit",
            "source":{"turnId":"T1","span":"以后文档用简体中文"},"relations":[]},
        {"text":"伪造事实","scope":"project","entities":[],"effectiveAt":"2026-10-08","origin":"assistant",
            "source":{"turnId":"T1","span":"以后文档用简体中文"},"relations":[]},
        {"text":"丢弃无效更新引用","scope":"project","entities":[],"effectiveAt":"2026-10-08","origin":"user",
            "source":{"turnId":"T1","span":"配置已同步"},"relations":[{"memoryId":"F3","relation":"updates"}]},
        {"text":"SurrealDB端口为8100","scope":"project","entities":["SurrealDB"],"effectiveAt":"2026-10-07","origin":"user",
            "source":{"turnId":"T1","span":"SurrealDB端口改成8100了"},"relations":[{"memoryId":"M1","relation":"updates"}]},
        {"text":"非法日期","scope":"project","entities":[],"effectiveAt":"2026-02-30","origin":"user",
            "source":{"turnId":"T1","span":"配置已同步"},"relations":[]}
    ]});
    let (facts, new_entities) = validate(
        &t,
        output,
        &[old.clone()],
        &turns,
        None,
        "p1",
        "codex",
        false,
    )
    .await
    .unwrap();
    assert_eq!(facts.len(), 2);
    assert_eq!(facts[0].sources.len(), 2);
    assert!(new_entities.is_empty());
    let applied = relations::apply(
        &eng,
        &t,
        facts,
        WriteCtx {
            platform: "codex".into(),
            now: now.clone(),
            turns: turns
                .clone()
                .into_iter()
                .map(|tr| (store::turn_rid(&tr.platform, &tr.turn_id), tr))
                .collect(),
            text_request: None,
            text_turn_id: None,
            new_entities,
        },
    )
    .await
    .unwrap();
    assert_eq!(applied.created.len(), 2);
    old = t.store.get_memory("moldport").await.unwrap().unwrap();
    assert_eq!(old.status, S_SUPERSEDED);
    assert_eq!(old.valid_until.as_deref(), Some("2026-10-07"));
    let new = t
        .store
        .get_memory(old.superseded_by.as_deref().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(new.source.origin, ORIGIN_USER);
    assert_eq!(new.evidence_count, 2);
    assert_eq!(new.entity_ids, vec!["entsurreal"]);
    let tr = t.store.get_turn("codex", "T1").await.unwrap().unwrap();
    assert_eq!(tr.extract_state, "done");
    assert_eq!(tr.derived_memory_ids.len(), 2);
    assert!(tr.extracted_at.is_some());
    let task = ingest::get_task(
        &eng,
        &t,
        GetTaskReq {
            request_id: id.into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(task["status"], "completed");
    indexer::drain(&eng, &t).await.unwrap();
    let recalled = recall(&eng, &t, "SurrealDB port", "p1", "codex", None, 10)
        .await
        .unwrap();
    assert!(!recalled
        .iter()
        .any(|m| m.id == "mhidden" || m.id == "moldport"));
    // The evidence copies and idempotency receipt remain available after the source turn is purged.
    let evidence_count = t.store.count("evidence", None).await.unwrap();
    assert_eq!(evidence_count, 3);
    let mut tx = Tx::new();
    tx.delete("turns", &store::turn_rid("codex", "T1")).unwrap();
    t.store.commit(tx).await.unwrap();
    let after_purge = ingest::add_memories(&eng, &t, &c, req).await.unwrap();
    assert_eq!(after_purge["requestId"], id);
    assert_eq!(after_purge["duplicate"], true);
    assert_eq!(
        t.store.count("evidence", None).await.unwrap(),
        evidence_count
    );
    // A synchronous model failure must persist a receipt and retry payload before returning 503.
    let error = ingest::add_memories(
        &eng,
        &t,
        &c,
        AddMemoriesReq {
            scope,
            text: Some("记住测试用端口8100".into()),
            ..Default::default()
        },
    )
    .await;
    assert!(matches!(error, Err(ApiError::ModelUnavailable(_))));
    let queued = t
        .store
        .select("SELECT * OMIT id FROM requests WHERE kind = 'text'", vec![])
        .await
        .unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0]["status"], "queued");
    assert_eq!(queued[0]["failures"], 1);
    assert_eq!(eng.llm.usage_snapshot()["calls"], 0);
    println!("formation_selftest OK: provenance/date/reference checks; exact dedupe; atomic evidence/turn/receipt/update-chain; scope; post-purge idempotency; durable failure; zero LLM calls; data {}",dir.display());
}
