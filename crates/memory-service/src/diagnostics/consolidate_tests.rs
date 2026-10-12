//! Maintenance integration selftest. Supplied records and mock embeddings; never invokes a real LLM.
use super::{
    config::ServiceConfig,
    consolidate,
    engine::Engine,
    indexer,
    llm::Llm,
    models::ModelHub,
    store::{self, to_doc, Tx},
    tenant::Tenant,
    types::*,
    util,
};
use serde_json::json;
use std::sync::Arc;

pub async fn consolidation_selftest() {
    assert!(!consolidate::profile_length_exceeded(
        &"中".repeat(500),
        "zh",
        500
    ));
    assert!(consolidate::profile_length_exceeded(
        &"中".repeat(501),
        "zh",
        500
    ));
    assert!(!consolidate::profile_length_exceeded(
        &vec!["word"; 350].join(" "),
        "en",
        500
    ));
    assert!(consolidate::profile_length_exceeded(
        &vec!["word"; 351].join(" "),
        "en",
        500
    ));
    assert_eq!(std::env::var("MEMORY_MOCK_MODELS").as_deref(), Ok("1"));
    let dir = std::env::temp_dir().join(format!(
        "memory-consolidation-selftest-{}",
        util::new_id("t")
    ));
    let eng = Engine::new(
        ServiceConfig {
            memory_strategy: super::config::MemoryStrategy::Current,
            data_dir: dir.clone(),
            ..Default::default()
        },
        Llm::new("http://127.0.0.1:1", "", "disabled", 1, 1).unwrap(),
        ModelHub::mock(),
    );
    let t = Arc::new(
        Tenant::open("consolidation-zh", &dir, eng.models.dim)
            .await
            .unwrap(),
    );
    let c = Caller {
        tenant: t.name.clone(),
        platform: "codex".into(),
    };
    let as_of = "2026-10-10T00:00:00Z";
    let a = MemoryRec {
        id: "mexacta".into(),
        mtype: T_FACT.into(),
        text: "user prefers plain documents".into(),
        scope: MemScope::global(),
        status: S_CURRENT.into(),
        effective_at: "2026-10-01".into(),
        recorded_at: as_of.into(),
        stated_at: "2026-10-01T00:00:00Z".into(),
        last_accessed_at: as_of.into(),
        evidence_count: 1,
        entity_ids: vec!["entdocument".into()],
        source: Source {
            platform: "codex".into(),
            origin: ORIGIN_EXPLICIT.into(),
            turn_ids: vec!["T1".into()],
        },
        ..Default::default()
    };
    let b = MemoryRec {
        id: "mexactb".into(),
        text: "user  prefers\nplain documents".into(),
        entity_ids: vec!["entuser".into()],
        source: Source {
            platform: "codex".into(),
            origin: ORIGIN_USER.into(),
            turn_ids: vec!["T2".into()],
        },
        ..a.clone()
    };
    let mut tx = Tx::new();
    for m in [&a, &b] {
        tx.put("memories", &m.id, to_doc(m).unwrap()).unwrap();
        tx.index_task("memories", &m.id, "upsert").unwrap();
    }
    for (id, mid, tid) in [("eexacta", "mexacta", "T1"), ("eexactb", "mexactb", "T2")] {
        let e = EvidenceRec {
            context: None,
            id: id.into(),
            memory_ids: vec![mid.into()],
            platform: "codex".into(),
            turn_id: tid.into(),
            role: "user".into(),
            span: "original evidence must survive maintenance".into(),
            stated_at: as_of.into(),
            created_at: as_of.into(),
        };
        tx.put("evidence", &e.id, to_doc(&e).unwrap()).unwrap();
    }
    t.store.commit(tx).await.unwrap();
    indexer::drain(&eng, &t).await.unwrap();
    let merged = consolidate::run_jobs(&eng, &t, &c, json!({"jobs":["merge"],"asOf":as_of}))
        .await
        .unwrap();
    assert_eq!(merged["jobs"]["merge"]["autoMerged"], 1);
    assert_eq!(merged["jobs"]["merge"]["proposals"], 0);
    let kept = t
        .store
        .find_memories("type = 'fact' AND status = 'current'", vec![])
        .await
        .unwrap();
    assert_eq!(kept.len(), 1);
    let canonical = &kept[0];
    assert_eq!(canonical.evidence_count, 2);
    assert_eq!(canonical.source.turn_ids.len(), 2);
    assert_eq!(canonical.entity_ids.len(), 2);
    assert_eq!(
        t.store.get_memory("mexacta").await.unwrap().unwrap().text,
        a.text
    );
    assert_eq!(
        t.store.get_memory("mexactb").await.unwrap().unwrap().text,
        b.text
    );
    for id in ["eexacta", "eexactb"] {
        let e = t.store.get_doc("evidence", id).await.unwrap().unwrap();
        assert!(e["memoryIds"]
            .as_array()
            .unwrap()
            .contains(&json!(canonical.id)));
    }
    let merged_again = consolidate::run_jobs(&eng, &t, &c, json!({"jobs":["merge"],"asOf":as_of}))
        .await
        .unwrap();
    assert_eq!(merged_again["jobs"]["merge"]["autoMerged"], 0);
    assert_eq!(
        t.store
            .get_memory(&canonical.id)
            .await
            .unwrap()
            .unwrap()
            .evidence_count,
        2
    );
    assert_eq!(t.store.count("evidence", None).await.unwrap(), 2);

    // Experience text omits approach/result; equal text cannot erase a successful vs failed attempt.
    // Keep these records unindexed so this plumbing test never triggers P4's generative stage.
    let success = MemoryRec {
        id: "mexpsuccess".into(),
        mtype: T_EXPERIENCE.into(),
        text: "中文关键词漏召回：先查分词模式".into(),
        scope: MemScope::project("p1"),
        source: Source {
            platform: "codex".into(),
            origin: ORIGIN_INFERRED.into(),
            turn_ids: vec!["T3".into()],
        },
        detail: Some(
            json!({"situation":"中文关键词漏召回","approach":"统一分词模式","result":"success：测试通过","lesson":"先查分词模式"}),
        ),
        ..a.clone()
    };
    let failure = MemoryRec {
        id: "mexpfailure".into(),
        detail: Some(
            json!({"situation":"中文关键词漏召回","approach":"调高向量检索参数","result":"failure：没有帮助","lesson":"先查分词模式"}),
        ),
        ..success.clone()
    };
    let mut tx = Tx::new();
    for m in [&success, &failure] {
        tx.put("memories", &m.id, to_doc(m).unwrap()).unwrap();
    }
    t.store.commit(tx).await.unwrap();
    let distinct = consolidate::run_jobs(&eng, &t, &c, json!({"jobs":["merge"],"asOf":as_of}))
        .await
        .unwrap();
    assert_eq!(distinct["jobs"]["merge"]["autoMerged"], 0);
    for m in [&success, &failure] {
        let stored = t.store.get_memory(&m.id).await.unwrap().unwrap();
        assert_eq!(stored.status, S_CURRENT);
        assert_eq!(stored.detail, m.detail);
    }

    let fixtures = [
        ("Tready", "Sready", "done", true, 1, "2026-10-01T00:00:00Z"),
        ("Twait", "Swait", "done", true, 0, "2026-10-01T00:00:00Z"),
        (
            "Tpending",
            "Spending",
            "pending",
            false,
            0,
            "2026-10-01T00:00:00Z",
        ),
        (
            "Trecent",
            "Srecent",
            "done",
            true,
            1,
            "2026-10-09T00:00:00Z",
        ),
        // Neither state alone nor a P2 watermark proves successful P1 without extractedAt.
        (
            "Tmissingwatermark",
            "Smissingwatermark",
            "done",
            false,
            1,
            "2026-10-01T00:00:00Z",
        ),
    ];
    let mut tx = Tx::new();
    for (tid, sid, state, extracted, p2, end) in fixtures {
        let tr = TurnRec {
            key: turn_key("codex", tid),
            turn_id: tid.into(),
            platform: "codex".into(),
            project: "p1".into(),
            session: sid.into(),
            user_message: format!("buffer-original-{tid}"),
            assistant_message: "final reply".into(),
            user_ts: end.into(),
            assistant_ts: end.into(),
            started_at: end.into(),
            ended_at: end.into(),
            recorded_at: end.into(),
            digest: format!("buffer-original-{tid}"),
            seq: 1,
            extract_state: state.into(),
            extracted_at: extracted.then(|| "2026-10-02T00:00:00Z".into()),
            ..Default::default()
        };
        let s = SessionRec {
            key: session_key("codex", sid),
            platform: "codex".into(),
            session: sid.into(),
            project: "p1".into(),
            goal: "test maintenance".into(),
            turn_count: 1,
            last_experience_seq: p2,
            updated_at: end.into(),
            last_turn_at: end.into(),
            ended_at: Some(end.into()),
            ..Default::default()
        };
        let rid = store::turn_rid("codex", tid);
        tx.put("turns", &rid, to_doc(&tr).unwrap()).unwrap();
        tx.index_task("turns", &rid, "upsert").unwrap();
        tx.put(
            "sessions",
            &store::session_rid("codex", sid),
            to_doc(&s).unwrap(),
        )
        .unwrap();
    }
    let evidence = EvidenceRec {
        context: None,
        id: "ebuffer".into(),
        memory_ids: vec![canonical.id.clone()],
        platform: "codex".into(),
        turn_id: "Tready".into(),
        role: "user".into(),
        span: "copied supporting span".into(),
        stated_at: "2026-10-01T00:00:00Z".into(),
        created_at: as_of.into(),
    };
    tx.put("evidence", &evidence.id, to_doc(&evidence).unwrap())
        .unwrap();
    t.store.commit(tx).await.unwrap();
    indexer::drain(&eng, &t).await.unwrap();
    let purge = consolidate::run_jobs(
        &eng,
        &t,
        &c,
        json!({"jobs":["purge"],"asOf":as_of,"retentionDays":3}),
    )
    .await
    .unwrap();
    assert_eq!(purge["jobs"]["purge"]["purgedTurns"], 1);
    assert_eq!(purge["jobs"]["purge"]["purgedSessions"], 1);
    assert!(t.store.get_turn("codex", "Tready").await.unwrap().is_none());
    assert!(t
        .store
        .get_session("codex", "Sready")
        .await
        .unwrap()
        .is_none());
    for id in ["Twait", "Tpending", "Trecent", "Tmissingwatermark"] {
        assert!(t.store.get_turn("codex", id).await.unwrap().is_some());
    }
    assert!(t
        .store
        .get_doc("evidence", "ebuffer")
        .await
        .unwrap()
        .is_some());
    assert!(!t
        .vecs
        .read()
        .await
        .turns
        .contains_key(&store::turn_rid("codex", "Tready")));
    let documents =
        t.ft.search(
            "buffer-original-Tready",
            &memory_fulltext::FtFilter {
                kinds: vec!["turn".into()],
                ..Default::default()
            },
            50,
        )
        .unwrap();
    assert!(!documents
        .iter()
        .any(|(id, _)| id == &store::turn_rid("codex", "Tready")));
    let again = consolidate::run_jobs(
        &eng,
        &t,
        &c,
        json!({"jobs":["purge"],"asOf":as_of,"retentionDays":3}),
    )
    .await
    .unwrap();
    assert_eq!(again["jobs"]["purge"]["purgedTurns"], 0);
    assert_eq!(t.store.count("evidence", None).await.unwrap(), 3);
    let force = consolidate::run_jobs(
        &eng,
        &t,
        &c,
        json!({"jobs":["purge"],"asOf":as_of,"retentionDays":0}),
    )
    .await
    .unwrap();
    assert_eq!(force["jobs"]["purge"]["purgedTurns"], 1);
    assert!(t
        .store
        .get_turn("codex", "Trecent")
        .await
        .unwrap()
        .is_none());
    for id in ["Twait", "Tpending", "Tmissingwatermark"] {
        assert!(t.store.get_turn("codex", id).await.unwrap().is_some());
    }
    assert_eq!(t.store.count("evidence", None).await.unwrap(), 3);
    assert_eq!(eng.llm.usage_snapshot()["calls"], 0);
    assert!(matches!(
        consolidate::run_jobs(&eng, &t, &c, json!({"jobs":["unknown"]})).await,
        Err(ApiError::InvalidArgument(_))
    ));
    assert!(matches!(
        consolidate::run_jobs(&eng, &t, &c, json!({"jobs":["purge"],"retentionDays":-1})).await,
        Err(ApiError::InvalidArgument(_))
    ));
    println!("consolidation_selftest OK: exact-merge evidence/body preservation and idempotency; P1/P2/age purge gates; evidence retained; index removal; zero LLM calls; data {}",dir.display());
}
