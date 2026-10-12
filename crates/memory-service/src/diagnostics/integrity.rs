//! Persistent integrity and instruction tests. No generation or external model calls.
use memory_config::ServiceConfig;
use memory_domain::types::*;
use memory_engine::app::ingest;
use memory_engine::retrieval;
use memory_engine::runtime::{engine::Engine, llm::Llm, models::ModelHub, tenant::Tenant};
use memory_rules::util;
use memory_store::schema;
use memory_store::store::{to_doc, Tx};
use serde_json::json;
use std::sync::Arc;

pub async fn selftest() {
    let dir = std::env::temp_dir().join(util::new_id("memory-integrity-"));
    let engine = Engine::new(
        ServiceConfig {
            memory_strategy: memory_config::MemoryStrategy::Memos,
            ..Default::default()
        },
        Llm::new("http://127.0.0.1:1", "", "mock", 1, 1).unwrap(),
        ModelHub::mock(),
        crate::algorithms::select(memory_config::MemoryStrategy::Memos),
    );
    let tenant = Arc::new(
        Tenant::open("integrity", &dir, 1024, engine.algorithm().schema())
            .await
            .unwrap(),
    );
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
        text: "Old profile".into(),
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
    tenant.store.commit(tx).await.unwrap();
    let first = tenant.store.active_memory_meta().await.unwrap();
    assert!(Arc::ptr_eq(
        &first,
        &tenant.store.active_memory_meta().await.unwrap()
    ));
    let mut rollback = Tx::new();
    rollback
        .merge(schema::MEMORIES, &fact.id, json!({"status":S_SUPERSEDED}))
        .unwrap();
    rollback.raw("THROW 'rollback'", vec![]);
    assert!(tenant.store.commit(rollback).await.is_err());
    assert_eq!(
        tenant
            .store
            .get_memory(&profile.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        S_CURRENT
    );
    let mut tx = Tx::new();
    tx.merge(schema::MEMORIES, &fact.id, json!({"status":S_SUPERSEDED}))
        .unwrap();
    tenant.store.commit(tx).await.unwrap();
    for id in [&profile.id, &skill.id] {
        assert_eq!(
            tenant.store.get_memory(id).await.unwrap().unwrap().status,
            S_STALE
        );
    }
    assert_eq!(
        tenant
            .store
            .get_memory(&historical.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        S_CURRENT
    );
    let after = tenant.store.active_memory_meta().await.unwrap();
    assert_eq!(after.len(), 1);
    assert!(!Arc::ptr_eq(&first, &after));
    // Legacy view referencing a missing source is repaired before startup retrieval.
    let missing = MemoryRec {
        id: "missingview".into(),
        used_memory_ids: vec!["removed".into()],
        ..profile.clone()
    };
    tenant
        .store
        .put_doc(schema::MEMORIES, &missing.id, &missing)
        .await
        .unwrap();
    tenant.store.repair_integrity().await.unwrap();
    assert_eq!(
        tenant
            .store
            .get_memory(&missing.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        S_STALE
    );

    let caller = Caller {
        tenant: tenant.name.clone(),
        platform: "agent".into(),
    };
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
    let receipt = ingest::add_memories(&engine, &tenant, &caller, request.clone())
        .await
        .unwrap();
    assert_eq!(receipt["status"], "completed");
    assert_eq!(
        ingest::add_memories(&engine, &tenant, &caller, request.clone())
            .await
            .unwrap()["duplicate"],
        true
    );
    let context = retrieval::get_context(
        &engine,
        &tenant,
        &caller,
        GetContextReq {
            scope: request.scope.clone(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(context["instructions"].as_array().unwrap().len(), 1);
    assert!(context["text"]
        .as_str()
        .unwrap()
        .contains("Prefer brief answers."));
    let other = retrieval::get_context(
        &engine,
        &tenant,
        &caller,
        GetContextReq {
            scope: ReqScope {
                project: Some("other".into()),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(other["instructions"].as_array().unwrap().is_empty());
    let invalid = AddMemoriesReq {
        messages: Some(vec![]),
        ..request
    };
    assert!(ingest::add_memories(&engine, &tenant, &caller, invalid)
        .await
        .is_err());
    assert_eq!(engine.llm.usage_snapshot()["calls"], 0);
    let port = MemoryRec {
        id: "port".into(),
        text: "Port is 8200".into(),
        ..fact.clone()
    };
    tenant
        .store
        .put_doc(schema::MEMORIES, &port.id, &port)
        .await
        .unwrap();
    let applied = memory_algo_memos::formation::relations::apply(
        &engine,
        &tenant,
        vec![memory_algo_memos::formation::relations::NewFact {
            fidx: 1,
            text: "Port is 8100".into(),
            scope: port.scope.clone(),
            entity_ids: vec![],
            effective_at: "2026-10-11".into(),
            origin: ORIGIN_USER.into(),
            sources: vec![memory_algo_memos::formation::relations::FactSource {
                turn_id: "T1".into(),
                span: "Port is 8100".into(),
                role: "user".into(),
                stated_at: "2026-10-11T00:00:00Z".into(),
                stated: (1, 0),
            }],
            relations: vec![memory_algo_memos::formation::relations::plan::Rel {
                target: memory_algo_memos::formation::relations::plan::Target::Old(port.id.clone()),
                relation: "conflicts".into(),
            }],
            reason: None,
        }],
        memory_algo_memos::formation::relations::WriteCtx {
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
    let uncertainties: Vec<UncertaintyRec> = tenant
        .store
        .select_as(
            "SELECT * OMIT id FROM uncertainties WHERE status = 'open'",
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(uncertainties.len(), 1);
    let chosen = &applied.created[0];
    let resolution = ResolveUncertaintyReq {
        id: uncertainties[0].id.clone(),
        keep_memory_id: chosen.clone(),
        reason: "Verified the configured port".into(),
    };
    memory_engine::app::management::resolve_uncertainty(&tenant, &caller, resolution.clone())
        .await
        .unwrap();
    assert_eq!(
        tenant
            .store
            .get_memory(chosen)
            .await
            .unwrap()
            .unwrap()
            .status,
        S_CURRENT
    );
    assert_eq!(
        tenant
            .store
            .get_memory(&port.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        S_SUPERSEDED
    );
    assert_eq!(
        tenant
            .store
            .get_doc(schema::UNCERTAINTIES, &resolution.id)
            .await
            .unwrap()
            .unwrap()["status"],
        "resolved"
    );
    assert!(
        memory_engine::app::management::resolve_uncertainty(&tenant, &caller, resolution)
            .await
            .is_err()
    );
    tenant.store.repair_integrity().await.unwrap();
    assert_eq!(
        tenant
            .store
            .count(schema::UNCERTAINTIES, Some("status = 'open'"))
            .await
            .unwrap(),
        0
    );
    shared_conflict_regression(&tenant, &caller, &fact).await;
    drop(tenant);
    // The embedded connection releases its datastore asynchronously after its final drop.
    let reopened = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match Tenant::open("integrity", &dir, 1024, engine.algorithm().schema()).await {
                Ok(tenant) => break tenant,
                Err(error)
                    if format!("{error:#}").contains("is already locked by another process") =>
                {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
                Err(error) => panic!("reopen integrity store: {error:#}"),
            }
        }
    })
    .await
    .expect("integrity store lock did not release after drop");
    reopened.store.repair_integrity().await.unwrap();
    for id in ["chain_c", "continued_c", "chosen_a"] {
        assert_eq!(
            reopened.store.get_memory(id).await.unwrap().unwrap().status,
            S_CURRENT
        );
    }
    assert_eq!(
        reopened
            .store
            .count(schema::UNCERTAINTIES, Some("status = 'open'"))
            .await
            .unwrap(),
        0
    );
    drop(reopened);
    std::fs::remove_dir_all(dir).unwrap();
    println!("integrity_selftest OK: atomic/transitive invalidation, rollback, restart repair, historical lineage, snapshot invalidation, scoped/idempotent explicit instructions, conflict materialization and explicit resolution; zero LLM calls");
}

async fn shared_conflict_regression(t: &Arc<Tenant>, caller: &Caller, template: &MemoryRec) {
    // The final edge either terminates at C or continues to D. A separate case keeps A disputed.
    for (name, pairs) in [
        ("chain", vec![("a", "b"), ("b", "c")]),
        ("continued", vec![("a", "b"), ("b", "c"), ("c", "d")]),
        ("chosen", vec![("a", "b"), ("a", "c")]),
    ] {
        let ids = |suffix: &str| format!("{name}_{suffix}");
        let mut tx = Tx::new();
        for suffix in ["a", "b", "c", "d"] {
            let related = pairs
                .iter()
                .filter_map(|(a, b)| {
                    let other = if *a == suffix {
                        b
                    } else if *b == suffix {
                        a
                    } else {
                        return None;
                    };
                    Some(RelEdge {
                        id: ids(other),
                        relation: "conflicts".into(),
                    })
                })
                .collect::<Vec<_>>();
            if related.is_empty() {
                continue;
            }
            let record = MemoryRec {
                id: ids(suffix),
                status: S_CONFLICT.into(),
                related,
                ..template.clone()
            };
            tx.put(schema::MEMORIES, &record.id, to_doc(&record).unwrap())
                .unwrap();
        }
        t.store.commit(tx).await.unwrap();
        t.store.repair_integrity().await.unwrap();
        let open = || async {
            t.store
                .select_as::<UncertaintyRec>(
                    "SELECT * OMIT id FROM uncertainties WHERE status = 'open'",
                    vec![],
                )
                .await
                .unwrap()
        };
        let ab = open()
            .await
            .into_iter()
            .find(|u| u.memory_ids.contains(&ids("a")) && u.memory_ids.contains(&ids("b")))
            .unwrap();
        if name == "chain" {
            let mut rollback = Tx::new();
            rollback
                .merge(schema::MEMORIES, &ids("b"), json!({"status":S_SUPERSEDED}))
                .unwrap();
            rollback.raw("THROW 'rollback shared conflict'", vec![]);
            assert!(t.store.commit(rollback).await.is_err());
            assert_eq!(
                t.store.get_memory(&ids("c")).await.unwrap().unwrap().status,
                S_CONFLICT
            );
            assert_eq!(open().await.len(), 2);
        }
        memory_engine::app::management::resolve_uncertainty(
            t,
            caller,
            ResolveUncertaintyReq {
                id: ab.id,
                keep_memory_id: ids("a"),
                reason: "Confirmed A".into(),
            },
        )
        .await
        .unwrap();
        let a = t.store.get_memory(&ids("a")).await.unwrap().unwrap();
        let c = t.store.get_memory(&ids("c")).await.unwrap().unwrap();
        assert_eq!(
            a.status,
            if name == "chosen" {
                S_CONFLICT
            } else {
                S_CURRENT
            }
        );
        assert_eq!(
            c.status,
            if name == "chain" {
                S_CURRENT
            } else {
                S_CONFLICT
            }
        );
        assert!(!a.related.iter().any(|edge| edge.id == ids("b")));
        assert!(!c.related.iter().any(|edge| edge.id == ids("b")));
        assert_eq!(
            t.store.get_memory(&ids("b")).await.unwrap().unwrap().text,
            template.text
        );
        let remaining = open()
            .await
            .into_iter()
            .filter(|u| u.memory_ids.iter().any(|id| id.starts_with(name)))
            .collect::<Vec<_>>();
        assert_eq!(remaining.len(), usize::from(name != "chain"));
        if let Some(question) = remaining.first() {
            memory_engine::app::management::resolve_uncertainty(
                t,
                caller,
                ResolveUncertaintyReq {
                    id: question.id.clone(),
                    keep_memory_id: if name == "chosen" { ids("a") } else { ids("c") },
                    reason: "Confirmed remaining assertion".into(),
                },
            )
            .await
            .unwrap();
        }
        t.store.repair_integrity().await.unwrap();
        assert!(!open()
            .await
            .iter()
            .any(|u| u.memory_ids.iter().any(|id| id.starts_with(name))));
    }
}
