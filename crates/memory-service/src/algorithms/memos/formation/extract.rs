//! P1/P1-b: visible recall, documented prompt layout, deterministic validation and atomic formation.
use super::{prompts, relations};
use crate::algorithms::memos::repo;
use crate::domain::types::*;
use crate::runtime::{engine::Engine, tenant::Tenant};
use crate::storage::schema;
use anyhow::{anyhow, Result};
use memory_fulltext::FtFilter;
use memory_rules::util;
use relations::{
    plan::{Rel, Target},
    Applied, FactSource, NewFact, WriteCtx,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub use crate::retrieval::visibility::visible;

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

/// Persist stage output/final failure; the LLM client durably accounts for every billed reply.
pub async fn call_logged(
    eng: &Arc<Engine>,
    t: &Arc<Tenant>,
    stage: &str,
    system: &str,
    user: &str,
    max_tokens: u32,
) -> Result<Value> {
    call_logged_contract(eng, t, stage, system, user, max_tokens, None).await
}

async fn call_logged_contract(
    eng: &Arc<Engine>,
    t: &Arc<Tenant>,
    stage: &str,
    system: &str,
    user: &str,
    max_tokens: u32,
    contract: Option<&crate::runtime::llm::JsonContract>,
) -> Result<Value> {
    let values_only = contract.is_none() && matches!(stage, "P1" | "P1-b");
    let reply = if let Some(contract) = contract {
        eng.llm
            .contract_call_logged(system, user, max_tokens, &t.store, stage, contract)
            .await
    } else if values_only {
        eng.llm
            .values_call_logged(system, user, max_tokens, &t.store, stage)
            .await
    } else {
        eng.llm
            .json_call_logged(system, user, max_tokens, &t.store, stage)
            .await
    };
    let now = util::now_str();
    match reply {
        Ok(r) => {
            let output = if values_only {
                facts_from_values(&r.json)
            } else if contract
                .map(|c| c.name == "facts_object_v2")
                .unwrap_or(false)
            {
                facts_from_flat_object(&r.json)
            } else {
                Ok(r.json.clone())
            };
            let output = match output {
                Ok(output) => output,
                Err(error) => {
                    t.store
                        .log(
                            schema::EXTRACT_LOG,
                            json!({"stage":stage,"createdAt":now,
                        "status":"failed","error":error.to_string(),"modelOutput":r.json}),
                        )
                        .await?;
                    return Err(error);
                }
            };
            let mut record = json!({"stage":stage,"createdAt":now,"variant":prompts::for_tenant(eng, t).variant,
                "systemHash":blake3::hash(system.as_bytes()).to_hex().to_string(),"inputHash":blake3::hash(user.as_bytes()).to_hex().to_string(),
                "outputHash":blake3::hash(output.to_string().as_bytes()).to_hex().to_string(),"output":output,"status":"model_completed",
                "responseLogId":r.response_log_id});
            if values_only || contract.is_some() {
                record["outputFormat"] = json!(if stage == "P2" {
                    super::schema::EXPERIENCE_FORMAT
                } else if contract.is_some() {
                    super::schema::FACT_FORMAT
                } else {
                    "fact-values-v1"
                });
                record["modelOutput"] = r.json;
            }
            t.store.log(schema::EXTRACT_LOG, record).await?;
            Ok(output)
        }
        Err(e) => {
            t.store
                .log(
                    schema::EXTRACT_LOG,
                    json!({"stage":stage,"createdAt":now,"status":"failed","error":e.to_string()}),
                )
                .await?;
            Err(e)
        }
    }
}

const MAX_SOURCE_SPAN_CHARS: usize = 1024;

/// One explicit external contract, no legacy-shape fallback. The model supplies values for flat
/// provenance fields; code owns the internal nested source keys. Raw model replies stay logged.
pub(super) fn facts_from_flat_object(output: &Value) -> Result<Value> {
    let items = output["facts"]
        .as_array()
        .ok_or_else(|| anyhow!("facts-object-v2 requires facts array"))?;
    let facts = items
        .iter()
        .map(|item| {
            let mut fields = item
                .as_object()
                .ok_or_else(|| anyhow!("fact must be an object"))?
                .clone();
            let turn = fields
                .remove("sourceTurnId")
                .ok_or_else(|| anyhow!("sourceTurnId missing"))?;
            let span = fields
                .remove("sourceSpan")
                .ok_or_else(|| anyhow!("sourceSpan missing"))?;
            fields.insert("source".into(), json!({"turnId":turn,"span":span}));
            Ok(Value::Object(fields))
        })
        .collect::<Result<Vec<Value>>>()?;
    Ok(json!({"facts":facts}))
}

/// The tuple fixes both arity and types. Missing optional values must retain their slots.
type FactValues = (
    String,
    String,
    Vec<String>,
    String,
    String,
    String,
    String,
    Vec<(String, String)>,
    Option<String>,
);

/// Construct the internal object schema ourselves; preserve rejected row positions for F references.
pub(super) fn facts_from_values(output: &Value) -> Result<Value> {
    let rows = output
        .as_array()
        .ok_or_else(|| anyhow!("fact-values-v1 requires a top-level array"))?;
    let mut errors = serde_json::Map::new();
    let facts: Vec<Value> = rows.iter().enumerate().map(|(i, row)| {
        match serde_json::from_value::<FactValues>(row.clone()) {
            Ok((text, scope, entities, effective_at, origin, turn_id, span, relations, reason)) => {
                let relations: Vec<Value> = relations.into_iter()
                    .map(|(memory_id, relation)| json!({"memoryId":memory_id,"relation":relation})).collect();
                json!({"text":text,"scope":scope,"entities":entities,"effectiveAt":effective_at,
                    "origin":origin,"source":{"turnId":turn_id,"span":span},
                    "relations":relations,"reason":reason})
            }
            Err(error) => {
                errors.insert((i+1).to_string(), json!(util::truncate_bytes(&error.to_string(),300)));
                Value::Null
            }
        }
    }).collect();
    Ok(json!({"facts":facts,"valueErrors":errors}))
}

/// Model values have four fixed sections; the program owns every object key in both extractors.
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
pub(super) struct RawRelation {
    // Preserve existing internal aliases; model values are assembled into canonical keys above.
    // Ambiguous objects containing both aliases remain invalid.
    #[serde(rename = "memoryId", alias = "target")]
    pub(super) memory_id: String,
    #[serde(alias = "type")]
    pub(super) relation: String,
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

pub(super) async fn validate(
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
            Err(error) => {
                let detail = output["valueErrors"][(i + 1).to_string()]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| error.to_string());
                rejected.push(
                    json!({"fact":i+1,"kind":"fact_discarded","reason":"schema_invalid",
                    "detail":util::truncate_bytes(&detail, 300)}),
                );
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
        } else if r.source.span.chars().count() > MAX_SOURCE_SPAN_CHARS {
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
    let ents: Vec<EntityRec> = t.store.entities().await?;
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
    t.store.log(schema::EXTRACT_LOG,json!({"stage":if text.is_some(){"P1-b_validation"}else{"P1_validation"},
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

/// Frequent batches form only facts. Session-level P2 is a separate job after session end.
pub async fn batch(eng: &Arc<Engine>, t: &Arc<Tenant>, turns: Vec<TurnRec>) -> Result<Applied> {
    let turns: Vec<TurnRec> = turns
        .iter()
        .map(|tr| tr.clipped(eng.cfg.max_message_bytes))
        .collect();
    let first = turns
        .first()
        .ok_or_else(|| anyhow!("empty extraction batch"))?;
    let p = prompts::for_tenant(eng, t);
    let query = turns
        .iter()
        .map(TurnRec::full_text)
        .collect::<Vec<_>>()
        .join("\n");
    let old = recall(
        eng,
        t,
        &query,
        &first.project,
        &first.platform,
        Some(T_FACT),
        eng.cfg.recall_for_extract,
    )
    .await?;
    let all = t
        .store
        .session_turns(&first.platform, &first.session)
        .await?;
    let previous = all
        .iter()
        .filter(|tr| tr.seq < first.seq)
        .rev()
        .take(eng.cfg.prev_digest_turns)
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
                if previous.is_empty() {
                    p.l.previous_none
                } else {
                    &previous
                }
            )
        ),
        prompts::block(p.l.batch, &conversation(&turns, p))
    );
    let ids = turns
        .iter()
        .map(|tr| tr.turn_id.clone())
        .collect::<Vec<_>>();
    let contract = crate::runtime::llm::JsonContract {
        echo_invalid_response: true,
        name: "facts_object_v2",
        schema: fact_source_contract(&ids, &turns, p.both_speakers_first_hand),
        validate: validate_fact_source_contract,
    };
    let output = call_logged_contract(
        eng,
        t,
        "P1",
        &prompts::facts(p, false),
        &user,
        8192,
        Some(&contract),
    )
    .await?;
    commit_facts(eng, t, turns, old, output).await
}

/// Provenance is part of the local model contract, so a bad quote receives the same bounded
/// repair exchange as malformed JSON. Never approximate a quote or repair evidence ourselves.
pub(super) fn fact_source_contract(ids: &[String], turns: &[TurnRec], first_hand: bool) -> Value {
    let mut schema = super::schema::facts_for_turns(ids);
    schema["x-source-turns"] = json!(turns);
    schema["x-both-speakers-first-hand"] = json!(first_hand);
    schema
}

pub(super) fn validate_fact_source_contract(schema: &Value, output: &Value) -> Result<()> {
    super::schema::validate_schema(schema, output)?;
    let output = facts_from_flat_object(output)?;
    let turns: Vec<TurnRec> = serde_json::from_value(schema["x-source-turns"].clone())?;
    let first_hand = schema["x-both-speakers-first-hand"]
        .as_bool()
        .unwrap_or(false);
    for (index, value) in output["facts"].as_array().unwrap().iter().enumerate() {
        let fact: RawFact = serde_json::from_value(value.clone())?;
        if !util::is_valid_date(&fact.effective_at) {
            return Err(anyhow!(
                "$.facts[{index}].effectiveAt: invalid calendar date"
            ));
        }
        if fact.text.trim().is_empty() || collapsed(&fact.source.span).is_empty() {
            return Err(anyhow!(
                "$.facts[{index}]: text and source.span must be nonempty"
            ));
        }
        let text = schema["x-source-text"].as_str().map(|text| (text, ""));
        if supporting_source(&fact, &turns, text, first_hand).is_none() {
            return Err(anyhow!("$.facts[{index}].source.span: span_not_verbatim or origin_role_mismatch in turn {}; copy a contiguous excerpt from the permitted source message verbatim", fact.source.turn_id));
        }
    }
    Ok(())
}

pub(super) async fn commit_facts(
    eng: &Arc<Engine>,
    t: &Arc<Tenant>,
    turns: Vec<TurnRec>,
    old: Vec<MemoryRec>,
    output: Value,
) -> Result<Applied> {
    let first = turns.first().ok_or_else(|| anyhow!("empty fact commit"))?;
    let p = prompts::for_tenant(eng, t);
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
    relations::apply(
        eng,
        t,
        facts,
        WriteCtx {
            platform: first.platform.clone(),
            now: util::now_str(),
            turns: turns
                .into_iter()
                .map(|tr| {
                    (
                        crate::storage::store::turn_rid(&tr.platform, &tr.turn_id),
                        tr,
                    )
                })
                .collect(),
            text_request: None,
            text_turn_id: None,
            new_entities,
        },
    )
    .await
}

/// Read the retained session transcript once it ends; never re-extract its facts.
pub async fn session_experience(
    eng: &Arc<Engine>,
    t: &Arc<Tenant>,
    session: &SessionRec,
) -> Result<()> {
    if session.ended_at.is_none() {
        return Err(anyhow!("experience requires an ended session"));
    }
    let watermark = session.turn_count;
    if session.last_experience_seq >= watermark {
        return Ok(());
    }
    let turns: Vec<TurnRec> = t
        .store
        .session_turns(&session.platform, &session.session)
        .await?
        .into_iter()
        .filter(|tr| tr.seq <= watermark)
        .map(|tr| tr.clipped(eng.cfg.max_message_bytes))
        .collect();
    if turns.is_empty() {
        return Err(anyhow!("session experience source turns missing"));
    }
    let new_ids: HashSet<String> = turns
        .iter()
        .filter(|tr| tr.seq > session.last_experience_seq)
        .map(|tr| tr.turn_id.clone())
        .collect();
    let p = prompts::for_tenant(eng, t);
    let query = turns
        .iter()
        .map(TurnRec::full_text)
        .collect::<Vec<_>>()
        .join("\n");
    let old = recall(
        eng,
        t,
        &query,
        &session.project,
        &session.platform,
        Some(T_EXPERIENCE),
        eng.cfg.recall_for_experience,
    )
    .await?;
    let rows = old
        .iter()
        .enumerate()
        .map(|(i, m)| {
            format!(
                "E{} | {} | {}",
                i + 1,
                m.scope.kind,
                m.detail.as_ref().unwrap_or(&Value::Null)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut ordered_new = new_ids.iter().cloned().collect::<Vec<_>>();
    ordered_new.sort();
    let new_tag = if p.variant == "en" {
        "new turns"
    } else {
        "新增轮"
    };
    let user = format!(
        "{}\n\n{}\n\n{}\n\n{}",
        prompts::block(
            p.l.old_experiences,
            if rows.is_empty() { p.l.none } else { &rows }
        ),
        prompts::block(p.l.summary, &serde_json::to_string(session)?),
        prompts::block(new_tag, &ordered_new.join(", ")),
        prompts::block(p.l.conversation, &conversation(&turns, p))
    );
    let ids = turns
        .iter()
        .map(|tr| tr.turn_id.clone())
        .collect::<Vec<_>>();
    let contract = crate::runtime::llm::JsonContract {
        echo_invalid_response: true,
        name: "session_experience_object_v1",
        schema: super::schema::experiences_for_turns(&ids),
        validate: super::schema::validate_schema,
    };
    let output = call_logged_contract(
        eng,
        t,
        "P2",
        &prompts::session_experience(p),
        &user,
        8192,
        Some(&contract),
    )
    .await?;
    commit_experience(eng, t, session, turns, new_ids, old, output, watermark).await
}

pub(super) async fn commit_experience(
    eng: &Arc<Engine>,
    t: &Arc<Tenant>,
    snapshot: &SessionRec,
    turns: Vec<TurnRec>,
    new_ids: HashSet<String>,
    old: Vec<MemoryRec>,
    output: Value,
    watermark: i64,
) -> Result<()> {
    // New arrivals may reopen the session during generation; preserve their sequence and end state.
    let _ingest = t.lock_ingest().await;
    let session = t
        .store
        .get_session(&snapshot.platform, &snapshot.session)
        .await?
        .ok_or_else(|| anyhow!("experience session missing"))?;
    let prepared = super::experience::prepare(
        &session,
        &turns,
        &new_ids,
        &old,
        output,
        watermark,
        prompts::for_tenant(eng, t).variant,
        &util::now_str(),
    )?;
    prepared.commit(t, &snapshot.platform).await
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
    let p = prompts::for_tenant(eng, t);
    let old = recall(
        eng,
        t,
        text,
        project,
        platform,
        Some(T_FACT),
        eng.cfg.recall_for_extract,
    )
    .await?;
    let user = format!(
        "{}\n\n<{} time=\"{}\">\n{}\n</{}>",
        headers(t, &old, text, project, platform, p).await?,
        p.l.remember,
        now,
        text,
        p.l.remember
    );
    let mut schema = fact_source_contract(&["text".into()], &[], false);
    schema["x-source-text"] = json!(text);
    let contract = crate::runtime::llm::JsonContract {
        echo_invalid_response: true,
        name: "facts_object_v2",
        schema,
        validate: validate_fact_source_contract,
    };
    let output = call_logged_contract(
        eng,
        t,
        "P1-b",
        &prompts::facts(p, true),
        &user,
        4096,
        Some(&contract),
    )
    .await?;
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
