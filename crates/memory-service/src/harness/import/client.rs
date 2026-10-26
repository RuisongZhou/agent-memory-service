//! The daemon side of an import (docs/design/memory-import.md §6.7, §6.13): a plan's units go to
//! `/admin/import_propose` in batches that make one run, whose proposals are then read, applied, rolled
//! back or listed through the other `/admin/import_*` endpoints of the loopback daemon. Every call is one
//! `Daemon::admin` request; nothing here reads host files except to check that the sources of a plan, or
//! of a run's proposals, are still as they were.
use memory_domain::import::{
    is_session_turn, ImportProposal, ImportProposeReq, ImportRun, ImportRunReq, ImportRunsReq,
    ImportUnit, MODE_SESSION_TURN, PROPOSED, REJECT,
};
use memory_rules::import::units::content_hash;
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use super::plan::Plan;
use super::review::{self, Review};
use super::source::{expand_in, read_text};
use crate::harness::{invalid, AdminError, CallError, Daemon, Error};

/// Platform label of the calls that act for no particular host.
pub const PLATFORM: &str = "agent-memoryd";
/// Largest source file read again to check a plan.
const MAX_SOURCE: u64 = 8 << 20;
/// Characters of the daemon's refusal message shown on the `[error]` line.
const MAX_MESSAGE: usize = 300;
/// JSON bytes of units per `import_propose` request, well under the daemon's 2 MB body limit.
const MAX_BATCH_BYTES: usize = 1 << 20;

/// A run and its proposals, in the run's order.
#[derive(Debug, Clone, PartialEq)]
pub struct RunView {
    pub run: ImportRun,
    pub proposals: Vec<ImportProposal>,
}

/// A unit of the plan whose source file changed or vanished after the plan was made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stale {
    pub unit: String,
    /// Display form.
    pub path: String,
    /// `changed` or `vanished`.
    pub why: &'static str,
}

/// A source file of a run's proposals that changed or vanished after they were proposed; apply declines
/// its proposals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedSource {
    /// Display form, as `source.path` has it.
    pub path: String,
    /// `changed` or `vanished`.
    pub why: &'static str,
    /// The still-proposed proposals of the file.
    pub proposals: Vec<String>,
}

/// What `apply` did: the run afterwards, and the sources whose proposals it declined.
#[derive(Debug, Clone, PartialEq)]
pub struct Applied {
    pub view: RunView,
    pub changed: Vec<ChangedSource>,
}

/// The hash of a source file at display path `path` (`~/` under `home`), read again as the plan read it:
/// `Err("vanished")` when it is gone, `Err("changed")` when it can no longer be read so (a link, too
/// large, not text).
fn source_hash(home: &Path, path: &str) -> Result<String, &'static str> {
    let file = expand_in(home, path);
    match read_text(&file, MAX_SOURCE) {
        Ok((raw, _)) => Ok(content_hash(&raw)),
        Err(_) if path.is_empty() || std::fs::symlink_metadata(&file).is_err() => Err("vanished"),
        Err(_) => Err("changed"),
    }
}

/// The plan's units to propose, those whose source file still hashes as when the plan was made, and the
/// stale ones (`stale-plan`). Each file is read once. A session turn is always fresh: its messages travel in
/// the plan, and a transcript keeps growing while its session runs.
pub fn fresh_units(plan: &Plan, home: &Path) -> (Vec<ImportUnit>, Vec<Stale>) {
    let mut hashes: HashMap<String, Result<String, &'static str>> = HashMap::new();
    let (mut fresh, mut stale) = (Vec::new(), Vec::new());
    for unit in plan.import_units() {
        if unit.mode == MODE_SESSION_TURN {
            fresh.push(unit.clone());
            continue;
        }
        let path = unit
            .source
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let hash = hashes
            .entry(path.clone())
            .or_insert_with(|| source_hash(home, &path))
            .clone();
        match hash {
            Ok(hash) if hash == unit.file_hash => fresh.push(unit.clone()),
            Ok(_) => stale.push(Stale {
                unit: unit.unit.clone(),
                path,
                why: "changed",
            }),
            Err(why) => stale.push(Stale {
                unit: unit.unit.clone(),
                path,
                why,
            }),
        }
    }
    (fresh, stale)
}

/// The source files, read again under `home` as `fresh_units` reads them, whose hash is no longer the
/// `fileHash` of their still-proposed proposals, in the proposals' order. Each file is read once; a
/// proposal without `source.path`, and a session turn, is not checked.
pub fn changed_sources(proposals: &[ImportProposal], home: &Path) -> Vec<ChangedSource> {
    let mut hashes: HashMap<&str, Result<String, &'static str>> = HashMap::new();
    let mut changed: Vec<ChangedSource> = Vec::new();
    for proposal in proposals
        .iter()
        .filter(|p| p.status == PROPOSED && !is_session_turn(p))
    {
        let Some(path) = proposal
            .source
            .get("path")
            .and_then(Value::as_str)
            .filter(|path| !path.is_empty())
        else {
            continue;
        };
        let why = match hashes
            .entry(path)
            .or_insert_with(|| source_hash(home, path))
        {
            Ok(hash) if *hash == proposal.file_hash => continue,
            Ok(_) => "changed",
            Err(why) => *why,
        };
        let id = proposal.proposal_id.clone();
        match changed.iter_mut().find(|c| c.path == path) {
            Some(source) => source.proposals.push(id),
            None => changed.push(ChangedSource {
                path: path.to_string(),
                why,
                proposals: vec![id],
            }),
        }
    }
    changed
}

/// Calls to one daemon.
pub struct Client {
    daemon: Daemon,
    url: String,
}

impl Client {
    pub fn new(url: &str, timeout: Duration) -> Result<Self, Error> {
        let daemon =
            Daemon::new(url, timeout).map_err(|e| invalid(format!("--url {url}: {}", e.kind())))?;
        Ok(Client {
            daemon,
            url: url.to_string(),
        })
    }

    async fn call(
        &self,
        name: &str,
        body: &impl Serialize,
        platform: &str,
    ) -> Result<Value, Error> {
        let body = serde_json::to_value(body).map_err(|_| Error::Json)?;
        self.daemon
            .admin(name, &body, platform)
            .await
            .map_err(|e| self.failed(e))
    }

    /// One line for a failed call: the daemon's status with its own reason (the API's validation
    /// message, flattened and shortened), or why it could not answer.
    fn failed(&self, AdminError { error, message }: AdminError) -> Error {
        invalid(match error {
            CallError::Status(code) => match message.as_deref().map(one_line) {
                Some(message) if !message.is_empty() => format!("HTTP {code}: {message}"),
                _ => format!("HTTP {code}: {}", reason(code)),
            },
            CallError::Timeout => format!("the daemon at {} did not answer in time", self.url),
            CallError::Connection => {
                format!("cannot reach the daemon at {}; is it running?", self.url)
            }
            CallError::Invalid => format!(
                "the daemon at {} did not answer with a JSON object of at most 32 MiB",
                self.url
            ),
        })
    }

    /// Send `units` (the plan's fresh import units) as one run, `batch` units per request: the first
    /// request creates the run, every later one names it, the last one finishes it. With `reconsider`,
    /// every batch asks the daemon to propose again the units whose last import applied nothing although
    /// their text is unchanged. `progress` hears of each batch (done, of, run id). Returns the run as the
    /// daemon has it afterwards.
    pub async fn propose(
        &self,
        plan: &Plan,
        units: &[ImportUnit],
        batch: usize,
        reconsider: bool,
        progress: &mut (dyn FnMut(usize, usize, &str) + Send),
    ) -> Result<RunView, Error> {
        let summary = &plan.summary;
        let batches = batches(units, batch, MAX_BATCH_BYTES);
        let mut run_id: Option<String> = None;
        for (index, chunk) in batches.iter().enumerate() {
            let request = ImportProposeReq {
                project: summary.project.clone(),
                host: summary.host.clone(),
                run_id: run_id.clone(),
                finish: index + 1 == batches.len(),
                limits: summary.limits.wire,
                plan_hash: Some(summary.plan_hash.clone()),
                reconsider,
                units: chunk.to_vec(),
            };
            let answer = match (
                self.call("import_propose", &request, &summary.host).await,
                &run_id,
            ) {
                (Ok(answer), _) => answer,
                (Err(e), None) => return Err(e),
                // The run so far stays `proposing`: name it, so it can be inspected.
                (Err(e), Some(id)) => {
                    return Err(invalid(format!(
                        "batch {}/{} of run {id}: {}",
                        index + 1,
                        batches.len(),
                        super::message(&e)
                    )))
                }
            };
            let answered = run_id_of(&answer)
                .ok_or_else(|| invalid("import_propose answered without a runId"))?;
            let id = run_id.get_or_insert_with(|| answered.clone());
            if *id != answered {
                return Err(invalid(format!(
                    "import_propose answered run {answered} for a batch of run {id}"
                )));
            }
            progress(index + 1, batches.len(), id);
        }
        self.run(&run_id.expect("propose sends at least one batch"))
            .await
    }

    /// A run and its proposals (`import_run`).
    pub async fn run(&self, run_id: &str) -> Result<RunView, Error> {
        let request = ImportRunReq {
            run_id: run_id.into(),
            decisions: BTreeMap::new(),
        };
        run_view(self.call("import_run", &request, PLATFORM).await?)
    }

    /// Apply a run with the decisions of a review file, or without one with every proposal accepted. First the source file of every proposal still proposed is read again
    /// under `home`: the proposals of a file that changed or vanished since propose are declined, whatever
    /// the review says. Calls as the run's host.
    pub async fn apply(
        &self,
        run_id: &str,
        review: Option<&Path>,
        home: &Path,
    ) -> Result<Applied, Error> {
        let current = self.run(run_id).await?;
        let Review { mut decisions } = match review {
            None => Review::default(),
            Some(path) => complete(review::read(path, run_id)?, &current.proposals)?,
        };
        let changed = changed_sources(&current.proposals, home);
        for id in changed.iter().flat_map(|source| &source.proposals) {
            decisions.insert(id.clone(), REJECT.into());
        }
        let request = ImportRunReq {
            run_id: run_id.into(),
            decisions,
        };
        let answer = self
            .call("import_apply", &request, &current.run.host)
            .await?;
        Ok(Applied {
            view: run_view(answer)?,
            changed,
        })
    }

    /// Roll an applied run back (`import_rollback`), as the run's host.
    pub async fn rollback(&self, run_id: &str) -> Result<RunView, Error> {
        let current = self.run(run_id).await?;
        let request = ImportRunReq {
            run_id: run_id.into(),
            decisions: BTreeMap::new(),
        };
        let answer = self
            .call("import_rollback", &request, &current.run.host)
            .await?;
        run_view(answer)
    }

    /// The newest runs first (`import_runs`), of one project when given.
    pub async fn runs(&self, project: Option<&str>, limit: usize) -> Result<Vec<ImportRun>, Error> {
        let request = ImportRunsReq {
            project: project.map(String::from),
            limit: Some(limit),
        };
        let answer = self.call("import_runs", &request, PLATFORM).await?;
        serde_json::from_value(answer["runs"].clone())
            .map_err(|e| invalid(format!("import_runs answered runs of another shape: {e}")))
    }
}

/// `units` in order, at most `batch` units and about `max_bytes` of JSON per request; a unit larger than
/// that goes alone.
fn batches(units: &[ImportUnit], batch: usize, max_bytes: usize) -> Vec<&[ImportUnit]> {
    let mut out = Vec::new();
    let (mut start, mut size) = (0, 0);
    for (i, unit) in units.iter().enumerate() {
        let bytes = serde_json::to_vec(unit).map_or(0, |json| json.len());
        if i > start && (i - start == batch || size + bytes > max_bytes) {
            out.push(&units[start..i]);
            (start, size) = (i, 0);
        }
        size += bytes;
    }
    if start < units.len() {
        out.push(&units[start..]);
    }
    out
}

/// `text` on one line, whitespace runs collapsed, cut to `MAX_MESSAGE` characters.
fn one_line(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.char_indices().nth(MAX_MESSAGE) {
        Some((end, _)) => format!("{}…", &flat[..end]),
        None => flat,
    }
}

/// The canonical reason phrase of an HTTP status.
fn reason(code: u16) -> &'static str {
    reqwest::StatusCode::from_u16(code)
        .ok()
        .and_then(|status| status.canonical_reason())
        .unwrap_or("error")
}

fn run_id_of(answer: &Value) -> Option<String> {
    answer["run"]["runId"]
        .as_str()
        .filter(|id| !id.is_empty())
        .map(String::from)
}

/// A run as the daemon answers it: `{"run": {...}, "proposals": [...]}`.
pub fn run_view(answer: Value) -> Result<RunView, Error> {
    let run: ImportRun = serde_json::from_value(answer["run"].clone())
        .map_err(|e| invalid(format!("the daemon answered a run of another shape: {e}")))?;
    let proposals = serde_json::from_value(answer["proposals"].clone()).map_err(|e| {
        invalid(format!(
            "the daemon answered proposals of another shape: {e}"
        ))
    })?;
    Ok(RunView { run, proposals })
}

/// The decisions apply sends for a review: every proposal still proposed gets the review's decision, or
/// `reject` when the review no longer lists it (deleting a line declines a proposal, as it skips a unit in a
/// plan). A review naming a proposal the run does not have belongs to something else. Session turns are not
/// reviewed (the plan chose them): they are accepted.
pub fn complete(listed: Review, proposals: &[ImportProposal]) -> Result<Review, Error> {
    let known: HashSet<&str> = proposals.iter().map(|p| p.proposal_id.as_str()).collect();
    if let Some(unknown) = listed
        .decisions
        .keys()
        .find(|id| !known.contains(id.as_str()))
    {
        return Err(invalid(format!(
            "the review lists proposal {unknown}, which the run does not have"
        )));
    }
    let mut review = Review::default();
    for proposal in proposals
        .iter()
        .filter(|p| p.status == PROPOSED && !is_session_turn(p))
    {
        let id = &proposal.proposal_id;
        let decision = listed.decisions.get(id).map_or(REJECT, String::as_str);
        review.decisions.insert(id.clone(), decision.to_string());
    }
    Ok(review)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::harness::import::plan::{build, PlanOptions};
    use crate::harness::import::review::tests::{proposal, run};
    use crate::harness::import::source::{display, Issue, Policy, ScanOutput, Segment, SourceDoc};
    use axum::extract::{Path as UrlPath, State};
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::{IntoResponse, Response};
    use axum::routing::post;
    use axum::{Json, Router};
    use memory_domain::import::{ImportLimits, ACCEPT, REJECTED, RUN_APPLIED};
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    /// What the stub daemon was asked: (endpoint, X-Memory-Platform, body).
    pub(crate) type Requests = Arc<Mutex<Vec<(String, String, Value)>>>;

    /// The run `ir1` the stub answers with, and what it was asked.
    #[derive(Clone)]
    struct StubState {
        requests: Requests,
        run: Value,
        proposals: Vec<ImportProposal>,
    }

    /// The run `ir1` as the stub has it.
    pub(crate) fn stub_run() -> Value {
        let mut run = serde_json::to_value(run("ir1")).unwrap();
        run["counts"]["proposals"] = json!(4);
        run
    }

    pub(crate) fn stub_proposals() -> Vec<ImportProposal> {
        vec![
            proposal("ip1", "fact", PROPOSED),
            proposal("ip2", "preference", PROPOSED),
            proposal("ip3", "fact", REJECTED),
            proposal("ip4", "preference", PROPOSED),
        ]
    }

    async fn answer(
        State(stub): State<StubState>,
        UrlPath(name): UrlPath<String>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> Response {
        let platform = headers
            .get("x-memory-platform")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        stub.requests
            .lock()
            .unwrap()
            .push((name.clone(), platform, body.clone()));
        let known = body["runId"] == "ir1";
        let failing = body["units"]
            .as_array()
            .is_some_and(|units| units.iter().any(|u| u["text"] == "FAIL"));
        match name.as_str() {
            "import_propose" if failing => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            "import_propose" => {
                Json(json!({"run": {"runId": "ir1", "status": "proposing"}})).into_response()
            }
            "import_run" if known => {
                Json(json!({"run": stub.run, "proposals": stub.proposals})).into_response()
            }
            "import_apply" | "import_rollback" if known => {
                let mut run = stub.run.clone();
                run["status"] = json!(RUN_APPLIED);
                run["counts"]["applied"] = json!(2);
                Json(json!({"run": run, "proposals": stub.proposals})).into_response()
            }
            "import_runs" => Json(json!({"runs": [stub.run]})).into_response(),
            _ => (
                StatusCode::NOT_FOUND,
                Json(json!({"code": "not_found", "message": "unknown import run"})),
            )
                .into_response(),
        }
    }

    /// A loopback daemon answering the import endpoints for run `ir1`.
    pub(crate) async fn stub() -> (String, Requests) {
        stub_with(stub_run(), stub_proposals()).await
    }

    /// A loopback daemon answering the import endpoints for run `ir1`, as `run` with `proposals`.
    pub(crate) async fn stub_with(
        run: Value,
        proposals: Vec<ImportProposal>,
    ) -> (String, Requests) {
        let requests: Requests = Arc::default();
        let state = StubState {
            requests: requests.clone(),
            run,
            proposals,
        };
        let app = Router::new()
            .route("/admin/{name}", post(answer))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (url, requests)
    }

    /// Proposals of the run over files `a.md`, `b.md` and `c.md` in `home`, each with the file's hash as
    /// written here: ip1 of a.md, ip2 (a preference) and ip3 of b.md, ip4 of c.md, ip5 without a source
    /// path, and ip6 of b.md, rejected when proposed.
    pub(crate) fn sourced_proposals(home: &Path) -> Vec<ImportProposal> {
        let sourced = |id: &str, kind: &str, status: &str, name: Option<&str>| {
            let mut p = proposal(id, kind, status);
            if let Some(name) = name {
                let path = home.join(name);
                let raw = format!("Notes of {name}.\n");
                std::fs::write(&path, &raw).unwrap();
                p.source = json!({"path": display(home, &path), "via": "codex-rollout-summary"});
                p.file_hash = content_hash(&raw);
            }
            p
        };
        vec![
            sourced("ip1", "fact", PROPOSED, Some("a.md")),
            sourced("ip2", "preference", PROPOSED, Some("b.md")),
            sourced("ip3", "fact", PROPOSED, Some("b.md")),
            sourced("ip4", "fact", PROPOSED, Some("c.md")),
            sourced("ip5", "fact", PROPOSED, None),
            sourced("ip6", "fact", REJECTED, Some("b.md")),
        ]
    }

    /// A plan over notes `a.md`, `b.md` and `c.md` in `home`: one, two and three units, and a skipped one.
    pub(crate) fn plan_over(home: &Path) -> Plan {
        let mut docs = Vec::new();
        for (name, count) in [("a.md", 1), ("b.md", 2), ("c.md", 3)] {
            let path = home.join(name);
            let raw = format!("Notes of {name}.\n");
            std::fs::write(&path, &raw).unwrap();
            let segments = (0..count)
                .map(|i| {
                    let text = format!("Fact {i} of {name}.");
                    Segment::new(format!("s{i}"), vec![], text, Policy::VerbatimIfFits)
                })
                .chain((name == "a.md").then(|| {
                    Segment::new("blank".into(), vec![], " ".into(), Policy::VerbatimIfFits)
                }))
                .collect();
            docs.push(SourceDoc {
                host: "codex".into(),
                via: "codex-rollout-summary",
                display_path: display(home, &path),
                path,
                native_id: name.into(),
                title: None,
                cwd: None,
                user_level: false,
                file_hash: content_hash(&raw),
                stated_at: "2026-01-01T00:00:00Z".into(),
                kind_hint: None,
                segments,
                extra: Value::Null,
            });
        }
        let scan = ScanOutput {
            docs,
            issues: vec![Issue {
                path: "~/x".into(),
                reason: "derived".into(),
            }],
            sessions: vec![],
        };
        let opts = PlanOptions {
            host: "codex".into(),
            project: "my-project".into(),
            home: home.to_path_buf(),
            workspaces: vec![],
            limits: ImportLimits::default(),
            no_extract: false,
            created_at: "2026-10-22T12:00:00Z".into(),
            sessions: None,
        };
        build(scan, &opts).unwrap()
    }

    #[tokio::test]
    async fn propose_threads_one_run_through_its_batches_and_skips_stale_units() {
        let home = tempfile::tempdir().unwrap();
        let plan = plan_over(home.path());
        assert_eq!(plan.import_units().count(), 6);
        // After the plan, b.md changes and c.md is deleted.
        std::fs::write(home.path().join("b.md"), "Edited.\n").unwrap();
        std::fs::remove_file(home.path().join("c.md")).unwrap();
        let (units, stale) = fresh_units(&plan, home.path());
        assert_eq!(units.len(), 1);
        let why: Vec<(&str, &str)> = stale.iter().map(|s| (s.path.as_str(), s.why)).collect();
        assert_eq!(
            why,
            [
                ("~/b.md", "changed"),
                ("~/b.md", "changed"),
                ("~/c.md", "vanished"),
                ("~/c.md", "vanished"),
                ("~/c.md", "vanished")
            ]
        );
        assert_eq!(stale[0].unit, "codex:b.md#s0");

        // With every file as planned: six units in batches of four and two.
        let home = tempfile::tempdir().unwrap();
        let plan = plan_over(home.path());
        let (units, stale) = fresh_units(&plan, home.path());
        assert!(stale.is_empty());
        let (url, requests) = stub().await;
        let client = Client::new(&url, Duration::from_secs(5)).unwrap();
        let mut seen = Vec::new();
        let view = client
            .propose(&plan, &units, 4, false, &mut |done, of, run| {
                seen.push((done, of, run.to_string()))
            })
            .await
            .unwrap();
        assert_eq!(view.run.run_id, "ir1");
        assert_eq!(view.proposals.len(), 4);
        assert_eq!(seen, [(1, 2, "ir1".to_string()), (2, 2, "ir1".to_string())]);
        let requests = requests.lock().unwrap();
        let names: Vec<&str> = requests.iter().map(|r| r.0.as_str()).collect();
        assert_eq!(names, ["import_propose", "import_propose", "import_run"]);
        let (first, second) = (&requests[0].2, &requests[1].2);
        assert_eq!(
            (requests[0].1.as_str(), requests[1].1.as_str()),
            ("codex", "codex")
        );
        assert_eq!(requests[2].1, PLATFORM);
        assert_eq!(first["runId"], Value::Null);
        assert_eq!(second["runId"], "ir1");
        assert_eq!(
            (first["finish"].clone(), second["finish"].clone()),
            (json!(false), json!(true))
        );
        assert_eq!(first["units"].as_array().unwrap().len(), 4);
        assert_eq!(second["units"].as_array().unwrap().len(), 2);
        assert_eq!(first["planHash"], plan.summary.plan_hash.as_str());
        assert_eq!(
            (first["reconsider"].clone(), second["reconsider"].clone()),
            (json!(false), json!(false))
        );
        assert_eq!(first["project"], "my-project");
        assert_eq!(first["host"], "codex");
        assert_eq!(
            first["limits"],
            json!({"recallBudget": 400, "recallTopK": 5})
        );
        // Units go out as planned and in plan order; the blank one was skipped in the plan.
        let sent: Vec<&str> = [first, second]
            .iter()
            .flat_map(|b| b["units"].as_array().unwrap())
            .map(|u| u["unit"].as_str().unwrap())
            .collect();
        assert_eq!(
            sent,
            [
                "codex:a.md#s0",
                "codex:b.md#s0",
                "codex:b.md#s1",
                "codex:c.md#s0",
                "codex:c.md#s1",
                "codex:c.md#s2"
            ]
        );
        let unit: ImportUnit = serde_json::from_value(first["units"][0].clone()).unwrap();
        assert_eq!(unit, units[0]);
    }

    #[tokio::test]
    async fn a_batch_size_dividing_the_units_sends_no_empty_batch() {
        let home = tempfile::tempdir().unwrap();
        let plan = plan_over(home.path());
        let (units, _) = fresh_units(&plan, home.path());
        let (url, _) = stub().await;
        let client = Client::new(&url, Duration::from_secs(5)).unwrap();
        // Six units in threes are two batches, no empty third; a later batch that fails names the run
        // it leaves proposing.
        let mut failing = units.clone();
        failing[4].text = "FAIL".into();
        let error = client_error(
            client
                .propose(&plan, &failing, 3, false, &mut |_, _, _| {})
                .await,
        );
        assert_eq!(
            error,
            "batch 2/2 of run ir1: HTTP 500: Internal Server Error"
        );
        failing[0].text = "FAIL".into();
        let error = client_error(
            client
                .propose(&plan, &failing, 3, false, &mut |_, _, _| {})
                .await,
        );
        assert_eq!(error, "HTTP 500: Internal Server Error");
    }

    fn client_error<T: std::fmt::Debug>(result: Result<T, Error>) -> String {
        match result.unwrap_err() {
            Error::Invalid(message) => message,
            other => other.to_string(),
        }
    }

    #[tokio::test]
    async fn apply_sends_the_review_decisions_and_declines_unlisted_proposals() {
        let (url, requests) = stub().await;
        let client = Client::new(&url, Duration::from_secs(5)).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let path = dir.path().join("ir1.review.jsonl");
        // The review keeps its decisions and no longer lists ip4.
        let listed: Vec<ImportProposal> = stub_proposals().into_iter().take(3).collect();
        std::fs::write(&path, review::render(&run("ir1"), &listed).unwrap()).unwrap();
        let applied = client.apply("ir1", Some(&path), home).await.unwrap();
        // The stub's proposals name no source file: nothing to recheck.
        assert!(applied.changed.is_empty());
        let view = applied.view;
        assert_eq!(view.run.status, RUN_APPLIED);
        assert_eq!(view.run.counts.applied, 2);
        {
            let requests = requests.lock().unwrap();
            let (name, platform, body) = requests.last().unwrap();
            assert_eq!(
                (name.as_str(), platform.as_str()),
                ("import_apply", "codex")
            );
            assert_eq!(
                body,
                &json!({"runId": "ir1", "decisions": {"ip1": ACCEPT, "ip2": ACCEPT, "ip4": REJECT}})
            );
        }
        // A review of another run, and a review naming proposals the run lacks, are refused.
        let other = dir.path().join("other.review.jsonl");
        std::fs::write(&other, review::render(&run("ir2"), &listed).unwrap()).unwrap();
        assert!(client_error(client.apply("ir1", Some(&other), home).await).contains("\"ir2\""));
        let mut foreign = listed.clone();
        foreign[0].proposal_id = "ip9".into();
        std::fs::write(&other, review::render(&run("ir1"), &foreign).unwrap()).unwrap();
        assert!(client_error(client.apply("ir1", Some(&other), home).await).contains("ip9"));
        let applies = requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.0 == "import_apply")
            .count();
        assert_eq!(applies, 1, "a refused review sends nothing to apply");
    }

    #[tokio::test]
    async fn apply_declines_the_proposals_of_sources_changed_since_propose() {
        let home = tempfile::tempdir().unwrap();
        let proposals = sourced_proposals(home.path());
        let (url, requests) = stub_with(stub_run(), proposals.clone()).await;
        let client = Client::new(&url, Duration::from_secs(5)).unwrap();
        let path = home.path().join("ir1.review.jsonl");
        // Everything accepted.
        let text = review::render(&run("ir1"), &proposals).unwrap();
        std::fs::write(&path, &text).unwrap();
        // Unchanged sources: the review goes as it is.
        let applied = client.apply("ir1", Some(&path), home.path()).await.unwrap();
        assert!(applied.changed.is_empty());
        assert_eq!(
            requests.lock().unwrap().last().unwrap().2["decisions"]["ip2"],
            ACCEPT
        );

        // After propose, b.md changes and c.md is deleted.
        std::fs::write(home.path().join("b.md"), "Edited.\n").unwrap();
        std::fs::remove_file(home.path().join("c.md")).unwrap();
        let applied = client.apply("ir1", Some(&path), home.path()).await.unwrap();
        assert_eq!(
            applied.changed,
            [
                ChangedSource {
                    path: "~/b.md".into(),
                    why: "changed",
                    proposals: vec!["ip2".into(), "ip3".into()],
                },
                ChangedSource {
                    path: "~/c.md".into(),
                    why: "vanished",
                    proposals: vec!["ip4".into()],
                },
            ]
        );
        // Their proposals are declined whatever the review says; one without a source path is left.
        assert_eq!(
            requests.lock().unwrap().last().unwrap().2,
            json!({"runId": "ir1",
                   "decisions": {"ip1": ACCEPT, "ip2": REJECT, "ip3": REJECT, "ip4": REJECT, "ip5": ACCEPT}})
        );
        // Without a review file too.
        client.apply("ir1", None, home.path()).await.unwrap();
        assert_eq!(
            requests.lock().unwrap().last().unwrap().2,
            json!({"runId": "ir1", "decisions": {"ip2": REJECT, "ip3": REJECT, "ip4": REJECT}})
        );
    }

    #[test]
    fn session_turns_skip_the_source_checks_and_keep_their_default() {
        let home = tempfile::tempdir().unwrap();
        let mut plan = plan_over(home.path());
        let mut turn = plan.units[0].clone();
        turn.decision = crate::harness::import::plan::IMPORT.into();
        turn.unit.mode = MODE_SESSION_TURN.into();
        turn.unit.unit = "codex:session%3As1#t1".into();
        turn.unit.source = json!({"path": "~/.codex/sessions/rollout-gone.jsonl"});
        plan.units.push(turn);
        // The transcript is gone (or grew): the turn's messages travel in the plan.
        let (fresh, stale) = fresh_units(&plan, home.path());
        assert!(stale.is_empty());
        assert_eq!(fresh.last().unwrap().unit, "codex:session%3As1#t1");
        let mut row = crate::harness::import::review::tests::turn_row("ip9", "t1", PROPOSED);
        row.source = json!({"path": "~/.codex/sessions/rollout-gone.jsonl"});
        assert!(changed_sources(&[row.clone()], home.path()).is_empty());
        // A review never lists a turn, and does not decline it either.
        let mut proposals = stub_proposals();
        proposals.push(row);
        let review = complete(Review::default(), &proposals).unwrap();
        assert!(!review.decisions.contains_key("ip9"));
        assert_eq!(review.decisions.len(), 3);
    }

    #[tokio::test]
    async fn runs_rollback_and_errors() {
        let (url, requests) = stub().await;
        let client = Client::new(&url, Duration::from_secs(5)).unwrap();
        let runs = client.runs(Some("my-project"), 10).await.unwrap();
        assert_eq!(runs[0].run_id, "ir1");
        assert_eq!(
            requests.lock().unwrap().last().unwrap().2,
            json!({"project": "my-project", "limit": 10})
        );
        let view = client.rollback("ir1").await.unwrap();
        assert_eq!(view.run.run_id, "ir1");
        let (name, platform, _) = requests.lock().unwrap().last().unwrap().clone();
        assert_eq!(
            (name.as_str(), platform.as_str()),
            ("import_rollback", "codex")
        );
        // HTTP errors are one line with the status and the daemon's reason; a daemon that is down is named.
        assert_eq!(
            client_error(client.run("missing").await),
            "HTTP 404: unknown import run"
        );
        assert_eq!(one_line(" run ir1 is\napplied  "), "run ir1 is applied");
        assert_eq!(one_line(&"x".repeat(301)).chars().count(), 301);
        let closed = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            format!("http://{}", listener.local_addr().unwrap())
        };
        let down = Client::new(&closed, Duration::from_secs(2)).unwrap();
        let error = client_error(down.run("ir1").await);
        assert!(
            error.contains("cannot reach the daemon") && error.contains(&closed),
            "{error}"
        );
    }

    #[test]
    fn batches_respect_the_unit_count_and_the_body_size() {
        let unit = |text: &str| ImportUnit {
            unit: format!("codex:t#{}", text.len()),
            text: text.into(),
            ..ImportUnit::default()
        };
        let small: Vec<ImportUnit> = (0..5).map(|i| unit(&"a".repeat(i + 1))).collect();
        let sizes: Vec<usize> = batches(&small, 2, MAX_BATCH_BYTES)
            .iter()
            .map(|b| b.len())
            .collect();
        assert_eq!(sizes, [2, 2, 1]);
        // Three units of about 600 KB each: no two share a 1 MiB request.
        let big: Vec<ImportUnit> = (1..=3).map(|i| unit(&"b".repeat(600_000 + i))).collect();
        let sizes: Vec<usize> = batches(&big, 8, MAX_BATCH_BYTES)
            .iter()
            .map(|b| b.len())
            .collect();
        assert_eq!(sizes, [1, 1, 1]);
        assert!(batches(&[], 8, MAX_BATCH_BYTES).is_empty());
    }

    #[test]
    fn runs_are_read_from_the_daemon_answer() {
        let view = run_view(json!({"run": stub_run(), "proposals": stub_proposals()})).unwrap();
        assert_eq!(view.proposals.len(), 4);
        assert!(run_view(json!({"unexpected": true})).is_err());
        assert_eq!(
            run_id_of(&json!({"run": {"runId": "ir7"}})).as_deref(),
            Some("ir7")
        );
        assert_eq!(run_id_of(&json!({"run": {"runId": ""}})), None);
    }
}
