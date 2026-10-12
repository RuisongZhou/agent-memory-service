//! Formation self-tests run by `agent-memoryd selftest` (feature `selftest`).
use super::experience;
use super::extract::{
    commit_experience, commit_facts, fact_source_contract, facts_from_flat_object,
    facts_from_values, recall, session_experience, validate, validate_fact_source_contract,
    RawRelation,
};
use super::relations::{self, WriteCtx};
use crate::runtime::{engine::Engine, tenant::Tenant};
use memory_domain::types::*;
use memory_rules::util;
use memory_store::schema;
use serde_json::json;
use std::collections::HashSet;
use std::sync::Arc;

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
    use crate::app::ingest;
    use crate::runtime::{config::ServiceConfig, indexer, llm::Llm, models::ModelHub};
    use memory_store::store::{self, to_doc, Tx};
    assert_eq!(std::env::var("MEMORY_MOCK_MODELS").as_deref(), Ok("1"));
    experience::rule_selftest();
    super::ingest::trigger_selftest();
    let dir = std::env::temp_dir().join(format!("memory-formation-selftest-{}", util::new_id("t")));
    let eng = Engine::new(
        ServiceConfig {
            memory_strategy: crate::runtime::config::MemoryStrategy::Memos,
            data_dir: dir.clone(),
            ..Default::default()
        },
        Llm::new("http://127.0.0.1:1", "", "disabled", 1, 1).unwrap(),
        ModelHub::mock(),
        crate::algorithms::select(crate::runtime::config::MemoryStrategy::Memos),
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
    tx.put(schema::MEMORIES, &old.id, to_doc(&old).unwrap())
        .unwrap();
    let entity = EntityRec {
        id: "entsurreal".into(),
        name: "SurrealDB".into(),
        scope: MemScope::project("p1"),
        ..Default::default()
    };
    tx.put(schema::ENTITIES, &entity.id, to_doc(&entity).unwrap())
        .unwrap();
    let hidden = MemoryRec {
        id: "mhidden".into(),
        text: "SurrealDB hidden private port".into(),
        scope: MemScope::project("p2"),
        ..old.clone()
    };
    tx.put(schema::MEMORIES, &hidden.id, to_doc(&hidden).unwrap())
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
    let evidence_count = t.store.count(schema::EVIDENCE, None).await.unwrap();
    assert_eq!(evidence_count, 3);
    let mut tx = Tx::new();
    tx.delete(schema::TURNS, &store::turn_rid("codex", "T1"))
        .unwrap();
    t.store.commit(tx).await.unwrap();
    let after_purge = ingest::add_memories(&eng, &t, &c, req).await.unwrap();
    assert_eq!(after_purge["requestId"], id);
    assert_eq!(after_purge["duplicate"], true);
    assert_eq!(
        t.store.count(schema::EVIDENCE, None).await.unwrap(),
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
    use crate::app::ingest;
    use crate::runtime::indexer;
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
            .count(schema::MEMORIES, Some("type = 'experience'"))
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
    assert_eq!(t.store.count(schema::MEMORIES, None).await.unwrap(), 1);
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
            .count(schema::MEMORIES, Some("type = 'experience'"))
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
            schema::SESSIONS,
            &memory_store::store::session_rid("test", "S"),
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
    let evidence = t.store.count(schema::EVIDENCE, None).await.unwrap();
    assert_eq!(
        consolidate::purge(&t, &util::now_str(), 3.0).await.unwrap()["purgedTurns"],
        6
    );
    assert_eq!(
        t.store.count(schema::EVIDENCE, None).await.unwrap(),
        evidence
    );
    let duplicate = ingest::add_memories(eng, &t, &c, payloads[0].clone())
        .await
        .unwrap();
    assert_eq!(duplicate["duplicate"], true);
    assert_eq!(t.store.count(schema::MEMORIES, None).await.unwrap(), 2);
    assert_eq!(eng.llm.usage_snapshot()["calls"], 0);
}
