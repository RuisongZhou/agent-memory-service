//! Offline checks that the shared entry points preserve each algorithm's lifecycle.
use super::{engine, tenant};
use memory_config::{MemoryStrategy, ServiceConfig};
use memory_domain::types::*;
use memory_engine::app::ingest;
use memory_engine::retrieval;
use memory_store::schema;
use memory_store::store;
use serde_json::json;

pub async fn selftest() {
    for strategy in [MemoryStrategy::Mem0, MemoryStrategy::Memos] {
        let eng = engine(ServiceConfig {
            memory_strategy: strategy,
            ..Default::default()
        });
        // Workers are deliberately not started: each pending input can be inspected before formation.
        let (t, caller, _) = tenant(&eng, "isolation", "test").await;
        let old_session = SessionRec {
            key: session_key("test", "old"),
            platform: "test".into(),
            project: "project".into(),
            session: "old".into(),
            ..Default::default()
        };
        t.store
            .put_doc(
                schema::SESSIONS,
                &store::session_rid("test", "old"),
                &old_session,
            )
            .await
            .unwrap();
        let scope = json!({"project":"project", "session":"old"});
        let payload = match strategy {
            MemoryStrategy::Mem0 => json!({"scope":scope, "turnId":"input", "sync":false,
                "text":"database port 8765", "infer":false}),
            MemoryStrategy::Memos => json!({"scope":scope, "turnId":"input", "sync":false,
            "messages":[
                {"role":"user", "content":"database port 8765", "timestamp":"2026-10-12T00:00:00Z"},
                {"role":"assistant", "content":"recorded", "timestamp":"2026-10-12T00:00:01Z"}
            ]}),
        };
        let receipt =
            ingest::add_memories(&eng, &t, &caller, serde_json::from_value(payload).unwrap())
                .await
                .unwrap();
        assert_eq!(receipt["status"], "queued");
        // Mem0 queues the raw input itself; Memos buffers a turn for later formation.
        let pending_turns = t
            .store
            .count(schema::TURNS, Some("extractState = 'pending'"))
            .await
            .unwrap();
        assert_eq!(pending_turns, (strategy == MemoryStrategy::Memos) as i64);
        if strategy == MemoryStrategy::Mem0 {
            ingest::wait_idle(&eng, &t).await.unwrap();
            assert_eq!(t.store.count(schema::MEMORIES, None).await.unwrap(), 1);
        }

        // Bootstrapping a new session closes buffered sessions only under Memos.
        retrieval::get_context(
            &eng,
            &t,
            &caller,
            GetContextReq {
                scope: ReqScope {
                    project: Some("project".into()),
                    session: Some("new".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let session = t.store.get_session("test", "old").await.unwrap().unwrap();
        assert_eq!(
            session.ended_at.is_some(),
            strategy == MemoryStrategy::Memos
        );

        // Explicit instructions are common to both algorithms.
        let instruction: AddMemoriesReq = serde_json::from_value(json!({
            "scope":{"project":"project","session":"new"}, "turnId":"instruction",
            "memoryType":"instruction", "text":"Always include source IDs."
        }))
        .unwrap();
        let saved = ingest::add_memories(&eng, &t, &caller, instruction)
            .await
            .unwrap();
        assert_eq!(saved["status"], "completed");
        assert_eq!(eng.llm.usage_snapshot()["calls"], 0);
    }
    println!("algorithm_isolation OK: startup-selected input jobs and session lifecycle; shared instructions; zero LLM calls");
}
