//! Session-end P2 validation and a transaction independent of fact formation.
use anyhow::{anyhow, Result};
use memory_domain::types::*;
use memory_rules::util;
use memory_store::schema;
use memory_store::store::{self, to_doc, Tx};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Deserialize)]
struct Output {
    analysis: Vec<Analysis>,
    experiences: Vec<Experience>,
    summary: Summary,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Analysis {
    #[serde(default)]
    task: String,
    result: String,
    turn_ids: Vec<String>,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Experience {
    situation: String,
    approach: String,
    result: String,
    lesson: String,
    scope: String,
    turn_ids: Vec<String>,
    relations: Vec<Relation>,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Relation {
    memory_id: String,
    relation: String,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Summary {
    goal: String,
    outcome: String,
    decisions: Vec<String>,
    open_issues: Vec<String>,
}

fn result_kind(result: &str) -> Option<&str> {
    ["success", "failure", "partial"].into_iter().find(|label| {
        result == *label
            || result
                .strip_prefix(*label)
                .map(|suffix| {
                    suffix.starts_with(':') || suffix.starts_with('：') || suffix.starts_with(' ')
                })
                .unwrap_or(false)
    })
}
/// Model task names are free text; trivial case/whitespace drift must not orphan a retained task.
fn same_task(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

/// Retention bounds: an unresolved (or renamed) task releases its turns instead of holding them
/// from purge and re-sending them in every later prompt.
const PENDING_TASK_TTL_TURNS: i64 = 30;
const PENDING_TASK_MAX_TURNS: usize = 12;
const MAX_PENDING_TASKS: usize = 5;

fn push_unique(to: &mut Vec<String>, extra: Vec<String>) {
    for s in extra {
        if !s.trim().is_empty() && !to.contains(&s) {
            to.push(s);
        }
    }
}
fn valid_experience(e: &Experience, analysis: &[Analysis], turn_ids: &HashSet<String>) -> bool {
    if e.turn_ids.is_empty()
        || e.turn_ids.iter().any(|id| !turn_ids.contains(id))
        || !["global", "project"].contains(&e.scope.as_str())
        || [&e.situation, &e.approach, &e.lesson]
            .into_iter()
            .any(|s| s.trim().is_empty())
    {
        return false;
    }
    let Some(kind) = result_kind(&e.result) else {
        return false;
    };
    // Unknown/interrupted tasks cannot supply experience. Every supporting turn must be covered by
    // the same task's verified outcome; mixing unrelated task outcomes would fabricate provenance.
    analysis
        .iter()
        .any(|a| a.result == kind && e.turn_ids.iter().all(|id| a.turn_ids.contains(id)))
}
fn relation_target<'a>(r: &Relation, old: &'a [MemoryRec]) -> Option<&'a MemoryRec> {
    if !["same", "related"].contains(&r.relation.as_str()) {
        return None;
    }
    let n = r.memory_id.strip_prefix('E')?.parse::<usize>().ok()?;
    if n == 0 {
        return None;
    }
    old.get(n - 1)
}

/// P2 records and evidence are prepared without writes, then committed independently.
pub struct Prepared {
    memories: Vec<MemoryRec>,
    evidence: Vec<EvidenceRec>,
    session: String,
    summary: Value,
    log: Value,
    created: Vec<String>,
    per_turn: HashMap<String, Vec<String>>,
}

impl Prepared {
    pub async fn commit(
        self,
        tenant: &memory_engine::runtime::tenant::Tenant,
        platform: &str,
    ) -> Result<()> {
        let mut tx = Tx::new();
        let mut out = super::relations::Applied::default();
        let mut texts = HashMap::new();
        self.append(&mut tx, &mut out, &mut texts)?;
        for (turn_id, ids) in &out.per_turn {
            // Merge only annotations; fact receipts and formation state remain independent.
            let Some(turn) = tenant.store.get_turn(platform, turn_id).await? else {
                continue;
            };
            let mut derived_ids = turn.derived_memory_ids;
            let new_ids: Vec<_> = ids
                .iter()
                .filter(|id| !derived_ids.contains(id))
                .cloned()
                .collect();
            push_unique(&mut derived_ids, ids.clone());
            let mut derived_text = turn.derived_text;
            for text in new_ids.iter().filter_map(|id| texts.get(id)) {
                if !derived_text.is_empty() {
                    derived_text.push('\n');
                }
                derived_text.push_str(text);
            }
            let rid = store::turn_rid(platform, turn_id);
            tx.merge(
                schema::TURNS,
                &rid,
                json!({"derivedMemoryIds":derived_ids,"derivedText":derived_text}),
            )?;
            tx.index_task(schema::TURNS, &rid, "upsert")?;
        }
        tenant.store.commit(tx).await?;
        tenant.wake_indexer();
        Ok(())
    }

    pub fn append(
        self,
        tx: &mut Tx,
        out: &mut super::relations::Applied,
        texts: &mut HashMap<String, String>,
    ) -> Result<()> {
        for m in self.memories {
            texts.insert(m.id.clone(), m.text.clone());
            tx.put(
                schema::MEMORIES,
                &m.id,
                super::relations::strip_nulls(to_doc(&m)?),
            )?;
            tx.index_task(schema::MEMORIES, &m.id, "upsert")?;
        }
        for ev in self.evidence {
            tx.put(schema::EVIDENCE, &ev.id, to_doc(&ev)?)?;
        }
        tx.merge(schema::SESSIONS, &self.session, self.summary)?;
        let log_id = util::new_id("l");
        tx.put(schema::EXTRACT_LOG, &log_id, self.log)?;
        out.created.extend(self.created);
        for (turn, ids) in self.per_turn {
            let mapped = out.per_turn.entry(turn).or_default();
            for id in ids {
                if !mapped.contains(&id) {
                    mapped.push(id.clone());
                }
                if !out.all_ids.contains(&id) {
                    out.all_ids.push(id);
                }
            }
        }
        Ok(())
    }
}

/// Caller holds the formation and ingest locks (`lock_formation`, `lock_ingest`). Current and retained unfinished-task turns may supply evidence.
pub fn prepare(
    session: &SessionRec,
    turns: &[TurnRec],
    current_ids: &HashSet<String>,
    old: &[MemoryRec],
    raw: Value,
    watermark: i64,
    variant: &str,
    now: &str,
) -> Result<Prepared> {
    let output_hash = blake3::hash(raw.to_string().as_bytes())
        .to_hex()
        .to_string();
    let output: Output =
        serde_json::from_value(raw).map_err(|e| anyhow!("invalid P2 schema: {e}"))?;
    let model_experiences = output.experiences.len();
    let mut analysis_results: HashMap<String, usize> = HashMap::new();
    for a in &output.analysis {
        *analysis_results.entry(a.result.clone()).or_default() += 1;
    }
    let mut same_count = 0usize;
    let mut created_count = 0usize;
    let tids: HashSet<String> = turns.iter().map(|tr| tr.turn_id.clone()).collect();
    let mut rejected = Vec::new();
    let mut used_tasks = HashSet::new();
    let mut memories: HashMap<String, MemoryRec> =
        old.iter().map(|m| (m.id.clone(), m.clone())).collect();
    let mut evidence = Vec::new();
    let mut created = Vec::new();
    let mut per_turn: HashMap<String, Vec<String>> = HashMap::new();
    let now = now.to_string();
    let mut pending = session.pending_tasks.clone();
    for a in &output.analysis {
        if a.task.trim().is_empty()
            || a.turn_ids.is_empty()
            || a.turn_ids.iter().any(|id| !tids.contains(id))
        {
            continue;
        }
        // `partial` is a verified outcome (it may form an experience), so only `unknown` stays open.
        if a.result == "unknown" {
            if let Some(task) = pending
                .iter_mut()
                .find(|task| same_task(&task.task, &a.task))
            {
                push_unique(&mut task.turn_ids, a.turn_ids.clone());
                task.updated_seq = watermark;
            } else {
                pending.push(PendingTask {
                    task: a.task.clone(),
                    turn_ids: a.turn_ids.clone(),
                    updated_seq: watermark,
                });
            }
        } else if matches!(a.result.as_str(), "success" | "failure" | "partial")
            && a.turn_ids.iter().any(|id| current_ids.contains(id))
        {
            // A named unfinished task can close only with current evidence and all retained sources.
            pending.retain(|task| {
                !same_task(&task.task, &a.task)
                    || !task.turn_ids.iter().all(|id| a.turn_ids.contains(id))
            });
        }
    }
    let mut expired = Vec::new();
    pending.retain(|task| {
        let keep = watermark - task.updated_seq <= PENDING_TASK_TTL_TURNS
            && task.turn_ids.len() <= PENDING_TASK_MAX_TURNS;
        if !keep {
            expired.push(task.task.clone());
        }
        keep
    });
    if pending.len() > MAX_PENDING_TASKS {
        pending.sort_by_key(|task| std::cmp::Reverse(task.updated_seq));
        expired.extend(
            pending
                .split_off(MAX_PENDING_TASKS)
                .into_iter()
                .map(|task| task.task),
        );
    }
    for (i, e) in output.experiences.into_iter().enumerate() {
        if i >= 3
            || !valid_experience(&e, &output.analysis, &tids)
            || !e.turn_ids.iter().any(|id| current_ids.contains(id))
        {
            let reason = if i >= 3 {
                "experience_limit_exceeded"
            } else if !["global", "project"].contains(&e.scope.as_str()) {
                "scope_invalid"
            } else if [&e.situation, &e.approach, &e.lesson]
                .into_iter()
                .any(|s| s.trim().is_empty())
            {
                "experience_field_empty"
            } else if e.turn_ids.is_empty() || e.turn_ids.iter().any(|id| !tids.contains(id)) {
                "source_turn_unknown"
            } else if result_kind(&e.result).is_none() {
                "result_unknown_or_invalid"
            } else {
                "task_outcome_not_verified_or_mismatched"
            };
            rejected.push(json!({"experience":i+1,"kind":"experience_discarded","reason":reason}));
            continue;
        }
        let task = output
            .analysis
            .iter()
            .enumerate()
            .find(|(index, a)| {
                !used_tasks.contains(index)
                    && Some(a.result.as_str()) == result_kind(&e.result)
                    && e.turn_ids.iter().all(|id| a.turn_ids.contains(id))
                    && session
                        .pending_tasks
                        .iter()
                        .filter(|task| same_task(&task.task, &a.task))
                        .all(|task| task.turn_ids.iter().all(|id| e.turn_ids.contains(id)))
            })
            .map(|(index, _)| index);
        let Some(task) = task else {
            rejected.push(json!({"experience":i+1,"kind":"experience_discarded","reason":"task_unmatched_or_sources_incomplete"}));
            continue;
        };
        used_tasks.insert(task);
        for r in &e.relations {
            if relation_target(r, &old).is_none() {
                rejected.push(json!({"experience":i+1,"kind":"relation_removed","memoryId":r.memory_id,"reason":"experience_reference_invalid"}));
            }
        }
        let targets: Vec<_> = e
            .relations
            .iter()
            .filter_map(|r| relation_target(r, &old).map(|m| (m.id.clone(), r.relation.clone())))
            .collect();
        let same = targets
            .iter()
            .find(|(_, r)| r == "same")
            .map(|(id, _)| id.clone());
        let stated = turns
            .iter()
            .filter(|tr| e.turn_ids.contains(&tr.turn_id))
            .max_by_key(|tr| util::ts_secs(&tr.ended_at))
            .map(|tr| tr.ended_at.clone())
            .unwrap_or(now.clone());
        let id = if let Some(id) = same {
            same_count += 1;
            let m = memories.get_mut(&id).unwrap();
            m.evidence_count += 1;
            push_unique(&mut m.source.turn_ids, e.turn_ids.clone());
            let input_memories = turns
                .iter()
                .filter(|tr| e.turn_ids.contains(&tr.turn_id))
                .flat_map(|tr| tr.derived_memory_ids.iter().cloned())
                .collect::<Vec<_>>();
            let lineage = m.derivation.get_or_insert_with(|| Derivation {
                stage: "session-experience".into(),
                input_memory_ids: vec![],
                input_turn_ids: m.source.turn_ids.clone(),
                current_view: false,
            });
            push_unique(&mut lineage.input_turn_ids, e.turn_ids.clone());
            push_unique(&mut lineage.input_memory_ids, input_memories);
            if util::ts_secs(&stated) > util::ts_secs(&m.stated_at) {
                m.stated_at = stated.clone();
            }
            id
        } else {
            created_count += 1;
            let id = util::new_id("m");
            created.push(id.clone());
            let separator = if variant == "en" { ": " } else { "：" };
            let m = MemoryRec {
                id: id.clone(),
                mtype: T_EXPERIENCE.into(),
                text: format!("{}{separator}{}", e.situation, e.lesson),
                scope: if e.scope == "global" {
                    MemScope::global()
                } else {
                    MemScope::project(&session.project)
                },
                status: S_CURRENT.into(),
                effective_at: util::date_of(&stated),
                recorded_at: now.clone(),
                stated_at: stated.clone(),
                last_accessed_at: now.clone(),
                evidence_count: 1,
                derivation: Some(Derivation {
                    stage: "session-experience".into(),
                    input_memory_ids: turns
                        .iter()
                        .filter(|tr| e.turn_ids.contains(&tr.turn_id))
                        .flat_map(|tr| tr.derived_memory_ids.iter().cloned())
                        .collect::<std::collections::BTreeSet<_>>()
                        .into_iter()
                        .collect(),
                    input_turn_ids: e.turn_ids.clone(),
                    current_view: false,
                }),
                source: Source {
                    platform: session.platform.clone(),
                    turn_ids: e.turn_ids.clone(),
                    origin: ORIGIN_INFERRED.into(),
                },
                detail: Some(
                    json!({"situation":e.situation,"approach":e.approach,"result":e.result,"lesson":e.lesson}),
                ),
                ..Default::default()
            };
            memories.insert(id.clone(), m);
            for (target, rel) in &targets {
                if rel != "related" {
                    continue;
                }
                let edge = RelEdge {
                    id: target.clone(),
                    relation: "related".into(),
                };
                if !memories[&id].related.contains(&edge) {
                    memories.get_mut(&id).unwrap().related.push(edge);
                }
                let edge = RelEdge {
                    id: id.clone(),
                    relation: "related".into(),
                };
                if !memories[target].related.contains(&edge) {
                    memories.get_mut(target).unwrap().related.push(edge);
                }
            }
            id
        };
        for turn_id in &e.turn_ids {
            per_turn
                .entry(turn_id.clone())
                .or_default()
                .push(id.clone());
        }
        // P2 cites whole supporting turns; copy their stored original content before buffer purge.
        for tr in turns.iter().filter(|tr| e.turn_ids.contains(&tr.turn_id)) {
            for (role, span, stated) in [
                ("user", &tr.user_message, &tr.user_ts),
                ("assistant", &tr.assistant_message, &tr.assistant_ts),
            ] {
                if span.is_empty() {
                    continue;
                }
                let ev = EvidenceRec {
                    context: None,
                    id: util::new_id("e"),
                    memory_ids: vec![id.clone()],
                    platform: session.platform.clone(),
                    turn_id: tr.turn_id.clone(),
                    role: role.into(),
                    span: span.clone(),
                    stated_at: stated.clone(),
                    created_at: now.clone(),
                };
                evidence.push(ev);
            }
        }
    }
    let goal = if output.summary.goal.trim().is_empty() {
        &session.goal
    } else {
        &output.summary.goal
    };
    let outcome = if output.summary.outcome.trim().is_empty() {
        &session.outcome
    } else {
        &output.summary.outcome
    };
    let no_summary_change = output.summary.goal.trim().is_empty()
        && output.summary.outcome.trim().is_empty()
        && output.summary.decisions.is_empty()
        && output.summary.open_issues.is_empty();
    let issues = if no_summary_change {
        session.open_issues.clone()
    } else {
        output.summary.open_issues
    };
    let mut decisions = session.decisions.clone();
    push_unique(&mut decisions, output.summary.decisions);
    let summary = json!({"goal":goal,"outcome":outcome,"decisions":decisions,
        "openIssues":issues,"pendingTasks":pending,
        "lastExperienceSeq":watermark,"updatedAt":now,
        "experienceFailures":0,"experienceState":"done","experienceError":Value::Null,
        "experienceRetryAtTs":0});
    let changed = memories
        .into_values()
        .filter(|m| {
            old.iter()
                .find(|o| o.id == m.id)
                .map(|o| serde_json::to_value(o).ok() != serde_json::to_value(m).ok())
                .unwrap_or(true)
        })
        .collect();
    let log = json!({"stage":"P2_validation","createdAt":now,"outputHash":output_hash,
        "promptVariant":variant,"groupLabel":format!("{}::{}::{}",session.platform,session.project,session.session),
        "sourceTurnIds":turns.iter().map(|tr|tr.turn_id.clone()).collect::<Vec<_>>(),
        "inputCounts":{"analysisTasks":output.analysis.len(),"modelExperiences":model_experiences,"accepted":used_tasks.len()},
        "analysisResults":analysis_results,"same":same_count,"created":created_count,
        "rejected":rejected,"accepted":used_tasks.len(),"expiredTasks":expired,"session":session.session,"watermark":watermark});
    Ok(Prepared {
        memories: changed,
        evidence,
        session: store::session_rid(&session.platform, &session.session),
        summary,
        log,
        created,
        per_turn,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn analysis(result: &str, turn: &str) -> Analysis {
        Analysis {
            task: "slow query".into(),
            result: result.into(),
            turn_ids: vec![turn.into()],
        }
    }

    #[test]
    fn experiences_need_a_known_outcome_on_their_turns() {
        let e = Experience {
            situation: "slow".into(),
            approach: "check".into(),
            result: "success: verified".into(),
            lesson: "check first".into(),
            scope: "global".into(),
            turn_ids: vec!["T1".into()],
            relations: vec![],
        };
        let ids = HashSet::from(["T1".into()]);
        assert!(valid_experience(&e, &[analysis("success", "T1")], &ids));
        assert!(!valid_experience(&e, &[analysis("unknown", "T1")], &ids));
        assert!(!valid_experience(&e, &[analysis("success", "T2")], &ids));
        assert_eq!(result_kind("failure：测试失败"), Some("failure"));
        assert_eq!(result_kind("successful maybe"), None);
    }

    #[test]
    fn pending_tasks_extend_close_and_expire() {
        let session = SessionRec {
            pending_tasks: vec![PendingTask {
                task: "deployment".into(),
                turn_ids: vec!["T0".into()],
                updated_seq: 1,
            }],
            ..Default::default()
        };
        let turns = ["T0", "T1"].map(|id| TurnRec {
            turn_id: id.into(),
            ..Default::default()
        });
        let run = |analysis: Value, experiences: Value, watermark: i64| {
            let raw = json!({"analysis":analysis,"experiences":experiences,
                "summary":{"goal":"","outcome":"","decisions":[],"openIssues":[]}});
            prepare(
                &session,
                &turns,
                &HashSet::from(["T1".into()]),
                &[],
                raw,
                watermark,
                "en",
                "2026-10-10T00:00:00Z",
            )
            .unwrap()
        };
        // unknown outcomes keep the task open; case/whitespace drift still extends it
        let pending = run(
            json!([{"task":" Deployment","result":"unknown","turnIds":["T0","T1"]}]),
            json!([]),
            2,
        );
        assert_eq!(
            pending.summary["pendingTasks"][0]["turnIds"],
            json!(["T0", "T1"])
        );
        // a verified outcome closes the task and forms an experience
        let partial = run(
            json!([{"task":"deployment","result":"partial","turnIds":["T0","T1"]}]),
            json!([{"situation":"deploy","approach":"apply half","result":"partial: one host left",
                "lesson":"stage hosts","scope":"project","turnIds":["T0","T1"],"relations":[]}]),
            2,
        );
        assert_eq!(partial.summary["pendingTasks"], json!([]));
        assert_eq!(partial.created.len(), 1);
        // tasks that are never resolved expire
        let stale = run(json!([]), json!([]), 2 + PENDING_TASK_TTL_TURNS);
        assert_eq!(stale.log["expiredTasks"], json!(["deployment"]));
    }
}
