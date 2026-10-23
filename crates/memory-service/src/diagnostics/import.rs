//! Memory import end to end, with the local generation stub and mock embeddings: propose in batches
//! (verbatim, split, extract with a provenance rejection, instruction candidates, an over-cap unit), apply
//! with review decisions, unchanged reruns, a changed verbatim unit superseding its record, a duplicate of
//! another unit, a unit imported by another run after propose, rollback with a corrected record kept, and
//! the extraction cache. No external model call.
use super::consolidation::{engine, generation_stub};
use super::{fresh_store, llm_calls, memory};
use memory_algo_mem0::{HASHES, SOURCES};
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
use std::sync::Arc;

const PROJECT: &str = "atlas";
const HOST: &str = "claude-code";
const STATED: &str = "2026-03-01T09:00:00Z";
const VERBATIM: &str = "The atlas service deploys with Nomad on Fridays.";
const PIECE_KEPT: &str = "Atlas CI runs on Buildkite.";
const PIECE_DROPPED: &str = "Atlas releases are tagged by the release manager.";
const EXTRACT: &str = "- The atlas staging cluster lives in eu-west.\n- The TRAP job retries flaky tests.\n- Always run make check before pushing atlas.";
const FACT: &str = "The atlas staging cluster lives in eu-west.";
const ALWAYS: &str = "Always run make check before pushing atlas.";
const FEEDBACK: &str =
    "Prefer small pull requests for atlas.\n\nThe team reviews them within a day.";
const FIRST_PARAGRAPH: &str = "Prefer small pull requests for atlas.";
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

fn plan() -> Vec<ImportUnit> {
    let mut split = unit(
        "memory/ci.md",
        MODE_SPLIT,
        &format!("{PIECE_KEPT} {PIECE_DROPPED}"),
    );
    split.pieces = vec![PIECE_KEPT.into(), PIECE_DROPPED.into()];
    let mut feedback = unit("memory/reviews.md", MODE_VERBATIM, FEEDBACK);
    feedback.kind_hint = Some("feedback".into());
    feedback.instruction_candidate = true;
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
        scopes: BTreeMap::new(),
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

pub async fn selftest() {
    let url = generation_stub().await;
    let eng = engine(&url, true);
    let (t, caller, dir) = fresh_store(&eng, "import-selftest", "test").await;

    // ---- requests are checked before anything is written
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
    let offline = super::engine(memory_config::ServiceConfig::default());
    assert!(matches!(
        import::propose(&offline, &t, bad(PROJECT, plan())).await,
        Err(ApiError::ModelUnavailable(_))
    ));

    // ---- 1. one run proposed in two batches
    let before = llm_calls(&eng);
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
            &run["counts"]["instructionCandidates"],
            &run["counts"]["generationCalls"],
            &run["counts"]["cacheHits"],
        ),
        (&json!(9), &json!(2), &json!(2), &json!(1), &json!(0)),
        "{run}"
    );
    assert_eq!(llm_calls(&eng) - before, 1, "one generation call");
    assert_eq!(out["units"][1]["index"], 4);
    let mut proposals = first["proposals"].as_array().unwrap().clone();
    proposals.extend(out["proposals"].as_array().unwrap().iter().cloned());
    let proposals = Value::Array(proposals);
    let extract = &run["units"][2];
    assert_eq!(extract["outcome"], UNIT_NEW);
    assert!(extract["coverage"].as_f64().unwrap() > 0.5, "{extract}");
    assert!(extract["responseLogId"].is_string());
    let trap = find(&proposals, "The TRAP job retries flaky tests. in 2031");
    assert_eq!(trap["status"], REJECTED);
    assert!(
        trap["rejectReason"].as_str().unwrap().contains("2031"),
        "{trap}"
    );
    let archive = &proposals.as_array().unwrap()[8];
    assert_eq!(archive["status"], REJECTED);
    assert!(archive["rejectReason"]
        .as_str()
        .unwrap()
        .contains("more than the unit cap 200"));
    let always = find(&proposals, ALWAYS);
    assert_eq!(
        (
            &always["itemKind"],
            &always["defaultDecision"],
            &always["mode"]
        ),
        (&json!("instruction"), &json!(REJECT), &json!(MODE_EXTRACT))
    );
    assert_eq!(always["responseLogId"], extract["responseLogId"]);
    let candidate = find(&proposals, FIRST_PARAGRAPH);
    assert_eq!(
        (&candidate["itemKind"], &candidate["defaultDecision"]),
        (&json!("instruction"), &json!(REJECT))
    );
    let preference = find(&proposals, FEEDBACK);
    assert_eq!(
        (&preference["itemKind"], &preference["defaultDecision"]),
        (&json!("preference"), &json!(ACCEPT))
    );
    let dropped = find(&proposals, PIECE_DROPPED);
    assert_eq!(
        (dropped["cluster"].clone(), dropped["position"].clone()),
        (json!(1), json!(1))
    );
    // Proposing writes no memory record.
    assert_eq!(t.store.count(schema::MEMORIES, None).await.unwrap(), 0);
    let stored = import::get_run(&t, run_req(&r1, &[])).await.unwrap();
    assert_eq!(stored["proposals"].as_array().unwrap().len(), 9);

    // ---- 2. apply: one fact rejected by decision, one instruction accepted
    let always_id = str_of(&always["proposalId"]);
    let dropped_id = str_of(&dropped["proposalId"]);
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
    let out = import::apply(
        &t,
        run_req(
            &r1,
            &[(dropped_id.as_str(), REJECT), (always_id.as_str(), ACCEPT)],
        ),
    )
    .await
    .unwrap();
    assert_eq!(out["run"]["status"], RUN_APPLIED, "{out}");
    let counts = &out["run"]["counts"];
    assert_eq!(
        (&counts["applied"], &counts["dropped"], &counts["skipped"]),
        (&json!(5), &json!(2), &json!(0)),
        "{counts}"
    );
    let applied = &out["proposals"];
    assert_eq!(find(applied, PIECE_DROPPED)["status"], DROPPED);
    assert_eq!(find(applied, FIRST_PARAGRAPH)["status"], DROPPED);
    assert_eq!(
        find(applied, "The TRAP job retries flaky tests. in 2031")["status"],
        REJECTED
    );
    let id_of = |text: &str| str_of(&find(applied, text)["memoryId"]);
    let (verbatim, kept, fact, instruction, feedback) = (
        id_of(VERBATIM),
        id_of(PIECE_KEPT),
        id_of(FACT),
        id_of(ALWAYS),
        id_of(FEEDBACK),
    );
    for id in [&verbatim, &kept, &fact, &feedback] {
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
        assert_eq!(
            evidence[0].turn_id,
            m.detail.as_ref().unwrap()["import"]["unit"]
        );
        // Not formation output: no content key and no source batch.
        assert!(t.store.get_doc(SOURCES, id).await.unwrap().is_none());
    }
    let doc = t
        .store
        .get_doc(schema::MEMORIES, &verbatim)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(doc["ingestionMode"], "imported");
    assert_eq!(doc["contentHash"], content_hash(VERBATIM).as_str());
    assert_eq!(t.store.count(HASHES, None).await.unwrap(), 0);
    let rule = memory(&t, &instruction).await;
    assert_eq!(
        (rule.mtype.as_str(), rule.source.origin.as_str()),
        (T_INSTRUCTION, ORIGIN_EXPLICIT)
    );
    assert_eq!(rule.scope, MemScope::project(PROJECT));
    assert_eq!(rule.detail.as_ref().unwrap()["import"]["statedAt"], STATED);
    assert_eq!(rows(&eng, &t, schema::IMPORT_LEDGER).await.len(), 5);
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
    assert_eq!(
        ids(&context["instructions"]),
        std::slice::from_ref(&instruction),
        "{context}"
    );
    for imported in [&verbatim, &kept, &fact, &feedback] {
        assert!(
            !ids(&context["projectFacts"]).contains(imported),
            "{context}"
        );
        assert!(!ids(&context["facts"]).contains(imported), "{context}");
    }
    let found = eng
        .search_memories(
            &t,
            &caller,
            SearchReq {
                query: VERBATIM.into(),
                scope: Some(scope.clone()),
                top_k: Some(5),
                token_budget: Some(400),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let hits: Vec<&Value> = found["results"].as_array().unwrap().iter().collect();
    assert!(
        hits.iter().any(|r| r["memory"]["id"] == verbatim.as_str()
            && r["memory"]["source"]["origin"] == "imported"),
        "{found}"
    );
    assert!(found["tokensUsed"].as_u64().unwrap() <= 400);

    // ---- 3. the same units again: all unchanged, nothing to propose, no model call
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

    // ---- 4. a changed verbatim unit replaces its record; 5. another unit duplicates an imported text.
    // A changed split unit keeps one piece: a duplicate of its own record, linked by default.
    let monday = "The atlas service deploys with Nomad on Mondays.";
    let mut changed = unit("memory/deploy.md", MODE_VERBATIM, monday);
    changed.stated_at = "2026-04-01T09:00:00Z".into();
    let duplicate = unit("memory/ci-copy.md", MODE_VERBATIM, PIECE_KEPT);
    let mut split = unit(
        "memory/ci.md",
        MODE_SPLIT,
        &format!("{PIECE_KEPT} {NIGHTLY}"),
    );
    split.pieces = vec![PIECE_KEPT.into(), NIGHTLY.into()];
    let out = propose(
        &eng,
        &t,
        None,
        true,
        vec![changed.clone(), duplicate, split],
    )
    .await;
    let r2 = str_of(&out["run"]["runId"]);
    assert_eq!(out["units"][0]["outcome"], UNIT_CHANGED, "{out}");
    assert_eq!(out["units"][0]["previousMemoryIds"], json!([]));
    let replacing = find(&out["proposals"], monday);
    assert_eq!(replacing["supersedes"], verbatim.as_str());
    assert!(!replacing["similar"].as_array().unwrap().is_empty());
    // A duplicate of another unit's record is linked by default: the unit keeps its content even if the
    // other unit later replaces its own copy.
    let copy = pick(&out["proposals"], 1, PIECE_KEPT);
    assert_eq!(copy["duplicateOf"], kept.as_str());
    assert_eq!(copy["defaultDecision"], ACCEPT);
    let own = pick(&out["proposals"], 2, PIECE_KEPT);
    assert_eq!(own["duplicateOf"], kept.as_str());
    assert_eq!(own["defaultDecision"], ACCEPT);
    assert_eq!(out["units"][2]["outcome"], UNIT_CHANGED);
    assert_eq!(out["units"][2]["previousMemoryIds"], json!([kept]));
    assert_eq!(out["units"][2]["previousRecords"][0]["text"], PIECE_KEPT);
    assert_eq!(out["run"]["counts"]["duplicates"], 2);
    // A third version proposed before the second is applied: the unit moves under it.
    let mut tuesday = changed;
    tuesday.text = "The atlas service deploys with Nomad on Tuesdays.".into();
    tuesday.unit_hash = content_hash(&tuesday.text);
    let late = propose(&eng, &t, None, true, vec![tuesday]).await;
    let r2b = str_of(&late["run"]["runId"]);

    let out = import::apply(&t, run_req(&r2, &[])).await.unwrap();
    assert_eq!(out["run"]["counts"]["applied"], 4, "{out}");
    let monday_id = str_of(&find(&out["proposals"], monday)["memoryId"]);
    let nightly = str_of(&find(&out["proposals"], NIGHTLY)["memoryId"]);
    let linked = pick(&out["proposals"], 2, PIECE_KEPT);
    assert_eq!(linked["status"], APPLIED);
    assert_eq!(linked["memoryId"], kept.as_str());
    assert_eq!(linked["applied"]["dedupedInto"], kept.as_str());
    let old = memory(&t, &verbatim).await;
    assert_eq!(old.status, S_SUPERSEDED);
    assert_eq!(old.superseded_by.as_deref(), Some(monday_id.as_str()));
    assert_eq!(old.valid_until.as_deref(), Some("2026-04-01T09:00:00Z"));
    assert_eq!(
        memory(&t, &monday_id).await.supersedes.as_deref(),
        Some(verbatim.as_str())
    );
    let copied = pick(&out["proposals"], 1, PIECE_KEPT);
    assert_eq!(copied["status"], APPLIED, "{copied}");
    assert_eq!(copied["applied"]["dedupedInto"], kept.as_str());
    let out = import::apply(&t, run_req(&r2b, &[])).await.unwrap();
    let skipped = &out["proposals"][0];
    assert_eq!(skipped["status"], SKIPPED, "{out}");
    assert!(
        skipped["skipReason"].as_str().unwrap().contains(&r2),
        "{skipped}"
    );
    assert_eq!(memory(&t, &monday_id).await.status, S_CURRENT);
    indexer::drain(&eng, &t).await.unwrap();

    // ---- 6. rollback, newest run first; a record a person corrected stays
    let corrected = management::correct_memory(
        &eng,
        &t,
        &caller,
        CorrectMemoryReq {
            id: feedback.clone(),
            text: Some("Prefer small, focused pull requests for atlas.".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let out = import::rollback(&eng, &t, run_req(&r2, &[])).await.unwrap();
    assert_eq!(out["run"]["status"], RUN_ROLLED_BACK, "{out}");
    let restored = memory(&t, &verbatim).await;
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
        .get_doc(schema::MEMORIES, &monday_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(undone["status"], S_SUPERSEDED);
    assert_eq!(undone["importRollback"]["runId"], r2.as_str());
    assert_eq!(memory(&t, &nightly).await.status, S_SUPERSEDED);
    let unlinked = pick(&out["proposals"], 2, PIECE_KEPT);
    assert_eq!(unlinked["status"], ROLLED_BACK, "{unlinked}");
    assert_eq!(unlinked["rollback"]["unlinked"], kept.as_str());
    assert_eq!(memory(&t, &kept).await.status, S_CURRENT);
    assert!(matches!(
        import::rollback(&eng, &t, run_req(&r2, &[])).await,
        Err(ApiError::InvalidArgument(_))
    ));
    let out = import::rollback(&eng, &t, run_req(&r1, &[])).await.unwrap();
    assert_eq!(out["run"]["status"], RUN_PARTIALLY_ROLLED_BACK, "{out}");
    for id in [&verbatim, &kept, &fact, &instruction] {
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
    let context = retrieval::get_context(
        &eng,
        &t,
        &caller,
        GetContextReq {
            scope,
            token_budget: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(context["instructions"], json!([]), "{context}");
    // Only the unit whose record a person corrected keeps a ledger row.
    let ledger = rows(&eng, &t, schema::IMPORT_LEDGER).await;
    assert_eq!(ledger.len(), 1, "{ledger:?}");
    assert_eq!(ledger[0]["memoryIds"], json!([feedback]));

    // ---- 7. proposing the extract unit again reuses the cached extraction
    let before = llm_calls(&eng);
    let again = propose(
        &eng,
        &t,
        None,
        true,
        vec![unit("memory/staging.md", MODE_EXTRACT, EXTRACT)],
    )
    .await;
    assert_eq!(again["units"][0]["outcome"], UNIT_NEW, "{again}");
    assert_eq!(again["units"][0]["cached"], true);
    assert_eq!(again["run"]["counts"]["cacheHits"], 1);
    assert_eq!(again["run"]["counts"]["generationCalls"], 0);
    assert_eq!(llm_calls(&eng), before);
    assert_eq!(again["proposals"].as_array().unwrap().len(), 3);
    assert_eq!(find(&again["proposals"], FACT)["duplicateOf"], Value::Null);

    let runs = import::list_runs(&t, ImportRunsReq::default())
        .await
        .unwrap();
    let order: Vec<String> = runs["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| str_of(&r["runId"]))
        .collect();
    let newest = str_of(&again["run"]["runId"]);
    let same_id = str_of(&same["run"]["runId"]);
    assert_eq!(order, [newest, r2b, r2, same_id, r1.clone()]);
    assert_eq!(rows(&eng, &t, schema::IMPORT_RUNS).await.len(), 5);
    assert_eq!(rows(&eng, &t, schema::IMPORT_EXTRACTIONS).await.len(), 1);
    drop(t);
    let _ = std::fs::remove_dir_all(dir);
    println!("import_selftest OK: batched propose, provenance and size rejections, review decisions, unchanged rerun, verbatim supersede, duplicates, moved units, rollback with a kept correction, extraction cache; local generation stub");
}

/// One run of `units`: proposed in one batch with `reconsider` and `limits`, then applied with `decisions`
/// and `scopes`. Returns the proposed and the applied answers.
#[allow(clippy::too_many_arguments)]
async fn round(
    eng: &Arc<Engine>,
    t: &Arc<Instance>,
    units: Vec<ImportUnit>,
    reconsider: bool,
    limits: ImportLimits,
    decide: impl Fn(&Value) -> Vec<(String, String)>,
    scopes: impl Fn(&Value) -> Vec<(String, String)>,
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
        scopes: scopes(&proposed["proposals"]).into_iter().collect(),
    };
    let applied = import::apply(t, req).await.unwrap();
    (proposed, applied)
}

fn none(_: &Value) -> Vec<(String, String)> {
    vec![]
}

/// Every proposal with `text` gets `decision`.
fn on(text: &'static str, decision: &'static str) -> impl Fn(&Value) -> Vec<(String, String)> {
    move |proposals: &Value| {
        proposals
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| p["text"] == text)
            .map(|p| (str_of(&p["proposalId"]), decision.to_string()))
            .collect()
    }
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

/// Repeated imports of the same sources (the review findings): a duplicate linked to another unit keeps its
/// content when that unit changes; a changed note replaces its fact and its accepted instruction by role; a
/// declined change keeps the old record for the next version to replace; `reconsider`; extraction cache keys
/// and retries; instruction scope and size.
pub async fn repeated_runs() {
    let url = generation_stub().await;
    let eng = engine(&url, true);
    let (t, _caller, dir) = fresh_store(&eng, "import-repeated", "test").await;
    let limits = ImportLimits::default();

    // ---- a duplicate of another unit is linked, and survives that unit's change
    const SHARED: &str = "The orbit gateway terminates TLS at the edge.";
    const SHARED_NEXT: &str = "The orbit gateway terminates TLS at the load balancer.";
    let (_, a1) = round(
        &eng,
        &t,
        vec![unit("memory/gw.md", MODE_VERBATIM, SHARED)],
        false,
        limits,
        none,
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
        none,
    )
    .await;
    assert_eq!(
        find(&p2["proposals"], SHARED)["defaultDecision"],
        ACCEPT,
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
        none,
    )
    .await;
    assert_eq!(rerun["units"][0]["outcome"], UNIT_UNCHANGED);
    assert_eq!(current_with(&t, SHARED).await.len(), 1);

    // ---- a changed feedback note replaces its fact and its accepted instruction, by role
    const RULE: &str = "Prefer small pull requests for orbit.";
    const RULE_NEXT: &str = "Prefer tiny pull requests for orbit.";
    let note = |rule: &str| {
        let mut u = unit(
            "memory/prs.md",
            MODE_VERBATIM,
            &format!("{rule}\n\nReviews stay quick."),
        );
        u.kind_hint = Some("feedback".into());
        u.instruction_candidate = true;
        u
    };
    let (_, b1) = round(
        &eng,
        &t,
        vec![note(RULE)],
        false,
        limits,
        on(RULE, ACCEPT),
        none,
    )
    .await;
    let first_rule = str_of(&find(&b1["proposals"], RULE)["memoryId"]);
    let (p2, b2) = round(
        &eng,
        &t,
        vec![note(RULE_NEXT)],
        false,
        limits,
        on(RULE_NEXT, ACCEPT),
        none,
    )
    .await;
    let next_rule = find(&p2["proposals"], RULE_NEXT);
    assert_eq!(next_rule["supersedes"], first_rule.as_str(), "{p2}");
    assert_eq!(p2["units"][0]["previousRecords"], Value::Null, "{p2}");
    let retired = memory(&t, &first_rule).await;
    assert_eq!(retired.status, S_SUPERSEDED);
    let newest = str_of(&find(&b2["proposals"], RULE_NEXT)["memoryId"]);
    assert_eq!(retired.superseded_by.as_deref(), Some(newest.as_str()));
    let facts = t.store.current_project_facts(PROJECT).await.unwrap();
    assert_eq!(
        facts
            .iter()
            .filter(|m| m.text.contains("pull requests for orbit"))
            .count(),
        1
    );

    // ---- a declined change keeps the old record, and the next version replaces it
    const V1: &str = "Orbit logs are kept for 30 days.";
    const V2: &str = "Orbit logs are kept for 60 days.";
    const V3: &str = "Orbit logs are kept for 90 days.";
    let (_, c1) = round(
        &eng,
        &t,
        vec![unit("memory/logs.md", MODE_VERBATIM, V1)],
        false,
        limits,
        none,
        none,
    )
    .await;
    let v1 = str_of(&find(&c1["proposals"], V1)["memoryId"]);
    let (_, c2) = round(
        &eng,
        &t,
        vec![unit("memory/logs.md", MODE_VERBATIM, V2)],
        false,
        limits,
        all(REJECT),
        none,
    )
    .await;
    assert_eq!(find(&c2["proposals"], V2)["status"], DROPPED);
    let (again, _) = round(
        &eng,
        &t,
        vec![unit("memory/logs.md", MODE_VERBATIM, V2)],
        false,
        limits,
        all(REJECT),
        none,
    )
    .await;
    assert_eq!(again["units"][0]["outcome"], UNIT_UNCHANGED, "{again}");
    let (back, _) = round(
        &eng,
        &t,
        vec![unit("memory/logs.md", MODE_VERBATIM, V2)],
        true,
        limits,
        all(REJECT),
        none,
    )
    .await;
    assert_eq!(back["units"][0]["outcome"], UNIT_RECONSIDERED, "{back}");
    assert_eq!(find(&back["proposals"], V2)["supersedes"], v1.as_str());
    let (p3, c3) = round(
        &eng,
        &t,
        vec![unit("memory/logs.md", MODE_VERBATIM, V3)],
        false,
        limits,
        none,
        none,
    )
    .await;
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

    // ---- the extraction cache key covers the limits; a reply with no usable item is not cached
    const QUEUE: &str = "- The orbit queue drains every hour.";
    let before = llm_calls(&eng);
    round(
        &eng,
        &t,
        vec![unit("memory/queue.md", MODE_EXTRACT, QUEUE)],
        false,
        limits,
        all(REJECT),
        none,
    )
    .await;
    assert_eq!(llm_calls(&eng) - before, 1);
    round(
        &eng,
        &t,
        vec![unit("memory/queue.md", MODE_EXTRACT, QUEUE)],
        true,
        limits,
        all(REJECT),
        none,
    )
    .await;
    assert_eq!(llm_calls(&eng) - before, 1, "same input: cached");
    let wide = ImportLimits {
        recall_budget: 2000,
        recall_top_k: 2,
    };
    round(
        &eng,
        &t,
        vec![unit("memory/queue.md", MODE_EXTRACT, QUEUE)],
        true,
        wide,
        all(REJECT),
        none,
    )
    .await;
    assert_eq!(llm_calls(&eng) - before, 2, "other limits: extracted again");
    const SENSOR: &str = "- The TRAP sensor reports orbit drift.";
    let before = llm_calls(&eng);
    round(
        &eng,
        &t,
        vec![unit("memory/sensor.md", MODE_EXTRACT, SENSOR)],
        false,
        limits,
        none,
        none,
    )
    .await;
    round(
        &eng,
        &t,
        vec![unit("memory/sensor.md", MODE_EXTRACT, SENSOR)],
        true,
        limits,
        none,
        none,
    )
    .await;
    assert_eq!(llm_calls(&eng) - before, 2, "every item failed: not cached");

    // ---- instructions: project scope unless a person chooses global; the instruction limits apply
    const DEPLOY: &str = "Always tag orbit releases before deploying.";
    let mut rule = unit("export/rule-1", MODE_VERBATIM, DEPLOY);
    rule.kind_hint = Some("instruction".into());
    let global = |proposals: &Value| -> Vec<(String, String)> { on(DEPLOY, "global")(proposals) };
    let (_, d1) = round(
        &eng,
        &t,
        vec![rule],
        false,
        limits,
        on(DEPLOY, ACCEPT),
        global,
    )
    .await;
    let deploy = memory(&t, &str_of(&find(&d1["proposals"], DEPLOY)["memoryId"])).await;
    assert_eq!(
        (deploy.mtype.as_str(), deploy.scope.kind.as_str()),
        (T_INSTRUCTION, "global")
    );
    const CHECK: &str = "Always run the orbit checks before pushing.";
    let mut local = unit("export/rule-2", MODE_VERBATIM, CHECK);
    local.kind_hint = Some("instruction".into());
    let (_, d2) = round(
        &eng,
        &t,
        vec![local],
        false,
        limits,
        on(CHECK, ACCEPT),
        none,
    )
    .await;
    let check = memory(&t, &str_of(&find(&d2["proposals"], CHECK)["memoryId"])).await;
    assert_eq!(check.scope, MemScope::project(PROJECT));
    let long_rule = "Always explain orbit changes. ".repeat(40);
    let mut long = unit("export/rule-3", MODE_VERBATIM, long_rule.trim());
    long.kind_hint = Some("instruction".into());
    let roomy = ImportLimits {
        recall_budget: 3000,
        recall_top_k: 5,
    };
    let (p4, _) = round(&eng, &t, vec![long], false, roomy, none, none).await;
    let refused = &p4["proposals"][0];
    assert_eq!(refused["status"], REJECTED, "{p4}");
    assert!(
        refused["rejectReason"]
            .as_str()
            .unwrap()
            .contains("more than 1024"),
        "{refused}"
    );

    drop(t);
    let _ = std::fs::remove_dir_all(dir);
    println!("import_repeated_selftest OK: linked duplicates survive their source's change, supersede by role, declined changes, reconsider, extraction cache key and retries, instruction scope and size; local generation stub");
}

#[cfg(test)]
mod tests {
    #[tokio::test(flavor = "multi_thread")]
    async fn imports_reviews_and_rolls_back() {
        super::selftest().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn repeated_imports_keep_sources_and_memories_in_step() {
        super::repeated_runs().await;
    }
}
