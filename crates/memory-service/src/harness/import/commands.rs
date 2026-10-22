//! The work of the `agent-memoryd import` commands and the reports they print: `make_plan` scans a host
//! and writes the plan, `propose`, `show`, `apply`, `status` and `rollback` call the daemon through
//! `client`. Reports are plain text for a person; the files they name hold the full detail.
use chrono::{DateTime, Utc};
use memory_domain::import::{ImportLimits, ImportProposal, ImportRun, UNIT_FAILED};
use memory_rules::util::fmt_ts;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::client::{self, Client};
use super::plan::{self, Plan, PlanOptions};
use super::source::{Issue, ScanContext};
use super::{project, review, shown, DaemonArgs, Host, PlanArgs, DEFAULT_TENANT, DEFAULT_URL};
use crate::harness::{expand, home, invalid, Error};

/// Seconds per daemon request of every command but `propose`.
const CALL_TIMEOUT: Duration = Duration::from_secs(120);
/// Runs `status` lists without a run.
const STATUS_RUNS: usize = 10;
/// Proposals `show` prints.
const SHOW_PROPOSALS: usize = 10;
/// Paths the plan summary prints per issue reason.
const ISSUE_PATHS: usize = 5;

/// Scan the host, plan its units and write the plan file; returns where.
pub fn make_plan(args: &PlanArgs, now: DateTime<Utc>) -> Result<(PathBuf, Plan), Error> {
    if args.host != Host::Jsonl && (args.from.is_some() || args.label.is_some()) {
        return Err(invalid("--from and --label are for --host jsonl"));
    }
    if args.all && args.host != Host::ClaudeCode {
        return Err(invalid("--all is for --host claude-code"));
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
    let scan = args.host.adapter().scan(&ctx)?;
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
    let _ = writeln!(
        out,
        "  to import: {} verbatim, {} split, {} extract",
        c.verbatim, c.split, c.extract
    );
    if c.skipped > 0 {
        let _ = writeln!(out, "  skipped: {} ({})", c.skipped, tally(&s.skip_reasons));
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

/// ` --url U --tenant T` for the commands a report suggests, when they are not the defaults.
fn daemon_flags(daemon: &DaemonArgs) -> String {
    let mut flags = String::new();
    if daemon.url != DEFAULT_URL {
        let _ = write!(flags, " --url {}", daemon.url);
    }
    if daemon.tenant != DEFAULT_TENANT {
        let _ = write!(flags, " --tenant {}", daemon.tenant);
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
    let client = Client::new(&daemon.url, &daemon.tenant, timeout)?;
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

/// Proposals per status and per kind.
fn proposal_counts(proposals: &[ImportProposal]) -> String {
    let mut statuses: BTreeMap<String, usize> = BTreeMap::new();
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    for proposal in proposals {
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
    let client = Client::new(&daemon.url, &daemon.tenant, CALL_TIMEOUT)?;
    let view = client.run(run_id).await?;
    review::write(&path, &view.run, &view.proposals)?;
    let mut text = run_report(&view.run);
    let _ = writeln!(text, "{}", proposal_counts(&view.proposals));
    for proposal in view.proposals.iter().take(SHOW_PROPOSALS) {
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
    if let Some(more) = view
        .proposals
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

/// `apply`: with a review file its decisions, without one the defaults; the proposals whose source file
/// (under `home`) changed since propose are declined either way.
pub async fn apply(
    run_id: &str,
    review: Option<&Path>,
    daemon: &DaemonArgs,
    home: &Path,
) -> Result<String, Error> {
    let client = Client::new(&daemon.url, &daemon.tenant, CALL_TIMEOUT)?;
    let applied = client.apply(run_id, review, home).await?;
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
    if review.is_none() {
        let _ = writeln!(
            out,
            "  default decisions: facts and preferences accepted, instruction candidates and duplicates not written"
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
    let client = Client::new(&daemon.url, &daemon.tenant, CALL_TIMEOUT)?;
    if let Some(run_id) = run_id {
        let view = client.run(run_id).await?;
        let mut out = run_report(&view.run);
        if !view.proposals.is_empty() {
            let _ = writeln!(out, "{}", proposal_counts(&view.proposals));
        }
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

/// `rollback`.
pub async fn rollback(run_id: &str, daemon: &DaemonArgs) -> Result<String, Error> {
    let client = Client::new(&daemon.url, &daemon.tenant, CALL_TIMEOUT)?;
    Ok(run_report(&client.rollback(run_id).await?.run))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::import::client::tests::{
        plan_over, sourced_proposals, stub, stub_proposals, stub_run, stub_with,
    };
    use crate::harness::import::message;
    use crate::harness::import::review::tests::changed_unit;
    use serde_json::Value;

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
        }
    }

    #[test]
    fn plan_writes_a_private_plan_file_and_reports_it() {
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
        std::fs::write(
            summaries.join("b.md"),
            header(&home.path().join("elsewhere")) + "# Other\n",
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
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(plan::read(&path).unwrap(), plan);
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
            "other-project (1): ~/.codex/memories/rollout_summaries/b.md",
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
        let error = message(&make_plan(&plan_args(home.path(), Host::Codex), now).unwrap_err());
        assert!(error.contains("--workspace"), "{error}");
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
            issue("~/e.jsonl", "invalid-line 9: unknown kind \"x\""),
            issue("~/c.md", "symlink"),
        ]);
        assert_eq!(groups["symlink"], ["~/a.md", "~/c.md"]);
        assert_eq!(groups["unreadable"], ["~/b.md (permission denied)"]);
        assert_eq!(
            groups["invalid-line"],
            [
                "~/e.jsonl (line 3: no text)",
                "~/e.jsonl (line 9: unknown kind \"x\")"
            ]
        );
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
        let daemon = DaemonArgs {
            url: url.clone(),
            tenant: "default".into(),
        };
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
        assert_eq!(
            review::read(&review_path, "ir1").unwrap().decisions.len(),
            3
        );
        let applied = apply("ir1", Some(&review_path), &daemon, home.path())
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
        assert!(rollback("ir1", &daemon)
            .await
            .unwrap()
            .starts_with("Run ir1"));

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
        let daemon = DaemonArgs {
            url,
            tenant: "default".into(),
        };
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
        // Another --out is written; a link at the path counts as an existing file.
        let other = home.path().join("other.review.jsonl");
        show("ir1", Some(&other), false, &daemon, home.path())
            .await
            .unwrap();
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
        let daemon = DaemonArgs {
            url,
            tenant: "default".into(),
        };
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
        // They follow the unit's last proposal.
        assert_eq!(lines[5]["memoryId"], "m1");
        assert_eq!(review::read(&path, "ir1").unwrap().decisions.len(), 3);
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
        let (url, requests) = stub_with(stub_run(), sourced_proposals(home.path())).await;
        let daemon = DaemonArgs {
            url,
            tenant: "default".into(),
        };
        std::fs::write(home.path().join("b.md"), "Edited.\n").unwrap();
        std::fs::remove_file(home.path().join("c.md")).unwrap();
        let report = apply("ir1", None, &daemon, home.path()).await.unwrap();
        for part in [
            "~/b.md changed since propose: its 2 proposals were declined",
            "~/c.md vanished since propose: its 1 proposals were declined",
            "propose again to import the current text",
            "default decisions",
        ] {
            assert!(report.contains(part), "{part}\n{report}");
        }
        assert_eq!(report.matches("since propose").count(), 2, "{report}");
        let last = requests.lock().unwrap().last().unwrap().2.clone();
        assert_eq!(last["decisions"]["ip4"], "reject");
    }
}
