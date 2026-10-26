//! Applying and rolling back consolidation runs end to end, with the local generation stub and mock
//! embeddings: merge, supersede and conflict, deduplication into a merged record, rollback, rollback
//! after a user correction, and deletion of a merged record or one of its inputs.
use super::consolidation::{add, engine};
use crate::test_support::{fresh_store, generation_stub, memory};
use memory_algo_mem0::{HASHES, SOURCES};
use memory_domain::types::*;
use memory_engine::app::{consolidation, management};
use memory_engine::runtime::engine::Engine;
use memory_engine::runtime::indexer;
use memory_engine::runtime::instance::Instance;
use memory_store::schema;
use serde_json::{json, Value};
use std::sync::Arc;

const FILLERS: [&str; 12] = [
    "User drinks green tea every morning",
    "User keeps a dog named Rex",
    "User plays chess on weekends",
    "User commutes by bicycle",
    "User writes Rust at work",
    "User reads science fiction novels",
    "User cooks pasta on Sundays",
    "User swims twice a week",
    "User collects vinyl records",
    "User studies Japanese in the evening",
    "User grows tomatoes in the garden",
    "User volunteers at the library",
];
const LAPTOP: &str = "User uses Arch Linux on the laptop";
const WORK: &str = "User uses Arch Linux on the work laptop";

fn id_of(facts: &[MemoryRec], text: &str) -> String {
    facts.iter().find(|m| m.text == text).unwrap().id.clone()
}

async fn hash_target(t: &Instance, project: &str, text: &str) -> Value {
    let key = memory_rules::mem0::content_key(project, text, true);
    t.store.get_doc(HASHES, &key).await.unwrap().unwrap()["memoryId"].clone()
}

async fn source_requests(t: &Instance, id: &str) -> Value {
    t.store.get_doc(SOURCES, id).await.unwrap().unwrap()["inputRequestIds"].clone()
}

fn run(id: &str) -> ConsolidationRunReq {
    ConsolidationRunReq { run_id: id.into() }
}

/// Propose for `project` and apply the run; returns the run id and the first proposal per op kind.
async fn propose_and_apply(
    eng: &Arc<Engine>,
    t: &Arc<Instance>,
    project: &str,
) -> (String, Vec<Value>) {
    let proposed = consolidation::consolidate(
        eng,
        t,
        ConsolidateReq {
            project: project.into(),
            dry_run: Some(true),
        },
    )
    .await
    .unwrap();
    assert_eq!(proposed["run"]["status"], "proposed", "{proposed}");
    let run_id = proposed["run"]["runId"].as_str().unwrap().to_string();
    let out = consolidation::apply(eng, t, run(&run_id)).await.unwrap();
    assert_eq!(out["run"]["status"], "applied", "{out}");
    assert_eq!(out["run"]["skippedOps"], 0, "{out}");
    (run_id, out["proposals"].as_array().unwrap().clone())
}

fn applied<'a>(proposals: &'a [Value], op: &str) -> &'a Value {
    let p = proposals.iter().find(|p| p["op"] == op).unwrap();
    assert_eq!(p["status"], "applied", "{p}");
    &p["applied"]
}

/// One merge cluster (laptop, work laptop) among ten facts in `project`.
async fn merge_project(
    eng: &Arc<Engine>,
    t: &Arc<Instance>,
    c: &Caller,
    project: &str,
) -> (String, String) {
    let mut texts = vec![LAPTOP];
    texts.extend(&FILLERS[..9]);
    add(
        eng,
        t,
        c,
        project,
        &format!("{project}-1"),
        "2026-01-01T09:00:00Z",
        &texts,
    )
    .await;
    add(
        eng,
        t,
        c,
        project,
        &format!("{project}-2"),
        "2026-02-01T09:00:00Z",
        &[WORK],
    )
    .await;
    let facts = t.store.current_project_facts(project).await.unwrap();
    (id_of(&facts, LAPTOP), id_of(&facts, WORK))
}

pub async fn selftest() {
    let url = generation_stub().await;
    let eng = engine(&url, true);
    let (t, caller, dir) = fresh_store(&eng, "consolidation-apply", "test").await;
    let c = &caller;

    // ---- merge, supersede and conflict applied, deduplication into the merge, then rollback
    let mut first = vec![
        LAPTOP,
        "User's team runs CI on Jenkins",
        "User's manager at Acme Corp is Dana",
    ];
    first.extend(&FILLERS);
    let r1 = add(&eng, &t, c, "ops", "o1", "2026-01-01T09:00:00Z", &first).await;
    let r2 = add(
        &eng,
        &t,
        c,
        "ops",
        "o2",
        "2026-02-01T09:00:00Z",
        &[
            WORK,
            "User's team now runs CI on GitHub Actions instead of Jenkins",
            "User's manager at Acme Corp is Lee",
        ],
    )
    .await;
    let facts = t.store.current_project_facts("ops").await.unwrap();
    let (laptop, work) = (id_of(&facts, LAPTOP), id_of(&facts, WORK));
    let jenkins = id_of(&facts, "User's team runs CI on Jenkins");
    let actions = id_of(
        &facts,
        "User's team now runs CI on GitHub Actions instead of Jenkins",
    );
    let (dana, lee) = (
        id_of(&facts, "User's manager at Acme Corp is Dana"),
        id_of(&facts, "User's manager at Acme Corp is Lee"),
    );
    let disabled = engine(&url, false);
    assert!(matches!(
        consolidation::apply(&disabled, &t, run("cr0")).await,
        Err(ApiError::InvalidArgument(_))
    ));
    assert!(matches!(
        consolidation::apply(&eng, &t, run("missing")).await,
        Err(ApiError::NotFound(_))
    ));
    let (run_id, proposals) = propose_and_apply(&eng, &t, "ops").await;
    let merge = applied(&proposals, "merge");
    let merged_id = merge["mergedId"].as_str().unwrap().to_string();
    let merged = memory(&t, &merged_id).await;
    assert_eq!(merged.status, S_CURRENT);
    assert_eq!(merged.text, WORK);
    let derivation = merged.derivation.as_ref().unwrap();
    assert_eq!(derivation.stage, "consolidation");
    assert!(!derivation.current_view);
    assert_eq!(derivation.input_memory_ids, [laptop.clone(), work.clone()]);
    for input in [&laptop, &work] {
        let m = memory(&t, input).await;
        assert_eq!(m.status, S_SUPERSEDED);
        assert_eq!(m.superseded_by.as_deref(), Some(merged_id.as_str()));
    }
    assert_eq!(source_requests(&t, &merged_id).await, json!([r1, r2]));
    // The merged text equals one input's: its key is redirected once, with that input as before.
    assert_eq!(hash_target(&t, "ops", LAPTOP).await, merged_id);
    assert_eq!(hash_target(&t, "ops", WORK).await, merged_id);
    assert_eq!(merge["hashes"].as_array().unwrap().len(), 2);
    let old = memory(&t, &jenkins).await;
    let new = memory(&t, &actions).await;
    assert_eq!(old.status, S_SUPERSEDED);
    assert_eq!(old.superseded_by.as_deref(), Some(actions.as_str()));
    assert_eq!(old.valid_until.as_deref(), Some(new.stated_at.as_str()));
    assert_eq!(new.supersedes.as_deref(), Some(jenkins.as_str()));
    let question_id = applied(&proposals, "conflict")["uncertaintyId"]
        .as_str()
        .unwrap()
        .to_string();
    let question = t
        .store
        .get_doc(schema::UNCERTAINTIES, &question_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(question["status"], "open");
    let mut pair = vec![dana.clone(), lee.clone()];
    pair.sort();
    assert_eq!(question["memoryIds"], json!(pair));
    for member in [&dana, &lee] {
        let m = memory(&t, member).await;
        assert_eq!(m.status, S_CONFLICT);
        assert!(m.related.is_empty());
    }
    // Re-extracting an input's text deduplicates into the merged record and extends its sources.
    let again = AddMemoriesReq {
        scope: ReqScope {
            project: Some("ops".into()),
            session: Some("s".into()),
            ..Default::default()
        },
        turn_id: Some("o3".into()),
        messages: Some(vec![InMessage {
            role: "user".into(),
            content: LAPTOP.into(),
            timestamp: "2026-03-01T09:00:00Z".into(),
        }]),
        sync: Some(true),
        ..Default::default()
    };
    let out = memory_engine::app::ingest::add_memories(&eng, &t, c, again.clone())
        .await
        .unwrap();
    assert_eq!(out["memoryIds"], json!([merged_id]), "{out}");
    let r3 = out["requestId"].clone();
    assert_eq!(source_requests(&t, &merged_id).await, json!([r1, r2, r3]));
    assert!(matches!(
        consolidation::apply(&eng, &t, run(&run_id)).await,
        Err(ApiError::InvalidArgument(_))
    ));
    indexer::drain(&eng, &t).await.unwrap();

    let out = consolidation::rollback(&eng, &t, run(&run_id))
        .await
        .unwrap();
    assert_eq!(out["run"]["status"], "rolled_back", "{out}");
    assert_eq!(out["run"]["rolledBackOps"], 3);
    let merged = memory(&t, &merged_id).await;
    assert_eq!(merged.status, S_SUPERSEDED);
    for input in [&laptop, &work] {
        let m = memory(&t, input).await;
        assert_eq!(m.status, S_CURRENT);
        assert_eq!(m.superseded_by, None);
    }
    assert_eq!(hash_target(&t, "ops", LAPTOP).await, laptop);
    assert_eq!(hash_target(&t, "ops", WORK).await, work);
    let old = memory(&t, &jenkins).await;
    assert_eq!(
        (old.status.as_str(), old.superseded_by, old.valid_until),
        (S_CURRENT, None, None)
    );
    assert_eq!(memory(&t, &actions).await.supersedes, None);
    // Withdrawn by the rollback, not resolved by the store's conflict repair.
    let question = t
        .store
        .get_doc(schema::UNCERTAINTIES, &question_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(question["status"], "withdrawn");
    for member in [&dana, &lee] {
        assert_eq!(memory(&t, member).await.status, S_CURRENT);
    }
    // After rollback the input owns its text again; the rolled-back run is final.
    let again = AddMemoriesReq {
        turn_id: Some("o4".into()),
        ..again
    };
    let out = memory_engine::app::ingest::add_memories(&eng, &t, c, again)
        .await
        .unwrap();
    assert_eq!(out["memoryIds"], json!([laptop]), "{out}");
    assert!(matches!(
        consolidation::rollback(&eng, &t, run(&run_id)).await,
        Err(ApiError::InvalidArgument(_))
    ));

    // ---- a user correction of the merged record is never undone
    let (laptop, work) = merge_project(&eng, &t, c, "corr").await;
    let (run_id, proposals) = propose_and_apply(&eng, &t, "corr").await;
    let merged_id = applied(&proposals, "merge")["mergedId"]
        .as_str()
        .unwrap()
        .to_string();
    let corrected = management::correct_memory(
        &eng,
        &t,
        c,
        CorrectMemoryReq {
            id: merged_id.clone(),
            text: Some("User uses Fedora on the work laptop".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let out = consolidation::rollback(&eng, &t, run(&run_id))
        .await
        .unwrap();
    assert_eq!(out["run"]["status"], "partially_rolled_back", "{out}");
    let kept = out["proposals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["op"] == "merge")
        .unwrap();
    assert_eq!(kept["status"], "applied");
    assert!(kept["rollbackSkipReason"]
        .as_str()
        .unwrap()
        .contains("replaced by"));
    assert_eq!(
        memory(&t, corrected["memory"]["id"].as_str().unwrap())
            .await
            .status,
        S_CURRENT
    );
    for input in [&laptop, &work] {
        assert_eq!(memory(&t, input).await.status, S_SUPERSEDED);
    }

    // ---- deleting the merged record: its inputs stay superseded, and rollback has nothing to undo
    let (laptop, work) = merge_project(&eng, &t, c, "delmerge").await;
    let (run_id, proposals) = propose_and_apply(&eng, &t, "delmerge").await;
    let merged_id = applied(&proposals, "merge")["mergedId"]
        .as_str()
        .unwrap()
        .to_string();
    let out = management::delete_memory(
        &eng,
        &t,
        IdReq {
            id: merged_id.clone(),
        },
    )
    .await
    .unwrap();
    assert_eq!(out["restoredInputs"], json!([]), "{out}");
    for input in [&laptop, &work] {
        let m = memory(&t, input).await;
        assert_eq!((m.status.as_str(), m.superseded_by), (S_SUPERSEDED, None));
    }
    let out = consolidation::rollback(&eng, &t, run(&run_id))
        .await
        .unwrap();
    assert_eq!(out["run"]["status"], "partially_rolled_back", "{out}");

    // ---- deleting an input deletes the merge, which held its content, and restores the other input
    let (laptop, work) = merge_project(&eng, &t, c, "delinput").await;
    let (_, proposals) = propose_and_apply(&eng, &t, "delinput").await;
    let merged_id = applied(&proposals, "merge")["mergedId"]
        .as_str()
        .unwrap()
        .to_string();
    let out = management::delete_memory(&eng, &t, IdReq { id: laptop.clone() })
        .await
        .unwrap();
    assert_eq!(out["derivedDeleted"], json!([merged_id]), "{out}");
    assert_eq!(out["restoredInputs"], json!([work]), "{out}");
    assert!(t.store.get_memory(&merged_id).await.unwrap().is_none());
    let m = memory(&t, &work).await;
    assert_eq!((m.status.as_str(), m.superseded_by), (S_CURRENT, None));
    assert_eq!(hash_target(&t, "delinput", WORK).await, work);

    drop(t);
    let _ = std::fs::remove_dir_all(dir);
    println!("consolidation_apply_selftest OK: merge/supersede/conflict apply, dedup into merge, rollback, correction kept, deletion of merge and input");
}

#[cfg(test)]
mod tests {
    #[tokio::test(flavor = "multi_thread")]
    async fn applies_and_rolls_back() {
        super::selftest().await;
    }
}
