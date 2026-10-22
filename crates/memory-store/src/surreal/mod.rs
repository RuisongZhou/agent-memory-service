//! The store's database: the user's own SurrealDB server ([`server`]), started on the store's
//! SurrealKV datastore, and a client for SurrealDB's RPC protocol. No part of SurrealDB is compiled
//! into this crate. Requests are HTTP `POST /rpc` with CBOR bodies ([`cbor`]), the encoding of
//! SurrealDB's own SDKs: its JSON format reads strings that look like datetimes or record IDs as those
//! types, which would change stored documents.
mod cbor;
mod server;

use anyhow::{anyhow, bail, ensure, Context, Result};
use bytes::Bytes;
use ciborium::Value as Cbor;
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use reqwest::StatusCode;
use serde_json::Value;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

pub use server::INSTALL_HINT;

const NAMESPACE: &str = "memory";
const DATABASE: &str = "main";
const USER: &str = "root";
const CBOR: &str = "application/cbor";
/// Root tokens expire after an hour; sign in again well before. Wall-clock time, so that a laptop's
/// sleep counts.
const TOKEN_AGE: Duration = Duration::from_secs(30 * 60);
/// RPC error code of an invalid or expired token.
const INVALID_AUTH: i128 = -32002;

/// A SurrealDB server on the store's datastore and a client signed in to it as root.
pub struct Database {
    server: server::Server,
    http: reqwest::Client,
    rpc: String,
    password: String,
    token: tokio::sync::Mutex<Option<(String, SystemTime)>>,
    ids: AtomicU64,
    /// Held while the store is open: one daemon per datastore.
    _lock: std::fs::File,
}

/// One result per statement of a query.
pub struct Response(Vec<Option<std::result::Result<Value, String>>>);

impl Database {
    /// Start `surreal` on `root/db` and sign in. `root` also holds the datastore's root password
    /// (`db.password`, created with the datastore) and the lock that keeps a second daemon out
    /// (`db.lock`). A datastore without a root user, such as one written by an earlier embedded
    /// version, gets one with that password when its server first starts.
    pub async fn open(root: &Path, surreal: &Path) -> Result<Self> {
        std::fs::create_dir_all(root)
            .with_context(|| format!("cannot create {}", root.display()))?;
        let root = std::path::absolute(root)?;
        let lock = lock(&root.join("db.lock"))?;
        let password = password(&root.join("db.password"))?;
        let server = server::Server::start(surreal, &root.join("db"), &password).await?;
        let db = Self {
            rpc: format!("{}/rpc", server.url),
            server,
            http: reqwest::Client::builder()
                .no_proxy()
                .connect_timeout(Duration::from_secs(5))
                .build()?,
            password,
            token: tokio::sync::Mutex::new(None),
            ids: AtomicU64::new(1),
            _lock: lock,
        };
        db.token(true).await.with_context(|| {
            format!(
                "cannot sign in to SurrealDB as root with the password in {}; restore that file from \
                 the backup of this data directory",
                root.join("db.password").display()
            )
        })?;
        // Requests select the namespace and database by header; only `USE` creates them in a new
        // datastore, as the SDK's `use_ns`/`use_db` did.
        db.query(&format!("USE NS {NAMESPACE} DB {DATABASE}"), vec![])
            .await?
            .check()
            .context("select the store's namespace and database")?;
        Ok(db)
    }

    /// Run `sql` (statements separated by `;`) with variables `vars` (names without `$`).
    pub async fn query(&self, sql: &str, vars: Vec<(String, Value)>) -> Result<Response> {
        let vars = vars
            .into_iter()
            .map(|(k, v)| (Cbor::Text(k), cbor::from_json(v)))
            .collect();
        let body = Bytes::from(request(
            self.ids.fetch_add(1, Ordering::Relaxed),
            "query",
            vec![cbor::text(sql), Cbor::Map(vars)],
        )?);
        let reply = match self
            .send(body.clone(), Some(&self.token(false).await?))
            .await
        {
            // The query did not run; a fresh token fixes an expired one.
            Err(Failure::Auth(_)) => self.send(body, Some(&self.token(true).await?)).await,
            reply => reply,
        };
        let Cbor::Array(statements) = reply.map_err(Failure::into_error)? else {
            bail!("SurrealDB answered a query with something other than a list of results");
        };
        statements
            .into_iter()
            .map(statement)
            .collect::<Result<_>>()
            .map(Response)
    }

    /// Stop the server and wait until it has released the datastore.
    pub async fn shutdown(&self) {
        self.server.stop().await;
    }

    /// Completes once the server has exited, with how it exited.
    pub async fn exited(&self) -> String {
        self.server.exited().await
    }

    /// The root token, signing in when there is none, it is old or `renew` asks for a new one.
    async fn token(&self, renew: bool) -> Result<String> {
        let mut token = self.token.lock().await;
        if let Some((value, issued)) = &*token {
            let age = SystemTime::now()
                .duration_since(*issued)
                .unwrap_or(TOKEN_AGE);
            if !renew && age < TOKEN_AGE {
                return Ok(value.clone());
            }
        }
        let credentials = Cbor::Map(vec![
            (cbor::text("user"), cbor::text(USER)),
            (cbor::text("pass"), cbor::text(&self.password)),
        ]);
        let body = Bytes::from(request(
            self.ids.fetch_add(1, Ordering::Relaxed),
            "signin",
            vec![credentials],
        )?);
        let value = match self.send(body, None).await.map_err(Failure::into_error)? {
            Cbor::Text(value) => value,
            // An access method with refresh tokens answers {access, refresh}.
            Cbor::Map(mut fields) => match cbor::take(&mut fields, "access") {
                Some(Cbor::Text(value)) => value,
                _ => bail!("SurrealDB signin returned no token"),
            },
            _ => bail!("SurrealDB signin returned no token"),
        };
        *token = Some((value.clone(), SystemTime::now()));
        Ok(value)
    }

    async fn send(&self, body: Bytes, token: Option<&str>) -> Result<Cbor, Failure> {
        let mut request = self
            .http
            .post(&self.rpc)
            .header(CONTENT_TYPE, CBOR)
            .header(ACCEPT, CBOR)
            .header("surreal-ns", NAMESPACE)
            .header("surreal-db", DATABASE)
            .body(body);
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        let response = request
            .send()
            .await
            .map_err(|e| Failure::Other(anyhow!(e).context("SurrealDB request failed")))?;
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|e| Failure::Other(anyhow!(e).context("SurrealDB response")))?;
        let Ok(Cbor::Map(mut reply)) = ciborium::from_reader::<Cbor, _>(&bytes[..]) else {
            let text = String::from_utf8_lossy(&bytes[..bytes.len().min(300)]).into_owned();
            let error = anyhow!("SurrealDB answered HTTP {status}: {}", text.trim());
            return Err(if status == StatusCode::UNAUTHORIZED {
                Failure::Auth(error)
            } else {
                Failure::Other(error)
            });
        };
        if let Some(error) = cbor::take(&mut reply, "error") {
            let (code, message) = match error {
                Cbor::Map(mut fields) => (
                    match cbor::take(&mut fields, "code") {
                        Some(Cbor::Integer(code)) => i128::from(code),
                        _ => 0,
                    },
                    match cbor::take(&mut fields, "message") {
                        Some(Cbor::Text(message)) => message,
                        _ => "unknown error".into(),
                    },
                ),
                other => (0, cbor::to_json(other).to_string()),
            };
            let error = anyhow!("SurrealDB: {message}");
            return Err(
                if code == INVALID_AUTH || status == StatusCode::UNAUTHORIZED {
                    Failure::Auth(error)
                } else {
                    Failure::Other(error)
                },
            );
        }
        cbor::take(&mut reply, "result").ok_or_else(|| {
            Failure::Other(anyhow!("SurrealDB answered HTTP {status} without a result"))
        })
    }
}

enum Failure {
    /// Rejected before running: no valid token.
    Auth(anyhow::Error),
    Other(anyhow::Error),
}

impl Failure {
    fn into_error(self) -> anyhow::Error {
        match self {
            Failure::Auth(e) | Failure::Other(e) => e,
        }
    }
}

impl Response {
    /// The first failed statement's error, if any statement failed.
    pub fn check(self) -> Result<Self> {
        if let Some(message) = self.0.iter().flatten().find_map(|r| r.as_ref().err()) {
            bail!("{message}");
        }
        Ok(self)
    }

    /// Statement `index`'s rows: its array, or its single value; none when it did not run.
    pub fn take(&mut self, index: usize) -> Result<Vec<Value>> {
        match self.0.get_mut(index).and_then(Option::take) {
            None => Ok(Vec::new()),
            Some(Err(message)) => bail!("{message}"),
            Some(Ok(Value::Array(rows))) => Ok(rows),
            Some(Ok(value)) => Ok(vec![value]),
        }
    }
}

/// One statement's `{status, result}`: its value, or for `ERR` its error message.
fn statement(value: Cbor) -> Result<Option<std::result::Result<Value, String>>> {
    let Cbor::Map(mut fields) = value else {
        bail!("SurrealDB returned a statement result that is not an object");
    };
    let result = cbor::take(&mut fields, "result").map_or(Value::Null, cbor::to_json);
    Ok(Some(match cbor::take(&mut fields, "status") {
        Some(Cbor::Text(status)) if status == "OK" => Ok(result),
        _ => Err(match result {
            Value::String(message) => message,
            other => other.to_string(),
        }),
    }))
}

fn request(id: u64, method: &str, params: Vec<Cbor>) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    ciborium::into_writer(
        &Cbor::Map(vec![
            (cbor::text("id"), Cbor::Integer(id.into())),
            (cbor::text("method"), cbor::text(method)),
            (cbor::text("params"), Cbor::Array(params)),
        ]),
        &mut body,
    )
    .context("encode a SurrealDB request")?;
    Ok(body)
}

/// Lock `path` for as long as the returned file is open.
fn lock(path: &Path) -> Result<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => bail!(
            "{} is in use by another agent-memoryd; do not run two daemons on one data directory",
            path.parent().unwrap_or(path).display()
        ),
        Err(std::fs::TryLockError::Error(e)) => {
            Err(e).with_context(|| format!("cannot lock {}", path.display()))
        }
    }
}

/// The root password in `path`, created (mode 0600) from 32 random bytes when the file is missing.
fn password(path: &Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let password = text.trim();
            ensure!(!password.is_empty(), "{} is empty", path.display());
            return Ok(password.to_string());
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut random| random.read_exact(&mut bytes))
        .context("cannot read /dev/urandom")?;
    let password: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .and_then(|mut file| file.write_all(format!("{password}\n").as_bytes()))
        .with_context(|| format!("cannot write {}", path.display()))?;
    Ok(password)
}
