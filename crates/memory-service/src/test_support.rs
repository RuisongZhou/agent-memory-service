//! Test fixtures: engines of the composed service on mock embeddings, a local generation stub and fresh
//! stores. No test makes an external model call.
use axum::routing::post;
use axum::{Json, Router};
use memory_config::ServiceConfig;
use memory_domain::types::{Caller, MemoryRec};
use memory_engine::runtime::{engine::Engine, instance::Instance, llm::Llm, models::ModelHub};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;

/// Engine with mock embeddings and an unreachable LLM, so a test can never make a model call.
pub(crate) fn engine(cfg: ServiceConfig) -> Arc<Engine> {
    engine_with(
        cfg,
        Llm::new("http://127.0.0.1:1", "", "disabled", 1, 1).unwrap(),
    )
}

/// Engine with mock embeddings whose generation calls go to [`generation_stub`] at `url`.
pub(crate) fn stub_engine(cfg: ServiceConfig, url: &str) -> Arc<Engine> {
    engine_with(cfg, Llm::new(url, "stub", "stub-model", 2, 10).unwrap())
}

fn engine_with(cfg: ServiceConfig, llm: Llm) -> Arc<Engine> {
    let strategy = cfg.memory_strategy;
    Engine::new(
        cfg,
        llm,
        ModelHub::mock(),
        crate::algorithms::select(strategy),
    )
}

/// Generation calls the engine has made.
pub(crate) fn llm_calls(eng: &Engine) -> u64 {
    eng.llm.usage_snapshot()["calls"].as_u64().unwrap()
}

/// A store in a fresh temporary directory, with a caller on `platform`.
pub(crate) async fn fresh_store(
    eng: &Engine,
    name: &str,
    platform: &str,
) -> (Arc<Instance>, Caller, PathBuf) {
    let dir = std::env::temp_dir().join(memory_rules::util::new_id(&format!("memory-{name}-")));
    let t = Instance::open(
        &dir,
        &eng.cfg.surreal_bin,
        eng.models.dim,
        eng.algorithm().schema(),
    )
    .await
    .unwrap();
    let caller = Caller {
        platform: platform.into(),
    };
    (Arc::new(t), caller, dir)
}

pub(crate) async fn memory(t: &Instance, id: &str) -> MemoryRec {
    t.store.get_memory(id).await.unwrap().unwrap()
}

/// The section of a prompt that follows `header`, up to the next section.
fn section<'a>(prompt: &'a str, header: &str) -> &'a str {
    let start = prompt
        .find(header)
        .map_or(prompt.len(), |i| i + header.len());
    let rest = &prompt[start..];
    &rest[..rest
        .find("\n\n## ")
        .or(rest.find("\n\n# "))
        .unwrap_or(rest.len())]
}

/// Formation: every user message line becomes one fact. Consolidation: a fixed decision per topic.
/// Import: every `- ` line of the note becomes one quoted memory; a `TRAP` line gains a number the note
/// does not contain, and an `Always` line is a preference.
fn reply(system: &str, prompt: &str) -> Value {
    if system.contains("Memory Importer") {
        let start = prompt
            .find("<note>\n")
            .map_or(prompt.len(), |i| i + "<note>\n".len());
        let end = prompt[start..]
            .find("\n</note>")
            .map_or(prompt.len(), |i| start + i);
        let memories: Vec<Value> = prompt[start..end]
            .lines()
            .filter_map(|line| line.strip_prefix("- "))
            .map(|line| {
                let text = if line.contains("TRAP") {
                    format!("{line} in 2031")
                } else {
                    line.to_string()
                };
                let kind = if line.starts_with("Always") {
                    "preference"
                } else {
                    "fact"
                };
                json!({"text": text, "kind": kind, "scope": "project", "evidence": line})
            })
            .collect();
        json!({ "memories": memories })
    } else if system.contains("Memory Consolidator") {
        let facts: Vec<Value> = serde_json::from_str(section(prompt, "## Facts\n")).unwrap();
        let ids: Vec<&Value> = facts.iter().map(|f| &f["id"]).collect();
        let texts: String = facts.iter().filter_map(|f| f["text"].as_str()).collect();
        if texts.contains("Arch Linux") {
            json!({"operations":[{"op":"merge","ids":ids,"text":"User uses Arch Linux on the work laptop"}]})
        } else if texts.contains("Jenkins") {
            // Members arrive in statement order: the January fact is "0".
            json!({"operations":[{"op":"supersede","old":"0","new":"1"}]})
        } else if texts.contains("Acme Corp") {
            json!({"operations":[{"op":"conflict","ids":ids,"reason":"manager differs"}]})
        } else if texts.contains("Postgres") {
            // Version 16 appears in no input: provenance validation rejects this merge.
            json!({"operations":[{"op":"merge","ids":ids,"text":"User runs Postgres 16 in production on AWS"}]})
        } else {
            json!({"operations":[{"op":"keep","ids":ids}]})
        }
    } else {
        let memory: Vec<Value> = section(prompt, "## New Messages\n")
            .lines()
            .filter_map(|line| line.strip_prefix("user: "))
            .enumerate()
            .map(|(i, text)| json!({"id":i.to_string(),"text":text,"attributed_to":"user"}))
            .collect();
        json!({ "memory": memory })
    }
}

/// An OpenAI-compatible chat-completions stub on a loopback port; returns its base URL.
pub(crate) async fn generation_stub() -> String {
    let app = Router::new().route(
        "/chat/completions",
        post(|Json(body): Json<Value>| async move {
            let system = body["messages"][0]["content"].as_str().unwrap_or_default();
            let prompt = body["messages"][1]["content"].as_str().unwrap_or_default();
            Json(json!({"id":"stub","choices":[{"message":{"content":reply(system, prompt).to_string()},
                "finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}
