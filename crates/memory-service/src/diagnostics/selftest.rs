//! Storage transaction round trip used by `agent-memoryd selftest`.
use memory_domain::types::*;
use memory_rules::util;
use memory_store::schema;
use memory_store::store::*;
use serde_json::json;

pub async fn store_roundtrip() {
    let dir = std::env::temp_dir().join(util::new_id("memory-store-"));
    let s = Store::open(&dir.join("db"), 4, "").await.unwrap();
    let m = MemoryRec {
        id: "mabc123".into(),
        mtype: T_FACT.into(),
        text: "共享记忆库测试环境端口是 8100".into(),
        scope: MemScope::project("p1"),
        status: S_CURRENT.into(),
        effective_at: "2026-10-07".into(),
        recorded_at: "2026-10-08T10:00:00Z".into(),
        source: Source {
            platform: "codex".into(),
            turn_ids: vec!["T1".into()],
            origin: "user".into(),
        },
        evidence_count: 1,
        ..Default::default()
    };
    let mut tx = Tx::new();
    tx.put(schema::MEMORIES, &m.id, to_doc(&m).unwrap())
        .unwrap();
    tx.merge(schema::MEMORIES, &m.id, json!({"evidenceCount": 2}))
        .unwrap();
    s.commit(tx).await.unwrap();
    let got = s.get_memory(&m.id).await.unwrap().unwrap();
    assert_eq!(got.text, m.text);
    assert_eq!(got.evidence_count, 2);
    s.set_embedding(schema::MEMORIES, &m.id, Some(&[0.1, 0.2, 0.3, 0.4]), "x")
        .await
        .unwrap();
    assert_eq!(
        s.load_embeddings(schema::MEMORIES).await.unwrap()[0].0,
        m.id
    );
    // A failed transaction rolls back every write in it.
    let mut tx = Tx::new();
    tx.merge(schema::MEMORIES, &m.id, json!({"status": S_SUPERSEDED}))
        .unwrap();
    tx.raw("THROW 'boom'", vec![]);
    assert!(s.commit(tx).await.is_err());
    assert_eq!(
        s.get_memory(&m.id).await.unwrap().unwrap().status,
        S_CURRENT
    );
    println!("store_roundtrip OK");
}
