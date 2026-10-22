//! The SurrealDB server process: the user's `surreal start` on the store's SurrealKV datastore,
//! bound to a free loopback port. A shell watchdog between this process and the server stops the
//! server when the store closes the watchdog's stdin and also when this process exits however it
//! exits (a crash, `panic = abort`, SIGKILL): the pipe then reaches end of file. So a server never
//! outlives its daemon or keeps holding the datastore.
use anyhow::{bail, ensure, Context, Result};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{ChildStderr, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::watch;

/// Runs `"$@"` (`surreal start ...`) and sends it SIGTERM once stdin reaches end of file, then exits
/// with the server's status. Background commands read /dev/null, so the watcher reads the pipe as fd 3.
const WATCHDOG: &str = r#"exec 3<&0
"$@" </dev/null 3<&- 1>&2 &
server=$!
(read -r _ <&3; kill -TERM "$server" 2>/dev/null) &
watcher=$!
exec 3<&-
wait "$server"
status=$?
kill "$watcher" 2>/dev/null
exit "$status""#;
/// SurrealDB releases the store runs on: 3.2 (which earlier versions embedded) up to the next major.
const VERSIONS: ((u64, u64), (u64, u64)) = ((3, 2), (4, 0));
/// How long a server may take to answer `/health` after it starts.
const READY_TIMEOUT: Duration = Duration::from_secs(60);
/// A server that exits during startup is started again for this long: a stopping predecessor may
/// still hold the datastore, or another process may have taken the port.
const RETRY_FOR: Duration = Duration::from_secs(15);
const STOP_TIMEOUT: Duration = Duration::from_secs(15);
const LOG_LINES: usize = 20;
pub const INSTALL_HINT: &str = "install SurrealDB 3.x (`brew install surrealdb/tap/surreal`, or \
    `curl -sSf https://install.surrealdb.com | sh`; see docs/deployment.md#install-surrealdb)";

pub struct Server {
    /// `http://127.0.0.1:<port>`.
    pub url: String,
    /// The watchdog's stdin; closing it stops the server.
    stop: Mutex<Option<ChildStdin>>,
    /// How the watchdog exited, once it has.
    exit: watch::Receiver<Option<String>>,
    /// The server's last log lines, for startup errors.
    log: Arc<Mutex<VecDeque<String>>>,
}

enum NotReady {
    Exited(String),
    TimedOut,
}

impl Server {
    /// Start `surreal` on `datastore` as root with `password` and wait until it answers.
    pub async fn start(surreal: &Path, datastore: &Path, password: &str) -> Result<Server> {
        let program = executable(surreal)?;
        // Before the server touches the datastore: another major release could rewrite it.
        supported(&program).await?;
        let path = datastore
            .to_str()
            .filter(|p| !p.contains('?'))
            .with_context(|| {
                format!(
                    "SurrealDB cannot open {}: the path must be UTF-8 without '?'",
                    datastore.display()
                )
            })?;
        let started = Instant::now();
        loop {
            let server = Server::spawn(&program, path, password)?;
            let reason = match server.ready(started + READY_TIMEOUT).await {
                Ok(()) => return Ok(server),
                Err(NotReady::Exited(reason)) if started.elapsed() < RETRY_FOR => {
                    tracing::debug!(%reason, "SurrealDB exited during startup; starting it again");
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    continue;
                }
                Err(NotReady::Exited(reason)) => reason,
                Err(NotReady::TimedOut) => {
                    format!("it did not answer within {}s", READY_TIMEOUT.as_secs())
                }
            };
            bail!(
                "SurrealDB ({}) did not start on {path}: {reason}{}",
                program.display(),
                server.log_tail()
            );
        }
    }

    fn spawn(program: &Path, datastore: &str, password: &str) -> Result<Server> {
        let port = std::net::TcpListener::bind(("127.0.0.1", 0))?
            .local_addr()?
            .port();
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg(WATCHDOG)
            .arg("surreal-watchdog")
            .arg(program)
            .arg("start")
            .arg("--bind")
            .arg(format!("127.0.0.1:{port}"))
            // Warnings include one about the root user on every start after the first.
            .args(["--log", "error", "--no-banner"])
            .arg(format!("surrealkv://{datastore}"))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        // Only these settings reach the server: an inherited SURREAL_* variable could, for example,
        // turn authentication off.
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("SURREAL_") {
                command.env_remove(key);
            }
        }
        command
            .env("SURREAL_USER", super::USER)
            .env("SURREAL_PASS", password)
            // A transaction's embeddings can exceed the default 4 MiB.
            .env("SURREAL_HTTP_MAX_RPC_BODY_SIZE", "256MiB")
            .env("NO_COLOR", "1");
        let mut child = command.spawn().context("cannot run /bin/sh")?;
        let stop = child.stdin.take();
        let stderr = child.stderr.take().context("no stderr pipe")?;
        let log = Arc::new(Mutex::new(VecDeque::new()));
        let lines = log.clone();
        std::thread::spawn(move || forward(stderr, &lines));
        let (exited, exit) = watch::channel(None);
        std::thread::spawn(move || {
            let reason = match child.wait() {
                Ok(status) => format!("it exited ({status})"),
                Err(e) => format!("its watchdog could not be waited for: {e}"),
            };
            let _ = exited.send(Some(reason));
        });
        Ok(Server {
            url: format!("http://127.0.0.1:{port}"),
            stop: Mutex::new(stop),
            exit,
            log,
        })
    }

    async fn ready(&self, deadline: Instant) -> Result<(), NotReady> {
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(2))
            .build()
            .map_err(|e| NotReady::Exited(e.to_string()))?;
        let health = format!("{}/health", self.url);
        loop {
            if let Some(reason) = self.exit.borrow().clone() {
                return Err(NotReady::Exited(reason));
            }
            if client
                .get(&health)
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
            {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(NotReady::TimedOut);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Stop the server and wait, at most `STOP_TIMEOUT`, until it has exited and released the datastore.
    pub async fn stop(&self) {
        drop(self.stop.lock().expect("server stop poisoned").take());
        let mut exit = self.exit.clone();
        let _ = tokio::time::timeout(STOP_TIMEOUT, exit.wait_for(Option::is_some)).await;
    }

    /// Completes once the server has exited, with how it exited and its last log lines.
    pub async fn exited(&self) -> String {
        let mut exit = self.exit.clone();
        let reason = match exit.wait_for(Option::is_some).await {
            Ok(reason) => reason.clone().unwrap_or_default(),
            Err(_) => "its monitor stopped".into(),
        };
        format!("{reason}{}", self.log_tail())
    }

    fn log_tail(&self) -> String {
        let log = self.log.lock().expect("server log poisoned");
        if log.is_empty() {
            return String::new();
        }
        let lines: Vec<&str> = log.iter().map(String::as_str).collect();
        format!("\n  {}", lines.join("\n  "))
    }
}

/// The server's output, to the log and the startup-error tail.
fn forward(stderr: ChildStderr, log: &Mutex<VecDeque<String>>) {
    for line in BufReader::new(stderr).lines().map_while(Result::ok) {
        tracing::warn!(target: "surrealdb", "{line}");
        let mut tail = log.lock().expect("server log poisoned");
        if tail.len() == LOG_LINES {
            tail.pop_front();
        }
        tail.push_back(line);
    }
}

/// Refuse a `surreal` outside `VERSIONS`, by what `surreal version` prints ("3.2.4 for macos on aarch64").
async fn supported(program: &Path) -> Result<()> {
    let output = tokio::process::Command::new(program)
        .arg("version")
        .stdin(Stdio::null())
        .output()
        .await
        .with_context(|| format!("cannot run {} version", program.display()))?;
    let text = String::from_utf8_lossy(&output.stdout);
    let version = text
        .split_whitespace()
        .find_map(|word| {
            let mut parts = word.split('.').map(|p| p.parse::<u64>().ok());
            Some((parts.next()??, parts.next()??, word))
        })
        .filter(|_| output.status.success());
    let Some((major, minor, word)) = version else {
        bail!(
            "{} is not SurrealDB: `surreal version` printed no version; {INSTALL_HINT}",
            program.display()
        );
    };
    let (oldest, newer) = VERSIONS;
    ensure!(
        oldest <= (major, minor) && (major, minor) < newer,
        "{} is SurrealDB {word}; the store needs SurrealDB 3.2 or a later 3.x: {INSTALL_HINT}",
        program.display()
    );
    Ok(())
}

/// `program` as an executable file: a path as given, a bare name looked up on PATH.
fn executable(program: &Path) -> Result<PathBuf> {
    if program.components().count() > 1 {
        ensure!(
            is_executable(program),
            "{} is not an executable SurrealDB `surreal`; {INSTALL_HINT}",
            program.display()
        );
        return Ok(program.to_path_buf());
    }
    std::env::var_os("PATH")
        .iter()
        .flat_map(std::env::split_paths)
        .map(|dir| dir.join(program))
        .find(|candidate| is_executable(candidate))
        .with_context(|| {
            format!(
                "SurrealDB is not installed: no `{}` on PATH; {INSTALL_HINT}, or give its path with --surreal-bin",
                program.display()
            )
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in server that records SIGTERM in `marker` and exits.
    fn server(marker: &Path) -> String {
        format!(
            "trap 'echo stopped > {}; exit 0' TERM; while :; do sleep 0.1; done",
            marker.display()
        )
    }

    fn marker() -> PathBuf {
        std::env::temp_dir().join(memory_rules::util::new_id("watchdog_test"))
    }

    fn watchdog(argv: &[&str]) -> std::process::Child {
        Command::new("/bin/sh")
            .arg("-c")
            .arg(WATCHDOG)
            .arg("surreal-watchdog")
            .args(argv)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }

    fn exit_within(child: &mut std::process::Child, limit: Duration) -> std::process::ExitStatus {
        let deadline = Instant::now() + limit;
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < deadline, "the watchdog did not exit");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn closing_stdin_stops_the_server() {
        let marker = marker();
        let mut child = watchdog(&["/bin/sh", "-c", &server(&marker)]);
        std::thread::sleep(Duration::from_millis(300));
        assert!(child.try_wait().unwrap().is_none());
        drop(child.stdin.take());
        assert!(exit_within(&mut child, Duration::from_secs(5)).success());
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "stopped\n");
        std::fs::remove_file(marker).unwrap();
    }

    #[test]
    fn the_watchdog_exits_with_the_server() {
        let mut child = watchdog(&["/bin/sh", "-c", "exit 3"]);
        // Its stdin is still open: the server's exit alone ends the watchdog.
        assert_eq!(
            exit_within(&mut child, Duration::from_secs(5)).code(),
            Some(3)
        );
    }

    #[test]
    fn the_server_stops_when_the_process_holding_the_pipe_exits() {
        // `sleep` holds the pipe's write end and exits as a crashed daemon would, closing nothing itself.
        let marker = marker();
        let pipeline = format!(
            "sleep 1 | /bin/sh -c \"$0\" surreal-watchdog /bin/sh -c {}",
            sh_quote(&server(&marker))
        );
        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg(pipeline)
            .arg(WATCHDOG)
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        assert!(exit_within(&mut child, Duration::from_secs(10)).success());
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "stopped\n");
        std::fs::remove_file(marker).unwrap();
    }

    fn sh_quote(word: &str) -> String {
        format!("'{}'", word.replace('\'', r"'\''"))
    }

    #[tokio::test]
    async fn only_surrealdb_3_2_or_a_later_3_x_is_started() {
        let dir = marker();
        std::fs::create_dir_all(&dir).unwrap();
        let fake = |name: &str, output: &str| {
            let path = dir.join(name);
            std::fs::write(&path, format!("#!/bin/sh\necho '{output}'\n")).unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        };
        for ok in ["3.2.4 for macos on aarch64", "3.10.0 for linux on x86_64"] {
            supported(&fake("ok", ok)).await.unwrap();
        }
        for (old, message) in [
            (
                "2.3.7 for linux on x86_64",
                "is SurrealDB 2.3.7; the store needs SurrealDB 3.2",
            ),
            ("3.1.9 for linux on x86_64", "is SurrealDB 3.1.9"),
            ("4.0.0 for linux on x86_64", "is SurrealDB 4.0.0"),
            ("something else", "is not SurrealDB"),
        ] {
            let e = supported(&fake("other", old)).await.unwrap_err();
            assert!(e.to_string().contains(message), "{e}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_missing_surreal_says_how_to_install_it() {
        let e = executable(Path::new("surreal-not-installed-here")).unwrap_err();
        assert!(e.to_string().contains("SurrealDB is not installed"), "{e}");
        assert!(e.to_string().contains("install.surrealdb.com"), "{e}");
        let e = executable(Path::new("/nonexistent/surreal")).unwrap_err();
        assert!(e.to_string().contains("not an executable"), "{e}");
    }
}
