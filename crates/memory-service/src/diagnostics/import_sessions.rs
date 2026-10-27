//! Session import end to end (docs/design/memory-import.md §6.10, §6.16), with the local generation stub and mock
//! embeddings: turns registered without a model call, admitted only as the run's applied turns, deduplicated
//! against live capture by `platform + turnId`, and rolled back fact by fact, keeping every fact another request
//! also formed. No external model call. The same path through the dialogue outboxes is tested with
//! `harness::import::commands`.
use crate::test_support::{fresh_store, generation_stub, llm_calls, memory, stub_engine};
use memory_config::ServiceConfig;
use memory_domain::import::*;
use memory_domain::types::*;
use memory_engine::app::{import, ingest};
use memory_engine::runtime::engine::Engine;
use memory_engine::runtime::instance::Instance;
use memory_rules::import::units::{content_hash, turn_key, unit_key};
use memory_store::schema;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;

pub(crate) const PROJECT: &str = "atlas";
pub(crate) const HOST: &str = "claude-code";
pub(crate) const STAGING: &str = "Atlas releases go to the staging bucket.";
pub(crate) const CI: &str = "Atlas CI runs on Buildkite.";
const ONCALL: &str = "The atlas on-call rotation is weekly.";

fn message(role: &str, content: &str, at: &str) -> InMessage {
    InMessage {
        role: role.into(),
        content: content.into(),
        timestamp: at.into(),
    }
}

/// A session-turn unit: `prompt` (one fact per line, as the stub forms them) answered at `at`.
pub(crate) fn turn_unit(session: &str, native: &str, prompt: &str, at: &str) -> ImportUnit {
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

pub(crate) async fn propose(eng: &Arc<Engine>, t: &Arc<Instance>, units: Vec<ImportUnit>) -> Value {
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

pub(crate) fn run_req(run_id: &str) -> ImportRunReq {
    ImportRunReq {
        run_id: run_id.into(),
        ..ImportRunReq::default()
    }
}

/// The run's row of `turn`.
pub(crate) async fn row(t: &Arc<Instance>, run_id: &str, unit: &ImportUnit) -> Value {
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

/// Four turns of two sessions. The stub forms one fact per `user: ` line of the new messages: s1/p2's prompt
/// holds two, one of them s1/p1's.
struct Turns {
    s1_1: ImportUnit,
    s1_2: ImportUnit,
    s1_3: ImportUnit,
    s2_1: ImportUnit,
}

impl Turns {
    fn new() -> Self {
        Self {
            s1_1: turn_unit("s1", "p1", STAGING, "2026-03-01T10:00:00Z"),
            s1_2: turn_unit(
                "s1",
                "p2",
                &format!("{STAGING}\nuser: {CI}"),
                "2026-03-01T10:05:00Z",
            ),
            s1_3: turn_unit("s1", "p3", ONCALL, "2026-03-01T10:10:00Z"),
            s2_1: turn_unit(
                "s2",
                "p4",
                "Atlas deploys on Fridays.",
                "2026-03-02T09:00:00Z",
            ),
        }
    }

    fn all(&self) -> Vec<ImportUnit> {
        vec![
            self.s1_1.clone(),
            self.s1_2.clone(),
            self.s1_3.clone(),
            self.s2_1.clone(),
        ]
    }
}

async fn store(name: &str) -> (Arc<Engine>, Arc<Instance>, Caller, PathBuf) {
    let url = generation_stub().await;
    let eng = stub_engine(ServiceConfig::default(), &url);
    let (t, caller, dir) = fresh_store(&eng, name, HOST).await;
    (eng, t, caller, dir)
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_turns_are_refused_before_anything_is_stored() {
    let (eng, t, _, dir) = store("import-turns-refused").await;
    let turns = Turns::new();
    let mut other_project = turns.s1_1.clone();
    other_project.turn.as_mut().unwrap().turn_id = turn_key("other", "s1", "p1");
    let mut reversed = turns.s1_1.clone();
    reversed.turn.as_mut().unwrap().messages.reverse();
    let mut bare = turns.s1_1.clone();
    bare.turn = None;
    // The same turn twice in one run.
    let twice = vec![
        turns.s1_1.clone(),
        turn_unit("s1", "p1", "Other text.", "2026-03-01T10:00:00Z"),
    ];
    for units in [vec![other_project], vec![reversed], vec![bare], twice] {
        let req = ImportProposeReq {
            project: PROJECT.into(),
            host: HOST.into(),
            units,
            ..Default::default()
        };
        assert!(matches!(
            import::propose(&eng, &t, req).await,
            Err(ApiError::InvalidArgument(_))
        ));
    }
    assert_eq!(t.store.count(schema::IMPORT_RUNS, None).await.unwrap(), 0);
    drop(t);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn proposing_registers_turns_without_a_model_call_and_admits_none_until_applied() {
    let (eng, t, caller, dir) = store("import-turns-propose").await;
    let turns = Turns::new();
    let out = propose(&eng, &t, turns.all()).await;
    let run = &out["run"];
    let r1 = run["runId"].as_str().unwrap().to_string();
    assert_eq!(run["status"], RUN_PROPOSED, "{run}");
    assert_eq!(
        (&run["counts"]["sessionTurns"], &run["counts"]["proposals"]),
        (&json!(4), &json!(0)),
        "{run}"
    );
    assert_eq!(llm_calls(&eng), 0, "registering turns calls no model");
    assert!(run["units"]
        .as_array()
        .unwrap()
        .iter()
        .all(|u| u["outcome"] == UNIT_NEW && u["mode"] == MODE_SESSION_TURN));
    let first = row(&t, &r1, &turns.s1_1).await;
    assert_eq!(
        first["proposalId"],
        import::turn_proposal_id(&r1, &turns.s1_1.turn.as_ref().unwrap().turn_id)
    );
    assert_eq!(
        (&first["itemKind"], &first["status"]),
        (&json!(ITEM_SESSION_TURN), &json!(PROPOSED))
    );
    assert_eq!(t.store.count(schema::MEMORIES, None).await.unwrap(), 0);
    // Until applied, the run admits no turn.
    let refused = ingest::add_memories(&eng, &t, &caller, submission(&turns.s1_2, Some(&r1))).await;
    assert!(
        matches!(refused, Err(ApiError::InvalidArgument(_))),
        "{refused:?}"
    );
    drop(t);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn applied_turns_are_admitted_once_and_deduplicated_against_live_capture() {
    let (eng, t, caller, dir) = store("import-turns-admit").await;
    let turns = Turns::new();
    let r1 = propose(&eng, &t, turns.all()).await["run"]["runId"]
        .as_str()
        .unwrap()
        .to_string();
    // The turn s1/p1 was captured live before the import.
    let live = ingest::add_memories(
        &eng,
        &t,
        &caller,
        AddMemoriesReq {
            sync: Some(true),
            ..submission(&turns.s1_1, None)
        },
    )
    .await
    .unwrap();
    assert_eq!(live["status"], "completed", "{live}");
    let live_request = live["requestId"].as_str().unwrap().to_string();

    // Apply queues every turn and writes no record.
    let applied = import::apply(&t, run_req(&r1)).await.unwrap();
    assert_eq!(applied["run"]["status"], RUN_APPLIED);
    assert_eq!(applied["run"]["counts"]["turnsQueued"], 4);
    assert_eq!(applied["run"]["counts"]["applied"], 0);
    assert_eq!(
        row(&t, &r1, &turns.s1_2).await["applied"],
        json!({"queued": true})
    );
    assert_eq!(t.store.count(schema::MEMORIES, None).await.unwrap(), 1);

    // What is not the run's applied turn is refused.
    let codex = Caller {
        platform: "codex".into(),
    };
    let s1_2 = || submission(&turns.s1_2, Some(&r1));
    let mut wrong_project = s1_2();
    wrong_project.scope.project = Some("other".into());
    let mut edited = s1_2();
    edited.messages.as_mut().unwrap()[0].content = "Something else.".into();
    let mut waiting = s1_2();
    waiting.sync = Some(true);
    let mut unknown = s1_2();
    unknown.turn_id = Some(turn_key(PROJECT, "s1", "p9"));
    let mut as_text = s1_2();
    as_text.messages = None;
    as_text.text = Some(CI.into());
    let mut instruction = as_text.clone();
    instruction.memory_type = Some(T_INSTRUCTION.into());
    for (who, req) in [
        (&codex, s1_2()),
        (&caller, wrong_project),
        (&caller, edited),
        (&caller, waiting),
        (&caller, unknown),
        (&caller, as_text),
        (&caller, instruction),
        (&caller, submission(&turns.s1_2, Some("irmissing"))),
    ] {
        assert!(
            ingest::add_memories(&eng, &t, who, req).await.is_err(),
            "refused"
        );
    }
    // The live turn is a duplicate; s1/p2 is new, and a resend after a lost answer is a duplicate on the
    // wire, still the run's own request on its row.
    let dup = ingest::add_memories(&eng, &t, &caller, submission(&turns.s1_1, Some(&r1)))
        .await
        .unwrap();
    assert_eq!(dup["duplicate"], true, "{dup}");
    assert_eq!(dup["requestId"], live_request.as_str());
    let new = ingest::add_memories(&eng, &t, &caller, s1_2())
        .await
        .unwrap();
    assert_eq!(new["duplicate"], false, "{new}");
    let new_request = new["requestId"].as_str().unwrap().to_string();
    let resent = ingest::add_memories(&eng, &t, &caller, s1_2())
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
    let dup_row = row(&t, &r1, &turns.s1_1).await["applied"].clone();
    assert_eq!(
        (&dup_row["requestId"], &dup_row["duplicate"]),
        (&json!(live_request), &json!(true))
    );
    assert_eq!(
        row(&t, &r1, &turns.s1_2).await["applied"]["duplicate"],
        false
    );
    // Formation: the staging fact was formed live and again from s1/p2; the CI fact only from s1/p2.
    assert_eq!(fact(&t, STAGING).await.status, S_CURRENT);
    assert_eq!(fact(&t, CI).await.status, S_CURRENT);

    // A row whose bookkeeping was lost learns again, from the request, that the turn is the run's own.
    let mut lost = row(&t, &r1, &turns.s1_2).await;
    lost["applied"] = json!({"queued": true});
    let mut tx = memory_store::store::Tx::new();
    let id = lost["proposalId"].as_str().unwrap().to_string();
    tx.put(schema::PROPOSALS, &id, lost).unwrap();
    t.store.commit(tx).await.unwrap();
    ingest::add_memories(&eng, &t, &caller, s1_2())
        .await
        .unwrap();
    let relearned = row(&t, &r1, &turns.s1_2).await["applied"].clone();
    assert_eq!(
        (&relearned["requestId"], &relearned["duplicate"]),
        (&json!(new_request), &json!(false))
    );
    drop(t);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn rollback_retires_only_facts_no_other_request_formed_and_retries_once_formed() {
    let (eng, t, caller, dir) = store("import-turns-rollback").await;
    let turns = Turns::new();
    let r1 = propose(&eng, &t, turns.all()).await["run"]["runId"]
        .as_str()
        .unwrap()
        .to_string();
    let live = ingest::add_memories(
        &eng,
        &t,
        &caller,
        AddMemoriesReq {
            sync: Some(true),
            ..submission(&turns.s1_1, None)
        },
    )
    .await
    .unwrap();
    let live_request = live["requestId"].as_str().unwrap().to_string();
    import::apply(&t, run_req(&r1)).await.unwrap();
    for unit in [&turns.s1_1, &turns.s1_2] {
        ingest::add_memories(&eng, &t, &caller, submission(unit, Some(&r1)))
            .await
            .unwrap();
    }
    ingest::wait_idle(&eng, &t).await.unwrap();
    let staging = fact(&t, STAGING).await;
    let ci = fact(&t, CI).await;

    // s1/p3 is submitted and still forming when the run is rolled back.
    ingest::add_memories(&eng, &t, &caller, submission(&turns.s1_3, Some(&r1)))
        .await
        .unwrap();
    let out = import::rollback(&eng, &t, run_req(&r1)).await.unwrap();
    assert_eq!(
        out["run"]["status"], RUN_PARTIALLY_ROLLED_BACK,
        "{}",
        out["run"]
    );
    let forming = row(&t, &r1, &turns.s1_3).await;
    assert_eq!(forming["status"], APPLIED);
    assert!(forming["rollbackSkipReason"]
        .as_str()
        .unwrap()
        .contains("still forming"));
    // The run admits no more turns: s2/p4 never reached the daemon.
    assert!(
        ingest::add_memories(&eng, &t, &caller, submission(&turns.s2_1, Some(&r1)))
            .await
            .is_err()
    );
    let unsent = row(&t, &r1, &turns.s2_1).await;
    assert_eq!(
        (&unsent["status"], &unsent["rollback"]),
        (&json!(ROLLED_BACK), &json!({"submitted": false}))
    );
    // The duplicate's facts belong to live capture; the CI fact had only the run's request as its source.
    let dup_undone = row(&t, &r1, &turns.s1_1).await;
    assert_eq!(dup_undone["rollback"]["duplicate"], true);
    assert_eq!(memory(&t, &staging.id).await.status, S_CURRENT);
    assert_eq!(memory(&t, &ci.id).await.status, S_SUPERSEDED);
    let ci_doc = t
        .store
        .get_doc(schema::MEMORIES, &ci.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ci_doc["importRollback"]["runId"], r1.as_str());
    let undone = row(&t, &r1, &turns.s1_2).await["rollback"].clone();
    assert_eq!(undone["retired"], json!([ci.id]));
    assert!(undone["kept"][0]["reason"]
        .as_str()
        .unwrap()
        .contains(&live_request));

    // Once formed, a second rollback retires what the still-forming turn formed.
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
}

#[tokio::test(flavor = "multi_thread")]
async fn a_turn_planned_before_a_redaction_rule_is_admitted_and_kept_redacted() {
    let (eng, t, caller, dir) = store("import-turns-redacted").await;
    // A client of an earlier version planned this turn without redacting the stated password.
    let unit = turn_unit(
        "s9",
        "p1",
        "测试库的数据库密码是 Pg-Test-2026-xyz。",
        "2026-03-03T10:00:00Z",
    );
    let r1 = propose(&eng, &t, vec![unit.clone()]).await["run"]["runId"]
        .as_str()
        .unwrap()
        .to_string();
    import::apply(&t, run_req(&r1)).await.unwrap();
    let out = ingest::add_memories(&eng, &t, &caller, submission(&unit, Some(&r1)))
        .await
        .unwrap();
    assert_eq!(out["duplicate"], false, "{out}");
    ingest::wait_idle(&eng, &t).await.unwrap();
    let request = t
        .store
        .get_doc(schema::REQUESTS, out["requestId"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert!(
        !request.to_string().contains("Pg-Test-2026-xyz"),
        "{request}"
    );
    let facts = t.store.current_project_facts(PROJECT).await.unwrap();
    assert!(
        facts
            .iter()
            .any(|m| m.text.contains("[redacted:env-secret]")),
        "{facts:?}"
    );
    assert!(facts.iter().all(|m| !m.text.contains("Pg-Test-2026-xyz")));
    drop(t);
    let _ = std::fs::remove_dir_all(dir);
}
