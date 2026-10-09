//! P2 extracts verified outcomes and updates the session summary at one committed watermark.
use super::{
    engine::Engine,
    extract, prompts,
    store::{self, to_doc, Tx},
    tenant::Tenant,
    types::*,
    util,
};
use anyhow::{anyhow, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[derive(Debug, Deserialize)]
struct Output {
    analysis: Vec<Analysis>,
    experiences: Vec<Experience>,
    summary: Summary,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Analysis {
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

/// Caller holds extract_lock. New turns received while the model runs remain beyond the saved watermark.
pub async fn process(eng: &Arc<Engine>, t: &Arc<Tenant>, session: &SessionRec) -> Result<()> {
    let turns: Vec<TurnRec> = t
        .store
        .session_turns(&session.platform, &session.session)
        .await?
        .into_iter()
        .filter(|tr| tr.seq > session.last_experience_seq && tr.seq <= session.turn_count)
        .collect();
    if turns.is_empty() {
        return Ok(());
    }
    if turns.iter().any(|tr| tr.extract_state != "done") {
        return Err(anyhow!(
            "P2 supporting turns have not completed fact extraction"
        ));
    }
    let p = eng.prompts_for(t);
    let query = turns
        .iter()
        .map(TurnRec::full_text)
        .collect::<Vec<_>>()
        .join("\n");
    let old = extract::recall(
        eng,
        t,
        &query,
        &session.project,
        &session.platform,
        Some(T_EXPERIENCE),
        5,
    )
    .await?;
    let rows = old
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let detail = m.detail.as_ref().unwrap_or(&Value::Null);
            format!(
                "E{} | {} | {} | {}",
                i + 1,
                m.scope.kind,
                detail["situation"].as_str().unwrap_or(&m.text),
                detail["lesson"].as_str().unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let user = format!(
        "{}\n\n{}\n\n{}",
        prompts::block(p.l.summary, &serde_json::to_string(session)?),
        prompts::block(
            p.l.old_experiences,
            if rows.is_empty() { p.l.none } else { &rows }
        ),
        prompts::block(p.l.conversation, &extract::conversation(&turns, p))
    );
    let raw = extract::call_logged(eng, t, "P2", p.p2, &user, 4096).await?;
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
    let mut tx = Tx::new();
    let now = util::now_str();
    let watermark = turns
        .iter()
        .map(|tr| tr.seq)
        .max()
        .unwrap_or(session.last_experience_seq);
    for (i, e) in output.experiences.into_iter().enumerate() {
        if i >= 3 || !valid_experience(&e, &output.analysis, &tids) {
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
            })
            .map(|(index, _)| index);
        let Some(task) = task else {
            rejected.push(json!({"experience":i+1,"kind":"experience_discarded","reason":"task_already_has_experience"}));
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
            if util::ts_secs(&stated) > util::ts_secs(&m.stated_at) {
                m.stated_at = stated.clone();
            }
            id
        } else {
            created_count += 1;
            let id = util::new_id("m");
            let separator = if p.variant == "en" { ": " } else { "：" };
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
        // P2 cites whole supporting turns; copy their stored original content before buffer purge.
        for tr in turns.iter().filter(|tr| e.turn_ids.contains(&tr.turn_id)) {
            for (role, span, stated) in [
                ("user", &tr.user_message, &tr.user_ts),
                ("assistant", &tr.assistant_message, &tr.assistant_ts),
            ] {
                if span.is_empty() {
                    continue;
                }
                let evidence = EvidenceRec {
                    id: util::new_id("e"),
                    memory_ids: vec![id.clone()],
                    platform: session.platform.clone(),
                    turn_id: tr.turn_id.clone(),
                    role: role.into(),
                    span: span.clone(),
                    stated_at: stated.clone(),
                    created_at: now.clone(),
                };
                tx.put("evidence", &evidence.id, to_doc(&evidence)?)?;
            }
        }
    }
    // This lock protects session sequence changes while we preserve turns that arrived during P2.
    let _ingest = t.ingest_lock.lock().await;
    let mut updated = t
        .store
        .get_session(&session.platform, &session.session)
        .await?
        .ok_or_else(|| anyhow!("session disappeared during P2"))?;
    if !output.summary.goal.trim().is_empty() {
        updated.goal = output.summary.goal;
    }
    if !output.summary.outcome.trim().is_empty() {
        updated.outcome = output.summary.outcome;
    }
    push_unique(&mut updated.decisions, output.summary.decisions);
    push_unique(&mut updated.open_issues, output.summary.open_issues);
    updated.last_experience_seq = watermark;
    updated.updated_at = now.clone();
    tx.put(
        "sessions",
        &store::session_rid(&session.platform, &session.session),
        to_doc(&updated)?,
    )?;
    for (id, m) in memories {
        // Unchanged recalled experiences do not need a write or a redundant index task.
        if old
            .iter()
            .find(|o| o.id == id)
            .map(|o| serde_json::to_value(o).ok() == serde_json::to_value(&m).ok())
            .unwrap_or(false)
        {
            continue;
        }
        tx.put("memories", &id, to_doc(&m)?)?;
        tx.index_task("memories", &id, "upsert")?;
    }
    t.store.commit(tx).await?;
    t.store.log("extract_log",json!({"stage":"P2_validation","createdAt":now,"outputHash":output_hash,
        "promptVariant":p.variant,"groupLabel":format!("{}::{}::{}",session.platform,session.project,session.session),
        "sourceTurnIds":turns.iter().map(|tr|tr.turn_id.clone()).collect::<Vec<_>>(),
        "inputCounts":{"analysisTasks":output.analysis.len(),"modelExperiences":model_experiences,"accepted":used_tasks.len()},
        "analysisResults":analysis_results,"same":same_count,"created":created_count,
        "rejected":rejected,"accepted":used_tasks.len(),"session":session.session,"watermark":watermark})).await?;
    t.index_notify.notify_one();
    Ok(())
}

/// Pure acceptance checks callable from the daemon selftest without loading a model or a test build.
pub fn rule_selftest() {
    let e = Experience {
        situation: "slow query".into(),
        approach: "compare tokenization".into(),
        result: "success: verification passed".into(),
        lesson: "compare both tokenizers first".into(),
        scope: "global".into(),
        turn_ids: vec!["T1".into()],
        relations: vec![],
    };
    let ids = HashSet::from(["T1".into()]);
    assert!(!valid_experience(
        &e,
        &[Analysis {
            result: "unknown".into(),
            turn_ids: vec!["T1".into()]
        }],
        &ids
    ));
    assert!(valid_experience(
        &e,
        &[Analysis {
            result: "success".into(),
            turn_ids: vec!["T1".into()]
        }],
        &ids
    ));
    assert!(!valid_experience(
        &e,
        &[Analysis {
            result: "success".into(),
            turn_ids: vec!["T2".into()]
        }],
        &ids
    ));
    assert_eq!(result_kind("successful maybe"), None);
    assert_eq!(result_kind("failure：测试失败"), Some("failure"));
    assert_eq!(result_kind("unknown: interrupted"), None);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unknown_outcomes_do_not_create_experiences() {
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
        assert!(!valid_experience(
            &e,
            &[Analysis {
                result: "unknown".into(),
                turn_ids: vec!["T1".into()]
            }],
            &ids
        ));
        assert!(valid_experience(
            &e,
            &[Analysis {
                result: "success".into(),
                turn_ids: vec!["T1".into()]
            }],
            &ids
        ));
        assert!(!valid_experience(
            &e,
            &[Analysis {
                result: "success".into(),
                turn_ids: vec!["T2".into()]
            }],
            &ids
        ));
        assert_eq!(result_kind("successful maybe"), None);
    }
}
