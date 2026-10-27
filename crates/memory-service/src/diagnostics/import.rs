//! Memory import end to end (`memory_engine::app::import`) through the composed engine, with the local generation
//! stub and mock embeddings: requests checked first, batched propose with provenance and size rejections, review
//! decisions, unchanged reruns, a changed unit superseding its record, duplicates of another unit, a unit imported
//! by another run after propose, rollback with a corrected record kept, declined changes, `reconsider` and the
//! extraction cache. No external model call.
use crate::test_support::{engine, fresh_store, generation_stub, llm_calls, memory, stub_engine};
use memory_algo_mem0::{HASHES, SOURCES};
use memory_config::ServiceConfig;
use memory_domain::import::*;
use memory_domain::types::*;
use memory_engine::app::{import, management};
use memory_engine::retrieval;
use memory_engine::runtime::engine::Engine;
use memory_engine::runtime::indexer;
use memory_engine::runtime::instance::Instance;
use memory_rules::import::units::{content_hash, unit_key};
use memory_store::schema;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

const PROJECT: &str = "atlas";
const HOST: &str = "claude-code";
const STATED: &str = "2026-03-01T09:00:00Z";
const VERBATIM: &str = "The atlas service deploys with Nomad on Fridays.";
const MONDAY: &str = "The atlas service deploys with Nomad on Mondays.";
const TUESDAY: &str = "The atlas service deploys with Nomad on Tuesdays.";
const PIECE_KEPT: &str = "Atlas CI runs on Buildkite.";
const PIECE_DROPPED: &str = "Atlas releases are tagged by the release manager.";
const EXTRACT: &str = "- The atlas staging cluster lives in eu-west.\n- The TRAP job retries flaky tests.\n- Always run make check before pushing atlas.";
const FACT: &str = "The atlas staging cluster lives in eu-west.";
const TRAP: &str = "The TRAP job retries flaky tests. in 2031";
const ALWAYS: &str = "Always run make check before pushing atlas.";
const FEEDBACK: &str =
    "Prefer small pull requests for atlas.\n\nThe team reviews them within a day.";
const NIGHTLY: &str = "Atlas nightly builds publish artifacts to the release bucket.";

fn unit(native: &str, mode: &str, text: &str) -> ImportUnit {
    ImportUnit {
        unit: unit_key(HOST, native, "body"),
        host: HOST.into(),
        mode: mode.into(),
        text: text.into(),
        stated_at: STATED.into(),
        heading_path: vec!["Atlas notes".into()],
        file_hash: content_hash(native),
        unit_hash: content_hash(text),
        source: json!({"path": native}),
        ..Default::default()
    }
}

/// Five units: verbatim, split into two pieces, extracted (one item invents a number), a feedback note and one
/// over the unit cap.
fn plan() -> Vec<ImportUnit> {
    let mut split = unit(
        "memory/ci.md",
        MODE_SPLIT,
        &format!("{PIECE_KEPT} {PIECE_DROPPED}"),
    );
    split.pieces = vec![PIECE_KEPT.into(), PIECE_DROPPED.into()];
    let mut feedback = unit("memory/reviews.md", MODE_VERBATIM, FEEDBACK);
    feedback.kind_hint = Some("feedback".into());
    let archive = "The atlas archive keeps every build log for audits. ".repeat(20);
    vec![
        unit("memory/deploy.md", MODE_VERBATIM, VERBATIM),
        split,
        unit("memory/staging.md", MODE_EXTRACT, EXTRACT),
        feedback,
        unit("memory/archive.md", MODE_VERBATIM, archive.trim()),
    ]
}

async fn propose(
    eng: &Arc<Engine>,
    t: &Arc<Instance>,
    run_id: Option<&str>,
    finish: bool,
    units: Vec<ImportUnit>,
) -> Value {
    let req = ImportProposeReq {
        project: PROJECT.into(),
        host: HOST.into(),
        run_id: run_id.map(str::to_string),
        finish,
        limits: ImportLimits::default(),
        plan_hash: Some("plan-1".into()),
        reconsider: false,
        units,
    };
    import::propose(eng, t, req).await.unwrap()
}

fn run_req(run_id: &str, decisions: &[(&str, &str)]) -> ImportRunReq {
    ImportRunReq {
        run_id: run_id.into(),
        decisions: decisions
            .iter()
            .map(|(id, d)| (id.to_string(), d.to_string()))
            .collect::<BTreeMap<_, _>>(),
        confirmed_via: None,
    }
}

fn find<'a>(proposals: &'a Value, text: &str) -> &'a Value {
    proposals
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["text"] == text)
        .unwrap_or_else(|| panic!("no proposal {text:?} in {proposals}"))
}

/// The proposal of unit `cluster` with `text`.
fn pick<'a>(proposals: &'a Value, cluster: usize, text: &str) -> &'a Value {
    proposals
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["cluster"] == cluster && p["text"] == text)
        .unwrap_or_else(|| panic!("no proposal {text:?} of unit {cluster} in {proposals}"))
}

fn str_of(v: &Value) -> String {
    v.as_str().unwrap().to_string()
}

async fn rows(eng: &Arc<Engine>, t: &Arc<Instance>, table: &str) -> Vec<Value> {
    management::export(eng, t, json!({ "table": table }))
        .await
        .unwrap()["records"]
        .as_array()
        .unwrap()
        .clone()
}

/// A fresh store whose engine extracts with the generation stub.
async fn store(name: &str) -> (Arc<Engine>, Arc<Instance>, Caller, PathBuf) {
    let url = generation_stub().await;
    let eng = stub_engine(ServiceConfig::default(), &url);
    let (t, caller, dir) = fresh_store(&eng, name, "test").await;
    (eng, t, caller, dir)
}

/// The records of the first run of [`plan`], applied with the split's second piece declined.
struct First {
    r1: String,
    verbatim: String,
    kept: String,
    fact: String,
    always: String,
    feedback: String,
}

async fn first_run(eng: &Arc<Engine>, t: &Arc<Instance>) -> First {
    let out = propose(eng, t, None, true, plan()).await;
    let r1 = str_of(&out["run"]["runId"]);
    let dropped = str_of(&find(&out["proposals"], PIECE_DROPPED)["proposalId"]);
    let applied = import::apply(t, run_req(&r1, &[(dropped.as_str(), REJECT)]))
        .await
        .unwrap();
    assert_eq!(applied["run"]["status"], RUN_APPLIED, "{applied}");
    let id_of = |text: &str| str_of(&find(&applied["proposals"], text)["memoryId"]);
    let first = First {
        verbatim: id_of(VERBATIM),
        kept: id_of(PIECE_KEPT),
        fact: id_of(FACT),
        always: id_of(ALWAYS),
        feedback: id_of(FEEDBACK),
        r1,
    };
    indexer::drain(eng, t).await.unwrap();
    first
}

/// The second run after [`first_run`]: the verbatim unit changed, a copy of the kept piece in another unit, and
/// the split unit with a new second piece; then a third version of the verbatim unit proposed before the second
/// run is applied. Both runs are applied in that order.
struct Second {
    proposed: Value,
    applied: Value,
    late_applied: Value,
    r2: String,
    monday: String,
    nightly: String,
}

async fn second_run(eng: &Arc<Engine>, t: &Arc<Instance>) -> Second {
    let mut changed = unit("memory/deploy.md", MODE_VERBATIM, MONDAY);
    changed.stated_at = "2026-04-01T09:00:00Z".into();
    let duplicate = unit("memory/ci-copy.md", MODE_VERBATIM, PIECE_KEPT);
    let mut split = unit(
        "memory/ci.md",
        MODE_SPLIT,
        &format!("{PIECE_KEPT} {NIGHTLY}"),
    );
    split.pieces = vec![PIECE_KEPT.into(), NIGHTLY.into()];
    let proposed = propose(eng, t, None, true, vec![changed.clone(), duplicate, split]).await;
    let r2 = str_of(&proposed["run"]["runId"]);
    let mut tuesday = changed;
    tuesday.text = TUESDAY.into();
    tuesday.unit_hash = content_hash(&tuesday.text);
    let late = propose(eng, t, None, true, vec![tuesday]).await;
    let applied = import::apply(t, run_req(&r2, &[])).await.unwrap();
    let late_applied = import::apply(t, run_req(&str_of(&late["run"]["runId"]), &[]))
        .await
        .unwrap();
    indexer::drain(eng, t).await.unwrap();
    Second {
        monday: str_of(&find(&applied["proposals"], MONDAY)["memoryId"]),
        nightly: str_of(&find(&applied["proposals"], NIGHTLY)["memoryId"]),
        proposed,
        applied,
        late_applied,
        r2,
    }
}

/// One run of `units`: proposed in one batch with `reconsider` and `limits`, then applied with the decisions
/// `decide` gives. Returns the proposed and the applied answers.
async fn round(
    eng: &Arc<Engine>,
    t: &Arc<Instance>,
    units: Vec<ImportUnit>,
    reconsider: bool,
    limits: ImportLimits,
    decide: impl Fn(&Value) -> Vec<(String, String)>,
) -> (Value, Value) {
    let proposed = import::propose(
        eng,
        t,
        ImportProposeReq {
            project: PROJECT.into(),
            host: HOST.into(),
            finish: true,
            limits,
            reconsider,
            units,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let run_id = str_of(&proposed["run"]["runId"]);
    let req = ImportRunReq {
        run_id,
        decisions: decide(&proposed["proposals"]).into_iter().collect(),
        confirmed_via: None,
    };
    let applied = import::apply(t, req).await.unwrap();
    (proposed, applied)
}

fn none(_: &Value) -> Vec<(String, String)> {
    vec![]
}

/// Every proposal gets `decision`.
fn all(decision: &'static str) -> impl Fn(&Value) -> Vec<(String, String)> {
    move |proposals: &Value| {
        proposals
            .as_array()
            .unwrap()
            .iter()
            .map(|p| (str_of(&p["proposalId"]), decision.to_string()))
            .collect()
    }
}

/// The project's current facts with `text`.
async fn current_with(t: &Instance, text: &str) -> Vec<MemoryRec> {
    t.store
        .current_project_facts(PROJECT)
        .await
        .unwrap()
        .into_iter()
        .filter(|m| m.text == text)
        .collect()
}

/// Requests are checked first; a run proposed in batches stores proposals and rejections but no memory; apply follows review decisions, and imported facts are searchable but never presented as established.
#[tokio::test(flavor = "multi_thread")]
async fn propose_checks_requests_and_apply_follows_review_decisions() {
    let (eng, t, caller, dir) = store("import-propose-apply").await;
    // Requests are checked before anything is written.
    let bad = |project: &str, units: Vec<ImportUnit>| ImportProposeReq {
        project: project.into(),
        host: HOST.into(),
        units,
        ..Default::default()
    };
    let too_many = vec![unit("memory/a.md", MODE_VERBATIM, VERBATIM); MAX_BATCH_UNITS + 1];
    let mut twice = plan();
    twice.push(plan()[0].clone());
    let mut tiny = bad(PROJECT, vec![]);
    tiny.limits.recall_budget = 39;
    for req in [
        bad("*", plan()),
        bad(PROJECT, too_many),
        bad(PROJECT, twice),
        tiny,
    ] {
        assert!(matches!(
            import::propose(&eng, &t, req).await,
            Err(ApiError::InvalidArgument(_))
        ));
    }
    // Extraction needs a generation model.
    let offline = engine(ServiceConfig::default());
    assert!(matches!(
        import::propose(&offline, &t, bad(PROJECT, plan())).await,
        Err(ApiError::ModelUnavailable(_))
    ));
    assert_eq!(t.store.count(schema::IMPORT_RUNS, None).await.unwrap(), 0);
    // A run proposed in two batches stores proposals and rejections but writes no memory.
    let mut units = plan();
    let second = units.split_off(3);
    let first = propose(&eng, &t, None, false, units).await;
    assert_eq!(first["run"]["status"], RUN_PROPOSING, "{first}");
    let r1 = str_of(&first["run"]["runId"]);
    assert_eq!(first["units"].as_array().unwrap().len(), 3);
    let out = propose(&eng, &t, Some(&r1), true, second).await;
    let run = &out["run"];
    assert_eq!(run["status"], RUN_PROPOSED, "{run}");
    assert_eq!(run["kind"], KIND);
    assert_eq!(run["llmModel"], "stub-model");
    assert_eq!(run["promptFormat"], "import-v1");
    assert_eq!(run["planHash"], "plan-1");
    assert_eq!(run["units"].as_array().unwrap().len(), 5);
    assert_eq!(
        (
            &run["counts"]["proposals"],
            &run["counts"]["rejected"],
            &run["counts"]["generationCalls"],
            &run["counts"]["cacheHits"],
        ),
        (&json!(8), &json!(2), &json!(1), &json!(0)),
        "{run}"
    );
    assert_eq!(llm_calls(&eng), 1, "one generation call");
    assert_eq!(out["units"][1]["index"], 4);
    let mut proposals = first["proposals"].as_array().unwrap().clone();
    proposals.extend(out["proposals"].as_array().unwrap().iter().cloned());
    let proposals = Value::Array(proposals);
    let extract = &run["units"][2];
    assert_eq!(extract["outcome"], UNIT_NEW);
    assert!(extract["coverage"].as_f64().unwrap() > 0.5, "{extract}");
    assert!(extract["responseLogId"].is_string());
    // An item with a number its unit lacks, and a unit over the cap, are rejected.
    let trap = find(&proposals, TRAP);
    assert_eq!(trap["status"], REJECTED);
    assert!(
        trap["rejectReason"].as_str().unwrap().contains("2031"),
        "{trap}"
    );
    let archive = &proposals.as_array().unwrap()[7];
    assert_eq!(archive["status"], REJECTED);
    assert!(archive["rejectReason"]
        .as_str()
        .unwrap()
        .contains("more than the unit cap 200"));
    // A rule about how to work is a preference, and a feedback note offers no instruction.
    let always = find(&proposals, ALWAYS);
    assert_eq!(
        (&always["itemKind"], &always["mode"]),
        (&json!("preference"), &json!(MODE_EXTRACT))
    );
    assert_eq!(always["responseLogId"], extract["responseLogId"]);
    assert_eq!(find(&proposals, FEEDBACK)["itemKind"], "preference");
    assert!(proposals
        .as_array()
        .unwrap()
        .iter()
        .all(|p| p["itemKind"] != "instruction"));
    let dropped = find(&proposals, PIECE_DROPPED);
    assert_eq!(
        (dropped["cluster"].clone(), dropped["position"].clone()),
        (json!(1), json!(1))
    );
    assert_eq!(t.store.count(schema::MEMORIES, None).await.unwrap(), 0);
    let stored = import::get_run(&t, run_req(&r1, &[])).await.unwrap();
    assert_eq!(stored["proposals"].as_array().unwrap().len(), 8);
    // Apply follows review decisions; imported facts are searchable but not established.
    let proposed = propose(&eng, &t, None, true, plan()).await;
    let r1 = str_of(&proposed["run"]["runId"]);
    let always_id = str_of(&find(&proposed["proposals"], ALWAYS)["proposalId"]);
    let dropped_id = str_of(&find(&proposed["proposals"], PIECE_DROPPED)["proposalId"]);
    for decisions in [
        vec![(always_id.as_str(), "maybe")],
        vec![("ipmissing", ACCEPT)],
    ] {
        assert!(matches!(
            import::apply(&t, run_req(&r1, &decisions)).await,
            Err(ApiError::InvalidArgument(_))
        ));
    }
    assert!(matches!(
        import::apply(&t, run_req("irmissing", &[])).await,
        Err(ApiError::NotFound(_))
    ));
    let by_mail = ImportRunReq {
        confirmed_via: Some("mail".into()),
        ..run_req(&r1, &[])
    };
    assert!(matches!(
        import::apply(&t, by_mail).await,
        Err(ApiError::InvalidArgument(_))
    ));
    // Confirmed in a conversation: the run keeps how. One piece is declined, the rest accepted without a decision.
    let out = import::apply(
        &t,
        ImportRunReq {
            confirmed_via: Some(CONFIRMED_CHAT.into()),
            ..run_req(
                &r1,
                &[(dropped_id.as_str(), REJECT), (always_id.as_str(), ACCEPT)],
            )
        },
    )
    .await
    .unwrap();
    assert_eq!(out["run"]["status"], RUN_APPLIED, "{out}");
    assert_eq!(out["run"]["confirmedVia"], CONFIRMED_CHAT);
    let counts = &out["run"]["counts"];
    assert_eq!(
        (&counts["applied"], &counts["dropped"], &counts["skipped"]),
        (&json!(5), &json!(1), &json!(0)),
        "{counts}"
    );
    let applied = &out["proposals"];
    assert_eq!(find(applied, PIECE_DROPPED)["status"], DROPPED);
    assert_eq!(find(applied, TRAP)["status"], REJECTED);
    let imported: Vec<String> = [VERBATIM, PIECE_KEPT, FACT, ALWAYS, FEEDBACK]
        .iter()
        .map(|text| str_of(&find(applied, text)["memoryId"]))
        .collect();
    for id in &imported {
        let m = memory(&t, id).await;
        assert_eq!(m.mtype, T_FACT);
        assert_eq!(m.source.origin, ORIGIN_IMPORTED);
        assert_eq!(m.source.platform, HOST);
        assert_eq!(m.scope, MemScope::project(PROJECT));
        assert_eq!(m.stated_at, STATED);
        assert!(m.effective_at.is_empty());
        let detail = &m.detail.as_ref().unwrap()["import"];
        assert_eq!(detail["runId"], r1.as_str());
        assert_eq!(detail["host"], HOST);
        let evidence = t.store.evidence_referencing(id).await.unwrap();
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].role, "import");
        assert_eq!(evidence[0].turn_id, detail["unit"]);
        // Not formation output: no content key and no source batch.
        assert!(t.store.get_doc(SOURCES, id).await.unwrap().is_none());
    }
    let doc = t
        .store
        .get_doc(schema::MEMORIES, &imported[0])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(doc["ingestionMode"], "imported");
    assert_eq!(doc["contentHash"], content_hash(VERBATIM).as_str());
    assert_eq!(t.store.count(HASHES, None).await.unwrap(), 0);
    assert_eq!(rows(&eng, &t, schema::IMPORT_LEDGER).await.len(), 5);

    // Search finds imported facts; the bootstrap context never presents them as established.
    indexer::drain(&eng, &t).await.unwrap();
    let scope = ReqScope {
        project: Some(PROJECT.into()),
        ..Default::default()
    };
    let context = retrieval::get_context(
        &eng,
        &t,
        &caller,
        GetContextReq {
            scope: scope.clone(),
            token_budget: None,
        },
    )
    .await
    .unwrap();
    let ids = |list: &Value| -> Vec<String> {
        list.as_array()
            .unwrap()
            .iter()
            .map(|m| str_of(&m["id"]))
            .collect()
    };
    assert_eq!(context["instructions"], json!([]), "{context}");
    for id in &imported {
        assert!(!ids(&context["projectFacts"]).contains(id), "{context}");
        assert!(!ids(&context["facts"]).contains(id), "{context}");
    }
    let found = eng
        .search_memories(
            &t,
            &caller,
            SearchReq {
                query: VERBATIM.into(),
                scope: Some(scope),
                top_k: Some(5),
                token_budget: Some(400),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(
        found["results"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["memory"]["id"] == imported[0].as_str()
                && r["memory"]["source"]["origin"] == "imported"),
        "{found}"
    );
    assert!(found["tokensUsed"].as_u64().unwrap() <= 400);
    drop(t);
    let _ = std::fs::remove_dir_all(dir);
}

/// After a first run: unchanged units are not proposed again; changed units supersede their records and duplicates are linked; rollback runs newest first and keeps a record a person corrected.
#[tokio::test(flavor = "multi_thread")]
async fn reruns_changes_and_rollbacks_keep_the_ledger_in_step() {
    let (eng, t, caller, dir) = store("import-ledger").await;
    // A first run of the plan.
    let first = first_run(&eng, &t).await;
    // Unchanged units are not proposed again.
    let before = llm_calls(&eng);
    let same = propose(&eng, &t, None, true, plan()).await;
    assert!(
        same["units"]
            .as_array()
            .unwrap()
            .iter()
            .all(|u| u["outcome"] == UNIT_UNCHANGED),
        "{same}"
    );
    assert_eq!(same["proposals"], json!([]));
    assert_eq!(same["run"]["counts"]["unchanged"], 5);
    assert_eq!(same["run"]["counts"]["generationCalls"], 0);
    assert_eq!(llm_calls(&eng), before);
    // A second run: changed units supersede their records and duplicates are linked.
    let second = second_run(&eng, &t).await;
    let out = &second.proposed;
    // A changed verbatim unit replaces its record.
    assert_eq!(out["units"][0]["outcome"], UNIT_CHANGED, "{out}");
    assert_eq!(out["units"][0]["previousMemoryIds"], json!([]));
    let replacing = find(&out["proposals"], MONDAY);
    assert_eq!(replacing["supersedes"], first.verbatim.as_str());
    assert!(!replacing["similar"].as_array().unwrap().is_empty());
    // A duplicate of another unit's record is linked by default: the unit keeps its content even if the other
    // unit later replaces its own copy.
    let copy = pick(&out["proposals"], 1, PIECE_KEPT);
    assert_eq!(copy["duplicateOf"], first.kept.as_str());
    // A changed split unit keeps one piece: a duplicate of its own record, linked by default.
    let own = pick(&out["proposals"], 2, PIECE_KEPT);
    assert_eq!(own["duplicateOf"], first.kept.as_str());
    assert_eq!(out["units"][2]["outcome"], UNIT_CHANGED);
    assert_eq!(out["units"][2]["previousMemoryIds"], json!([first.kept]));
    assert_eq!(out["units"][2]["previousRecords"][0]["text"], PIECE_KEPT);
    assert_eq!(out["run"]["counts"]["duplicates"], 2);

    let applied = &second.applied;
    assert_eq!(applied["run"]["counts"]["applied"], 4, "{applied}");
    let linked = pick(&applied["proposals"], 2, PIECE_KEPT);
    assert_eq!(linked["status"], APPLIED);
    assert_eq!(linked["memoryId"], first.kept.as_str());
    assert_eq!(linked["applied"]["dedupedInto"], first.kept.as_str());
    let old = memory(&t, &first.verbatim).await;
    assert_eq!(old.status, S_SUPERSEDED);
    assert_eq!(old.superseded_by.as_deref(), Some(second.monday.as_str()));
    assert_eq!(old.valid_until.as_deref(), Some("2026-04-01T09:00:00Z"));
    assert_eq!(
        memory(&t, &second.monday).await.supersedes.as_deref(),
        Some(first.verbatim.as_str())
    );
    let copied = pick(&applied["proposals"], 1, PIECE_KEPT);
    assert_eq!(copied["status"], APPLIED, "{copied}");
    assert_eq!(copied["applied"]["dedupedInto"], first.kept.as_str());
    // A third version proposed before the second was applied: the unit moved under the second run.
    let skipped = &second.late_applied["proposals"][0];
    assert_eq!(skipped["status"], SKIPPED, "{}", second.late_applied);
    assert!(
        skipped["skipReason"].as_str().unwrap().contains(&second.r2),
        "{skipped}"
    );
    assert_eq!(memory(&t, &second.monday).await.status, S_CURRENT);
    // Rollback runs newest first and keeps a record a person corrected.
    let corrected = management::correct_memory(
        &eng,
        &t,
        &caller,
        CorrectMemoryReq {
            id: first.feedback.clone(),
            text: Some("Prefer small, focused pull requests for atlas.".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let out = import::rollback(&eng, &t, run_req(&second.r2, &[]))
        .await
        .unwrap();
    assert_eq!(out["run"]["status"], RUN_ROLLED_BACK, "{out}");
    let restored = memory(&t, &first.verbatim).await;
    assert_eq!(
        (
            restored.status.as_str(),
            restored.superseded_by,
            restored.valid_until
        ),
        (S_CURRENT, None, None)
    );
    let undone = t
        .store
        .get_doc(schema::MEMORIES, &second.monday)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(undone["status"], S_SUPERSEDED);
    assert_eq!(undone["importRollback"]["runId"], second.r2.as_str());
    assert_eq!(memory(&t, &second.nightly).await.status, S_SUPERSEDED);
    let unlinked = pick(&out["proposals"], 2, PIECE_KEPT);
    assert_eq!(unlinked["status"], ROLLED_BACK, "{unlinked}");
    assert_eq!(unlinked["rollback"]["unlinked"], first.kept.as_str());
    assert_eq!(memory(&t, &first.kept).await.status, S_CURRENT);
    assert!(matches!(
        import::rollback(&eng, &t, run_req(&second.r2, &[])).await,
        Err(ApiError::InvalidArgument(_))
    ));

    let out = import::rollback(&eng, &t, run_req(&first.r1, &[]))
        .await
        .unwrap();
    assert_eq!(out["run"]["status"], RUN_PARTIALLY_ROLLED_BACK, "{out}");
    for id in [&first.verbatim, &first.kept, &first.fact, &first.always] {
        assert_eq!(memory(&t, id).await.status, S_SUPERSEDED, "{id}");
    }
    let kept_back = find(&out["proposals"], FEEDBACK);
    assert_eq!(kept_back["status"], APPLIED);
    assert!(kept_back["rollbackSkipReason"]
        .as_str()
        .unwrap()
        .contains("replaced by"));
    assert_eq!(
        memory(&t, corrected["memory"]["id"].as_str().unwrap())
            .await
            .status,
        S_CURRENT
    );
    // Only the unit whose record a person corrected keeps a ledger row.
    let ledger = rows(&eng, &t, schema::IMPORT_LEDGER).await;
    assert_eq!(ledger.len(), 1, "{ledger:?}");
    assert_eq!(ledger[0]["memoryIds"], json!([first.feedback]));
    drop(t);
    let _ = std::fs::remove_dir_all(dir);
}

/// Repeated imports of the same sources: cached extractions, linked duplicates, a changed feedback note, a declined change, and the extraction cache's key and failures.
#[tokio::test(flavor = "multi_thread")]
async fn repeated_imports_link_replace_decline_and_cache_by_the_ledger() {
    let (eng, t, _, dir) = store("import-repeated").await;
    // A later run reuses a cached extraction; runs are listed newest first.
    let extract = || vec![unit("memory/staging.md", MODE_EXTRACT, EXTRACT)];
    let first = propose(&eng, &t, None, true, extract()).await;
    assert_eq!(first["run"]["counts"]["generationCalls"], 1, "{first}");
    let before = llm_calls(&eng);
    // The first run was never applied, so the unit is new again.
    let again = propose(&eng, &t, None, true, extract()).await;
    assert_eq!(again["units"][0]["outcome"], UNIT_NEW, "{again}");
    assert_eq!(again["units"][0]["cached"], true);
    assert_eq!(again["run"]["counts"]["cacheHits"], 1);
    assert_eq!(again["run"]["counts"]["generationCalls"], 0);
    assert_eq!(llm_calls(&eng), before);
    assert_eq!(again["proposals"].as_array().unwrap().len(), 3);
    assert_eq!(find(&again["proposals"], FACT)["duplicateOf"], Value::Null);
    assert_eq!(rows(&eng, &t, schema::IMPORT_EXTRACTIONS).await.len(), 1);
    // Runs are listed newest first.
    let runs = import::list_runs(&t, ImportRunsReq::default())
        .await
        .unwrap();
    let order: Vec<String> = runs["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| str_of(&r["runId"]))
        .collect();
    assert_eq!(
        order,
        [
            str_of(&again["run"]["runId"]),
            str_of(&first["run"]["runId"])
        ]
    );
    // A duplicate linked to another unit survives that unit's change.
    let limits = ImportLimits::default();
    const SHARED: &str = "The orbit gateway terminates TLS at the edge.";
    const SHARED_NEXT: &str = "The orbit gateway terminates TLS at the load balancer.";
    let (_, a1) = round(
        &eng,
        &t,
        vec![unit("memory/gw.md", MODE_VERBATIM, SHARED)],
        false,
        limits,
        none,
    )
    .await;
    let original = str_of(&find(&a1["proposals"], SHARED)["memoryId"]);
    let (p2, a2) = round(
        &eng,
        &t,
        vec![unit("memory/edge.md", MODE_VERBATIM, SHARED)],
        false,
        limits,
        none,
    )
    .await;
    assert_eq!(
        find(&p2["proposals"], SHARED)["duplicateOf"],
        original.as_str(),
        "{p2}"
    );
    assert_eq!(
        find(&a2["proposals"], SHARED)["applied"]["dedupedInto"],
        original.as_str()
    );
    let (p3, _) = round(
        &eng,
        &t,
        vec![unit("memory/gw.md", MODE_VERBATIM, SHARED_NEXT)],
        false,
        limits,
        none,
    )
    .await;
    // The shared record is held by another unit: listed, not replaced.
    assert_eq!(
        find(&p3["proposals"], SHARED_NEXT)["supersedes"],
        Value::Null,
        "{p3}"
    );
    assert_eq!(
        p3["units"][0]["previousRecords"][0]["memoryId"],
        original.as_str()
    );
    assert_eq!(memory(&t, &original).await.status, S_CURRENT);
    let (rerun, _) = round(
        &eng,
        &t,
        vec![unit("memory/edge.md", MODE_VERBATIM, SHARED)],
        false,
        limits,
        none,
    )
    .await;
    assert_eq!(rerun["units"][0]["outcome"], UNIT_UNCHANGED);
    assert_eq!(current_with(&t, SHARED).await.len(), 1);
    // A changed feedback note is one preference whose new text replaces the old.
    let limits = ImportLimits::default();
    const RULE: &str = "Prefer small pull requests for orbit.";
    const RULE_NEXT: &str = "Prefer tiny pull requests for orbit.";
    let text = |rule: &str| format!("{rule}\n\nReviews stay quick.");
    let note = |rule: &str| {
        let mut u = unit("memory/prs.md", MODE_VERBATIM, &text(rule));
        u.kind_hint = Some("feedback".into());
        u
    };
    let (p1, b1) = round(&eng, &t, vec![note(RULE)], false, limits, none).await;
    assert_eq!(p1["proposals"].as_array().unwrap().len(), 1, "{p1}");
    assert_eq!(p1["proposals"][0]["itemKind"], "preference");
    let first = str_of(&find(&b1["proposals"], &text(RULE))["memoryId"]);
    let (p2, b2) = round(&eng, &t, vec![note(RULE_NEXT)], false, limits, none).await;
    let next = find(&p2["proposals"], &text(RULE_NEXT));
    assert_eq!(next["supersedes"], first.as_str(), "{p2}");
    assert_eq!(p2["units"][0]["previousRecords"], Value::Null, "{p2}");
    let retired = memory(&t, &first).await;
    assert_eq!(retired.status, S_SUPERSEDED);
    let newest = str_of(&find(&b2["proposals"], &text(RULE_NEXT))["memoryId"]);
    assert_eq!(retired.superseded_by.as_deref(), Some(newest.as_str()));
    let facts = t.store.current_project_facts(PROJECT).await.unwrap();
    assert_eq!(
        facts
            .iter()
            .filter(|m| m.text.contains("pull requests for orbit"))
            .count(),
        1
    );
    // A declined change keeps the old record for the next version to replace.
    let limits = ImportLimits::default();
    const V1: &str = "Orbit logs are kept for 30 days.";
    const V2: &str = "Orbit logs are kept for 60 days.";
    const V3: &str = "Orbit logs are kept for 90 days.";
    let logs = |text: &str| vec![unit("memory/logs.md", MODE_VERBATIM, text)];
    let (_, c1) = round(&eng, &t, logs(V1), false, limits, none).await;
    let v1 = str_of(&find(&c1["proposals"], V1)["memoryId"]);
    let (_, c2) = round(&eng, &t, logs(V2), false, limits, all(REJECT)).await;
    assert_eq!(find(&c2["proposals"], V2)["status"], DROPPED);
    // The declined text is not offered again unless reconsidered.
    let (again, _) = round(&eng, &t, logs(V2), false, limits, all(REJECT)).await;
    assert_eq!(again["units"][0]["outcome"], UNIT_UNCHANGED, "{again}");
    let (back, _) = round(&eng, &t, logs(V2), true, limits, all(REJECT)).await;
    assert_eq!(back["units"][0]["outcome"], UNIT_RECONSIDERED, "{back}");
    assert_eq!(find(&back["proposals"], V2)["supersedes"], v1.as_str());
    let (p3, c3) = round(&eng, &t, logs(V3), false, limits, none).await;
    assert_eq!(
        find(&p3["proposals"], V3)["supersedes"],
        v1.as_str(),
        "{p3}"
    );
    let v3 = str_of(&find(&c3["proposals"], V3)["memoryId"]);
    assert_eq!(
        memory(&t, &v1).await.superseded_by.as_deref(),
        Some(v3.as_str())
    );
    // The extraction cache key covers the recall limits.
    let before = llm_calls(&eng);
    let limits = ImportLimits::default();
    let queue = || {
        vec![unit(
            "memory/queue.md",
            MODE_EXTRACT,
            "- The orbit queue drains every hour.",
        )]
    };
    round(&eng, &t, queue(), false, limits, all(REJECT)).await;
    assert_eq!(llm_calls(&eng) - before, 1);
    round(&eng, &t, queue(), true, limits, all(REJECT)).await;
    assert_eq!(llm_calls(&eng) - before, 1, "same input: cached");
    let wide = ImportLimits {
        recall_budget: 2000,
        recall_top_k: 2,
    };
    round(&eng, &t, queue(), true, wide, all(REJECT)).await;
    assert_eq!(llm_calls(&eng) - before, 2, "other limits: extracted again");
    // A reply with no usable item is not cached.
    let before = llm_calls(&eng);
    let limits = ImportLimits::default();
    // The stub adds a number the note lacks to a TRAP line, so its only item is rejected.
    let sensor = || {
        vec![unit(
            "memory/sensor.md",
            MODE_EXTRACT,
            "- The TRAP sensor reports orbit drift.",
        )]
    };
    round(&eng, &t, sensor(), false, limits, none).await;
    round(&eng, &t, sensor(), true, limits, none).await;
    assert_eq!(llm_calls(&eng) - before, 2, "every item failed: not cached");
    drop(t);
    let _ = std::fs::remove_dir_all(dir);
}
