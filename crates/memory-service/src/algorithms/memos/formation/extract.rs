//! P1/P1-b: visible recall, documented prompt layout, deterministic validation and atomic formation.
use super::{prompts, relations};
use crate::{engine::Engine, tenant::Tenant, types::*, util};
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

pub use crate::retrieve::visibility::visible;

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
            .filter_map(|m| crate::indexer::vector_text(m).map(|text| (m.id.clone(), text)))
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
    contract: Option<&crate::llm::JsonContract>,
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
                            "extract_log",
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
            t.store.log("extract_log", record).await?;
            Ok(output)
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

const MAX_SOURCE_SPAN_CHARS: usize = 1024;

/// One explicit external contract, no legacy-shape fallback. The model supplies values for flat
/// provenance fields; code owns the internal nested source keys. Raw model replies stay logged.
fn facts_from_flat_object(output: &Value) -> Result<Value> {
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
fn facts_from_values(output: &Value) -> Result<Value> {
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
struct RawRelation {
    // Preserve existing internal aliases; model values are assembled into canonical keys above.
    // Ambiguous objects containing both aliases remain invalid.
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
    let contract = crate::llm::JsonContract {
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
fn fact_source_contract(ids: &[String], turns: &[TurnRec], first_hand: bool) -> Value {
    let mut schema = super::schema::facts_for_turns(ids);
    schema["x-source-turns"] = json!(turns);
    schema["x-both-speakers-first-hand"] = json!(first_hand);
    schema
}

fn validate_fact_source_contract(schema: &Value, output: &Value) -> Result<()> {
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

async fn commit_facts(
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
                .map(|tr| (crate::store::turn_rid(&tr.platform, &tr.turn_id), tr))
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
    let contract = crate::llm::JsonContract {
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

async fn commit_experience(
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
    let _ingest = t.ingest_lock.lock().await;
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
    let contract = crate::llm::JsonContract {
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

/// Deterministic formation/storage integration test. Uses supplied JSON output and mock embeddings;
/// the LLM client has no key and this test makes zero generative-model requests.
pub async fn formation_selftest() {
    let source_turn = TurnRec {
        turn_id: "T-proof".into(),
        user_message: "I chose the inclusive organisation.".into(),
        assistant_message: "My daily walk helps me recharge.".into(),
        user_ts: "2026-10-08T00:00:00Z".into(),
        assistant_ts: "2026-10-08T00:00:01Z".into(),
        ..Default::default()
    };
    let schema = fact_source_contract(&["T-proof".into()], &[source_turn.clone()], true);
    let mut question_turn = source_turn.clone();
    question_turn.user_message = "Which season is the planned activity for?".into();
    let context = relations::source_context(&question_turn, "assistant", true).unwrap();
    assert_eq!(context.span, question_turn.user_message);
    assert_eq!(context.stated_at, question_turn.user_ts);
    assert!(relations::source_context(&question_turn, "user", true).is_none());
    assert!(relations::source_context(&question_turn, "assistant", false).is_none());
    question_turn.user_message = format!("{}?", "x".repeat(1024));
    assert!(relations::source_context(&question_turn, "assistant", true).is_none());
    let mut output = json!({"facts":[{"text":"The daily walk helps recharge.","scope":"global",
        "entities":[],"effectiveAt":"2026-10-08","origin":"user",
        "sourceTurnId":"T-proof","sourceSpan":"My daily walk helps me recharge.","relations":[]}]});
    validate_fact_source_contract(&schema, &output).unwrap();
    let mut typed_entities = output.clone();
    typed_entities["facts"][0]["entities"] = json!([{"type":"person","value":"Alex"}]);
    assert!(validate_fact_source_contract(&schema, &typed_entities)
        .unwrap_err()
        .to_string()
        .contains("entities"));
    assert!(facts_from_flat_object(&json!({"facts":[{"source":{"turnId":"T-proof","span":"My daily walk helps me recharge."}}]})).is_err());
    output["facts"][0]["sourceSpan"] = json!("The daily walk helps me recharge.");
    assert!(validate_fact_source_contract(&schema, &output)
        .unwrap_err()
        .to_string()
        .contains("span_not_verbatim"));
    output["facts"][0]["sourceSpan"] = json!("My daily walk helps me recharge.");
    let strict = fact_source_contract(&["T-proof".into()], &[source_turn], false);
    assert!(validate_fact_source_contract(&strict, &output).is_err());
    output["facts"][0]["effectiveAt"] = json!("2026-02-30");
    assert!(validate_fact_source_contract(&schema, &output)
        .unwrap_err()
        .to_string()
        .contains("calendar date"));
    // Value-only output cannot rename source/relation keys; those keys are constructed here.
    let values = json!([
        ["A fact","global",["Caroline"],"2023-05-08","user","T1","quoted",[["M1","related"]],null],
        ["A shifted row","global",[],"2023-05-08","user","T1",[],null],
        ["Wrong entities","global",[{"name":"Caroline"}],"2023-05-08","user","T1","quoted",[],null],
        ["Wrong relation","global",[],"2023-05-08","user","T1","quoted",[{"memoryId":"M1","relation":"related"}],null],
        ["Null excerpt","global",[],"2023-05-08","user","T1",null,[],null],
        ["Last fact","global",[],"2023-05-08","user","T1","quoted",[["F1","related"]],"detail"]
    ]);
    let assembled = facts_from_values(&values).unwrap();
    assert_eq!(assembled["facts"].as_array().unwrap().len(), 6);
    assert_eq!(
        assembled["facts"][0]["source"],
        json!({"turnId":"T1","span":"quoted"})
    );
    assert_eq!(
        assembled["facts"][0]["relations"],
        json!([{"memoryId":"M1","relation":"related"}])
    );
    for i in 1..5 {
        assert!(assembled["facts"][i].is_null());
        assert!(assembled["valueErrors"][(i + 1).to_string()].is_string());
    }
    assert_eq!(assembled["facts"][5]["relations"][0]["memoryId"], "F1");
    assert_eq!(facts_from_values(&json!([])).unwrap()["facts"], json!([]));
    assert!(facts_from_values(&json!({"facts":[]})).is_err());
    assert!(facts_from_values(&json!(null)).is_err());
    let legacy: RawRelation =
        serde_json::from_value(json!({"target":"M1","type":"updates"})).unwrap();
    assert_eq!(legacy.memory_id, "M1");
    assert_eq!(legacy.relation, "updates");
    assert!(serde_json::from_value::<RawRelation>(
        json!({"target":"M1","memoryId":"M2","type":"updates"})
    )
    .is_err());
    assert!(serde_json::from_value::<RawRelation>(json!({"target":12,"type":"updates"})).is_err());
    let canonical: RawRelation =
        serde_json::from_value(json!({"memoryId":"F2","relation":"related"})).unwrap();
    assert_eq!(canonical.memory_id, "F2");
    assert!(
        serde_json::from_value::<RawRelation>(json!({"F":"F2","M":null,"type":"related"})).is_err()
    );
    use crate::{
        config::ServiceConfig,
        indexer, ingest,
        llm::Llm,
        models::ModelHub,
        store::{self, to_doc, Tx},
    };
    assert_eq!(std::env::var("MEMORY_MOCK_MODELS").as_deref(), Ok("1"));
    super::experience::rule_selftest();
    ingest::trigger_selftest();
    let dir = std::env::temp_dir().join(format!("memory-formation-selftest-{}", util::new_id("t")));
    let eng = Engine::new(
        ServiceConfig {
            memory_strategy: crate::config::MemoryStrategy::Memos,
            data_dir: dir.clone(),
            ..Default::default()
        },
        Llm::new("http://127.0.0.1:1", "", "disabled", 1, 1).unwrap(),
        ModelHub::mock(),
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
    // Verify the character boundary through the complete provenance/schema validator, for
    // both conversation extraction and explicit writes. Unicode chars are not UTF-8 bytes.
    for (length, accepted_count) in [(81, 1), (1024, 1), (1025, 0)] {
        let span = "证".repeat(length);
        let long_turn = TurnRec {
            turn_id: "T-long".into(),
            user_message: span.clone(),
            user_ts: "2026-10-08T10:00:00+08:00".into(),
            ..Default::default()
        };
        for explicit in [false, true] {
            let output = facts_from_values(&json!([[
                "A grounded long excerpt",
                "global",
                [],
                "2026-10-08",
                if explicit { "explicit" } else { "user" },
                if explicit { "text" } else { "T-long" },
                span,
                [],
                null
            ]]))
            .unwrap();
            let (accepted, _) = validate(
                &t,
                output,
                &[],
                &[long_turn.clone()],
                explicit.then_some((span.as_str(), "2026-10-08T10:00:00+08:00")),
                "p1",
                "codex",
                false,
            )
            .await
            .unwrap();
            assert_eq!(accepted.len(), accepted_count);
            if let Some(fact) = accepted.first() {
                assert_eq!(fact.sources[0].span.chars().count(), length);
            }
        }
    }
    let broken_reference = facts_from_values(&json!([
        ["missing slots"],
        [
            "SurrealDB端口改成8100了",
            "project",
            [],
            "2026-10-08",
            "user",
            "T1",
            "端口改成8100了",
            [["F1", "updates"]],
            "replaces"
        ],
        [
            "配置已同步",
            "project",
            [],
            "2026-10-08",
            "user",
            "T1",
            "配置已同步",
            [["F1", "related"]],
            null
        ]
    ]))
    .unwrap();
    let (accepted, _) = validate(
        &t,
        broken_reference,
        &[],
        &turns,
        None,
        "p1",
        "codex",
        false,
    )
    .await
    .unwrap();
    assert_eq!(accepted.len(), 1);
    assert_eq!(accepted[0].fidx, 3);
    assert!(accepted[0].relations.is_empty());
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
    session_split_selftest(&eng, &dir).await;
    println!("formation_selftest OK: independent facts and session-end experiences; no P2 on open-session flush; fact receipts survive P2 failure; resumed-session watermark; source retention; provenance/date/reference checks; update-chain; durable failure; zero LLM calls; data {}",dir.display());
}

/// Verify shared commits, retained cross-batch provenance, retry safety and legacy P1 backfill.
async fn session_split_selftest(eng: &Arc<Engine>, dir: &std::path::PathBuf) {
    use super::consolidate;
    use crate::{indexer, ingest};
    let t = Arc::new(
        Tenant::open("split-selftest-en", dir, eng.models.dim)
            .await
            .unwrap(),
    );
    let c = Caller {
        tenant: t.name.clone(),
        platform: "test".into(),
    };
    let scope = ReqScope {
        project: Some("split".into()),
        session: Some("S".into()),
        ..Default::default()
    };
    let base = util::now() - chrono::Duration::days(10);
    let mut payloads = Vec::new();
    for i in 1..=4 {
        let payload = AddMemoriesReq {
            scope: scope.clone(),
            turn_id: Some(format!("U{i}")),
            messages: Some(vec![
                InMessage {
                    role: "user".into(),
                    content: format!("Deployment step {i}"),
                    timestamp: util::fmt_ts(&(base + chrono::Duration::seconds(i * 2))),
                },
                InMessage {
                    role: "assistant".into(),
                    content: if i == 4 {
                        "Tests passed."
                    } else {
                        "Not verified yet."
                    }
                    .into(),
                    timestamp: util::fmt_ts(&(base + chrono::Duration::seconds(i * 2 + 1))),
                },
            ]),
            ..Default::default()
        };
        ingest::add_memories(eng, &t, &c, payload.clone())
            .await
            .unwrap();
        payloads.push(payload);
    }
    let all = t.store.session_turns("test", "S").await.unwrap();
    let first = all[..3].to_vec();
    let output = facts_from_values(&json!([[
        "Deployment step 1",
        "project",
        ["service"],
        util::date_of(&first[0].user_ts),
        "user",
        "U1",
        "Deployment step 1",
        [],
        null
    ]]))
    .unwrap();
    let applied = commit_facts(eng, &t, first, vec![], output).await.unwrap();
    assert_eq!(applied.created.len(), 1);
    let s = t.store.get_session("test", "S").await.unwrap().unwrap();
    assert_eq!(s.last_experience_seq, 0);
    assert_eq!(s.turn_count, 4);
    assert_eq!(
        t.store
            .count("memories", Some("type = 'experience'"))
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        consolidate::purge(&t, &util::now_str(), 3.0).await.unwrap()["purgedTurns"],
        0
    );
    let fourth = t.store.get_turn("test", "U4").await.unwrap().unwrap();
    commit_facts(eng, &t, vec![fourth], vec![], json!({"facts":[]}))
        .await
        .unwrap();
    // Force only facts while the session is open; no P2 or external request is made.
    assert_eq!(ingest::wait_idle(eng, &t).await.unwrap()["status"], "idle");
    assert_eq!(
        t.store
            .get_session("test", "S")
            .await
            .unwrap()
            .unwrap()
            .last_experience_seq,
        0
    );
    assert!(session_experience(eng, &t, &s).await.is_err());
    let end = AddMemoriesReq {
        scope: scope.clone(),
        session_ended: Some(true),
        ..Default::default()
    };
    let receipt = ingest::add_memories(eng, &t, &c, end.clone())
        .await
        .unwrap();
    let snapshot = t.store.get_session("test", "S").await.unwrap().unwrap();
    let turns = t.store.session_turns("test", "S").await.unwrap();
    let ids = turns
        .iter()
        .map(|tr| tr.turn_id.clone())
        .collect::<HashSet<_>>();
    let output = json!({"analysis":[{"task":"deployment","result":"success","turnIds":["U1","U2","U3","U4"]}],
        "experiences":[{"situation":"deployment validation","approach":"apply fix and test","result":"success: Tests passed.","lesson":"verify after changing","scope":"project","turnIds":["U1","U2","U3","U4"],"relations":[]}],
        "summary":{"goal":"deploy","outcome":"verified","decisions":["test changes"],"openIssues":[]}});
    let mut broken = output.clone();
    broken["summary"] = json!(false);
    assert!(commit_experience(
        eng,
        &t,
        &snapshot,
        turns.clone(),
        ids.clone(),
        vec![],
        broken,
        4
    )
    .await
    .is_err());
    assert_eq!(t.store.count("memories", None).await.unwrap(), 1);
    assert_eq!(
        t.store
            .get_turn("test", "U4")
            .await
            .unwrap()
            .unwrap()
            .extract_state,
        "done"
    );
    commit_experience(eng, &t, &snapshot, turns, ids, vec![], output, 4)
        .await
        .unwrap();
    assert_eq!(ingest::wait_idle(eng, &t).await.unwrap()["status"], "idle");
    let end_status = ingest::get_task(
        eng,
        &t,
        GetTaskReq {
            request_id: receipt["requestId"].as_str().unwrap().into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(end_status["status"], "completed");
    // Completed end replays cannot create duplicate experiences.
    ingest::add_memories(eng, &t, &c, end.clone())
        .await
        .unwrap();
    assert_eq!(ingest::wait_idle(eng, &t).await.unwrap()["status"], "idle");
    assert_eq!(
        t.store
            .count("memories", Some("type = 'experience'"))
            .await
            .unwrap(),
        1
    );
    let fact_receipt = ingest::get_task(
        eng,
        &t,
        GetTaskReq {
            request_id: all[0].request_id.clone(),
        },
    )
    .await
    .unwrap();
    assert_eq!(fact_receipt["memoryIds"].as_array().unwrap().len(), 1);
    let exp = t
        .store
        .find_memories("type = 'experience'", vec![])
        .await
        .unwrap();
    assert_eq!(exp[0].source.turn_ids.len(), 4);
    let recalled_facts = recall(eng, &t, "deployment", "split", "test", Some(T_FACT), 10)
        .await
        .unwrap();
    assert!(!recalled_facts.is_empty());
    assert!(recalled_facts.iter().all(|m| m.mtype == T_FACT));

    // A resumed session can add facts immediately; experiences wait for its next close.
    let mut resumed = payloads[0].clone();
    resumed.turn_id = Some("U5".into());
    ingest::add_memories(eng, &t, &c, resumed.clone())
        .await
        .unwrap();
    let turn = t.store.get_turn("test", "U5").await.unwrap().unwrap();
    commit_facts(eng, &t, vec![turn.clone()], vec![], json!({"facts":[]}))
        .await
        .unwrap();
    assert_eq!(ingest::wait_idle(eng, &t).await.unwrap()["status"], "idle");
    let s = t.store.get_session("test", "S").await.unwrap().unwrap();
    assert!(s.ended_at.is_none());
    assert_eq!(s.last_experience_seq, 4);
    let receipt = ingest::add_memories(eng, &t, &c, end.clone())
        .await
        .unwrap();
    t.store
        .merge_doc(
            "sessions",
            &crate::store::session_rid("test", "S"),
            json!({"experienceFailures":4,"experienceRetryAtTs":0}),
        )
        .await
        .unwrap();
    let idle = ingest::wait_idle(eng, &t).await.unwrap();
    assert_eq!(idle["failedExperiences"], 1);
    assert_eq!(idle["failedTurns"], 0);
    assert_eq!(
        ingest::get_task(
            eng,
            &t,
            GetTaskReq {
                request_id: turn.request_id.clone()
            }
        )
        .await
        .unwrap()["status"],
        "completed"
    );
    assert_eq!(
        ingest::get_task(
            eng,
            &t,
            GetTaskReq {
                request_id: receipt["requestId"].as_str().unwrap().into()
            }
        )
        .await
        .unwrap()["status"],
        "failed"
    );
    // Failed P2 sources remain durable. A later reopened session can complete a new P2 job.
    let mut resumed = resumed;
    resumed.turn_id = Some("U6".into());
    ingest::add_memories(eng, &t, &c, resumed).await.unwrap();
    let turn = t.store.get_turn("test", "U6").await.unwrap().unwrap();
    commit_facts(eng, &t, vec![turn], vec![], json!({"facts":[]}))
        .await
        .unwrap();
    let receipt = ingest::add_memories(eng, &t, &c, end).await.unwrap();
    let snapshot = t.store.get_session("test", "S").await.unwrap().unwrap();
    let turns = t.store.session_turns("test", "S").await.unwrap();
    let ids = HashSet::from(["U5".into(), "U6".into()]);
    commit_experience(eng,&t,&snapshot,turns,ids,vec![],json!({"analysis":[],"experiences":[],"summary":{"goal":"deploy","outcome":"verified","decisions":[],"openIssues":[]}}),6).await.unwrap();
    assert_eq!(ingest::wait_idle(eng, &t).await.unwrap()["status"], "idle");
    assert_eq!(
        ingest::get_task(
            eng,
            &t,
            GetTaskReq {
                request_id: receipt["requestId"].as_str().unwrap().into()
            }
        )
        .await
        .unwrap()["status"],
        "completed"
    );
    indexer::drain(eng, &t).await.unwrap();
    let evidence = t.store.count("evidence", None).await.unwrap();
    assert_eq!(
        consolidate::purge(&t, &util::now_str(), 3.0).await.unwrap()["purgedTurns"],
        6
    );
    assert_eq!(t.store.count("evidence", None).await.unwrap(), evidence);
    let duplicate = ingest::add_memories(eng, &t, &c, payloads[0].clone())
        .await
        .unwrap();
    assert_eq!(duplicate["duplicate"], true);
    assert_eq!(t.store.count("memories", None).await.unwrap(), 2);
    assert_eq!(eng.llm.usage_snapshot()["calls"], 0);
}
