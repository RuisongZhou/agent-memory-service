//! Imports a person drives in a host's conversation (docs/design/memory-import-mcp.md): the host's agent
//! submits another assistant's export page by page with the MCP tool `propose_import`, and applies the run
//! with `apply_import` after the person agrees in the conversation. A page is planned as `import plan --host
//! jsonl` plans an export file and proposed like a plan, under the connection's platform; nothing here reads
//! a file. The reports are the shim's own words, so what the person sees before agreeing is the daemon's
//! proposals, not the agent's summary of them.
use memory_domain::import::{
    is_session_turn, ImportLimits, ImportProposal, ImportProposeReq, ImportUnit, CONFIRMED_CHAT,
    MAX_BATCH_UNITS, MODE_EXTRACT, PROPOSED, REJECTED, RUN_PROPOSED, RUN_PROPOSING, UNIT_FAILED,
};
use memory_rules::import::units::content_hash;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use super::client::{batches, run_id_of, Client, RunView, MAX_BATCH_BYTES};
use super::commands::one_line;
use super::jsonl;
use super::plan::{build, Plan, PlanOptions, IMPORT};
use super::source::{ScanOutput, SourceDoc};
use crate::harness::{invalid, CallError, Error};

/// `via` of the units of an export page sent through MCP.
pub const VIA: &str = "mcp-export";
/// Largest export page one `propose_import` call takes, in bytes.
pub const MAX_PAGE: usize = 64 << 10;
/// Most proposals one confirmation in the conversation applies; a larger run is reviewed with the CLI.
pub const MAX_CHAT_APPLY: usize = 200;
/// Proposals a report lists by text.
const LISTED: usize = 50;
/// Where the issues of a page are filed.
const PAGE: &str = "export page";

/// One `propose_import` call, checked where it entered.
#[derive(Debug, Clone, Copy)]
pub struct Page<'a> {
    pub project: &'a str,
    /// The connection's platform label: the run's host.
    pub platform: &'a str,
    /// The source the agent named (`chatgpt`, `self`); provenance only.
    pub source: Option<&'a str>,
    pub export: &'a str,
    /// The run of the earlier pages.
    pub run_id: Option<&'a str>,
    pub finish: bool,
}

/// The plan of one export page: its lines as the units an export file gives (design §6.11 of the import),
/// redacted and sized by the default recall budget, under the connection's platform, with the source the
/// agent named kept as `exportedFrom`. With `no_extract`, lines over the unit cap are split.
pub fn plan_page(page: &Page, no_extract: bool, home: &Path, now: &str) -> Result<Plan, Error> {
    let (segments, invalid_lines) = jsonl::segments(page.export);
    let mut scan = ScanOutput::default();
    for why in invalid_lines {
        scan.issue(PAGE.into(), why);
    }
    if !segments.is_empty() {
        scan.docs.push(SourceDoc {
            host: page.platform.into(),
            via: VIA,
            path: PathBuf::new(),
            display_path: String::new(),
            native_id: "export".into(),
            title: None,
            cwd: None,
            user_level: false,
            file_hash: content_hash(page.export),
            stated_at: String::new(),
            kind_hint: None,
            segments,
            extra: json!({ "exportedFrom": page.source }),
        });
    }
    let options = PlanOptions {
        host: page.platform.into(),
        project: page.project.into(),
        home: home.to_path_buf(),
        workspaces: vec![],
        limits: ImportLimits::default(),
        no_extract,
        created_at: now.into(),
        sessions: None,
    };
    build(scan, &options)
}

/// The units of `plan` to send: those it imports that the run does not have yet. A text an earlier page
/// already gave has the same unit key, and a run takes each key once.
fn new_units(plan: &Plan, known: &HashSet<String>) -> (Vec<ImportUnit>, usize) {
    let units: Vec<&ImportUnit> = plan.import_units().collect();
    let fresh: Vec<ImportUnit> = units
        .iter()
        .filter(|u| !known.contains(&u.unit))
        .map(|u| (*u).clone())
        .collect();
    let repeated = units.len() - fresh.len();
    (fresh, repeated)
}

/// Why sending a page stopped.
enum Stopped {
    /// The daemon has no generation model for an extract unit; the run so far, if one was made.
    NoModel(Option<String>),
    Failed(Error),
}

/// Send `units` to `run_id` (a new run when `None`) in batches; with `finish` the last batch, or an empty
/// one when there is nothing new, makes the run `proposed`. Returns the run, if there is one.
async fn send(
    client: &Client,
    page: &Page<'_>,
    mut run_id: Option<String>,
    units: &[ImportUnit],
) -> Result<Option<String>, Stopped> {
    let mut chunks = batches(units, MAX_BATCH_UNITS, MAX_BATCH_BYTES);
    if chunks.is_empty() {
        if !(page.finish && run_id.is_some()) {
            return Ok(run_id);
        }
        chunks.push(&[]);
    }
    let last = chunks.len() - 1;
    for (index, chunk) in chunks.into_iter().enumerate() {
        let request = ImportProposeReq {
            project: page.project.into(),
            host: page.platform.into(),
            run_id: run_id.clone(),
            finish: page.finish && index == last,
            limits: ImportLimits::default(),
            plan_hash: None,
            reconsider: false,
            units: chunk.to_vec(),
        };
        match client
            .call_admin("import_propose", &request, page.platform)
            .await
        {
            Ok(answer) => {
                let id = run_id_of(&answer).ok_or_else(|| {
                    Stopped::Failed(invalid("import_propose answered without a runId"))
                })?;
                run_id = Some(id);
            }
            // `MODEL_UNAVAILABLE`: a batch with an extract unit and no generation model, refused before any
            // unit of it was taken.
            Err(e)
                if e.error == CallError::Status(503)
                    && chunk.iter().any(|u| u.mode == MODE_EXTRACT) =>
            {
                return Err(Stopped::NoModel(run_id))
            }
            Err(e) => return Err(Stopped::Failed(client.failed(e))),
        }
    }
    Ok(run_id)
}

/// `propose_import`: plan the page and propose its new units as part of the run. When the daemon has no
/// generation model, the page is planned again with lines over the cap split at sentences, and the units
/// the run does not have yet are sent. Returns the report for the agent.
pub async fn propose_page(client: &Client, page: &Page<'_>, home: &Path) -> Result<String, Error> {
    let mut known = HashSet::new();
    if let Some(id) = page.run_id {
        let view = client.run(id).await?;
        if view.run.status != RUN_PROPOSING {
            return Err(invalid(format!(
                "run {id} is {}; only a run still taking pages (proposing) takes another one",
                view.run.status
            )));
        }
        if view.run.host != page.platform || view.run.project != page.project {
            return Err(invalid(format!(
                "run {id} imports into project {:?} from {}, not {:?} from {}",
                view.run.project, view.run.host, page.project, page.platform
            )));
        }
        known = view.run.units.iter().map(|u| u.unit.clone()).collect();
    }
    let now = crate::harness::now();
    let mut plan = plan_page(page, false, home, &now)?;
    let (units, mut repeated) = new_units(&plan, &known);
    // The lines an earlier page gave; this page's own batches do not count.
    let earlier = known.clone();
    let run_id = page.run_id.map(str::to_string);
    let mut split = false;
    let run_id = match send(client, page, run_id, &units).await {
        Ok(id) => id,
        Err(Stopped::Failed(e)) => return Err(e),
        Err(Stopped::NoModel(so_far)) => {
            split = true;
            plan = plan_page(page, true, home, &now)?;
            if let Some(id) = &so_far {
                known = client
                    .run(id)
                    .await?
                    .run
                    .units
                    .iter()
                    .map(|u| u.unit.clone())
                    .collect();
            }
            let (units, _) = new_units(&plan, &known);
            repeated = new_units(&plan, &earlier).1;
            match send(client, page, so_far, &units).await {
                Ok(id) => id,
                Err(Stopped::Failed(e)) => return Err(e),
                Err(Stopped::NoModel(_)) => {
                    unreachable!("a page planned with no_extract has no extract unit")
                }
            }
        }
    };
    let view = match &run_id {
        Some(id) => Some(client.run(id).await?),
        None => None,
    };
    Ok(page_report(page, &plan, repeated, split, view.as_ref()))
}

/// The proposals a person decides on: not rejected when proposed, not decided yet, no session turn.
fn open(proposals: &[ImportProposal]) -> Vec<&ImportProposal> {
    proposals
        .iter()
        .filter(|p| p.status == PROPOSED && !is_session_turn(p))
        .collect()
}

fn page_report(
    page: &Page,
    plan: &Plan,
    repeated: usize,
    split: bool,
    view: Option<&RunView>,
) -> String {
    let mut out = String::new();
    let units = plan.units.len();
    let mut skipped: BTreeMap<&str, usize> = BTreeMap::new();
    for unit in plan.units.iter().filter(|u| u.decision != IMPORT) {
        *skipped.entry(unit.reason.as_str()).or_insert(0) += 1;
    }
    let invalid_lines = plan.summary.issues.len();
    let _ = writeln!(
        out,
        "本页：{units} 个导入单元（超长的行会切成几个），{invalid_lines} 行不是合法的导出行；跳过 {}；和这个 run 已有的行重复 {repeated} 行。",
        tally(&skipped)
    );
    for issue in plan.summary.issues.iter().take(5) {
        let _ = writeln!(out, "  {}", issue.reason);
    }
    if split {
        let _ = writeln!(
            out,
            "daemon 没有配置生成模型：超过单元上限的行按段落和句子切开，没有抽取。"
        );
    }
    let Some(view) = view else {
        let next = match page.finish {
            true => "没有要写入的内容。",
            false => "还有下一页时照常提交，不用带 runId。",
        };
        let _ = writeln!(out, "这一页没有可以导入的行，没有建立 run。{next}");
        return out;
    };
    let run = &view.run;
    let reviewed: Vec<&ImportProposal> = view
        .proposals
        .iter()
        .filter(|p| !is_session_turn(p))
        .collect();
    let waiting = open(&view.proposals);
    let rejected: Vec<&&ImportProposal> =
        reviewed.iter().filter(|p| p.status == REJECTED).collect();
    let duplicates = waiting.iter().filter(|p| p.duplicate_of.is_some()).count();
    let _ = writeln!(
        out,
        "run {}：{}（项目 {}，平台 {}，来源 {}）",
        run.run_id,
        run.status,
        run.project,
        run.host,
        page.source.unwrap_or("未注明")
    );
    let _ = writeln!(
        out,
        "到目前为止：提议 {} 条，其中待决定 {} 条（{} 条和已有记录文本相同，写入时链接到那条记录），被拒 {} 条；没有变化的单元 {} 个。",
        reviewed.len(),
        waiting.len(),
        duplicates,
        rejected.len(),
        run.counts.unchanged
    );
    let failed: Vec<_> = run
        .units
        .iter()
        .filter(|u| u.outcome == UNIT_FAILED)
        .collect();
    if !failed.is_empty() {
        let _ = writeln!(
            out,
            "有 {} 个单元抽取失败，没有提议，写入时不会包含它们（另起一次导入时会重新抽取）：",
            failed.len()
        );
        for unit in failed.iter().take(5) {
            let _ = writeln!(
                out,
                "  {}：{}",
                unit.unit,
                unit.error.as_deref().unwrap_or("")
            );
        }
    }
    for p in rejected.iter().take(10) {
        let _ = writeln!(
            out,
            "  被拒：{} —— {}",
            one_line(&p.text, 80),
            p.reject_reason.as_deref().unwrap_or("")
        );
    }
    if !waiting.is_empty() {
        let _ = writeln!(out, "待决定的提议：");
        for (n, p) in waiting.iter().take(LISTED).enumerate() {
            let _ = writeln!(
                out,
                "  {}. [{}] {}",
                n + 1,
                p.item_kind,
                one_line(&p.text, 160)
            );
        }
        if waiting.len() > LISTED {
            let _ = writeln!(
                out,
                "  ……另有 {} 条，用 agent-memoryd import show {} 查看",
                waiting.len() - LISTED,
                run.run_id
            );
        }
    }
    if run.status == RUN_PROPOSING {
        let _ = writeln!(
            out,
            "下一步：还有下一页时再调用 propose_import，runId 填 {}；最后一页带 finish: true（没有新的行也可以只带 finish）。这些提议还没有写入。",
            run.run_id
        );
    } else {
        let _ = writeln!(
            out,
            "这些提议还没有写入。下一步：把上面的提议给用户看，问他是否写入，然后停下来等他回复。用户在自己的消息里明确同意后，调用 apply_import，runId 填 {}，expectedProposals 填 {}（待决定的条数）。用户要逐条决定时，请他在终端运行 agent-memoryd import show {}，改好评审文件后 agent-memoryd import apply {} --review <评审文件>。",
            run.run_id,
            waiting.len(),
            run.run_id,
            run.run_id
        );
    }
    out
}

/// `apply_import`: write every open proposal of a run the connection proposed, after the person agreed in
/// the conversation. The server cannot see that agreement, so the step is narrow: only a `proposed` run of
/// this platform whose proposals all came from `propose_import`, only when `expected` is the number of
/// proposals still to decide (what the person was shown), and at most `MAX_CHAT_APPLY` of them. The run
/// records `confirmedVia: chat`.
pub async fn apply_chat(
    client: &Client,
    platform: &str,
    run_id: &str,
    expected: usize,
) -> Result<String, Error> {
    let view = client.run(run_id).await?;
    let run = &view.run;
    if run.host != platform {
        return Err(invalid(format!(
            "run {run_id} was proposed by {}, not by this connection ({platform})",
            run.host
        )));
    }
    if run.status != RUN_PROPOSED {
        return Err(invalid(format!(
            "run {run_id} is {}; only a run whose last page was sent with finish (proposed) can be applied",
            run.status
        )));
    }
    let reviewed: Vec<&ImportProposal> = view
        .proposals
        .iter()
        .filter(|p| !is_session_turn(p))
        .collect();
    if reviewed
        .iter()
        .any(|p| p.source.get("via") != Some(&json!(VIA)))
    {
        return Err(invalid(format!(
            "run {run_id} was not proposed through propose_import; review it in a terminal: agent-memoryd import show {run_id}"
        )));
    }
    let waiting = open(&view.proposals).len();
    if waiting == 0 {
        return Err(invalid(format!(
            "run {run_id} has no proposal waiting for a decision: nothing to write"
        )));
    }
    if waiting != expected {
        return Err(invalid(format!(
            "run {run_id} has {waiting} proposals waiting for a decision, not {expected}: show the person the run's proposals again and ask before applying"
        )));
    }
    if waiting > MAX_CHAT_APPLY {
        return Err(invalid(format!(
            "run {run_id} has {waiting} proposals, more than {MAX_CHAT_APPLY} for one confirmation in a conversation: review them in a terminal with agent-memoryd import show {run_id}, then agent-memoryd import apply {run_id} --review <review file>"
        )));
    }
    let applied = client
        .apply_confirmed(run_id, &run.host, CONFIRMED_CHAT)
        .await?;
    let linked = applied
        .proposals
        .iter()
        .filter(|p| {
            p.applied
                .as_ref()
                .is_some_and(|a| a.get("dedupedInto").is_some())
        })
        .count();
    let c = &applied.run.counts;
    Ok(format!(
        "run {run_id}：{}。写入 {} 条（其中 {linked} 条链接到文本相同的已有记录），跳过 {} 条，拒绝 {} 条。\n\
         这些记录是项目 {} 的参考事实（origin imported），不是指令，get_context 不会列出它们。\n\
         要撤销整次导入，请用户在终端运行 agent-memoryd import rollback {run_id}。\n",
        applied.run.status,
        c.applied,
        c.skipped,
        c.dropped,
        applied.run.project
    ))
}

/// `reason count` pairs, or `0 行`.
fn tally(counts: &BTreeMap<&str, usize>) -> String {
    match counts.is_empty() {
        true => "0 行".into(),
        false => counts
            .iter()
            .map(|(reason, n)| format!("{reason} {n} 行"))
            .collect::<Vec<_>>()
            .join("、"),
    }
}

/// A JSON number argument that is a count; a whole number sent as a float (`3.0`) counts too.
pub fn count_arg(value: Option<&Value>) -> Option<usize> {
    let value = value?;
    let n = match value.as_u64() {
        Some(n) => n,
        None => {
            let f = value.as_f64().filter(|f| *f >= 0.0 && f.fract() == 0.0)?;
            f as u64
        }
    };
    usize::try_from(n).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::{Path as UrlPath, State};
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::{IntoResponse, Response};
    use axum::routing::post;
    use axum::{Json, Router};
    use memory_domain::import::{item_kind_for_hint, ImportRun, KIND, MODE_SPLIT, RUN_APPLIED};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    const HOME: &str = "/home/u";

    /// The one run `ir1` of the stub daemon and what it was asked: (endpoint, platform, body).
    #[derive(Default)]
    struct Daemon {
        run: Option<ImportRun>,
        proposals: Vec<ImportProposal>,
        requests: Vec<(String, String, Value)>,
        /// Refuse batches with an extract unit, as a daemon without a generation key does.
        no_model: bool,
    }

    type Shared = Arc<Mutex<Daemon>>;

    async fn answer(
        State(daemon): State<Shared>,
        UrlPath(name): UrlPath<String>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> Response {
        let platform = headers["x-memory-platform"].to_str().unwrap().to_string();
        let mut d = daemon.lock().unwrap();
        d.requests.push((name.clone(), platform, body.clone()));
        let refuse = |status: StatusCode, message: &str| {
            (status, Json(json!({"code": "x", "message": message}))).into_response()
        };
        match name.as_str() {
            "import_propose" => {
                let req: ImportProposeReq = serde_json::from_value(body).unwrap();
                if d.no_model && req.units.iter().any(|u| u.mode == MODE_EXTRACT) {
                    return refuse(StatusCode::SERVICE_UNAVAILABLE, "no generation model");
                }
                let run = d.run.get_or_insert_with(|| ImportRun {
                    run_id: "ir1".into(),
                    kind: KIND.into(),
                    project: req.project.clone(),
                    host: req.host.clone(),
                    status: RUN_PROPOSING.into(),
                    ..ImportRun::default()
                });
                if req.run_id.as_deref().is_some_and(|id| id != "ir1") {
                    return refuse(StatusCode::NOT_FOUND, "unknown import run");
                }
                let base = run.units.len();
                for (offset, unit) in req.units.iter().enumerate() {
                    run.units.push(memory_domain::import::UnitOutcome {
                        unit: unit.unit.clone(),
                        index: base + offset,
                        mode: unit.mode.clone(),
                        ..Default::default()
                    });
                }
                if req.finish {
                    run.status = RUN_PROPOSED.into();
                }
                let answer = json!({"run": run});
                let n = d.proposals.len();
                for (offset, unit) in req.units.iter().enumerate() {
                    d.proposals.push(ImportProposal {
                        proposal_id: format!("ip{}", n + offset + 1),
                        run_id: "ir1".into(),
                        unit: unit.unit.clone(),
                        mode: unit.mode.clone(),
                        item_kind: item_kind_for_hint(unit.kind_hint.as_deref()).into(),
                        text: unit.text.clone(),
                        source: unit.source.clone(),
                        status: PROPOSED.into(),
                        ..ImportProposal::default()
                    });
                }
                Json(answer).into_response()
            }
            "import_run" if body["runId"] == "ir1" && d.run.is_some() => {
                Json(json!({"run": d.run, "proposals": d.proposals})).into_response()
            }
            "import_apply" if body["runId"] == "ir1" => {
                let applied = d.proposals.iter().filter(|p| p.status == PROPOSED).count();
                let run = d.run.as_mut().unwrap();
                run.status = RUN_APPLIED.into();
                run.counts.applied = applied;
                run.confirmed_via = body["confirmedVia"].as_str().map(String::from);
                for p in d.proposals.iter_mut().filter(|p| p.status == PROPOSED) {
                    p.status = "applied".into();
                }
                Json(json!({"run": d.run, "proposals": d.proposals})).into_response()
            }
            _ => refuse(StatusCode::NOT_FOUND, "unknown import run"),
        }
    }

    async fn daemon(state: Daemon) -> (Client, Shared) {
        let shared: Shared = Arc::new(Mutex::new(state));
        let app = Router::new()
            .route("/admin/{name}", post(answer))
            .with_state(shared.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (Client::new(&url, Duration::from_secs(5)).unwrap(), shared)
    }

    fn page<'a>(export: &'a str, run_id: Option<&'a str>, finish: bool) -> Page<'a> {
        Page {
            project: "atlas",
            platform: "claude-code",
            source: Some("chatgpt"),
            export,
            run_id,
            finish,
        }
    }

    fn line(text: &str, kind: &str) -> String {
        json!({"text": text, "kind": kind, "date": "2026-05-01"}).to_string()
    }

    #[test]
    fn a_page_is_planned_under_the_connection_with_the_named_source() {
        let export = format!(
            "```json\n{}\nnot json\n{}\n```",
            line("Atlas deploys on Fridays.", "fact"),
            line("Always run make check before pushing.", "instruction"),
        );
        let plan = plan_page(&page(&export, None, false), false, Path::new(HOME), "now").unwrap();
        let units: Vec<&ImportUnit> = plan.import_units().collect();
        assert_eq!(units.len(), 2);
        let of = |text: &str| *units.iter().find(|u| u.text == text).unwrap();
        let unit = of("Atlas deploys on Fridays.");
        assert!(
            unit.unit.starts_with("claude-code:export#"),
            "{}",
            unit.unit
        );
        assert_eq!(unit.host, "claude-code");
        assert_eq!(unit.stated_at, "2026-05-01T00:00:00Z");
        // Provenance names the page's line and the source as the agent gave it, and no file.
        assert_eq!(unit.source["via"], VIA);
        assert_eq!(unit.source["exportedFrom"], "chatgpt");
        assert_eq!(unit.source["line"], 2);
        assert!(unit.source.get("path").is_none(), "{}", unit.source);
        // A rule is a preference, never an instruction.
        let rule = of("Always run make check before pushing.");
        assert_eq!(rule.kind_hint.as_deref(), Some("preference"));
        assert_eq!(
            plan.summary.issues[0].reason,
            "invalid-line 3: not a JSON object"
        );
    }

    #[tokio::test]
    async fn pages_make_one_run_and_a_line_repeated_across_pages_is_sent_once() {
        let (client, daemon) = daemon(Daemon::default()).await;
        let home = Path::new(HOME);
        let first = format!(
            "{}\n{}",
            line("Atlas deploys on Fridays.", "fact"),
            line("Atlas uses Nomad.", "fact")
        );
        let report = propose_page(&client, &page(&first, None, false), home)
            .await
            .unwrap();
        assert!(report.contains("run ir1：proposing"), "{report}");
        assert!(report.contains("runId 填 ir1"), "{report}");
        let second = format!(
            "{}\n{}",
            line("Atlas uses Nomad.", "fact"),
            line("Docs live in the atlas wiki.", "reference")
        );
        let report = propose_page(&client, &page(&second, Some("ir1"), true), home)
            .await
            .unwrap();
        assert!(report.contains("重复 1 行"), "{report}");
        assert!(report.contains("run ir1：proposed"), "{report}");
        assert!(!report.contains("抽取失败"), "{report}");
        assert!(report.contains("expectedProposals 填 3"), "{report}");
        let d = daemon.lock().unwrap();
        assert_eq!(d.run.as_ref().unwrap().units.len(), 3);
        let proposes: Vec<&(String, String, Value)> = d
            .requests
            .iter()
            .filter(|r| r.0 == "import_propose")
            .collect();
        assert_eq!(proposes.len(), 2);
        assert!(proposes.iter().all(|r| r.1 == "claude-code"));
        assert_eq!(proposes[1].2["runId"], "ir1");
        assert_eq!(proposes[1].2["finish"], true);
    }

    #[tokio::test]
    async fn a_finish_only_page_closes_the_run_and_other_runs_are_refused() {
        let (client, daemon) = daemon(Daemon::default()).await;
        let home = Path::new(HOME);
        let export = line("Atlas uses Nomad.", "fact");
        propose_page(&client, &page(&export, None, false), home)
            .await
            .unwrap();
        // Another platform, or another project, cannot add to the run.
        let codex = Page {
            platform: "codex",
            ..page(&export, Some("ir1"), false)
        };
        assert!(crate::harness::import::message(
            &propose_page(&client, &codex, home).await.unwrap_err()
        )
        .contains("from claude-code"));
        let report = propose_page(&client, &page("", Some("ir1"), true), home)
            .await
            .unwrap();
        assert!(report.contains("run ir1：proposed"), "{report}");
        let last = daemon
            .lock()
            .unwrap()
            .requests
            .iter()
            .rev()
            .find(|r| r.0 == "import_propose")
            .unwrap()
            .2
            .clone();
        assert_eq!(
            (last["finish"].clone(), last["units"].clone()),
            (json!(true), json!([]))
        );
        // A proposed run takes no more pages.
        let error = propose_page(&client, &page(&export, Some("ir1"), false), home)
            .await
            .unwrap_err();
        assert!(crate::harness::import::message(&error).contains("is proposed"));
        // An empty first page makes no run.
        let (client, daemon) = super::tests::daemon(Daemon::default()).await;
        let report = propose_page(&client, &page("not json", None, true), home)
            .await
            .unwrap();
        assert!(report.contains("没有建立 run"), "{report}");
        assert!(daemon.lock().unwrap().requests.is_empty());
    }

    #[tokio::test]
    async fn without_a_generation_model_long_lines_are_split_instead() {
        let (client, daemon) = daemon(Daemon {
            no_model: true,
            ..Daemon::default()
        })
        .await;
        let long = "Atlas keeps every build log for audits. ".repeat(30);
        let export = format!(
            "{}\n{}",
            line("Atlas uses Nomad.", "fact"),
            line(long.trim(), "fact")
        );
        let report = propose_page(&client, &page(&export, None, true), Path::new(HOME))
            .await
            .unwrap();
        assert!(report.contains("没有配置生成模型"), "{report}");
        assert!(report.contains("重复 0 行"), "{report}");
        let d = daemon.lock().unwrap();
        let modes: Vec<&str> = d
            .run
            .as_ref()
            .unwrap()
            .units
            .iter()
            .map(|u| u.mode.as_str())
            .collect();
        assert!(modes.contains(&MODE_SPLIT), "{modes:?}");
        assert!(!modes.contains(&MODE_EXTRACT), "{modes:?}");
        assert_eq!(d.run.as_ref().unwrap().status, RUN_PROPOSED);
    }

    #[test]
    fn counts_may_arrive_as_whole_floats() {
        assert_eq!(count_arg(Some(&json!(3))), Some(3));
        assert_eq!(count_arg(Some(&json!(3.0))), Some(3));
        for bad in [json!(3.5), json!(-1), json!("3"), json!(null)] {
            assert_eq!(count_arg(Some(&bad)), None, "{bad}");
        }
        assert_eq!(count_arg(None), None);
    }

    #[test]
    fn the_page_report_names_units_whose_extraction_failed() {
        let plan = plan_page(&page("", None, true), false, Path::new(HOME), "now").unwrap();
        let run = ImportRun {
            run_id: "ir1".into(),
            status: RUN_PROPOSED.into(),
            units: vec![memory_domain::import::UnitOutcome {
                unit: "claude-code:export#abc".into(),
                outcome: UNIT_FAILED.into(),
                error: Some("generation timed out".into()),
                ..Default::default()
            }],
            ..ImportRun::default()
        };
        let view = RunView {
            run,
            proposals: vec![],
        };
        let report = page_report(&page("", None, true), &plan, 0, false, Some(&view));
        assert!(report.contains("1 个单元抽取失败"), "{report}");
        assert!(
            report.contains("claude-code:export#abc：generation timed out"),
            "{report}"
        );
    }

    /// A stub daemon holding the proposed run `ir1` of `claude-code` with `n` open proposals from `via`.
    async fn proposed(n: usize, via: &str) -> (Client, Shared) {
        let proposals = (0..n)
            .map(|i| ImportProposal {
                proposal_id: format!("ip{i}"),
                run_id: "ir1".into(),
                item_kind: "fact".into(),
                text: format!("Fact {i}."),
                source: json!({"via": via}),
                status: PROPOSED.into(),
                ..ImportProposal::default()
            })
            .collect();
        let run = ImportRun {
            run_id: "ir1".into(),
            project: "atlas".into(),
            host: "claude-code".into(),
            status: RUN_PROPOSED.into(),
            ..ImportRun::default()
        };
        daemon(Daemon {
            run: Some(run),
            proposals,
            ..Daemon::default()
        })
        .await
    }

    #[tokio::test]
    async fn chat_confirmation_applies_only_the_batch_the_person_saw() {
        let refused =
            |result: Result<String, Error>| crate::harness::import::message(&result.unwrap_err());
        let (client, daemon) = proposed(3, VIA).await;
        assert!(refused(apply_chat(&client, "claude-code", "ir1", 2).await)
            .contains("3 proposals waiting for a decision, not 2"));
        assert!(refused(apply_chat(&client, "codex", "ir1", 3).await)
            .contains("not by this connection"));
        assert!(daemon
            .lock()
            .unwrap()
            .requests
            .iter()
            .all(|r| r.0 != "import_apply"));
        let report = apply_chat(&client, "claude-code", "ir1", 3).await.unwrap();
        assert!(report.contains("写入 3 条"), "{report}");
        assert!(
            report.contains("agent-memoryd import rollback ir1"),
            "{report}"
        );
        let d = daemon.lock().unwrap();
        let (name, platform, body) = d.requests.last().unwrap();
        assert_eq!(
            (name.as_str(), platform.as_str()),
            ("import_apply", "claude-code")
        );
        assert_eq!(
            body,
            &json!({"runId": "ir1", "decisions": {}, "confirmedVia": "chat"})
        );
        assert_eq!(
            d.run.as_ref().unwrap().confirmed_via.as_deref(),
            Some(CONFIRMED_CHAT)
        );
        drop(d);
        // Applied: nothing waits any more.
        assert!(refused(apply_chat(&client, "claude-code", "ir1", 3).await).contains("is applied"));

        // A run proposed with the CLI, one too large, and one with nothing to write are left to the terminal.
        let (client, _) = proposed(2, "agent-export").await;
        assert!(refused(apply_chat(&client, "claude-code", "ir1", 2).await)
            .contains("not proposed through propose_import"));
        let (client, _) = proposed(MAX_CHAT_APPLY + 1, VIA).await;
        assert!(
            refused(apply_chat(&client, "claude-code", "ir1", MAX_CHAT_APPLY + 1).await)
                .contains("import show ir1")
        );
        let (client, _) = proposed(0, VIA).await;
        assert!(refused(apply_chat(&client, "claude-code", "ir1", 0).await)
            .contains("nothing to write"));
    }
}
