//! Persistent integrity and instruction tests. No generation or external model calls.
use super::{engine, fresh_store, memory};
use memory_config::ServiceConfig;
use memory_domain::types::*;
use memory_engine::app::{ingest, management};
use memory_engine::retrieval;
use memory_store::schema;
use memory_store::store::{to_doc, Tx};
use serde_json::json;

#[tokio::test(flavor = "multi_thread")]
async fn selftest() {
    let eng = engine(ServiceConfig::default());
    let (t, caller, dir) = fresh_store(&eng, "integrity", "agent").await;
    let fact = MemoryRec {
        id: "fact".into(),
        mtype: T_FACT.into(),
        text: "Old fact".into(),
        status: S_CURRENT.into(),
        scope: MemScope::project("project"),
        ..Default::default()
    };
    let profile = MemoryRec {
        id: "profile".into(),
        mtype: T_PROFILE.into(),
        used_memory_ids: vec![fact.id.clone()],
        ..fact.clone()
    };
    let skill = MemoryRec {
        id: "skill".into(),
        mtype: T_SKILL.into(),
        used_memory_ids: vec![profile.id.clone()],
        ..profile.clone()
    };
    let historical = MemoryRec {
        id: "history".into(),
        mtype: T_EXPERIENCE.into(),
        derivation: Some(Derivation {
            stage: "experience".into(),
            input_memory_ids: vec![fact.id.clone()],
            input_turn_ids: vec![],
            current_view: false,
        }),
        ..fact.clone()
    };
    let mut tx = Tx::new();
    for rec in [&fact, &profile, &skill, &historical] {
        tx.put(schema::MEMORIES, &rec.id, to_doc(rec).unwrap())
            .unwrap();
    }
    t.store.commit(tx).await.unwrap();
    // Superseding a source stales current views transitively in the same commit; history keeps its lineage.
    let mut tx = Tx::new();
    tx.merge(schema::MEMORIES, &fact.id, json!({"status":S_SUPERSEDED}))
        .unwrap();
    t.store.commit(tx).await.unwrap();
    assert_eq!(memory(&t, "profile").await.status, S_STALE);
    assert_eq!(memory(&t, "skill").await.status, S_STALE);
    assert_eq!(memory(&t, "history").await.status, S_CURRENT);
    assert_eq!(t.store.active_memory_meta().await.unwrap().len(), 1);
    // A view whose source is missing is repaired at startup.
    let missing = MemoryRec {
        id: "missingview".into(),
        used_memory_ids: vec!["removed".into()],
        ..profile.clone()
    };
    t.store
        .put_doc(schema::MEMORIES, &missing.id, &missing)
        .await
        .unwrap();
    t.store.repair_integrity().await.unwrap();
    assert_eq!(memory(&t, "missingview").await.status, S_STALE);

    // Explicit instructions are idempotent and scoped to their project.
    let request = AddMemoriesReq {
        text: Some("Prefer brief answers.".into()),
        memory_type: Some(T_INSTRUCTION.into()),
        turn_id: Some("instruction-1".into()),
        scope: ReqScope {
            project: Some("project".into()),
            session: Some("session".into()),
            ..Default::default()
        },
        ..Default::default()
    };
    let receipt = ingest::add_memories(&eng, &t, &caller, request.clone())
        .await
        .unwrap();
    assert_eq!(receipt["status"], "completed");
    assert_eq!(
        ingest::add_memories(&eng, &t, &caller, request.clone())
            .await
            .unwrap()["duplicate"],
        true
    );
    let context = |project: &str| GetContextReq {
        scope: ReqScope {
            project: Some(project.into()),
            ..Default::default()
        },
        ..Default::default()
    };
    let own = retrieval::get_context(&eng, &t, &caller, context("project"))
        .await
        .unwrap();
    assert!(own["text"]
        .as_str()
        .unwrap()
        .contains("Prefer brief answers."));
    let other = retrieval::get_context(&eng, &t, &caller, context("other"))
        .await
        .unwrap();
    assert!(other["instructions"].as_array().unwrap().is_empty());

    // Legacy competing assertions remain repairable without the retired formation strategy.
    let port = MemoryRec {
        id: "port".into(),
        text: "Port is 8200".into(),
        status: S_CONFLICT.into(),
        related: vec![RelEdge {
            id: "newport".into(),
            relation: "conflicts".into(),
        }],
        ..fact.clone()
    };
    let new_port = MemoryRec {
        id: "newport".into(),
        text: "Port is 8100".into(),
        related: vec![RelEdge {
            id: "port".into(),
            relation: "conflicts".into(),
        }],
        ..port.clone()
    };
    let mut tx = Tx::new();
    for rec in [&port, &new_port] {
        tx.put(schema::MEMORIES, &rec.id, to_doc(rec).unwrap())
            .unwrap();
    }
    t.store.commit(tx).await.unwrap();
    t.store.repair_integrity().await.unwrap();
    let open: Vec<UncertaintyRec> = t
        .store
        .select_as(
            "SELECT * OMIT id FROM uncertainties WHERE status = 'open'",
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(open.len(), 1);
    let chosen = &new_port.id;
    management::resolve_uncertainty(
        &t,
        &caller,
        ResolveUncertaintyReq {
            id: open[0].id.clone(),
            keep_memory_id: chosen.clone(),
            reason: "Verified the configured port".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(memory(&t, chosen).await.status, S_CURRENT);
    assert_eq!(memory(&t, "port").await.status, S_SUPERSEDED);
    assert_eq!(
        t.store
            .count(schema::UNCERTAINTIES, Some("status = 'open'"))
            .await
            .unwrap(),
        0
    );
    assert_eq!(eng.llm.usage_snapshot()["calls"], 0);
    println!("integrity_selftest OK: transitive invalidation, historical lineage, startup repair, scoped/idempotent instructions, conflict questions and resolution; zero LLM calls; data {}", dir.display());
}
