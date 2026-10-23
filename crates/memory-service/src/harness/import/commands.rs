//! The work of the `agent-memoryd import` commands and the reports they print: `make_plan` scans a host
//! and writes the plan, `propose`, `show`, `apply`, `status` and `rollback` call the daemon through
//! `client`. Reports are plain text for a person; the files they name hold the full detail.
use chrono::{DateTime, Utc};
use memory_domain::import::{
    is_session_turn, ImportLimits, ImportProposal, ImportRun, APPLIED, ROLLED_BACK, RUN_APPLIED,
    UNIT_FAILED,
};
use memory_rules::util::fmt_ts;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::client::{self, Client, RunView};
use super::plan::{self, Plan, PlanOptions, SessionOptions};
use super::source::{Issue, ScanContext, ScanOutput};
use super::{project, review, shown, DaemonArgs, Host, Include, PlanArgs, DEFAULT_URL};
use crate::harness::outbox::{Outbox, Queued};
use crate::harness::{expand, home, invalid, Error};

/// Seconds per daemon request of every command but `propose`.
const CALL_TIMEOUT: Duration = Duration::from_secs(120);
/// Runs `status` lists without a run.
const STATUS_RUNS: usize = 10;
/// Proposals `show` prints.
const SHOW_PROPOSALS: usize = 10;
/// Paths the plan summary prints per issue reason.
const ISSUE_PATHS: usize = 5;
/// Session turns `apply` reports progress after.
const PROGRESS_TURNS: usize = 25;

/// Scan the host, plan its units and write the plan file; returns where.
pub fn make_plan(args: &PlanArgs, now: DateTime<Utc>) -> Result<(PathBuf, Plan), Error> {
    if args.host != Host::Jsonl && (args.from.is_some() || args.label.is_some()) {
        return Err(invalid("--from and --label are for --host jsonl"));
    }
    if args.all && args.host != Host::ClaudeCode {
        return Err(invalid("--all is for --host claude-code"));
    }
    let sessions = args.include.contains(&Include::Sessions);
    let session_adapter = match sessions {
        true => Some(args.host.session_adapter().ok_or_else(|| {
            invalid("--include sessions is for --host claude-code and --host codex")
        })?),
        false => None,
    };
    if !sessions && args.since.is_some() {
        return Err(invalid("--since is for --include sessions"));
    }
    let home = args.home.as_deref().map(expand).unwrap_or_else(home);
    let workspaces = workspace_roots(&args.workspaces)?;
    let ctx = ScanContext {
        home: home.clone(),
        claude_config_dir: args.claude_config_dir.as_deref().map(expand),
        codex_home: args.codex_home.as_deref().map(expand),
        workbuddy_home: args.workbuddy_home.as_deref().map(expand),
        workspaces: workspaces.clone(),
        all: args.all,
        from: args.from.as_deref().map(expand),
        label: args.label.clone(),
    };
    let mut scan = match args.include.contains(&Include::Notes) {
        true => args.host.adapter().scan(&ctx)?,
        false => ScanOutput::default(),
    };
    if let Some(adapter) = session_adapter {
        let found = adapter.scan_sessions(&ctx)?;
        scan.sessions = found.sessions;
        scan.issues.extend(found.issues);
    }
    let host = args.host.label(args.label.as_deref());
    let options = PlanOptions {
        host: host.clone(),
        project: args.project.clone(),
        home: home.clone(),
        workspaces,
        limits: ImportLimits {
            recall_budget: args.recall_budget,
            recall_top_k: args.recall_top_k,
        },
        no_extract: args.no_extract,
        created_at: fmt_ts(&now),
        sessions: sessions.then(|| SessionOptions {
            since: args.since.clone(),
            limit: args.limit,
        }),
    };
    let plan = plan::build(scan, &options)?;
    let path = match args.out.as_deref() {
        Some(out) => expand(out),
        None => plan::default_path(&home, &host, &args.project, now),
    };
    plan::write(&plan, &path)?;
    Ok((path, plan))
}

/// The repository roots of the `--workspace` directories, each once.
fn workspace_roots(dirs: &[PathBuf]) -> Result<Vec<PathBuf>, Error> {
    let mut roots: Vec<PathBuf> = Vec::new();
    for dir in dirs {
        let path = expand(dir);
        if !path.is_dir() {
            return Err(invalid(format!(
                "--workspace {}: not a directory",
                shown(&path)
            )));
        }
        let root = project::repository_root(&path);
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
    Ok(roots)
}

/// `a 1, b 2`.
fn tally<'a>(counts: impl IntoIterator<Item = (&'a String, &'a usize)>) -> String {
    counts
        .into_iter()
        .map(|(name, n)| format!("{name} {n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Issues by kind of reason, one entry per path with the reason's detail after it: `unreadable: <error>`
/// counts as `unreadable`, `invalid-line N: <why>` as `invalid-line`.
fn issue_groups(issues: &[Issue]) -> BTreeMap<String, Vec<String>> {
    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for issue in issues {
        let reason = issue.reason.as_str();
        let (kind, detail) = match reason.split_once(": ") {
            Some((head, why)) => match head.split_once(' ') {
                Some((kind, line)) => (kind, format!("line {line}: {why}")),
                None => (head, why.to_string()),
            },
            None => (reason, String::new()),
        };
        let entry = match detail.is_empty() {
            true => issue.path.clone(),
            false => format!("{} ({detail})", issue.path),
        };
        groups.entry(kind.to_string()).or_default().push(entry);
    }
    groups
}

/// What `plan` prints: counts, skip reasons, issues by reason, what goes to the model services, the file.
pub fn plan_report(path: &Path, plan: &Plan, args: &PlanArgs) -> String {
    let s = &plan.summary;
    let c = &s.counts;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "Plan for {} into project {:?}: {} documents, {} units",
        s.host, s.project, c.docs, c.units
    );
    if !s.workspaces.is_empty() {
        let _ = writeln!(out, "  workspaces: {}", s.workspaces.join(", "));
    }
    let turns = match &s.sessions {
        Some(_) => format!(", {} session turns", c.session_turns),
        None => String::new(),
    };
    let _ = writeln!(
        out,
        "  to import: {} verbatim, {} split, {} extract{turns}",
        c.verbatim, c.split, c.extract
    );
    if c.skipped > 0 {
        let _ = writeln!(out, "  skipped: {} ({})", c.skipped, tally(&s.skip_reasons));
    }
    if let Some(sessions) = &s.sessions {
        let from = match &sessions.since {
            Some(since) => format!("from {since}, "),
            None => String::new(),
        };
        let _ = writeln!(
            out,
            "  sessions: {} read; turns {from}oldest first, at most {}",
            sessions.sessions, sessions.limit
        );
        let mut left = Vec::new();
        if sessions.before_since > 0 {
            left.push(format!("{} before --since", sessions.before_since));
        }
        if sessions.copies > 0 {
            left.push(format!(
                "{} repeated by a forked or resumed session",
                sessions.copies
            ));
        }
        if sessions.over_limit > 0 {
            left.push(format!("{} over --limit", sessions.over_limit));
        }
        if !left.is_empty() {
            let _ = writeln!(out, "  turns left out: {}", left.join(", "));
        }
        if let Some(next) = &sessions.next_since {
            let _ = writeln!(
                out,
                "  next batch: plan again with --since {next} once this one is applied"
            );
        }
    }
    if !s.redactions.is_empty() {
        let _ = writeln!(out, "  redacted: {}", tally(&s.redactions));
    }
    for (reason, entries) in issue_groups(&s.issues) {
        let listed = entries[..entries.len().min(ISSUE_PATHS)].join(", ");
        let more = match entries.len().saturating_sub(ISSUE_PATHS) {
            0 => String::new(),
            n => format!(", and {n} more"),
        };
        let _ = writeln!(out, "  {reason} ({}): {listed}{more}", entries.len());
    }
    let e = &s.estimates;
    let _ = writeln!(
        out,
        "  model services: {} generation calls with {} characters; {} texts with {} characters to embed, plus two per extracted item",
        e.generation_calls, e.chars_to_generation, e.embedding_texts, e.chars_to_embedding
    );
    if c.session_turns > 0 {
        let _ = writeln!(
            out,
            "  session turns go through Mem0 formation once applied: a generation call each, with the session's earlier turns as context, and an embedding per fact formed"
        );
    }
    let _ = writeln!(
        out,
        "  unit cap: cost {} (extraction aims at {}), from recall budget {} and top-k {}",
        s.limits.hard_cap, s.limits.target, s.limits.wire.recall_budget, s.limits.wire.recall_top_k
    );
    let file = shown(path);
    let home_flag = match &args.home {
        Some(dir) => format!(" --home {}", shown(&expand(dir))),
        None => String::new(),
    };
    let _ = writeln!(out, "Plan written to {file}");
    let _ = writeln!(
        out,
        "Next: review it (delete unit lines or set \"decision\" to \"skip\"), then run: agent-memoryd import propose {file}{home_flag}"
    );
    out
}

/// ` --url U` for the commands a report suggests, when it is not the default.
fn daemon_flags(daemon: &DaemonArgs) -> String {
    let mut flags = String::new();
    if daemon.url != DEFAULT_URL {
        let _ = write!(flags, " --url {}", daemon.url);
    }
    flags
}

/// `propose`: check the plan's sources are unchanged, send its units, report the run. With `reconsider`
/// the daemon also proposes again the unchanged units whose last import applied nothing. Progress goes
/// to `say` as it happens.
pub async fn propose(
    plan_path: &Path,
    daemon: &DaemonArgs,
    batch: usize,
    timeout: Duration,
    home: &Path,
    reconsider: bool,
    say: &mut (dyn FnMut(&str) + Send),
) -> Result<String, Error> {
    let plan = plan::read(plan_path)?;
    let planned = plan.import_units().count();
    let (units, stale) = client::fresh_units(&plan, home);
    for unit in &stale {
        say(&format!(
            "stale-plan: {} ({} {} since the plan)",
            unit.unit, unit.path, unit.why
        ));
    }
    if units.is_empty() {
        return Err(invalid(match planned {
            0 => "nothing to propose: the plan has no units to import".to_string(),
            n => format!(
                "nothing to propose: the sources of all {n} units changed since the plan; make a new plan"
            ),
        }));
    }
    let client = Client::new(&daemon.url, timeout)?;
    say(&format!(
        "Proposing {} units of {} for project {:?}, {batch} per request",
        units.len(),
        plan.summary.host,
        plan.summary.project
    ));
    let mut batch_done = |done: usize, of: usize, run: &str| {
        say(&format!("  batch {done}/{of} sent (run {run})"));
    };
    let view = client
        .propose(&plan, &units, batch, reconsider, &mut batch_done)
        .await?;
    let mut out = run_report(&view.run);
    if !stale.is_empty() {
        let _ = writeln!(
            out,
            "  {} units not proposed: their source changed since the plan (stale-plan)",
            stale.len()
        );
    }
    let (id, flags) = (&view.run.run_id, daemon_flags(daemon));
    // Apply reads the sources again under the same home, or finds them all vanished.
    let home_flag = match home == crate::harness::home() {
        true => String::new(),
        false => format!(" --home {}", shown(home)),
    };
    if view.run.counts.proposals == 0 && view.run.counts.session_turns > 0 {
        let _ = writeln!(
            out,
            "Next: agent-memoryd import apply {id} --yes{flags} queues the session turns into the dialogue outbox (--outbox, as the uploader drains it)"
        );
        return Ok(out);
    }
    let _ = writeln!(out, "Next: agent-memoryd import show {id}{flags}");
    let _ = writeln!(
        out,
        "  then agent-memoryd import apply {id} --review <review file>{flags}{home_flag}, or --yes for the default decisions"
    );
    Ok(out)
}

/// A run's counts, failed units and error.
fn run_report(run: &ImportRun) -> String {
    let c = &run.counts;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "Run {}: {} ({} into project {:?}, created {})",
        run.run_id, run.status, run.host, run.project, run.created_at
    );
    let _ = writeln!(
        out,
        "  units {}: {} unchanged; proposals {}: {} rejected, {} instruction candidates, {} duplicates",
        c.units, c.unchanged, c.proposals, c.rejected, c.instruction_candidates, c.duplicates
    );
    let _ = writeln!(
        out,
        "  generation calls {}, cache hits {}",
        c.generation_calls, c.cache_hits
    );
    if c.applied + c.dropped + c.skipped + c.rolled_back > 0 {
        let _ = writeln!(
            out,
            "  applied {}, dropped {}, skipped {}, rolled back {}",
            c.applied, c.dropped, c.skipped, c.rolled_back
        );
    }
    if c.session_turns > 0 {
        let _ = writeln!(
            out,
            "  session turns {}, registered without a model call: {} queued, {} rolled back",
            c.session_turns, c.turns_queued, c.turns_rolled_back
        );
    }
    for unit in run.units.iter().filter(|u| u.outcome == UNIT_FAILED) {
        let error = unit.error.as_deref().unwrap_or("no error recorded");
        let _ = writeln!(out, "  failed unit {}: {error}", unit.unit);
    }
    if let Some(error) = &run.error {
        let _ = writeln!(out, "  error: {error}");
    }
    out
}

/// The current records earlier imports of the run's changed units wrote, which this run leaves alone
/// unless a proposal supersedes them, and how a person retracts one; nothing when there are none.
/// `listed` says where they are listed.
fn previous_report(run: &ImportRun, listed: &str) -> String {
    let records: usize = run.units.iter().map(|u| u.previous_records.len()).sum();
    if records == 0 {
        return String::new();
    }
    let units = run
        .units
        .iter()
        .filter(|u| !u.previous_records.is_empty())
        .count();
    format!(
        "  earlier records: {records} of {units} changed units stay current unless a proposal of their unit supersedes them; {listed}\n    \
         to retract one, call the MCP tool correct_memory without text, or delete_memory\n"
    )
}

/// Where a run's session turns stand, from their rows: submitted as new requests or found to duplicate a turn
/// the daemon had, still to be submitted, and what rollback did; nothing for a run without turns.
fn turns_report(view: &RunView) -> String {
    let turns: Vec<&ImportProposal> = view
        .proposals
        .iter()
        .filter(|p| is_session_turn(p))
        .collect();
    if turns.is_empty() {
        return String::new();
    }
    let applied = |p: &&ImportProposal| p.applied.clone().unwrap_or(Value::Null);
    let (mut formed, mut duplicates, mut waiting) = (0, 0, 0);
    for p in turns.iter().filter(|p| p.status == APPLIED) {
        match applied(p)["duplicate"].as_bool() {
            Some(true) => duplicates += 1,
            Some(false) => formed += 1,
            None => waiting += 1,
        }
    }
    let mut out = String::new();
    if formed + duplicates + waiting > 0 {
        let _ = writeln!(
            out,
            "  session turns submitted: {formed} new, {duplicates} the daemon already had (captured live or imported before); {waiting} not submitted yet"
        );
    }
    let undone: Vec<Value> = turns
        .iter()
        .filter(|p| p.status == ROLLED_BACK)
        .map(|p| p.rollback.clone().unwrap_or(Value::Null))
        .collect();
    let forming = turns
        .iter()
        .filter(|p| p.status == APPLIED && p.rollback_skip_reason.is_some())
        .count();
    if !undone.is_empty() || forming > 0 {
        let length = |v: &Value, key: &str| v[key].as_array().map_or(0, Vec::len);
        let retired: usize = undone.iter().map(|r| length(r, "retired")).sum();
        let kept: usize = undone.iter().map(|r| length(r, "kept")).sum();
        let _ = writeln!(
            out,
            "  session turns rolled back {}: {retired} facts retired, {kept} kept (also formed from other turns, or changed since)",
            undone.len()
        );
        if forming > 0 {
            let _ = writeln!(
                out,
                "  {forming} session turns are still forming: roll back again once the daemon is done with them"
            );
        }
    }
    out
}

/// Proposals per status and per kind; session turns are reported apart.
fn proposal_counts(proposals: &[ImportProposal]) -> String {
    let mut statuses: BTreeMap<String, usize> = BTreeMap::new();
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    let proposals: Vec<&ImportProposal> =
        proposals.iter().filter(|p| !is_session_turn(p)).collect();
    for proposal in &proposals {
        *statuses.entry(proposal.status.clone()).or_insert(0) += 1;
        *kinds.entry(proposal.item_kind.clone()).or_insert(0) += 1;
    }
    format!(
        "  {} proposals by status: {}; by kind: {}",
        proposals.len(),
        tally(&statuses),
        tally(&kinds)
    )
}

/// `text` on one line, cut to `max` characters.
fn one_line(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.chars().count() > max {
        true => flat.chars().take(max).collect::<String>() + "…",
        false => flat,
    }
}

/// `show`: write the review file of a run and summarize its proposals. An existing review file may hold
/// a person's decisions, so it is replaced only with `force`.
pub async fn show(
    run_id: &str,
    out: Option<&Path>,
    force: bool,
    daemon: &DaemonArgs,
    home: &Path,
) -> Result<String, Error> {
    let path = match out {
        Some(path) => path.to_path_buf(),
        None => review::default_path(home, run_id),
    };
    if !force && std::fs::symlink_metadata(&path).is_ok() {
        return Err(invalid(format!(
            "{} exists and may hold your decisions; pass --force to replace it, or another --out",
            shown(&path)
        )));
    }
    let client = Client::new(&daemon.url, CALL_TIMEOUT)?;
    let view = client.run(run_id).await?;
    review::write(&path, &view.run, &view.proposals)?;
    let mut text = run_report(&view.run);
    let _ = writeln!(text, "{}", proposal_counts(&view.proposals));
    let reviewed: Vec<&ImportProposal> = view
        .proposals
        .iter()
        .filter(|p| !is_session_turn(p))
        .collect();
    for proposal in reviewed.iter().take(SHOW_PROPOSALS) {
        let line = review::ProposalLine::new(proposal);
        let hint = match line.item_kind == review::INSTRUCTION {
            true if line.scope_hint.is_empty() => " (no scope hint)".to_string(),
            true => format!(" (scope hint: {})", line.scope_hint),
            false => String::new(),
        };
        let _ = writeln!(
            text,
            "  [{}] {} {}{hint}: {}",
            line.decision,
            line.item_kind,
            line.proposal_id,
            one_line(&line.text, 100)
        );
    }
    if let Some(more) = reviewed
        .len()
        .checked_sub(SHOW_PROPOSALS)
        .filter(|n| *n > 0)
    {
        let _ = writeln!(text, "  … and {more} more in the review file");
    }
    text.push_str(&previous_report(
        &view.run,
        "the review file lists them as \"previous\" lines",
    ));
    let file = shown(&path);
    let _ = writeln!(text, "Review file: {file}");
    let _ = writeln!(
        text,
        "Next: set each \"decision\" to \"accept\" or \"reject\" (a deleted line is declined), and the \"scope\" of an instruction to \"global\" to follow it in every project, then run: agent-memoryd import apply {} --review {file}{}",
        view.run.run_id,
        daemon_flags(daemon)
    );
    Ok(text)
}

/// How fast `apply` queues session turns (design §6.14): at most `per_minute`, each only while the outbox
/// holds fewer than `max_pending` turns not yet formed, so newly captured turns are not held up behind an
/// import. `per_minute` 0 queues every turn at once.
#[derive(Debug, Clone)]
pub struct Pace {
    pub per_minute: f64,
    pub max_pending: usize,
    /// How often a wait looks at the outbox again, and after how long it says why it waits.
    pub poll: Duration,
    pub note_after: Duration,
}

impl Pace {
    pub fn new(per_minute: f64, max_pending: usize) -> Self {
        Pace {
            per_minute,
            max_pending,
            poll: Duration::from_secs(2),
            note_after: Duration::from_secs(30),
        }
    }
}

/// What queueing a run's session turns found.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Queue {
    pub added: usize,
    /// Queued by an earlier, interrupted apply of the run.
    pub again: usize,
    /// Held by a job of live capture, which the daemon deduplicates the turn against.
    pub captured: usize,
    /// Held by an earlier import run's job.
    pub earlier: usize,
}

/// Queue the run's applied session turns that no request answers yet into the dialogue outbox at `path`, in the
/// run's order (oldest first in each session), as the run's host, each naming the run. A turn the outbox already
/// holds is never replaced. Progress goes to `say`.
pub async fn queue_turns(
    view: &RunView,
    path: &Path,
    pace: &Pace,
    say: &mut (dyn FnMut(&str) + Send),
) -> Result<Queue, Error> {
    let run = &view.run;
    let turns: Vec<&ImportProposal> = view
        .proposals
        .iter()
        .filter(|p| is_session_turn(p) && p.status == APPLIED)
        .filter(|p| {
            p.applied
                .as_ref()
                .is_none_or(|a| a.get("requestId").is_none())
        })
        .collect();
    let mut queue = Queue::default();
    if turns.is_empty() {
        return Ok(queue);
    }
    let mut outbox = Outbox::open(path)?;
    let interval = match pace.per_minute > 0.0 {
        true => Some(Duration::from_secs_f64(60.0 / pace.per_minute)),
        false => None,
    };
    let mut last_added: Option<Instant> = None;
    for (i, p) in turns.iter().enumerate() {
        let Some(turn) = &p.turn else { continue };
        if let Some(interval) = interval {
            let waiting = Instant::now();
            let mut noted = false;
            loop {
                let backlog = outbox.backlog()?;
                if backlog < pace.max_pending {
                    break;
                }
                if !noted && waiting.elapsed() >= pace.note_after {
                    say(&format!(
                        "  waiting: {backlog} turns in {} are not formed yet; the daemon uploads them when started with --outbox on that file, or run `agent-memoryd outbox drain`",
                        shown(path)
                    ));
                    noted = true;
                }
                tokio::time::sleep(pace.poll).await;
            }
            if let Some(last) = last_added {
                tokio::time::sleep(interval.saturating_sub(last.elapsed())).await;
            }
        }
        let body = json!({"scope": {"project": run.project, "session": turn.session},
                          "turnId": turn.turn_id, "messages": turn.messages, "importRunId": run.run_id});
        match outbox.enqueue_import(&run.host, &body)? {
            Queued::Added => {
                queue.added += 1;
                last_added = Some(Instant::now());
            }
            Queued::Again => queue.again += 1,
            Queued::Held { run: None, .. } => queue.captured += 1,
            Queued::Held { run: Some(_), .. } => queue.earlier += 1,
        }
        if (i + 1) % PROGRESS_TURNS == 0 && i + 1 < turns.len() {
            say(&format!(
                "  queued {} of {} session turns",
                i + 1,
                turns.len()
            ));
        }
    }
    Ok(queue)
}

/// `apply`: with a review file its decisions, without one the defaults; the proposals whose source file
/// (under `home`) changed since propose are declined either way. Then the run's session turns go into the
/// dialogue outbox at `outbox`, paced by `pace`. A run already applied only has its remaining turns queued, so
/// an interrupted apply resumes.
pub async fn apply(
    run_id: &str,
    review: Option<&Path>,
    daemon: &DaemonArgs,
    home: &Path,
    outbox: &Path,
    pace: &Pace,
    say: &mut (dyn FnMut(&str) + Send),
) -> Result<String, Error> {
    let client = Client::new(&daemon.url, CALL_TIMEOUT)?;
    let current = client.run(run_id).await?;
    let resume = current.run.status == RUN_APPLIED && current.run.counts.session_turns > 0;
    let applied = match resume {
        true => client::Applied {
            view: current,
            changed: vec![],
        },
        false => client.apply(run_id, review, home).await?,
    };
    let queue = queue_turns(&applied.view, outbox, pace, say).await?;
    let mut out = run_report(&applied.view.run);
    for source in &applied.changed {
        let _ = writeln!(
            out,
            "  {} {} since propose: its {} proposals were declined",
            source.path,
            source.why,
            source.proposals.len()
        );
    }
    if !applied.changed.is_empty() {
        let _ = writeln!(
            out,
            "  make a new plan and propose again to import the current text"
        );
    }
    if review.is_none() && applied.view.run.counts.proposals > 0 {
        let _ = writeln!(
            out,
            "  default decisions: facts and preferences accepted, instruction candidates and duplicates not written"
        );
    }
    out.push_str(&turns_report(&applied.view));
    if queue != Queue::default() {
        let _ = writeln!(
            out,
            "  session turns queued into {}: {} new, {} already queued by this run, {} already captured live, {} queued by an earlier import",
            shown(outbox),
            queue.added,
            queue.again,
            queue.captured,
            queue.earlier
        );
        let _ = writeln!(
            out,
            "  the uploader submits them and the daemon forms each turn it does not have yet; follow with: agent-memoryd import status {run_id}{}",
            daemon_flags(daemon)
        );
    }
    Ok(out)
}

/// `status`: one run, or the newest runs.
pub async fn status(
    run_id: Option<&str>,
    project: Option<&str>,
    daemon: &DaemonArgs,
) -> Result<String, Error> {
    let client = Client::new(&daemon.url, CALL_TIMEOUT)?;
    if let Some(run_id) = run_id {
        let view = client.run(run_id).await?;
        let mut out = run_report(&view.run);
        if view.run.counts.proposals > 0 {
            let _ = writeln!(out, "{}", proposal_counts(&view.proposals));
        }
        out.push_str(&turns_report(&view));
        out.push_str(&previous_report(
            &view.run,
            &format!("agent-memoryd import show {run_id} lists them"),
        ));
        return Ok(out);
    }
    let runs = client.runs(project, STATUS_RUNS).await?;
    if runs.is_empty() {
        return Ok("No import runs\n".into());
    }
    let mut out = String::new();
    for run in runs {
        let c = &run.counts;
        let _ = writeln!(
            out,
            "{}  {}  {} into {:?}  created {}  units {}, proposals {}, applied {}",
            run.run_id,
            run.status,
            run.host,
            run.project,
            run.created_at,
            c.units,
            c.proposals,
            c.applied
        );
    }
    Ok(out)
}

/// `rollback`: first the run's session turns still waiting in the dialogue outbox at `outbox` are cancelled, so
/// no uploader submits them (the daemon would refuse them anyway once the run rolls back), then the daemon
/// undoes the run.
pub async fn rollback(run_id: &str, daemon: &DaemonArgs, outbox: &Path) -> Result<String, Error> {
    let client = Client::new(&daemon.url, CALL_TIMEOUT)?;
    let current = client.run(run_id).await?;
    let cancelled =
        match current.run.counts.session_turns > 0 && std::fs::symlink_metadata(outbox).is_ok() {
            true => Outbox::open(outbox)?.cancel_import(run_id)?,
            false => 0,
        };
    let view = client.rollback(run_id).await?;
    let mut out = run_report(&view.run);
    if cancelled > 0 {
        let _ = writeln!(
            out,
            "  {cancelled} session turns not yet submitted were cancelled in {}",
            shown(outbox)
        );
    }
    out.push_str(&turns_report(&view));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::import::client::tests::{
        plan_over, sourced_proposals, stub, stub_proposals, stub_run, stub_with,
    };
    use crate::harness::import::message;
    use crate::harness::import::review::tests::{changed_unit, turn_row};
    use crate::harness::import::session::{SessionDoc, Turn};
    use memory_domain::import::RUN_PROPOSED;

    fn plan_args(home: &Path, host: Host) -> PlanArgs {
        PlanArgs {
            host,
            project: "my-project".into(),
            workspaces: vec![],
            all: false,
            from: None,
            label: None,
            no_extract: false,
            recall_budget: 400,
            recall_top_k: 5,
            home: Some(home.to_path_buf()),
            claude_config_dir: None,
            codex_home: None,
            workbuddy_home: None,
            out: None,
            include: vec![Include::Notes],
            since: None,
            limit: 200,
        }
    }

    /// No pacing: every turn is queued at once.
    fn at_once() -> Pace {
        Pace::new(0.0, 4)
    }

    #[test]
    fn plan_writes_a_plan_file_and_reports_it() {
        let home = tempfile::tempdir().unwrap();
        let workspace = home.path().join("code/my-app");
        std::fs::create_dir_all(&workspace).unwrap();
        let summaries = home.path().join(".codex/memories/rollout_summaries");
        std::fs::create_dir_all(&summaries).unwrap();
        std::fs::write(home.path().join(".codex/memories/MEMORY.md"), "# Derived\n").unwrap();
        let header = |cwd: &Path| {
            format!(
                "thread_id: t-1\nupdated_at: 2026-01-02T03:04:05Z\ncwd: {}\n\n",
                cwd.display()
            )
        };
        std::fs::write(
            summaries.join("a.md"),
            header(&workspace)
                + "# Build\n## Task 1: Cache\n- The cache key ignored the lockfile.\n",
        )
        .unwrap();
        let mut args = plan_args(home.path(), Host::Codex);
        args.workspaces = vec![workspace.clone()];
        let now = DateTime::parse_from_rfc3339("2026-10-22T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let (path, plan) = make_plan(&args, now).unwrap();
        assert_eq!(
            path,
            home.path()
                .join(plan::IMPORTS_DIR)
                .join("codex-my-project-20261022T120000Z.plan.jsonl")
        );
        assert_eq!(plan.summary.created_at, "2026-10-22T12:00:00Z");
        assert_eq!(plan.summary.workspaces, ["~/code/my-app"]);
        assert_eq!(
            (plan.summary.counts.docs, plan.summary.counts.extract),
            (1, 1)
        );
        let report = plan_report(&path, &plan, &args);
        for part in [
            "Plan for codex into project \"my-project\": 1 documents, 1 units",
            "1 extract",
            "derived (1): ~/.codex/memories/MEMORY.md",
            "1 generation calls",
            // The extract unit's text is embedded once for its context; its items are not known yet.
            "1 texts with",
            "to embed, plus two per extracted item",
            "unit cap: cost 200 (extraction aims at 80)",
            "agent-memoryd import propose ",
            // The plan's `~/` paths are under --home, so propose needs it too.
            " --home ",
        ] {
            assert!(report.contains(part), "{part}\n{report}");
        }

        // Flags of other hosts and missing workspaces are refused before anything is read.
        let mut wrong = plan_args(home.path(), Host::Codex);
        wrong.from = Some(PathBuf::from("x.jsonl"));
        assert!(message(&make_plan(&wrong, now).unwrap_err()).contains("--host jsonl"));
        let mut wrong = plan_args(home.path(), Host::Codex);
        wrong.all = true;
        assert!(message(&make_plan(&wrong, now).unwrap_err()).contains("--all"));
        let mut wrong = plan_args(home.path(), Host::Codex);
        wrong.workspaces = vec![home.path().join("missing")];
        assert!(message(&make_plan(&wrong, now).unwrap_err()).contains("not a directory"));
    }

    #[test]
    fn issues_group_by_reason_with_their_details() {
        let issue = |path: &str, reason: &str| Issue {
            path: path.into(),
            reason: reason.into(),
        };
        let groups = issue_groups(&[
            issue("~/a.md", "symlink"),
            issue("~/b.md", "unreadable: permission denied"),
            issue("~/e.jsonl", "invalid-line 3: no text"),
            issue("~/c.md", "symlink"),
        ]);
        assert_eq!(groups["symlink"], ["~/a.md", "~/c.md"]);
        assert_eq!(groups["unreadable"], ["~/b.md (permission denied)"]);
        assert_eq!(groups["invalid-line"], ["~/e.jsonl (line 3: no text)"]);
        assert_eq!(one_line("a\n  b\tc", 10), "a b c");
        assert_eq!(one_line("abcdef", 3), "abc…");
    }

    #[tokio::test]
    async fn propose_show_and_apply_through_the_commands() {
        let home = tempfile::tempdir().unwrap();
        let plan = plan_over(home.path());
        let path = home.path().join("p.plan.jsonl");
        plan::write(&plan, &path).unwrap();
        let (url, requests) = stub().await;
        let daemon = DaemonArgs { url: url.clone() };
        let mut said = Vec::new();
        let report = propose(
            &path,
            &daemon,
            8,
            Duration::from_secs(5),
            home.path(),
            true,
            &mut |line| said.push(line.to_string()),
        )
        .await
        .unwrap();
        assert_eq!(said.len(), 2, "{said:?}");
        assert!(said[1].contains("batch 1/1 sent (run ir1)"));
        assert!(report.starts_with("Run ir1: proposed (codex into project \"my-project\""));
        assert!(report.contains(&format!("agent-memoryd import show ir1 --url {url}")));
        // The plan's home is not $HOME: apply needs it to read the sources again.
        assert!(
            report.contains(&format!(
                "agent-memoryd import apply ir1 --review <review file> --url {url} --home {}",
                shown(home.path())
            )),
            "{report}"
        );
        assert_eq!(requests.lock().unwrap()[0].2["reconsider"], true);

        let review_path = home.path().join("ir1.review.jsonl");
        let text = show("ir1", Some(&review_path), false, &daemon, home.path())
            .await
            .unwrap();
        assert!(
            text.contains("4 proposals by status: proposed 3, rejected 1"),
            "{text}"
        );
        assert!(
            text.contains("[reject] instruction ip2 (scope hint: project): Proposal ip2 text."),
            "{text}"
        );
        assert!(text.contains("[accept] fact ip1: Proposal ip1 text."));
        assert!(text.contains("\"scope\" of an instruction to \"global\""));
        assert!(!text.contains("earlier records"), "{text}");
        let outbox = home.path().join("outbox.sqlite3");
        let applied = apply(
            "ir1",
            Some(&review_path),
            &daemon,
            home.path(),
            &outbox,
            &at_once(),
            &mut |_| {},
        )
        .await
        .unwrap();
        assert!(applied.contains("applied 2"), "{applied}");
        assert!(!applied.contains("since propose"), "{applied}");
        let last = requests.lock().unwrap().last().unwrap().2.clone();
        assert_eq!(last["decisions"]["ip2"], "reject");
        assert!(status(None, Some("my-project"), &daemon)
            .await
            .unwrap()
            .starts_with("ir1  proposed"));
        assert!(rollback("ir1", &daemon, &outbox)
            .await
            .unwrap()
            .starts_with("Run ir1"));
        // A run without session turns opens no outbox.
        assert!(std::fs::symlink_metadata(&outbox).is_err());

        // A plan whose sources all changed proposes nothing.
        for name in ["a.md", "b.md", "c.md"] {
            std::fs::write(home.path().join(name), "changed\n").unwrap();
        }
        let error = propose(
            &path,
            &daemon,
            8,
            Duration::from_secs(5),
            home.path(),
            false,
            &mut |_| {},
        )
        .await
        .unwrap_err();
        assert!(
            message(&error).contains("all 6 units changed"),
            "{}",
            message(&error)
        );
    }

    #[tokio::test]
    async fn show_keeps_an_edited_review_unless_forced() {
        let home = tempfile::tempdir().unwrap();
        let (url, requests) = stub().await;
        let daemon = DaemonArgs { url };
        // The default file, under the home directory.
        let default = review::default_path(home.path(), "ir1");
        show("ir1", None, false, &daemon, home.path())
            .await
            .unwrap();
        let written = std::fs::read_to_string(&default).unwrap();
        let edited = written.replacen("\"decision\":\"accept\"", "\"decision\":\"reject\"", 1);
        std::fs::write(&default, &edited).unwrap();
        let error = message(
            &show("ir1", None, false, &daemon, home.path())
                .await
                .unwrap_err(),
        );
        assert!(
            error.contains("ir1.review.jsonl exists")
                && error.contains("--force")
                && error.contains("--out")
                && !error.contains('\n'),
            "{error}"
        );
        assert_eq!(std::fs::read_to_string(&default).unwrap(), edited);
        // Refused before the daemon is asked.
        assert_eq!(requests.lock().unwrap().len(), 1);
        show("ir1", None, true, &daemon, home.path()).await.unwrap();
        assert_eq!(std::fs::read_to_string(&default).unwrap(), written);
        // A link at the path counts as an existing file.
        #[cfg(unix)]
        {
            let link = home.path().join("link.review.jsonl");
            std::os::unix::fs::symlink(home.path().join("missing"), &link).unwrap();
            assert!(show("ir1", Some(&link), false, &daemon, home.path())
                .await
                .is_err());
        }
    }

    #[tokio::test]
    async fn show_and_status_count_the_earlier_records_of_changed_units() {
        let home = tempfile::tempdir().unwrap();
        let mut run = stub_run();
        run["units"] = serde_json::to_value(vec![
            changed_unit(
                "codex:t-1#task-1",
                0,
                &[("m1", "Old fact one."), ("m2", "Old fact two.")],
            ),
            changed_unit("codex:t-2#task-1", 1, &[("m3", "Old fact three.")]),
        ])
        .unwrap();
        let (url, _) = stub_with(run, stub_proposals()).await;
        let daemon = DaemonArgs { url };
        let path = home.path().join("ir1.review.jsonl");
        let text = show("ir1", Some(&path), false, &daemon, home.path())
            .await
            .unwrap();
        for part in [
            "earlier records: 3 of 2 changed units stay current unless a proposal of their unit supersedes them",
            "the review file lists them as \"previous\" lines",
            "correct_memory without text, or delete_memory",
        ] {
            assert!(text.contains(part), "{part}\n{text}");
        }
        let lines: Vec<Value> = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let previous: Vec<&str> = lines
            .iter()
            .filter(|l| l["type"] == "previous")
            .map(|l| l["memoryId"].as_str().unwrap())
            .collect();
        assert_eq!(previous, ["m1", "m2", "m3"]);
        let status = status(Some("ir1"), None, &daemon).await.unwrap();
        for part in [
            "earlier records: 3 of 2 changed units",
            "agent-memoryd import show ir1 lists them",
            "delete_memory",
        ] {
            assert!(status.contains(part), "{part}\n{status}");
        }
    }

    #[tokio::test]
    async fn apply_reports_the_sources_changed_since_propose() {
        let home = tempfile::tempdir().unwrap();
        let (url, _) = stub_with(stub_run(), sourced_proposals(home.path())).await;
        let daemon = DaemonArgs { url };
        std::fs::write(home.path().join("b.md"), "Edited.\n").unwrap();
        std::fs::remove_file(home.path().join("c.md")).unwrap();
        let outbox = home.path().join("outbox.sqlite3");
        let report = apply(
            "ir1",
            None,
            &daemon,
            home.path(),
            &outbox,
            &at_once(),
            &mut |_| {},
        )
        .await
        .unwrap();
        for part in [
            "~/b.md changed since propose: its 2 proposals were declined",
            "~/c.md vanished since propose: its 1 proposals were declined",
            "propose again to import the current text",
            "default decisions",
        ] {
            assert!(report.contains(part), "{part}\n{report}");
        }
        assert_eq!(report.matches("since propose").count(), 2, "{report}");
    }

    /// Run `ir1` as a session import in `status`: turn t1 already submitted, t2 and t3 queued.
    fn session_run(status: &str) -> (Value, Vec<ImportProposal>) {
        let mut run = stub_run();
        run["status"] = json!(status);
        run["counts"]["proposals"] = json!(0);
        run["counts"]["sessionTurns"] = json!(3);
        run["counts"]["turnsQueued"] = json!(3);
        let mut t1 = turn_row("ip1", "t1", APPLIED);
        t1.applied = Some(json!({"queued": true, "requestId": "rq1", "duplicate": false}));
        let mut t2 = turn_row("ip2", "t2", APPLIED);
        t2.applied = Some(json!({"queued": true}));
        let mut t3 = turn_row("ip3", "t3", APPLIED);
        t3.applied = Some(json!({"queued": true}));
        (run, vec![t1, t2, t3])
    }

    /// (turn id, platform, status, the body's importRunId) of every job of the outbox at `path`.
    fn jobs(path: &Path) -> Vec<(String, String, String, Value)> {
        let db = rusqlite::Connection::open(path).unwrap();
        let mut statement = db
            .prepare("SELECT turn_id, platform, status, body FROM jobs ORDER BY seq")
            .unwrap();
        statement
            .query_map([], |row| {
                let body: String = row.get(3)?;
                let body: Value = serde_json::from_str(&body).unwrap();
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    body["importRunId"].clone(),
                ))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    #[test]
    fn plan_reads_session_transcripts_up_to_the_limit() {
        use crate::harness::import::codex_session::tests::{complete, meta, user_message, Fixture};
        let f = Fixture::new();
        let mut lines = vec![meta(
            "thr-1",
            "2026-04-01T09:00:00Z",
            &f.workspace,
            Some("user"),
        )];
        for (i, turn) in ["t1", "t2", "t3"].iter().enumerate() {
            let at = format!("2026-04-01T09:0{i}:00Z");
            lines.push(user_message(&at, turn, &format!("Question {turn}?")));
            lines.push(complete(&at, turn, Some(&format!("Answer {turn}."))));
        }
        f.rollout("sessions/2026/04/01/rollout-thr-1.jsonl", &lines);
        f.rollout(
            "sessions/2026/04/01/rollout-sub.jsonl",
            &[meta(
                "sub",
                "2026-04-01T09:00:00Z",
                &f.workspace,
                Some("subagent"),
            )],
        );
        let mut args = plan_args(f.home.path(), Host::Codex);
        args.workspaces = vec![f.workspace.clone()];
        args.include = vec![Include::Sessions];
        args.limit = 2;
        let now = Utc::now();
        let (path, plan) = make_plan(&args, now).unwrap();
        let turns: Vec<&str> = plan
            .import_units()
            .map(|u| u.turn.as_ref().unwrap().native_id.as_str())
            .collect();
        assert_eq!(turns, ["t1", "t2"]);
        assert_eq!(plan.summary.host, "codex");
        assert_eq!(
            plan.summary
                .sessions
                .as_ref()
                .unwrap()
                .next_since
                .as_deref(),
            Some("2026-04-01T09:02:00Z")
        );
        let report = plan_report(&path, &plan, &args);
        for part in [
            "to import: 0 verbatim, 0 split, 0 extract, 2 session turns",
            "sessions: 1 read; turns oldest first, at most 2",
            "turns left out: 1 over --limit",
            "next batch: plan again with --since 2026-04-01T09:02:00Z",
            "thread_source:subagent (1): ~/.codex/sessions/2026/04/01/rollout-sub.jsonl",
            "2 generation calls",
            "session turns go through Mem0 formation",
        ] {
            assert!(report.contains(part), "{part}\n{report}");
        }
        // Notes and sessions together: the Codex memories directory is then needed too.
        args.include = vec![Include::Notes, Include::Sessions];
        assert!(message(&make_plan(&args, now).unwrap_err()).contains("memories"));
        // Only hosts whose turns capture files by transcript ids have sessions to import.
        let mut wrong = plan_args(f.home.path(), Host::Workbuddy);
        wrong.include = vec![Include::Sessions];
        assert!(message(&make_plan(&wrong, now).unwrap_err()).contains("--include sessions"));
        let mut wrong = plan_args(f.home.path(), Host::Codex);
        wrong.workspaces = vec![f.workspace.clone()];
        wrong.since = Some("2026-01-01T00:00:00Z".into());
        assert!(message(&make_plan(&wrong, now).unwrap_err()).contains("--since"));
    }

    #[tokio::test]
    async fn a_sessions_only_run_is_applied_without_a_review() {
        let home = tempfile::tempdir().unwrap();
        let mut opts = PlanOptions {
            host: "codex".into(),
            project: "my-project".into(),
            home: home.path().to_path_buf(),
            workspaces: vec![],
            limits: ImportLimits::default(),
            no_extract: false,
            created_at: "2026-10-22T12:00:00Z".into(),
            sessions: Some(SessionOptions {
                since: None,
                limit: 10,
            }),
        };
        opts.workspaces.clear();
        let scan = ScanOutput {
            sessions: vec![SessionDoc {
                host: "codex".into(),
                via: "codex-rollout",
                path: home.path().join("rollout-s1.jsonl"),
                display_path: "~/rollout-s1.jsonl".into(),
                session: "s1".into(),
                cwd: home.path().to_path_buf(),
                file_hash: "f".into(),
                started_at: "2026-01-02T03:04:05Z".into(),
                turns: vec![Turn {
                    native_id: "t1".into(),
                    prompt: "Prompt t1.".into(),
                    reply: "Reply t1.".into(),
                    prompt_at: "2026-01-02T03:04:05Z".into(),
                    reply_at: "2026-01-02T03:04:06Z".into(),
                    skip: None,
                }],
                extra: Value::Null,
            }],
            ..ScanOutput::default()
        };
        let plan = plan::build(scan, &opts).unwrap();
        let path = home.path().join("s.plan.jsonl");
        plan::write(&plan, &path).unwrap();
        let (mut run, _) = session_run(RUN_PROPOSED);
        run["counts"]["turnsQueued"] = json!(0);
        let (url, _) = stub_with(run, vec![turn_row("ip1", "t1", "proposed")]).await;
        let daemon = DaemonArgs { url };
        let report = propose(
            &path,
            &daemon,
            8,
            Duration::from_secs(5),
            home.path(),
            false,
            &mut |_| {},
        )
        .await
        .unwrap();
        assert!(
            report.contains("Next: agent-memoryd import apply ir1 --yes"),
            "{report}"
        );
        assert!(!report.contains("import show"), "{report}");
    }

    #[tokio::test]
    async fn apply_queues_the_runs_turns_and_resumes_an_applied_run() {
        let home = tempfile::tempdir().unwrap();
        let (run, rows) = session_run(RUN_APPLIED);
        let (url, requests) = stub_with(run, rows).await;
        let daemon = DaemonArgs { url };
        let outbox = home.path().join("outbox.sqlite3");
        let report = apply(
            "ir1",
            None,
            &daemon,
            home.path(),
            &outbox,
            &at_once(),
            &mut |_| {},
        )
        .await
        .unwrap();
        for part in [
            "session turns 3, registered without a model call: 3 queued",
            "session turns submitted: 1 new, 0 the daemon already had",
            "2 not submitted yet",
            "session turns queued into",
            "2 new, 0 already queued by this run, 0 already captured live, 0 queued by an earlier import",
            "agent-memoryd import status ir1 --url ",
        ] {
            assert!(report.contains(part), "{part}\n{report}");
        }
        // The run was applied already: only its turns were queued, t1 not, as its request answered it.
        let names: Vec<String> = requests
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.0.clone())
            .collect();
        assert_eq!(names, ["import_run"]);
        let queued = jobs(&outbox);
        assert_eq!(
            queued,
            [
                (
                    "my-project/s1/t2".to_string(),
                    "codex".to_string(),
                    "pending".to_string(),
                    json!("ir1")
                ),
                (
                    "my-project/s1/t3".into(),
                    "codex".into(),
                    "pending".into(),
                    json!("ir1")
                ),
            ]
        );
        let again = apply(
            "ir1",
            None,
            &daemon,
            home.path(),
            &outbox,
            &at_once(),
            &mut |_| {},
        )
        .await
        .unwrap();
        assert!(
            again.contains("0 new, 2 already queued by this run"),
            "{again}"
        );
        assert_eq!(jobs(&outbox).len(), 2);
        let status = status(Some("ir1"), None, &daemon).await.unwrap();
        assert!(
            status.contains("session turns submitted: 1 new"),
            "{status}"
        );
        assert!(!status.contains("proposals by status"), "{status}");
    }

    #[tokio::test]
    async fn rollback_cancels_the_runs_turns_still_in_the_outbox() {
        let home = tempfile::tempdir().unwrap();
        let (run, rows) = session_run(RUN_APPLIED);
        let outbox = home.path().join("outbox.sqlite3");
        let view = RunView {
            run: serde_json::from_value(run.clone()).unwrap(),
            proposals: rows.clone(),
        };
        queue_turns(&view, &outbox, &at_once(), &mut |_| {})
            .await
            .unwrap();
        let mut other = view.clone();
        other.run.run_id = "ir2".into();
        for p in &mut other.proposals {
            p.turn.as_mut().unwrap().turn_id += "-b";
        }
        queue_turns(&other, &outbox, &at_once(), &mut |_| {})
            .await
            .unwrap();
        let (url, requests) = stub_with(run, rows).await;
        let daemon = DaemonArgs { url };
        let report = rollback("ir1", &daemon, &outbox).await.unwrap();
        assert!(
            report.contains("2 session turns not yet submitted were cancelled"),
            "{report}"
        );
        let statuses: Vec<(String, Value)> =
            jobs(&outbox).into_iter().map(|j| (j.2, j.3)).collect();
        assert_eq!(
            statuses,
            [
                ("cancelled".to_string(), json!("ir1")),
                ("cancelled".into(), json!("ir1")),
                ("pending".into(), json!("ir2")),
                ("pending".into(), json!("ir2")),
            ]
        );
        assert_eq!(
            requests.lock().unwrap().last().unwrap().0,
            "import_rollback"
        );
    }

    #[tokio::test]
    async fn turns_wait_while_the_outbox_holds_turns_not_yet_formed() {
        let home = tempfile::tempdir().unwrap();
        let (run, mut rows) = session_run(RUN_APPLIED);
        rows[0].applied = Some(json!({"queued": true}));
        let view = RunView {
            run: serde_json::from_value(run).unwrap(),
            proposals: rows,
        };
        let outbox = home.path().join("outbox.sqlite3");
        let pace = Pace {
            per_minute: 60_000.0,
            max_pending: 2,
            poll: Duration::from_millis(5),
            note_after: Duration::ZERO,
        };
        // An uploader that forms the first two turns once the wait for them is announced.
        let (say, heard) = std::sync::mpsc::channel::<String>();
        let path = outbox.clone();
        let uploader = std::thread::spawn(move || {
            let said = heard.recv().unwrap();
            let db = rusqlite::Connection::open(path).unwrap();
            db.busy_timeout(Duration::from_secs(2)).unwrap();
            db.execute("UPDATE jobs SET status='completed'", [])
                .unwrap();
            said
        });
        let queue = queue_turns(&view, &outbox, &pace, &mut |line| {
            say.send(line.to_string()).unwrap()
        })
        .await
        .unwrap();
        let said = uploader.join().unwrap();
        assert_eq!(queue.added, 3);
        assert!(said.contains("waiting: 2 turns in") && said.contains("outbox drain"));
        // The third turn was queued only after the first two were formed.
        let order: Vec<(String, String)> = jobs(&outbox).into_iter().map(|j| (j.0, j.2)).collect();
        assert_eq!(
            order,
            [
                ("my-project/s1/t1".into(), "completed".into()),
                ("my-project/s1/t2".into(), "completed".into()),
                ("my-project/s1/t3".into(), "pending".into())
            ]
        );
    }
}
