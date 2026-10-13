//! `agent-memoryd selftest`: storage, retrieval and long-turn round trips (kept out of `cargo test` so
//! the main crate does not need a second dependency build with dev-dependency features).
use super::{engine, memory, tenant};
use memory_config::{MemoryStrategy, ServiceConfig};
use memory_domain::types::*;
use memory_engine::app::{ingest, management};
use memory_engine::retrieval;
use memory_engine::runtime::indexer;
use memory_rules::util;
use memory_store::schema;
use memory_store::store::*;
use serde_json::{json, Value};

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

/// Mock-model integration coverage for retrieval and management, including primary storage and indexes.
pub async fn retrieval_roundtrip() {
    let eng = engine(ServiceConfig {
        memory_strategy: MemoryStrategy::Memos,
        ..Default::default()
    });
    let (t, c, dir) = tenant(&eng, "retrieval-selftest", "codex").await;
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
    mems[5].text = "secretproject confidential alpha".into();
    mems[5].scope = MemScope::project("p2");
    // Assistant-reported results are facts like any other: they reach "recent changes" after the
    // project notes, and can be one side of a conflict.
    let later = util::fmt_ts(&(util::now() + chrono::Duration::minutes(1)));
    mems[3].source.origin = ORIGIN_ASSISTANT.into();
    mems[8].mtype = T_PROFILE.into();
    mems[8].text = "current project notes".into();
    mems[8].recorded_at = "2026-01-01T00:00:00Z".into();
    mems[10].text = "assistant reported deployment finished".into();
    mems[10].source.origin = ORIGIN_ASSISTANT.into();
    for i in [2, 3, 10] {
        mems[i].recorded_at = later.clone();
    }
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
    .map(|(id, project, text, derived)| {
        let (platform, session) = if project == "p1" {
            ("codex", "s1")
        } else {
            ("claude-code", "s2")
        };
        TurnRec {
            key: turn_key(platform, id),
            turn_id: id.into(),
            platform: platform.into(),
            project: project.into(),
            session: session.into(),
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
        }
    })
    .collect();
    let evidence = |id: &str, memory_ids: &[&str], span: String| EvidenceRec {
        context: None,
        id: id.into(),
        memory_ids: memory_ids.iter().map(|m| m.to_string()).collect(),
        platform: "codex".into(),
        turn_id: "T1".into(),
        role: "user".into(),
        span,
        stated_at: now.clone(),
        created_at: now.clone(),
    };
    let evidence = vec![
        evidence("eport", &["m01"], "原始出处".repeat(100)),
        evidence("eshared", &["m02", "m03"], "shared region evidence".into()),
    ];
    management::seed(&eng, &t, json!({"memories":mems, "turns":turns, "evidence":evidence,
        "entities":[{"id":"eservice", "name":"deployment service", "aliases":["service"], "scope":{"kind":"project", "project":"p1"}}]}))
        .await
        .unwrap();
    let search = |query: &str| SearchReq {
        query: query.into(),
        scope: Some(scope.clone()),
        ..Default::default()
    };
    let ids = |r: &Value| -> Vec<String> {
        r["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["memory"]["id"].as_str().unwrap().to_string())
            .collect()
    };
    let port = eng
        .search_memories(
            &t,
            &c,
            SearchReq {
                include_history: Some(true),
                include_evidence: Some(true),
                ..search("deployment service port 8100")
            },
        )
        .await
        .unwrap();
    let hit = port["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["memory"]["id"] == "m01")
        .unwrap();
    assert_eq!(hit["history"][0]["id"], "m00");
    assert!(hit["evidence"][0]["span"].as_str().unwrap().chars().count() <= 300);
    // A turn already formed into memories is not repeated as a recent turn.
    assert!(!port["recentTurns"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v["turnId"] == "T1"));
    // Both sides of a conflict come back together, even with top_k = 1.
    let conflict = eng
        .search_memories(
            &t,
            &c,
            SearchReq {
                top_k: Some(1),
                ..search("deployment region")
            },
        )
        .await
        .unwrap();
    assert!(ids(&conflict).contains(&"m02".into()) && ids(&conflict).contains(&"m03".into()));
    assert_eq!(
        conflict["results"][0]["slot"],
        conflict["results"][1]["slot"]
    );
    // Other projects stay invisible, including the private successor of a visible memory.
    let private = eng
        .search_memories(&t, &c, search("secretproject"))
        .await
        .unwrap();
    assert!(ids(&private).is_empty() && private["recentTurns"].as_array().unwrap().is_empty());
    let cross = eng
        .search_memories(&t, &c, search("crossscope public predecessor"))
        .await
        .unwrap();
    assert!(ids(&cross).contains(&"m14".into()) && !ids(&cross).contains(&"m13".into()));
    let buffer = eng
        .search_memories(&t, &c, search("bufferonly"))
        .await
        .unwrap();
    assert_eq!(buffer["recentTurns"][0]["turnId"], "T2");
    // touch = false leaves neither a retrieval log nor an access time.
    let log_count = t.store.count(schema::RETRIEVAL_LOG, None).await.unwrap();
    let accessed = memory(&t, "m01").await.last_accessed_at;
    eng.search_memories(
        &t,
        &c,
        SearchReq {
            touch: Some(false),
            ..search("port 8100")
        },
    )
    .await
    .unwrap();
    assert_eq!(
        t.store.count(schema::RETRIEVAL_LOG, None).await.unwrap(),
        log_count
    );
    assert_eq!(memory(&t, "m01").await.last_accessed_at, accessed);
    // Context respects the budget and leaves out candidates and skill steps.
    let mut text = String::new();
    for budget in [50, 1500] {
        let context = retrieval::get_context(
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
        text = context["text"].as_str().unwrap().to_string();
        assert!(util::est_tokens(&text) <= budget);
        assert!(!text.contains("candidate profile") && !text.contains("secret full steps"));
    }
    assert!(text.contains("assistant reported deployment finished"));
    let conflict_line = text
        .lines()
        .find(|l| l.contains("deployment region"))
        .unwrap();
    assert!(conflict_line.contains("east") && conflict_line.contains("west"));
    let raw = eng
        .list_turns(
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
    let history = retrieval::get_memory(
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
    // Three helpful uses enable a candidate skill; a wrong use demotes it again.
    for (outcome, status) in [
        ("helpful", S_CANDIDATE),
        ("helpful", S_CANDIDATE),
        ("helpful", S_CURRENT),
        ("wrong", S_CANDIDATE),
    ] {
        let r = eng
            .search_memories(&t, &c, search("candidateskill"))
            .await
            .unwrap();
        management::feedback(
            &eng,
            &t,
            FeedbackReq {
                retrieval_id: r["retrievalId"].as_str().unwrap().into(),
                used: vec!["m11".into()],
                outcome: outcome.into(),
            },
        )
        .await
        .unwrap();
        assert_eq!(memory(&t, "m11").await.status, status);
    }
    // Corrections write an explicit successor; a correction without text retracts.
    let corrected = management::correct_memory(
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
    assert_eq!(corrected["memory"]["source"]["origin"], ORIGIN_EXPLICIT);
    assert_eq!(memory(&t, "m20").await.status, S_SUPERSEDED);
    management::correct_memory(
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
    assert_eq!(memory(&t, "m21").await.status, S_SUPERSEDED);
    // Deleting one side of a conflict restores the other; deleting a source removes what derives from it.
    let deletion = management::delete_memory(&eng, &t, IdReq { id: "m02".into() })
        .await
        .unwrap();
    assert_eq!(deletion["evidenceUpdated"], 1);
    assert_eq!(memory(&t, "m03").await.status, S_CURRENT);
    let deleted = management::delete_memory(&eng, &t, IdReq { id: "m01".into() })
        .await
        .unwrap();
    assert_eq!(deleted["derivedDeleted"], json!(["m16"]));
    assert!(t
        .store
        .get_doc(schema::EVIDENCE, "eport")
        .await
        .unwrap()
        .is_none());
    assert!(memory(&t, "m00").await.superseded_by.is_none());
    assert!(!t.vecs.read().await.memories.contains_key("m01"));
    indexer::drain(&eng, &t).await.unwrap();
    assert_eq!(
        management::export(&eng, &t, json!({})).await.unwrap()["count"],
        28
    );
    assert_eq!(eng.llm.usage_snapshot()["calls"], 0);
    println!("retrieval_roundtrip OK: scope/history/conflict/budget/recent/context/feedback/correction/deletion; zero LLM calls; data {}", dir.display());
}

/// Long turns: the buffer stores complete messages, indexes their windows, and search returns the
/// window that matched; short turns keep their exact digest.
pub async fn long_turn_roundtrip() {
    let eng = engine(ServiceConfig {
        memory_strategy: MemoryStrategy::Memos,
        ..Default::default()
    });
    let (digest_max, formation_max) = (eng.cfg.digest_max_bytes, eng.cfg.max_message_bytes);
    let (t, c, dir) = tenant(&eng, "long-turn-selftest", "codex").await;
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
    assert_eq!(long.assistant_message, log);
    assert!(!long.digest_complete(digest_max));
    assert_eq!(long.chunks.last().map(|ch| ch.end), Some(log.len()));
    // Formation keeps its bounded head/tail view.
    assert!(!long
        .clipped(formation_max)
        .assistant_message
        .contains(needle));
    indexer::drain(&eng, &t).await.unwrap();
    let search = |query: &str| SearchReq {
        query: query.into(),
        scope: Some(scope.clone()),
        touch: Some(false),
        ..Default::default()
    };
    let turn = |r: &Value, id: &str| {
        r["recentTurns"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["turnId"] == id)
            .cloned()
            .unwrap()
    };
    let r = eng
        .search_memories(&t, &c, search("zephyrquartz linker failed"))
        .await
        .unwrap();
    let hit = turn(&r, "L1");
    let view = hit["digest"].as_str().unwrap();
    assert!(view.contains("zephyrquartz") && view.starts_with("run the full build\n…"));
    assert_eq!(hit["complete"], false);
    assert_eq!(hit["excerpt"]["role"], "assistant");
    let r = eng
        .search_memories(&t, &c, search("which tea do I like"))
        .await
        .unwrap();
    let hit = turn(&r, "S1");
    assert_eq!(hit["digest"], "which tea do I like\ngreen tea");
    assert_eq!(hit["complete"], true);
    println!(
        "long_turn_roundtrip OK: {} bytes stored whole; matching window recalled; short digest unchanged; zero LLM calls; data {}",
        log.len(),
        dir.display()
    );
}
