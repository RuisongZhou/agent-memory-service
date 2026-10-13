//! Persistent integrity and instruction tests. No generation or external model calls.
use super::{engine, memory, tenant};
use memory_algo_memos::formation::relations::{self, plan, FactSource, NewFact, WriteCtx};
use memory_config::{MemoryStrategy, ServiceConfig};
use memory_domain::types::*;
use memory_engine::app::{ingest, management};
use memory_engine::retrieval;
use memory_rules::util;
use memory_store::schema;
use memory_store::store::{to_doc, Tx};
use serde_json::json;

pub async fn selftest() {
    let eng = engine(ServiceConfig {
        memory_strategy: MemoryStrategy::Memos,
        ..Default::default()
    });
    let (t, caller, dir) = tenant(&eng, "integrity", "agent").await;
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

    // A conflicting fact opens a question; resolving it keeps the chosen memory.
    let port = MemoryRec {
        id: "port".into(),
        text: "Port is 8200".into(),
        ..fact.clone()
    };
    t.store
        .put_doc(schema::MEMORIES, &port.id, &port)
        .await
        .unwrap();
    let applied = relations::apply(
        &eng,
        &t,
        vec![NewFact {
            fidx: 1,
            text: "Port is 8100".into(),
            scope: port.scope.clone(),
            entity_ids: vec![],
            effective_at: "2026-10-11".into(),
            origin: ORIGIN_USER.into(),
            sources: vec![FactSource {
                turn_id: "T1".into(),
                span: "Port is 8100".into(),
                role: "user".into(),
                stated_at: "2026-10-11T00:00:00Z".into(),
                stated: (1, 0),
            }],
            relations: vec![plan::Rel {
                target: plan::Target::Old(port.id.clone()),
                relation: "conflicts".into(),
            }],
            reason: None,
        }],
        WriteCtx {
            platform: caller.platform.clone(),
            now: util::now_str(),
            turns: vec![],
            text_request: None,
            text_turn_id: None,
            new_entities: vec![],
        },
    )
    .await
    .unwrap();
    let open: Vec<UncertaintyRec> = t
        .store
        .select_as(
            "SELECT * OMIT id FROM uncertainties WHERE status = 'open'",
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(open.len(), 1);
    let chosen = &applied.created[0];
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
