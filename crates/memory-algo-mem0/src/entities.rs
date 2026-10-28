//! Rebuildable Mem0 entity projection. Primary memories remain authoritative.
//! The regex fallback is a supported Node SDK configuration; compromise NLP is not used.
use anyhow::Result;
use memory_engine::runtime::{engine::Engine, instance::Instance};
use memory_rules::util;
use memory_store::{schema, store::Tx};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    sync::Arc,
};

pub use memory_rules::mem0_entities::{extract_entities, normalize, Entity};
// v3: TOPIC rows (lowercase word runs) are no longer projected; they were most of the rows, each embedded, and
// selective recall ranks with the reranker instead.
const VERSION: &str = "node331-regex-f64-v3-named";

fn scope_key(scope: &Value) -> String {
    scope.to_string()
}
fn visible(scope: &Value, project: &str, platform: &str) -> bool {
    match scope["kind"].as_str().unwrap_or("") {
        "global" => true,
        "platform" => platform == "*" || scope["platform"] == platform,
        "project" => project == "*" || scope["project"] == project,
        _ => false,
    }
}

/// Node stores vectors as Float32Array but accumulates cosine in JavaScript numbers
/// (f64). Gate entity matches before narrowing the final boost to the API's f32 score.
fn cosine(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() {
        return f64::NAN;
    }
    let (mut dot, mut norm_a, mut norm_b) = (0.0_f64, 0.0_f64, 0.0_f64);
    for (&a, &b) in a.iter().zip(b) {
        let (a, b) = (a as f64, b as f64);
        dot += a * b;
        norm_a += a * a;
        norm_b += b * b;
    }
    dot / (norm_a.sqrt() * norm_b.sqrt())
}

/// Missing ingestionMode is reconstructed only from existing Mem0 provenance or a
/// correction predecessor. Unknown legacy facts are excluded rather than treating raw
/// facts as inferred. Corrections of inferred facts inherit entity-index eligibility.
/// Any recorded mode but `inferred` (raw text, or a record another use case wrote) is not
/// formation inference.
async fn inferred(t: &Instance, doc: &Value) -> Result<bool> {
    let mut doc = doc.clone();
    let mut seen = HashSet::new();
    for _ in 0..128 {
        match doc["ingestionMode"].as_str() {
            Some("inferred") => return Ok(true),
            Some(_) => return Ok(false),
            None => {}
        }
        let mid = doc["mid"].as_str().unwrap_or("");
        if !seen.insert(mid.to_owned()) {
            return Ok(false);
        }
        if let Some(source) = t.store.get_doc(super::SOURCES, mid).await? {
            for request in source["inputRequestIds"].as_array().into_iter().flatten() {
                if let Some(id) = request.as_str() {
                    if let Some(input) = t.store.get_doc(schema::REQUESTS, id).await? {
                        if input["formed"] == true && input["infer"] == true {
                            return Ok(true);
                        }
                    }
                }
            }
        }
        let Some(previous) = doc["supersedes"].as_str() else {
            return Ok(false);
        };
        let Some(previous) = t.store.get_doc(schema::MEMORIES, previous).await? else {
            return Ok(false);
        };
        doc = previous;
    }
    Ok(false)
}

// Only these authoritative tables affect eligibility, batch order or entity text.
const DEPENDENCIES: &[&str] = &[
    schema::MEMORIES,
    super::SOURCES,
    schema::REQUESTS,
    super::ENTITIES,
    super::ENTITY_STATE,
    super::ENTITY_INPUTS,
];

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct Extracted {
    text: String,
    kind: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct EntityRow {
    mid: String,
    scope: Value,
    text: String,
    entity_type: String,
    normalized_text: String,
    embedding: Vec<f32>,
    linked_memory_ids: BTreeSet<String>,
    version: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
struct Fact {
    doc: Value,
    eligible: bool,
    request_ids: Vec<String>,
    seq: u64,
    entities: Vec<Extracted>,
}

#[derive(Clone)]
struct Snapshot {
    revision: u64,
    model: String,
    facts: BTreeMap<String, Fact>,
    rows: Vec<EntityRow>,
}

type ProjectionCache = tokio::sync::Mutex<Option<Snapshot>>;

async fn prepare_fact(
    t: &Instance,
    doc: Value,
    known: &HashSet<String>,
    saved: Option<&Value>,
) -> Result<Fact> {
    let mid = doc["mid"].as_str().unwrap_or("");
    let eligible =
        inferred(t, &doc).await? || (doc["ingestionMode"].is_null() && known.contains(mid));
    let source = t.store.get_doc(super::SOURCES, mid).await?;
    let request_ids: Vec<String> = source
        .as_ref()
        .and_then(|s| s["inputRequestIds"].as_array())
        .into_iter()
        .flatten()
        .filter_map(|id| id.as_str().map(str::to_owned))
        .collect();
    let mut seq = u64::MAX;
    if let Some(id) = request_ids.first() {
        if let Some(request) = t.store.get_doc(schema::REQUESTS, id).await? {
            seq = request["inputSeq"].as_u64().unwrap_or(u64::MAX);
        }
    }
    let entities = if !eligible {
        Vec::new()
    } else if let Some(saved) =
        saved.filter(|s| s["version"] == VERSION && s["text"] == doc["text"])
    {
        serde_json::from_value(saved["entities"].clone())
            .unwrap_or_else(|_| projected(doc["text"].as_str().unwrap_or("")))
    } else {
        projected(doc["text"].as_str().unwrap_or(""))
    };
    Ok(Fact {
        doc,
        eligible,
        request_ids,
        seq,
        entities,
    })
}

/// The entities of `text` the projection keeps: named ones (PROPER, QUOTED, IDENTIFIER), not TOPIC word runs.
fn projected(text: &str) -> Vec<Extracted> {
    extract_entities(text)
        .into_iter()
        .filter(|entity| entity.kind != "TOPIC")
        .map(|entity| Extracted {
            text: entity.text,
            kind: entity.kind.into(),
        })
        .collect()
}

fn extraction(fact: &Fact) -> Value {
    json!({"version":VERSION,"text":fact.doc["text"],"entities":fact.entities})
}

/// Replay the historical canonical algorithm over cached extraction intermediates.
/// Starting from surviving canonical rows is intentional: semantic aliases overwrite
/// canonical vectors, so an append-only implementation can change later IDs and scores.
/// This CPU replay preserves that behavior without re-reading/re-extracting old facts.
async fn project(
    eng: &Arc<Engine>,
    facts: &BTreeMap<String, Fact>,
    old: &[EntityRow],
    model_matches: bool,
) -> Result<Vec<EntityRow>> {
    let mut ordered: Vec<_> = facts.iter().filter(|(_, fact)| fact.eligible).collect();
    ordered.sort_by(|(mid_a, a), (mid_b, b)| {
        (
            a.doc["recordedAt"].as_str().unwrap_or(""),
            a.seq,
            a.doc["formationPosition"].as_u64().unwrap_or(u64::MAX),
            *mid_a,
        )
            .cmp(&(
                b.doc["recordedAt"].as_str().unwrap_or(""),
                b.seq,
                b.doc["formationPosition"].as_u64().unwrap_or(u64::MAX),
                *mid_b,
            ))
    });
    let mut desired: Vec<(
        (String, String),
        (Extracted, Value, BTreeSet<String>, String),
    )> = Vec::new();
    let mut positions = HashMap::new();
    for (mid, fact) in ordered {
        let scope = &fact.doc["scope"];
        let batch = if fact.seq == u64::MAX {
            format!("correction:{mid}")
        } else {
            format!("request:{}", fact.seq)
        };
        for entity in &fact.entities {
            let key = (scope_key(scope), normalize(&entity.text));
            let index = if let Some(index) = positions.get(&key) {
                *index
            } else {
                let index = desired.len();
                positions.insert(key.clone(), index);
                desired.push((
                    key,
                    (
                        entity.clone(),
                        scope.clone(),
                        BTreeSet::new(),
                        batch.clone(),
                    ),
                ));
                index
            };
            desired[index].1 .2.insert(mid.clone());
        }
    }
    let mut old_exact = HashMap::new();
    for row in old {
        old_exact
            .entry((scope_key(&row.scope), normalize(&row.text)))
            .or_insert(row);
    }
    let mut texts = Vec::new();
    let mut needed = Vec::new();
    let mut vectors: BTreeMap<(String, String), Vec<f32>> = BTreeMap::new();
    for (key, (entity, _, _, _)) in &desired {
        if let Some(row) = old_exact
            .get(key)
            .filter(|row| model_matches && !row.embedding.is_empty())
        {
            vectors.insert(key.clone(), row.embedding.clone());
        } else {
            needed.push(key.clone());
            texts.push(entity.text.clone());
        }
    }
    let embedded = eng.models.embed(&texts).await?;
    for (key, vector) in needed.into_iter().zip(embedded) {
        vectors.insert(key, (*vector).clone());
    }
    let active_ids: HashSet<_> = facts
        .iter()
        .filter(|(_, fact)| fact.eligible)
        .map(|(id, _)| id)
        .collect();
    let mut records: Vec<EntityRow> = if model_matches {
        old.iter()
            .filter(|row| {
                row.linked_memory_ids
                    .iter()
                    .any(|mid| active_ids.contains(mid))
            })
            .map(|row| {
                let mut row = row.clone();
                row.linked_memory_ids.clear();
                row
            })
            .collect()
    } else {
        Vec::new()
    };
    let mut batch = String::new();
    let mut prior_batch_count = records.len();
    for (key, (entity, scope, links, current_batch)) in desired {
        if current_batch != batch {
            prior_batch_count = records.len();
            batch = current_batch;
        }
        let vector = vectors.remove(&key).expect("entity vector planned");
        let matching = records
            .iter()
            .position(|row| row.scope == scope && normalize(&row.text) == key.1)
            .or_else(|| {
                records[..prior_batch_count]
                    .iter()
                    .enumerate()
                    .filter(|(_, row)| row.scope == scope)
                    .filter_map(|(i, row)| {
                        let score = cosine(&vector, &row.embedding);
                        (score >= 0.95).then_some((i, score))
                    })
                    .max_by(|a, b| a.1.total_cmp(&b.1))
                    .map(|(i, _)| i)
            });
        if let Some(i) = matching {
            records[i].linked_memory_ids.extend(links);
            records[i].embedding = vector;
        } else {
            let mid = old_exact
                .get(&key)
                .map(|row| row.mid.clone())
                .unwrap_or_else(|| util::new_id("ent"));
            records.push(EntityRow {
                mid,
                scope,
                text: entity.text,
                entity_type: entity.kind,
                normalized_text: key.1,
                embedding: vector,
                linked_memory_ids: links,
                version: VERSION.into(),
            });
        }
    }
    records.retain(|row| !row.linked_memory_ids.is_empty());
    // The original next reconcile reads entity rows ORDER BY mid ASC. Preserve tie
    // resolution, semantic max-by ordering and exact-match precedence after publishing.
    records.sort_by(|a, b| a.mid.cmp(&b.mid));
    Ok(records)
}

/// The projection cache serialises callers; the formation lock is not required. A primary
/// commit that lands while a projection is prepared moves the dependency revision, so the
/// projection is prepared again rather than published. Callers that read entity rows together
/// with primary records still hold Instance::lock_formation. Snapshot publication follows a
/// successful projection transaction. Restart, lost journal and opaque writes use full
/// authoritative recovery; ordinary changes read only their affected primary IDs.
pub async fn reconcile_locked(eng: &Arc<Engine>, t: &Arc<Instance>) -> Result<()> {
    let cache = t.algorithm_state::<ProjectionCache>();
    let mut cached = cache.lock().await;
    loop {
        let revision = t.store.document_revision(DEPENDENCIES);
        if cached
            .as_ref()
            .is_some_and(|s| s.revision == revision && s.model == eng.models.embed_model_id)
        {
            return Ok(());
        }
        let prior = t.store.get_doc(super::ENTITY_STATE, "projection").await?;
        let known: HashSet<String> = prior
            .as_ref()
            .and_then(|s| s["eligibleMemoryIds"].as_array())
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect();
        let changes = cached
            .as_ref()
            .and_then(|s| t.store.document_changes_since(s.revision, DEPENDENCIES))
            .filter(|changes| {
                ![super::ENTITIES, super::ENTITY_STATE, super::ENTITY_INPUTS]
                    .iter()
                    .any(|table| changes.contains_key(*table))
            });
        let recovery = changes.is_none();
        let (mut facts, old) = if recovery {
            let old = super::repo::entity_rows(t)
                .await?
                .into_iter()
                .map(serde_json::from_value)
                .collect::<std::result::Result<Vec<EntityRow>, _>>()?;
            (BTreeMap::new(), old)
        } else {
            let snapshot = cached.as_ref().expect("cache for incremental changes");
            (snapshot.facts.clone(), snapshot.rows.clone())
        };
        let saved: HashMap<String, Value> = if recovery {
            super::repo::entity_extractions(t)
                .await?
                .into_iter()
                .filter_map(|row| row["mid"].as_str().map(|mid| (mid.to_owned(), row.clone())))
                .collect()
        } else {
            HashMap::new()
        };
        let inputs = if let Some(changes) = changes {
            let mut ids = changes.get(schema::MEMORIES).cloned().unwrap_or_default();
            ids.extend(changes.get(super::SOURCES).into_iter().flatten().cloned());
            if let Some(requests) = changes.get(schema::REQUESTS) {
                ids.extend(
                    facts
                        .iter()
                        .filter(|(_, fact)| fact.request_ids.iter().any(|id| requests.contains(id)))
                        .map(|(mid, _)| mid.clone()),
                );
                // Legacy eligibility can inherit through a correction predecessor.
                // Provenance changes are uncommon; conservatively reconsider unknown
                // active facts while still reusing their cached extraction intermediates.
            }
            if [schema::MEMORIES, super::SOURCES, schema::REQUESTS]
                .iter()
                .any(|table| changes.contains_key(*table))
            {
                ids.extend(
                    facts
                        .iter()
                        .filter(|(_, fact)| fact.doc["ingestionMode"].is_null())
                        .map(|(mid, _)| mid.clone()),
                );
            }
            let inputs = super::repo::entity_memory_inputs_by_id(t, &ids).await?;
            let refreshed: HashSet<_> = inputs
                .iter()
                .filter_map(|doc| doc["mid"].as_str())
                .collect();
            facts.retain(|mid, _| !ids.contains(mid) || refreshed.contains(mid.as_str()));
            inputs
        } else {
            super::repo::entity_memory_inputs(t).await?
        };
        for doc in inputs {
            let mid = doc["mid"].as_str().unwrap_or("").to_owned();
            let previous = facts.get(&mid).filter(|fact| fact.eligible).map(extraction);
            let fact = prepare_fact(
                t,
                doc,
                &known,
                previous.as_ref().or_else(|| saved.get(&mid)),
            )
            .await?;
            facts.insert(mid, fact);
        }
        let unchanged = !recovery
            && cached
                .as_ref()
                .is_some_and(|s| s.facts == facts && s.model == eng.models.embed_model_id);
        if unchanged {
            // Unrelated request/status/access metadata writes do not alter the projection.
            cached.as_mut().unwrap().revision = revision;
            return Ok(());
        }
        let inputs: Vec<_> = facts.values().map(|fact| &fact.doc).collect();
        let fingerprint = util::hex_hash(&serde_json::to_string(&inputs)?);
        let model_matches = prior.as_ref().is_some_and(|state| {
            state["version"] == VERSION && state["embeddingModel"] == eng.models.embed_model_id
        });
        let prepared_fingerprint = util::hex_hash(&serde_json::to_string(&facts)?);
        let manifest_matches = recovery
            && prior.as_ref().is_some_and(|state| {
                model_matches
                    && state["fingerprint"] == fingerprint
                    && state["preparedFingerprint"] == prepared_fingerprint
                    && state["entityCount"].as_u64() == Some(old.len() as u64)
            });
        let rows = if manifest_matches {
            old.clone()
        } else {
            project(eng, &facts, &old, model_matches).await?
        };
        // Search reconciles without the formation lock, and low-level store callers never take
        // it: a primary commit while this projection was prepared means preparing it again.
        if t.store.document_revision(DEPENDENCIES) != revision {
            continue;
        }
        let mut tx = Tx::new();
        let new_by_id: HashMap<_, _> = rows.iter().map(|row| (&row.mid, row)).collect();
        let old_by_id: HashMap<_, _> = old.iter().map(|row| (&row.mid, row)).collect();
        for row in &old {
            if !new_by_id.contains_key(&row.mid) {
                tx.delete(super::ENTITIES, &row.mid)?;
            }
        }
        for row in &rows {
            if old_by_id.get(&row.mid).is_none_or(|old| *old != row) {
                tx.put(super::ENTITIES, &row.mid, serde_json::to_value(row)?)?;
            }
        }
        for (mid, fact) in &facts {
            if fact.eligible
                && (recovery
                    || cached
                        .as_ref()
                        .and_then(|s| s.facts.get(mid))
                        .is_none_or(|old| old.doc["text"] != fact.doc["text"] || !old.eligible))
            {
                tx.put(super::ENTITY_INPUTS, mid, extraction(fact))?;
            }
        }
        let previous_ids: BTreeSet<_> = if recovery {
            saved.keys().cloned().collect()
        } else {
            cached
                .as_ref()
                .into_iter()
                .flat_map(|s| {
                    s.facts
                        .iter()
                        .filter(|(_, fact)| fact.eligible)
                        .map(|(id, _)| id.clone())
                })
                .collect()
        };
        for mid in previous_ids {
            if !facts.get(&mid).is_some_and(|fact| fact.eligible) {
                tx.delete(super::ENTITY_INPUTS, &mid)?;
            }
        }
        tx.put(super::ENTITY_STATE, "projection", json!({"version":VERSION,"fingerprint":fingerprint,
            "embeddingModel":eng.models.embed_model_id,"entityCount":rows.len(),"primaryGeneration":revision,"preparedFingerprint":prepared_fingerprint,
            "eligibleMemoryIds":facts.iter().filter(|(_,fact)|fact.eligible).map(|(id,_)|id).collect::<Vec<_>>(),"updatedAt":util::now_str()}))?;
        let Some(revision) = t
            .store
            .commit_if_revision(tx, DEPENDENCIES, revision)
            .await?
        else {
            continue;
        };
        *cached = Some(Snapshot {
            revision,
            model: eng.models.embed_model_id.clone(),
            facts,
            rows,
        });
        return Ok(());
    }
}

/// SDK per-query entity top-500 / >=0.5 scoring over a committed typed snapshot. Takes no
/// formation lock: a formation job holds it across its model call and retries.
pub async fn boosts(
    eng: &Arc<Engine>,
    t: &Arc<Instance>,
    query: &str,
    project: &str,
    platform: &str,
) -> Result<HashMap<String, f32>> {
    reconcile_locked(eng, t).await?;
    let entities = extract_entities(query)
        .into_iter()
        .filter(|entity| entity.kind != "TOPIC")
        .take(8)
        .collect::<Vec<_>>();
    if entities.is_empty() {
        return Ok(HashMap::new());
    }
    let texts = entities.iter().map(|e| e.text.clone()).collect::<Vec<_>>();
    let vectors = eng.models.embed(&texts).await?;
    let cache = t.algorithm_state::<ProjectionCache>();
    let cached = cache.lock().await;
    let snapshot = cached.as_ref().expect("reconciled projection snapshot");
    let allowed: HashSet<_> = snapshot
        .facts
        .iter()
        .filter(|(_, fact)| visible(&fact.doc["scope"], project, platform))
        .map(|(id, _)| id)
        .collect();
    let mut result: HashMap<String, f32> = HashMap::new();
    for vector in vectors {
        let mut matches = Vec::new();
        for row in &snapshot.rows {
            if !visible(&row.scope, project, platform) {
                continue;
            }
            let score = cosine(&vector, &row.embedding);
            if score >= 0.5 {
                matches.push((row, score));
            }
        }
        matches.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.mid.cmp(&b.0.mid)));
        for (row, score) in matches.into_iter().take(500) {
            let n = row.linked_memory_ids.len().max(1) as f64;
            let boost = (score * 0.5 / (1.0 + 0.001 * (n - 1.0).powi(2))) as f32;
            for mid in &row.linked_memory_ids {
                if allowed.contains(mid) {
                    result
                        .entry(mid.clone())
                        .and_modify(|b| *b = b.max(boost))
                        .or_insert(boost);
                }
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_threshold_uses_sdk_double_accumulation() {
        let value = (0.5_f64.sqrt()) as f32;
        let a = [value, value, 0.0];
        let b = [value, 0.0, value];
        assert!(util::cosine(&a, &b) >= 0.5);
        assert_eq!(cosine(&a, &b), 0.49999999999999994);
        assert!(cosine(&a, &b) < 0.5);
        assert!(cosine(&a, &a) >= 0.95);
        assert!(cosine(&[0.0], &[0.0]).is_nan());
    }
}

#[cfg(test)]
mod projection_tests;
