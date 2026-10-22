//! Frozen pre-optimization projection planner used only for regression comparison.
//! Keeps JSON vector conversion/full-text extraction independent of the typed planner.
use super::*;
use memory_config::{MemoryStrategy, ServiceConfig};
use memory_domain::types::{MemScope, MemoryRec, S_CURRENT, T_FACT};
use memory_engine::runtime::{llm::Llm, models::ModelHub};

async fn legacy_project(
    eng: &Arc<Engine>,
    facts: &BTreeMap<String, Fact>,
    old_rows: &[EntityRow],
    model_matches: bool,
) -> Result<Vec<EntityRow>> {
    let old = old_rows
        .iter()
        .map(serde_json::to_value)
        .collect::<std::result::Result<Vec<Value>, _>>()?;
    let eligible: Vec<Value> = facts
        .values()
        .filter(|fact| fact.eligible)
        .map(|fact| fact.doc.clone())
        .collect();
    let mut ordered: Vec<_> = facts
        .iter()
        .filter(|(_, fact)| fact.eligible)
        .map(|(mid, fact)| {
            (
                fact.doc["recordedAt"].as_str().unwrap_or("").to_owned(),
                fact.seq,
                fact.doc["formationPosition"].as_u64().unwrap_or(u64::MAX),
                mid.clone(),
                &fact.doc,
            )
        })
        .collect();
    ordered.sort_by(|a, b| (&a.0, a.1, a.2, &a.3).cmp(&(&b.0, b.1, b.2, &b.3)));
    let mut desired: Vec<((String, String), (Entity, Value, BTreeSet<String>, String))> =
        Vec::new();
    let mut positions = HashMap::new();
    for (_, seq, _, mid, doc) in ordered {
        let scope = doc["scope"].clone();
        let batch = if seq == u64::MAX {
            format!("correction:{mid}")
        } else {
            format!("request:{seq}")
        };
        for entity in extract_entities(doc["text"].as_str().unwrap_or("")) {
            let key = (scope_key(&scope), normalize(&entity.text));
            let index = if let Some(index) = positions.get(&key) {
                *index
            } else {
                let index = desired.len();
                positions.insert(key.clone(), index);
                desired.push((key, (entity, scope.clone(), BTreeSet::new(), batch.clone())));
                index
            };
            desired[index].1 .2.insert(mid.clone());
        }
    }
    // Existing vectors can be reused for an exact canonical text with the same model.
    // New entities are batch embedded once; no generative model is involved.
    let mut texts = Vec::new();
    let mut needed = Vec::new();
    let mut vectors: BTreeMap<(String, String), Vec<f32>> = BTreeMap::new();
    for (key, (entity, _, _, _)) in &desired {
        let exact = if model_matches {
            old.iter().find(|row| {
                scope_key(&row["scope"]) == key.0
                    && normalize(row["text"].as_str().unwrap_or("")) == key.1
            })
        } else {
            None
        };
        if let Some(row) = exact {
            let vector: Vec<f32> = serde_json::from_value(row["embedding"].clone())?;
            if !vector.is_empty() {
                vectors.insert(key.clone(), vector);
                continue;
            }
        }
        needed.push(key.clone());
        texts.push(entity.text.clone());
    }
    let embedded = eng.models.embed(&texts).await?;
    ensure!(
        embedded.len() == needed.len(),
        "entity embedding batch length mismatch"
    );
    for (key, vector) in needed.into_iter().zip(embedded) {
        ensure!(!vector.is_empty(), "empty entity embedding");
        vectors.insert(key, (*vector).clone());
    }
    let active_ids: HashSet<_> = eligible
        .iter()
        .filter_map(|doc| doc["mid"].as_str())
        .collect();
    // Keep an existing canonical entity only while at least one linked fact survives.
    // Reconstruct all links from current texts, so corrected/deleted facts disappear.
    let mut records: Vec<Value> = if model_matches {
        old.iter()
            .filter(|row| {
                row["linkedMemoryIds"].as_array().is_some_and(|links| {
                    links
                        .iter()
                        .any(|mid| mid.as_str().is_some_and(|mid| active_ids.contains(mid)))
                })
            })
            .map(|row| {
                let mut row = row.clone();
                row["linkedMemoryIds"] = json!([]);
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
        // Exact normalization takes precedence over semantic merging. The semantic
        // fallback is local to the same durable visibility scope, never another project.
        let matching = records
            .iter()
            .position(|row| {
                row["scope"] == scope && normalize(row["text"].as_str().unwrap_or("")) == key.1
            })
            .or_else(|| {
                records[..prior_batch_count]
                    .iter()
                    .enumerate()
                    .filter(|(_, row)| row["scope"] == scope)
                    .filter_map(|(i, row)| {
                        let old: Vec<f32> =
                            serde_json::from_value(row["embedding"].clone()).ok()?;
                        let score = cosine(&vector, &old);
                        (score >= 0.95).then_some((i, score))
                    })
                    .max_by(|a, b| a.1.total_cmp(&b.1))
                    .map(|(i, _)| i)
            });
        if let Some(i) = matching {
            let mut merged: BTreeSet<String> =
                serde_json::from_value(records[i]["linkedMemoryIds"].clone())?;
            merged.extend(links);
            records[i]["linkedMemoryIds"] = json!(merged);
            records[i]["embedding"] = json!(vector);
        } else {
            let exact = old.iter().find(|row| {
                row["scope"] == scope && normalize(row["text"].as_str().unwrap_or("")) == key.1
            });
            let mid = exact
                .and_then(|row| row["mid"].as_str())
                .map(str::to_owned)
                .unwrap_or_else(|| util::new_id("ent"));
            records.push(json!({"mid":mid,"scope":scope,"text":entity.text,"entityType":entity.kind,
                "normalizedText":key.1,"embedding":vector,"linkedMemoryIds":links,"version":VERSION}));
        }
    }
    records.retain(|row| {
        row["linkedMemoryIds"]
            .as_array()
            .is_some_and(|links| !links.is_empty())
    });
    let mut rows = records
        .into_iter()
        .map(serde_json::from_value)
        .collect::<std::result::Result<Vec<EntityRow>, _>>()?;
    rows.sort_by(|a, b| a.mid.cmp(&b.mid));
    Ok(rows)
}

fn engine() -> Arc<Engine> {
    Engine::new(
        ServiceConfig {
            memory_strategy: MemoryStrategy::Mem0,
            ..Default::default()
        },
        Llm::new("http://127.0.0.1:1", "", "disabled", 1, 1).unwrap(),
        ModelHub::mock(),
        Arc::new(crate::Mem0),
    )
}

fn fact_doc(mid: &str, text: &str, mode: Option<&str>, time: &str) -> Value {
    let record = MemoryRec {
        id: mid.into(),
        mtype: T_FACT.into(),
        text: text.into(),
        scope: MemScope::project("a"),
        status: S_CURRENT.into(),
        recorded_at: time.into(),
        ..Default::default()
    };
    let mut doc = memory_store::store::to_doc(&record).unwrap();
    if let Some(mode) = mode {
        doc["ingestionMode"] = json!(mode);
    }
    doc
}

async fn tenant(eng: &Engine) -> (Arc<Tenant>, std::path::PathBuf) {
    let root = std::env::temp_dir().join(util::new_id("mem0_entities_test"));
    let t = Arc::new(
        Tenant::open(&root, eng.models.dim, crate::SCHEMA)
            .await
            .unwrap(),
    );
    (t, root)
}

fn comparable(rows: &[EntityRow]) -> Vec<Value> {
    let mut rows: Vec<_> = rows
        .iter()
        .map(|row| {
            let mut value = serde_json::to_value(row).unwrap();
            value.as_object_mut().unwrap().remove("mid");
            value
        })
        .collect();
    rows.sort_by_key(Value::to_string);
    rows
}

#[tokio::test]
async fn typed_replay_matches_frozen_full_extraction_planner() -> Result<()> {
    let eng = engine();
    let (t, root) = tenant(&eng).await;
    let mut facts = BTreeMap::new();
    let mut old = Vec::new();
    for (batch, (id, text)) in [
        ("m1", "User met Alice, Bob, London."),
        ("m2", "User met Bob, Acme Corp, Paris."),
        ("m3", "User met Alice, Acme Corp."),
    ]
    .into_iter()
    .enumerate()
    {
        let doc = fact_doc(
            id,
            text,
            Some("inferred"),
            &format!("2026-01-0{}T00:00:00Z", batch + 1),
        );
        let mut fact = prepare_fact(&t, doc, &HashSet::new(), None).await?;
        fact.seq = batch as u64;
        facts.insert(id.into(), fact);
        if batch == 1 {
            // An existing canonical vector can have been overwritten by an alias.
            // The next historical replay must preserve that exact seed behavior.
            let bob = eng.models.embed(&["Bob".into()]).await?;
            if let Some(alice) = old
                .iter_mut()
                .find(|row: &&mut EntityRow| row.text == "Alice")
            {
                alice.embedding = (*bob[0]).clone();
            }
        }
        let legacy = legacy_project(&eng, &facts, &old, true).await?;
        let typed = project(&eng, &facts, &old, true).await?;
        assert_eq!(comparable(&typed), comparable(&legacy));
        for row in &typed {
            if let Some(previous) = old
                .iter()
                .find(|old| old.text == row.text && old.scope == row.scope)
            {
                assert_eq!(row.mid, previous.mid);
            }
        }
        old = typed;
    }
    facts.remove("m1");
    let legacy = legacy_project(&eng, &facts, &old, true).await?;
    let typed = project(&eng, &facts, &old, true).await?;
    assert_eq!(comparable(&typed), comparable(&legacy));
    assert!(typed
        .iter()
        .all(|row| !row.linked_memory_ids.contains("m1")));
    drop(t);
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[tokio::test]
async fn cache_handles_correction_raw_delete_projection_failure_and_recovery() -> Result<()> {
    let eng = engine();
    let (t, root) = tenant(&eng).await;
    t.store
        .put_doc(
            schema::MEMORIES,
            "m1",
            &fact_doc(
                "m1",
                "User met Alice, Bob, London.",
                Some("inferred"),
                "2026-01-01T00:00:00Z",
            ),
        )
        .await?;
    reconcile_locked(&eng, &t).await?;
    let before = boosts(&eng, &t, "\"Alice\"", "a", "test").await?;
    assert!(before.contains_key("m1"));
    let projection_revision = t.store.document_revision(&[crate::ENTITY_STATE]);
    reconcile_locked(&eng, &t).await?;
    assert_eq!(
        t.store.document_revision(&[crate::ENTITY_STATE]),
        projection_revision,
        "hot reconcile must not write the projection"
    );
    t.store
        .put_doc(
            schema::MEMORIES,
            "raw1",
            &fact_doc(
                "raw1",
                "User met Alice, Bob, London.",
                Some("raw"),
                "2026-01-02T00:00:00Z",
            ),
        )
        .await?;
    reconcile_locked(&eng, &t).await?;
    assert!(!boosts(&eng, &t, "\"Alice\"", "a", "test")
        .await?
        .contains_key("raw1"));
    assert!(t
        .store
        .get_doc(crate::ENTITY_INPUTS, "raw1")
        .await?
        .is_none());
    // A correction outside formation inherits eligibility from its inferred predecessor.
    let mut correction = fact_doc("m2", "User met Carol, Paris.", None, "2026-01-03T00:00:00Z");
    correction["supersedes"] = json!("m1");
    let mut tx = Tx::new();
    tx.put(schema::MEMORIES, "m2", correction)?;
    tx.merge(
        schema::MEMORIES,
        "m1",
        json!({"status":"superseded","supersededBy":"m2"}),
    )?;
    t.store.commit(tx).await?;
    // Make the rebuildable projection transaction fail; its old snapshot remains tagged
    // with the old revision and cannot be published as the new authoritative version.
    crate::repo::reject_entity_manifest(&t, true).await?;
    let cached_before = t
        .algorithm_state::<ProjectionCache>()
        .lock()
        .await
        .as_ref()
        .unwrap()
        .revision;
    assert!(reconcile_locked(&eng, &t).await.is_err());
    assert_eq!(
        t.algorithm_state::<ProjectionCache>()
            .lock()
            .await
            .as_ref()
            .unwrap()
            .revision,
        cached_before
    );
    crate::repo::reject_entity_manifest(&t, false).await?;
    reconcile_locked(&eng, &t).await?;
    let corrected = boosts(&eng, &t, "\"Carol\"", "a", "test").await?;
    assert!(corrected.contains_key("m2"));
    assert!(!corrected.contains_key("m1"));
    assert!(t.store.get_doc(crate::ENTITY_INPUTS, "m1").await?.is_none());
    let mut tx = Tx::new();
    tx.delete(schema::MEMORIES, "m2")?;
    t.store.commit(tx).await?;
    reconcile_locked(&eng, &t).await?;
    assert!(boosts(&eng, &t, "\"Carol\"", "a", "test").await?.is_empty());
    assert!(t.store.get_doc(crate::ENTITY_INPUTS, "m2").await?.is_none());
    // A warm cache must recover from an opaque projection purge, and malformed saved
    // extraction data must be a cache miss rather than blocking authoritative recovery.
    t.store
        .put_doc(
            schema::MEMORIES,
            "m3",
            &fact_doc(
                "m3",
                "User met Dave, Berlin.",
                Some("inferred"),
                "2026-01-04T00:00:00Z",
            ),
        )
        .await?;
    reconcile_locked(&eng, &t).await?;
    let mut tx = Tx::new();
    crate::repo::purge_entity_projection(&mut tx);
    tx.put(
        crate::ENTITY_INPUTS,
        "m3",
        json!({"version":VERSION,"text":"User met Dave, Berlin.","entities":"malformed"}),
    )?;
    t.store.commit(tx).await?;
    assert!(boosts(&eng, &t, "\"Dave\"", "a", "test")
        .await?
        .contains_key("m3"));
    assert!(t.store.get_doc(crate::ENTITY_INPUTS, "m3").await?.unwrap()["entities"].is_array());
    let rows_before = crate::repo::entity_rows(&t).await?;
    // Simulate a restart before publishing a new primary batch.
    t.store
        .put_doc(
            schema::MEMORIES,
            "m4",
            &fact_doc(
                "m4",
                "User met Eve, Rome.",
                Some("inferred"),
                "2026-01-05T00:00:00Z",
            ),
        )
        .await?;
    drop(t);
    let mut reopened = None;
    for _ in 0..40 {
        match Tenant::open(&root, eng.models.dim, crate::SCHEMA).await {
            Ok(tenant) => {
                reopened = Some(Arc::new(tenant));
                break;
            }
            Err(error) if error.to_string() == "open surrealkv" => {
                tokio::time::sleep(std::time::Duration::from_millis(25)).await
            }
            Err(error) => return Err(error),
        }
    }
    let reopened = reopened.expect("SurrealKV release after dropping tenant");
    assert!(reopened
        .algorithm_state::<ProjectionCache>()
        .lock()
        .await
        .is_none());
    assert!(boosts(&eng, &reopened, "\"Eve\"", "a", "test")
        .await?
        .contains_key("m4"));
    for row in rows_before {
        assert!(crate::repo::entity_rows(&reopened)
            .await?
            .iter()
            .any(|current| current["mid"] == row["mid"]));
    }
    // A different store must never reuse this instance's state.
    let (other, other_root) = tenant(&eng).await;
    assert!(boosts(&eng, &other, "\"Eve\"", "a", "test")
        .await?
        .is_empty());
    assert!(!Arc::ptr_eq(
        &reopened.algorithm_state::<ProjectionCache>(),
        &other.algorithm_state::<ProjectionCache>()
    ));
    drop(other);
    drop(reopened);
    std::fs::remove_dir_all(other_root)?;
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[tokio::test]
async fn additive_changes_use_changed_ids_and_legacy_provenance_is_reconsidered() -> Result<()> {
    let eng = engine();
    let (t, root) = tenant(&eng).await;
    t.store
        .put_doc(
            schema::MEMORIES,
            "first",
            &fact_doc(
                "first",
                "User met Alice, London.",
                Some("inferred"),
                "2026-01-01T00:00:00Z",
            ),
        )
        .await?;
    reconcile_locked(&eng, &t).await?;
    let previous = t
        .algorithm_state::<ProjectionCache>()
        .lock()
        .await
        .as_ref()
        .unwrap()
        .revision;
    t.store
        .put_doc(
            schema::MEMORIES,
            "second",
            &fact_doc(
                "second",
                "User met Bob, Paris.",
                Some("inferred"),
                "2026-01-02T00:00:00Z",
            ),
        )
        .await?;
    let changes = t
        .store
        .document_changes_since(previous, DEPENDENCIES)
        .expect("ordinary admission has an exact journal");
    assert_eq!(changes[schema::MEMORIES], BTreeSet::from(["second".into()]));
    reconcile_locked(&eng, &t).await?;
    let extracted = t
        .store
        .document_changes_since(previous, &[crate::ENTITY_INPUTS])
        .unwrap();
    assert_eq!(
        extracted[crate::ENTITY_INPUTS],
        BTreeSet::from(["second".into()]),
        "previous text extraction must not be rewritten"
    );
    // Raw -> inferred in place must extract the old text, rather than reusing the
    // ineligible fact's empty in-memory extraction placeholder.
    t.store
        .put_doc(
            schema::MEMORIES,
            "raw",
            &fact_doc(
                "raw",
                "User met Carol, Rome.",
                Some("raw"),
                "2026-01-03T00:00:00Z",
            ),
        )
        .await?;
    reconcile_locked(&eng, &t).await?;
    t.store
        .merge_doc(schema::MEMORIES, "raw", json!({"ingestionMode":"inferred"}))
        .await?;
    assert!(boosts(&eng, &t, "\"Carol\"", "a", "test")
        .await?
        .contains_key("raw"));
    // Historical source backfill can make a formerly unknown active legacy fact
    // eligible, even though no authoritative memory row changed in that commit.
    t.store
        .put_doc(
            schema::MEMORIES,
            "legacy",
            &fact_doc(
                "legacy",
                "User met Dave, Berlin.",
                None,
                "2026-01-04T00:00:00Z",
            ),
        )
        .await?;
    reconcile_locked(&eng, &t).await?;
    assert!(!boosts(&eng, &t, "\"Dave\"", "a", "test")
        .await?
        .contains_key("legacy"));
    let mut tx = Tx::new();
    tx.put(
        schema::REQUESTS,
        "request1",
        json!({"inputSeq":5,"infer":true,"formed":false}),
    )?;
    tx.put(
        crate::SOURCES,
        "legacy",
        json!({"inputRequestIds":["request1"]}),
    )?;
    t.store.commit(tx).await?;
    reconcile_locked(&eng, &t).await?;
    assert!(!boosts(&eng, &t, "\"Dave\"", "a", "test")
        .await?
        .contains_key("legacy"));
    t.store
        .merge_doc(schema::REQUESTS, "request1", json!({"formed":true}))
        .await?;
    assert!(boosts(&eng, &t, "\"Dave\"", "a", "test")
        .await?
        .contains_key("legacy"));
    // A later unrelated receipt update changes the dependency revision but must
    // leave all canonical rows and the projection transaction untouched.
    let rows = crate::repo::entity_rows(&t).await?;
    let projection_revision = t.store.document_revision(&[crate::ENTITY_STATE]);
    t.store
        .merge_doc(schema::REQUESTS, "request1", json!({"status":"completed"}))
        .await?;
    reconcile_locked(&eng, &t).await?;
    assert_eq!(crate::repo::entity_rows(&t).await?, rows);
    assert_eq!(
        t.store.document_revision(&[crate::ENTITY_STATE]),
        projection_revision
    );
    drop(t);
    std::fs::remove_dir_all(root)?;
    Ok(())
}
