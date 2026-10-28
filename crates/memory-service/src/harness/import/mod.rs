//! `agent-memoryd import` (docs/design/memory-import.md): read the memories an agent host already keeps
//! into a plan file to review (`plan`: files only, no network), send the plan to the daemon as one run of
//! proposals (`propose`: writes no memory), review the proposals (`show`), write the accepted ones
//! (`apply`), and inspect or undo runs (`status`, `rollback`). `prompt` prints the request that has a
//! host's own agent export its memories as JSONL, or the one that has a connected host's agent import such an
//! export in its conversation through the MCP tools `propose_import` and `apply_import` (`chat`). Host knowledge lives in the adapters (`claude`, `codex`,
//! `workbuddy`, `jsonl`), generic policy in `plan`, the commands' work and reports in `commands`. Past session
//! transcripts (`--include sessions`) come from `claude_session` and `codex_session`; their turns are queued into
//! the dialogue outbox at apply, so Mem0 formation forms them as it forms captured turns. Every command prints to
//! stdout and exits 0; an error is one `[error] …` line on stderr and exit 1.
pub mod chat;
pub mod claude;
pub mod claude_session;
pub mod client;
pub mod codex;
pub mod codex_session;
pub mod commands;
pub mod jsonl;
pub mod plan;
pub mod project;
pub mod prompt;
pub mod review;
pub mod session;
pub mod source;
pub mod workbuddy;

use chrono::Utc;
use clap::Subcommand;
use memory_rules::import::units::UnitLimits;
use std::path::{Path, PathBuf};
use std::time::Duration;

use self::prompt::Lang;
use self::session::SessionAdapter;
use self::source::{display, timestamp, Adapter};
use super::{
    expand, home, invalid, loopback_url, outbox, positive, positive_seconds, private_file, Error,
};

pub const DEFAULT_URL: &str = "http://127.0.0.1:8765";
/// Units per `import_propose` request at most.
const MAX_BATCH: usize = 64;
/// Session turns a plan takes by default (design §6.14): enough for a first look, few enough not to hold up
/// newly captured turns for long.
const DEFAULT_TURN_LIMIT: usize = 200;

/// Where the memories come from.
#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Host {
    /// Claude Code auto memory (`~/.claude/projects/<dir>/memory`)
    #[value(name = "claude-code")]
    ClaudeCode,
    /// Codex rollout summaries (`~/.codex/memories/rollout_summaries`)
    Codex,
    /// WorkBuddy memory files (`~/.workbuddy`)
    Workbuddy,
    /// A host agent's export in the JSONL `import prompt` asks for (`--from FILE`)
    Jsonl,
}

impl Host {
    fn adapter(self) -> Box<dyn Adapter> {
        match self {
            Host::ClaudeCode => Box::new(claude::Claude),
            Host::Codex => Box::new(codex::Codex),
            Host::Workbuddy => Box::new(workbuddy::WorkBuddy),
            Host::Jsonl => Box::new(jsonl::Export),
        }
    }

    /// The reader of the host's session transcripts, for the hosts whose turns capture files under ids the
    /// transcripts hold.
    fn session_adapter(self) -> Option<Box<dyn SessionAdapter>> {
        match self {
            Host::ClaudeCode => Some(Box::new(claude_session::ClaudeSessions)),
            Host::Codex => Some(Box::new(codex_session::CodexSessions)),
            Host::Workbuddy | Host::Jsonl => None,
        }
    }

    /// The platform label the memories are filed under.
    fn label(self, label: Option<&str>) -> String {
        match self {
            Host::ClaudeCode => claude::HOST.into(),
            Host::Codex => codex::HOST.into(),
            Host::Workbuddy => workbuddy::HOST.into(),
            Host::Jsonl => label.unwrap_or(jsonl::DEFAULT_LABEL).into(),
        }
    }
}

/// What a plan reads.
#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Include {
    /// The memory files the host keeps
    Notes,
    /// Past session transcripts (claude-code, codex): each turn a person asked, formed by Mem0 like a captured one
    Sessions,
}

/// The daemon an import command talks to.
#[derive(clap::Args, Debug, Clone, PartialEq, Eq)]
pub struct DaemonArgs {
    /// Daemon base URL; loopback only
    #[arg(long, default_value = DEFAULT_URL, value_parser = loopback_url)]
    pub url: String,
}

#[derive(clap::Args, Debug, Clone)]
pub struct PlanArgs {
    /// Where the memories are
    #[arg(long, value_enum)]
    pub host: Host,
    /// Project id to import into, as capture and recall file sessions: the project's folder, as a path.
    /// [default: the project of the single --workspace; claude-code and codex only]
    #[arg(long, value_parser = nonblank)]
    pub project: Option<String>,
    /// A directory of the project; repeatable, worktrees map to their repository (claude-code, codex)
    #[arg(long = "workspace", value_name = "DIR")]
    pub workspaces: Vec<PathBuf>,
    /// claude-code: also every project directory whose transcripts name a workspace directory
    #[arg(long)]
    pub all: bool,
    /// jsonl: the export to read
    #[arg(long, value_name = "FILE")]
    pub from: Option<PathBuf>,
    /// jsonl: platform label of the host that exported [default: agent-export]
    #[arg(long, value_parser = platform_label)]
    pub label: Option<String>,
    /// What to read, comma-separated: notes, sessions
    #[arg(long, value_enum, value_delimiter = ',', default_value = "notes")]
    pub include: Vec<Include>,
    /// sessions: only turns asked at or after this time (RFC3339, or YYYY-MM-DD in UTC)
    #[arg(long, value_parser = since_time)]
    pub since: Option<String>,
    /// sessions: at most this many turns, the oldest first; the report names the --since of the next batch
    #[arg(long, default_value_t = DEFAULT_TURN_LIMIT, value_parser = positive)]
    pub limit: usize,
    /// Call no generation model: split units over the cap instead of extracting them
    #[arg(long)]
    pub no_extract: bool,
    /// tokenBudget of the recall hook's search; a unit costs at most half of it
    #[arg(long, default_value_t = 400, value_parser = recall_budget)]
    pub recall_budget: usize,
    /// topK of the recall hook's search
    #[arg(long, default_value_t = 5, value_parser = positive)]
    pub recall_top_k: usize,
    /// Home directory the host directories and `~/` paths are under [default: $HOME]
    #[arg(long, value_name = "DIR")]
    pub home: Option<PathBuf>,
    /// Claude Code configuration directory [default: ~/.claude]
    #[arg(long, value_name = "DIR")]
    pub claude_config_dir: Option<PathBuf>,
    /// Codex home [default: ~/.codex]
    #[arg(long, value_name = "DIR")]
    pub codex_home: Option<PathBuf>,
    /// WorkBuddy directory [default: ~/.workbuddy]
    #[arg(long, value_name = "DIR")]
    pub workbuddy_home: Option<PathBuf>,
    /// Plan file [default: ~/.local/state/agent-memory-service/imports/<host>-<project>-<time>.plan.jsonl]
    #[arg(long, value_name = "FILE")]
    pub out: Option<PathBuf>,
}

/// Where `apply` queues a run's session turns, and how fast.
#[derive(clap::Args, Debug, Clone, PartialEq)]
pub struct TurnArgs {
    /// Dialogue outbox the uploader drains, as the capture hooks use it
    #[arg(long, default_value_os_t = outbox::default_path())]
    pub outbox: PathBuf,
    /// Session turns queued per minute at most; 0 queues them all at once, without waiting
    #[arg(long, default_value_t = 20.0, value_parser = rate)]
    pub rate: f64,
    /// Wait while the outbox holds this many turns not yet formed (live capture's too), so newly captured
    /// turns are not held up behind the import
    #[arg(long, default_value_t = 4, value_parser = positive)]
    pub max_pending: usize,
}

#[derive(Subcommand, Debug)]
pub enum ImportCmd {
    /// Read a host's memory files, or its past sessions, into a plan file to review; reads files only, no network
    Plan(PlanArgs),
    /// Send a plan's units to the daemon as one run of proposals; writes no memory
    Propose {
        /// Plan file from `import plan`
        #[arg(value_name = "PLAN")]
        plan: PathBuf,
        #[command(flatten)]
        daemon: DaemonArgs,
        /// Units per request
        #[arg(long, default_value_t = 8, value_parser = batch_size)]
        batch: usize,
        /// Seconds per request; extraction waits for the generation model
        #[arg(long, default_value_t = 900.0, value_parser = positive_seconds)]
        timeout: f64,
        /// Home directory the plan's `~/` paths are under, as given to `import plan` [default: $HOME]
        #[arg(long, value_name = "DIR")]
        home: Option<PathBuf>,
        /// Also propose again the units whose last import applied nothing (every proposal declined or
        /// rejected, or nothing extracted) although their text is unchanged; without it they are
        /// `unchanged` until their text changes
        #[arg(long)]
        reconsider: bool,
    },
    /// Write a run's proposals to a review file, to set each decision to accept or reject
    Show {
        #[arg(value_name = "RUN")]
        run: String,
        /// Review file [default: ~/.local/state/agent-memory-service/imports/<run>.review.jsonl]
        #[arg(long, value_name = "FILE")]
        out: Option<PathBuf>,
        /// Replace the review file if it exists, losing the decisions made in it
        #[arg(long)]
        force: bool,
        #[command(flatten)]
        daemon: DaemonArgs,
    },
    /// Write a run's accepted proposals, as a review file decides or with --yes all of them
    #[command(group(clap::ArgGroup::new("decisions").required(true).args(["review", "yes"])))]
    Apply {
        #[arg(value_name = "RUN")]
        run: String,
        /// Review file from `import show`; a proposal it no longer lists is declined
        #[arg(long, value_name = "FILE")]
        review: Option<PathBuf>,
        /// Accept every proposal; a duplicate is linked to its record, not written again
        #[arg(long)]
        yes: bool,
        #[command(flatten)]
        daemon: DaemonArgs,
        /// Home directory the run's `~/` source paths are under, as given to `import plan`; the
        /// proposals whose source file changed since propose are declined [default: $HOME]
        #[arg(long, value_name = "DIR")]
        home: Option<PathBuf>,
        #[command(flatten)]
        turns: TurnArgs,
    },
    /// One run's summary, or the newest runs
    Status {
        #[arg(value_name = "RUN")]
        run: Option<String>,
        /// Only runs of this project
        #[arg(long, conflicts_with = "run")]
        project: Option<String>,
        #[command(flatten)]
        daemon: DaemonArgs,
    },
    /// Undo an applied run, newest write first; records changed since are left alone
    Rollback {
        #[arg(value_name = "RUN")]
        run: String,
        #[command(flatten)]
        daemon: DaemonArgs,
        /// Dialogue outbox the run's session turns were queued into; those not yet submitted are cancelled
        #[arg(long, default_value_os_t = outbox::default_path())]
        outbox: PathBuf,
    },
    /// Print the prompt that has an assistant export its memories for `plan --host jsonl`, or with
    /// `--for host` the prompt that walks a connected host's agent through an import in its conversation
    Prompt {
        #[arg(long, value_parser = nonblank)]
        project: String,
        #[arg(long, value_enum, default_value_t = Lang::Zh)]
        lang: Lang,
        /// Who reads the prompt: the assistant that exports, or the agent of a host connected to shared-memory
        #[arg(long = "for", value_enum, default_value_t = Audience::Source)]
        audience: Audience,
        /// --for host: the platform label of the assistant the memories come from (`chatgpt`), or `self`
        #[arg(long, value_parser = platform_label, required_if_eq("audience", "host"))]
        source: Option<String>,
    },
}

/// Who an `import prompt` is for.
#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Audience {
    /// The assistant whose memories are exported
    Source,
    /// The agent of a host connected to shared-memory, which imports the export through MCP
    Host,
}

/// Run an import command; returns the process exit code.
pub async fn run(cmd: ImportCmd) -> i32 {
    match execute(cmd).await {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("[error] {}", message(&e));
            1
        }
    }
}

async fn execute(cmd: ImportCmd) -> Result<(), Error> {
    let output = match cmd {
        ImportCmd::Plan(args) => {
            let (path, plan) = commands::make_plan(&args, Utc::now())?;
            commands::plan_report(&path, &plan, &args)
        }
        ImportCmd::Propose {
            plan,
            daemon,
            batch,
            timeout,
            home: plan_home,
            reconsider,
        } => {
            let plan_home = plan_home.as_deref().map(expand).unwrap_or_else(home);
            let timeout = Duration::from_secs_f64(timeout);
            let mut say = |line: &str| println!("{line}");
            commands::propose(
                &expand(&plan),
                &daemon,
                batch,
                timeout,
                &plan_home,
                reconsider,
                &mut say,
            )
            .await?
        }
        ImportCmd::Show {
            run,
            out,
            force,
            daemon,
        } => {
            let out = out.as_deref().map(expand);
            commands::show(&run, out.as_deref(), force, &daemon, &home()).await?
        }
        ImportCmd::Apply {
            run,
            review,
            yes: _,
            daemon,
            home: plan_home,
            turns,
        } => {
            let review = review.as_deref().map(expand);
            let plan_home = plan_home.as_deref().map(expand).unwrap_or_else(home);
            let pace = commands::Pace::new(turns.rate, turns.max_pending);
            let mut say = |line: &str| println!("{line}");
            commands::apply(
                &run,
                review.as_deref(),
                &daemon,
                &plan_home,
                &expand(&turns.outbox),
                &pace,
                &mut say,
            )
            .await?
        }
        ImportCmd::Status {
            run,
            project,
            daemon,
        } => commands::status(run.as_deref(), project.as_deref(), &daemon).await?,
        ImportCmd::Rollback {
            run,
            daemon,
            outbox,
        } => commands::rollback(&run, &daemon, &expand(&outbox)).await?,
        ImportCmd::Prompt {
            project,
            lang,
            audience: Audience::Source,
            ..
        } => prompt::text(&project, lang),
        ImportCmd::Prompt {
            project,
            lang,
            audience: Audience::Host,
            source,
        } => prompt::host_text(
            &project,
            source
                .as_deref()
                .expect("clap requires --source for --for host"),
            None,
            lang,
        ),
    };
    print!("{output}");
    Ok(())
}

/// The message of the `[error]` line: field names and reasons, never file contents; from the daemon only
/// its validation `message`, on one line.
pub(crate) fn message(error: &Error) -> String {
    match error {
        Error::Invalid(message) => message.clone(),
        other => other.to_string(),
    }
}

/// A path for messages: `~/…` under the home directory.
pub(crate) fn shown(path: &Path) -> String {
    display(&home(), path)
}

/// Write `text` to `path` owner-only (new directories 0700, the file 0600), replacing an earlier file.
pub(crate) fn write_private(path: &Path, text: &str) -> Result<(), Error> {
    let failed = |e: std::io::Error| invalid(format!("{}: {e}", shown(path)));
    private_file(path).map_err(failed)?;
    std::fs::write(path, text).map_err(failed)
}

fn nonblank(value: &str) -> Result<String, String> {
    match value.trim().is_empty() {
        true => Err("must not be blank".into()),
        false => Ok(value.to_string()),
    }
}

/// A recall budget that leaves a unit room for text.
fn recall_budget(value: &str) -> Result<usize, String> {
    match value.parse::<usize>() {
        Ok(n) if n >= UnitLimits::MIN_RECALL_BUDGET => Ok(n),
        _ => Err(format!(
            "must be an integer of at least {}",
            UnitLimits::MIN_RECALL_BUDGET
        )),
    }
}

/// A `--since` time, normalized to RFC3339 UTC.
fn since_time(value: &str) -> Result<String, String> {
    timestamp(value).ok_or_else(|| "must be an RFC3339 time or a YYYY-MM-DD date".into())
}

/// Turns per minute: 0 (no pacing) or a positive number.
fn rate(value: &str) -> Result<f64, String> {
    match value.parse::<f64>() {
        Ok(n) if n >= 0.0 && n.is_finite() => Ok(n),
        _ => Err("must be 0 or a positive number of turns per minute".into()),
    }
}

fn batch_size(value: &str) -> Result<usize, String> {
    match value.parse::<usize>() {
        Ok(n) if (1..=MAX_BATCH).contains(&n) => Ok(n),
        _ => Err(format!("must be an integer from 1 to {MAX_BATCH}")),
    }
}

/// A platform label, as sent in `X-Memory-Platform`: ASCII letters, digits, `.`, `_` and `-`.
pub(crate) fn platform_label(value: &str) -> Result<String, String> {
    let valid = (1..=64).contains(&value.len())
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c));
    match valid {
        true => Ok(value.to_string()),
        false => Err("must be 1 to 64 ASCII letters, digits, '.', '_' or '-'".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, Parser};

    #[derive(Parser, Debug)]
    struct Cli {
        #[command(subcommand)]
        cmd: ImportCmd,
    }

    fn parse(args: &[&str]) -> Result<ImportCmd, clap::Error> {
        let argv = ["agent-memoryd-import"].iter().chain(args);
        Cli::try_parse_from(argv).map(|cli| cli.cmd)
    }

    #[test]
    fn every_command_parses_with_its_defaults() {
        Cli::command().debug_assert();
        let ImportCmd::Plan(plan) = parse(&[
            "plan",
            "--host",
            "codex",
            "--project",
            "p",
            "--workspace",
            "/a",
            "--workspace",
            "/b",
        ])
        .unwrap() else {
            panic!("plan")
        };
        assert_eq!(plan.host, Host::Codex);
        assert_eq!(plan.project.as_deref(), Some("p"));
        assert_eq!(plan.workspaces, [PathBuf::from("/a"), PathBuf::from("/b")]);
        assert_eq!((plan.recall_budget, plan.recall_top_k), (400, 5));
        assert!(!plan.all && !plan.no_extract && plan.out.is_none());
        assert_eq!(plan.include, [Include::Notes]);
        assert_eq!((plan.since.as_deref(), plan.limit), (None, 200));
        let ImportCmd::Plan(plan) = parse(&[
            "plan",
            "--host",
            "claude-code",
            "--project",
            "p",
            "--include",
            "notes,sessions",
            "--since",
            "2026-03-01",
            "--limit",
            "50",
        ])
        .unwrap() else {
            panic!("plan")
        };
        assert_eq!(plan.include, [Include::Notes, Include::Sessions]);
        assert_eq!(
            (plan.since.as_deref(), plan.limit),
            (Some("2026-03-01T00:00:00Z"), 50)
        );
        let ImportCmd::Plan(plan) = parse(&[
            "plan",
            "--host",
            "jsonl",
            "--project",
            "p",
            "--from",
            "x.jsonl",
            "--label",
            "chat.gpt",
            "--no-extract",
            "--recall-budget",
            "40",
            "--recall-top-k",
            "1",
            "--out",
            "p.jsonl",
        ])
        .unwrap() else {
            panic!("plan")
        };
        assert_eq!(
            (plan.host, plan.label.as_deref()),
            (Host::Jsonl, Some("chat.gpt"))
        );
        assert_eq!(
            (plan.recall_budget, plan.recall_top_k, plan.no_extract),
            (40, 1, true)
        );
        assert!(matches!(
            parse(&["plan", "--host", "claude-code", "--project", "p", "--all"]).unwrap(),
            ImportCmd::Plan(PlanArgs {
                all: true,
                host: Host::ClaudeCode,
                ..
            })
        ));
        // Without --project the plan takes the project of its single workspace; `make_plan` refuses the rest.
        let ImportCmd::Plan(plan) =
            parse(&["plan", "--host", "codex", "--workspace", "/a"]).unwrap()
        else {
            panic!("plan")
        };
        assert_eq!(plan.project, None);
        for bad in [
            &["plan", "--host", "cursor", "--project", "p"][..],
            &["plan", "--host", "codex", "--project", " "],
            &[
                "plan",
                "--host",
                "codex",
                "--project",
                "p",
                "--recall-budget",
                "39",
            ],
            &[
                "plan",
                "--host",
                "codex",
                "--project",
                "p",
                "--recall-top-k",
                "0",
            ],
            &[
                "plan",
                "--host",
                "jsonl",
                "--project",
                "p",
                "--label",
                "two words",
            ],
            &[
                "plan",
                "--host",
                "codex",
                "--project",
                "p",
                "--include",
                "rules",
            ],
            &[
                "plan",
                "--host",
                "codex",
                "--project",
                "p",
                "--since",
                "last week",
            ],
            &["plan", "--host", "codex", "--project", "p", "--limit", "0"],
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }

        let ImportCmd::Propose {
            plan,
            daemon,
            batch,
            timeout,
            home,
            reconsider,
        } = parse(&["propose", "p.plan.jsonl"]).unwrap()
        else {
            panic!("propose")
        };
        assert_eq!(plan, PathBuf::from("p.plan.jsonl"));
        assert_eq!(
            daemon,
            DaemonArgs {
                url: DEFAULT_URL.into(),
            }
        );
        assert_eq!((batch, timeout, home, reconsider), (8, 900.0, None, false));
        let ImportCmd::Propose {
            daemon,
            batch,
            reconsider,
            ..
        } = parse(&[
            "propose",
            "p",
            "--url",
            "http://localhost:9000/",
            "--batch",
            "64",
            "--reconsider",
        ])
        .unwrap()
        else {
            panic!("propose")
        };
        assert_eq!((daemon.url.as_str(), batch), ("http://localhost:9000", 64));
        assert!(reconsider);
        for bad in [
            // Both bounds: the client relies on a batch of at least one.
            &["propose", "p", "--batch", "0"][..],
            &["propose", "p", "--batch", "65"],
            &["propose", "p", "--timeout", "0"],
            &["propose"],
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }

        assert!(matches!(
            parse(&["show", "ir1", "--out", "r.jsonl"]).unwrap(),
            ImportCmd::Show {
                out: Some(_),
                force: false,
                ..
            }
        ));
        assert!(matches!(
            parse(&["show", "ir1", "--force"]).unwrap(),
            ImportCmd::Show {
                out: None,
                force: true,
                ..
            }
        ));
        let ImportCmd::Apply {
            yes: true,
            review: None,
            home: None,
            turns,
            ..
        } = parse(&["apply", "ir1", "--yes"]).unwrap()
        else {
            panic!("apply")
        };
        assert_eq!(
            turns,
            TurnArgs {
                outbox: outbox::default_path(),
                rate: 20.0,
                max_pending: 4
            }
        );
        let ImportCmd::Apply { turns, .. } = parse(&[
            "apply",
            "ir1",
            "--yes",
            "--outbox",
            "/o.sqlite3",
            "--rate",
            "0",
            "--max-pending",
            "1",
        ])
        .unwrap() else {
            panic!("apply")
        };
        assert_eq!(
            (turns.outbox, turns.rate, turns.max_pending),
            (PathBuf::from("/o.sqlite3"), 0.0, 1)
        );
        for bad in [
            &["apply", "ir1", "--yes", "--rate", "-1"][..],
            &["apply", "ir1", "--yes", "--max-pending", "0"],
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
        let ImportCmd::Apply { home, .. } =
            parse(&["apply", "ir1", "--yes", "--home", "/h"]).unwrap()
        else {
            panic!("apply")
        };
        assert_eq!(home, Some(PathBuf::from("/h")));
        assert!(matches!(
            parse(&["apply", "ir1", "--review", "r.jsonl"]).unwrap(),
            ImportCmd::Apply {
                yes: false,
                review: Some(_),
                ..
            }
        ));
        assert!(
            parse(&["apply", "ir1"]).is_err(),
            "apply needs --review or --yes"
        );
        assert!(parse(&["apply", "ir1", "--review", "r", "--yes"]).is_err());
        assert!(matches!(
            parse(&["status"]).unwrap(),
            ImportCmd::Status {
                run: None,
                project: None,
                ..
            }
        ));
        assert!(matches!(
            parse(&["status", "ir1"]).unwrap(),
            ImportCmd::Status { run: Some(_), .. }
        ));
        assert!(matches!(
            parse(&["status", "--project", "p"]).unwrap(),
            ImportCmd::Status {
                project: Some(_),
                ..
            }
        ));
        assert!(parse(&["status", "ir1", "--project", "p"]).is_err());
        let ImportCmd::Rollback { outbox: path, .. } = parse(&["rollback", "ir1"]).unwrap() else {
            panic!("rollback")
        };
        assert_eq!(path, outbox::default_path());
        assert!(parse(&["rollback"]).is_err());
        assert!(matches!(
            parse(&["prompt", "--project", "p"]).unwrap(),
            ImportCmd::Prompt { lang: Lang::Zh, .. }
        ));
        assert!(matches!(
            parse(&["prompt", "--project", "p", "--lang", "en"]).unwrap(),
            ImportCmd::Prompt {
                lang: Lang::En,
                audience: Audience::Source,
                source: None,
                ..
            }
        ));
        assert!(parse(&["prompt"]).is_err());
        let ImportCmd::Prompt {
            audience, source, ..
        } = parse(&[
            "prompt",
            "--project",
            "p",
            "--for",
            "host",
            "--source",
            "chatgpt",
        ])
        .unwrap()
        else {
            panic!("prompt")
        };
        assert_eq!(
            (audience, source.as_deref()),
            (Audience::Host, Some("chatgpt"))
        );
        for bad in [
            &["prompt", "--project", "p", "--for", "host"][..],
            &[
                "prompt",
                "--project",
                "p",
                "--for",
                "host",
                "--source",
                "chat gpt",
            ],
            &["prompt", "--project", "p", "--for", "everyone"],
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn platform_labels_are_plain_ascii() {
        assert!(platform_label("claude-code").is_ok());
        assert!(platform_label("").is_err() && platform_label("名").is_err());
    }
}
