//! Agent harness commands: the dialogue collector and automatic recall that agent hosts run as
//! hooks, and the uploader that submits captured dialogue to the daemon. They keep the files,
//! schemas and hook contracts of `scripts/harness_memory.py` and `scripts/harness_recall.py`, so
//! either implementation can share one outbox and one recall record while installations migrate.
//! See docs/mcp-harness.md.
pub mod outbox;
pub mod recall;
pub mod uploader;

use clap::Subcommand;
use serde_json::Value;
use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Daemon response bytes read before giving up.
const MAX_RESPONSE: usize = 1 << 20;

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
    /// Capture the Codex agent-turn-complete JSON argument that `notify` appends
    CodexNotify {
        #[command(flatten)]
        at: outbox::Target,
        event: String,
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
        #[arg(long, default_value = "default")]
        tenant: String,
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
                &at.tenant,
                &at.platform,
                turn_id_field,
            )?;
            // Never inject raw chat, block Stop or request a continuation.
            println!("{{}}");
            Ok(0)
        }),
        HookCmd::CodexNotify { at, event } => adapter(|| {
            let event: Value = serde_json::from_str(&event).map_err(|_| Error::Json)?;
            outbox::Outbox::open(&at.outbox)?.capture_notify(
                &event,
                &at.project,
                &at.tenant,
                &at.platform,
            )?;
            Ok(0)
        }),
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
        OutboxCmd::Enqueue {
            outbox,
            platform,
            tenant,
        } => adapter(|| {
            let body = stdin_json()?;
            outbox::Outbox::open(&outbox)?.enqueue(&tenant, &platform, &body)?;
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
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, false)
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
    matches!(
        url.host_str(),
        Some("127.0.0.1" | "localhost" | "::1" | "[::1]")
    )
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
    pub async fn post(
        &self,
        name: &str,
        body: &Value,
        tenant: &str,
        platform: &str,
    ) -> Result<Value, CallError> {
        let mut response = self
            .client
            .post(format!("{}/v1/{name}", self.base))
            .header("X-Memory-Tenant", tenant)
            .header("X-Memory-Platform", platform)
            .json(body)
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(CallError::Status(response.status().as_u16()));
        }
        let mut data = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            data.extend_from_slice(&chunk);
            if data.len() > MAX_RESPONSE {
                return Err(CallError::Invalid);
            }
        }
        match serde_json::from_slice(&data) {
            Ok(value @ Value::Object(_)) => Ok(value),
            _ => Err(CallError::Invalid),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
