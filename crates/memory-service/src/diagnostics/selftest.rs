//! `agent-memoryd selftest`: storage-layer round trip (kept out of `cargo test` so the main crate does not
//! need a second dependency build with dev-dependency features).
use serde_json::json;

use super::store::*;
use super::types::*;

fn tempdir() -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("memory-selftest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

pub async fn store_roundtrip() {
    let dir = tempdir();
    let s = Store::open(&dir.as_path().join("db"), 4).await.unwrap();
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
    tx.put("memories", &m.id, to_doc(&m).unwrap()).unwrap();
    tx.index_task("memories", &m.id, "upsert").unwrap();
    tx.merge("memories", &m.id, json!({"evidenceCount": 2}))
        .unwrap();
    s.commit(tx).await.unwrap();
    let got = s.get_memory("mabc123").await.unwrap().unwrap();
    assert_eq!(got.text, m.text);
    assert_eq!(got.evidence_count, 2);
    assert_eq!(got.scope.project.as_deref(), Some("p1"));
    s.set_embedding("memories", "mabc123", Some(&[0.1, 0.2, 0.3, 0.4]), "x")
        .await
        .unwrap();
    let e = s.load_embeddings("memories").await.unwrap();
    assert_eq!(e.len(), 1);
    assert_eq!(e[0].0, "mabc123");
    let found = s
        .find_memories(
            "status = $s AND effectiveTs > 0",
            vec![("s", json!("current"))],
        )
        .await
        .unwrap();
    assert_eq!(found.len(), 1);
    let many = s
        .get_memories(&["mabc123".to_string(), "mnone".to_string()])
        .await
        .unwrap();
    assert_eq!(many.len(), 1);
    assert_eq!(s.count("index_tasks", None).await.unwrap(), 1);
    let tasks = s
        .select(
            "SELECT * OMIT id FROM index_tasks ORDER BY createdTs ASC LIMIT 5",
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(tasks[0]["targetId"], json!("mabc123"));
    // transaction failure rolls back
    let mut tx = Tx::new();
    tx.merge("memories", "mabc123", json!({"status": "superseded"}))
        .unwrap();
    tx.raw("THROW 'boom'", vec![]);
    assert!(s.commit(tx).await.is_err());
    assert_eq!(
        s.get_memory("mabc123").await.unwrap().unwrap().status,
        "current"
    );
    println!("store_roundtrip OK");
}

/// Mock-model integration coverage for retrieval and management, including primary storage and indexes.
/// No API key, generative-model request or background worker is involved.
pub async fn retrieval_roundtrip() {
    use super::{
        config::ServiceConfig, engine::Engine, indexer, llm::Llm, manage, models::ModelHub,
        retrieve, tenant::Tenant, timeparse, util,
    };
    use std::sync::Arc;
    assert_eq!(
        std::env::var("MEMORY_MOCK_MODELS").as_deref(),
        Ok("1"),
        "run selftest with MEMORY_MOCK_MODELS=1"
    );
    let dir = std::env::temp_dir().join(format!(
        "memory-retrieval-selftest-{}-{}",
        std::process::id(),
        util::new_id("t")
    ));
    let cfg = ServiceConfig {
        memory_strategy: super::config::MemoryStrategy::Current,
        data_dir: dir.clone(),
        ..Default::default()
    };
    let llm = Llm::new("http://127.0.0.1:1", "", "disabled", 1, 1).unwrap();
    let models = ModelHub::mock();
    let eng = Engine::new(cfg, llm, models);
    let t = Arc::new(
        Tenant::open("retrieval-selftest", &dir, eng.models.dim)
            .await
            .unwrap(),
    );
    let c = Caller {
        tenant: t.name.clone(),
        platform: "codex".into(),
    };
    let scope = ReqScope {
        project: Some("p1".into()),
        session: Some("s1".into()),
        ..Default::default()
    };
    let now = util::now_str();
    let mut mems: Vec<MemoryRec> = (0..30)
        .map(|i| MemoryRec {
            id: format!("m{i:02}"),
            mtype: T_FACT.into(),
            text: format!("unrelated filler item {i}"),
            scope: MemScope::project("p1"),
            status: S_CURRENT.into(),
            effective_at: now.clone(),
            recorded_at: now.clone(),
            stated_at: now.clone(),
            last_accessed_at: now.clone(),
            source: Source {
                platform: "codex".into(),
                origin: ORIGIN_USER.into(),
                ..Default::default()
            },
            evidence_count: 1,
            ..Default::default()
        })
        .collect();
    mems[0].text = "deployment service port 8000".into();
    mems[0].effective_at = "2026-01-01".into();
    mems[0].status = S_SUPERSEDED.into();
    mems[0].superseded_by = Some("m01".into());
    mems[1].text = "deployment service port 8100".into();
    mems[1].supersedes = Some("m00".into());
    mems[1].entity_ids = vec!["eservice".into()];
    for (i, region, other) in [(2, "east", "m03"), (3, "west", "m02")] {
        mems[i].text = format!("deployment region {region}");
        mems[i].status = S_CONFLICT.into();
        mems[i].related = vec![RelEdge {
            id: other.into(),
            relation: "conflicts".into(),
        }];
    }
    mems[4].text = "candidatefact awaiting confirmation".into();
    mems[4].status = S_CANDIDATE.into();
    mems[5].text = "secretproject confidential alpha".into();
    mems[5].scope = MemScope::project("p2");
    mems[6].text = "user prefers green tea".into();
    mems[6].scope = MemScope::global();
    mems[7].mtype = T_PROFILE.into();
    mems[7].scope = MemScope::global();
    mems[7].text = "current user profile".into();
    mems[7].recorded_at = "2026-01-01T00:00:00Z".into();
    mems[8].mtype = T_PROFILE.into();
    mems[8].text = "current project notes".into();
    mems[8].recorded_at = "2026-01-01T00:00:00Z".into();
    mems[9].text = "recent user preference".into();
    mems[10].text = "assistant speculative preference".into();
    mems[10].source.origin = ORIGIN_ASSISTANT.into();
    mems[11].mtype = T_SKILL.into();
    mems[11].text = "candidateskill deployment check".into();
    mems[11].status = S_CANDIDATE.into();
    mems[12].mtype = T_SKILL.into();
    mems[12].text = "enabledskill deployment review".into();
    mems[12].detail = Some(
        json!({"name":"enabledskill", "preconditions":"deployment ready", "steps":"secret full steps"}),
    );
    mems[13].scope = MemScope::project("p2");
    mems[13].text = "crossscope private successor".into();
    mems[13].supersedes = Some("m14".into());
    mems[14].text = "crossscope public predecessor".into();
    mems[14].status = S_SUPERSEDED.into();
    mems[14].superseded_by = Some("m13".into());
    mems[15].mtype = T_PROFILE.into();
    mems[15].scope = MemScope::global();
    mems[15].status = S_CANDIDATE.into();
    mems[15].text = "candidate profile must not inject".into();
    mems[16].mtype = T_SKILL.into();
    mems[16].status = S_CANDIDATE.into();
    mems[16].text = "derived safety skill".into();
    mems[16].used_memory_ids = vec!["m01".into()];
    let turns: Vec<TurnRec> = [
        (
            "T1",
            "p1",
            "deployment service port 8100",
            vec!["m01".to_string()],
        ),
        ("T2", "p1", "bufferonly recently repaired printer", vec![]),
        ("T2", "p2", "secretproject confidential alpha", vec![]),
    ]
    .into_iter()
    .map(|(id, project, text, derived)| TurnRec {
        key: turn_key(
            if project == "p1" {
                "codex"
            } else {
                "claude-code"
            },
            id,
        ),
        turn_id: id.into(),
        platform: if project == "p1" {
            "codex"
        } else {
            "claude-code"
        }
        .into(),
        project: project.into(),
        session: if project == "p1" { "s1" } else { "s2" }.into(),
        user_message: text.into(),
        assistant_message: "acknowledged".into(),
        digest: text.into(),
        user_ts: now.clone(),
        assistant_ts: now.clone(),
        started_at: now.clone(),
        ended_at: now.clone(),
        recorded_at: now.clone(),
        extracted_at: Some(now.clone()),
        extract_state: "done".into(),
        seq: 1,
        derived_memory_ids: derived,
        ..Default::default()
    })
    .collect();
    let sessions = vec![SessionRec {
        key: session_key("codex", "s1"),
        platform: "codex".into(),
        project: "p1".into(),
        session: "s1".into(),
        goal: "deployment task".into(),
        turn_count: 2,
        updated_at: now.clone(),
        last_turn_at: now.clone(),
        ..Default::default()
    }];
    let evidence = vec![
        EvidenceRec {
            context: None,
            id: "eport".into(),
            memory_ids: vec!["m01".into()],
            platform: "codex".into(),
            turn_id: "T1".into(),
            role: "user".into(),
            span: "原始出处".repeat(100),
            stated_at: now.clone(),
            created_at: now.clone(),
        },
        EvidenceRec {
            context: None,
            id: "eshared".into(),
            memory_ids: vec!["m02".into(), "m03".into()],
            platform: "codex".into(),
            turn_id: "T1".into(),
            role: "user".into(),
            span: "shared region evidence".into(),
            stated_at: now.clone(),
            created_at: now.clone(),
        },
    ];
    let seeded = manage::seed(&eng, &t, json!({"memories":mems, "turns":turns, "sessions":sessions,
        "evidence":evidence, "entities":[{"id":"eservice", "name":"deployment service", "aliases":["service"], "scope":{"kind":"project", "project":"p1"}}]})).await.unwrap();
    assert_eq!(seeded["memories"], 30);
    let search_req = |query: &str| SearchReq {
        query: query.into(),
        scope: Some(scope.clone()),
        ..Default::default()
    };
    let ids = |r: &serde_json::Value| {
        r["results"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|x| {
                x.pointer("/memory/id")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })
            .collect::<Vec<String>>()
    };
    let port = retrieve::search_memories(
        &eng,
        &t,
        &c,
        SearchReq {
            include_history: Some(true),
            include_evidence: Some(true),
            ..search_req("deployment service port 8100")
        },
    )
    .await
    .unwrap();
    assert!(ids(&port).contains(&"m01".into()));
    let p = port["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["memory"]["id"] == "m01")
        .unwrap();
    assert_eq!(p["history"][0]["id"], "m00");
    assert!(p["evidence"][0]["span"].as_str().unwrap().chars().count() <= 300);
    assert!(!port["recentTurns"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v["turnId"] == "T1"));
    let conflict = retrieve::search_memories(
        &eng,
        &t,
        &c,
        SearchReq {
            top_k: Some(1),
            ..search_req("deployment region")
        },
    )
    .await
    .unwrap();
    assert!(ids(&conflict).contains(&"m02".into()) && ids(&conflict).contains(&"m03".into()));
    assert_eq!(
        conflict["results"][0]["slot"],
        conflict["results"][1]["slot"]
    );
    let candidate = retrieve::search_memories(&eng, &t, &c, search_req("candidatefact"))
        .await
        .unwrap();
    assert!(ids(&candidate).contains(&"m04".into()));
    let private = retrieve::search_memories(&eng, &t, &c, search_req("secretproject"))
        .await
        .unwrap();
    assert!(ids(&private).is_empty() && private["recentTurns"].as_array().unwrap().is_empty());
    let cross =
        retrieve::search_memories(&eng, &t, &c, search_req("crossscope public predecessor"))
            .await
            .unwrap();
    assert!(ids(&cross).contains(&"m14".into()));
    assert!(!ids(&cross).contains(&"m13".into()));
    let buffer = retrieve::search_memories(&eng, &t, &c, search_req("bufferonly"))
        .await
        .unwrap();
    assert_eq!(buffer["recentTurns"][0]["turnId"], "T2");
    let log_count = t.store.count("retrieval_log", None).await.unwrap();
    let access = t
        .store
        .get_memory("m01")
        .await
        .unwrap()
        .unwrap()
        .last_accessed_at;
    retrieve::search_memories(
        &eng,
        &t,
        &c,
        SearchReq {
            touch: Some(false),
            ..search_req("port 8100")
        },
    )
    .await
    .unwrap();
    assert_eq!(
        t.store.count("retrieval_log", None).await.unwrap(),
        log_count
    );
    assert_eq!(
        t.store
            .get_memory("m01")
            .await
            .unwrap()
            .unwrap()
            .last_accessed_at,
        access
    );
    for budget in [1, 50, 1500] {
        let context = retrieve::get_context(
            &eng,
            &t,
            &c,
            GetContextReq {
                scope: scope.clone(),
                token_budget: Some(budget),
            },
        )
        .await
        .unwrap();
        assert!(util::est_tokens(context["text"].as_str().unwrap()) <= budget);
        assert_eq!(
            context["tokensUsed"].as_u64().unwrap() as usize,
            util::est_tokens(context["text"].as_str().unwrap())
        );
        assert!(!context["text"]
            .as_str()
            .unwrap()
            .contains("candidate profile"));
        assert!(!context["text"]
            .as_str()
            .unwrap()
            .contains("assistant speculative"));
        assert!(!context["text"]
            .as_str()
            .unwrap()
            .contains("secret full steps"));
    }
    let listed = retrieve::list_turns(
        &eng,
        &t,
        &c,
        ListTurnsReq {
            scope: scope.clone(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(listed["sessions"][0]["session"], "s1");
    let raw = retrieve::list_turns(
        &eng,
        &t,
        &c,
        ListTurnsReq {
            scope: scope.clone(),
            turn_ids: Some(vec!["T2".into()]),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(raw["turns"].as_array().unwrap().len(), 1);
    assert_eq!(raw["turns"][0]["project"], "p1");
    let history = retrieve::get_memory(
        &eng,
        &t,
        GetMemoryReq {
            id: "m00".into(),
            include_history: Some(true),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(history["history"].as_array().unwrap().len(), 2);
    assert_eq!(history["history"][0]["validUntil"], now);
    for n in 1..=3 {
        let r = retrieve::search_memories(&eng, &t, &c, search_req("candidateskill"))
            .await
            .unwrap();
        assert!(ids(&r).contains(&"m11".into()));
        let req = FeedbackReq {
            retrieval_id: r["retrievalId"].as_str().unwrap().into(),
            used: vec!["m11".into(), "m11".into()],
            outcome: "helpful".into(),
        };
        let (a, b) = tokio::join!(
            manage::feedback(&eng, &t, req.clone()),
            manage::feedback(&eng, &t, req)
        );
        assert_ne!(a.unwrap()["accepted"], b.unwrap()["accepted"]);
        assert_eq!(
            t.store
                .get_memory("m11")
                .await
                .unwrap()
                .unwrap()
                .helpful_count,
            n
        );
    }
    assert_eq!(
        t.store.get_memory("m11").await.unwrap().unwrap().status,
        S_CURRENT
    );
    let r = retrieve::search_memories(&eng, &t, &c, search_req("candidateskill"))
        .await
        .unwrap();
    manage::feedback(
        &eng,
        &t,
        FeedbackReq {
            retrieval_id: r["retrievalId"].as_str().unwrap().into(),
            used: vec!["m11".into()],
            outcome: "wrong".into(),
        },
    )
    .await
    .unwrap();
    let skill = t.store.get_memory("m11").await.unwrap().unwrap();
    assert_eq!(skill.status, S_CANDIDATE);
    assert_eq!(skill.helpful_count, 0);
    let expired = t.store.log("retrieval_log", json!({"kind":"search", "createdAt":(util::now()-chrono::Duration::hours(25)).to_rfc3339(),
        "returned":{"memories":[{"id":"m04"}]}})).await.unwrap();
    assert_eq!(
        manage::feedback(
            &eng,
            &t,
            FeedbackReq {
                retrieval_id: expired,
                used: vec!["m04".into()],
                outcome: "helpful".into()
            }
        )
        .await
        .unwrap()["accepted"],
        false
    );
    let corrected = manage::correct_memory(
        &eng,
        &t,
        &c,
        CorrectMemoryReq {
            id: "m20".into(),
            text: Some("corrected deployment value".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let new_id = corrected["memory"]["id"].as_str().unwrap().to_string();
    assert_eq!(corrected["memory"]["source"]["origin"], ORIGIN_EXPLICIT);
    assert!(
        matches!(manage::correct_memory(&eng, &t, &c, CorrectMemoryReq { id:"m20".into(), ..Default::default() }).await, Err(ApiError::NotCurrent(id)) if id == new_id)
    );
    assert!(matches!(
        manage::correct_memory(
            &eng,
            &t,
            &c,
            CorrectMemoryReq {
                id: "m04".into(),
                ..Default::default()
            }
        )
        .await,
        Err(ApiError::NotCurrent(_))
    ));
    assert!(matches!(
        manage::correct_memory(
            &eng,
            &t,
            &c,
            CorrectMemoryReq {
                id: "m23".into(),
                text: Some(" ".into()),
                ..Default::default()
            }
        )
        .await,
        Err(ApiError::InvalidArgument(_))
    ));
    manage::correct_memory(
        &eng,
        &t,
        &c,
        CorrectMemoryReq {
            id: "m21".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        t.store.get_memory("m21").await.unwrap().unwrap().status,
        S_SUPERSEDED
    );
    let deletion = manage::delete_memory(&eng, &t, IdReq { id: "m02".into() })
        .await
        .unwrap();
    assert_eq!(deletion["evidenceUpdated"], 1);
    assert_eq!(
        t.store.get_memory("m03").await.unwrap().unwrap().status,
        S_CURRENT
    );
    let deleted = manage::delete_memory(&eng, &t, IdReq { id: "m01".into() })
        .await
        .unwrap();
    assert_eq!(deleted["derivedDeleted"], json!(["m16"]));
    assert!(t.store.get_memory("m16").await.unwrap().is_none());
    assert!(t
        .store
        .get_doc("evidence", "eport")
        .await
        .unwrap()
        .is_none());
    assert!(t
        .store
        .get_memory("m00")
        .await
        .unwrap()
        .unwrap()
        .superseded_by
        .is_none());
    assert!(!t.vecs.read().await.memories.contains_key("m01"));
    indexer::drain(&eng, &t).await.unwrap();
    assert_eq!(
        manage::export(&eng, &t, json!({})).await.unwrap()["count"],
        28
    );
    assert!(
        manage::stats(&eng, &t).await.unwrap()["counts"]["retrieval_log"]
            .as_i64()
            .unwrap()
            > 0
    );
    // Pure logic remains covered by selftest without rebuilding main-crate test dependencies.
    assert_eq!(
        retrieve::mmr_order(
            &[1.0, 0.98, 0.9],
            &|i, j| if (i == 0 && j == 1) || (i == 1 && j == 0) {
                0.99
            } else {
                0.1
            },
            0.7
        ),
        vec![0, 2, 1]
    );
    let mut used = 0;
    assert_eq!(
        retrieve::pack(&[50, 4, 3], 7, &mut used),
        vec![false, true, true]
    );
    assert!((retrieve::evidence_mult(8) - 1.15).abs() < 1e-10);
    let reference = chrono::DateTime::parse_from_rfc3339("2026-10-09T10:00:00+08:00").unwrap();
    for (q, start, end) in [
        ("last Saturday", "2026-10-03", "2026-10-04"),
        ("two weeks ago", "2026-09-21", "2026-09-28"),
        ("in May 2023", "2023-05-01", "2023-06-01"),
        ("on 8 May 2023", "2023-05-08", "2023-05-09"),
        ("yesterday", "2026-10-08", "2026-10-09"),
        ("last year", "2025-01-01", "2026-01-01"),
        ("recently", "2026-09-26", "2026-10-10"),
    ] {
        let window =
            timeparse::parse(q, reference).unwrap_or_else(|| panic!("time parsing failed: {q}"));
        assert_eq!(
            window
                .start
                .with_timezone(reference.offset())
                .date_naive()
                .to_string(),
            start
        );
        assert_eq!(
            window
                .end
                .with_timezone(reference.offset())
                .date_naive()
                .to_string(),
            end
        );
    }
    assert!(timeparse::history_intent("previously used to"));
    assert_eq!(eng.llm.usage_snapshot()["calls"], 0);
    println!("retrieval_roundtrip OK: 30 memories; scope/history/conflict/budget/recent/context/correction/deletion/feedback; zero LLM calls; data {}", dir.display());
}

/// Long turns: the buffer stores complete messages, indexes their windows, and search returns the
/// window that matched; short turns keep their exact digest.
pub async fn long_turn_roundtrip() {
    use super::{
        config::ServiceConfig, engine::Engine, indexer, ingest, llm::Llm, models::ModelHub,
        retrieve, tenant::Tenant, util,
    };
    use std::sync::Arc;
    let dir = std::env::temp_dir().join(format!(
        "memory-long-turn-selftest-{}-{}",
        std::process::id(),
        util::new_id("t")
    ));
    let cfg = ServiceConfig {
        memory_strategy: super::config::MemoryStrategy::Current,
        data_dir: dir.clone(),
        ..Default::default()
    };
    let (digest_max, formation_max) = (cfg.digest_max_bytes, cfg.max_message_bytes);
    let llm = Llm::new("http://127.0.0.1:1", "", "disabled", 1, 1).unwrap();
    let models = ModelHub::mock();
    let eng = Engine::new(cfg, llm, models);
    let t = Arc::new(
        Tenant::open("long-turn-selftest", &dir, eng.models.dim)
            .await
            .unwrap(),
    );
    let c = Caller {
        tenant: t.name.clone(),
        platform: "codex".into(),
    };
    let scope = ReqScope {
        project: Some("p1".into()),
        session: Some("s1".into()),
        ..Default::default()
    };
    let now = util::now_str();
    let add = |turn_id: &str, user: &str, assistant: &str| AddMemoriesReq {
        scope: scope.clone(),
        turn_id: Some(turn_id.into()),
        messages: Some(vec![
            InMessage {
                role: "user".into(),
                content: user.into(),
                timestamp: now.clone(),
            },
            InMessage {
                role: "assistant".into(),
                content: assistant.into(),
                timestamp: now.clone(),
            },
        ]),
        ..Default::default()
    };
    // A build log far beyond the formation view, whose only failure sits in the middle.
    let steps = |r: std::ops::Range<usize>| {
        r.map(|i| format!("step {i} compiled module ok\n"))
            .collect::<String>()
    };
    let needle = "error: linker failed for module zephyrquartz";
    let log = format!("{}{needle}\n{}", steps(0..600), steps(600..1200));
    ingest::add_memories(&eng, &t, &c, add("L1", "run the full build", &log))
        .await
        .unwrap();
    ingest::add_memories(&eng, &t, &c, add("S1", "which tea do I like", "green tea"))
        .await
        .unwrap();

    let long = t.store.get_turn("codex", "L1").await.unwrap().unwrap();
    assert_eq!(
        long.assistant_message, log,
        "the buffer stores complete messages"
    );
    assert!(long.digest.len() <= digest_max && !long.digest_complete(digest_max));
    assert!(long.chunks.iter().all(|ch| ch.role == "assistant"));
    assert_eq!(long.chunks.first().map(|ch| ch.start), Some(0));
    assert_eq!(long.chunks.last().map(|ch| ch.end), Some(log.len()));
    assert!(
        !long
            .clipped(formation_max)
            .assistant_message
            .contains(needle),
        "formation keeps its bounded head/tail view"
    );
    let short = t.store.get_turn("codex", "S1").await.unwrap().unwrap();
    assert!(short.chunks.is_empty() && short.digest_complete(digest_max));
    assert_eq!(short.digest, "which tea do I like\ngreen tea");

    indexer::drain(&eng, &t).await.unwrap();
    let rid = turn_rid("codex", "L1");
    let stored = t.store.load_chunk_embeddings().await.unwrap();
    assert_eq!(stored.len(), 1, "only the long turn has chunk vectors");
    assert_eq!(stored[0].0, rid);
    assert_eq!(stored[0].1.len(), long.chunks.len());
    // A later re-index (e.g. after extraction updates derived text) keeps the window vectors.
    let mut tx = Tx::new();
    tx.index_task("turns", &rid, "upsert").unwrap();
    t.store.commit(tx).await.unwrap();
    indexer::drain(&eng, &t).await.unwrap();
    assert_eq!(
        t.vecs.read().await.turn_chunks[&rid].len(),
        long.chunks.len()
    );

    let search = |query: &str| SearchReq {
        query: query.into(),
        scope: Some(scope.clone()),
        touch: Some(false),
        ..Default::default()
    };
    let turn = |r: &serde_json::Value, id: &str| {
        r["recentTurns"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["turnId"] == id)
            .cloned()
            .unwrap_or_else(|| panic!("{id} not recalled: {r}"))
    };
    let r = retrieve::search_memories(&eng, &t, &c, search("zephyrquartz linker failed"))
        .await
        .unwrap();
    let hit = turn(&r, "L1");
    let view = hit["digest"].as_str().unwrap();
    assert!(view.contains("zephyrquartz"), "window view: {view}");
    assert!(view.starts_with("run the full build\n…"));
    assert!(view.len() <= digest_max + 2 * "…".len());
    assert_eq!(hit["complete"], false);
    assert_eq!(hit["excerpt"]["role"], "assistant");
    let r = retrieve::search_memories(&eng, &t, &c, search("which tea do I like"))
        .await
        .unwrap();
    let hit = turn(&r, "S1");
    assert_eq!(hit["digest"], "which tea do I like\ngreen tea");
    assert_eq!(hit["complete"], true);
    assert!(hit.get("excerpt").is_none());
    println!(
        "long_turn_roundtrip OK: {} bytes stored whole; {} windows indexed and recalled; short digest unchanged; zero LLM calls; data {}",
        log.len(),
        long.chunks.len(),
        dir.display()
    );
}
