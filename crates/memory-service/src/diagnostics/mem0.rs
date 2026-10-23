//! Offline integrity checks for the additive strategy, using mock embeddings and no provider.
use super::{engine, fresh_store};
use memory_algo_mem0::{formation as mem0_formation, retrieval as mem0_retrieval};
use memory_config::ServiceConfig;
use memory_domain::types::*;
use memory_engine::app::{ingest, management};
use memory_engine::retrieval;
use memory_engine::runtime::indexer;
use memory_rules::util;
use memory_store::schema;
use serde_json::{json, Value};

#[tokio::test(flavor = "multi_thread")]
async fn selftest() {
    let schema = json!({"x-existingIds":["0"]});
    assert!(mem0_formation::validate_output(&schema, &json!({"memory":[]})).is_ok());
    assert!(mem0_formation::validate_output(
        &schema,
        &json!({"memory":[{"id":"0","text":"x","attributed_to":"user","linked_memory_ids":["9"]}]})
    )
    .is_err());
    let eng = engine(ServiceConfig::default());
    let (t, caller, dir) = fresh_store(&eng, "mem0-selftest", "test").await;
    let scope = ReqScope {
        project: Some("a".into()),
        session: Some("s".into()),
        ..Default::default()
    };
    let request = AddMemoriesReq {
        scope: scope.clone(),
        turn_id: Some("raw1".into()),
        text: Some("database port 8765".into()),
        infer: Some(false),
        sync: Some(true),
        ..Default::default()
    };
    let saved = ingest::add_memories(&eng, &t, &caller, request.clone())
        .await
        .unwrap();
    assert_eq!(saved["searchable"], true);
    let id = saved["memoryIds"][0].as_str().unwrap().to_string();
    assert_eq!(
        ingest::add_memories(&eng, &t, &caller, request.clone())
            .await
            .unwrap()["duplicate"],
        true
    );
    // The same content under another turn maps to the same memory.
    let same = AddMemoriesReq {
        turn_id: Some("raw2".into()),
        ..request
    };
    assert_eq!(
        ingest::add_memories(&eng, &t, &caller, same).await.unwrap()["memoryIds"][0],
        id
    );
    let search = |eng, project: &str, query: &str| {
        mem0_retrieval::search(
            eng,
            &t,
            &caller,
            SearchReq {
                query: query.into(),
                scope: Some(ReqScope {
                    project: Some(project.into()),
                    ..scope.clone()
                }),
                ..Default::default()
            },
        )
    };
    let ids = |r: &Value| -> Vec<String> {
        r["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["memory"]["id"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(
        ids(&search(&eng, "a", "database port 8765").await.unwrap()),
        [id.clone()]
    );
    // The ANN path returns the same memory as brute force.
    let ann = engine(ServiceConfig {
        brute_force_limit: 0,
        ..Default::default()
    });
    assert_eq!(
        ids(&search(&ann, "a", "database port 8765").await.unwrap()),
        [id.clone()]
    );
    assert!(ids(&search(&eng, "b", "database port 8765").await.unwrap()).is_empty());
    // Mem0 attributes suggestions to the assistant; they stay out of the bootstrap context.
    let suggestion = MemoryRec {
        id: "msuggestion".into(),
        mtype: T_FACT.into(),
        text: "User was recommended Postgres".into(),
        scope: MemScope::project("a"),
        status: S_CURRENT.into(),
        recorded_at: util::now_str(),
        source: Source {
            platform: "test".into(),
            origin: ORIGIN_ASSISTANT.into(),
            ..Default::default()
        },
        ..Default::default()
    };
    t.store
        .put_doc(schema::MEMORIES, &suggestion.id, &suggestion)
        .await
        .unwrap();
    let context = retrieval::get_context(
        &eng,
        &t,
        &caller,
        GetContextReq {
            scope: scope.clone(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let text = context["text"].as_str().unwrap();
    assert!(text.contains("database port 8765") && !text.contains("recommended Postgres"));
    // The indexing recovery path publishes already committed vectors without another extraction.
    let rid = saved["requestId"].as_str().unwrap();
    t.store
        .merge_doc(
            schema::REQUESTS,
            rid,
            json!({"status":"indexing","searchable":false}),
        )
        .await
        .unwrap();
    t.vecs.write().await.memories.clear();
    mem0_formation::process_jobs(&eng, &t).await.unwrap();
    assert_eq!(
        ingest::get_task(
            &eng,
            &t,
            GetTaskReq {
                request_id: rid.into()
            }
        )
        .await
        .unwrap()["searchable"],
        true
    );
    // A correction hides the old value before any later query.
    let corrected = management::correct_memory(
        &eng,
        &t,
        &caller,
        CorrectMemoryReq {
            id: id.clone(),
            text: Some("database port 9000".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let new_id = corrected["memory"]["id"].as_str().unwrap().to_string();
    indexer::drain(&eng, &t).await.unwrap();
    let result = search(&eng, "a", "database port").await.unwrap();
    assert_eq!(ids(&result), [new_id.clone()]);
    let feedback = management::feedback(
        &eng,
        &t,
        FeedbackReq {
            retrieval_id: result["retrievalId"].as_str().unwrap().into(),
            used: vec![new_id.clone()],
            outcome: "helpful".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(feedback["accepted"], true);
    management::delete_memory(&eng, &t, IdReq { id: new_id })
        .await
        .unwrap();
    assert!(ids(&search(&eng, "a", "database port").await.unwrap()).is_empty());
    assert_eq!(eng.llm.usage_snapshot()["calls"], 0);
    println!("mem0: contract, scope, raw/idempotent writes, immediate visibility, publication recovery, correction, deletion, zero generation passed; data {}", dir.display());
}
