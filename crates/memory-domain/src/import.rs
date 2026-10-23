//! Importing memories an agent host already holds (docs/design/memory-import.md): the request, run and
//! proposal shapes shared by the `agent-memoryd import` client and the daemon's `/admin/import_*`
//! endpoints. Pure data and small pure mappings; the use cases live in `memory_engine::app::import`.
use crate::types::InMessage;
use memory_rules::import::units::{turn_key, UnitLimits};
use memory_rules::util;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub mod apply;

/// Most units one `import_propose` request may carry; a longer plan is sent in several batches.
pub const MAX_BATCH_UNITS: usize = 64;
/// Largest unit text one extraction call takes (design §6.6); the plan cuts longer sections into parts.
pub const MAX_EXTRACT_BYTES: usize = 8 * 1024;

/// How a unit becomes proposals: its text is one proposal, its precomputed pieces are one proposal
/// each (no model call either way), or one generation call extracts the items.
pub const MODE_VERBATIM: &str = "verbatim";
pub const MODE_SPLIT: &str = "split";
pub const MODE_EXTRACT: &str = "extract";
/// A past session turn (design §6.10): no proposal text to review and no model call at propose; apply
/// queues it into the dialogue outbox, and Mem0 formation turns it into facts like a captured turn.
pub const MODE_SESSION_TURN: &str = "session-turn";
/// The `itemKind` of a session turn's proposal row.
pub const ITEM_SESSION_TURN: &str = "session-turn";

/// Run statuses. Batches of a plan arrive while the run is `proposing`; only a `proposed` run can be
/// applied, and an interrupted apply or rollback resumes from its in-progress status.
pub const RUN_PROPOSING: &str = "proposing";
pub const RUN_PROPOSED: &str = "proposed";
pub const RUN_APPLYING: &str = "applying";
pub const RUN_APPLIED: &str = "applied";
pub const RUN_ROLLING_BACK: &str = "rolling_back";
pub const RUN_ROLLED_BACK: &str = "rolled_back";
pub const RUN_PARTIALLY_ROLLED_BACK: &str = "partially_rolled_back";

/// Proposal statuses (rows of the shared `proposals` table with `kind: "import"`).
pub const PROPOSED: &str = "proposed";
/// Failed the size or provenance checks when proposed; never applied.
pub const REJECTED: &str = "rejected";
/// Declined at apply time, by a review decision or by the proposal's default decision.
pub const DROPPED: &str = "dropped";
pub const APPLIED: &str = "applied";
/// Rechecked at apply time and left alone (`skipReason`).
pub const SKIPPED: &str = "skipped";
pub const ROLLED_BACK: &str = "rolled_back";

/// Review decisions.
pub const ACCEPT: &str = "accept";
pub const REJECT: &str = "reject";

/// Unit outcomes recorded on the run.
pub const UNIT_NEW: &str = "new";
pub const UNIT_CHANGED: &str = "changed";
pub const UNIT_UNCHANGED: &str = "unchanged";
/// Unchanged text whose last import applied nothing, proposed again on request (`reconsider`).
pub const UNIT_RECONSIDERED: &str = "reconsidered";
pub const UNIT_FAILED: &str = "failed";

/// The `kind` of every import row in the shared `proposals` and run tables.
pub const KIND: &str = "import";

/// Unit size limits on the wire; see `memory_rules::import::units::UnitLimits`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportLimits {
    pub recall_budget: usize,
    pub recall_top_k: usize,
}

impl Default for ImportLimits {
    fn default() -> Self {
        let limits = UnitLimits::default();
        Self {
            recall_budget: limits.recall_budget,
            recall_top_k: limits.recall_top_k,
        }
    }
}

impl From<ImportLimits> for UnitLimits {
    fn from(limits: ImportLimits) -> Self {
        Self {
            recall_budget: limits.recall_budget,
            recall_top_k: limits.recall_top_k,
        }
    }
}

/// One unit of a plan: the text that becomes one record (verbatim), the section whose precomputed
/// pieces become one record each (split), or the input of one extraction call (extract). The client
/// redacts every text before sending it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportUnit {
    /// `memory_rules::import::units::unit_key(host, nativeId, locator)`; with the project, the ledger identity.
    pub unit: String,
    /// Platform label of the host the memory came from (`claude-code`, `codex`, `workbuddy`, ...).
    pub host: String,
    pub mode: String,
    pub text: String,
    /// Split mode only: the pieces, each one proposal.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pieces: Vec<String>,
    /// RFC3339 time the source states the memory; empty when unknown.
    #[serde(default)]
    pub stated_at: String,
    /// The source's own classification (Claude `type`, an export line's `kind`), when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind_hint: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub heading_path: Vec<String>,
    /// blake3 of the whole source file when the plan was made, and of `text`.
    pub file_hash: String,
    pub unit_hash: String,
    /// Also offer the unit as an explicit-instruction candidate (Claude `feedback` notes, exported instructions).
    #[serde(default)]
    pub instruction_candidate: bool,
    /// Provenance copied into the record's `detail.import`: path, nativeId, title, cwd, via, line, ...
    #[serde(default)]
    pub source: Value,
    /// Session-turn mode only: the turn as dialogue capture would queue it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<SessionTurn>,
}

/// One past session turn (design §6.10): the user's prompt and the final reply, with the host's original
/// times, under the turn id capture gives the same turn (`turn_key(project, session, nativeId)`), so the
/// daemon's `platform + turnId` deduplication finds a turn capture already sent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTurn {
    pub turn_id: String,
    pub session: String,
    pub native_id: String,
    pub messages: Vec<InMessage>,
}

/// Why `turn` cannot be queued as a turn of `project`: it must be the user's prompt and then the final reply,
/// both nonblank with RFC3339 times in order, under the turn id capture would give it. `None` when it can.
pub fn session_turn_problem(turn: &SessionTurn, project: &str) -> Option<String> {
    let parts = [&turn.session, &turn.native_id];
    if parts.iter().any(|part| part.trim().is_empty()) {
        return Some("a session turn needs its session and nativeId".into());
    }
    if turn.turn_id != turn_key(project, &turn.session, &turn.native_id) {
        return Some(format!(
            "turnId must be the capture turn key {:?}",
            turn_key(project, &turn.session, &turn.native_id)
        ));
    }
    let roles: Vec<&str> = turn.messages.iter().map(|m| m.role.as_str()).collect();
    if roles != ["user", "assistant"] {
        return Some("a session turn is one user message and then one assistant message".into());
    }
    if turn.messages.iter().any(|m| m.content.trim().is_empty()) {
        return Some("a session turn's messages must not be blank".into());
    }
    let times: Option<Vec<_>> = turn
        .messages
        .iter()
        .map(|m| util::parse_ts_fixed(&m.timestamp))
        .collect();
    match times {
        None => Some("message timestamps must be RFC3339".into()),
        Some(times) if times[0] > times[1] => {
            Some("the reply must not be older than the prompt".into())
        }
        Some(_) => None,
    }
}

/// Whether `p` is a session turn's row rather than a proposal to review.
pub fn is_session_turn(p: &ImportProposal) -> bool {
    p.mode == MODE_SESSION_TURN
}

/// `POST /admin/import_propose`: one batch of a plan's units. The first batch omits `runId` and
/// creates the run; later batches name it; the batch with `finish` makes the run `proposed`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportProposeReq {
    pub project: String,
    pub host: String,
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub finish: bool,
    #[serde(default)]
    pub limits: ImportLimits,
    #[serde(default)]
    pub plan_hash: Option<String>,
    /// Propose again the units whose last import applied nothing (every proposal declined or rejected,
    /// or nothing extracted), although their text is unchanged.
    #[serde(default)]
    pub reconsider: bool,
    #[serde(default)]
    pub units: Vec<ImportUnit>,
}

/// `import_run`, `import_apply` and `import_rollback`. `decisions` (apply only) maps proposal ids to
/// `accept` or `reject`; a proposal without one gets its `defaultDecision`. `scopes` (apply only) maps the
/// ids of accepted instructions to `project` or `global`; an instruction without one is the project's,
/// whatever the extraction suggested.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportRunReq {
    pub run_id: String,
    #[serde(default)]
    pub decisions: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub scopes: BTreeMap<String, String>,
}

/// `import_runs`: the newest runs first, optionally of one project.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportRunsReq {
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// One item the extraction model returned for a unit, before the provenance checks.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractedItem {
    pub text: String,
    pub kind: String,
    pub scope: String,
    pub evidence: String,
}

/// The result of one extraction call, as cached and as recorded on the run.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractedUnit {
    pub items: Vec<ExtractedItem>,
    #[serde(default)]
    pub response_log_id: Option<String>,
    pub input_hash: String,
    pub prompt_format: String,
    pub prompt_hash: String,
    pub model: String,
}

/// An existing memory shown next to a proposal for review.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimilarMemory {
    pub memory_id: String,
    pub text: String,
    pub score: f32,
}

/// A current record an earlier import of a changed unit wrote or linked, shown to the reviewer: it stays
/// current unless a proposal of the unit replaces it (`supersedes`) or a person retracts it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordRef {
    pub memory_id: String,
    #[serde(rename = "type")]
    pub mtype: String,
    pub text: String,
}

/// One proposal: a row of the shared `proposals` table with `kind: "import"`. `cluster` is the unit's
/// index in the run and `position` the item's place in the unit, the order `proposals_for_run` returns.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportProposal {
    pub proposal_id: String,
    pub run_id: String,
    pub kind: String,
    pub project: String,
    pub host: String,
    pub cluster: usize,
    pub position: usize,
    pub unit: String,
    pub mode: String,
    /// `fact`, `preference`, `instruction` or `reference`.
    pub item_kind: String,
    /// `project` or `global`. A hint only: facts are written project-scoped.
    pub scope_hint: String,
    pub text: String,
    #[serde(default)]
    pub evidence: String,
    #[serde(default)]
    pub stated_at: String,
    #[serde(default)]
    pub heading_path: Vec<String>,
    pub unit_hash: String,
    pub file_hash: String,
    #[serde(default)]
    pub source: Value,
    pub status: String,
    #[serde(default)]
    pub reject_reason: Option<String>,
    /// What apply does without a review decision: `accept`, or `reject` for instruction candidates.
    pub default_decision: String,
    /// A current imported record of the project with the same text.
    #[serde(default)]
    pub duplicate_of: Option<String>,
    /// A verbatim unit that changed since an earlier import: the record this one replaces.
    #[serde(default)]
    pub supersedes: Option<String>,
    #[serde(default)]
    pub similar: Vec<SimilarMemory>,
    #[serde(default)]
    pub response_log_id: Option<String>,
    pub created_at: String,
    /// The decision apply used.
    #[serde(default)]
    pub decision: Option<String>,
    /// The record apply wrote, or the duplicate the unit was linked to.
    #[serde(default)]
    pub memory_id: Option<String>,
    #[serde(default)]
    pub skip_reason: Option<String>,
    #[serde(default)]
    pub rollback_skip_reason: Option<String>,
    /// The values apply replaced, so rollback can restore them.
    #[serde(default)]
    pub applied: Option<Value>,
    #[serde(default)]
    pub applied_at: Option<String>,
    #[serde(default)]
    pub rolled_back_at: Option<String>,
    /// What rollback changed: the field patches it wrote, or the ledger link it removed.
    #[serde(default)]
    pub rollback: Option<Value>,
    /// A session turn's row: the turn apply queues.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<SessionTurn>,
}

/// What propose did with one unit.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnitOutcome {
    pub unit: String,
    pub index: usize,
    pub mode: String,
    /// `new`, `changed`, `unchanged` or `failed`.
    pub outcome: String,
    pub proposals: usize,
    pub rejected: usize,
    /// Extract mode: share of the unit text covered by the evidence of accepted items.
    #[serde(default)]
    pub coverage: Option<f64>,
    /// Extract mode: the items came from the extraction cache, without a model call.
    #[serde(default)]
    pub cached: bool,
    #[serde(default)]
    pub response_log_id: Option<String>,
    /// A changed unit: the current records its previous text became that no proposal of this run replaces,
    /// for the reviewer (`source-changed`); `previous_records` has their texts.
    #[serde(default)]
    pub previous_memory_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub previous_records: Vec<RecordRef>,
    #[serde(default)]
    pub error: Option<String>,
    /// The unit's `unitHash` and `fileHash` as proposed; apply records them in the ledger.
    #[serde(default)]
    pub unit_hash: String,
    #[serde(default)]
    pub file_hash: String,
    /// The ledger row the unit had when proposed: the run that imported it and that run's unit hash
    /// (`None`: never imported). Apply skips the unit when another run has imported it since.
    #[serde(default)]
    pub ledger_run_id: Option<String>,
    #[serde(default)]
    pub ledger_unit_hash: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunCounts {
    pub units: usize,
    pub unchanged: usize,
    pub proposals: usize,
    pub rejected: usize,
    pub instruction_candidates: usize,
    pub duplicates: usize,
    pub generation_calls: usize,
    pub cache_hits: usize,
    pub applied: usize,
    pub dropped: usize,
    pub skipped: usize,
    pub rolled_back: usize,
    /// Units whose extraction failed; they have no proposals and are proposed again next time.
    #[serde(default)]
    pub failed: usize,
    /// Session turns registered, queued by apply and rolled back. They are counted here only, not as
    /// proposals: whether the daemon formed one is on its row (`applied.requestId`, `applied.duplicate`).
    #[serde(default)]
    pub session_turns: usize,
    #[serde(default)]
    pub turns_queued: usize,
    #[serde(default)]
    pub turns_rolled_back: usize,
}

/// One import run (`import_runs` table).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportRun {
    pub run_id: String,
    pub kind: String,
    pub project: String,
    pub host: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub finished_at: Option<String>,
    pub limits: ImportLimits,
    #[serde(default)]
    pub plan_hash: Option<String>,
    #[serde(default)]
    pub llm_model: String,
    #[serde(default)]
    pub embedding_model: String,
    #[serde(default)]
    pub prompt_format: Option<String>,
    #[serde(default)]
    pub prompt_hash: Option<String>,
    #[serde(default)]
    pub units: Vec<UnitOutcome>,
    #[serde(default)]
    pub counts: RunCounts,
    #[serde(default)]
    pub error: Option<String>,
}

/// Item kind of a verbatim or split unit from its source's classification: Claude `user` and
/// `feedback` notes are preferences, `reference` notes and kinds an export already uses pass through,
/// and everything else is a fact.
pub fn item_kind_for_hint(hint: Option<&str>) -> &'static str {
    match hint {
        Some("user" | "feedback" | "preference") => "preference",
        Some("reference") => "reference",
        Some("instruction") => "instruction",
        _ => "fact",
    }
}

/// Apply's decision when the review names none: an explicit instruction needs a person's yes.
pub fn default_decision(item_kind: &str) -> &'static str {
    if item_kind == "instruction" {
        REJECT
    } else {
        ACCEPT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instructions_need_a_yes() {
        assert_eq!(default_decision("instruction"), REJECT);
        assert_eq!(default_decision("fact"), ACCEPT);
    }

    fn turn() -> SessionTurn {
        let message = |role: &str, content: &str, timestamp: &str| InMessage {
            role: role.into(),
            content: content.into(),
            timestamp: timestamp.into(),
        };
        SessionTurn {
            turn_id: "proj/s%201/p1".into(),
            session: "s 1".into(),
            native_id: "p1".into(),
            messages: vec![
                message("user", "Where do releases go?", "2026-05-03T09:00:00Z"),
                message(
                    "assistant",
                    "To the staging bucket.",
                    "2026-05-03T09:00:05+00:00",
                ),
            ],
        }
    }

    #[test]
    fn session_turns_must_be_a_prompt_and_its_reply_under_the_capture_key() {
        assert_eq!(session_turn_problem(&turn(), "proj"), None);
        let problem = |edit: &dyn Fn(&mut SessionTurn)| {
            let mut t = turn();
            edit(&mut t);
            session_turn_problem(&t, "proj").unwrap()
        };
        assert!(session_turn_problem(&turn(), "other")
            .unwrap()
            .contains("other/s%201/p1"));
        assert!(problem(&|t| t.native_id = " ".into()).contains("nativeId"));
        assert!(problem(&|t| t.messages.reverse()).contains("one user message"));
        assert!(problem(&|t| t.messages[1].content = "\n".into()).contains("blank"));
        assert!(problem(&|t| t.messages[0].timestamp = "yesterday".into()).contains("RFC3339"));
        assert!(
            problem(&|t| t.messages[0].timestamp = "2026-05-03T10:00:00Z".into()).contains("older")
        );
    }
}
