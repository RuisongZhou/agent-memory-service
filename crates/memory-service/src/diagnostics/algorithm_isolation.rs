//! Offline checks that the shared entry points preserve each algorithm's lifecycle.
use crate::{
    algorithms,
    config::{MemoryStrategy, ServiceConfig},
    engine::Engine,
    ingest,
    llm::Llm,
    models::ModelHub,
    retrieve, store,
    tenant::Tenant,
    types::*,
    util,
};
use serde_json::json;
use std::sync::Arc;

fn engine(strategy: MemoryStrategy) -> Arc<Engine> {
    Engine::new(
        ServiceConfig {
            memory_strategy: strategy,
            ..Default::default()
        },
        Llm::new("http://127.0.0.1:1", "", "disabled", 1, 1).unwrap(),
        ModelHub::mock(),
    )
}

pub async fn selftest() {
    assert_eq!(std::env::var("MEMORY_MOCK_MODELS").as_deref(), Ok("1"));
    for strategy in [MemoryStrategy::Mem0, MemoryStrategy::Memos] {
        let root = std::env::temp_dir().join(util::new_id("algorithm-isolation-"));
        let eng = engine(strategy);
        // Workers are deliberately not started: each pending input can be inspected before formation.
        let tenant = Arc::new(Tenant::open("test", &root, eng.models.dim).await.unwrap());
        let caller = Caller {
            tenant: "test".into(),
            platform: "test".into(),
        };
        let old_session = SessionRec {
            key: session_key("test", "old"),
            platform: "test".into(),
            project: "project".into(),
            session: "old".into(),
            ..Default::default()
        };
        tenant
            .store
            .put_doc("sessions", &store::session_rid("test", "old"), &old_session)
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
        let receipt = ingest::add_memories(
            &eng,
            &tenant,
            &caller,
            serde_json::from_value(payload).unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(receipt["status"], "queued");
        assert_eq!(tenant.store.count("memories", None).await.unwrap(), 0);

        match strategy {
            MemoryStrategy::Mem0 => {
                assert_eq!(tenant.store.count("turns", None).await.unwrap(), 0);
                assert_eq!(
                    ingest::wait_idle(&eng, &tenant).await.unwrap()["status"],
                    "idle"
                );
                assert_eq!(tenant.store.count("memories", None).await.unwrap(), 1);
                assert!(matches!(
                    algorithms::run_jobs(
                        &eng,
                        &tenant,
                        &caller,
                        json!({"jobs":["profile","skills"]})
                    )
                    .await,
                    Err(ApiError::InvalidArgument(_))
                ));
            }
            MemoryStrategy::Memos => {
                assert_eq!(
                    tenant
                        .store
                        .count("turns", Some("extractState = 'pending'"))
                        .await
                        .unwrap(),
                    1
                );
            }
        }

        // Bootstrap can close buffered sessions, but cannot alter them when Mem0 is selected.
        retrieve::get_context(
            &eng,
            &tenant,
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
        let session = tenant
            .store
            .get_session("test", "old")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            session.ended_at.is_some(),
            strategy == MemoryStrategy::Memos
        );

        // Explicit instructions and record reads remain common to both algorithms.
        let instruction: AddMemoriesReq = serde_json::from_value(json!({
            "scope":{"project":"project","session":"new"}, "turnId":"instruction",
            "memoryType":"instruction", "text":"Always include source IDs."
        }))
        .unwrap();
        let saved = ingest::add_memories(&eng, &tenant, &caller, instruction)
            .await
            .unwrap();
        assert_eq!(saved["status"], "completed");
        assert_eq!(
            tenant
                .store
                .count("memories", Some("type = 'instruction'"))
                .await
                .unwrap(),
            1
        );
        assert_eq!(tenant.store.count("llm_responses", None).await.unwrap(), 0);
        assert_eq!(tenant.store.count("llm_usage", None).await.unwrap(), 0);
        drop(tenant);
        std::fs::remove_dir_all(root).unwrap();
    }
    println!("algorithm_isolation OK: startup-selected input jobs and session lifecycle; shared instructions; zero LLM calls");
}
