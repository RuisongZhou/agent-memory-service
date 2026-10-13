//! Maintenance integration selftest. Supplied records and mock embeddings; never invokes a real LLM.
use super::{engine, memory, tenant};
use memory_algo_memos::formation::consolidate;
use memory_config::{MemoryStrategy, ServiceConfig};
use memory_domain::types::*;
use memory_engine::runtime::indexer;
use memory_store::schema;
use memory_store::store::{self, to_doc, Tx};
use serde_json::json;

pub async fn consolidation_selftest() {
    let eng = engine(ServiceConfig {
        memory_strategy: MemoryStrategy::Memos,
        ..Default::default()
    });
    let (t, c, dir) = tenant(&eng, "consolidation", "codex").await;
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
        tx.put(schema::MEMORIES, &m.id, to_doc(m).unwrap()).unwrap();
        tx.index_task(schema::MEMORIES, &m.id, "upsert").unwrap();
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
        tx.put(schema::EVIDENCE, &e.id, to_doc(&e).unwrap())
            .unwrap();
    }
    t.store.commit(tx).await.unwrap();
    indexer::drain(&eng, &t).await.unwrap();
    // Exact duplicates merge into one canonical memory that keeps both sources; originals keep their text.
    let merge = json!({"jobs":["merge"],"asOf":as_of});
    let merged = consolidate::run_jobs(&eng, &t, &c, merge.clone())
        .await
        .unwrap();
    assert_eq!(merged["jobs"]["merge"]["autoMerged"], 1);
    let kept = t
        .store
        .find_memories("type = 'fact' AND status = 'current'", vec![])
        .await
        .unwrap();
    assert_eq!(kept.len(), 1);
    let canonical = &kept[0];
    assert_eq!(canonical.evidence_count, 2);
    assert_eq!(canonical.entity_ids.len(), 2);
    assert_eq!(memory(&t, "mexactb").await.text, b.text);
    let e = t
        .store
        .get_doc(schema::EVIDENCE, "eexactb")
        .await
        .unwrap()
        .unwrap();
    assert!(e["memoryIds"]
        .as_array()
        .unwrap()
        .contains(&json!(canonical.id)));
    let again = consolidate::run_jobs(&eng, &t, &c, merge.clone())
        .await
        .unwrap();
    assert_eq!(again["jobs"]["merge"]["autoMerged"], 0);

    // Equal experience text cannot merge a successful attempt with a failed one.
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
        tx.put(schema::MEMORIES, &m.id, to_doc(m).unwrap()).unwrap();
    }
    t.store.commit(tx).await.unwrap();
    let distinct = consolidate::run_jobs(&eng, &t, &c, merge).await.unwrap();
    assert_eq!(distinct["jobs"]["merge"]["autoMerged"], 0);

    // Purge removes only old turns whose facts and session experience are formed; evidence stays.
    let fixtures = [
        ("Tready", "done", 1, "2026-10-01T00:00:00Z"),
        ("Twait", "done", 0, "2026-10-01T00:00:00Z"),
        ("Tpending", "pending", 0, "2026-10-01T00:00:00Z"),
        ("Trecent", "done", 1, "2026-10-09T00:00:00Z"),
    ];
    let mut tx = Tx::new();
    for (tid, state, p2, end) in fixtures {
        let sid = format!("S{tid}");
        let tr = TurnRec {
            key: turn_key("codex", tid),
            turn_id: tid.into(),
            platform: "codex".into(),
            project: "p1".into(),
            session: sid.clone(),
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
            extracted_at: (state == "done").then(|| "2026-10-02T00:00:00Z".into()),
            ..Default::default()
        };
        let s = SessionRec {
            key: session_key("codex", &sid),
            platform: "codex".into(),
            session: sid.clone(),
            project: "p1".into(),
            turn_count: 1,
            last_experience_seq: p2,
            updated_at: end.into(),
            last_turn_at: end.into(),
            ended_at: Some(end.into()),
            ..Default::default()
        };
        let rid = store::turn_rid("codex", tid);
        tx.put(schema::TURNS, &rid, to_doc(&tr).unwrap()).unwrap();
        tx.index_task(schema::TURNS, &rid, "upsert").unwrap();
        tx.put(
            schema::SESSIONS,
            &store::session_rid("codex", &sid),
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
    tx.put(schema::EVIDENCE, &evidence.id, to_doc(&evidence).unwrap())
        .unwrap();
    t.store.commit(tx).await.unwrap();
    indexer::drain(&eng, &t).await.unwrap();
    let evidence = t.store.count(schema::EVIDENCE, None).await.unwrap();
    let purge = consolidate::run_jobs(
        &eng,
        &t,
        &c,
        json!({"jobs":["purge"],"asOf":as_of,"retentionDays":3}),
    )
    .await
    .unwrap();
    assert_eq!(purge["jobs"]["purge"]["purgedTurns"], 1);
    assert!(t.store.get_turn("codex", "Tready").await.unwrap().is_none());
    for id in ["Twait", "Tpending", "Trecent"] {
        assert!(t.store.get_turn("codex", id).await.unwrap().is_some());
    }
    assert!(!t
        .vecs
        .read()
        .await
        .turns
        .contains_key(&store::turn_rid("codex", "Tready")));
    assert_eq!(
        t.store.count(schema::EVIDENCE, None).await.unwrap(),
        evidence
    );
    assert_eq!(eng.llm.usage_snapshot()["calls"], 0);
    println!("consolidation_selftest OK: exact merge keeps evidence and bodies, idempotent; experiences with different outcomes stay apart; purge gates; evidence retained; zero LLM calls; data {}", dir.display());
}
