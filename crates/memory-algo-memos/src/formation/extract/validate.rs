//! Deterministic validation of model facts: schema, provenance, references, entities and dedupe.
use super::super::relations::{
    plan::{Rel, Target},
    FactSource, NewFact,
};
use anyhow::{anyhow, Result};
use memory_domain::types::*;
use memory_engine::retrieval::visibility::visible;
use memory_engine::runtime::tenant::Tenant;
use memory_rules::util;
use memory_store::schema;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const MAX_SOURCE_SPAN_CHARS: usize = 1024;

/// Internal fact representation after the flat provenance fields have been assembled.
#[derive(Debug, Deserialize)]
pub(super) struct RawFact {
    pub(super) text: String,
    pub(super) scope: String,
    pub(super) entities: Vec<String>,
    #[serde(rename = "effectiveAt")]
    pub(super) effective_at: String,
    pub(super) origin: String,
    pub(super) source: RawSource,
    #[serde(deserialize_with = "parse_raw_relations")]
    pub(super) relations: Vec<RawRelation>,
    #[serde(default)]
    pub(super) reason: Option<String>,
}
#[derive(Debug, Deserialize)]
pub(super) struct RawSource {
    #[serde(rename = "turnId")]
    pub(super) turn_id: String,
    pub(super) span: String,
}
#[derive(Debug, Deserialize)]
pub(in crate::formation) struct RawRelation {
    // Preserve existing internal aliases; model values are assembled into canonical keys above.
    // Ambiguous objects containing both aliases remain invalid.
    #[serde(rename = "memoryId", alias = "target")]
    pub(in crate::formation) memory_id: String,
    #[serde(alias = "type")]
    pub(in crate::formation) relation: String,
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

pub(super) fn collapsed(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whitespace normalisation is the sole permitted change when matching a supporting span.
pub(super) fn supporting_source(
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

pub(in crate::formation) async fn validate(
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
                rejected.push(
                    json!({"fact":i+1,"kind":"fact_discarded","reason":"schema_invalid",
                    "detail":util::truncate_bytes(&error.to_string(), 300)}),
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
        let tr = [TurnRec {
            turn_id: "T1".into(),
            user_message: "port is 8100".into(),
            assistant_message: "port is 8200".into(),
            user_ts: "2026-10-08T00:00:00Z".into(),
            assistant_ts: "2026-10-08T00:00:01Z".into(),
            ..Default::default()
        }];
        assert!(supporting_source(&raw("user", "8100"), &tr, None, false).is_some());
        assert!(supporting_source(&raw("user", "8200"), &tr, None, false).is_none());
        assert!(supporting_source(&raw("assistant", "is   8200"), &tr, None, false).is_some());
        assert!(supporting_source(&raw("assistant", "is 8201"), &tr, None, false).is_none());
    }
    #[test]
    fn references_name_existing_memories_or_other_live_facts() {
        let live = HashSet::from([1, 3]);
        let old = vec![MemoryRec::default()];
        assert!(valid_target("M1", 1, &live, &old));
        assert!(!valid_target("M2", 1, &live, &old));
        assert!(valid_target("F3", 1, &live, &old));
        assert!(!valid_target("F1", 1, &live, &old));
    }
}
