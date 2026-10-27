//! Consolidation proposals end to end: Mem0 formation and consolidation both answered by a local
//! generation stub, with mock embeddings. No external model call.
use crate::test_support::{fresh_store, generation_stub, stub_engine};
use memory_config::ServiceConfig;
use memory_domain::types::*;
use memory_engine::app::{consolidation, ingest, management};
use memory_engine::runtime::engine::Engine;
use memory_store::schema;
use memory_store::store::Tx;
use serde_json::{json, Value};
use std::sync::Arc;

pub(crate) fn engine(url: &str, enabled: bool) -> Arc<Engine> {
    let cfg = ServiceConfig {
        consolidation_enabled: enabled,
        ..Default::default()
    };
    stub_engine(cfg, url)
}

pub(crate) async fn add(
    eng: &Arc<Engine>,
    t: &Arc<memory_engine::runtime::instance::Instance>,
    c: &Caller,
    project: &str,
    turn: &str,
    at: &str,
    texts: &[&str],
) -> String {
    let req = AddMemoriesReq {
        scope: ReqScope {
            project: Some(project.into()),
            session: Some("s".into()),
            ..Default::default()
        },
        turn_id: Some(turn.into()),
        messages: Some(
            texts
                .iter()
                .map(|text| InMessage {
                    role: "user".into(),
                    content: text.to_string(),
                    timestamp: at.into(),
                })
                .collect(),
        ),
        sync: Some(true),
        ..Default::default()
    };
    let out = ingest::add_memories(eng, t, c, req).await.unwrap();
    assert_eq!(out["status"], "completed", "{out}");
    out["requestId"].as_str().unwrap().to_string()
}

pub async fn selftest() {
    let url = generation_stub().await;
    let eng = engine(&url, true);
    let (t, caller, dir) = fresh_store(&eng, "consolidation-selftest", "test").await;
    let first = add(
        &eng,
        &t,
        &caller,
        "main",
        "c1",
        "2026-01-01T09:00:00Z",
        &[
            "User uses Arch Linux on the laptop",
            "User drinks green tea every morning",
            "User met Alice Smith at the conference",
            "User runs Postgres in production",
            "User keeps a dog named Rex",
            "User plays chess on weekends",
            "User writes Rust at work",
            "User commutes by bicycle",
        ],
    )
    .await;
    let second = add(
        &eng,
        &t,
        &caller,
        "main",
        "c2",
        "2026-02-01T09:00:00Z",
        &[
            "User uses Arch Linux on the work laptop",
            "User reports to Alice Smith",
            "User runs Postgres in production on AWS",
            "User uses Arch Linux on the home laptop",
        ],
    )
    .await;
    // Explicit text (origin explicit) is never a candidate, however similar.
    let raw = AddMemoriesReq {
        scope: ReqScope {
            project: Some("main".into()),
            session: Some("s".into()),
            ..Default::default()
        },
        turn_id: Some("c3".into()),
        text: Some("User uses Arch Linux on the laptop".into()),
        infer: Some(false),
        sync: Some(true),
        ..Default::default()
    };
    ingest::add_memories(&eng, &t, &caller, raw).await.unwrap();
    let facts = t.store.current_project_facts("main").await.unwrap();
    assert_eq!(facts.len(), 13);
    let id = |text: &str, origin: &str| {
        facts
            .iter()
            .find(|m| m.text == text && m.source.origin == origin)
            .map(|m| m.id.clone())
            .unwrap()
    };
    let (laptop, work) = (
        id("User uses Arch Linux on the laptop", ORIGIN_USER),
        id("User uses Arch Linux on the work laptop", ORIGIN_USER),
    );
    let explicit = id("User uses Arch Linux on the laptop", ORIGIN_EXPLICIT);
    // A record in an open uncertainty is never a candidate.
    let home = id("User uses Arch Linux on the home laptop", ORIGIN_USER);
    let mut tx = Tx::new();
    tx.put(
        schema::UNCERTAINTIES,
        "uopen",
        json!({"memoryIds":[home],"scope":MemScope::project("main"),
        "reason":"test","status":"open","createdAt":"2026-02-02T00:00:00Z"}),
    )
    .unwrap();
    t.store.commit(tx).await.unwrap();

    let request = |project: &str, dry_run: Option<bool>| ConsolidateReq {
        project: project.into(),
        dry_run,
    };
    // Off by default; dry runs only; one named project.
    let disabled = engine(&url, false);
    for (eng, req) in [
        (&disabled, request("main", Some(true))),
        (&eng, request("main", None)),
        (&eng, request("main", Some(false))),
        (&eng, request("*", Some(true))),
    ] {
        assert!(matches!(
            consolidation::consolidate(eng, &t, req).await,
            Err(ApiError::InvalidArgument(_))
        ));
    }

    let revision = t.store.memory_revision();
    let memories = t.store.count(schema::MEMORIES, None).await.unwrap();
    let out = consolidation::consolidate(&eng, &t, request("main", Some(true)))
        .await
        .unwrap();
    let run = &out["run"];
    assert_eq!(run["status"], "proposed", "{run}");
    assert_eq!(run["projectFacts"], 13);
    assert_eq!(run["eligibleFacts"], 11);
    assert_eq!(run["snapshotRevision"], revision);
    assert_eq!(run["promptFormat"], "mem0-consolidation-v1");
    assert_eq!(run["llmModel"], "stub-model");
    let clusters = run["clusters"].as_array().unwrap();
    assert_eq!(clusters.len(), 3, "{run}");
    let members: Vec<&Value> = clusters.iter().map(|c| &c["memberIds"]).collect();
    assert!(members.contains(&&json!([laptop, work])), "{members:?}");
    assert!(!members
        .iter()
        .flat_map(|m| m.as_array().unwrap())
        .any(|m| m == &explicit || m == &home));
    assert_eq!(run["acceptedOps"], 2);
    assert_eq!(run["rejectedOps"], 1);
    assert_eq!(run["retireCount"], 2);
    let proposals = out["proposals"].as_array().unwrap();
    let merge = proposals
        .iter()
        .find(|p| p["memoryIds"] == json!([laptop, work]))
        .unwrap();
    assert_eq!(merge["op"], "merge");
    assert_eq!(merge["status"], "proposed");
    assert_eq!(merge["text"], "User uses Arch Linux on the work laptop");
    assert_eq!(merge["detail"]["inputRequestIds"], json!([first, second]));
    assert_eq!(
        merge["detail"]["inputContentKeys"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let rejected = proposals
        .iter()
        .find(|p| p["status"] == "rejected")
        .unwrap();
    assert!(
        rejected["rejectReason"]
            .as_str()
            .unwrap()
            .contains("\"16\""),
        "{rejected}"
    );
    let keep = proposals.iter().find(|p| p["op"] == "keep").unwrap();
    assert_eq!(keep["status"], "proposed");
    assert_eq!(
        keep["detail"]["inputContentKeys"].as_array().unwrap().len(),
        2
    );
    assert!(keep["detail"].get("inputRequestIds").is_none());

    // Proposing writes no memory: same records, same statuses, same revision.
    assert_eq!(t.store.memory_revision(), revision);
    assert_eq!(
        t.store.count(schema::MEMORIES, None).await.unwrap(),
        memories
    );
    assert!(t.store.current_project_facts("main").await.unwrap().len() == 13);
    // The run, its proposals and the raw generation responses are exportable evidence.
    let runs = management::export(&eng, &t, json!({"table":"consolidation_runs"}))
        .await
        .unwrap();
    assert_eq!(runs["count"], 1);
    assert_eq!(runs["records"][0]["runId"], run["runId"]);
    let stored = management::export(&eng, &t, json!({"table":"proposals"}))
        .await
        .unwrap();
    assert_eq!(stored["count"], 3);
    let responses = management::export(&eng, &t, json!({"table":"llm_responses"}))
        .await
        .unwrap();
    for log in run["responseLogIds"].as_array().unwrap() {
        let row = responses["records"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| &r["mid"] == log)
            .unwrap();
        assert_eq!(row["stage"], "Mem0-consolidate");
        assert!(row["rawBody"].as_str().unwrap().contains("operations"));
    }
    // What the web UI may offer follows the flag and the key.
    for (eng, consolidation) in [(&eng, true), (&disabled, false)] {
        assert_eq!(
            management::stats(eng, &t).await.unwrap()["features"],
            json!({"consolidation": consolidation, "generation": true})
        );
    }
    // A formed request shows its extraction and the generation response it parsed.
    let shown = management::request(
        &t,
        GetTaskReq {
            request_id: first.clone(),
        },
    )
    .await
    .unwrap();
    assert_eq!(shown["request"]["id"], first);
    assert_eq!(
        shown["request"]["inputMessages"].as_array().unwrap().len(),
        8
    );
    let extractions = shown["extractions"].as_array().unwrap();
    assert_eq!(extractions.len(), 1, "{shown}");
    assert_eq!(
        (&extractions[0]["requestId"], &extractions[0]["stage"]),
        (&json!(first), &json!("Mem0-add"))
    );
    assert!(extractions[0]["id"].is_string() && extractions[0].get("mid").is_none());
    let responses = shown["responses"].as_array().unwrap();
    assert_eq!(responses.len(), 1, "{shown}");
    assert_eq!(responses[0]["id"], extractions[0]["responseLogId"]);
    assert!(responses[0]["rawContent"]
        .as_str()
        .unwrap()
        .contains("Arch Linux"));
    // Raw responses by stage, and the failed calls among them: a transport error, a server error and an output
    // of the wrong shape.
    let calls =
        |req: Value| management::list_model_responses(&t, serde_json::from_value(req).unwrap());
    let call_ids = |page: &Value| -> Vec<String> {
        let mut ids: Vec<String> = page["responses"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap().to_string())
            .collect();
        ids.sort();
        ids
    };
    let mut consolidating: Vec<String> = run["responseLogIds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| id.as_str().unwrap().to_string())
        .collect();
    consolidating.sort();
    let found = calls(json!({"stage": "Mem0-consolidate"})).await.unwrap();
    assert_eq!(call_ids(&found), consolidating);
    assert_eq!(
        calls(json!({"failedOnly": true})).await.unwrap()["responses"],
        json!([])
    );
    let now = memory_rules::util::now_str();
    let mut failed = vec![];
    for row in [
        json!({"stage": "Mem0-add", "attempt": 1, "transportError": "request error: timed out"}),
        json!({"stage": "Mem0-add", "attempt": 2, "httpStatus": 503, "rawBody": "busy"}),
        json!({"stage": "Mem0-add", "attempt": 3, "httpStatus": 200, "rawBody": "<html>"}),
        json!({"stage": "Mem0-consolidate", "attempt": 1, "httpStatus": 200, "rawBody": "{}",
            "rawContent": "{}", "validationError": "missing operations", "validOutputShape": false}),
    ] {
        let mut row = row;
        row["model"] = json!("stub-model");
        row["createdAt"] = json!(now);
        row["request"] = json!({});
        failed.push(t.store.log(schema::LLM_RESPONSES, row).await.unwrap());
    }
    failed.sort();
    let mut paged = vec![];
    let mut cursor = Value::Null;
    loop {
        let page = calls(json!({"failedOnly": true, "limit": 2, "cursor": cursor}))
            .await
            .unwrap();
        assert!(page["responses"].as_array().unwrap().len() <= 2);
        paged.extend(call_ids(&page));
        cursor = page["nextCursor"].clone();
        if cursor.is_null() {
            break;
        }
    }
    paged.sort();
    assert_eq!(paged, failed);
    let shape = calls(json!({"failedOnly": true, "stage": "Mem0-consolidate"}))
        .await
        .unwrap();
    assert_eq!(shape["responses"].as_array().unwrap().len(), 1);
    assert_eq!(
        shape["responses"][0]["validationError"],
        "missing operations"
    );
    for bad in [
        json!({"limit": 0}),
        json!({"limit": 101}),
        json!({"cursor": "yesterday:l1"}),
    ] {
        assert!(
            matches!(calls(bad.clone()).await, Err(ApiError::InvalidArgument(_))),
            "{bad}"
        );
    }

    // A run that would retire more than the per-run share of a project's facts is aborted.
    add(
        &eng,
        &t,
        &caller,
        "tiny",
        "t1",
        "2026-03-01T09:00:00Z",
        &[
            "User uses Arch Linux on the laptop",
            "User uses Arch Linux on the work laptop",
        ],
    )
    .await;
    let out = consolidation::consolidate(&eng, &t, request("tiny", Some(true)))
        .await
        .unwrap();
    assert_eq!(out["run"]["status"], "aborted", "{out}");
    assert!(out["run"]["error"]
        .as_str()
        .unwrap()
        .contains("per-run share"));
    assert_eq!(out["proposals"][0]["status"], "aborted");
    assert_eq!(
        t.store.current_project_facts("tiny").await.unwrap().len(),
        2
    );
    // An empty project needs no generation call and still records its run.
    let out = consolidation::consolidate(&eng, &t, request("empty", Some(true)))
        .await
        .unwrap();
    assert_eq!(out["run"]["clusterCount"], 0);
    assert_eq!(out["run"]["status"], "proposed");
    drop(t);
    let _ = std::fs::remove_dir_all(dir);
    println!("consolidation_selftest OK: dry-run clusters, exclusions, provenance rejection, retire cap, no memory writes; local generation stub");
}

#[cfg(test)]
mod tests {
    #[tokio::test(flavor = "multi_thread")]
    async fn proposes_without_writing_memory() {
        super::selftest().await;
    }
}
