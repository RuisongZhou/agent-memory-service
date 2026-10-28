//! Entity projection against a store: incremental reconciliation, its cache, recovery and provenance.
use super::*;
use memory_config::ServiceConfig;
use memory_domain::types::{ApiError, IdReq, ListEntitiesReq};
use memory_domain::types::{MemScope, MemoryRec, S_CURRENT, T_FACT};
use memory_engine::app::management;
use memory_engine::runtime::{llm::Llm, models::ModelHub};

fn engine() -> Arc<Engine> {
    Engine::new(
        ServiceConfig::default(),
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

async fn instance(eng: &Engine) -> (Arc<Instance>, std::path::PathBuf) {
    let root = std::env::temp_dir().join(util::new_id("mem0_entities_test"));
    let t = Arc::new(
        Instance::open(&root, &eng.cfg.surreal_bin, eng.models.dim, crate::SCHEMA)
            .await
            .unwrap(),
    );
    (t, root)
}

#[tokio::test]
async fn cache_handles_correction_raw_delete_projection_failure_and_recovery() -> Result<()> {
    let eng = engine();
    let (t, root) = instance(&eng).await;
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
    // The projection keeps named entities and drops the TOPIC word runs the extractor also finds.
    let text = "User enjoys hiking mountain trails with Alice";
    assert!(extract_entities(text).iter().any(|e| e.kind == "TOPIC"));
    let kept = projected(text);
    assert!(
        !kept.is_empty() && kept.iter().all(|e| e.kind != "TOPIC"),
        "{kept:?}"
    );
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
    // A formation job holds the formation lock across its model call and retries; search scoring
    // reconciles without waiting for it.
    let formation = t.lock_formation().await;
    let dave = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        boosts(&eng, &t, "\"Dave\"", "a", "test"),
    )
    .await
    .expect("search waited for the formation lock")?;
    drop(formation);
    assert!(dave.contains_key("m3"));
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
    // Dropping the instance stops its database server; reopening waits until the datastore is released.
    let reopened =
        Arc::new(Instance::open(&root, &eng.cfg.surreal_bin, eng.models.dim, crate::SCHEMA).await?);
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
    let (other, other_root) = instance(&eng).await;
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
    let (t, root) = instance(&eng).await;
    // Before the projection is built there is nothing to read, and no time it was updated.
    let empty = management::list_entities(&eng, &t, ListEntitiesReq::default())
        .await
        .unwrap();
    assert_eq!(
        empty,
        json!({"entities": [], "types": [], "nextCursor": null, "projectionUpdatedAt": null})
    );
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

    // ---- the web UI's reads, which show the projection as stored. Alice is in two project-a facts, and another
    // project has its own Alice.
    t.store
        .put_doc(
            schema::MEMORIES,
            "sixth",
            &fact_doc(
                "sixth",
                "User met Alice, \"Orion\".",
                Some("inferred"),
                "2026-01-06T00:00:00Z",
            ),
        )
        .await?;
    let mut elsewhere = fact_doc(
        "seventh",
        "User met Alice, Oslo.",
        Some("inferred"),
        "2026-01-07T00:00:00Z",
    );
    elsewhere["scope"] = json!({"kind": "project", "project": "b"});
    t.store
        .put_doc(schema::MEMORIES, "seventh", &elsewhere)
        .await?;
    reconcile_locked(&eng, &t).await?;
    let rows = crate::repo::entity_rows(&t).await?;
    let updated_at = t
        .store
        .get_doc(crate::ENTITY_STATE, "projection")
        .await?
        .unwrap()["updatedAt"]
        .clone();
    let list = |req: Value| {
        management::list_entities(
            &eng,
            &t,
            serde_json::from_value::<ListEntitiesReq>(req).unwrap(),
        )
    };
    let ids = |page: &Value| -> Vec<String> {
        page["entities"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["id"].as_str().unwrap().to_string())
            .collect()
    };
    // The ids of the stored rows `keep` accepts, most linked first and ties by id, descending.
    let expected = |keep: &dyn Fn(&Value) -> bool| -> Vec<String> {
        let mut found: Vec<(usize, String)> = rows
            .iter()
            .filter(|row| keep(row))
            .map(|row| {
                let links = row["linkedMemoryIds"].as_array().unwrap().len();
                (links, row["mid"].as_str().unwrap().to_string())
            })
            .collect();
        found.sort_by(|a, b| b.cmp(a));
        found.into_iter().map(|(_, id)| id).collect()
    };
    let in_project = |project: &'static str| move |row: &Value| row["scope"]["project"] == project;
    let all = list(json!({})).await.unwrap();
    assert_eq!(ids(&all), expected(&|_| true));
    assert_eq!(all["projectionUpdatedAt"], updated_at);
    assert!(all["nextCursor"].is_null());
    let counts: Vec<u64> = all["entities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["memoryCount"].as_u64().unwrap())
        .collect();
    assert!(
        counts.windows(2).all(|w| w[0] >= w[1]) && counts[0] == 2,
        "{counts:?}"
    );
    // No vector, normalized text or linked ids in a listed row.
    let first = all["entities"][0].as_object().unwrap();
    let mut fields: Vec<&str> = first.keys().map(String::as_str).collect();
    fields.sort();
    assert_eq!(fields, ["entityType", "id", "memoryCount", "scope", "text"]);
    // The types count the filter's project alone, so choosing a type leaves the others in the list.
    let types = |page: &Value| -> BTreeMap<String, i64> {
        page["types"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| {
                (
                    t["name"].as_str().unwrap().into(),
                    t["count"].as_i64().unwrap(),
                )
            })
            .collect()
    };
    let by_type = |keep: &dyn Fn(&Value) -> bool| -> BTreeMap<String, i64> {
        let mut counts = BTreeMap::new();
        for row in rows.iter().filter(|row| keep(row)) {
            *counts
                .entry(row["entityType"].as_str().unwrap().to_string())
                .or_default() += 1;
        }
        counts
    };
    assert!(by_type(&|_| true).len() >= 2, "{rows:?}");
    assert_eq!(types(&all), by_type(&|_| true));
    let project_a = list(json!({"project": "a"})).await.unwrap();
    assert_eq!(ids(&project_a), expected(&in_project("a")));
    assert_eq!(types(&project_a), by_type(&in_project("a")));
    let quoted = list(json!({"project": "a", "entityType": "QUOTED"}))
        .await
        .unwrap();
    assert_eq!(
        ids(&quoted),
        expected(&|row| in_project("a")(row) && row["entityType"] == "QUOTED")
    );
    assert!(!ids(&quoted).is_empty());
    assert_eq!(types(&quoted), types(&project_a));
    let alice = list(json!({"text": "ALIC"})).await.unwrap();
    assert_eq!(
        ids(&alice),
        expected(&|row| row["normalizedText"].as_str().unwrap().contains("alic"))
    );
    assert_eq!(ids(&alice).len(), 2);
    // Pages of two, the cursor carrying over entities that link as many memories as the last one did.
    let mut paged = vec![];
    let mut cursor = Value::Null;
    loop {
        let page = list(json!({"limit": 2, "cursor": cursor})).await.unwrap();
        assert!(ids(&page).len() <= 2);
        paged.extend(ids(&page));
        cursor = page["nextCursor"].clone();
        if cursor.is_null() {
            break;
        }
    }
    assert_eq!(paged, ids(&all));
    for bad in [
        json!({"limit": 0}),
        json!({"limit": 201}),
        json!({"cursor": "2"}),
        json!({"cursor": "x:entx"}),
        json!({"cursor": "2:no such"}),
    ] {
        assert!(
            matches!(list(bad.clone()).await, Err(ApiError::InvalidArgument(_))),
            "{bad}"
        );
    }

    // One entity, with the facts that link it, newest recorded first.
    let alice_a = rows
        .iter()
        .find(|row| row["text"] == "Alice" && in_project("a")(row))
        .unwrap();
    let alice_id = alice_a["mid"].as_str().unwrap();
    let shown = management::entity(
        &eng,
        &t,
        IdReq {
            id: alice_id.into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        shown["entity"],
        json!({"id": alice_id, "text": "Alice", "entityType": alice_a["entityType"],
            "normalizedText": "alice", "scope": {"kind": "project", "project": "a"}, "memoryCount": 2})
    );
    let linked: Vec<&str> = shown["memories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(linked, ["sixth", "first"]);
    assert_eq!(shown["projectionUpdatedAt"], updated_at);
    assert!(shown["memories"][0].get("statedAt").is_some());
    let get = |id: &str| management::entity(&eng, &t, IdReq { id: id.into() });
    assert!(matches!(
        get("no such").await,
        Err(ApiError::InvalidArgument(_))
    ));
    assert!(matches!(
        get("entmissing").await,
        Err(ApiError::NotFound(_))
    ));

    // The entities of one memory, by text.
    let of = |id: &str| management::memory_entities(&eng, &t, IdReq { id: id.into() });
    let first_entities = of("first").await.unwrap();
    let mut want: Vec<&Value> = rows
        .iter()
        .filter(|row| {
            row["linkedMemoryIds"]
                .as_array()
                .unwrap()
                .contains(&json!("first"))
        })
        .collect();
    want.sort_by_key(|row| row["text"].as_str().unwrap().to_string());
    let texts: Vec<&str> = first_entities["entities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["text"].as_str().unwrap())
        .collect();
    assert_eq!(
        texts,
        want.iter()
            .map(|row| row["text"].as_str().unwrap())
            .collect::<Vec<_>>()
    );
    assert!(texts.contains(&"Alice") && texts.contains(&"London"));
    assert_eq!(first_entities["projectionUpdatedAt"], updated_at);
    assert!(!of("raw").await.unwrap()["entities"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(matches!(
        of("no such").await,
        Err(ApiError::InvalidArgument(_))
    ));
    assert!(matches!(of("mmissing").await, Err(ApiError::NotFound(_))));

    // A read does not bring the projection up to date: a new fact is not there until a search or formation
    // reconciles, and the read writes nothing.
    let revision = t
        .store
        .document_revision(&[crate::ENTITY_STATE, crate::ENTITIES]);
    t.store
        .put_doc(
            schema::MEMORIES,
            "eighth",
            &fact_doc(
                "eighth",
                "User met Zed, Lima.",
                Some("inferred"),
                "2026-01-08T00:00:00Z",
            ),
        )
        .await?;
    assert_eq!(
        list(json!({"text": "zed"})).await.unwrap()["entities"],
        json!([])
    );
    assert_eq!(of("eighth").await.unwrap()["entities"], json!([]));
    assert_eq!(
        t.store
            .document_revision(&[crate::ENTITY_STATE, crate::ENTITIES]),
        revision
    );
    reconcile_locked(&eng, &t).await?;
    assert_eq!(
        list(json!({"text": "zed"})).await.unwrap()["entities"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        of("eighth").await.unwrap()["entities"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    drop(t);
    std::fs::remove_dir_all(root)?;
    Ok(())
}
