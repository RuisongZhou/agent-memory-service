//! Session import end to end (docs/design/memory-import.md §6.10, §6.16), with the local generation stub and mock
//! embeddings: turns registered without a model call, admitted only as the run's applied turns, deduplicated
//! against live capture by `platform + turnId`, and rolled back fact by fact, keeping every fact another request
//! also formed. One part goes through two dialogue outboxes and the in-process uploader: a turn captured live
//! and then imported comes back `duplicate:true`. No external model call.
use super::consolidation::generation_stub;
use super::{fresh_store, llm_calls, memory};
use crate::harness::import::client::run_view;
use crate::harness::import::commands::{queue_turns, Pace, Queue};
use crate::harness::outbox::{drain_once, Outbox};
use crate::harness::uploader::InProcess;
use memory_config::ServiceConfig;
use memory_domain::import::*;
use memory_domain::types::*;
use memory_engine::app::{import, ingest};
use memory_engine::runtime::engine::Engine;
use memory_engine::runtime::instance::Instance;
use memory_rules::import::units::{content_hash, turn_key, unit_key};
use memory_store::schema;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

const PROJECT: &str = "atlas";
const HOST: &str = "claude-code";
const STAGING: &str = "Atlas releases go to the staging bucket.";
const CI: &str = "Atlas CI runs on Buildkite.";
const ONCALL: &str = "The atlas on-call rotation is weekly.";

fn message(role: &str, content: &str, at: &str) -> InMessage {
    InMessage {
        role: role.into(),
        content: content.into(),
        timestamp: at.into(),
    }
}

/// A session-turn unit: `prompt` (one fact per line, as the stub forms them) answered at `at`.
fn turn_unit(session: &str, native: &str, prompt: &str, at: &str) -> ImportUnit {
    ImportUnit {
        unit: unit_key(HOST, &format!("session:{session}"), native),
        host: HOST.into(),
        mode: MODE_SESSION_TURN.into(),
        text: prompt.into(),
        stated_at: at.into(),
        file_hash: content_hash(session),
        unit_hash: content_hash(prompt),
        source: json!({"via": "claude-transcript", "path": format!("~/.claude/projects/-atlas/{session}.jsonl")}),
        turn: Some(SessionTurn {
            turn_id: turn_key(PROJECT, session, native),
            session: session.into(),
            native_id: native.into(),
            messages: vec![
                message("user", prompt, at),
                message("assistant", "Noted.", at),
            ],
        }),
        ..Default::default()
    }
}

/// The batch the uploader submits for `unit`'s turn, naming `run_id` when given.
fn submission(unit: &ImportUnit, run_id: Option<&str>) -> AddMemoriesReq {
    let turn = unit.turn.clone().unwrap();
    AddMemoriesReq {
        scope: ReqScope {
            project: Some(PROJECT.into()),
            session: Some(turn.session.clone()),
            ..Default::default()
        },
        turn_id: Some(turn.turn_id.clone()),
        messages: Some(turn.messages.clone()),
        sync: Some(false),
        import_run_id: run_id.map(str::to_string),
        ..Default::default()
    }
}

async fn propose(eng: &Arc<Engine>, t: &Arc<Instance>, units: Vec<ImportUnit>) -> Value {
    let req = ImportProposeReq {
        project: PROJECT.into(),
        host: HOST.into(),
        finish: true,
        plan_hash: Some("plan-sessions".into()),
        units,
        ..Default::default()
    };
    import::propose(eng, t, req).await.unwrap()
}

fn run_req(run_id: &str) -> ImportRunReq {
    ImportRunReq {
        run_id: run_id.into(),
        decisions: BTreeMap::new(),
    }
}

/// The run's row of `turn`.
async fn row(t: &Arc<Instance>, run_id: &str, unit: &ImportUnit) -> Value {
    let turn_id = &unit.turn.as_ref().unwrap().turn_id;
    let rows = import::get_run(t, run_req(run_id)).await.unwrap()["proposals"].clone();
    rows.as_array()
        .unwrap()
        .iter()
        .find(|p| p["turn"]["turnId"] == turn_id.as_str())
        .cloned()
        .unwrap_or_else(|| panic!("no row of {turn_id} in {rows}"))
}

/// The current fact of the project with `text`.
async fn fact(t: &Instance, text: &str) -> MemoryRec {
    t.store
        .current_project_facts(PROJECT)
        .await
        .unwrap()
        .into_iter()
        .find(|m| m.text == text)
        .unwrap_or_else(|| panic!("no current fact {text:?}"))
}

pub async fn selftest() {
    let url = generation_stub().await;
    let eng = super::consolidation::engine(&url, false);
    let (t, caller, dir) = fresh_store(&eng, "import-sessions", HOST).await;
    let s1_1 = turn_unit("s1", "p1", STAGING, "2026-03-01T10:00:00Z");
    // The stub forms one fact per `user: ` line of the new messages: this turn's prompt holds two.
    let s1_2 = turn_unit(
        "s1",
        "p2",
        &format!("{STAGING}\nuser: {CI}"),
        "2026-03-01T10:05:00Z",
    );
    let s1_3 = turn_unit("s1", "p3", ONCALL, "2026-03-01T10:10:00Z");
    let s2_1 = turn_unit(
        "s2",
        "p4",
        "Atlas deploys on Fridays.",
        "2026-03-02T09:00:00Z",
    );

    // ---- malformed turns are refused before anything is stored
    let mut other_project = s1_1.clone();
    other_project.turn.as_mut().unwrap().turn_id = turn_key("other", "s1", "p1");
    let mut reversed = s1_1.clone();
    reversed.turn.as_mut().unwrap().messages.reverse();
    let mut bare = s1_1.clone();
    bare.turn = None;
    for unit in [other_project, reversed, bare] {
        let req = ImportProposeReq {
            project: PROJECT.into(),
            host: HOST.into(),
            units: vec![unit],
            ..Default::default()
        };
        assert!(matches!(
            import::propose(&eng, &t, req).await,
            Err(ApiError::InvalidArgument(_))
        ));
    }

    // ---- 1. propose registers the turns without a model call
    let before = llm_calls(&eng);
    let out = propose(
        &eng,
        &t,
        vec![s1_1.clone(), s1_2.clone(), s1_3.clone(), s2_1.clone()],
    )
    .await;
    let run = &out["run"];
    let r1 = run["runId"].as_str().unwrap().to_string();
    assert_eq!(run["status"], RUN_PROPOSED, "{run}");
    assert_eq!(
        (&run["counts"]["sessionTurns"], &run["counts"]["proposals"]),
        (&json!(4), &json!(0)),
        "{run}"
    );
    assert_eq!(llm_calls(&eng), before, "registering turns calls no model");
    assert!(run["units"]
        .as_array()
        .unwrap()
        .iter()
        .all(|u| u["outcome"] == UNIT_NEW && u["mode"] == MODE_SESSION_TURN));
    let first = row(&t, &r1, &s1_1).await;
    assert_eq!(
        first["proposalId"],
        import::turn_proposal_id(&r1, &s1_1.turn.as_ref().unwrap().turn_id)
    );
    assert_eq!(
        (&first["itemKind"], &first["status"]),
        (&json!(ITEM_SESSION_TURN), &json!(PROPOSED))
    );
    assert_eq!(t.store.count(schema::MEMORIES, None).await.unwrap(), 0);
    // A turn twice in one run is refused.
    let again = ImportProposeReq {
        project: PROJECT.into(),
        host: HOST.into(),
        units: vec![
            s1_1.clone(),
            turn_unit("s1", "p1", "Other text.", "2026-03-01T10:00:00Z"),
        ],
        ..Default::default()
    };
    assert!(import::propose(&eng, &t, again).await.is_err());

    // ---- 2. until applied, the run admits no turn
    let refused = ingest::add_memories(&eng, &t, &caller, submission(&s1_2, Some(&r1))).await;
    assert!(
        matches!(refused, Err(ApiError::InvalidArgument(_))),
        "{refused:?}"
    );

    // ---- 3. the turn s1/p1 was captured live before the import
    let live = ingest::add_memories(
        &eng,
        &t,
        &caller,
        AddMemoriesReq {
            sync: Some(true),
            ..submission(&s1_1, None)
        },
    )
    .await
    .unwrap();
    assert_eq!(live["status"], "completed", "{live}");
    let live_request = live["requestId"].as_str().unwrap().to_string();

    // ---- 4. apply queues every turn and writes no record
    let applied = import::apply(&t, run_req(&r1)).await.unwrap();
    assert_eq!(applied["run"]["status"], RUN_APPLIED);
    assert_eq!(applied["run"]["counts"]["turnsQueued"], 4);
    assert_eq!(applied["run"]["counts"]["applied"], 0);
    assert_eq!(
        row(&t, &r1, &s1_2).await["applied"],
        json!({"queued": true})
    );
    assert_eq!(t.store.count(schema::MEMORIES, None).await.unwrap(), 1);

    // ---- 5. the uploader submits them; what is not the run's applied turn is refused
    let codex = Caller {
        platform: "codex".into(),
    };
    let mut wrong_project = submission(&s1_2, Some(&r1));
    wrong_project.scope.project = Some("other".into());
    let mut edited = submission(&s1_2, Some(&r1));
    edited.messages.as_mut().unwrap()[0].content = "Something else.".into();
    let mut waiting = submission(&s1_2, Some(&r1));
    waiting.sync = Some(true);
    let mut unknown = submission(&s1_2, Some(&r1));
    unknown.turn_id = Some(turn_key(PROJECT, "s1", "p9"));
    let mut as_text = submission(&s1_2, Some(&r1));
    as_text.messages = None;
    as_text.text = Some(CI.into());
    let mut instruction = as_text.clone();
    instruction.memory_type = Some(T_INSTRUCTION.into());
    for (who, req) in [
        (&codex, submission(&s1_2, Some(&r1))),
        (&caller, wrong_project),
        (&caller, edited),
        (&caller, waiting),
        (&caller, unknown),
        (&caller, as_text),
        (&caller, instruction),
        (&caller, submission(&s1_2, Some("irmissing"))),
    ] {
        assert!(
            ingest::add_memories(&eng, &t, who, req).await.is_err(),
            "refused"
        );
    }
    let dup = ingest::add_memories(&eng, &t, &caller, submission(&s1_1, Some(&r1)))
        .await
        .unwrap();
    assert_eq!(dup["duplicate"], true, "{dup}");
    assert_eq!(dup["requestId"], live_request.as_str());
    let new = ingest::add_memories(&eng, &t, &caller, submission(&s1_2, Some(&r1)))
        .await
        .unwrap();
    assert_eq!(new["duplicate"], false, "{new}");
    let new_request = new["requestId"].as_str().unwrap().to_string();
    // A resend after a lost answer is a duplicate on the wire, still the run's own request on its row.
    let resent = ingest::add_memories(&eng, &t, &caller, submission(&s1_2, Some(&r1)))
        .await
        .unwrap();
    assert_eq!(resent["duplicate"], true);
    ingest::wait_idle(&eng, &t).await.unwrap();
    let request = t
        .store
        .get_doc(schema::REQUESTS, &new_request)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(request["importRunId"], r1.as_str());
    let live_doc = t
        .store
        .get_doc(schema::REQUESTS, &live_request)
        .await
        .unwrap()
        .unwrap();
    assert!(live_doc["importRunId"].is_null(), "{live_doc}");
    let dup_row = row(&t, &r1, &s1_1).await["applied"].clone();
    assert_eq!(
        (&dup_row["requestId"], &dup_row["duplicate"]),
        (&json!(live_request), &json!(true))
    );
    assert_eq!(row(&t, &r1, &s1_2).await["applied"]["duplicate"], false);
    // Formation: the staging fact was formed live and again from s1/p2; the CI fact only from s1/p2.
    let staging = fact(&t, STAGING).await;
    let ci = fact(&t, CI).await;
    assert_eq!(staging.status, S_CURRENT);
    assert_eq!(ci.status, S_CURRENT);
    // A row whose bookkeeping was lost learns again, from the request, that the turn is the run's own.
    let mut lost = row(&t, &r1, &s1_2).await;
    lost["applied"] = json!({"queued": true});
    let mut tx = memory_store::store::Tx::new();
    let id = lost["proposalId"].as_str().unwrap().to_string();
    tx.put(schema::PROPOSALS, &id, lost).unwrap();
    t.store.commit(tx).await.unwrap();
    ingest::add_memories(&eng, &t, &caller, submission(&s1_2, Some(&r1)))
        .await
        .unwrap();
    let relearned = row(&t, &r1, &s1_2).await["applied"].clone();
    assert_eq!(
        (&relearned["requestId"], &relearned["duplicate"]),
        (&json!(new_request), &json!(false))
    );

    // ---- 6. s1/p3 is submitted and still forming when the run is rolled back
    ingest::add_memories(&eng, &t, &caller, submission(&s1_3, Some(&r1)))
        .await
        .unwrap();
    let out = import::rollback(&eng, &t, run_req(&r1)).await.unwrap();
    assert_eq!(
        out["run"]["status"], RUN_PARTIALLY_ROLLED_BACK,
        "{}",
        out["run"]
    );
    let forming = row(&t, &r1, &s1_3).await;
    assert_eq!(forming["status"], APPLIED);
    assert!(forming["rollbackSkipReason"]
        .as_str()
        .unwrap()
        .contains("still forming"));
    // The run admits no more turns: s2/p4 never reached the daemon.
    assert!(
        ingest::add_memories(&eng, &t, &caller, submission(&s2_1, Some(&r1)))
            .await
            .is_err()
    );
    let unsent = row(&t, &r1, &s2_1).await;
    assert_eq!(
        (&unsent["status"], &unsent["rollback"]),
        (&json!(ROLLED_BACK), &json!({"submitted": false}))
    );
    // The duplicate's facts belong to live capture; the CI fact had only the run's request as its source.
    let dup_undone = row(&t, &r1, &s1_1).await;
    assert_eq!(dup_undone["rollback"]["duplicate"], true);
    assert_eq!(memory(&t, &staging.id).await.status, S_CURRENT);
    let ci_after = memory(&t, &ci.id).await;
    assert_eq!(ci_after.status, S_SUPERSEDED);
    let ci_doc = t
        .store
        .get_doc(schema::MEMORIES, &ci.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ci_doc["importRollback"]["runId"], r1.as_str());
    let undone = row(&t, &r1, &s1_2).await["rollback"].clone();
    assert_eq!(undone["retired"], json!([ci.id]));
    assert!(undone["kept"][0]["reason"]
        .as_str()
        .unwrap()
        .contains(&live_request));

    // ---- 7. once formed, a second rollback retires what the still-forming turn formed
    ingest::wait_idle(&eng, &t).await.unwrap();
    let oncall = fact(&t, ONCALL).await;
    assert_eq!(oncall.status, S_CURRENT);
    let out = import::rollback(&eng, &t, run_req(&r1)).await.unwrap();
    assert_eq!(out["run"]["status"], RUN_ROLLED_BACK, "{}", out["run"]);
    assert_eq!(out["run"]["counts"]["turnsRolledBack"], 4);
    assert_eq!(memory(&t, &oncall.id).await.status, S_SUPERSEDED);
    assert_eq!(memory(&t, &staging.id).await.status, S_CURRENT);
    drop(t);
    let _ = std::fs::remove_dir_all(dir);

    through_outboxes(&url).await;
    println!("import_sessions_selftest OK: turns registered without a model call, admitted only as the run's applied turns, deduplicated against live capture, rolled back keeping facts of other requests, retried once formed; capture then import through two outboxes is duplicate:true; local generation stub");
}

/// Live capture queues a turn in one outbox, an import queues the same turn and a new one in another, and the
/// in-process uploader drains both: the captured turn comes back `duplicate:true`.
async fn through_outboxes(url: &str) {
    let dir = std::env::temp_dir().join(memory_rules::util::new_id("memory-import-outboxes-"));
    let cfg = ServiceConfig {
        data_dir: dir.join("data"),
        ..Default::default()
    };
    let eng = super::stub_engine(cfg, url);
    let t = eng.store().await.unwrap();
    let sender = InProcess(eng.clone());

    let captured = Arc::new(Mutex::new(
        Outbox::open(&dir.join("capture.sqlite3")).unwrap(),
    ));
    let hook = |event: &str, extra: Value| {
        let mut e = json!({"hook_event_name": event, "session_id": "s1", "prompt_id": "p1"});
        e.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        e
    };
    for event in [
        hook("UserPromptSubmit", json!({"prompt": STAGING})),
        hook("Stop", json!({"last_assistant_message": "Noted."})),
    ] {
        captured
            .lock()
            .unwrap()
            .capture_hook(&event, PROJECT, HOST, None)
            .unwrap();
    }
    assert_eq!(drain_once(&captured, &sender).await.unwrap(), 0);
    let live_request = captured.lock().unwrap().status().unwrap()[0]["request_id"]
        .as_str()
        .unwrap()
        .to_string();

    let p1 = turn_unit("s1", "p1", STAGING, "2026-03-01T10:00:00Z");
    let p2 = turn_unit("s1", "p2", CI, "2026-03-01T10:05:00Z");
    let run_id = propose(&eng, &t, vec![p1.clone(), p2.clone()]).await["run"]["runId"]
        .as_str()
        .unwrap()
        .to_string();
    import::apply(&t, run_req(&run_id)).await.unwrap();
    let view = run_view(import::get_run(&t, run_req(&run_id)).await.unwrap()).unwrap();
    let path = dir.join("import.sqlite3");
    let queue = queue_turns(&view, &path, &Pace::new(0.0, 4), &mut |_| {})
        .await
        .unwrap();
    assert_eq!(
        queue,
        Queue {
            added: 2,
            ..Queue::default()
        }
    );
    // The turn capture queued is not queued again in capture's outbox.
    let held = queue_turns(
        &view,
        &dir.join("capture.sqlite3"),
        &Pace::new(0.0, 4),
        &mut |_| {},
    )
    .await
    .unwrap();
    assert_eq!(held.captured, 1);
    let imported = Arc::new(Mutex::new(Outbox::open(&path).unwrap()));
    assert_eq!(drain_once(&imported, &sender).await.unwrap(), 0);
    let first = row(&t, &run_id, &p1).await["applied"].clone();
    assert_eq!(
        (&first["requestId"], &first["duplicate"]),
        (&json!(live_request), &json!(true)),
        "{first}"
    );
    assert_eq!(row(&t, &run_id, &p2).await["applied"]["duplicate"], false);
    ingest::wait_idle(&eng, &t).await.unwrap();
    drain_once(&imported, &sender).await.unwrap();
    let statuses: Vec<String> = imported
        .lock()
        .unwrap()
        .status()
        .unwrap()
        .iter()
        .map(|j| j["status"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(statuses, ["completed", "completed"]);
    // Every turn has its request now: a resumed apply queues nothing.
    let view = run_view(import::get_run(&t, run_req(&run_id)).await.unwrap()).unwrap();
    let rest = queue_turns(&view, &path, &Pace::new(0.0, 4), &mut |_| {})
        .await
        .unwrap();
    assert_eq!(rest, Queue::default());
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(test)]
mod tests {
    #[tokio::test(flavor = "multi_thread")]
    async fn session_turns_import_dedupe_and_roll_back() {
        super::selftest().await;
    }
}
