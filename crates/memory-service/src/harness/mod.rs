//! Agent harness commands: the dialogue collector and automatic recall that agent hosts run as
//! hooks, and the uploader that submits captured dialogue to the daemon. They keep the files,
//! schemas and hook contracts of the Python hooks that earlier installers wrote, so an existing
//! outbox and recall record stay usable. See docs/mcp-harness.md. `import` reads the memories a host already keeps into a reviewable plan
//! and drives the daemon's import runs (docs/design/memory-import.md).
pub mod import;
pub mod outbox;
pub mod recall;
pub mod uploader;

use clap::Subcommand;
use serde_json::{json, Value};
use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Daemon response bytes read before giving up.
const MAX_RESPONSE: usize = 1 << 20;
/// Admin response bytes read before giving up: an import run lists every proposal.
const MAX_ADMIN_RESPONSE: usize = 32 << 20;
/// Bytes of a refused admin call read for the daemon's `message`.
const MAX_ERROR_BODY: usize = 64 << 10;

#[derive(Subcommand, Debug)]
pub enum HookCmd {
    /// Capture UserPromptSubmit/Stop/StopFailure/SessionEnd hook JSON from stdin; prints {}
    Capture {
        #[command(flatten)]
        at: outbox::Target,
        /// Native turn id field (default: turn_id for codex, else prompt_id); none pairs Stop
        /// with the open prompt for hosts that send no id
        #[arg(long, value_enum)]
        turn_id_field: Option<outbox::TurnIdField>,
    },
    /// Capture the Codex agent-turn-complete JSON argument that `notify` appends. After `--`, the
    /// notify program this one wraps, which also runs with the event appended
    CodexNotify {
        #[command(flatten)]
        at: outbox::Target,
        /// [-- PROGRAM [ARGS...]] EVENT: the wrapped notify program, then the event JSON Codex appends
        #[arg(required = true, num_args = 1.., trailing_var_arg = true, allow_hyphen_values = true,
              value_name = "EVENT")]
        args: Vec<String>,
    },
    /// Inject shared memory on SessionStart and UserPromptSubmit; always prints one JSON object and exits 0
    Recall(recall::RecallArgs),
}

#[derive(Subcommand, Debug)]
pub enum OutboxCmd {
    /// Local receipts and errors, without chat text or network access
    Status {
        #[arg(long, default_value_os_t = outbox::default_path())]
        outbox: PathBuf,
    },
    /// Upload and poll the outbox; not needed while the daemon runs with --outbox
    Drain {
        #[arg(long, default_value_os_t = outbox::default_path())]
        outbox: PathBuf,
        #[arg(long, default_value = "http://127.0.0.1:8765")]
        url: String,
        /// Seconds per daemon request
        #[arg(long, default_value_t = 5.0, value_parser = positive_seconds)]
        timeout: f64,
        /// Seconds between passes
        #[arg(long, default_value_t = 5.0, value_parser = positive_seconds)]
        interval: f64,
        /// One upload/poll pass; prints the status counts and exits 1 on errors
        #[arg(long)]
        once: bool,
    },
    /// Enqueue normalized add_memories dialogue JSON from stdin
    Enqueue {
        #[arg(long, default_value_os_t = outbox::default_path())]
        outbox: PathBuf,
        #[arg(long, default_value = "codex")]
        platform: String,
    },
    /// Delete completed, rejected and cancelled jobs captured more than AGE ago; never pending or accepted
    /// ones, so it is safe while an uploader runs. Prints the count per status
    Prune {
        #[arg(long, default_value_os_t = outbox::default_path())]
        outbox: PathBuf,
        /// Whole days (30d) or hours (12h)
        #[arg(long = "older-than", value_name = "AGE", value_parser = cutoff)]
        before: String,
        /// Also delete failed jobs, the evidence of a failed formation
        #[arg(long)]
        include_failed: bool,
    },
}

/// Run a hook command; the returned process exit code is never 2, which would block a prompt.
pub async fn hook(cmd: HookCmd) -> i32 {
    match cmd {
        HookCmd::Recall(args) => recall::run(args).await,
        HookCmd::Capture { at, turn_id_field } => adapter(|| {
            let event = stdin_json()?;
            outbox::Outbox::open(&at.outbox)?.capture_hook(
                &event,
                &at.project,
                &at.platform,
                turn_id_field,
            )?;
            // Never inject raw chat, block Stop or request a continuation.
            println!("{{}}");
            Ok(0)
        }),
        HookCmd::CodexNotify { at, mut args } => {
            // clap requires one value: the event Codex appends comes last.
            let event = args.pop().expect("clap requires the event");
            // The wrapped program starts first, so a capture failure never keeps the event from it.
            let wrapped = (!args.is_empty()).then(|| forward(&args, &event));
            let captured = adapter(|| {
                let event: Value = serde_json::from_str(&event).map_err(|_| Error::Json)?;
                outbox::Outbox::open(&at.outbox)?.capture_notify(
                    &event,
                    &at.project,
                    &at.platform,
                )?;
                Ok(0)
            });
            let forwarded = match wrapped.map(|child| child.and_then(settle)) {
                None | Some(Ok(None)) => true,
                Some(Ok(Some(status))) if status.success() => true,
                Some(Ok(Some(status))) => {
                    eprintln!("memory adapter: wrapped notify program exited with {status}");
                    false
                }
                Some(Err(e)) => {
                    eprintln!(
                        "memory adapter: wrapped notify program failed: OSError ({:?})",
                        e.kind()
                    );
                    false
                }
            };
            if captured == 0 && forwarded {
                0
            } else {
                1
            }
        }
    }
}

/// Start the notify program a `codex-notify` command wraps, with the event appended as Codex would.
fn forward(program: &[String], event: &str) -> std::io::Result<std::process::Child> {
    std::process::Command::new(&program[0])
        .args(&program[1..])
        .arg(event)
        .spawn()
}

/// How long `codex-notify` waits for the program it wraps before leaving it running.
const WRAPPED_WAIT: Duration = Duration::from_secs(10);

/// The wrapped program's exit status, or None when it still runs after WRAPPED_WAIT; it is
/// never killed, so a slow program finishes on its own while the collector exits.
fn settle(mut child: std::process::Child) -> std::io::Result<Option<std::process::ExitStatus>> {
    let deadline = std::time::Instant::now() + WRAPPED_WAIT;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if std::time::Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Run an outbox command.
pub async fn outbox(cmd: OutboxCmd) -> i32 {
    match cmd {
        OutboxCmd::Status { outbox } => adapter(|| {
            let rows = outbox::Outbox::open(&outbox)?.status()?;
            println!("{}", Value::Array(rows));
            Ok(0)
        }),
        OutboxCmd::Enqueue { outbox, platform } => adapter(|| {
            let body = stdin_json()?;
            outbox::Outbox::open(&outbox)?.enqueue(&platform, &body)?;
            Ok(0)
        }),
        OutboxCmd::Prune {
            outbox,
            before,
            include_failed,
        } => adapter(|| {
            let deleted = outbox::Outbox::open(&outbox)?.prune(&before, include_failed)?;
            println!("{}", json!({"deleted": deleted, "before": before}));
            Ok(0)
        }),
        OutboxCmd::Drain {
            outbox,
            url,
            timeout,
            interval,
            once,
        } => match outbox::drain(
            &outbox,
            &url,
            Duration::from_secs_f64(timeout),
            Duration::from_secs_f64(interval),
            once,
        )
        .await
        {
            Ok(code) => code,
            Err(e) => {
                eprintln!("memory adapter failed: {e}");
                1
            }
        },
    }
}

fn adapter(run: impl FnOnce() -> Result<i32, Error>) -> i32 {
    run().unwrap_or_else(|e| {
        // Field errors contain names only; never echo payloads or HTTP bodies.
        eprintln!("memory adapter failed: {e}");
        1
    })
}

fn stdin_json() -> Result<Value, Error> {
    let mut text = String::new();
    std::io::stdin().read_to_string(&mut text)?;
    serde_json::from_str(&text).map_err(|_| Error::Json)
}

fn positive_seconds(value: &str) -> Result<f64, String> {
    match value.parse::<f64>() {
        Ok(v) if v.is_finite() && v > 0.0 => Ok(v),
        _ => Err("must be a positive number of seconds".into()),
    }
}

/// An age such as `30d` or `12h`, as the time that long ago in `now()`'s form.
fn cutoff(value: &str) -> Result<String, String> {
    let count = |n: &str| n.parse::<u32>().ok().map(i64::from);
    let age = match (value.strip_suffix('d'), value.strip_suffix('h')) {
        (Some(days), _) => count(days).and_then(chrono::TimeDelta::try_days),
        (_, Some(hours)) => count(hours).and_then(chrono::TimeDelta::try_hours),
        _ => None,
    };
    age.and_then(|age| chrono::Utc::now().checked_sub_signed(age))
        .map(stamp)
        .ok_or_else(|| "must be a whole number of days (30d) or hours (12h)".into())
}

fn positive(value: &str) -> Result<usize, String> {
    match value.parse::<usize>() {
        Ok(n) if n > 0 => Ok(n),
        _ => Err("must be a positive integer".into()),
    }
}

/// A daemon URL on this machine, without credentials; the trailing slash is dropped.
fn loopback_url(value: &str) -> Result<String, String> {
    let url = reqwest::Url::parse(value).map_err(|e| e.to_string())?;
    if !matches!(url.scheme(), "http" | "https")
        || !loopback(&url)
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("must be a daemon URL on loopback".into());
    }
    Ok(value.trim_end_matches('/').to_string())
}

/// A refused or failed outbox/recall-record operation. Display names fields or error kinds only.
#[derive(Debug)]
pub enum Error {
    Invalid(String),
    Json,
    Sqlite(rusqlite::Error),
    Io(std::io::Error),
}

impl Error {
    /// Error type name for hook stderr and the outbox `error` column.
    pub fn kind(&self) -> &'static str {
        match self {
            Error::Invalid(_) => "ValueError",
            Error::Json => "JSONDecodeError",
            Error::Sqlite(_) => "SQLiteError",
            Error::Io(_) => "OSError",
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Invalid(message) => write!(f, "ValueError: {message}"),
            Error::Sqlite(rusqlite::Error::SqliteFailure(e, _)) => {
                write!(f, "SQLiteError ({:?})", e.code)
            }
            Error::Io(e) => write!(f, "OSError ({:?})", e.kind()),
            other => f.write_str(other.kind()),
        }
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error::Sqlite(e)
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

pub fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid(message.into())
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// `~/...` expanded as Python's `Path.expanduser`.
pub fn expand(path: &Path) -> PathBuf {
    match path.strip_prefix("~") {
        Ok(rest) => home().join(rest),
        Err(_) => path.to_path_buf(),
    }
}

/// UTC now in Python's `datetime.now(timezone.utc).isoformat()` form.
pub fn now() -> String {
    stamp(chrono::Utc::now())
}

/// `time` in [`now`]'s form; outbox times in this form order as strings.
fn stamp(time: chrono::DateTime<chrono::Utc>) -> String {
    time.to_rfc3339_opts(chrono::SecondsFormat::Micros, false)
}

/// Create `path` private: new parent directories 0700, the file 0600 (an existing one is narrowed).
/// SQLite gives its -wal and -shm files the database file's permissions.
pub fn private_file(path: &Path) -> std::io::Result<()> {
    let mut dirs = std::fs::DirBuilder::new();
    let mut file = std::fs::OpenOptions::new();
    file.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        dirs.mode(0o700);
        file.mode(0o600);
    }
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        dirs.recursive(true).create(parent)?;
    }
    file.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Whether a daemon URL names this machine; such requests never go through a configured proxy.
pub fn loopback(url: &reqwest::Url) -> bool {
    matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
}

/// Why a daemon call failed. A status is the daemon's answer; the rest are client-side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallError {
    Status(u16),
    Timeout,
    Connection,
    Invalid,
}

impl CallError {
    pub fn kind(self) -> &'static str {
        match self {
            CallError::Status(_) => "HTTPError",
            CallError::Timeout => "TimeoutError",
            CallError::Connection => "ConnectionError",
            CallError::Invalid => "ValueError",
        }
    }
}

impl From<reqwest::Error> for CallError {
    fn from(e: reqwest::Error) -> Self {
        if e.is_timeout() {
            CallError::Timeout
        } else if e.is_builder() {
            CallError::Invalid
        } else {
            CallError::Connection
        }
    }
}

/// A failed admin call: why it failed, and the daemon's own `message` when it refused the call with one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminError {
    pub error: CallError,
    pub message: Option<String>,
}

impl From<CallError> for AdminError {
    fn from(error: CallError) -> Self {
        Self {
            error,
            message: None,
        }
    }
}

/// JSON client for the daemon's public interface (`POST {base}/v1/{name}`).
pub struct Daemon {
    client: reqwest::Client,
    base: String,
}

impl Daemon {
    pub fn new(base: &str, timeout: Duration) -> Result<Self, CallError> {
        let base = base.trim_end_matches('/').to_string();
        let url = reqwest::Url::parse(&base).map_err(|_| CallError::Invalid)?;
        let mut builder = reqwest::Client::builder().timeout(timeout);
        if loopback(&url) {
            builder = builder.no_proxy();
        }
        Ok(Self {
            client: builder.build()?,
            base,
        })
    }

    /// One call; the response must be a JSON object of at most 1 MiB.
    pub async fn post(&self, name: &str, body: &Value, platform: &str) -> Result<Value, CallError> {
        let url = format!("{}/v1/{name}", self.base);
        let response = self.send(url, body, platform).await?;
        if !response.status().is_success() {
            return Err(CallError::Status(response.status().as_u16()));
        }
        json_object(response, MAX_RESPONSE).await
    }

    /// One admin call (`POST {base}/admin/{name}`); the response must be a JSON object of at most 32 MiB.
    /// A refused call keeps the daemon's `message`, which says what to change.
    pub async fn admin(
        &self,
        name: &str,
        body: &Value,
        platform: &str,
    ) -> Result<Value, AdminError> {
        let url = format!("{}/admin/{name}", self.base);
        let response = self.send(url, body, platform).await?;
        if !response.status().is_success() {
            let error = CallError::Status(response.status().as_u16());
            let message = json_object(response, MAX_ERROR_BODY)
                .await
                .ok()
                .and_then(|body| body.get("message")?.as_str().map(str::to_string));
            return Err(AdminError { error, message });
        }
        Ok(json_object(response, MAX_ADMIN_RESPONSE).await?)
    }

    async fn send(
        &self,
        url: String,
        body: &Value,
        platform: &str,
    ) -> Result<reqwest::Response, CallError> {
        Ok(self
            .client
            .post(url)
            .header("X-Memory-Platform", platform)
            .json(body)
            .send()
            .await?)
    }
}

/// How long an operator command waits for the daemon: proposing a consolidation run makes one generation call
/// per cluster.
const ADMIN_COMMAND_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// One admin call of an operator command (`agent-memoryd reindex`, `consolidate`) on the daemon at `url`, with
/// the client of every other daemon call, so a loopback daemon is reached without a configured proxy. A refusal
/// keeps the daemon's message.
pub async fn admin_command(url: &str, name: &str, body: &Value) -> anyhow::Result<Value> {
    let daemon = Daemon::new(url, ADMIN_COMMAND_TIMEOUT)
        .map_err(|e| anyhow::anyhow!("--url {url}: {}", e.kind()))?;
    daemon
        .admin(name, body, "cli")
        .await
        .map_err(|AdminError { error, message }| {
            let reason = match error {
                CallError::Status(code) => format!("HTTP {code}"),
                other => other.kind().to_string(),
            };
            match message {
                Some(message) => anyhow::anyhow!("{reason}: {message}"),
                None => anyhow::anyhow!("{reason}"),
            }
        })
}

/// The body of `response` as a JSON object of at most `max` bytes.
async fn json_object(mut response: reqwest::Response, max: usize) -> Result<Value, CallError> {
    let mut data = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        data.extend_from_slice(&chunk);
        if data.len() > max {
            return Err(CallError::Invalid);
        }
    }
    match serde_json::from_slice(&data) {
        Ok(value @ Value::Object(_)) => Ok(value),
        _ => Err(CallError::Invalid),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Set in the child process that checks `admin_command` under a configured proxy.
    const PROXY_CHILD: &str = "MEMORY_ADMIN_PROXY_TEST";

    /// A daemon's admin routes on a loopback port: `reindex` is scheduled, anything else refused with a message.
    async fn admin_stub() -> String {
        use axum::extract::Path as UrlPath;
        use axum::http::StatusCode;
        use axum::routing::post;
        use axum::{Json, Router};
        let app = Router::new().route(
            "/admin/{name}",
            post(|UrlPath(name): UrlPath<String>| async move {
                if name == "reindex" {
                    (StatusCode::OK, Json(json!({"scheduled": true})))
                } else {
                    let refusal = json!({"code": "invalid_argument", "message": "consolidationEnabled is off"});
                    (StatusCode::BAD_REQUEST, Json(refusal))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        url
    }

    #[tokio::test]
    async fn admin_commands_return_the_answer_or_the_daemons_refusal() {
        let url = admin_stub().await;
        let scheduled = admin_command(&url, "reindex", &json!({})).await.unwrap();
        assert_eq!(scheduled, json!({"scheduled": true}));
        let refused = admin_command(
            &url,
            "consolidate",
            &json!({"project": "p", "dryRun": true}),
        )
        .await
        .unwrap_err();
        assert_eq!(refused.to_string(), "HTTP 400: consolidationEnabled is off");
        let closed = admin_command("http://127.0.0.1:1", "reindex", &json!({}))
            .await
            .unwrap_err();
        assert_eq!(closed.to_string(), "ConnectionError");
    }

    /// Operator commands reach a loopback daemon although the environment names a proxy. Checked in a child
    /// process whose environment names a proxy that refuses every connection, so no other test sees it.
    #[test]
    fn admin_commands_reach_a_loopback_daemon_past_a_configured_proxy() {
        let name = "harness::tests::admin_commands_reach_a_loopback_daemon_past_a_configured_proxy";
        if std::env::var_os(PROXY_CHILD).is_none() {
            let dead = "http://127.0.0.1:1";
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([name, "--exact"])
                .env(PROXY_CHILD, "1")
                .env("HTTP_PROXY", dead)
                .env("http_proxy", dead)
                .env("ALL_PROXY", dead)
                .env("all_proxy", dead)
                .env_remove("NO_PROXY")
                .env_remove("no_proxy")
                .stdin(std::process::Stdio::null())
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success() && stdout.contains("1 passed"),
                "{stdout}{}",
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let url = admin_stub().await;
            // The proxy is in effect: a client that does not bypass it cannot reach the daemon.
            let proxied = reqwest::Client::new()
                .post(format!("{url}/admin/reindex"))
                .json(&json!({}))
                .send()
                .await;
            assert!(proxied.is_err(), "{proxied:?}");
            let scheduled = admin_command(&url, "reindex", &json!({})).await.unwrap();
            assert_eq!(scheduled, json!({"scheduled": true}));
        });
    }

    #[test]
    fn only_loopback_hosts_bypass_proxies() {
        for url in [
            "http://127.0.0.1:8765",
            "http://localhost:1",
            "http://LOCALHOST",
            "http://[::1]:8765",
        ] {
            assert!(loopback(&reqwest::Url::parse(url).unwrap()), "{url}");
        }
        for url in [
            "http://192.0.2.1:8765",
            "http://127.0.0.2",
            "http://example.com",
        ] {
            assert!(!loopback(&reqwest::Url::parse(url).unwrap()), "{url}");
        }
    }

    #[derive(clap::Parser)]
    struct Cli {
        #[command(subcommand)]
        cmd: HookCmd,
    }

    fn codex_notify(argv: &[&str]) -> Result<(outbox::Target, Vec<String>), clap::Error> {
        let argv = ["agent-memoryd", "codex-notify"].iter().chain(argv);
        match <Cli as clap::Parser>::try_parse_from(argv)?.cmd {
            HookCmd::CodexNotify { at, args } => Ok((at, args)),
            _ => unreachable!(),
        }
    }

    #[test]
    fn codex_notify_takes_the_event_last_and_a_wrapped_program_after_double_dash() {
        let (at, args) = codex_notify(&["--project", "p", "{\"type\":\"x\"}"]).unwrap();
        assert_eq!(
            (at.project.as_str(), args),
            ("p", vec!["{\"type\":\"x\"}".to_string()])
        );
        // The wrapped program's own options stay its own, and the event still comes last. `--tenant`, which
        // hooks of earlier installers pass, is accepted.
        let (_, args) = codex_notify(&[
            "--project",
            "p",
            "--tenant",
            "t",
            "--",
            "/apps/Client",
            "--flag",
            "turn-ended",
            "{}",
        ])
        .unwrap();
        assert_eq!(args, ["/apps/Client", "--flag", "turn-ended", "{}"]);
        assert!(codex_notify(&["--project", "p"]).is_err());
    }

    #[derive(clap::Parser)]
    struct OutboxCli {
        #[command(subcommand)]
        cmd: OutboxCmd,
    }

    fn prune(argv: &[&str]) -> Result<(String, bool), clap::Error> {
        let argv = ["agent-memoryd", "prune"].iter().chain(argv);
        match <OutboxCli as clap::Parser>::try_parse_from(argv)?.cmd {
            OutboxCmd::Prune {
                before,
                include_failed,
                ..
            } => Ok((before, include_failed)),
            _ => unreachable!(),
        }
    }

    #[test]
    fn prune_takes_a_required_age_in_days_or_hours() {
        let ago = |age: chrono::TimeDelta| stamp(chrono::Utc::now() - age);
        let (before, failed) = prune(&["--older-than", "30d"]).unwrap();
        assert!(
            ago(chrono::TimeDelta::days(30)) >= before && before > ago(chrono::TimeDelta::days(31))
        );
        assert!(!failed);
        let (before, failed) = prune(&["--older-than", "12h", "--include-failed"]).unwrap();
        assert!(
            ago(chrono::TimeDelta::hours(12)) >= before
                && before > ago(chrono::TimeDelta::hours(13))
        );
        assert!(failed);
        assert!(prune(&["--older-than", "0d"]).is_ok());
        for age in ["30", "30m", "-1d", "1.5d", "d", "4294967295d", "30日"] {
            assert!(prune(&["--older-than", age]).is_err(), "{age}");
        }
        assert!(prune(&[]).is_err());
    }

    /// A wrapped notify program that records its arguments, as `<file>`: `<arg>|<event>`.
    fn recorder(file: &Path) -> Vec<String> {
        let script = r#"printf '%s|%s' "$1" "$2" > "$0""#;
        [
            "/bin/sh",
            "-c",
            script,
            file.to_str().unwrap(),
            "turn-ended",
        ]
        .map(String::from)
        .to_vec()
    }

    fn turn_event() -> String {
        serde_json::json!({"type": "agent-turn-complete", "thread-id": "s1", "turn-id": "t1",
                           "input-messages": ["Keep the source."], "last-assistant-message": "Kept."})
        .to_string()
    }

    fn notify_cmd(dir: &Path, wrapped: Vec<String>, event: String) -> HookCmd {
        let at = outbox::Target {
            outbox: dir.join("outbox.sqlite3"),
            project: "p".into(),
            platform: "codex".into(),
            tenant: None,
        };
        let mut args = wrapped;
        args.push(event);
        HookCmd::CodexNotify { at, args }
    }

    fn queued(dir: &Path) -> usize {
        outbox::Outbox::open(&dir.join("outbox.sqlite3"))
            .unwrap()
            .status()
            .unwrap()
            .len()
    }

    #[tokio::test]
    async fn codex_notify_passes_the_event_to_the_wrapped_program_and_captures_the_turn() {
        let dir = tempfile::tempdir().unwrap();
        let record = dir.path().join("record");
        let code = hook(notify_cmd(dir.path(), recorder(&record), turn_event())).await;
        assert_eq!(code, 0);
        assert_eq!(
            std::fs::read_to_string(&record).unwrap(),
            format!("turn-ended|{}", turn_event())
        );
        assert_eq!(queued(dir.path()), 1);
    }

    #[tokio::test]
    async fn codex_notify_forwards_even_when_capture_fails_and_reports_either_failure() {
        let dir = tempfile::tempdir().unwrap();
        let record = dir.path().join("record");
        let code = hook(notify_cmd(dir.path(), recorder(&record), "not json".into())).await;
        assert_eq!(code, 1);
        assert_eq!(
            std::fs::read_to_string(&record).unwrap(),
            "turn-ended|not json"
        );

        let missing = vec![dir.path().join("gone").to_str().unwrap().to_string()];
        assert_eq!(hook(notify_cmd(dir.path(), missing, turn_event())).await, 1);
        assert_eq!(queued(dir.path()), 1); // the turn is captured all the same

        let failing = ["/bin/sh", "-c", "exit 3"].map(String::from).to_vec();
        assert_eq!(hook(notify_cmd(dir.path(), failing, turn_event())).await, 1);
    }

    #[test]
    fn private_file_creates_owner_only_directories_and_file() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("a/b/outbox.sqlite3");
        private_file(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&path), 0o600);
            assert_eq!(mode(&root.path().join("a/b")), 0o700);
            assert_eq!(mode(&root.path().join("a")), 0o700);
        }
    }
}
