//! Entity projection against a store: incremental reconciliation, its cache, recovery and provenance.
use super::*;
use memory_config::ServiceConfig;
use memory_domain::types::{MemScope, MemoryRec, S_CURRENT, T_FACT};
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
