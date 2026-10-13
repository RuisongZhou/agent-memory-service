//! Formation self-tests run by `agent-memoryd selftest` (feature `selftest`).
use super::extract::{commit_experience, commit_facts, recall, validate};
use super::relations::{self, WriteCtx};
use memory_config::ServiceConfig;
use memory_domain::types::*;
use memory_engine::app::ingest;
use memory_engine::runtime::{engine::Engine, indexer, llm::Llm, models::ModelHub, tenant::Tenant};
use memory_rules::util;
use memory_store::schema;
use memory_store::store::{self, to_doc, Tx};
use serde_json::json;
use std::collections::HashSet;
use std::sync::Arc;

/// Deterministic formation/storage integration test. Uses supplied JSON output and mock embeddings;
/// the LLM client has no key and this test makes zero generative-model requests.
pub async fn formation_selftest() {
    let dir = std::env::temp_dir().join(format!("memory-formation-selftest-{}", util::new_id("t")));
    let eng = Engine::new(
        ServiceConfig {
            memory_strategy: memory_config::MemoryStrategy::Memos,
            data_dir: dir.clone(),
            ..Default::default()
        },
        Llm::new("http://127.0.0.1:1", "", "disabled", 1, 1).unwrap(),
        ModelHub::mock(),
        Arc::new(crate::Memos),
    );
    let t = Arc::new(
        Tenant::open(
            "formation-zh",
            &dir,
            eng.models.dim,
            eng.algorithm().schema(),
        )
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
    assert_eq!(
        ingest::add_memories(&eng, &t, &c, req).await.unwrap()["duplicate"],
        true
    );
    let old = MemoryRec {
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
    let entity = EntityRec {
        id: "entsurreal".into(),
        name: "SurrealDB".into(),
        scope: MemScope::project("p1"),
        ..Default::default()
    };
    let hidden = MemoryRec {
        id: "mhidden".into(),
        text: "SurrealDB hidden private port".into(),
        scope: MemScope::project("p2"),
        ..old.clone()
    };
    let mut tx = Tx::new();
    tx.put(schema::MEMORIES, &old.id, to_doc(&old).unwrap())
        .unwrap();
    tx.put(schema::ENTITIES, &entity.id, to_doc(&entity).unwrap())
        .unwrap();
    tx.put(schema::MEMORIES, &hidden.id, to_doc(&hidden).unwrap())
        .unwrap();
    t.store.commit(tx).await.unwrap();
    let turns = t.store.session_turns("codex", "S1").await.unwrap();
    // A rejected row keeps its position, so references to it cannot retarget another fact.
    let broken_reference = json!({"facts":[
        {"text":"missing fields"},
        {"text":"SurrealDB端口改成8100了","scope":"project","entities":[],
         "effectiveAt":"2026-10-08","origin":"user","source":{"turnId":"T1","span":"端口改成8100了"},
         "relations":[{"memoryId":"F1","relation":"updates"}]},
        {"text":"配置已同步","scope":"project","entities":[],"effectiveAt":"2026-10-08",
         "origin":"user","source":{"turnId":"T1","span":"配置已同步"},
         "relations":[{"memoryId":"F1","relation":"related"}]}
    ]});
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
    // Invalid origins, unverifiable spans and impossible dates are dropped; duplicates merge sources.
    let output = json!({"facts":[
        {"text":"SurrealDB端口为8100","scope":"project","entities":["SurrealDB"],"effectiveAt":"2026-10-07","origin":"user",
            "source":{"turnId":"T1","span":"端口改成8100了"},"relations":[{"memoryId":"M1","relation":"updates"}]},
        {"text":"用户要求文档用简体中文","scope":"global","entities":[],"effectiveAt":"2026-10-08","origin":"explicit",
            "source":{"turnId":"T1","span":"以后文档用简体中文"},"relations":[]},
        {"text":"伪造事实","scope":"project","entities":[],"effectiveAt":"2026-10-08","origin":"assistant",
            "source":{"turnId":"T1","span":"以后文档用简体中文"},"relations":[]},
        {"text":"SurrealDB端口为8100","scope":"project","entities":["SurrealDB"],"effectiveAt":"2026-10-07","origin":"user",
            "source":{"turnId":"T1","span":"SurrealDB端口改成8100了"},"relations":[{"memoryId":"M1","relation":"updates"}]},
        {"text":"非法日期","scope":"project","entities":[],"effectiveAt":"2026-02-30","origin":"user",
            "source":{"turnId":"T1","span":"配置已同步"},"relations":[]}
    ]});
    let (facts, new_entities) = validate(&t, output, &[old], &turns, None, "p1", "codex", false)
        .await
        .unwrap();
    assert_eq!(facts.len(), 2);
    assert_eq!(facts[0].sources.len(), 2);
    let applied = relations::apply(
        &eng,
        &t,
        facts,
        WriteCtx {
            platform: "codex".into(),
            now: now.clone(),
            turns: turns
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
    let old = t.store.get_memory("moldport").await.unwrap().unwrap();
    assert_eq!(old.status, S_SUPERSEDED);
    assert_eq!(old.valid_until.as_deref(), Some("2026-10-07"));
    let new = t
        .store
        .get_memory(old.superseded_by.as_deref().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(new.evidence_count, 2);
    assert_eq!(new.entity_ids, vec!["entsurreal"]);
    let tr = t.store.get_turn("codex", "T1").await.unwrap().unwrap();
    assert_eq!(tr.extract_state, "done");
    assert_eq!(tr.derived_memory_ids.len(), 2);
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
    assert_eq!(queued[0]["status"], "queued");
    assert_eq!(queued[0]["failures"], 1);
    session_split_selftest(&eng, &dir).await;
    assert_eq!(eng.llm.usage_snapshot()["calls"], 0);
    println!("formation_selftest OK: provenance/date/reference checks; update chain; durable failure; facts while open, one experience at session end; source retention; zero LLM calls; data {}", dir.display());
}

/// Facts form while a session is open; its experience forms once at session end, and purging the
/// sources keeps evidence and idempotency receipts.
async fn session_split_selftest(eng: &Arc<Engine>, dir: &std::path::PathBuf) {
    use super::consolidate;
    let t = Arc::new(
        Tenant::open(
            "split-selftest-en",
            dir,
            eng.models.dim,
            eng.algorithm().schema(),
        )
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
    let turns = t.store.session_turns("test", "S").await.unwrap();
    let output = json!({"facts":[{
        "text":"Deployment step 1","scope":"project","entities":["service"],
        "effectiveAt":util::date_of(&turns[0].user_ts),"origin":"user",
        "source":{"turnId":"U1","span":"Deployment step 1"},"relations":[]
    }]});
    commit_facts(eng, &t, turns[..3].to_vec(), vec![], output)
        .await
        .unwrap();
    commit_facts(eng, &t, turns[3..].to_vec(), vec![], json!({"facts":[]}))
        .await
        .unwrap();
    assert_eq!(ingest::wait_idle(eng, &t).await.unwrap()["status"], "idle");
    let experiences = "type = 'experience'";
    assert_eq!(
        t.store
            .count(schema::MEMORIES, Some(experiences))
            .await
            .unwrap(),
        0
    );
    let end = AddMemoriesReq {
        scope,
        session_ended: Some(true),
        ..Default::default()
    };
    let receipt = ingest::add_memories(eng, &t, &c, end.clone())
        .await
        .unwrap();
    let snapshot = t.store.get_session("test", "S").await.unwrap().unwrap();
    let ids = turns
        .iter()
        .map(|tr| tr.turn_id.clone())
        .collect::<HashSet<_>>();
    let output = json!({"analysis":[{"task":"deployment","result":"success","turnIds":["U1","U2","U3","U4"]}],
        "experiences":[{"situation":"deployment validation","approach":"apply fix and test","result":"success: Tests passed.","lesson":"verify after changing","scope":"project","turnIds":["U1","U2","U3","U4"],"relations":[]}],
        "summary":{"goal":"deploy","outcome":"verified","decisions":["test changes"],"openIssues":[]}});
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
    // Replaying the end cannot create a duplicate experience.
    ingest::add_memories(eng, &t, &c, end).await.unwrap();
    assert_eq!(ingest::wait_idle(eng, &t).await.unwrap()["status"], "idle");
    assert_eq!(
        t.store
            .count(schema::MEMORIES, Some(experiences))
            .await
            .unwrap(),
        1
    );
    let evidence = t.store.count(schema::EVIDENCE, None).await.unwrap();
    assert_eq!(
        consolidate::purge(&t, &util::now_str(), 3.0).await.unwrap()["purgedTurns"],
        4
    );
    assert_eq!(
        t.store.count(schema::EVIDENCE, None).await.unwrap(),
        evidence
    );
    let duplicate = ingest::add_memories(eng, &t, &c, payloads[0].clone())
        .await
        .unwrap();
    assert_eq!(duplicate["duplicate"], true);
}
