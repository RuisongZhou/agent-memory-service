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
use memory_store::store::{to_doc, Tx};
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
    // A reader lists the run without its clusters and id lists, and sees it as apply returned it, with a
    // summary of every memory its proposals name.
    let runs = |req: Value| consolidation::list_runs(&t, serde_json::from_value(req).unwrap());
    let listed = runs(json!({"project": "ops"})).await.unwrap()["runs"].clone();
    assert_eq!(listed.as_array().unwrap().len(), 1, "{listed}");
    assert_eq!(
        (&listed[0]["runId"], &listed[0]["status"]),
        (&json!(run_id), &json!("applied"))
    );
    assert_eq!(listed[0]["appliedOps"], 3);
    for heavy in ["clusters", "proposalIds", "responseLogIds"] {
        assert!(listed[0].get(heavy).is_none(), "{heavy}");
    }
    assert_eq!(
        runs(json!({"project": "other"})).await.unwrap()["runs"],
        json!([])
    );
    for bad in [json!({"limit": 0}), json!({"limit": 201})] {
        assert!(matches!(runs(bad).await, Err(ApiError::InvalidArgument(_))));
    }
    let shown = consolidation::get_run(&t, run(&run_id)).await.unwrap();
    assert_eq!(shown["proposals"], json!(proposals));
    assert_eq!(shown["run"]["clusters"].as_array().unwrap().len(), 3);
    let merged_id = applied(&proposals, "merge")["mergedId"]
        .as_str()
        .unwrap()
        .to_string();
    let named = shown["memories"].as_object().unwrap();
    let mut want = vec![&laptop, &work, &merged_id, &jenkins, &actions, &dana, &lee];
    want.sort();
    assert_eq!(named.keys().collect::<Vec<_>>(), want);
    assert_eq!(
        named[&merged_id],
        json!({"id": merged_id, "type": "fact", "status": "current", "text": WORK,
            "scope": {"kind": "project", "project": "ops"}})
    );
    assert_eq!(named[&laptop]["status"], "superseded");
    for bad in ["no such", ""] {
        assert!(matches!(
            consolidation::get_run(&t, run(bad)).await,
            Err(ApiError::InvalidArgument(_))
        ));
    }
    assert!(matches!(
        consolidation::get_run(&t, run("crmissing")).await,
        Err(ApiError::NotFound(_))
    ));
    let questions =
        |req: Value| management::list_uncertainties(&t, serde_json::from_value(req).unwrap());
    let merge = applied(&proposals, "merge");
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
    // The conflict is listed as an open question of the project, with its members.
    let open = questions(json!({"project": "ops"})).await.unwrap()["uncertainties"].clone();
    assert_eq!(open.as_array().unwrap().len(), 1, "{open}");
    assert_eq!(
        (&open[0]["id"], &open[0]["reason"]),
        (&json!(question_id), &json!("manager differs"))
    );
    let members: Vec<(&Value, &Value)> = open[0]["memories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| (&m["id"], &m["status"]))
        .collect();
    assert_eq!(
        members,
        [
            (&json!(pair[0]), &json!(S_CONFLICT)),
            (&json!(pair[1]), &json!(S_CONFLICT))
        ]
    );
    for member in [&dana, &lee] {
        let m = memory(&t, member).await;
        assert_eq!(m.status, S_CONFLICT);
        assert!(m.related.is_empty());
    }
    // ---- the lineage of the applied run: the merge derives from its inputs, which it replaced; the supersede
    // is held by both records and is one edge; a conflict pair that names each other is one `related` edge.
    let lineage = |id: &str, depth: Option<usize>| {
        management::memory_lineage(
            &t,
            MemoryLineageReq {
                id: id.into(),
                depth,
            },
        )
    };
    let edge = |from: &str, to: &str, kind: &str, more: Value| {
        let mut edge = json!({"from": from, "to": to, "kind": kind});
        edge.as_object_mut()
            .unwrap()
            .extend(more.as_object().cloned().unwrap_or_default());
        edge
    };
    let edges = |out: &Value| {
        let mut edges = out["edges"].as_array().unwrap().clone();
        edges.sort_by_key(|e| format!("{}>{}>{}", e["kind"], e["from"], e["to"]));
        edges
    };
    let nodes = |out: &Value| -> Vec<(String, u64)> {
        out["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| {
                (
                    n["id"].as_str().unwrap().to_string(),
                    n["depth"].as_u64().unwrap(),
                )
            })
            .collect()
    };
    let mut inputs = [laptop.clone(), work.clone()];
    inputs.sort();
    let out = lineage(&merged_id, None).await.unwrap();
    assert_eq!(
        (&out["root"], &out["truncated"]),
        (&json!(merged_id), &json!(false))
    );
    assert_eq!(
        nodes(&out),
        [
            (merged_id.clone(), 0),
            (inputs[0].clone(), 1),
            (inputs[1].clone(), 1)
        ]
    );
    assert_eq!(
        out["nodes"][0],
        json!({"id": merged_id, "type": "fact", "status": "current", "text": WORK,
            "scope": {"kind": "project", "project": "ops"}, "recordedAt": merged.recorded_at,
            "effectiveAt": merged.effective_at, "statedAt": merged.stated_at,
            "origin": merged.source.origin, "platform": "test", "depth": 0})
    );
    let mut want = vec![];
    for input in &inputs {
        want.push(edge(
            &merged_id,
            input,
            "derived",
            json!({"stage": "consolidation"}),
        ));
        want.push(edge(&merged_id, input, "supersedes", json!({})));
    }
    want.sort_by_key(|e| format!("{}>{}>{}", e["kind"], e["from"], e["to"]));
    assert_eq!(edges(&out), want);
    // From an input the merge is one link away and the other input two: the depth cuts the lineage, and an edge
    // to a record outside it is left out.
    let out = lineage(&laptop, Some(1)).await.unwrap();
    assert_eq!(nodes(&out), [(laptop.clone(), 0), (merged_id.clone(), 1)]);
    assert_eq!(
        edges(&out),
        [
            edge(
                &merged_id,
                &laptop,
                "derived",
                json!({"stage": "consolidation"})
            ),
            edge(&merged_id, &laptop, "supersedes", json!({})),
        ]
    );
    let out = lineage(&laptop, Some(2)).await.unwrap();
    assert_eq!(
        nodes(&out),
        [
            (laptop.clone(), 0),
            (merged_id.clone(), 1),
            (work.clone(), 2)
        ]
    );
    assert_eq!(edges(&out).len(), 4);
    // The supersede is held by the new record (`supersedes`) and by the old (`supersededBy`): one edge from either end.
    for root in [&actions, &jenkins] {
        let out = lineage(root, None).await.unwrap();
        assert_eq!(
            edges(&out),
            [edge(&actions, &jenkins, "supersedes", json!({}))],
            "{out}"
        );
        assert_eq!(out["nodes"].as_array().unwrap().len(), 2);
    }
    // The conflict op writes no `related` edge; give the pair the legacy edges, one on each record.
    let related = |to: &str| json!({"related": [{"id": to, "relation": "conflicts"}]});
    t.store
        .merge_doc(schema::MEMORIES, &dana, related(&lee))
        .await
        .unwrap();
    t.store
        .merge_doc(schema::MEMORIES, &lee, related(&dana))
        .await
        .unwrap();
    let out = lineage(&lee, None).await.unwrap();
    let (first, second) = if dana < lee {
        (&dana, &lee)
    } else {
        (&lee, &dana)
    };
    assert_eq!(
        edges(&out),
        [edge(
            first,
            second,
            "related",
            json!({"relation": "conflicts"})
        )]
    );
    assert_eq!(nodes(&out), [(lee.clone(), 0), (dana.clone(), 1)]);
    for member in [&dana, &lee] {
        t.store
            .merge_doc(schema::MEMORIES, member, json!({"related": []}))
            .await
            .unwrap();
    }
    for depth in [0, 7] {
        assert!(matches!(
            lineage(&laptop, Some(depth)).await,
            Err(ApiError::InvalidArgument(_))
        ));
    }
    assert!(matches!(
        lineage("no such", None).await,
        Err(ApiError::InvalidArgument(_))
    ));
    assert!(matches!(
        lineage("mmissing", None).await,
        Err(ApiError::NotFound(_))
    ));

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
    assert_eq!(
        questions(json!({})).await.unwrap()["uncertainties"],
        json!([])
    );
    let withdrawn =
        questions(json!({"status": "withdrawn"})).await.unwrap()["uncertainties"].clone();
    assert_eq!(withdrawn.as_array().unwrap().len(), 1, "{withdrawn}");
    assert_eq!(
        (&withdrawn[0]["id"], &withdrawn[0]["withdrawnBy"]),
        (&json!(question_id), &json!(run_id))
    );
    assert!(withdrawn[0]["withdrawnAt"].is_string());
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

    // ---- a lineage stops at 200 records and says so; a link to a deleted record is not an edge
    let linked = |id: String| MemoryRec {
        id,
        mtype: T_PROFILE.into(),
        status: S_CURRENT.into(),
        scope: MemScope::global(),
        used_memory_ids: vec!["lhub".into()],
        ..Default::default()
    };
    let hub = MemoryRec {
        id: "lhub".into(),
        mtype: T_FACT.into(),
        status: S_CURRENT.into(),
        scope: MemScope::global(),
        related: vec![RelEdge {
            id: "lghost".into(),
            relation: "related".into(),
        }],
        ..Default::default()
    };
    let mut tx = Tx::new();
    tx.put(schema::MEMORIES, "lhub", to_doc(&hub).unwrap())
        .unwrap();
    for n in 0..205 {
        let user = linked(format!("lu{n:03}"));
        tx.put(schema::MEMORIES, &user.id, to_doc(&user).unwrap())
            .unwrap();
    }
    t.store.commit(tx).await.unwrap();
    let out = lineage("lhub", Some(1)).await.unwrap();
    let listed = nodes(&out);
    assert_eq!(out["truncated"], true);
    assert_eq!(listed.len(), 200);
    assert_eq!(
        (listed[0].clone(), listed[199].clone()),
        (("lhub".into(), 0), ("lu198".into(), 1))
    );
    assert_eq!(edges(&out).len(), 199);
    assert!(edges(&out)
        .iter()
        .all(|e| e["kind"] == "uses" && e["to"] == "lhub"));

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
