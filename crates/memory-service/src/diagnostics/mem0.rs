//! Offline integrity checks for the additive strategy, using mock embeddings and no provider.
use super::{
    config::ServiceConfig, engine::Engine, ingest, llm::Llm, manage, mem0_formation,
    mem0_retrieval, models::ModelHub, tenant::Tenant, types::*, util,
};
use serde_json::json;
use std::sync::Arc;

pub async fn selftest() {
    let schema = json!({"x-existingIds":["0"]});
    assert!(mem0_formation::validate_output(&schema, &json!({"memory":[]})).is_ok());
    for invalid in [
        json!({"facts":[]}),
        json!({"memory":[{"id":"0","text":"x","attributed_to":"other"}]}),
        json!({"memory":[{"id":"1","text":"x","attributed_to":"user"}]}),
        json!({"memory":[{"id":"0","text":"x","attributed_to":"user","linked_memory_ids":["9"]}]}),
    ] {
        assert!(mem0_formation::validate_output(&schema, &invalid).is_err());
    }
    let numeric_link =
        json!({"memory":[{"id":"0","text":"x","attributed_to":"user","linked_memory_ids":[0]}]});
    let error = mem0_formation::validate_output(&schema, &numeric_link)
        .unwrap_err()
        .to_string();
    assert!(error.contains("memory[0].linked_memory_ids[0]"));
    assert!(error.contains("replace the number 0 with \"0\""));
    assert_eq!(numeric_link["memory"][0]["linked_memory_ids"][0], 0);
    assert!(mem0_formation::validate_output(
        &schema,
        &json!({"memory":[{"id":"0","text":"x","attributed_to":"user","linked_memory_ids":["0"]}]})
    )
    .is_ok());
    let root = std::env::temp_dir().join(util::new_id("mem0-selftest-"));
    let eng = Engine::new(
        ServiceConfig {
            data_dir: root.clone(),
            ..Default::default()
        },
        Llm::new("http://127.0.0.1:1", "", "disabled", 1, 1).unwrap(),
        ModelHub::mock(),
    );
    let tenant = Arc::new(Tenant::open("test", &root, eng.models.dim).await.unwrap());
    let caller = Caller {
        tenant: tenant.name.clone(),
        platform: "test".into(),
    };
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
    let saved = ingest::add_memories(&eng, &tenant, &caller, request.clone())
        .await
        .unwrap();
    assert_eq!(saved["status"], "completed");
    assert_eq!(saved["searchable"], true);
    let id = saved["memoryIds"][0].as_str().unwrap().to_string();
    assert_eq!(
        ingest::add_memories(&eng, &tenant, &caller, request.clone())
            .await
            .unwrap()["duplicate"],
        true
    );
    let mut retry = request.clone();
    retry.text = Some("a retry cannot overwrite the original".into());
    assert_eq!(
        ingest::add_memories(&eng, &tenant, &caller, retry)
            .await
            .unwrap()["memoryIds"][0],
        id
    );
    let mut same = request.clone();
    same.turn_id = Some("raw2".into());
    assert_eq!(
        ingest::add_memories(&eng, &tenant, &caller, same)
            .await
            .unwrap()["memoryIds"][0],
        id
    );
    let result = mem0_retrieval::search(
        &eng,
        &tenant,
        &caller,
        SearchReq {
            query: "database port 8765".into(),
            scope: Some(scope.clone()),
            include_evidence: Some(true),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(result["results"][0]["memory"]["id"], id);
    let ann_engine = Engine::new(
        ServiceConfig {
            brute_force_limit: 0,
            ..Default::default()
        },
        Llm::new("http://127.0.0.1:1", "", "disabled", 1, 1).unwrap(),
        ModelHub::mock(),
    );
    let ann = mem0_retrieval::search(
        &ann_engine,
        &tenant,
        &caller,
        SearchReq {
            query: "database port 8765".into(),
            scope: Some(scope.clone()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(ann["results"][0]["memory"]["id"], id);

    assert_eq!(
        result["results"][0]["sourceBatch"]["verifiedClaimSpan"],
        false
    );
    // Compare actual instants across offsets, including equality, and reject invalid filters.
    let clock_scope = ReqScope {
        project: Some("clock".into()),
        ..scope.clone()
    };
    for (key, timestamp) in [
        ("before", "2026-10-12T08:00:00+08:00"),
        ("equal", "2026-10-12T09:00:00+08:00"),
        ("after", "2026-10-11T20:00:00-08:00"),
    ] {
        ingest::add_memories(
            &eng,
            &tenant,
            &caller,
            AddMemoriesReq {
                scope: clock_scope.clone(),
                turn_id: Some(key.into()),
                infer: Some(false),
                sync: Some(true),
                messages: Some(vec![InMessage {
                    role: "user".into(),
                    content: key.into(),
                    timestamp: timestamp.into(),
                }]),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }
    let filtered = super::retrieve::list_turns(
        &eng,
        &tenant,
        &caller,
        ListTurnsReq {
            scope: clock_scope.clone(),
            since: Some("2026-10-12T01:00:00Z".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        filtered["turns"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["turnId"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["equal", "after"]
    );
    assert!(super::retrieve::list_turns(
        &eng,
        &tenant,
        &caller,
        ListTurnsReq {
            scope: clock_scope,
            since: Some("invalid".into()),
            ..Default::default()
        }
    )
    .await
    .is_err());
    assert!(result["profiles"].as_array().unwrap().is_empty());
    let other = ReqScope {
        project: Some("b".into()),
        ..scope.clone()
    };
    assert!(mem0_retrieval::search(
        &eng,
        &tenant,
        &caller,
        SearchReq {
            query: "database port 8765".into(),
            scope: Some(other),
            ..Default::default()
        }
    )
    .await
    .unwrap()["results"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(
        eng.llm
            .total_attempts
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    // The indexing recovery path publishes already committed vectors without another extraction.
    let rid = saved["requestId"].as_str().unwrap();
    tenant
        .store
        .merge_doc(
            "requests",
            rid,
            json!({"status":"indexing","searchable":false}),
        )
        .await
        .unwrap();
    tenant.vecs.write().await.memories.clear();
    mem0_formation::process_jobs(&eng, &tenant).await.unwrap();
    assert_eq!(
        ingest::get_task(
            &eng,
            &tenant,
            GetTaskReq {
                request_id: rid.into()
            }
        )
        .await
        .unwrap()["searchable"],
        true
    );
    // Corrections preserve the interface and hide the old value before any later query.
    let corrected = manage::correct_memory(
        &eng,
        &tenant,
        &caller,
        CorrectMemoryReq {
            id: id.clone(),
            text: Some("database port 9000".into()),
            reason: Some("explicit correction".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let new_id = corrected["memory"]["id"].as_str().unwrap().to_string();
    super::indexer::drain(&eng, &tenant).await.unwrap();
    let result = mem0_retrieval::search(
        &eng,
        &tenant,
        &caller,
        SearchReq {
            query: "database port".into(),
            scope: Some(scope.clone()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(result["results"]
        .as_array()
        .unwrap()
        .iter()
        .all(|r| r["memory"]["id"] != id));
    assert!(result["results"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["memory"]["id"] == new_id));
    // Feedback remains restricted to IDs actually returned by this search.
    let feedback = manage::feedback(
        &eng,
        &tenant,
        FeedbackReq {
            retrieval_id: result["retrievalId"].as_str().unwrap().into(),
            used: vec![new_id.clone()],
            outcome: "helpful".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(feedback["accepted"], true);
    let feedback_log = tenant
        .store
        .get_doc("retrieval_log", result["retrievalId"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(feedback_log["feedback"]["applied"][0], new_id);
    manage::delete_memory(&eng, &tenant, IdReq { id: new_id })
        .await
        .unwrap();
    assert!(mem0_retrieval::search(
        &eng,
        &tenant,
        &caller,
        SearchReq {
            query: "database port".into(),
            scope: Some(scope),
            ..Default::default()
        }
    )
    .await
    .unwrap()["results"]
        .as_array()
        .unwrap()
        .is_empty());
    drop(tenant);
    drop(eng);
    std::fs::remove_dir_all(root).unwrap();
    println!("mem0: contract, scope, raw/idempotent writes, immediate visibility, publication recovery, deletion, zero generation passed");
}
