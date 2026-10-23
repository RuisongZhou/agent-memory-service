//! `agent-memoryd service`: run `serve` as a per-user service, a macOS LaunchAgent or a Linux
//! systemd user unit, under the label and file name of the templates in docs/deployment.md.
//! Previews unless `--apply`; writes only the unit file and the directories it names, backs up a
//! file it replaces or removes, never reads the env file or puts a key in the unit, and prints the
//! launchctl/systemctl commands instead of running them unless `install --start` asks it to run
//! them in the login session.
use anyhow::{bail, ensure, Context, Result};
use clap::{Args, Subcommand, ValueEnum};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const LAUNCHD_LABEL: &str = "local.agent-memory-daemon";
pub const SYSTEMD_UNIT: &str = "agent-memory.service";
/// Default memory store, relative to the home directory.
pub const DEFAULT_DATA_DIR: &str = ".local/share/agent-memory-service/mem0";
/// Copies of replaced or removed unit files, relative to the home directory.
pub const BACKUPS: &str = ".local/state/agent-memory-service/service-backups";
/// LaunchAgent stdout/stderr logs, relative to the home directory.
pub const LAUNCHD_LOGS: &str = "Library/Logs/agent-memory-service";
/// Opening comment of every file `service install` writes.
const MARKER: &str = "Written by agent-memoryd service install";
const SYSTEMD_ENABLED: &str = ".config/systemd/user/default.target.wants";
/// How long `install --start` waits for the daemon to answer; startup probes the retrieval models.
const START_WAIT: Duration = Duration::from_secs(60);
/// How long `install --start` waits for launchd to unload the old job before loading the new one.
const UNLOAD_WAIT: Duration = Duration::from_secs(10);

#[derive(Subcommand, Debug)]
pub enum ServiceCmd {
    /// Write a LaunchAgent (macOS) or systemd user unit (Linux) that runs `serve`; preview unless --apply
    Install(InstallArgs),
    /// Remove the unit file after the service is stopped; preview unless --apply
    Uninstall {
        #[command(flatten)]
        at: Target,
        /// Remove the file (default: preview only)
        #[arg(long)]
        apply: bool,
    },
    /// Show the unit file, the command it runs and whether a daemon answers on its port
    Status {
        #[command(flatten)]
        at: Target,
        /// Print the unit's settings as one JSON object instead, without the health check
        #[arg(long)]
        json: bool,
    },
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Manager {
    /// LaunchAgent ~/Library/LaunchAgents/local.agent-memory-daemon.plist
    Launchd,
    /// systemd user unit ~/.config/systemd/user/agent-memory.service
    Systemd,
}

/// The service manager and home directory a command acts on.
#[derive(Args, Debug, Clone)]
pub struct Target {
    /// Service manager [default: launchd on macOS, systemd elsewhere]
    #[arg(long, value_enum)]
    pub manager: Option<Manager>,
    /// Home directory of the unit file and the default data, outbox, log and backup paths
    /// [default: $HOME]
    #[arg(long, value_name = "DIR")]
    pub home: Option<PathBuf>,
}

#[derive(Args, Debug, Clone)]
pub struct InstallArgs {
    #[command(flatten)]
    pub at: Target,
    /// Write the unit (default: preview only)
    #[arg(long)]
    pub apply: bool,
    /// Dotenv file with the provider keys, read by the daemon at each start (`serve --env-file`);
    /// the unit stores only its path
    #[arg(long, value_name = "PATH")]
    pub env_file: PathBuf,
    /// Memory store [default: ~/.local/share/agent-memory-service/mem0]
    #[arg(long, value_name = "DIR")]
    pub data_dir: Option<PathBuf>,
    /// Also upload captured dialogue from this outbox (the default outbox when given without PATH)
    #[arg(long, value_name = "PATH", num_args = 0..=1)]
    pub outbox: Option<Option<PathBuf>>,
    /// Port the daemon listens on (`serve --port`)
    #[arg(long, default_value_t = 8765)]
    pub port: u16,
    /// Concurrent generation requests (`serve --llm-concurrency`)
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..))]
    pub llm_concurrency: u32,
    /// agent-memoryd executable the unit runs [default: this executable]
    #[arg(long, value_name = "PATH")]
    pub binary: Option<PathBuf>,
    /// SurrealDB 3.x `surreal` executable the daemon starts for its store (`serve --surreal-bin`);
    /// the unit records its absolute path [default: `surreal` on PATH]
    #[arg(long, value_name = "PATH")]
    pub surreal_bin: Option<PathBuf>,
    /// Then load the service in your login session (reloading it when the unit changed) and wait
    /// until the daemon answers; runs launchctl or systemctl
    #[arg(long, requires = "apply")]
    pub start: bool,
    /// With --start, also restart a loaded service whose unit did not change, so it runs a
    /// replaced binary
    #[arg(long, requires = "start")]
    pub restart: bool,
}

impl Manager {
    pub fn unit_path(self, home: &Path) -> PathBuf {
        match self {
            Manager::Launchd => home
                .join("Library/LaunchAgents")
                .join(format!("{LAUNCHD_LABEL}.plist")),
            Manager::Systemd => home.join(".config/systemd/user").join(SYSTEMD_UNIT),
        }
    }
}

impl Target {
    pub fn manager(&self) -> Manager {
        self.manager.unwrap_or(if cfg!(target_os = "macos") {
            Manager::Launchd
        } else {
            Manager::Systemd
        })
    }

    pub fn home(&self) -> Result<PathBuf> {
        let home = match &self.home {
            Some(home) => home.clone(),
            None => std::env::var_os("HOME")
                .filter(|h| !h.is_empty())
                .map(PathBuf::from)
                .context("HOME is not set; pass --home")?,
        };
        Ok(std::path::absolute(home)?)
    }
}

/// The `serve` command a unit runs; every path is absolute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Serve {
    pub binary: PathBuf,
    pub env_file: PathBuf,
    pub data_dir: PathBuf,
    pub surreal_bin: PathBuf,
    pub outbox: Option<PathBuf>,
    pub port: u16,
    pub llm_concurrency: u32,
}

impl Serve {
    /// The command line, starting with the executable.
    pub fn argv(&self) -> Result<Vec<String>> {
        let mut argv = vec![
            unit_text(&self.binary)?,
            "serve".into(),
            "--env-file".into(),
            unit_text(&self.env_file)?,
            "--data-dir".into(),
            unit_text(&self.data_dir)?,
            "--surreal-bin".into(),
            unit_text(&self.surreal_bin)?,
            "--port".into(),
            self.port.to_string(),
            "--llm-concurrency".into(),
            self.llm_concurrency.to_string(),
        ];
        if let Some(outbox) = &self.outbox {
            argv.push("--outbox".into());
            argv.push(unit_text(outbox)?);
        }
        Ok(argv)
    }
}

/// A path as unit-file text: UTF-8 without control characters.
fn unit_text(path: &Path) -> Result<String> {
    let text = path
        .to_str()
        .with_context(|| format!("{} is not valid UTF-8", path.display()))?;
    ensure!(
        !text.chars().any(char::is_control),
        "{} contains a control character",
        path.display()
    );
    Ok(text.to_string())
}

/// The file `manager` reads: a LaunchAgent property list or a systemd user unit.
pub fn render(manager: Manager, serve: &Serve, home: &Path) -> Result<String> {
    let argv = serve.argv()?;
    let mut lines: Vec<String> = Vec::new();
    match manager {
        Manager::Launchd => {
            let string = |s: &str| format!("<string>{}</string>", xml_escape(s));
            let logs = home.join(LAUNCHD_LOGS);
            lines.push(r#"<?xml version="1.0" encoding="UTF-8"?>"#.into());
            lines.push(r#"<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">"#.into());
            lines.push(format!("<!-- {MARKER}. Keys stay in the env file. -->"));
            lines.push(r#"<plist version="1.0">"#.into());
            lines.push("<dict>".into());
            lines.push(format!("  <key>Label</key>{}", string(LAUNCHD_LABEL)));
            lines.push("  <key>ProgramArguments</key>".into());
            lines.push("  <array>".into());
            lines.extend(argv.iter().map(|arg| format!("    {}", string(arg))));
            lines.push("  </array>".into());
            lines.push("  <key>RunAtLoad</key><true/>".into());
            lines.push("  <key>KeepAlive</key><true/>".into());
            lines.push("  <key>ThrottleInterval</key><integer>10</integer>".into());
            // 63 is umask 077; property lists have no octal integers.
            lines.push("  <key>Umask</key><integer>63</integer>".into());
            for (key, file) in [
                ("StandardOutPath", "daemon.stdout.log"),
                ("StandardErrorPath", "daemon.stderr.log"),
            ] {
                let path = unit_text(&logs.join(file))?;
                lines.push(format!("  <key>{key}</key>{}", string(&path)));
            }
            lines.push("</dict>".into());
            lines.push("</plist>".into());
        }
        Manager::Systemd => {
            lines.push(format!("# {MARKER}. Keys stay in the env file."));
            lines.push("[Unit]".into());
            lines.push("Description=Agent Memory daemon".into());
            lines.push(String::new());
            lines.push("[Service]".into());
            let words: Vec<String> = argv.iter().map(|a| systemd_quote(a)).collect();
            lines.push(format!("ExecStart={}", words.join(" ")));
            // The daemon exits when the retrieval models do not answer at startup; retry later.
            lines.push("Restart=on-failure".into());
            lines.push("RestartSec=5".into());
            lines.push("UMask=0077".into());
            lines.push(String::new());
            lines.push("[Install]".into());
            lines.push("WantedBy=default.target".into());
        }
    }
    Ok(lines.join("\n") + "\n")
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// One systemd command-line word: `%` specifiers and `$` expansion escaped, quoted when needed.
fn systemd_quote(word: &str) -> String {
    let escaped = word.replace('%', "%%").replace('$', "$$");
    if !escaped.is_empty() && escaped.chars().all(safe_char) {
        return escaped;
    }
    format!("\"{}\"", escaped.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The words of a systemd command line written by `systemd_quote`.
fn systemd_words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut chars = line.chars().peekable();
    loop {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        let Some(first) = chars.next() else {
            return words;
        };
        let mut word = String::new();
        if first == '"' {
            while let Some(c) = chars.next() {
                match c {
                    '"' => break,
                    '\\' => word.extend(chars.next()),
                    c => word.push(c),
                }
            }
        } else {
            word.push(first);
            while let Some(c) = chars.next_if(|c| !c.is_whitespace()) {
                word.push(c);
            }
        }
        words.push(word.replace("%%", "%").replace("$$", "$"));
    }
}

/// Characters that need no quoting in a systemd command line or a shell.
fn safe_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || "/._-+:@=,%".contains(c)
}

/// `word` quoted for a POSIX shell.
fn sh(word: &str) -> String {
    if !word.is_empty() && word.chars().all(safe_char) {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

/// The `serve` argv a unit file runs, as written by `render` (or a hand-made file of the same form).
pub fn unit_argv(manager: Manager, text: &str) -> Option<Vec<String>> {
    match manager {
        Manager::Launchd => {
            let start = text.find("<key>ProgramArguments</key>")?;
            let rest = &text[start..];
            let array = &rest[..rest.find("</array>")?];
            let argv: Vec<String> = array
                .split("<string>")
                .skip(1)
                .filter_map(|s| s.split_once("</string>"))
                .map(|(s, _)| xml_unescape(s))
                .collect();
            (!argv.is_empty()).then_some(argv)
        }
        Manager::Systemd => text
            .lines()
            .find_map(|line| line.trim().strip_prefix("ExecStart="))
            .map(systemd_words),
    }
}

/// What `install` or `uninstall` did, as report lines.
#[derive(Debug, Default)]
pub struct Outcome {
    pub report: Vec<String>,
    pub path: PathBuf,
    /// The unit file changed on disk.
    pub changed: bool,
    pub backup: Option<PathBuf>,
}

/// Plan, and with `--apply` write, the unit that runs `serve`.
pub fn install(args: &InstallArgs) -> Result<Outcome> {
    let manager = args.at.manager();
    let home = args.at.home()?;
    let binary = match &args.binary {
        Some(binary) => std::path::absolute(binary)?,
        None => std::env::current_exe()?.canonicalize()?,
    };
    ensure!(
        is_executable(&binary),
        "{} is not an executable file",
        binary.display()
    );
    let env_file = std::path::absolute(&args.env_file)?;
    ensure!(
        env_file.is_file(),
        "--env-file {} does not exist; create it with the provider keys (docs/deployment.md)",
        env_file.display()
    );
    let serve = Serve {
        binary,
        env_file,
        data_dir: match &args.data_dir {
            Some(dir) => std::path::absolute(dir)?,
            None => home.join(DEFAULT_DATA_DIR),
        },
        surreal_bin: surreal_bin(args.surreal_bin.as_deref())?,
        outbox: match &args.outbox {
            Some(Some(path)) => Some(std::path::absolute(path)?),
            Some(None) => Some(home.join(crate::harness::outbox::DEFAULT_OUTBOX)),
            None => None,
        },
        port: args.port,
        llm_concurrency: args.llm_concurrency,
    };
    let text = render(manager, &serve, &home)?;
    let path = manager.unit_path(&home);
    let current = existing(&path)?;

    let mut out = Outcome {
        path: path.clone(),
        ..Outcome::default()
    };
    let argv: Vec<String> = serve.argv()?.iter().map(|a| sh(a)).collect();
    out.report.push(format!(
        "[plan] {} unit {}",
        manager_name(manager),
        path.display()
    ));
    out.report.push(format!("[plan] runs {}", argv.join(" ")));
    out.report.extend(private_note(&serve.env_file));
    if current.as_deref() == Some(text.as_str()) {
        out.report.push(format!(
            "[write] {} is up to date; nothing written",
            path.display()
        ));
    } else if !args.apply {
        out.report.push(format!(
            "[preview] {} {}:",
            if current.is_some() {
                "would replace (after a backup)"
            } else {
                "would write"
            },
            path.display()
        ));
        out.report.extend(text.lines().map(str::to_string));
        out.report
            .push("[result] preview only. Rerun with --apply to write it.".into());
        return Ok(out);
    } else {
        private_dir(&serve.data_dir)?;
        if manager == Manager::Launchd {
            private_dir(&home.join(LAUNCHD_LOGS))?;
        }
        if let Some(previous) = &current {
            let backup = back_up(&home, &path, previous)?;
            out.report.push(format!("[backup] {}", backup.display()));
            out.backup = Some(backup);
        }
        write_atomic(&path, &text)?;
        out.changed = true;
        out.report
            .push(format!("[write] {}; it holds no key", path.display()));
    }
    if args.start {
        return Ok(out);
    }
    out.report.push(format!(
        "[next] stop any daemon you started by hand on port {}, then run:",
        serve.port
    ));
    out.report.extend(
        start_commands(manager, &path, current.is_some())
            .into_iter()
            .map(|c| format!("  {c}")),
    );
    out.report.push(format!(
        "[next] check: curl -fsS http://127.0.0.1:{}/health",
        serve.port
    ));
    Ok(out)
}

/// Remove the unit file with `--apply`, after a backup. The data, outbox, logs and env file stay.
pub fn uninstall(at: &Target, apply: bool) -> Result<Outcome> {
    let manager = at.manager();
    let home = at.home()?;
    let path = manager.unit_path(&home);
    let mut out = Outcome {
        path: path.clone(),
        ..Outcome::default()
    };
    let Some(current) = existing(&path)? else {
        out.report.push(format!(
            "[uninstall] no unit at {}; nothing to remove",
            path.display()
        ));
        return Ok(out);
    };
    if !current.contains(MARKER) {
        out.report.push(format!(
            "[note] {} was not written by agent-memoryd service install; it is backed up before removal",
            path.display()
        ));
    }
    let stop = stop_command(manager);
    if manager == Manager::Systemd && enabled(&home) {
        out.report
            .push(format!("[next] disable and stop it first: {stop}"));
        ensure!(
            !apply,
            "{SYSTEMD_UNIT} is still enabled; run `{stop}`, then rerun with --apply"
        );
    }
    if !apply {
        if manager == Manager::Launchd {
            out.report
                .push(format!("[next] stop it first if it is loaded: {stop}"));
        }
        out.report.push(format!(
            "[result] preview only. Rerun with --apply to remove {} (after a backup).",
            path.display()
        ));
        return Ok(out);
    }
    let backup = back_up(&home, &path, &current)?;
    out.report.push(format!("[backup] {}", backup.display()));
    std::fs::remove_file(&path)?;
    out.changed = true;
    out.backup = Some(backup);
    out.report.push(format!(
        "[uninstall] removed {}; memory data, the outbox, logs and the env file are untouched",
        path.display()
    ));
    out.report.push(match manager {
        Manager::Launchd => format!("[next] if it is still loaded: {stop}"),
        Manager::Systemd => "[next] systemctl --user daemon-reload".into(),
    });
    Ok(out)
}

/// Report lines about the unit file; the port of its `serve` command, when one is found.
pub fn status_report(at: &Target) -> Result<(Vec<String>, Option<u16>)> {
    let manager = at.manager();
    let home = at.home()?;
    let path = manager.unit_path(&home);
    let mut report = Vec::new();
    let Some(text) = existing(&path)? else {
        report.push(format!(
            "[status] no {} unit at {}; see `agent-memoryd service install --help`",
            manager_name(manager),
            path.display()
        ));
        return Ok((report, None));
    };
    report.push(format!(
        "[status] {} unit {}{}",
        manager_name(manager),
        path.display(),
        if text.contains(MARKER) {
            ""
        } else {
            " (not written by agent-memoryd service install)"
        }
    ));
    let argv = unit_argv(manager, &text).unwrap_or_default();
    let quoted: Vec<String> = argv.iter().map(|a| sh(a)).collect();
    report.push(format!("[status] runs {}", quoted.join(" ")));
    if let Some(env_file) = flag_value(&argv, "--env-file") {
        let env_file = PathBuf::from(env_file);
        if env_file.is_file() {
            report.push(format!("[status] env file {} exists", env_file.display()));
            report.extend(private_note(&env_file));
        } else {
            report.push(format!(
                "[status] env file {} is missing; the daemon cannot start",
                env_file.display()
            ));
        }
    }
    if let Some(surreal) = flag_value(&argv, "--surreal-bin") {
        let state = if is_executable(Path::new(surreal)) {
            "is installed"
        } else {
            "is missing; the daemon cannot start"
        };
        report.push(format!("[status] SurrealDB {surreal} {state}"));
    }
    if manager == Manager::Systemd {
        report.push(format!(
            "[status] {}",
            if enabled(&home) {
                "enabled (starts with the user manager)"
            } else {
                "not enabled"
            }
        ));
    }
    report.push(format!("[next] inspect it: {}", inspect_command(manager)));
    Ok((report, serve_port(&argv)))
}

/// The port a unit's `serve` argv listens on: `--port`, else serve's default 8765.
fn serve_port(argv: &[String]) -> Option<u16> {
    match flag_value(argv, "--port") {
        Some(port) => port.parse().ok(),
        None => Some(8765),
    }
}

/// The unit file's settings as one JSON object, for scripts/install.sh; null where the unit sets
/// nothing. `managed` says whether `service install` wrote it.
pub fn status_json(at: &Target) -> Result<serde_json::Value> {
    let manager = at.manager();
    let path = manager.unit_path(&at.home()?);
    let text = existing(&path)?;
    let argv = text
        .as_deref()
        .and_then(|t| unit_argv(manager, t))
        .unwrap_or_default();
    let flag = |name| flag_value(&argv, name);
    let number = |value: Option<&str>| value.and_then(|v| v.parse::<u64>().ok());
    Ok(serde_json::json!({
        "manager": manager_id(manager),
        "unit": path,
        "installed": text.is_some(),
        "managed": text.as_deref().is_some_and(|t| t.contains(MARKER)),
        "binary": argv.first(),
        "envFile": flag("--env-file"),
        "dataDir": flag("--data-dir"),
        "surrealBin": flag("--surreal-bin"),
        "outbox": flag("--outbox"),
        "port": if text.is_some() { serve_port(&argv) } else { None },
        "llmConcurrency": number(flag("--llm-concurrency")),
    }))
}

/// The absolute path of the `surreal` the unit records: `given`, else the first `surreal` on PATH.
/// A service manager starts the daemon with a minimal PATH, so the unit cannot leave the lookup to it.
fn surreal_bin(given: Option<&Path>) -> Result<PathBuf> {
    let hint = memory_store::surreal::INSTALL_HINT;
    let found = match given {
        Some(path) => {
            let path = std::path::absolute(path)?;
            ensure!(
                is_executable(&path),
                "{} is not an executable SurrealDB `surreal`; {hint}",
                path.display()
            );
            path
        }
        None => std::env::var_os("PATH")
            .iter()
            .flat_map(std::env::split_paths)
            .map(|dir| dir.join("surreal"))
            .find(|candidate| candidate.is_absolute() && is_executable(candidate))
            .with_context(|| {
                format!("SurrealDB is not installed: no `surreal` on PATH; {hint}, or pass --surreal-bin")
            })?,
    };
    Ok(found)
}

/// Run a service command; prints its report and returns the process exit code.
pub async fn run(cmd: ServiceCmd) -> i32 {
    let result = match cmd {
        ServiceCmd::Install(args) => match install(&args) {
            // Starting prints each step as it runs, after the install report.
            Ok(out) if args.start => {
                for line in &out.report {
                    println!("{line}");
                }
                start(&args.at, &out.path, out.changed || args.restart, args.port)
                    .await
                    .map(|()| Vec::new())
            }
            result => result.map(|o| o.report),
        },
        ServiceCmd::Uninstall { at, apply } => uninstall(&at, apply).map(|o| o.report),
        ServiceCmd::Status { at, json: true } => status_json(&at).map(|v| vec![v.to_string()]),
        ServiceCmd::Status { at, json: false } => match status_report(&at) {
            Ok((mut report, port)) => {
                if let Some(port) = port {
                    report.push(health(port).await);
                }
                Ok(report)
            }
            Err(e) => Err(e),
        },
    };
    match result {
        Ok(report) => {
            for line in report {
                println!("{line}");
            }
            0
        }
        Err(e) => {
            eprintln!("[error] {e:#}");
            1
        }
    }
}

async fn health(port: u16) -> String {
    let url = format!("http://127.0.0.1:{port}/health");
    if answers(port).await {
        format!("[status] a daemon answers at {url}")
    } else {
        format!("[status] no daemon answers at {url}")
    }
}

/// Whether a daemon answers `/health` on the loopback `port`.
async fn answers(port: u16) -> bool {
    let answer = async {
        reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(3))
            .build()?
            .get(format!("http://127.0.0.1:{port}/health"))
            .send()
            .await?
            .error_for_status()
    };
    answer.await.is_ok()
}

/// `install --start`: load the unit at `path` in the login session, reloading (launchd) or
/// restarting (systemd) a loaded one when `reload`, then wait until a daemon answers on `port`.
/// Prints each step as it runs.
async fn start(at: &Target, path: &Path, reload: bool, port: u16) -> Result<()> {
    let manager = at.manager();
    let home = at.home()?;
    // launchctl and systemctl act on the session of the user running them, not on --home.
    if at.home.is_some() {
        let own = std::env::var_os("HOME").map(|own| same_dir(Path::new(&own), &home));
        ensure!(
            own == Some(true),
            "--start loads the unit into your own login session; it cannot start one under --home {}",
            home.display()
        );
    }
    let domain = match manager {
        Manager::Launchd => format!("gui/{}", user_id()?),
        Manager::Systemd => String::new(),
    };
    let loaded = is_loaded(manager, &domain);
    if !loaded && answers(port).await {
        bail!("a daemon this service did not start already answers on port {port}; stop it, then rerun");
    }
    for step in start_plan(manager, path, &domain, loaded, reload) {
        let quoted: Vec<String> = step.iter().map(|w| sh(w)).collect();
        println!("[start] {}", quoted.join(" "));
        execute(&step)?;
        if step[1] == "bootout" {
            // Bootstrapping while the old job still exits fails with an I/O error.
            let deadline = Instant::now() + UNLOAD_WAIT;
            while is_loaded(manager, &domain) {
                ensure!(
                    Instant::now() < deadline,
                    "launchd did not unload {LAUNCHD_LABEL} within {}s",
                    UNLOAD_WAIT.as_secs()
                );
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }
    let url = format!("http://127.0.0.1:{port}/health");
    let deadline = Instant::now() + START_WAIT;
    while !answers(port).await {
        ensure!(
            Instant::now() < deadline,
            "no daemon answers at {url} after {}s; inspect it with `{}` and read {}",
            START_WAIT.as_secs(),
            inspect_command(manager),
            match manager {
                Manager::Launchd => home
                    .join(LAUNCHD_LOGS)
                    .join("daemon.stderr.log")
                    .display()
                    .to_string(),
                Manager::Systemd => format!("`journalctl --user -u {SYSTEMD_UNIT} -n 50`"),
            }
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    println!("[start] the daemon answers at {url}");
    Ok(())
}

/// The launchctl/systemctl commands `--start` runs: launchd boots a loaded job out before
/// bootstrapping it again, systemd restarts it; nothing when it is loaded and `reload` is false.
fn start_plan(
    manager: Manager,
    path: &Path,
    domain: &str,
    loaded: bool,
    reload: bool,
) -> Vec<Vec<String>> {
    let command = |words: &[&str]| words.iter().map(|w| w.to_string()).collect::<Vec<_>>();
    let file = path.to_string_lossy();
    match manager {
        Manager::Launchd => {
            let mut plan = Vec::new();
            if loaded && reload {
                let job = format!("{domain}/{LAUNCHD_LABEL}");
                plan.push(command(&["launchctl", "bootout", &job]));
            }
            if !loaded || reload {
                plan.push(command(&["launchctl", "bootstrap", domain, &file]));
            }
            plan
        }
        Manager::Systemd if reload => vec![
            command(&["systemctl", "--user", "daemon-reload"]),
            command(&["systemctl", "--user", "enable", SYSTEMD_UNIT]),
            command(&["systemctl", "--user", "restart", SYSTEMD_UNIT]),
        ],
        Manager::Systemd if !loaded => vec![
            command(&["systemctl", "--user", "daemon-reload"]),
            command(&["systemctl", "--user", "enable", "--now", SYSTEMD_UNIT]),
        ],
        Manager::Systemd => Vec::new(),
    }
}

/// Whether the service is loaded (launchd) or active (systemd) in the login session.
fn is_loaded(manager: Manager, domain: &str) -> bool {
    let mut command = match manager {
        Manager::Launchd => {
            let mut c = std::process::Command::new("launchctl");
            c.arg("print").arg(format!("{domain}/{LAUNCHD_LABEL}"));
            c
        }
        Manager::Systemd => {
            let mut c = std::process::Command::new("systemctl");
            c.args(["--user", "is-active", "--quiet", SYSTEMD_UNIT]);
            c
        }
    };
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Run one `--start` step; a failure reports the tool's last line of error output.
fn execute(argv: &[String]) -> Result<()> {
    let output = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .output()
        .with_context(|| format!("cannot run {}", argv[0]))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let reason = stderr.lines().rev().find(|l| !l.trim().is_empty());
        bail!(
            "`{}` failed ({}): {}",
            argv.join(" "),
            output.status,
            reason.unwrap_or("no output").trim()
        );
    }
    Ok(())
}

/// The login user's numeric ID, which names the launchd `gui/<uid>` domain.
fn user_id() -> Result<String> {
    let output = std::process::Command::new("id")
        .arg("-u")
        .output()
        .context("cannot run id -u")?;
    let uid = String::from_utf8_lossy(&output.stdout).trim().to_string();
    ensure!(
        output.status.success() && !uid.is_empty() && uid.chars().all(|c| c.is_ascii_digit()),
        "id -u did not print a user ID"
    );
    Ok(uid)
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

fn manager_name(manager: Manager) -> &'static str {
    match manager {
        Manager::Launchd => "LaunchAgent",
        Manager::Systemd => "systemd user",
    }
}

fn manager_id(manager: Manager) -> &'static str {
    match manager {
        Manager::Launchd => "launchd",
        Manager::Systemd => "systemd",
    }
}

fn start_commands(manager: Manager, path: &Path, replaced: bool) -> Vec<String> {
    let file = sh(&path.to_string_lossy());
    match manager {
        Manager::Launchd => {
            let mut commands = vec![format!("plutil -lint {file}")];
            if replaced {
                commands.push(format!("{}   # if it is loaded", stop_command(manager)));
            }
            commands.push(format!("launchctl bootstrap \"gui/$(id -u)\" {file}"));
            commands
        }
        Manager::Systemd if replaced => vec![
            "systemctl --user daemon-reload".into(),
            format!("systemctl --user enable {SYSTEMD_UNIT}"),
            format!("systemctl --user restart {SYSTEMD_UNIT}"),
        ],
        Manager::Systemd => vec![
            "systemctl --user daemon-reload".into(),
            format!("systemctl --user enable --now {SYSTEMD_UNIT}"),
        ],
    }
}

fn stop_command(manager: Manager) -> String {
    match manager {
        Manager::Launchd => format!("launchctl bootout \"gui/$(id -u)/{LAUNCHD_LABEL}\""),
        Manager::Systemd => format!("systemctl --user disable --now {SYSTEMD_UNIT}"),
    }
}

fn inspect_command(manager: Manager) -> String {
    match manager {
        Manager::Launchd => format!("launchctl print \"gui/$(id -u)/{LAUNCHD_LABEL}\""),
        Manager::Systemd => format!("systemctl --user status {SYSTEMD_UNIT}"),
    }
}

fn flag_value<'a>(argv: &'a [String], flag: &str) -> Option<&'a str> {
    let at = argv.iter().position(|a| a == flag)?;
    argv.get(at + 1).map(String::as_str)
}

/// Whether the systemd unit is enabled for the user manager.
fn enabled(home: &Path) -> bool {
    std::fs::symlink_metadata(home.join(SYSTEMD_ENABLED).join(SYSTEMD_UNIT)).is_ok()
}

/// The unit file's text, or None when there is none. Refuses a symbolic link.
fn existing(path: &Path) -> Result<Option<String>> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("cannot inspect {}", path.display())),
        Ok(meta) if meta.file_type().is_symlink() => {
            bail!("{} is a symbolic link; change it by hand", path.display())
        }
        Ok(_) => {
            Ok(Some(std::fs::read_to_string(path).with_context(|| {
                format!("cannot read {}", path.display())
            })?))
        }
    }
}

/// A note when `path` (the env file) is accessible to other users.
#[cfg(unix)]
fn private_note(path: &Path) -> Option<String> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path).ok()?.permissions().mode();
    (mode & 0o077 != 0).then(|| {
        format!(
            "[note] {} is accessible to other users; run chmod 600 {}",
            path.display(),
            sh(&path.to_string_lossy())
        )
    })
}

#[cfg(not(unix))]
fn private_note(_: &Path) -> Option<String> {
    None
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// Create `dir` and missing parents 0700; an existing directory is left as it is.
fn private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
}

/// Copy the current unit file into the backup directory; returns the copy's path.
fn back_up(home: &Path, path: &Path, text: &str) -> Result<PathBuf> {
    let dir = home.join(BACKUPS);
    private_dir(&dir)?;
    let name = path
        .file_name()
        .expect("unit_path names a file")
        .to_string_lossy();
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    let suffix = &uuid::Uuid::new_v4().simple().to_string()[..8];
    let backup = dir.join(format!("{name}.{stamp}-{suffix}"));
    let mut file = std::fs::OpenOptions::new();
    file.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        file.mode(0o600);
    }
    use std::io::Write;
    file.open(&backup)
        .and_then(|mut f| f.write_all(text.as_bytes()))
        .with_context(|| format!("cannot write backup {}", backup.display()))?;
    Ok(backup)
}

/// Replace `path` with `text` (mode 0644) through a temporary file in its directory.
fn write_atomic(path: &Path, text: &str) -> Result<()> {
    let dir = path.parent().expect("unit_path is in a directory");
    std::fs::create_dir_all(dir)?;
    let name = path
        .file_name()
        .expect("unit_path names a file")
        .to_string_lossy();
    let temp = dir.join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4().simple()));
    let written = (|| -> std::io::Result<()> {
        std::fs::write(&temp, text)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o644))?;
        }
        std::fs::rename(&temp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written.with_context(|| format!("cannot write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Cli {
        #[command(subcommand)]
        cmd: ServiceCmd,
    }

    /// A temporary home holding executable stand-ins for agent-memoryd and surreal, and an env file.
    struct Home {
        dir: tempfile::TempDir,
    }

    impl Home {
        fn new(name: &str) -> Home {
            let dir = tempfile::Builder::new().prefix(name).tempdir().unwrap();
            let home = Home { dir };
            std::fs::create_dir_all(home.path().join("bin")).unwrap();
            std::fs::create_dir_all(home.path().join("checkout")).unwrap();
            std::fs::write(home.binary(), "#!/bin/sh\n").unwrap();
            std::fs::write(home.surreal(), "#!/bin/sh\n").unwrap();
            std::fs::write(home.env_file(), "QWEN_API_KEY=offline-test-value\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::Permissions::from_mode;
                std::fs::set_permissions(home.binary(), mode(0o755)).unwrap();
                std::fs::set_permissions(home.surreal(), mode(0o755)).unwrap();
                std::fs::set_permissions(home.env_file(), mode(0o600)).unwrap();
            }
            home
        }

        fn path(&self) -> &Path {
            self.dir.path()
        }

        fn binary(&self) -> PathBuf {
            self.path().join("bin/agent-memoryd")
        }

        fn surreal(&self) -> PathBuf {
            self.path().join("bin/surreal")
        }

        fn env_file(&self) -> PathBuf {
            self.path().join("checkout/.env")
        }

        fn parse(&self, manager: &str, args: &[&str]) -> ServiceCmd {
            let home = self.path().to_str().unwrap().to_string();
            let binary = self.binary().to_str().unwrap().to_string();
            let surreal = self.surreal().to_str().unwrap().to_string();
            let env_file = self.env_file().to_str().unwrap().to_string();
            let mut argv = vec![
                "agent-memoryd",
                args[0],
                "--manager",
                manager,
                "--home",
                home.as_str(),
            ];
            if args[0] == "install" {
                argv.extend(["--binary", binary.as_str(), "--env-file", env_file.as_str()]);
                argv.extend(["--surreal-bin", surreal.as_str()]);
            }
            argv.extend(&args[1..]);
            Cli::try_parse_from(argv).unwrap().cmd
        }

        fn install(&self, manager: &str, args: &[&str]) -> Result<Outcome> {
            let mut all = vec!["install"];
            all.extend(args);
            match self.parse(manager, &all) {
                ServiceCmd::Install(args) => install(&args),
                _ => unreachable!(),
            }
        }

        fn uninstall(&self, manager: &str, args: &[&str]) -> Result<Outcome> {
            let mut all = vec!["uninstall"];
            all.extend(args);
            match self.parse(manager, &all) {
                ServiceCmd::Uninstall { at, apply } => uninstall(&at, apply),
                _ => unreachable!(),
            }
        }

        fn backups(&self) -> Vec<PathBuf> {
            match std::fs::read_dir(self.path().join(BACKUPS)) {
                Ok(dir) => dir.map(|e| e.unwrap().path()).collect(),
                Err(_) => Vec::new(),
            }
        }
    }

    #[test]
    fn previews_then_writes_a_launch_agent_without_the_key() {
        let home = Home::new("svc-launchd");
        let unit = home
            .path()
            .join("Library/LaunchAgents/local.agent-memory-daemon.plist");

        let preview = home.install("launchd", &["--outbox"]).unwrap();
        assert_eq!(preview.path, unit);
        assert!(!preview.changed && !unit.exists());
        assert!(!home.path().join(DEFAULT_DATA_DIR).exists());
        let report = preview.report.join("\n");
        assert!(report.contains("[result] preview only"), "{report}");
        assert!(report.contains("<string>serve</string>"), "{report}");

        let written = home.install("launchd", &["--outbox", "--apply"]).unwrap();
        assert!(written.changed && written.backup.is_none());
        let text = std::fs::read_to_string(&unit).unwrap();
        assert!(!text.contains("offline-test-value"));
        assert!(text.contains(MARKER));
        let argv = unit_argv(Manager::Launchd, &text).unwrap();
        let p = |rel: &str| home.path().join(rel).to_str().unwrap().to_string();
        assert_eq!(
            argv,
            [
                p("bin/agent-memoryd"),
                "serve".into(),
                "--env-file".into(),
                p("checkout/.env"),
                "--data-dir".into(),
                p(DEFAULT_DATA_DIR),
                "--surreal-bin".into(),
                p("bin/surreal"),
                "--port".into(),
                "8765".into(),
                "--llm-concurrency".into(),
                "1".into(),
                "--outbox".into(),
                p(crate::harness::outbox::DEFAULT_OUTBOX),
            ]
        );
        assert!(text.contains(&p("Library/Logs/agent-memory-service/daemon.stderr.log")));
        assert!(home.path().join(LAUNCHD_LOGS).is_dir());
        assert!(home.path().join(DEFAULT_DATA_DIR).is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&unit), 0o644);
            assert_eq!(mode(&home.path().join(DEFAULT_DATA_DIR)), 0o700);
        }
        let next = written.report.join("\n");
        assert!(
            next.contains("launchctl bootstrap \"gui/$(id -u)\""),
            "{next}"
        );
        assert!(!next.contains("bootout"), "{next}");

        // A rerun with the same options writes nothing.
        let again = home.install("launchd", &["--outbox", "--apply"]).unwrap();
        assert!(!again.changed && again.backup.is_none());
        assert!(home.backups().is_empty());
    }

    #[test]
    fn replacing_a_systemd_unit_backs_it_up_and_keeps_the_key_out() {
        let home = Home::new("svc-systemd");
        let unit = home
            .path()
            .join(".config/systemd/user/agent-memory.service");
        std::fs::create_dir_all(unit.parent().unwrap()).unwrap();
        let hand_made = "[Service]\nExecStart=/usr/bin/python3 scripts/serve.py --max-calls 60\n";
        std::fs::write(&unit, hand_made).unwrap();

        let preview = home.install("systemd", &["--port", "9001"]).unwrap();
        assert!(!preview.changed);
        assert_eq!(std::fs::read_to_string(&unit).unwrap(), hand_made);

        let written = home
            .install("systemd", &["--port", "9001", "--apply"])
            .unwrap();
        let backup = written.backup.unwrap();
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), hand_made);
        assert_eq!(home.backups(), [backup]);
        let text = std::fs::read_to_string(&unit).unwrap();
        assert!(!text.contains("offline-test-value"));
        assert!(text.contains("\nWantedBy=default.target\n"));
        let argv = unit_argv(Manager::Systemd, &text).unwrap();
        assert_eq!(flag_value(&argv, "--port"), Some("9001"));
        assert_eq!(flag_value(&argv, "--llm-concurrency"), Some("1"));
        assert_eq!(flag_value(&argv, "--outbox"), None);
        let next = written.report.join("\n");
        assert!(
            next.contains("systemctl --user restart agent-memory.service"),
            "{next}"
        );
    }

    #[test]
    fn quotes_paths_with_spaces_and_special_characters() {
        let home = Home::new("svc quote &%$ 'x'");
        for manager in ["launchd", "systemd"] {
            home.install(manager, &["--outbox", "--apply"]).unwrap();
            let m = if manager == "launchd" {
                Manager::Launchd
            } else {
                Manager::Systemd
            };
            let text = std::fs::read_to_string(m.unit_path(home.path())).unwrap();
            let argv = unit_argv(m, &text).unwrap();
            assert_eq!(argv[0], home.binary().to_str().unwrap(), "{text}");
            assert_eq!(
                flag_value(&argv, "--env-file"),
                home.env_file().to_str(),
                "{text}"
            );
            if m == Manager::Systemd {
                assert!(text.contains("%%"), "{text}");
                assert!(text.contains("$$"), "{text}");
            } else {
                assert!(text.contains("&amp;"), "{text}");
            }
        }
    }

    #[test]
    fn refuses_a_missing_env_file_and_a_symlinked_unit() {
        let home = Home::new("svc-refuse");
        std::fs::remove_file(home.env_file()).unwrap();
        let e = home.install("systemd", &["--apply"]).unwrap_err();
        assert!(e.to_string().contains("--env-file"), "{e}");
        assert!(!Manager::Systemd.unit_path(home.path()).exists());

        let home = Home::new("svc-no-surreal");
        std::fs::remove_file(home.surreal()).unwrap();
        let e = home.install("systemd", &["--apply"]).unwrap_err();
        assert!(e.to_string().contains("not an executable SurrealDB"), "{e}");
        assert!(e.to_string().contains("install.surrealdb.com"), "{e}");
        assert!(!Manager::Systemd.unit_path(home.path()).exists());

        #[cfg(unix)]
        {
            let home = Home::new("svc-link");
            let unit = Manager::Systemd.unit_path(home.path());
            std::fs::create_dir_all(unit.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(home.env_file(), &unit).unwrap();
            let e = home.install("systemd", &["--apply"]).unwrap_err();
            assert!(e.to_string().contains("symbolic link"), "{e}");
            assert!(std::fs::symlink_metadata(&unit)
                .unwrap()
                .file_type()
                .is_symlink());
        }
    }

    #[test]
    fn uninstall_waits_for_disable_then_backs_up_and_removes() {
        let home = Home::new("svc-uninstall");
        let nothing = home.uninstall("systemd", &["--apply"]).unwrap();
        assert!(!nothing.changed);

        home.install("systemd", &["--apply"]).unwrap();
        let unit = Manager::Systemd.unit_path(home.path());
        let text = std::fs::read_to_string(&unit).unwrap();
        #[cfg(unix)]
        {
            let wants = home.path().join(SYSTEMD_ENABLED);
            std::fs::create_dir_all(&wants).unwrap();
            std::os::unix::fs::symlink(&unit, wants.join(SYSTEMD_UNIT)).unwrap();
            let e = home.uninstall("systemd", &["--apply"]).unwrap_err();
            assert!(e.to_string().contains("disable --now"), "{e}");
            assert!(unit.exists());
            std::fs::remove_file(wants.join(SYSTEMD_UNIT)).unwrap();
        }
        let preview = home.uninstall("systemd", &[]).unwrap();
        assert!(!preview.changed && unit.exists());

        let removed = home.uninstall("systemd", &["--apply"]).unwrap();
        assert!(removed.changed && !unit.exists());
        assert_eq!(
            std::fs::read_to_string(removed.backup.unwrap()).unwrap(),
            text
        );
        assert!(home.env_file().exists());
        assert!(home.path().join(DEFAULT_DATA_DIR).is_dir());
    }

    #[test]
    fn status_reads_the_command_back() {
        let home = Home::new("svc-status");
        let at = Target {
            manager: Some(Manager::Launchd),
            home: Some(home.path().to_path_buf()),
        };
        let (report, port) = status_report(&at).unwrap();
        assert!(report[0].contains("no LaunchAgent unit"), "{report:?}");
        assert_eq!(port, None);

        home.install("launchd", &["--port", "9002", "--apply"])
            .unwrap();
        let (report, port) = status_report(&at).unwrap();
        assert_eq!(port, Some(9002));
        let report = report.join("\n");
        assert!(
            report.contains("env file") && report.contains("exists"),
            "{report}"
        );
        assert!(report.contains("launchctl print"), "{report}");
        assert!(
            report.contains("[status] SurrealDB ") && report.contains(" is installed"),
            "{report}"
        );
        std::fs::remove_file(home.surreal()).unwrap();
        let (report, _) = status_report(&at).unwrap();
        assert!(
            report
                .join("\n")
                .contains("is missing; the daemon cannot start"),
            "{report:?}"
        );
    }

    #[test]
    fn status_json_reads_back_every_option_install_wrote() {
        let home = Home::new("svc-json");
        let data = home.path().join("custom data");
        let data = data.to_str().unwrap();
        for (manager, m) in [("launchd", Manager::Launchd), ("systemd", Manager::Systemd)] {
            let status = |home: &Home| match home.parse(manager, &["status", "--json"]) {
                ServiceCmd::Status { at, json: true } => status_json(&at).unwrap(),
                _ => unreachable!(),
            };
            let none = status(&home);
            assert_eq!(none["installed"], false);
            assert_eq!(none["port"], serde_json::Value::Null);
            assert_eq!(none["unit"], m.unit_path(home.path()).to_str().unwrap());

            let options = [
                "--data-dir",
                data,
                "--outbox",
                "--port",
                "9003",
                "--llm-concurrency",
                "2",
                "--apply",
            ];
            home.install(manager, &options).unwrap();
            let json = status(&home);
            let p = |rel: &str| home.path().join(rel).to_str().unwrap().to_string();
            assert_eq!(json["manager"], manager);
            assert_eq!(json["installed"], true);
            assert_eq!(json["managed"], true);
            assert_eq!(json["binary"], p("bin/agent-memoryd"));
            assert_eq!(json["envFile"], p("checkout/.env"));
            assert_eq!(json["dataDir"], data);
            assert_eq!(json["surrealBin"], p("bin/surreal"));
            assert_eq!(json["outbox"], p(crate::harness::outbox::DEFAULT_OUTBOX));
            assert_eq!(json["port"], 9003);
            assert_eq!(json["llmConcurrency"], 2);

            home.install(manager, &["--apply"]).unwrap();
            let json = status(&home);
            assert_eq!(json["outbox"], serde_json::Value::Null);
            assert_eq!(json["port"], 8765);
        }
        let unit = Manager::Systemd.unit_path(home.path());
        std::fs::write(
            &unit,
            "[Service]\nExecStart=/usr/bin/python3 scripts/serve.py\n",
        )
        .unwrap();
        let at = Target {
            manager: Some(Manager::Systemd),
            home: Some(home.path().to_path_buf()),
        };
        let hand_made = status_json(&at).unwrap();
        assert_eq!(hand_made["managed"], false);
        assert_eq!(hand_made["envFile"], serde_json::Value::Null);
    }

    #[test]
    fn start_loads_reloads_or_leaves_the_service() {
        let plist = Path::new("/h/Library/LaunchAgents/local.agent-memory-daemon.plist");
        let words = |plan: Vec<Vec<String>>| -> Vec<String> {
            plan.iter().map(|step| step.join(" ")).collect()
        };
        let launchd = |loaded, reload| {
            words(start_plan(
                Manager::Launchd,
                plist,
                "gui/501",
                loaded,
                reload,
            ))
        };
        let bootstrap = format!("launchctl bootstrap gui/501 {}", plist.display());
        let bootout = "launchctl bootout gui/501/local.agent-memory-daemon".to_string();
        assert_eq!(launchd(false, false), [bootstrap.clone()]);
        assert_eq!(launchd(false, true), [bootstrap.clone()]);
        assert_eq!(launchd(true, true), [bootout, bootstrap]);
        assert!(launchd(true, false).is_empty());

        let unit = Path::new("/h/.config/systemd/user/agent-memory.service");
        let systemd =
            |loaded, reload| words(start_plan(Manager::Systemd, unit, "", loaded, reload));
        let reloaded = [
            "systemctl --user daemon-reload",
            "systemctl --user enable agent-memory.service",
            "systemctl --user restart agent-memory.service",
        ];
        assert_eq!(systemd(true, true), reloaded);
        assert_eq!(systemd(false, true), reloaded);
        assert_eq!(
            systemd(false, false),
            [
                "systemctl --user daemon-reload",
                "systemctl --user enable --now agent-memory.service"
            ]
        );
        assert!(systemd(true, false).is_empty());
    }

    #[test]
    fn start_needs_apply_and_replaces_the_printed_commands() {
        let parse = |args: &[&str]| {
            let mut argv = vec!["t", "install", "--env-file", "x"];
            argv.extend(args);
            Cli::try_parse_from(argv)
        };
        assert!(parse(&["--start"]).is_err());
        assert!(parse(&["--apply", "--restart"]).is_err());
        assert!(parse(&["--apply", "--start", "--restart"]).is_ok());
        assert!(parse(&["--llm-concurrency", "0"]).is_err());

        let home = Home::new("svc-start");
        let out = home.install("systemd", &["--apply", "--start"]).unwrap();
        assert!(out.changed);
        let report = out.report.join("\n");
        assert!(!report.contains("[next]"), "{report}");
    }

    #[tokio::test]
    async fn start_refuses_a_unit_under_another_home() {
        let home = Home::new("svc-other-home");
        let at = Target {
            manager: Some(Manager::Systemd),
            home: Some(home.path().to_path_buf()),
        };
        let unit = Manager::Systemd.unit_path(home.path());
        let e = start(&at, &unit, true, 9).await.unwrap_err();
        assert!(e.to_string().contains("--home"), "{e}");
    }

    #[test]
    fn systemd_words_round_trip() {
        for word in [
            "/plain/path",
            "a b",
            "100%",
            "$HOME",
            "q\"uote",
            "back\\slash",
            "it's",
        ] {
            assert_eq!(systemd_words(&systemd_quote(word)), [word]);
        }
        assert_eq!(sh("/a b/it's"), r"'/a b/it'\''s'");
        assert_eq!(sh("/plain/path"), "/plain/path");
    }
}
