//! Offline integrity checks for the additive strategy, using mock embeddings and no provider.
use crate::test_support::{engine, fresh_store};
use memory_algo_mem0::{formation as mem0_formation, retrieval as mem0_retrieval};
use memory_config::ServiceConfig;
use memory_domain::types::*;
use memory_engine::app::{ingest, management};
use memory_engine::retrieval;
use memory_engine::runtime::indexer;
use memory_rules::util;
use memory_store::recalls::Recall;
use memory_store::schema;
use serde_json::{json, Value};
use std::collections::BTreeMap;

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
    let first = search(&eng, "a", "database port 8765").await.unwrap();
    assert_eq!(ids(&first), [id.clone()]);
    // The first recall of a memory writes its summary with a count of 1, at the time of the log row.
    let logged = t
        .store
        .get_doc(
            schema::RETRIEVAL_LOG,
            first["retrievalId"].as_str().unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
    let recall = |at: &Value, count| Recall {
        last_recalled_at: at.as_str().unwrap().into(),
        count,
    };
    assert_eq!(
        t.store.recalls(std::slice::from_ref(&id)).await.unwrap(),
        BTreeMap::from([(id.clone(), recall(&logged["createdAt"], 1))])
    );
    let row = t
        .store
        .get_doc(schema::MEMORY_RECALLS, &id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (&row["mid"], &row["memoryId"], &row["lastRecalledTs"]),
        (&json!(id), &json!(id), &logged["createdTs"])
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
    // A search is logged with who searched and where, like a context read.
    let row = t
        .store
        .get_doc(
            schema::RETRIEVAL_LOG,
            result["retrievalId"].as_str().unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
    for (field, value) in [
        ("kind", json!("search")),
        ("platform", json!("test")),
        ("project", json!("a")),
        ("session", json!("s")),
    ] {
        assert_eq!(row[field], value, "{field}");
    }
    assert_eq!(row["returned"]["gap"], false);
    assert!(row["createdTs"].as_i64().unwrap() > 0);
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
    management::delete_memory(&eng, &t, IdReq { id: new_id.clone() })
        .await
        .unwrap();
    assert!(ids(&search(&eng, "a", "database port").await.unwrap()).is_empty());
    // This search and the one in project "b" found nothing. Without a key nothing generates.
    let stats = management::stats(&eng, &t).await.unwrap();
    assert_eq!(stats["retrieval"]["gapSearches"], 2);
    assert_eq!(
        stats["features"],
        json!({"consolidation": false, "generation": false})
    );

    // ---- the recall log: five searches and a context read by "test", a search of the web UI, a search
    // logged before searches recorded who searched (as the startup repair leaves it), and one 31 days old.
    let now = util::now();
    let logged = |platform: Option<&str>, at: chrono::DateTime<chrono::Utc>| {
        let mut row = json!({"kind": "search", "query": "port", "strategy": "mem0",
            "returned": {"memories": [{"id": id, "rank": 1}], "tokens": 4, "gap": false},
            "createdAt": util::fmt_ts(&at), "createdTs": at.timestamp()});
        if let Some(platform) = platform {
            row["platform"] = json!(platform);
            row["project"] = json!("a");
            row["session"] = json!("s");
        }
        row
    };
    // The web UI's own search is logged, and a recall of `id` that is not counted.
    let web = t
        .store
        .log_retrieval(logged(Some("web"), now))
        .await
        .unwrap();
    let log = |row| t.store.log(schema::RETRIEVAL_LOG, row);
    let legacy = log(logged(None, now)).await.unwrap();
    let old = log(logged(None, now - chrono::Duration::days(31)))
        .await
        .unwrap();
    let recalls =
        |req: Value| management::list_retrievals(&t, serde_json::from_value(req).unwrap());
    let row_ids = |page: &Value| -> Vec<String> {
        page["retrievals"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap().to_string())
            .collect()
    };
    let all = recalls(json!({})).await.unwrap();
    assert_eq!(row_ids(&all).len(), 9);
    assert!(all["nextCursor"].is_null() && all.get("window").is_none());
    assert_eq!(all["memories"][&id]["status"], S_SUPERSEDED);
    assert_eq!(all["memories"][&id]["text"], "database port 8765");
    // A row without a platform is kept when platforms are excluded; the web UI's own search is not.
    let mine = recalls(json!({"excludePlatforms": ["web"]})).await.unwrap();
    let mine_ids = row_ids(&mine);
    assert_eq!(mine_ids.len(), 8);
    assert!(mine_ids.contains(&legacy) && mine_ids.contains(&old) && !mine_ids.contains(&web));
    let row = mine["retrievals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == legacy.as_str())
        .unwrap();
    assert!(row.get("platform").is_none() && row.get("mid").is_none());
    // Newest first in pages of one, the cursor carrying over ties within a second.
    let mut paged = vec![];
    let mut cursor = Value::Null;
    loop {
        let page = recalls(json!({"excludePlatforms": ["web"], "limit": 1, "cursor": cursor}))
            .await
            .unwrap();
        paged.extend(row_ids(&page));
        cursor = page["nextCursor"].clone();
        if cursor.is_null() {
            break;
        }
    }
    assert_eq!(paged, mine_ids);
    assert_eq!(mine_ids.last(), Some(&old));
    let created: Vec<i64> = mine["retrievals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["createdTs"].as_i64().unwrap())
        .collect();
    assert!(created.windows(2).all(|w| w[0] >= w[1]), "{created:?}");
    let context = recalls(json!({"kind": "context"})).await.unwrap();
    let context = context["retrievals"].as_array().unwrap();
    assert_eq!(context.len(), 1);
    assert_eq!(
        (
            &context[0]["kind"],
            &context[0]["query"],
            &context[0]["platform"]
        ),
        (&json!("context"), &Value::Null, &json!("test"))
    );
    assert!(context[0].get("feedback").is_none());
    for (filter, want) in [
        (json!({"kind": "search", "platform": "test"}), 5),
        (json!({"platform": "web"}), 1),
        (json!({"project": "b", "session": "s"}), 1),
        (
            json!({"project": "a", "kind": "search", "platform": "test"}),
            4,
        ),
    ] {
        assert_eq!(
            row_ids(&recalls(filter.clone()).await.unwrap()).len(),
            want,
            "{filter}"
        );
    }
    // `memoryId` looks back 30 days: two searches, the context read and the legacy search returned `id`.
    let recalled = recalls(json!({"memoryId": id, "excludePlatforms": ["web"]}))
        .await
        .unwrap();
    assert_eq!(recalled["window"], "30d");
    let recalled_ids = row_ids(&recalled);
    assert_eq!(recalled_ids.len(), 4, "{recalled}");
    assert!(recalled_ids.contains(&legacy) && !recalled_ids.contains(&old));
    // The search whose memory was later deleted keeps its feedback; the memory is gone from `memories`.
    let used = recalls(json!({"memoryId": new_id})).await.unwrap();
    assert_eq!(used["retrievals"].as_array().unwrap().len(), 1);
    assert_eq!(used["retrievals"][0]["id"], result["retrievalId"]);
    assert_eq!(used["retrievals"][0]["feedback"]["outcome"], "helpful");
    assert_eq!(used["memories"], json!({}));
    for bad in [
        json!({"kind": "chat"}),
        json!({"limit": 0}),
        json!({"limit": 201}),
        json!({"cursor": "port"}),
        json!({"cursor": "12:no such"}),
        json!({"memoryId": "no such"}),
    ] {
        assert!(
            matches!(
                recalls(bad.clone()).await,
                Err(ApiError::InvalidArgument(_))
            ),
            "{bad}"
        );
    }

    // ---- the recall summary: `id` was returned by two searches and the context read (which lists it more than
    // once; the web UI's search above does not count), the corrected record by one search.
    assert_eq!(
        t.store
            .recalls(&[id.clone(), new_id.clone()])
            .await
            .unwrap(),
        BTreeMap::from([
            (id.clone(), recall(&context[0]["createdAt"], 3)),
            (
                new_id.clone(),
                recall(&used["retrievals"][0]["createdAt"], 1)
            ),
        ])
    );
    // Three records recorded 40 to 42 days ago: never recalled, recalled 5 days ago and recalled 35 days ago.
    let old_fact = |n: u32, days: i64| MemoryRec {
        id: format!("mold{n}"),
        mtype: T_FACT.into(),
        text: format!("Old fact {n}"),
        scope: MemScope::project("a"),
        status: S_CURRENT.into(),
        recorded_at: util::fmt_ts(&(now - chrono::Duration::days(days))),
        ..Default::default()
    };
    for (n, days) in [(1, 40), (2, 41), (3, 42)] {
        let record = old_fact(n, days);
        t.store
            .put_doc(schema::MEMORIES, &record.id, &record)
            .await
            .unwrap();
    }
    let mut recalled = BTreeMap::new();
    for (memory, days) in [("mold3", 35), ("mold2", 5)] {
        let at = now - chrono::Duration::days(days);
        t.store
            .log_retrieval(json!({"kind": "search", "platform": "test", "query": "old",
                "returned": {"memories": [{"id": memory, "rank": 1}], "tokens": 1, "gap": false},
                "createdAt": util::fmt_ts(&at), "createdTs": at.timestamp()}))
            .await
            .unwrap();
        recalled.insert(memory, util::fmt_ts(&at));
    }
    let listed = |req: Value| management::list_memories(&t, serde_json::from_value(req).unwrap());
    let listed_ids = |page: &Value| -> Vec<String> {
        page["memories"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap().to_string())
            .collect()
    };
    let page = listed(json!({})).await.unwrap();
    assert_eq!(
        listed_ids(&page),
        ["msuggestion", "mold1", "mold2", "mold3"]
    );
    let summaries: Vec<(&Value, &Value)> = page["memories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| (&m["lastRecalledAt"], &m["recallCount"]))
        .collect();
    assert_eq!(
        summaries,
        [
            (&Value::Null, &json!(0)),
            (&Value::Null, &json!(0)),
            (&json!(recalled["mold2"]), &json!(1)),
            (&json!(recalled["mold3"]), &json!(1)),
        ]
    );
    let superseded = listed(json!({"statuses": ["superseded"]})).await.unwrap();
    assert_eq!(listed_ids(&superseded), std::slice::from_ref(&id));
    assert_eq!(
        (
            &superseded["memories"][0]["lastRecalledAt"],
            &superseded["memories"][0]["recallCount"]
        ),
        (&context[0]["createdAt"], &json!(3))
    );
    for (filter, want) in [
        (json!({"neverRecalled": true}), vec!["msuggestion", "mold1"]),
        (
            json!({"neverRecalled": false}),
            vec!["msuggestion", "mold1", "mold2", "mold3"],
        ),
        (json!({"notRecalledDays": 30}), vec!["mold1", "mold3"]),
        (
            json!({"notRecalledDays": 3}),
            vec!["mold1", "mold2", "mold3"],
        ),
        (
            json!({"notRecalledDays": 30, "text": "fact 3"}),
            vec!["mold3"],
        ),
        (json!({"neverRecalled": true, "text": "old"}), vec!["mold1"]),
    ] {
        assert_eq!(
            listed_ids(&listed(filter.clone()).await.unwrap()),
            want,
            "{filter}"
        );
    }
    // Pages of one: the total counts every match, and the cursor carries over the subquery's filter.
    let first = listed(json!({"notRecalledDays": 30, "limit": 1}))
        .await
        .unwrap();
    assert_eq!(
        (listed_ids(&first), &first["total"]),
        (vec!["mold1".into()], &json!(2))
    );
    let cursor = first["nextCursor"].clone();
    let second = listed(json!({"notRecalledDays": 30, "limit": 1, "cursor": cursor}))
        .await
        .unwrap();
    assert_eq!(
        (listed_ids(&second), &second["nextCursor"]),
        (vec!["mold3".into()], &Value::Null)
    );
    for bad in [
        json!({"neverRecalled": true, "notRecalledDays": 30}),
        json!({"notRecalledDays": 0}),
        json!({"notRecalledDays": 3651}),
    ] {
        assert!(
            matches!(listed(bad.clone()).await, Err(ApiError::InvalidArgument(_))),
            "{bad}"
        );
    }
    // The overview counts the same active records: never recalled, and silent for 30 days.
    let stats = management::stats(&eng, &t).await.unwrap();
    assert_eq!(
        (
            &stats["retrieval"]["neverRecalled"],
            &stats["retrieval"]["notRecalled30d"]
        ),
        (&json!(2), &json!(2))
    );

    // ---- requests: the two text writes and an explicit instruction, without their input messages
    let instruction = ingest::add_memories(
        &eng,
        &t,
        &caller,
        AddMemoriesReq {
            scope: scope.clone(),
            memory_type: Some(T_INSTRUCTION.into()),
            text: Some("Always run the tests".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let requests = |req: Value| management::list_requests(&t, serde_json::from_value(req).unwrap());
    let request_ids = |page: &Value| -> Vec<String> {
        page["requests"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap().to_string())
            .collect()
    };
    let listed = requests(
        json!({"kind": "mem0", "statuses": ["completed"], "platform": "test", "project": "a"}),
    )
    .await
    .unwrap();
    assert_eq!(listed["requests"].as_array().unwrap().len(), 2, "{listed}");
    for row in listed["requests"].as_array().unwrap() {
        assert_eq!(
            (&row["messageCount"], &row["id"]),
            (&json!(1), &row["requestId"])
        );
        assert!(row.get("inputMessages").is_none() && row.get("mid").is_none());
    }
    assert!(request_ids(&listed).contains(&rid.to_string()));
    let only = requests(json!({"kind": "instruction"})).await.unwrap();
    assert_eq!(
        request_ids(&only),
        [instruction["requestId"].as_str().unwrap()]
    );
    assert_eq!(only["requests"][0]["messageCount"], 0);
    assert_eq!(
        requests(json!({"statuses": ["failed"]})).await.unwrap()["requests"],
        json!([])
    );
    let mut paged = vec![];
    let mut cursor = Value::Null;
    loop {
        let page = requests(json!({"limit": 1, "cursor": cursor}))
            .await
            .unwrap();
        paged.extend(request_ids(&page));
        cursor = page["nextCursor"].clone();
        if cursor.is_null() {
            break;
        }
    }
    assert_eq!(paged, request_ids(&requests(json!({})).await.unwrap()));
    assert_eq!(paged.len(), 3);
    for bad in [
        json!({"kind": "fact"}),
        json!({"limit": 0}),
        json!({"limit": 201}),
        json!({"cursor": "1:rq"}),
    ] {
        assert!(
            matches!(
                requests(bad.clone()).await,
                Err(ApiError::InvalidArgument(_))
            ),
            "{bad}"
        );
    }
    // A raw write calls no model: the whole request, with no extractions or responses.
    let shown = management::request(
        &t,
        GetTaskReq {
            request_id: rid.into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(shown["request"]["id"], rid);
    assert_eq!(
        shown["request"]["inputMessages"][0]["content"],
        "database port 8765"
    );
    assert_eq!(
        (&shown["extractions"], &shown["responses"]),
        (&json!([]), &json!([]))
    );
    let show = |id: &str| {
        management::request(
            &t,
            GetTaskReq {
                request_id: id.into(),
            },
        )
    };
    assert!(matches!(
        show("no such").await,
        Err(ApiError::InvalidArgument(_))
    ));
    assert!(matches!(
        show("rqmissing").await,
        Err(ApiError::NotFound(_))
    ));
    assert_eq!(eng.llm.usage_snapshot()["calls"], 0);
    println!("mem0: contract, scope, raw/idempotent writes, immediate visibility, publication recovery, correction, deletion, zero generation passed; data {}", dir.display());
}
