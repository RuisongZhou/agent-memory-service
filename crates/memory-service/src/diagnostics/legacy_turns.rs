//! A store the retired memos strategy wrote: its turns stay readable and lose the ids of deleted memories,
//! but are no longer vector-indexed. Mock embeddings, no generation calls.
use super::{engine, fresh_store};
use memory_config::ServiceConfig;
use memory_domain::types::*;
use memory_engine::app::management;
use memory_engine::runtime::{indexer, instance::Instance};
use memory_store::schema;
use memory_store::store::{to_doc, turn_rid, Tx};
use serde_json::json;
use std::sync::Arc;

async fn has_turn_index(t: &Instance, name: &str) -> bool {
    let info = t
        .store
        .select("INFO FOR TABLE turns", vec![])
        .await
        .unwrap();
    info[0]["indexes"].as_object().unwrap().contains_key(name)
}

#[tokio::test(flavor = "multi_thread")]
async fn selftest() {
    let eng = engine(ServiceConfig::default());
    let (t, _, dir) = fresh_store(&eng, "legacy-turns", "test").await;
    let dim = eng.models.dim;
    // What memos left: its turn vector index, a turn with its vectors and the turn's pending index task.
    t.store
        .query(
            &format!(
                "DEFINE INDEX turns_vec ON turns FIELDS embedding HNSW DIMENSION {dim} DIST COSINE"
            ),
            vec![],
        )
        .await
        .unwrap();
    let fact = MemoryRec {
        id: "fact".into(),
        mtype: T_FACT.into(),
        text: "The API listens on port 8200".into(),
        status: S_CURRENT.into(),
        scope: MemScope::project("p"),
        ..Default::default()
    };
    let turn = turn_rid("test", "T1");
    let mut tx = Tx::new();
    tx.put(schema::MEMORIES, &fact.id, to_doc(&fact).unwrap())
        .unwrap();
    tx.index_task(schema::MEMORIES, &fact.id, "upsert").unwrap();
    tx.put(
        schema::TURNS,
        &turn,
        json!({"key": "test::T1", "turnId": "T1", "platform": "test", "project": "p", "session": "s",
            "userMessage": "Which port does the API use?", "assistantMessage": "It listens on 8200.",
            "userTs": "2026-10-12T08:00:00Z", "assistantTs": "2026-10-12T08:00:01Z",
            "startedAt": "2026-10-12T08:00:00Z", "endedAt": "2026-10-12T08:00:01Z",
            "recordedAt": "2026-10-12T08:00:01Z", "digest": "API port 8200", "seq": 1,
            "derivedMemoryIds": [fact.id], "derivedText": fact.text,
            "chunks": [{"role": "assistant", "start": 0, "end": 9}],
            "embedding": vec![0.5; dim], "embeddingModel": "retired",
            "chunkEmbeddings": [vec![0.5; dim]], "chunkEmbeddingModel": "retired"}),
    )
    .unwrap();
    tx.index_task(schema::TURNS, &turn, "upsert").unwrap();
    t.store.commit(tx).await.unwrap();
    assert!(has_turn_index(&t, "turns_vec").await);

    // Opening the store drops the turn vector index.
    t.store.shutdown().await;
    drop(t);
    let t = Arc::new(
        Instance::open(&dir, &eng.cfg.surreal_bin, dim, eng.algorithm().schema())
            .await
            .unwrap(),
    );
    assert!(!has_turn_index(&t, "turns_vec").await);

    // The turn's task is consumed without embedding the turn or its chunks.
    indexer::drain(&eng, &t).await.unwrap();
    assert_eq!(indexer::pending(&t).await.unwrap(), 0);
    assert_eq!(
        t.vecs.read().await.memories.keys().collect::<Vec<_>>(),
        [&fact.id]
    );
    let stored = t
        .store
        .get_doc(schema::TURNS, &turn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored["embeddingModel"], "retired");
    assert_eq!(stored["chunkEmbeddingModel"], "retired");

    // A rebuild re-embeds memories only.
    indexer::schedule_rebuild(&t).await.unwrap();
    assert_eq!(indexer::pending(&t).await.unwrap(), 1);
    indexer::drain(&eng, &t).await.unwrap();

    // Deleting the fact removes it from the turn it was derived from.
    let deleted = management::delete_memory(
        &eng,
        &t,
        IdReq {
            id: fact.id.clone(),
        },
    )
    .await
    .unwrap();
    assert_eq!(deleted["turnsUpdated"], 1);
    let stored = t
        .store
        .get_doc(schema::TURNS, &turn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored["derivedMemoryIds"], json!([]));
    assert_eq!(stored["assistantMessage"], "It listens on 8200.");
    assert!(t.vecs.read().await.memories.is_empty());
    println!("legacy_turns_selftest OK: turn vector index dropped, turn tasks consumed without embedding, rebuild queues memories only, deletion repairs turns; data {}", dir.display());
}
