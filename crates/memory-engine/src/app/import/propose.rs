//! `propose`: one batch of a plan's units becomes unit outcomes and proposals on a `proposing` run. A unit whose
//! hash matches its ledger row is `unchanged` and skipped; a verbatim or split unit offers its text or pieces without
//! a model call; an extract unit costs one generation call through the algorithm, cached by project, unit text,
//! prompt and model, and every item it returns is checked by the pure provenance rules of
//! `memory_rules::import::validate`. A session turn becomes one row without a model call (`turns::register`). The
//! run and its proposal rows commit together; no lock is held during model calls and no memory record is written.
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::Arc;

use super::{
    count, invalid, ledger, ledger_id, live_records, load_run, other_units_naming, run_proposals,
    turns, LEDGER_APPLIED,
};
use crate::runtime::engine::Engine;
use crate::runtime::tenant::Tenant;
use memory_domain::import::apply::{self as plan, INSTRUCTION};
use memory_domain::import::{
    default_decision, is_session_turn, item_kind_for_hint, session_turn_problem, ExtractedUnit,
    ImportProposal, ImportProposeReq, ImportRun, ImportUnit, RecordRef, SimilarMemory, UnitOutcome,
    KIND, MAX_BATCH_UNITS, MAX_EXTRACT_BYTES, MODE_EXTRACT, MODE_SESSION_TURN, MODE_SPLIT,
    MODE_VERBATIM, PROPOSED, REJECTED, RUN_PROPOSED, RUN_PROPOSING, UNIT_CHANGED, UNIT_FAILED,
    UNIT_NEW, UNIT_RECONSIDERED, UNIT_UNCHANGED,
};
use memory_domain::types::*;
use memory_rules::import::markdown;
use memory_rules::import::units::{self, UnitLimits};
use memory_rules::import::validate::{self, ItemRef, MAX_INSTRUCTION_BYTES, MAX_INSTRUCTION_CHARS};
use memory_rules::util;
use memory_store::schema;
use memory_store::store::{hashed_id, Tx};

/// Existing memories shown to the extraction model, and to the reviewer next to each proposal.
const EXTRACTION_CONTEXT: usize = 10;
const SIMILAR: usize = 3;

/// The extraction cache row of a unit: everything the extraction input is made of except the existing
/// memories, which change with every import (the project, which extracted texts name, the unit text and its
/// metadata, the length the limits allow, the prompt variant), plus the prompt and the model.
fn cache_id(
    project: &str,
    unit: &ImportUnit,
    limits: &UnitLimits,
    variant: &str,
    prompt_hash: &str,
    model: &str,
) -> String {
    let parts = [
        project,
        &unit.unit_hash,
        &unit.heading_path.join("\u{1f}"),
        &unit.stated_at,
        unit.kind_hint.as_deref().unwrap_or(""),
        &limits.recall_budget.to_string(),
        &limits.recall_top_k.to_string(),
        variant,
        prompt_hash,
        model,
    ];
    hashed_id("ix", &parts.join("\n"))
}

fn check_limits(limits: &UnitLimits) -> ApiResult<()> {
    if limits.recall_budget < UnitLimits::MIN_RECALL_BUDGET || limits.recall_top_k == 0 {
        return Err(invalid(format!(
            "recallBudget must be at least {} and recallTopK positive",
            UnitLimits::MIN_RECALL_BUDGET
        )));
    }
    Ok(())
}

/// Shape checks of one unit of `project`; the unit hash is filled in when the client left it empty.
fn checked_unit(unit: &ImportUnit, host: &str, project: &str) -> ApiResult<ImportUnit> {
    let name = unit.unit.as_str();
    if name.trim().is_empty() {
        return Err(invalid("every unit needs its unit key"));
    }
    if !unit.host.is_empty() && unit.host != host {
        return Err(invalid(format!(
            "unit {name} is from host {:?}, not the run's host {host:?}",
            unit.host
        )));
    }
    if ![MODE_VERBATIM, MODE_SPLIT, MODE_EXTRACT, MODE_SESSION_TURN].contains(&unit.mode.as_str()) {
        return Err(invalid(format!(
            "unit {name}: mode must be verbatim, split, extract or session-turn"
        )));
    }
    if unit.mode == MODE_SESSION_TURN {
        let Some(turn) = &unit.turn else {
            return Err(invalid(format!("session-turn unit {name} needs its turn")));
        };
        if let Some(problem) = session_turn_problem(turn, project) {
            return Err(invalid(format!("unit {name}: {problem}")));
        }
    }
    if unit.text.trim().is_empty() {
        return Err(invalid(format!("unit {name} has no text")));
    }
    if unit.mode == MODE_SPLIT
        && (unit.pieces.is_empty() || unit.pieces.iter().any(|p| p.trim().is_empty()))
    {
        return Err(invalid(format!("split unit {name} needs nonblank pieces")));
    }
    let hash = units::content_hash(&unit.text);
    if !unit.unit_hash.is_empty() && unit.unit_hash != hash {
        return Err(invalid(format!(
            "unit {name}: unitHash is not the blake3 hex of its text"
        )));
    }
    if !unit.stated_at.is_empty() && util::parse_ts(&unit.stated_at).is_none() {
        return Err(invalid(format!(
            "unit {name}: statedAt must be RFC3339 or YYYY-MM-DD"
        )));
    }
    let mut unit = unit.clone();
    unit.unit_hash = hash;
    unit.host = host.into();
    Ok(unit)
}

/// One text a unit offers, before its proposal row is built.
struct Candidate {
    text: String,
    item_kind: String,
    scope_hint: String,
    evidence: String,
    reject_reason: Option<String>,
    response_log_id: Option<String>,
}

impl Candidate {
    /// A verbatim text or split piece: one record of its own text, if it fits.
    fn verbatim(text: &str, item_kind: &str, limits: &UnitLimits) -> Self {
        let text = text.trim();
        let (cost, cap) = (units::record_cost(text), limits.hard_cap());
        Self {
            text: text.into(),
            item_kind: item_kind.into(),
            scope_hint: "project".into(),
            evidence: text.into(),
            reject_reason: (cost > cap)
                .then(|| format!("text costs {cost}, more than the unit cap {cap}")),
            response_log_id: None,
        }
    }
}

/// Why an instruction text exceeds the explicit-instruction limits, as `validate::check_item` words it.
fn instruction_too_long(text: &str) -> Option<String> {
    let chars = text.chars().count();
    if chars > MAX_INSTRUCTION_CHARS {
        return Some(format!(
            "instruction has {chars} characters, more than {MAX_INSTRUCTION_CHARS}"
        ));
    }
    if text.len() > MAX_INSTRUCTION_BYTES {
        return Some(format!(
            "instruction has {} bytes, more than {MAX_INSTRUCTION_BYTES}",
            text.len()
        ));
    }
    None
}

/// The state of one `propose` request.
struct Proposer<'a> {
    eng: &'a Arc<Engine>,
    t: &'a Arc<Tenant>,
    run: &'a ImportRun,
    limits: UnitLimits,
    prompt_hash: Option<String>,
    /// The tenant's prompt variant (`zh`, `en`), part of the extraction input.
    variant: String,
    reconsider: bool,
    now: String,
    generation_calls: usize,
    cache_hits: usize,
}

/// What a changed unit's earlier import left current, and which of those records its new proposals may
/// replace, by role: the one fact a one-record unit was imported as, and the one instruction a person
/// accepted from it. A record another unit also holds is never a target.
struct Previous {
    live: Vec<MemoryRec>,
    fact: Option<String>,
    instruction: Option<String>,
}

impl Proposer<'_> {
    async fn previous(&self, unit: &ImportUnit, ids: &[String]) -> anyhow::Result<Previous> {
        let project = self.run.project.as_str();
        let live = live_records(self.t, ids).await?;
        let mut targets = [None, None];
        for (slot, wanted) in targets.iter_mut().zip([T_FACT, T_INSTRUCTION]) {
            let of_role: Vec<&MemoryRec> = live
                .iter()
                .filter(|m| m.mtype == wanted && plan::is_import_record(m))
                .collect();
            if let [only] = of_role.as_slice() {
                if other_units_naming(self.t, &only.id, project, &unit.unit)
                    .await?
                    .is_empty()
                {
                    *slot = Some(only.id.clone());
                }
            }
        }
        let [fact, instruction] = targets;
        Ok(Previous {
            live,
            // Only a one-record unit maps its new text onto its old record.
            fact: fact.filter(|_| unit.mode == MODE_VERBATIM),
            instruction,
        })
    }

    /// The unit's outcome and proposals. Errors of the algorithm's model calls fail the unit only.
    async fn unit(
        &mut self,
        unit: &ImportUnit,
        index: usize,
    ) -> ApiResult<(UnitOutcome, Vec<ImportProposal>)> {
        if unit.mode == MODE_SESSION_TURN {
            return Ok(turns::register(self.run, unit, index, &self.now));
        }
        let project = self.run.project.as_str();
        let mut outcome = UnitOutcome {
            unit: unit.unit.clone(),
            index,
            mode: unit.mode.clone(),
            unit_hash: unit.unit_hash.clone(),
            file_hash: unit.file_hash.clone(),
            ..Default::default()
        };
        let row = ledger(self.t, &ledger_id(project, &unit.unit)).await?;
        let previous_ids = match &row {
            Some((_, row)) => {
                outcome.ledger_run_id = Some(row.run_id.clone());
                outcome.ledger_unit_hash = Some(row.unit_hash.clone());
                if row.unit_hash == unit.unit_hash {
                    if row.status == LEDGER_APPLIED || !self.reconsider {
                        outcome.outcome = UNIT_UNCHANGED.into();
                        return Ok((outcome, vec![]));
                    }
                    outcome.outcome = UNIT_RECONSIDERED.into();
                } else {
                    outcome.outcome = UNIT_CHANGED.into();
                }
                row.memory_ids.clone()
            }
            None => {
                outcome.outcome = UNIT_NEW.into();
                vec![]
            }
        };
        let previous = self.previous(unit, &previous_ids).await?;
        let hint_kind = item_kind_for_hint(unit.kind_hint.as_deref());
        let mut candidates = Vec::new();
        let mut fresh_extraction = None;
        match unit.mode.as_str() {
            MODE_VERBATIM => {
                candidates.push(Candidate::verbatim(&unit.text, hint_kind, &self.limits));
            }
            MODE_SPLIT => {
                for piece in &unit.pieces {
                    candidates.push(Candidate::verbatim(piece, hint_kind, &self.limits));
                }
            }
            _ => match self.extract(unit, &mut outcome).await? {
                Some((extracted, cached)) => {
                    if !cached {
                        fresh_extraction = Some(extracted.clone());
                    }
                    let context: Vec<&str> = [project, unit.host.as_str()]
                        .into_iter()
                        .chain(unit.heading_path.iter().map(String::as_str))
                        .chain([unit.stated_at.as_str()])
                        .filter(|s| !s.is_empty())
                        .collect();
                    for item in &extracted.items {
                        let verdict = validate::check_item(
                            &ItemRef {
                                text: &item.text,
                                kind: &item.kind,
                                scope: &item.scope,
                                evidence: &item.evidence,
                            },
                            &unit.text,
                            &context,
                            &self.limits,
                        );
                        candidates.push(Candidate {
                            text: item.text.trim().into(),
                            item_kind: item.kind.clone(),
                            scope_hint: item.scope.clone(),
                            evidence: item.evidence.clone(),
                            reject_reason: verdict.err(),
                            response_log_id: extracted.response_log_id.clone(),
                        });
                    }
                    let quotes: Vec<&str> = candidates
                        .iter()
                        .filter(|c| c.reject_reason.is_none())
                        .map(|c| c.evidence.as_str())
                        .collect();
                    outcome.coverage = Some(validate::coverage(&unit.text, &quotes));
                }
                None => return Ok((outcome, vec![])),
            },
        }
        // Every instruction obeys the explicit-instruction limits, wherever it came from.
        for c in candidates
            .iter_mut()
            .filter(|c| c.item_kind == INSTRUCTION && c.reject_reason.is_none())
        {
            c.reject_reason = instruction_too_long(&c.text);
        }
        let offers_instruction = candidates
            .iter()
            .any(|c| c.item_kind == INSTRUCTION && c.reject_reason.is_none());
        if unit.instruction_candidate && !offers_instruction {
            if let Some(first) = markdown::paragraphs(&unit.text).into_iter().next() {
                candidates.push(Candidate {
                    reject_reason: instruction_too_long(&first),
                    text: first.clone(),
                    item_kind: INSTRUCTION.into(),
                    scope_hint: "project".into(),
                    evidence: first,
                    response_log_id: None,
                });
            }
        }
        // A fresh extraction is cached unless every item failed its checks: such a reply is retried next time.
        if let Some(extracted) = fresh_extraction {
            let usable =
                extracted.items.is_empty() || candidates.iter().any(|c| c.reject_reason.is_none());
            if usable {
                self.cache(unit, &extracted).await?;
            }
        }
        // Each role's earlier record is replaced by the unit's one proposal of that role, if it has one.
        let open = |kind: &str| {
            candidates
                .iter()
                .filter(|c| {
                    c.reject_reason.is_none()
                        && (c.item_kind == INSTRUCTION) == (kind == INSTRUCTION)
                })
                .count()
        };
        let fact_target = previous.fact.clone().filter(|_| open(T_FACT) == 1);
        let instruction_target = previous
            .instruction
            .clone()
            .filter(|_| open(INSTRUCTION) == 1);
        let mut proposals = Vec::with_capacity(candidates.len());
        for (position, c) in candidates.into_iter().enumerate() {
            let mut p = ImportProposal {
                proposal_id: util::new_id("ip"),
                run_id: self.run.run_id.clone(),
                kind: KIND.into(),
                project: project.into(),
                host: unit.host.clone(),
                cluster: index,
                position,
                unit: unit.unit.clone(),
                mode: unit.mode.clone(),
                default_decision: default_decision(&c.item_kind).into(),
                item_kind: c.item_kind,
                scope_hint: c.scope_hint,
                text: c.text,
                evidence: c.evidence,
                stated_at: unit.stated_at.clone(),
                heading_path: unit.heading_path.clone(),
                unit_hash: unit.unit_hash.clone(),
                file_hash: unit.file_hash.clone(),
                source: unit.source.clone(),
                status: if c.reject_reason.is_some() {
                    REJECTED
                } else {
                    PROPOSED
                }
                .into(),
                reject_reason: c.reject_reason,
                response_log_id: c.response_log_id,
                created_at: self.now.clone(),
                ..Default::default()
            };
            if p.status == PROPOSED {
                self.review_context(&mut p, &mut outcome).await?;
                if outcome.outcome == UNIT_FAILED {
                    return Ok((outcome, vec![]));
                }
                p.supersedes = if p.item_kind == INSTRUCTION {
                    instruction_target.clone()
                } else {
                    fact_target.clone()
                };
            }
            proposals.push(p);
        }
        // The current records of the previous text that no proposal replaces stay current; the reviewer sees
        // them (`source-changed`) and can retract one with `correct_memory`.
        let replaced: Vec<&String> = proposals
            .iter()
            .filter_map(|p| p.supersedes.as_ref())
            .collect();
        let kept: Vec<&MemoryRec> = previous
            .live
            .iter()
            .filter(|m| !replaced.contains(&&m.id))
            .collect();
        outcome.previous_memory_ids = kept.iter().map(|m| m.id.clone()).collect();
        outcome.previous_records = kept
            .iter()
            .map(|m| RecordRef {
                memory_id: m.id.clone(),
                mtype: m.mtype.clone(),
                text: m.text.clone(),
            })
            .collect();
        outcome.proposals = proposals.len();
        outcome.rejected = proposals.iter().filter(|p| p.status == REJECTED).count();
        Ok((outcome, proposals))
    }

    fn cache_key(&self, unit: &ImportUnit) -> String {
        cache_id(
            &self.run.project,
            unit,
            &self.limits,
            &self.variant,
            self.prompt_hash.as_deref().unwrap_or_default(),
            &self.eng.llm.model,
        )
    }

    async fn cache(&self, unit: &ImportUnit, extracted: &ExtractedUnit) -> ApiResult<()> {
        let mut tx = Tx::new();
        tx.put(
            schema::IMPORT_EXTRACTIONS,
            &self.cache_key(unit),
            json!({"project": self.run.project, "unit": unit.unit, "unitHash": unit.unit_hash,
                "promptFormat": extracted.prompt_format, "promptHash": self.prompt_hash,
                "variant": self.variant, "limits": self.run.limits,
                "model": self.eng.llm.model, "extracted": extracted, "createdAt": util::now_str()}),
        )?;
        self.t.store.commit(tx).await?;
        Ok(())
    }

    /// The unit's extraction and whether it came from the cache; `None` marks the unit failed. A fresh
    /// extraction is cached by the caller once its items are checked.
    async fn extract(
        &mut self,
        unit: &ImportUnit,
        outcome: &mut UnitOutcome,
    ) -> ApiResult<Option<(ExtractedUnit, bool)>> {
        let project = self.run.project.as_str();
        if unit.text.len() > MAX_EXTRACT_BYTES {
            outcome.outcome = UNIT_FAILED.into();
            outcome.error = Some(format!(
                "the unit text has {} bytes; one extraction takes at most {MAX_EXTRACT_BYTES}, so the plan must split it",
                unit.text.len()
            ));
            return Ok(None);
        }
        if let Some(doc) = self
            .t
            .store
            .get_doc(schema::IMPORT_EXTRACTIONS, &self.cache_key(unit))
            .await?
        {
            if let Ok(extracted) = serde_json::from_value::<ExtractedUnit>(doc["extracted"].clone())
            {
                outcome.cached = true;
                outcome.response_log_id = extracted.response_log_id.clone();
                self.cache_hits += 1;
                return Ok(Some((extracted, true)));
            }
        }
        let algorithm = self.eng.algorithm();
        let existing = match algorithm
            .import_similar(
                self.eng,
                self.t,
                project,
                &unit.host,
                &unit.text,
                EXTRACTION_CONTEXT,
            )
            .await
        {
            Ok(similar) => similar.into_iter().map(|(m, _)| m).collect::<Vec<_>>(),
            Err(e) => {
                outcome.outcome = UNIT_FAILED.into();
                outcome.error = Some(format!("existing memories: {e:#}"));
                return Ok(None);
            }
        };
        self.generation_calls += 1;
        let extracted = match algorithm
            .import_extract(self.eng, self.t, project, unit, &self.limits, &existing)
            .await
        {
            Ok(Some(extracted)) => extracted,
            Ok(None) => {
                outcome.outcome = UNIT_FAILED.into();
                outcome.error =
                    Some("the active memory algorithm does not support import extraction".into());
                return Ok(None);
            }
            Err(e) => {
                outcome.outcome = UNIT_FAILED.into();
                outcome.error = Some(format!("extraction: {e:#}"));
                return Ok(None);
            }
        };
        outcome.response_log_id = extracted.response_log_id.clone();
        Ok(Some((extracted, false)))
    }

    /// The duplicate and nearest memories of a proposal that passed its checks. Accepting a duplicate links
    /// the unit to that record instead of writing another one, so the default decision stays as it is: the
    /// unit keeps its content even if another unit later replaces its own copy.
    async fn review_context(
        &self,
        p: &mut ImportProposal,
        outcome: &mut UnitOutcome,
    ) -> ApiResult<()> {
        let mtype = if p.item_kind == INSTRUCTION {
            T_INSTRUCTION
        } else {
            T_FACT
        };
        let key = units::import_key(&p.project, &p.text);
        p.duplicate_of = self
            .t
            .store
            .current_imported(&p.project, mtype, &key)
            .await?
            .map(|m| m.id);
        match self
            .eng
            .algorithm()
            .import_similar(self.eng, self.t, &p.project, &p.host, &p.text, SIMILAR)
            .await
        {
            Ok(similar) => {
                p.similar = similar
                    .into_iter()
                    .map(|(m, score)| SimilarMemory {
                        memory_id: m.id,
                        text: m.text,
                        score,
                    })
                    .collect()
            }
            Err(e) => {
                outcome.outcome = UNIT_FAILED.into();
                outcome.error = Some(format!("existing memories: {e:#}"));
            }
        }
        Ok(())
    }
}

/// Propose one batch of a plan's units (design §6.7). The first batch creates the run; later batches name
/// it, and the batch with `finish` makes it `proposed`. Returns the run, this batch's proposals and this
/// batch's unit outcomes.
pub async fn propose(
    eng: &Arc<Engine>,
    t: &Arc<Tenant>,
    req: ImportProposeReq,
) -> ApiResult<Value> {
    let project = req.project.trim();
    if project.is_empty() || project == "*" {
        return Err(invalid("project must name one project"));
    }
    let host = req.host.trim();
    if host.is_empty() {
        return Err(invalid("host is required"));
    }
    if req.units.len() > MAX_BATCH_UNITS {
        return Err(invalid(format!(
            "at most {MAX_BATCH_UNITS} units per request; send the plan in batches"
        )));
    }
    let batch = req
        .units
        .iter()
        .map(|unit| checked_unit(unit, host, project))
        .collect::<ApiResult<Vec<_>>>()?;
    let algorithm = eng.algorithm();
    let prompt = algorithm.import_prompt();
    if batch.iter().any(|unit| unit.mode == MODE_EXTRACT) {
        if prompt.is_none() {
            return Err(invalid(
                "the active memory algorithm does not support import extraction",
            ));
        }
        if !eng.llm.is_configured() {
            return Err(ApiError::ModelUnavailable(
                "generation model API key not configured".into(),
            ));
        }
    }
    let now = util::now_str();
    let created = req.run_id.is_none();
    let mut run = match req.run_id.as_deref() {
        Some(id) => {
            let run = load_run(t, id).await?;
            if run.status != RUN_PROPOSING {
                return Err(invalid(format!(
                    "run {id} is {}; only a proposing run takes more units",
                    run.status
                )));
            }
            if run.project != project || run.host != host {
                return Err(invalid(format!(
                    "run {id} imports {} from {}, not {project} from {host}",
                    run.project, run.host
                )));
            }
            run
        }
        None => {
            let limits: UnitLimits = req.limits.into();
            check_limits(&limits)?;
            let (prompt_format, prompt_hash) = prompt.clone().unzip();
            ImportRun {
                run_id: util::new_id("ir"),
                kind: KIND.into(),
                project: project.into(),
                host: host.into(),
                status: RUN_PROPOSING.into(),
                created_at: now.clone(),
                updated_at: now.clone(),
                limits: req.limits,
                plan_hash: req.plan_hash.clone(),
                llm_model: eng.llm.model.clone(),
                embedding_model: eng.models.embed_model_id.clone(),
                prompt_format: prompt_format.map(str::to_string),
                prompt_hash,
                ..Default::default()
            }
        }
    };
    let mut keys: HashSet<&str> = run.units.iter().map(|u| u.unit.as_str()).collect();
    for unit in &batch {
        if !keys.insert(unit.unit.as_str()) {
            return Err(invalid(format!(
                "unit {} appears twice in run {}",
                unit.unit, run.run_id
            )));
        }
    }
    // A turn's row is found by run and turn id: one row per turn.
    let mut turn_ids = HashSet::new();
    if !created {
        for p in run_proposals(t, &run.run_id).await? {
            if let Some(turn) = p.turn {
                turn_ids.insert(turn.turn_id);
            }
        }
    }
    for turn in batch.iter().filter_map(|unit| unit.turn.as_ref()) {
        if !turn_ids.insert(turn.turn_id.clone()) {
            return Err(invalid(format!(
                "turn {} appears twice in run {}",
                turn.turn_id, run.run_id
            )));
        }
    }
    let base = run.units.len();
    let mut proposer = Proposer {
        eng,
        t,
        run: &run,
        limits: run.limits.into(),
        prompt_hash: prompt.map(|(_, hash)| hash),
        variant: eng.prompt_variant_for(t).to_string(),
        reconsider: req.reconsider,
        now: now.clone(),
        generation_calls: 0,
        cache_hits: 0,
    };
    let mut outcomes = Vec::with_capacity(batch.len());
    let mut proposals = Vec::new();
    for (offset, unit) in batch.iter().enumerate() {
        let (outcome, rows) = proposer.unit(unit, base + offset).await?;
        outcomes.push(outcome);
        proposals.extend(rows);
    }
    let (generation_calls, cache_hits) = (proposer.generation_calls, proposer.cache_hits);
    let counts = &mut run.counts;
    counts.units += outcomes.len();
    counts.unchanged += outcomes
        .iter()
        .filter(|o| o.outcome == UNIT_UNCHANGED)
        .count();
    counts.failed += outcomes.iter().filter(|o| o.outcome == UNIT_FAILED).count();
    let turn_rows = proposals.iter().filter(|p| is_session_turn(p)).count();
    counts.proposals += proposals.len() - turn_rows;
    counts.session_turns += turn_rows;
    counts.rejected += count(&proposals, REJECTED);
    counts.instruction_candidates += proposals
        .iter()
        .filter(|p| p.item_kind == INSTRUCTION && p.status == PROPOSED)
        .count();
    counts.duplicates += proposals
        .iter()
        .filter(|p| p.duplicate_of.is_some())
        .count();
    counts.generation_calls += generation_calls;
    counts.cache_hits += cache_hits;
    run.units.extend(outcomes.iter().cloned());
    run.updated_at = now.clone();
    if req.finish {
        run.status = RUN_PROPOSED.into();
        run.finished_at = Some(now);
    }
    let mut doc = serde_json::to_value(&run).map_err(anyhow::Error::from)?;
    let mut tx = Tx::new();
    if created {
        // Orders runs created within the same second.
        doc["createdNs"] = json!(util::now().timestamp_nanos_opt().unwrap_or(0));
        tx.put(schema::IMPORT_RUNS, &run.run_id, doc)?;
    } else {
        tx.merge(schema::IMPORT_RUNS, &run.run_id, doc)?;
    }
    for p in &proposals {
        tx.put(
            schema::PROPOSALS,
            &p.proposal_id,
            serde_json::to_value(p).map_err(anyhow::Error::from)?,
        )?;
    }
    t.store.commit(tx).await?;
    Ok(json!({"run": run, "proposals": proposals, "units": outcomes}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use memory_domain::import::SessionTurn;

    #[test]
    fn units_are_checked_and_hashed() {
        let unit = ImportUnit {
            unit: "codex:t#1".into(),
            mode: MODE_VERBATIM.into(),
            text: "Use tabs.".into(),
            ..Default::default()
        };
        let checked = checked_unit(&unit, "codex", "proj").unwrap();
        assert_eq!(checked.unit_hash, units::content_hash("Use tabs."));
        assert_eq!(checked.host, "codex");
        let bad = |unit: ImportUnit| {
            checked_unit(&unit, "codex", "proj")
                .unwrap_err()
                .to_string()
        };
        assert!(bad(ImportUnit {
            unit: " ".into(),
            ..unit.clone()
        })
        .contains("unit key"));
        assert!(bad(ImportUnit {
            host: "claude-code".into(),
            ..unit.clone()
        })
        .contains("host"));
        assert!(bad(ImportUnit {
            mode: "summarize".into(),
            ..unit.clone()
        })
        .contains("mode"));
        assert!(bad(ImportUnit {
            text: "\n".into(),
            ..unit.clone()
        })
        .contains("no text"));
        assert!(bad(ImportUnit {
            mode: MODE_SPLIT.into(),
            ..unit.clone()
        })
        .contains("pieces"));
        assert!(bad(ImportUnit {
            unit_hash: "abc".into(),
            ..unit.clone()
        })
        .contains("unitHash"));
        assert!(bad(ImportUnit {
            stated_at: "yesterday".into(),
            ..unit.clone()
        })
        .contains("statedAt"));
        assert!(checked_unit(
            &ImportUnit {
                stated_at: "2026-01-02".into(),
                ..unit.clone()
            },
            "codex",
            "proj"
        )
        .is_ok());
        // A session turn needs its turn, under the capture key of the run's project.
        let turn = SessionTurn {
            turn_id: units::turn_key("proj", "s1", "p1"),
            session: "s1".into(),
            native_id: "p1".into(),
            messages: vec![
                InMessage {
                    role: "user".into(),
                    content: "Where do releases go?".into(),
                    timestamp: "2026-01-02T03:04:05Z".into(),
                },
                InMessage {
                    role: "assistant".into(),
                    content: "To staging.".into(),
                    timestamp: "2026-01-02T03:04:09Z".into(),
                },
            ],
        };
        let session = ImportUnit {
            mode: MODE_SESSION_TURN.into(),
            turn: Some(turn.clone()),
            ..unit.clone()
        };
        assert!(checked_unit(&session, "codex", "proj").is_ok());
        assert!(checked_unit(&session, "codex", "other")
            .unwrap_err()
            .to_string()
            .contains("capture turn key"));
        assert!(bad(ImportUnit {
            turn: None,
            ..session
        })
        .contains("needs its turn"));
    }

    #[test]
    fn verbatim_texts_must_fit_the_unit_cap() {
        let limits = UnitLimits::default();
        let fits = Candidate::verbatim(" Use tabs. ", "fact", &limits);
        assert_eq!(
            (fits.text.as_str(), fits.evidence.as_str()),
            ("Use tabs.", "Use tabs.")
        );
        assert_eq!(fits.reject_reason, None);
        let long = Candidate::verbatim(&"汉".repeat(193), "fact", &limits);
        assert_eq!(
            long.reject_reason.as_deref(),
            Some("text costs 201, more than the unit cap 200")
        );
        assert!(instruction_too_long(&"a".repeat(1025))
            .unwrap()
            .contains("1025 characters"));
        assert_eq!(instruction_too_long("Always run make test."), None);
        assert!(check_limits(&UnitLimits {
            recall_budget: 39,
            recall_top_k: 5
        })
        .is_err());
        assert!(check_limits(&UnitLimits {
            recall_budget: 400,
            recall_top_k: 0
        })
        .is_err());
        assert!(check_limits(&limits).is_ok());
    }
}
