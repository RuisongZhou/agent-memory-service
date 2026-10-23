//! Applying and rolling back consolidation ops as logical record changes. Every change is a field patch
//! that keeps the values it replaced, so rollback is the inverse patch, taken only while the fields still
//! hold what apply wrote. Nothing here deletes a record. Algorithm-private tables are the algorithm's.
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::HashSet;

use super::{check_op, eligible, statement_order, ConsolidationOp};
use crate::types::*;

/// `derivation.stage` of a merged record.
pub const STAGE: &str = "consolidation";

/// Field changes to one memory record. `null` in `before` means the field was absent; in `after`,
/// that the field is cleared.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct FieldPatch {
    pub id: String,
    pub before: Map<String, Value>,
    pub after: Map<String, Value>,
}

impl FieldPatch {
    /// A patch writing `after` over `m`, recording the values it replaces.
    pub fn over(m: &MemoryRec, after: &[(&str, Value)]) -> Self {
        Self {
            id: m.id.clone(),
            before: after
                .iter()
                .map(|(k, _)| (k.to_string(), field(m, k)))
                .collect(),
            after: after
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        }
    }

    pub fn inverse(&self) -> Self {
        Self {
            id: self.id.clone(),
            before: self.after.clone(),
            after: self.before.clone(),
        }
    }
}

/// A record's field as stored (camelCase); `null` when absent.
fn field(m: &MemoryRec, key: &str) -> Value {
    serde_json::to_value(m).expect("a memory record serializes")[key].clone()
}

/// Whether `m` was created by a consolidation merge.
pub fn is_merge(m: &MemoryRec) -> bool {
    m.derivation.as_ref().is_some_and(|d| d.stage == STAGE)
}

/// Outcome of applying or rolling back one op: what changed, or why it was left alone.
#[derive(Debug, Clone, PartialEq)]
pub enum OpOutcome {
    Done(Value),
    Skipped(String),
}

pub struct ApplyContext<'a> {
    pub project: &'a str,
    pub run_id: &'a str,
    pub proposal_id: &'a str,
    /// Id of the merged record or the opened question.
    pub new_id: &'a str,
    pub now: &'a str,
    /// Records named by an open uncertainty.
    pub disputed: &'a HashSet<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ApplyPlan {
    /// The merged record of a merge.
    pub created: Option<MemoryRec>,
    pub patches: Vec<FieldPatch>,
    /// The question a conflict opens.
    pub uncertainty: Option<UncertaintyRec>,
}

/// What an applied op changed, as stored with its proposal for rollback.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AppliedOp {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merged_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uncertainty_id: Option<String>,
    pub patches: Vec<FieldPatch>,
}

impl ApplyPlan {
    pub fn applied(&self) -> AppliedOp {
        AppliedOp {
            merged_id: self.created.as_ref().map(|m| m.id.clone()),
            uncertainty_id: self.uncertainty.as_ref().map(|u| u.id.clone()),
            patches: self.patches.clone(),
        }
    }
}

fn merged_record(inputs: &[&MemoryRec], text: &str, ctx: &ApplyContext) -> MemoryRec {
    let mut turn_ids: Vec<String> = Vec::new();
    let mut entity_ids: Vec<String> = Vec::new();
    for m in inputs {
        for id in &m.source.turn_ids {
            if !turn_ids.contains(id) {
                turn_ids.push(id.clone());
            }
        }
        for id in &m.entity_ids {
            if !entity_ids.contains(id) {
                entity_ids.push(id.clone());
            }
        }
    }
    let platform = match inputs.first() {
        Some(first)
            if inputs
                .iter()
                .all(|m| m.source.platform == first.source.platform) =>
        {
            first.source.platform.clone()
        }
        _ => STAGE.into(),
    };
    // A merge is presented no more firmly than its weakest input: an assistant suggestion stays one,
    // and a note copied from another host is never promoted to something the user said.
    let origin = [ORIGIN_ASSISTANT, ORIGIN_IMPORTED, ORIGIN_INFERRED]
        .into_iter()
        .find(|origin| inputs.iter().any(|m| m.source.origin == *origin))
        .unwrap_or(ORIGIN_USER);
    let latest = inputs
        .iter()
        .max_by(|a, b| statement_order(a).cmp(&statement_order(b)))
        .expect("a merge has inputs");
    let effective_at = match inputs.first() {
        Some(first) if inputs.iter().all(|m| m.effective_at == first.effective_at) => {
            first.effective_at.clone()
        }
        _ => String::new(),
    };
    MemoryRec {
        id: ctx.new_id.into(),
        mtype: T_FACT.into(),
        text: text.into(),
        scope: MemScope::project(ctx.project),
        status: S_CURRENT.into(),
        effective_at,
        recorded_at: ctx.now.into(),
        source: Source {
            platform,
            turn_ids: turn_ids.clone(),
            origin: origin.into(),
        },
        entity_ids,
        stated_at: latest.stated_at.clone(),
        stated_pos: latest.stated_pos,
        last_accessed_at: ctx.now.into(),
        proposal: Some(json!({"runId": ctx.run_id, "proposalId": ctx.proposal_id})),
        derivation: Some(Derivation {
            stage: STAGE.into(),
            input_memory_ids: inputs.iter().map(|m| m.id.clone()).collect(),
            input_turn_ids: turn_ids,
            // A historical restatement: a current view would be staled by its own inputs' retirement.
            current_view: false,
        }),
        ..Default::default()
    }
}

/// The record changes of applying `op` to `records` (its inputs as stored now). Every input must still
/// be an eligible candidate and the op must still validate; `Err` is the reason to skip it.
///
/// - merge: a new current fact restating the inputs, with `derivation {stage:"consolidation",
///   current_view:false}`; each input becomes `superseded` with `supersededBy` the new fact.
/// - supersede: `old` becomes `superseded` with `supersededBy: new` and `validUntil` the new value's
///   effective time (its statement time when unknown); `new.supersedes = old`, unless `new` already
///   replaces another record (a chain cannot fork).
/// - conflict: an open question naming the members, which become `conflict`. No `related` edges: the
///   startup repair would open a second question for every edge pair.
pub fn plan_apply(
    op: &ConsolidationOp,
    records: &[MemoryRec],
    ctx: &ApplyContext,
) -> Result<ApplyPlan, String> {
    let mut inputs = Vec::new();
    for id in op.ids() {
        let m = records
            .iter()
            .find(|m| m.id == id)
            .ok_or_else(|| format!("{id} no longer exists"))?;
        if !eligible(m, ctx.project, ctx.disputed) {
            let why = if ctx.disputed.contains(id) {
                "named by an open question".to_string()
            } else {
                m.status.clone()
            };
            return Err(format!("{id} is no longer a candidate ({why})"));
        }
        inputs.push(m);
    }
    check_op(records, op)?;
    let get = |id: &str| *inputs.iter().find(|m| m.id == id).expect("checked input");
    let mut plan = ApplyPlan::default();
    match op {
        ConsolidationOp::Merge { text, .. } => {
            let merged = merged_record(&inputs, text, ctx);
            for m in &inputs {
                plan.patches.push(FieldPatch::over(
                    m,
                    &[
                        ("status", json!(S_SUPERSEDED)),
                        ("supersededBy", json!(merged.id)),
                    ],
                ));
            }
            plan.created = Some(merged);
        }
        ConsolidationOp::Supersede { old, new } => {
            let (old, new) = (get(old), get(new));
            if let Some(previous) = &new.supersedes {
                return Err(format!(
                    "{} already replaces {previous}; a supersede chain cannot fork",
                    new.id
                ));
            }
            let until = if new.effective_at.is_empty() {
                &new.stated_at
            } else {
                &new.effective_at
            };
            plan.patches.push(FieldPatch::over(
                old,
                &[
                    ("status", json!(S_SUPERSEDED)),
                    ("supersededBy", json!(new.id)),
                    ("validUntil", json!(until)),
                ],
            ));
            plan.patches
                .push(FieldPatch::over(new, &[("supersedes", json!(old.id))]));
        }
        ConsolidationOp::Conflict { ids, reason } => {
            let mut members = ids.clone();
            members.sort();
            for id in &members {
                plan.patches
                    .push(FieldPatch::over(get(id), &[("status", json!(S_CONFLICT))]));
            }
            plan.uncertainty = Some(UncertaintyRec {
                id: ctx.new_id.into(),
                memory_ids: members,
                scope: MemScope::project(ctx.project),
                reason: reason.clone(),
                status: "open".into(),
                created_at: ctx.now.into(),
            });
        }
        ConsolidationOp::Keep { .. } => {}
    }
    Ok(plan)
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RollbackPlan {
    /// Inverse patches; a merged record is retired to `superseded`, never deleted.
    pub patches: Vec<FieldPatch>,
    /// The question to withdraw.
    pub withdraw: Option<String>,
}

/// The inverse of an applied op, or the reason to leave it applied: the merged record was deleted,
/// corrected or consumed by a later run; the question was resolved by a person; another open question
/// names a record; or any field apply wrote has changed since. Rollback never overrides a later decision.
pub fn plan_rollback(
    applied: &AppliedOp,
    records: &[MemoryRec],
    uncertainty: Option<&UncertaintyRec>,
    disputed_elsewhere: &HashSet<String>,
) -> Result<RollbackPlan, String> {
    let get = |id: &str| records.iter().find(|m| m.id == id);
    let mut plan = RollbackPlan::default();
    if let Some(id) = &applied.merged_id {
        let merged = get(id).ok_or_else(|| format!("merged record {id} was deleted"))?;
        if merged.status != S_CURRENT {
            return Err(match &merged.superseded_by {
                Some(next) => format!("merged record {id} was replaced by {next}"),
                None => format!("merged record {id} is {}", merged.status),
            });
        }
        plan.patches
            .push(FieldPatch::over(merged, &[("status", json!(S_SUPERSEDED))]));
    }
    if let Some(id) = &applied.uncertainty_id {
        let question = uncertainty
            .filter(|u| &u.id == id)
            .ok_or_else(|| format!("question {id} no longer exists"))?;
        if question.status != "open" {
            return Err(format!(
                "question {id} is {}; a person's decision is kept",
                question.status
            ));
        }
        plan.withdraw = Some(id.clone());
    }
    for patch in &applied.patches {
        let m = get(&patch.id).ok_or_else(|| format!("{} was deleted", patch.id))?;
        if disputed_elsewhere.contains(&patch.id) {
            return Err(format!("{} is named by another open question", patch.id));
        }
        if let Some(key) = patch
            .after
            .iter()
            .find(|(k, v)| field(m, k) != **v)
            .map(|(k, _)| k)
        {
            return Err(format!("{}.{key} changed since apply", patch.id));
        }
        plan.patches.push(patch.inverse());
    }
    Ok(plan)
}

/// Patches for the inputs of `merged` when it is deleted. Inputs still pointing at it lose that link.
/// When the deletion started from one of its inputs (`restore`), the inputs that survive return to
/// `current`: only the merged text held the deleted content. Otherwise they stay `superseded`, as a
/// deleted chain head revives nothing. Ids in `deleted` are skipped.
pub fn release_inputs(
    merged: &MemoryRec,
    inputs: &[MemoryRec],
    deleted: &HashSet<String>,
    restore: bool,
) -> Vec<FieldPatch> {
    inputs
        .iter()
        .filter(|m| !deleted.contains(&m.id))
        .filter(|m| m.superseded_by.as_deref() == Some(merged.id.as_str()))
        .map(|m| {
            if restore {
                FieldPatch::over(
                    m,
                    &[("status", json!(S_CURRENT)), ("supersededBy", Value::Null)],
                )
            } else {
                FieldPatch::over(m, &[("supersededBy", Value::Null)])
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fact(id: &str, text: &str, stated_at: &str, origin: &str) -> MemoryRec {
        MemoryRec {
            id: id.into(),
            mtype: T_FACT.into(),
            text: text.into(),
            scope: MemScope::project("p"),
            status: S_CURRENT.into(),
            stated_at: stated_at.into(),
            source: Source {
                platform: "codex".into(),
                turn_ids: vec![format!("turn-{id}")],
                origin: origin.into(),
            },
            ..Default::default()
        }
    }

    fn records() -> Vec<MemoryRec> {
        vec![
            fact(
                "a",
                "User deploys with Ansible",
                "2026-01-01T00:00:00Z",
                ORIGIN_USER,
            ),
            fact(
                "b",
                "User deploys with Ansible on Fridays",
                "2026-02-01T00:00:00Z",
                ORIGIN_ASSISTANT,
            ),
            fact(
                "c",
                "User's CI runs on Jenkins",
                "2026-01-01T00:00:00Z",
                ORIGIN_USER,
            ),
            fact(
                "d",
                "User's CI now runs on GitHub Actions",
                "2026-03-01T00:00:00Z",
                ORIGIN_USER,
            ),
        ]
    }

    fn ctx<'a>(new_id: &'a str, disputed: &'a HashSet<String>) -> ApplyContext<'a> {
        ApplyContext {
            project: "p",
            run_id: "run",
            proposal_id: "prop",
            new_id,
            now: "2026-04-01T00:00:00Z",
            disputed,
        }
    }

    fn merge() -> ConsolidationOp {
        ConsolidationOp::Merge {
            ids: vec!["a".into(), "b".into()],
            text: "User deploys with Ansible on Fridays".into(),
        }
    }

    /// `records` after `patches` (and a created record) are written.
    fn after(records: &[MemoryRec], plan: &ApplyPlan) -> Vec<MemoryRec> {
        let mut out: Vec<MemoryRec> = records
            .iter()
            .map(|m| {
                let mut doc = serde_json::to_value(m).unwrap();
                for patch in plan.patches.iter().filter(|p| p.id == m.id) {
                    for (k, v) in &patch.after {
                        if v.is_null() {
                            doc.as_object_mut().unwrap().remove(k);
                        } else {
                            doc[k] = v.clone();
                        }
                    }
                }
                serde_json::from_value(doc).unwrap()
            })
            .collect();
        out.extend(plan.created.clone());
        out
    }

    #[test]
    fn merge_creates_a_historical_restatement_and_supersedes_inputs() {
        let disputed = HashSet::new();
        let plan = plan_apply(&merge(), &records(), &ctx("m1", &disputed)).unwrap();
        let merged = plan.created.as_ref().unwrap();
        let derivation = merged.derivation.as_ref().unwrap();
        assert_eq!(derivation.stage, STAGE);
        assert!(!derivation.current_view);
        assert_eq!(derivation.input_memory_ids, ["a", "b"]);
        assert_eq!(merged.supersedes, None);
        assert_eq!(merged.status, S_CURRENT);
        // The assistant's suggestion stays a suggestion; the latest statement time is kept.
        assert_eq!(merged.source.origin, ORIGIN_ASSISTANT);
        assert_eq!(merged.stated_at, "2026-02-01T00:00:00Z");
        assert_eq!(merged.source.turn_ids, ["turn-a", "turn-b"]);
        assert_eq!(
            merged.proposal,
            Some(json!({"runId":"run","proposalId":"prop"}))
        );
        assert!(crate::integrity::current_dependency(merged).is_none());
        assert_eq!(plan.patches.len(), 2);
        assert_eq!(
            plan.patches[0].before,
            json!({"status":"current","supersededBy":null})
                .as_object()
                .unwrap()
                .clone()
        );
        assert_eq!(plan.patches[0].after["supersededBy"], "m1");
        assert!(is_merge(merged));
    }

    #[test]
    fn a_merge_with_an_imported_input_is_never_presented_as_user() {
        let disputed = HashSet::new();
        let origin = |a: &str, b: &str| {
            let mut inputs = records();
            inputs[0].source.origin = a.into();
            inputs[1].source.origin = b.into();
            let plan = plan_apply(&merge(), &inputs, &ctx("m1", &disputed)).unwrap();
            plan.created.unwrap().source.origin
        };
        assert_eq!(origin(ORIGIN_USER, ORIGIN_IMPORTED), ORIGIN_IMPORTED);
        assert_eq!(origin(ORIGIN_IMPORTED, ORIGIN_INFERRED), ORIGIN_IMPORTED);
        // An assistant suggestion is weaker still.
        assert_eq!(origin(ORIGIN_IMPORTED, ORIGIN_ASSISTANT), ORIGIN_ASSISTANT);
        assert_eq!(origin(ORIGIN_USER, ORIGIN_USER), ORIGIN_USER);
    }

    #[test]
    fn supersede_links_one_predecessor_and_cannot_fork() {
        let disputed = HashSet::new();
        let op = ConsolidationOp::Supersede {
            old: "c".into(),
            new: "d".into(),
        };
        let plan = plan_apply(&op, &records(), &ctx("", &disputed)).unwrap();
        assert_eq!(plan.patches[0].after["validUntil"], "2026-03-01T00:00:00Z");
        assert_eq!(plan.patches[1].after["supersedes"], "c");
        let mut forked = records();
        forked[3].supersedes = Some("x".into());
        assert!(plan_apply(&op, &forked, &ctx("", &disputed))
            .unwrap_err()
            .contains("cannot fork"));
    }

    #[test]
    fn conflict_opens_one_question_without_edges() {
        let disputed = HashSet::new();
        let op = ConsolidationOp::Conflict {
            ids: vec!["d".into(), "c".into()],
            reason: "CI system differs".into(),
        };
        let plan = plan_apply(&op, &records(), &ctx("u1", &disputed)).unwrap();
        let question = plan.uncertainty.as_ref().unwrap();
        assert_eq!(question.memory_ids, ["c", "d"]);
        assert_eq!(question.status, "open");
        assert!(plan
            .patches
            .iter()
            .all(|p| p.after.keys().collect::<Vec<_>>() == ["status"]));
        assert_eq!(plan.applied().uncertainty_id.as_deref(), Some("u1"));
    }

    #[test]
    fn changed_inputs_are_skipped() {
        let disputed = HashSet::from(["a".to_string()]);
        assert!(plan_apply(&merge(), &records(), &ctx("m1", &disputed))
            .unwrap_err()
            .contains("open question"));
        let none = HashSet::new();
        let mut superseded = records();
        superseded[1].status = S_SUPERSEDED.into();
        assert!(plan_apply(&merge(), &superseded, &ctx("m1", &none)).is_err());
        assert!(plan_apply(&merge(), &records()[1..], &ctx("m1", &none))
            .unwrap_err()
            .contains("no longer exists"));
    }

    #[test]
    fn rollback_inverts_an_untouched_merge() {
        let none = HashSet::new();
        let plan = plan_apply(&merge(), &records(), &ctx("m1", &none)).unwrap();
        let now = after(&records(), &plan);
        let rollback = plan_rollback(&plan.applied(), &now, None, &none).unwrap();
        assert_eq!(rollback.patches[0].id, "m1");
        assert_eq!(rollback.patches[0].after["status"], S_SUPERSEDED);
        assert_eq!(rollback.patches[1], plan.patches[0].inverse());
        assert_eq!(rollback.patches[1].after["status"], S_CURRENT);
        assert!(rollback.patches[1].after["supersededBy"].is_null());
    }

    #[test]
    fn rollback_keeps_later_decisions() {
        let none = HashSet::new();
        let plan = plan_apply(&merge(), &records(), &ctx("m1", &none)).unwrap();
        let applied = plan.applied();
        let now = after(&records(), &plan);
        // A user corrected the merged record.
        let mut corrected = now.clone();
        corrected[4].status = S_SUPERSEDED.into();
        corrected[4].superseded_by = Some("correction".into());
        assert!(plan_rollback(&applied, &corrected, None, &none)
            .unwrap_err()
            .contains("replaced by correction"));
        // The merged record was deleted.
        assert!(plan_rollback(&applied, &now[..4], None, &none)
            .unwrap_err()
            .contains("deleted"));
        // An input's link was changed by someone else.
        let mut relinked = now.clone();
        relinked[0].superseded_by = None;
        assert!(plan_rollback(&applied, &relinked, None, &none)
            .unwrap_err()
            .contains("a.supersededBy changed"));
    }

    #[test]
    fn rollback_withdraws_only_open_questions() {
        let none = HashSet::new();
        let op = ConsolidationOp::Conflict {
            ids: vec!["c".into(), "d".into()],
            reason: "CI system differs".into(),
        };
        let plan = plan_apply(&op, &records(), &ctx("u1", &none)).unwrap();
        let now = after(&records(), &plan);
        let mut question = plan.uncertainty.clone().unwrap();
        let rollback = plan_rollback(&plan.applied(), &now, Some(&question), &none).unwrap();
        assert_eq!(rollback.withdraw.as_deref(), Some("u1"));
        assert!(rollback
            .patches
            .iter()
            .all(|p| p.after["status"] == S_CURRENT));
        let elsewhere = HashSet::from(["c".to_string()]);
        assert!(plan_rollback(&plan.applied(), &now, Some(&question), &elsewhere).is_err());
        question.status = "resolved".into();
        assert!(plan_rollback(&plan.applied(), &now, Some(&question), &none)
            .unwrap_err()
            .contains("decision is kept"));
        assert!(plan_rollback(&plan.applied(), &now, None, &none).is_err());
    }

    #[test]
    fn deleting_a_merge_releases_its_inputs() {
        let none = HashSet::new();
        let plan = plan_apply(&merge(), &records(), &ctx("m1", &none)).unwrap();
        let now = after(&records(), &plan);
        let merged = &now[4];
        // Deleting the merged record itself: inputs stay superseded and lose the link.
        let kept = release_inputs(merged, &now, &HashSet::from(["m1".into()]), false);
        assert_eq!(kept.len(), 2);
        assert!(kept
            .iter()
            .all(|p| p.after.keys().collect::<Vec<_>>() == ["supersededBy"]));
        // Deleting input a: the merge goes too, and b comes back.
        let restored = release_inputs(
            merged,
            &now,
            &HashSet::from(["a".into(), "m1".into()]),
            true,
        );
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].id, "b");
        assert_eq!(restored[0].after["status"], S_CURRENT);
    }
}
