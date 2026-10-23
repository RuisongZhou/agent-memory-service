//! Review files (docs/design/memory-import.md §6.7): a run's proposals one per line, for a person to set
//! each `decision` to `accept` or `reject`, and the `scope` of an instruction to `project` or `global`,
//! before apply. Only decisions and instruction scopes are read back: text cannot be rewritten here
//! (correct a record after apply with `correct_memory`). `previous` lines list, after a changed unit's
//! proposals, the records its earlier text became; they are for reading only.
use memory_domain::import::{
    is_session_turn, ImportProposal, ImportRun, SimilarMemory, UnitOutcome, ACCEPT, PROPOSED,
    REJECT, REJECTED,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use super::plan::{file_part, IMPORTS_DIR};
use super::{shown, write_private};
use crate::harness::{invalid, Error};

pub const VERSION: u64 = 1;
/// The item kind whose scope a person chooses.
pub const INSTRUCTION: &str = "instruction";
/// Scopes of an instruction: this project's (the default), or every project's.
pub const PROJECT: &str = "project";
pub const GLOBAL: &str = "global";

/// The first line of a review file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Header {
    #[serde(rename = "type")]
    pub line: String,
    pub version: u64,
    pub run_id: String,
    pub project: String,
    pub host: String,
    pub status: String,
    pub created_at: String,
}

/// One proposal of a review file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProposalLine {
    #[serde(rename = "type")]
    pub line: String,
    pub proposal_id: String,
    /// `accept` or `reject`: the proposal's default, `reject` when it failed its checks.
    pub decision: String,
    pub status: String,
    pub item_kind: String,
    /// An instruction's scope, `project` until a person sets `global`, whatever the hint. Other kinds
    /// show their hint here, and it is not read back: facts are always written to the project.
    pub scope: String,
    /// The scope the source or the extraction suggested; for reading only.
    pub scope_hint: String,
    pub text: String,
    pub evidence: String,
    pub unit: String,
    pub heading_path: Vec<String>,
    pub stated_at: String,
    pub reject_reason: Option<String>,
    pub duplicate_of: Option<String>,
    pub supersedes: Option<String>,
    pub similar: Vec<SimilarMemory>,
}

impl ProposalLine {
    pub fn new(proposal: &ImportProposal) -> Self {
        let decision = match proposal.status == REJECTED {
            true => REJECT.to_string(),
            false => proposal.default_decision.clone(),
        };
        let scope = match proposal.item_kind == INSTRUCTION {
            true => PROJECT.to_string(),
            false => proposal.scope_hint.clone(),
        };
        ProposalLine {
            line: "proposal".into(),
            proposal_id: proposal.proposal_id.clone(),
            decision,
            status: proposal.status.clone(),
            item_kind: proposal.item_kind.clone(),
            scope,
            scope_hint: proposal.scope_hint.clone(),
            text: proposal.text.clone(),
            evidence: proposal.evidence.clone(),
            unit: proposal.unit.clone(),
            heading_path: proposal.heading_path.clone(),
            stated_at: proposal.stated_at.clone(),
            reject_reason: proposal.reject_reason.clone(),
            duplicate_of: proposal.duplicate_of.clone(),
            supersedes: proposal.supersedes.clone(),
            similar: proposal.similar.clone(),
        }
    }
}

/// A current record an earlier import of a changed unit wrote or linked, listed after the unit's
/// proposals. It stays current unless a proposal of the unit supersedes it; it is not read back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviousLine {
    #[serde(rename = "type")]
    pub line: String,
    pub unit: String,
    pub memory_id: String,
    pub record_type: String,
    pub text: String,
}

/// What a review file decides for the proposals it lists as still `proposed`: each one's decision, and
/// `global` for each accepted instruction a person made global.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Review {
    pub decisions: BTreeMap<String, String>,
    pub scopes: BTreeMap<String, String>,
}

/// `<home>/.local/state/agent-memory-service/imports/<run>.review.jsonl`.
pub fn default_path(home: &Path, run_id: &str) -> PathBuf {
    home.join(IMPORTS_DIR)
        .join(format!("{}.review.jsonl", file_part(run_id)))
}

fn json_line(value: &impl Serialize) -> Result<String, Error> {
    let mut line = serde_json::to_string(value).map_err(|_| Error::Json)?;
    line.push('\n');
    Ok(line)
}

/// The `previous` lines of one unit.
fn previous_lines(unit: &UnitOutcome) -> Result<String, Error> {
    let mut out = String::new();
    for record in &unit.previous_records {
        out.push_str(&json_line(&PreviousLine {
            line: "previous".into(),
            unit: unit.unit.clone(),
            memory_id: record.memory_id.clone(),
            record_type: record.mtype.clone(),
            text: record.text.clone(),
        })?);
    }
    Ok(out)
}

/// The review file of a run: its header line, then one line per proposal in the run's order, each unit's
/// earlier records after its last proposal. Units with earlier records and no proposal come last. Session turns
/// are not listed: the plan chose them, and there is no text of the daemon's to review.
pub fn render(run: &ImportRun, proposals: &[ImportProposal]) -> Result<String, Error> {
    let proposals: Vec<ImportProposal> = proposals
        .iter()
        .filter(|p| !is_session_turn(p))
        .cloned()
        .collect();
    let header = Header {
        line: "review".into(),
        version: VERSION,
        run_id: run.run_id.clone(),
        project: run.project.clone(),
        host: run.host.clone(),
        status: run.status.clone(),
        created_at: run.created_at.clone(),
    };
    let mut out = json_line(&header)?;
    let changed: Vec<&UnitOutcome> = run
        .units
        .iter()
        .filter(|u| !u.previous_records.is_empty())
        .collect();
    let mut listed: HashSet<&str> = HashSet::new();
    for (i, proposal) in proposals.iter().enumerate() {
        out.push_str(&json_line(&ProposalLine::new(proposal))?);
        let last_of_unit = proposals
            .get(i + 1)
            .is_none_or(|next| next.unit != proposal.unit);
        if !last_of_unit {
            continue;
        }
        for unit in changed.iter().filter(|u| u.unit == proposal.unit) {
            if listed.insert(&unit.unit) {
                out.push_str(&previous_lines(unit)?);
            }
        }
    }
    for unit in &changed {
        if listed.insert(&unit.unit) {
            out.push_str(&previous_lines(unit)?);
        }
    }
    Ok(out)
}

/// Write the review file owner-only, replacing an earlier one.
pub fn write(path: &Path, run: &ImportRun, proposals: &[ImportProposal]) -> Result<(), Error> {
    write_private(path, &render(run, proposals)?)
}

pub fn read(path: &Path, run_id: &str) -> Result<Review, Error> {
    let text =
        std::fs::read_to_string(path).map_err(|e| invalid(format!("{}: {e}", shown(path))))?;
    parse(&text, run_id)
}

/// `value` for a message: `nothing` when absent, else as JSON (a string in quotes).
fn given(value: Option<&Value>) -> String {
    value.map_or("nothing".to_string(), Value::to_string)
}

/// The decisions and instruction scopes of a review file for run `run_id`, for the proposals it lists
/// as still `proposed`. Every proposal line needs an `accept` or `reject` decision, and every instruction
/// line a `project` or `global` scope; errors name the line. `previous` lines are skipped.
pub fn parse(text: &str, run_id: &str) -> Result<Review, Error> {
    let mut lines = text
        .lines()
        .enumerate()
        .map(|(i, line)| (i + 1, line))
        .filter(|(_, line)| !line.trim().is_empty());
    let Some((n, first)) = lines.next() else {
        return Err(invalid("the review file is empty"));
    };
    let header: Value =
        serde_json::from_str(first).map_err(|e| invalid(format!("line {n}: {e}")))?;
    if header.get("type").and_then(Value::as_str) != Some("review") {
        return Err(invalid(format!(
            "line {n}: a review file starts with its review line; write one with `agent-memoryd import show RUN`"
        )));
    }
    if header.get("version").and_then(Value::as_u64) != Some(VERSION) {
        return Err(invalid(format!(
            "line {n}: review version {} is not supported (expected {VERSION}); write a new one with `agent-memoryd import show`",
            header.get("version").unwrap_or(&Value::Null)
        )));
    }
    let file_run = header.get("runId").and_then(Value::as_str).unwrap_or("");
    if file_run != run_id {
        return Err(invalid(format!(
            "the review file is for run {file_run:?}, not {run_id:?}"
        )));
    }
    let mut review = Review::default();
    let mut listed = HashSet::new();
    for (n, line) in lines {
        let value: Value =
            serde_json::from_str(line).map_err(|e| invalid(format!("line {n}: {e}")))?;
        let field = |key: &str| value.get(key).and_then(Value::as_str);
        match field("type") {
            Some("proposal") => {}
            Some("previous") => continue,
            _ => {
                return Err(invalid(format!(
                    "line {n}: expected a proposal or previous line"
                )))
            }
        }
        let Some(id) = field("proposalId").filter(|id| !id.is_empty()) else {
            return Err(invalid(format!("line {n}: the proposal has no proposalId")));
        };
        let decision = match field("decision") {
            Some(d @ (ACCEPT | REJECT)) => d,
            other => {
                return Err(invalid(format!(
                    "line {n}: decision must be \"accept\" or \"reject\", not {}",
                    other.map_or("nothing".to_string(), |d| format!("{d:?}"))
                )))
            }
        };
        // Only an instruction's scope is read: other kinds are written to the project.
        let global = match field("itemKind") {
            Some(INSTRUCTION) => match field("scope") {
                Some(PROJECT) => false,
                Some(GLOBAL) => true,
                _ => {
                    return Err(invalid(format!(
                        "line {n}: scope must be \"project\" or \"global\", not {}",
                        given(value.get("scope"))
                    )))
                }
            },
            _ => false,
        };
        if !listed.insert(id.to_string()) {
            return Err(invalid(format!("line {n}: proposal {id} is listed twice")));
        }
        if field("status") == Some(PROPOSED) {
            review
                .decisions
                .insert(id.to_string(), decision.to_string());
            if global && decision == ACCEPT {
                review.scopes.insert(id.to_string(), GLOBAL.to_string());
            }
        }
    }
    Ok(review)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use memory_domain::import::{default_decision, RecordRef, APPLIED, KIND, RUN_PROPOSED};
    use serde_json::json;

    pub(crate) fn run(id: &str) -> ImportRun {
        ImportRun {
            run_id: id.into(),
            kind: KIND.into(),
            project: "my-project".into(),
            host: "codex".into(),
            status: RUN_PROPOSED.into(),
            created_at: "2026-10-22T12:00:00Z".into(),
            updated_at: "2026-10-22T12:00:01Z".into(),
            ..ImportRun::default()
        }
    }

    pub(crate) fn proposal(id: &str, kind: &str, status: &str) -> ImportProposal {
        ImportProposal {
            proposal_id: id.into(),
            run_id: "ir1".into(),
            kind: KIND.into(),
            project: "my-project".into(),
            host: "codex".into(),
            unit: "codex:t-1#task-1".into(),
            mode: "extract".into(),
            item_kind: kind.into(),
            scope_hint: "project".into(),
            text: format!("Proposal {id} text."),
            evidence: "quoted evidence".into(),
            stated_at: "2026-01-02T03:04:05Z".into(),
            heading_path: vec!["Title".into(), "Task 1".into()],
            unit_hash: "u".into(),
            file_hash: "f".into(),
            status: status.into(),
            default_decision: default_decision(kind).into(),
            created_at: "2026-10-22T12:00:00Z".into(),
            ..ImportProposal::default()
        }
    }

    /// The row of session turn `native` of codex session `s1`, as run `ir1` holds it.
    pub(crate) fn turn_row(id: &str, native: &str, status: &str) -> ImportProposal {
        let message = |role: &str, content: String| memory_domain::types::InMessage {
            role: role.into(),
            content,
            timestamp: "2026-01-02T03:04:05Z".into(),
        };
        ImportProposal {
            mode: memory_domain::import::MODE_SESSION_TURN.into(),
            item_kind: memory_domain::import::ITEM_SESSION_TURN.into(),
            unit: format!("codex:session%3As1#{native}"),
            text: format!("Prompt {native}."),
            evidence: String::new(),
            turn: Some(memory_domain::import::SessionTurn {
                turn_id: format!("my-project/s1/{native}"),
                session: "s1".into(),
                native_id: native.into(),
                messages: vec![
                    message("user", format!("Prompt {native}.")),
                    message("assistant", format!("Reply {native}.")),
                ],
            }),
            ..proposal(id, memory_domain::import::ITEM_SESSION_TURN, status)
        }
    }

    #[test]
    fn session_turns_are_not_reviewed() {
        let proposals = vec![
            proposal("ip1", "fact", PROPOSED),
            turn_row("ip2", "t1", PROPOSED),
        ];
        let text = render(&run("ir1"), &proposals).unwrap();
        assert_eq!(text.lines().count(), 2, "{text}");
        assert!(!text.contains("Prompt t1."));
    }

    #[test]
    fn review_files_list_proposals_with_their_default_decisions() {
        let mut rejected = proposal("ip3", "fact", REJECTED);
        rejected.reject_reason = Some("evidence is not a quote of the unit".into());
        let mut similar = proposal("ip1", "fact", PROPOSED);
        similar.similar = vec![SimilarMemory {
            memory_id: "m1".into(),
            text: "An existing memory.".into(),
            score: 0.5,
        }];
        let proposals = vec![
            similar,
            proposal("ip2", "instruction", PROPOSED),
            rejected,
            proposal("ip4", "fact", APPLIED),
        ];
        let dir = tempfile::tempdir().unwrap();
        let path = default_path(dir.path(), "ir1");
        assert!(path.ends_with(".local/state/agent-memory-service/imports/ir1.review.jsonl"));
        write(&path, &run("ir1"), &proposals).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(
            lines[0],
            json!({"type": "review", "version": 1, "runId": "ir1", "project": "my-project",
                   "host": "codex", "status": "proposed", "createdAt": "2026-10-22T12:00:00Z"})
        );
        assert_eq!(
            lines[1],
            json!({"type": "proposal", "proposalId": "ip1", "decision": "accept", "status": "proposed",
                   "itemKind": "fact", "scope": "project", "scopeHint": "project",
                   "text": "Proposal ip1 text.", "evidence": "quoted evidence",
                   "unit": "codex:t-1#task-1", "headingPath": ["Title", "Task 1"],
                   "statedAt": "2026-01-02T03:04:05Z", "rejectReason": null, "duplicateOf": null,
                   "supersedes": null, "similar": [{"memoryId": "m1", "text": "An existing memory.", "score": 0.5}]})
        );
        let decisions: Vec<&str> = lines[1..]
            .iter()
            .map(|l| l["decision"].as_str().unwrap())
            .collect();
        assert_eq!(decisions, ["accept", "reject", "reject", "accept"]);
        // Read back: only the proposals still proposed have a decision to send; every instruction stays
        // the project's.
        let read_back = read(&path, "ir1").unwrap();
        assert_eq!(
            read_back.decisions,
            BTreeMap::from([
                ("ip1".to_string(), "accept".to_string()),
                ("ip2".to_string(), "reject".to_string())
            ])
        );
        assert!(read_back.scopes.is_empty());
        // A person accepts the instruction candidate.
        let edited = text.replacen("\"decision\":\"reject\"", "\"decision\":\"accept\"", 1);
        assert_eq!(parse(&edited, "ir1").unwrap().decisions["ip2"], "accept");
    }

    /// The proposal line of `id` in a rendered review, edited by `edit`.
    fn edit_line(text: &str, id: &str, edit: impl Fn(&mut Value)) -> String {
        text.lines()
            .map(|line| {
                let mut value: Value = serde_json::from_str(line).unwrap();
                if value["proposalId"] == id {
                    edit(&mut value);
                }
                value.to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn instruction_scopes_start_as_the_project_and_global_is_opt_in() {
        let mut instruction = proposal("ip1", "instruction", PROPOSED);
        instruction.scope_hint = GLOBAL.into();
        let mut fact = proposal("ip2", "fact", PROPOSED);
        fact.scope_hint = GLOBAL.into();
        let mut applied = proposal("ip3", "instruction", APPLIED);
        applied.scope_hint = GLOBAL.into();
        let text = render(
            &run("ir1"),
            &[
                instruction,
                fact,
                applied,
                proposal("ip4", "instruction", PROPOSED),
            ],
        )
        .unwrap();
        let lines: Vec<Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        // An instruction starts as the project's even when the extraction suggests global; other kinds
        // show their hint.
        let scopes: Vec<(&str, &str)> = lines[1..]
            .iter()
            .map(|l| {
                (
                    l["scope"].as_str().unwrap(),
                    l["scopeHint"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            scopes,
            [
                ("project", "global"),
                ("global", "global"),
                ("project", "global"),
                ("project", "project")
            ]
        );
        assert!(parse(&text, "ir1").unwrap().scopes.is_empty());

        // Global is sent for an accepted instruction still proposed, and only then.
        let accept_global = |v: &mut Value| {
            v["decision"] = json!("accept");
            v["scope"] = json!("global");
        };
        let edited = edit_line(&text, "ip1", accept_global);
        let edited = edit_line(&edited, "ip3", accept_global);
        let edited = edit_line(&edited, "ip4", |v| v["scope"] = json!("global"));
        // A fact's scope is not read, whatever it says.
        let edited = edit_line(&edited, "ip2", |v| v["scope"] = json!("team"));
        let review = parse(&edited, "ir1").unwrap();
        assert_eq!(
            review.scopes,
            BTreeMap::from([("ip1".to_string(), "global".to_string())])
        );
        assert_eq!(review.decisions["ip1"], "accept");
        assert_eq!(review.decisions["ip4"], "reject");

        // Any other scope on an instruction line is an error naming the line.
        let team = edit_line(&text, "ip4", |v| v["scope"] = json!("team"));
        let error = parse(&team, "ir1").unwrap_err().to_string();
        assert!(
            error.contains("line 5") && error.contains("\"team\""),
            "{error}"
        );
        let missing = edit_line(&text, "ip1", |v| {
            v.as_object_mut().unwrap().remove("scope");
        });
        let error = parse(&missing, "ir1").unwrap_err().to_string();
        assert!(
            error.contains("line 2") && error.contains("not nothing"),
            "{error}"
        );
        let number = edit_line(&text, "ip1", |v| v["scope"] = json!(3));
        assert!(parse(&number, "ir1")
            .unwrap_err()
            .to_string()
            .contains("not 3"));
    }

    pub(crate) fn changed_unit(unit: &str, index: usize, records: &[(&str, &str)]) -> UnitOutcome {
        UnitOutcome {
            unit: unit.into(),
            index,
            mode: "extract".into(),
            outcome: "changed".into(),
            previous_memory_ids: records.iter().map(|(id, _)| id.to_string()).collect(),
            previous_records: records
                .iter()
                .map(|(id, text)| RecordRef {
                    memory_id: id.to_string(),
                    mtype: "fact".into(),
                    text: text.to_string(),
                })
                .collect(),
            ..UnitOutcome::default()
        }
    }

    #[test]
    fn earlier_records_of_changed_units_follow_their_proposals_and_are_not_read() {
        let mut run = run("ir1");
        let in_unit = |id: &str, unit: &str| {
            let mut p = proposal(id, "fact", PROPOSED);
            p.unit = unit.into();
            p
        };
        let proposals = [
            in_unit("ip1", "u:a"),
            in_unit("ip2", "u:a"),
            in_unit("ip3", "u:b"),
        ];
        run.units = vec![
            changed_unit(
                "u:a",
                0,
                &[("m1", "Old fact one."), ("m2", "Old fact two.")],
            ),
            changed_unit("u:b", 1, &[]),
            // Nothing extracted from the new text: its earlier record still shows.
            changed_unit("u:c", 2, &[("m3", "Old fact three.")]),
        ];
        let text = render(&run, &proposals).unwrap();
        let lines: Vec<Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let shape: Vec<String> = lines[1..]
            .iter()
            .map(|l| match l["type"].as_str().unwrap() {
                "proposal" => l["proposalId"].as_str().unwrap().to_string(),
                _ => format!(
                    "{} {}",
                    l["unit"].as_str().unwrap(),
                    l["memoryId"].as_str().unwrap()
                ),
            })
            .collect();
        assert_eq!(shape, ["ip1", "ip2", "u:a m1", "u:a m2", "ip3", "u:c m3"]);
        assert_eq!(
            lines[3],
            json!({"type": "previous", "unit": "u:a", "memoryId": "m1", "recordType": "fact",
                   "text": "Old fact one."})
        );
        // Read back, the previous lines decide nothing, wherever they are.
        let review = parse(&text, "ir1").unwrap();
        assert_eq!(review.decisions.len(), 3);
        let moved: Vec<&str> = text.lines().collect();
        let moved = [moved[0], moved[6], moved[1], moved[3]].join("\n");
        assert_eq!(parse(&moved, "ir1").unwrap().decisions.len(), 1);
        let other = text.replacen("\"type\":\"previous\"", "\"type\":\"note\"", 1);
        assert!(parse(&other, "ir1")
            .unwrap_err()
            .to_string()
            .contains("line 4"));
    }

    #[test]
    fn review_decisions_must_be_accept_or_reject_for_this_run() {
        let text = render(
            &run("ir1"),
            &[
                proposal("ip1", "fact", PROPOSED),
                proposal("ip2", "fact", PROPOSED),
            ],
        )
        .unwrap();
        let error = parse(&text, "ir2").unwrap_err().to_string();
        assert!(
            error.contains("\"ir1\"") && error.contains("\"ir2\""),
            "{error}"
        );
        let maybe = text.replacen("\"decision\":\"accept\"", "\"decision\":\"maybe\"", 2);
        let maybe = maybe.replacen("\"decision\":\"maybe\"", "\"decision\":\"accept\"", 1);
        let error = parse(&maybe, "ir1").unwrap_err().to_string();
        assert!(
            error.contains("line 3") && error.contains("\"maybe\""),
            "{error}"
        );
        let missing = text.replacen("\"decision\":\"accept\",", "", 1);
        assert!(parse(&missing, "ir1")
            .unwrap_err()
            .to_string()
            .contains("line 2"));
        let lines: Vec<&str> = text.lines().collect();
        let twice = [lines[0], lines[1], lines[1]].join("\n");
        assert!(parse(&twice, "ir1")
            .unwrap_err()
            .to_string()
            .contains("listed twice"));
        assert!(parse(lines[1], "ir1")
            .unwrap_err()
            .to_string()
            .contains("review line"));
        let old = text.replacen("\"version\":1", "\"version\":2", 1);
        assert!(parse(&old, "ir1")
            .unwrap_err()
            .to_string()
            .contains("version 2"));
        assert!(parse("", "ir1").is_err());
    }
}
