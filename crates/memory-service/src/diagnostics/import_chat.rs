//! An import a person drives in a conversation (docs/design/memory-import-mcp.md), end to end against the real
//! daemon: the MCP shim's `propose_import` pages and chat-confirmed `apply_import` go through the import client to
//! the HTTP router of an engine on a temporary store, with mock embeddings and the local generation stub. No
//! external model call.
use super::consolidation::generation_stub;
use super::stub_engine;
use crate::harness::import::chat::{self, Page, VIA};
use crate::harness::import::client::Client;
use memory_config::ServiceConfig;
use memory_domain::import::{is_session_turn, CONFIRMED_CHAT, PROPOSED, RUN_APPLIED};
use memory_domain::types::ORIGIN_IMPORTED;
use serde_json::json;
use std::path::Path;
use std::time::Duration;

const PROJECT: &str = "atlas";
const PLATFORM: &str = "claude-code";
const NOMAD: &str = "Atlas deploys with Nomad.";
const WIKI: &str = "Atlas docs live in the team wiki.";
const STAGING: &str = "The atlas staging cluster lives in eu-west.";

fn line(text: &str, kind: &str) -> String {
    json!({"text": text, "kind": kind, "date": "2026-05-01", "source": "conversation"}).to_string()
}

fn page<'a>(export: &'a str, run_id: Option<&'a str>, finish: bool) -> Page<'a> {
    Page {
        project: PROJECT,
        platform: PLATFORM,
        source: Some("chatgpt"),
        export,
        run_id,
        finish,
    }
}

pub async fn chat_import() {
    let generation = generation_stub().await;
    let dir = std::env::temp_dir().join(memory_rules::util::new_id("memory-import-chat-"));
    let cfg = ServiceConfig {
        data_dir: dir.clone(),
        ..Default::default()
    };
    let eng = stub_engine(cfg, &generation);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let router = crate::http::router(eng.clone());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = Client::new(&url, Duration::from_secs(60)).unwrap();
    let home = Path::new("/home/u");

    // ---- two pages: the second repeats a line of the first, and has one line over the unit cap to extract
    let first = format!(
        "```json\n{}\n{}\n```",
        line(NOMAD, "fact"),
        line(WIKI, "reference")
    );
    let report = chat::propose_page(&client, &page(&first, None, false), home)
        .await
        .unwrap();
    assert!(report.contains("proposing"), "{report}");
    let run_id = client.runs(Some(PROJECT), 1).await.unwrap()[0]
        .run_id
        .clone();
    let run_id = run_id.as_str();
    let notes = format!(
        "Atlas operations notes. {}\n- {STAGING}",
        "The atlas team keeps a long history of operations work. ".repeat(16)
    );
    let second = format!("{}\n{}", line(NOMAD, "fact"), line(&notes, "fact"));
    let report = chat::propose_page(&client, &page(&second, Some(run_id), true), home)
        .await
        .unwrap();
    assert!(report.contains("重复 1 行"), "{report}");
    let view = client.run(run_id).await.unwrap();
    let waiting = view
        .proposals
        .iter()
        .filter(|p| p.status == PROPOSED && !is_session_turn(p))
        .count();
    assert!(
        report.contains(&format!("expectedProposals 填 {waiting}")),
        "{report}"
    );
    let texts: Vec<&str> = view.proposals.iter().map(|p| p.text.as_str()).collect();
    for text in [NOMAD, WIKI, STAGING] {
        assert_eq!(
            texts.iter().filter(|t| **t == text).count(),
            1,
            "{text}: {texts:?}"
        );
    }
    assert!(view
        .proposals
        .iter()
        .all(|p| p.source["via"] == VIA && p.source.get("path").is_none()));

    // ---- the person agreed: apply as the conversation confirmed it, once
    let wrong = chat::apply_chat(&client, PLATFORM, run_id, waiting + 1)
        .await
        .unwrap_err();
    assert!(crate::harness::import::message(&wrong).contains("waiting for a decision"));
    let report = chat::apply_chat(&client, PLATFORM, run_id, waiting)
        .await
        .unwrap();
    assert!(report.contains(&format!("写入 {waiting} 条")), "{report}");
    let run = client.run(run_id).await.unwrap().run;
    assert_eq!(run.status, RUN_APPLIED);
    assert_eq!(run.confirmed_via.as_deref(), Some(CONFIRMED_CHAT));
    let again = chat::apply_chat(&client, PLATFORM, run_id, 0)
        .await
        .unwrap_err();
    assert!(crate::harness::import::message(&again).contains("is applied"));

    let t = eng.store().await.unwrap();
    let facts = t.store.current_project_facts(PROJECT).await.unwrap();
    for text in [NOMAD, WIKI, STAGING] {
        let fact = facts
            .iter()
            .find(|m| m.text == text)
            .unwrap_or_else(|| panic!("{text} was not written: {facts:?}"));
        assert_eq!(fact.source.origin, ORIGIN_IMPORTED);
        assert_eq!(fact.source.platform, PLATFORM);
        let source = &fact.detail.as_ref().unwrap()["import"]["source"];
        assert_eq!(source["via"], VIA);
        assert_eq!(source["exportedFrom"], "chatgpt");
        assert!(source.get("path").is_none(), "{source}");
    }
    drop(t);
    let _ = std::fs::remove_dir_all(dir);
    println!("import_chat_selftest OK: paged propose_import with a repeated and an extracted line, chat-confirmed apply_import, confirmedVia; real daemon router, local generation stub");
}

#[cfg(test)]
mod tests {
    #[tokio::test(flavor = "multi_thread")]
    async fn an_export_pasted_in_a_conversation_is_imported_through_the_real_daemon() {
        super::chat_import().await;
    }
}
