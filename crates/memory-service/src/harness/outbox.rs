//! Local dialogue outbox. Hooks only capture into it; an uploader submits jobs in order and
//! polls their receipts. Capture uses documented Codex notify / Claude Code hook fields, never private
//! transcript formats; `agent-memoryd import apply` queues past session turns read from those under the
//! same turn keys (`enqueue_import`). The SQLite schema is the one `scripts/harness_memory.py` writes.
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Map, Value};
use std::fs::File;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use super::{expand, invalid, now, private_file, CallError, Daemon, Error};

/// Default outbox, relative to the home directory.
pub const DEFAULT_OUTBOX: &str = ".local/share/agent-memory-service/harness-outbox.sqlite3";
/// Hook events the collector acts on; any other event is ignored.
const HOOK_EVENTS: [&str; 5] = [
    "UserPromptSubmit",
    "Stop",
    "Interrupt",
    "StopFailure",
    "SessionEnd",
];
const TASK_STATUSES: [&str; 5] = ["queued", "running", "indexing", "completed", "failed"];
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS prompts (
    tenant TEXT, platform TEXT, project TEXT, session TEXT,
    native_id TEXT NOT NULL, content TEXT NOT NULL, timestamp TEXT NOT NULL,
    PRIMARY KEY (tenant, platform, project, session)
);
CREATE TABLE IF NOT EXISTS jobs (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    tenant TEXT NOT NULL, platform TEXT NOT NULL, turn_id TEXT NOT NULL,
    body TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'pending',
    request_id TEXT, error TEXT, created_at TEXT NOT NULL,
    UNIQUE (tenant, platform, turn_id)
);";
const SESSION_KEY: &str = "tenant=?1 AND platform=?2 AND project=?3 AND session=?4";
/// The `tenant` column the schema keeps for the file `scripts/harness_memory.py` shares; it always holds this value.
const TENANT: &str = "default";

pub fn default_path() -> PathBuf {
    super::home().join(DEFAULT_OUTBOX)
}

/// Where a capture hook files its turns.
#[derive(clap::Args, Debug, Clone)]
pub struct Target {
    #[arg(long, default_value_os_t = default_path())]
    pub outbox: PathBuf,
    /// Stable project id shared by harnesses; never inferred from cwd
    #[arg(long)]
    pub project: String,
    #[arg(long, default_value = "codex")]
    pub platform: String,
    /// Ignored: hooks written by earlier installers pass it.
    #[arg(long, hide = true)]
    pub tenant: Option<String>,
}

/// The hook field holding the host's native turn id.
#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnIdField {
    #[value(name = "prompt_id")]
    PromptId,
    #[value(name = "turn_id")]
    TurnId,
    /// No id: Stop pairs with the session's open prompt.
    #[value(name = "none")]
    None,
}

/// `event[key]` as a nonempty string.
fn required<'a>(event: &'a Value, key: &str) -> Result<&'a str, Error> {
    let Value::Object(fields) = event else {
        return Err(invalid("expected a JSON object"));
    };
    match fields.get(key) {
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(s),
        _ => Err(invalid(format!(
            "missing nonempty {key}; use the documented event fields"
        ))),
    }
}

/// `project/session/native_id`, each part quoted as Python's `quote(part, safe='')`; session import gives a
/// past turn the same key (`memory_rules::import::units::turn_key`).
pub fn turn_key(project: &str, session: &str, native_id: &str) -> String {
    memory_rules::import::units::turn_key(project, session, native_id)
}

/// What [`Outbox::enqueue_import`] found for a turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Queued {
    /// The turn's job was added.
    Added,
    /// The same run queued it before: an interrupted apply resumes.
    Again,
    /// Another job holds the turn: live capture (`run` None) or another import run. It is never replaced.
    Held {
        run: Option<String>,
        status: String,
        request_id: Option<String>,
    },
}

fn turn_body(
    project: &str,
    session: &str,
    native_id: &str,
    user: &str,
    assistant: &str,
    started: &str,
    ended: &str,
) -> Value {
    json!({"scope": {"project": project, "session": session},
           "turnId": turn_key(project, session, native_id),
           "messages": [{"role": "user", "content": user, "timestamp": started},
                        {"role": "assistant", "content": assistant, "timestamp": ended}]})
}

/// Python's `not str(value or '').strip()` for a hook's `prompt` field.
fn blank(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) | Some(Value::Bool(false)) => true,
        Some(Value::String(s)) => s.trim().is_empty(),
        Some(Value::Number(n)) => n.as_f64() == Some(0.0),
        Some(Value::Array(a)) => a.is_empty(),
        Some(Value::Object(o)) => o.is_empty(),
        Some(Value::Bool(true)) => false,
    }
}

fn enqueue_in(db: &Connection, platform: &str, body: &Value) -> Result<(), Error> {
    let Value::Object(fields) = body else {
        return Err(invalid("expected a JSON object"));
    };
    let scope = fields.get("scope").cloned().unwrap_or_else(|| json!({}));
    required(&scope, "project")?;
    required(&scope, "session")?;
    let turn_id = required(body, "turnId")?;
    let present = |key: &str| fields.get(key).is_some_and(|v| !v.is_null());
    if present("text") || present("memoryType") {
        return Err(invalid(
            "dialogue outbox accepts messages or sessionEnded, not explicit text",
        ));
    }
    let messages = fields.get("messages").is_some_and(|v| !blank(Some(v)));
    if !messages && fields.get("sessionEnded") != Some(&Value::Bool(true)) {
        return Err(invalid("messages or sessionEnded required"));
    }
    let mut payload: Map<String, Value> = fields.clone();
    payload.insert("sync".into(), Value::Bool(false));
    // Keep the first payload and its original capture timestamps on every retry.
    db.execute(
        "INSERT OR IGNORE INTO jobs (tenant,platform,turn_id,body,created_at) VALUES (?1,?2,?3,?4,?5)",
        params![TENANT, platform, turn_id, Value::Object(payload).to_string(), now()],
    )?;
    Ok(())
}

#[derive(Debug, Clone)]
struct Job {
    seq: i64,
    platform: String,
    body: String,
    request_id: Option<String>,
}

pub struct Outbox {
    db: Connection,
}

impl Outbox {
    pub fn open(path: &Path) -> Result<Self, Error> {
        let path = expand(path);
        private_file(&path)?;
        let db = Connection::open(&path)?;
        db.busy_timeout(Duration::from_secs(2))?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")?;
        db.execute_batch(SCHEMA)?;
        Ok(Self { db })
    }

    pub fn enqueue(&mut self, platform: &str, body: &Value) -> Result<(), Error> {
        let tx = self.db.transaction()?;
        enqueue_in(&tx, platform, body)?;
        tx.commit()?;
        Ok(())
    }

    /// Queue a past session turn of the import run its body names (`importRunId`), unless a job of the same turn
    /// is there already: one live capture queued, or an earlier run's.
    pub fn enqueue_import(&mut self, platform: &str, body: &Value) -> Result<Queued, Error> {
        let run = required(body, "importRunId")?.to_string();
        let turn_id = required(body, "turnId")?.to_string();
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing: Option<(String, String, Option<String>)> = tx
            .query_row(
                "SELECT body,status,request_id FROM jobs WHERE tenant=?1 AND platform=?2 AND turn_id=?3",
                params![TENANT, platform, turn_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let queued = match existing {
            None => {
                enqueue_in(&tx, platform, body)?;
                Queued::Added
            }
            Some((held, status, request_id)) => {
                let held_run = serde_json::from_str::<Value>(&held)
                    .ok()
                    .and_then(|b| b["importRunId"].as_str().map(str::to_string));
                match held_run.as_deref() == Some(run.as_str()) {
                    true => Queued::Again,
                    false => Queued::Held {
                        run: held_run,
                        status,
                        request_id,
                    },
                }
            }
        };
        tx.commit()?;
        Ok(queued)
    }

    /// Mark the jobs of import run `run_id` that no uploader has submitted `cancelled`, which no uploader sends;
    /// returns how many.
    pub fn cancel_import(&mut self, run_id: &str) -> Result<usize, Error> {
        Ok(self.db.execute(
            "UPDATE jobs SET status='cancelled',error='import rolled back' \
             WHERE status='pending' AND json_extract(body,'$.importRunId')=?1",
            [run_id],
        )?)
    }

    /// Jobs waiting for an uploader (`pending`) or for formation to finish (`accepted`).
    pub fn backlog(&self) -> Result<usize, Error> {
        let n: i64 = self.db.query_row(
            "SELECT count(*) FROM jobs WHERE status IN ('pending','accepted')",
            [],
            |row| row.get(0),
        )?;
        Ok(n as usize)
    }

    /// A Codex `agent-turn-complete` notification becomes one completed turn.
    pub fn capture_notify(
        &mut self,
        event: &Value,
        project: &str,
        platform: &str,
    ) -> Result<(), Error> {
        let Value::Object(fields) = event else {
            return Err(invalid("expected a JSON object"));
        };
        if fields.get("type").and_then(Value::as_str) != Some("agent-turn-complete") {
            return Ok(());
        }
        let session = required(event, "thread-id")?;
        let native_id = required(event, "turn-id")?;
        let inputs: Vec<&str> = match fields.get("input-messages") {
            Some(Value::Array(items)) if !items.is_empty() => items
                .iter()
                .map(Value::as_str)
                .collect::<Option<_>>()
                .ok_or_else(|| invalid("input-messages must be a nonempty string array"))?,
            _ => return Err(invalid("input-messages must be a nonempty string array")),
        };
        let user = inputs.join("\n\n");
        if user.trim().is_empty() {
            return Err(invalid("input-messages must contain text"));
        }
        let assistant = required(event, "last-assistant-message")?;
        // notify does not supply source timestamps; these are observed completion times.
        let stamp = now();
        let body = turn_body(
            project, session, native_id, &user, assistant, &stamp, &stamp,
        );
        self.enqueue(platform, &body)
    }

    /// Pair UserPromptSubmit with its Stop into one completed turn. `id_field` names the event's
    /// native turn id; `TurnIdField::None` pairs Stop with the session's open prompt.
    pub fn capture_hook(
        &mut self,
        event: &Value,
        project: &str,
        platform: &str,
        id_field: Option<TurnIdField>,
    ) -> Result<(), Error> {
        let Value::Object(fields) = event else {
            return Err(invalid("expected a JSON object"));
        };
        let name = fields
            .get("hook_event_name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !HOOK_EVENTS.contains(&name) {
            return Ok(());
        }
        let session = required(event, "session_id")?;
        let key = params![TENANT, platform, project, session];
        let id_field = id_field.unwrap_or(if platform == "codex" {
            TurnIdField::TurnId
        } else {
            TurnIdField::PromptId
        });
        // Serialize capture transactions before reading pending state.
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let pending: Option<(String, String, String)> = tx
            .query_row(
                &format!("SELECT native_id, content, timestamp FROM prompts WHERE {SESSION_KEY}"),
                key,
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let forget =
            |tx: &Connection| tx.execute(&format!("DELETE FROM prompts WHERE {SESSION_KEY}"), key);
        if name == "SessionEnd" {
            forget(&tx)?;
            let saved = tx
                .query_row(
                    "SELECT 1 FROM jobs WHERE tenant=?1 AND platform=?2 AND \
                     json_extract(body,'$.scope.project')=?3 AND json_extract(body,'$.scope.session')=?4 LIMIT 1",
                    key,
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if saved {
                let marker = format!("session-end:{}", uuid::Uuid::new_v4());
                let body = json!({"scope": {"project": project, "session": session},
                                  "turnId": turn_key(project, session, &marker), "sessionEnded": true});
                enqueue_in(&tx, platform, &body)?;
            }
            tx.commit()?;
            return Ok(());
        }
        // Without native ids each prompt opens a new turn, identified once when it is stored.
        let mut native_id = match id_field {
            TurnIdField::None => format!("prompt-{}", uuid::Uuid::new_v4()),
            TurnIdField::PromptId => required(event, "prompt_id")?.to_string(),
            TurnIdField::TurnId => required(event, "turn_id")?.to_string(),
        };
        match name {
            "UserPromptSubmit" => {
                if id_field == TurnIdField::None && blank(fields.get("prompt")) {
                    // A prompt without text (an image only) still ends the open turn: never pair its reply with that.
                    forget(&tx)?;
                } else {
                    let prompt = required(event, "prompt")?;
                    if pending.as_ref().is_none_or(|p| p.0 != native_id) {
                        // Replacing an unmatched prompt discards an interrupted/incomplete turn.
                        tx.execute(
                            "INSERT OR REPLACE INTO prompts VALUES (?1,?2,?3,?4,?5,?6,?7)",
                            params![TENANT, platform, project, session, native_id, prompt, now()],
                        )?;
                    }
                }
            }
            "Interrupt" | "StopFailure" => {
                if pending
                    .as_ref()
                    .is_some_and(|p| id_field == TurnIdField::None || p.0 == native_id)
                {
                    forget(&tx)?;
                }
            }
            _ => {
                // Codex sends a null final message for a turn that ended without one: nothing to pair.
                if fields
                    .get("last_assistant_message")
                    .is_some_and(Value::is_null)
                {
                    if pending
                        .as_ref()
                        .is_some_and(|p| id_field == TurnIdField::None || p.0 == native_id)
                    {
                        forget(&tx)?;
                    }
                    return Ok(tx.commit()?);
                }
                if id_field == TurnIdField::None {
                    match &pending {
                        // A repeated or continued Stop: its turn was already queued.
                        None => return Ok(tx.commit()?),
                        Some(p) => native_id = p.0.clone(),
                    }
                }
                let turn_id = turn_key(project, session, &native_id);
                let existing = tx
                    .query_row(
                        "SELECT 1 FROM jobs WHERE tenant=?1 AND platform=?2 AND turn_id=?3",
                        params![TENANT, platform, turn_id],
                        |_| Ok(()),
                    )
                    .optional()?
                    .is_some();
                if existing {
                    return Ok(tx.commit()?);
                }
                let Some((_, user, started)) = pending.filter(|p| p.0 == native_id) else {
                    return Err(invalid(
                        "Stop has no matching UserPromptSubmit; start a new turn after installing hooks",
                    ));
                };
                let assistant = required(event, "last_assistant_message")?;
                let body = turn_body(
                    project,
                    session,
                    &native_id,
                    &user,
                    assistant,
                    &started,
                    &now(),
                );
                enqueue_in(&tx, platform, &body)?;
                forget(&tx)?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Identifiers, receipts and errors of every job, without chat text.
    pub fn status(&self) -> Result<Vec<Value>, Error> {
        let mut statement = self.db.prepare(
            "SELECT seq,platform,turn_id,status,request_id,error FROM jobs ORDER BY seq",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(json!({"seq": row.get::<_, i64>(0)?, "platform": row.get::<_, String>(1)?,
                      "turn_id": row.get::<_, String>(2)?, "status": row.get::<_, String>(3)?,
                      "request_id": row.get::<_, Option<String>>(4)?, "error": row.get::<_, Option<String>>(5)?}))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Job count per status.
    pub fn counts(&self) -> Result<Map<String, Value>, Error> {
        let mut statement = self
            .db
            .prepare("SELECT status,count(*) FROM jobs GROUP BY status")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, json!(row.get::<_, i64>(1)?)))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    fn jobs(&self, status: &str) -> Result<Vec<Job>, Error> {
        let mut statement = self.db.prepare(
            "SELECT seq,platform,body,request_id FROM jobs WHERE status=?1 ORDER BY seq",
        )?;
        let rows = statement.query_map([status], |row| {
            Ok(Job {
                seq: row.get(0)?,
                platform: row.get(1)?,
                body: row.get(2)?,
                request_id: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    fn accept(&self, seq: i64, request_id: &str) -> Result<(), Error> {
        self.db.execute(
            "UPDATE jobs SET status='accepted',request_id=?1,error=NULL WHERE seq=?2",
            params![request_id, seq],
        )?;
        Ok(())
    }

    fn set_status(&self, seq: i64, status: &str, error: Option<&str>) -> Result<(), Error> {
        self.db.execute(
            "UPDATE jobs SET status=?1,error=?2 WHERE seq=?3",
            params![status, error, seq],
        )?;
        Ok(())
    }

    fn set_error(&self, seq: i64, error: &str) -> Result<(), Error> {
        self.db
            .execute("UPDATE jobs SET error=?1 WHERE seq=?2", params![error, seq])?;
        Ok(())
    }
}

/// Submits dialogue and polls receipts: over HTTP for a standalone uploader, in-process in the daemon.
pub trait Sender {
    fn send(
        &self,
        name: &'static str,
        body: Value,
        platform: &str,
    ) -> impl Future<Output = Result<Value, CallError>> + Send;
}

impl Sender for Daemon {
    async fn send(
        &self,
        name: &'static str,
        body: Value,
        platform: &str,
    ) -> Result<Value, CallError> {
        self.post(name, &body, platform).await
    }
}

/// Run a short outbox operation off the async workers; SQLite may wait on a hook's write lock.
async fn with<T: Send + 'static>(
    outbox: &Arc<Mutex<Outbox>>,
    run: impl FnOnce(&Outbox) -> Result<T, Error> + Send + 'static,
) -> Result<T, Error> {
    let outbox = outbox.clone();
    tokio::task::spawn_blocking(move || run(&outbox.lock().unwrap_or_else(PoisonError::into_inner)))
        .await
        .map_err(|_| invalid("outbox operation stopped"))?
}

/// One upload and poll pass; returns the number of errors. Only one uploader may operate an
/// outbox (see [`UploaderLock`]); capture transactions stay short.
pub async fn drain_once<S: Sender>(
    outbox: &Arc<Mutex<Outbox>>,
    sender: &S,
) -> Result<usize, Error> {
    let mut errors = 0;
    let pending = with(outbox, |o| o.jobs("pending")).await?;
    for job in pending {
        let seq = job.seq;
        let Ok(body) = serde_json::from_str::<Value>(&job.body) else {
            with(outbox, move |o| o.set_error(seq, "JSONDecodeError")).await?;
            errors += 1;
            break;
        };
        match sender.send("add_memories", body, &job.platform).await {
            Ok(receipt) => match required(&receipt, "requestId") {
                Ok(request_id) => {
                    let request_id = request_id.to_string();
                    with(outbox, move |o| o.accept(seq, &request_id)).await?;
                }
                Err(e) => {
                    with(outbox, move |o| o.set_error(seq, e.kind())).await?;
                    errors += 1;
                    break;
                }
            },
            Err(CallError::Status(code)) => {
                let permanent = (400..500).contains(&code) && code != 408 && code != 429;
                let status = if permanent { "rejected" } else { "pending" };
                with(outbox, move |o| {
                    o.set_status(seq, status, Some(&format!("HTTP {code}")))
                })
                .await?;
                errors += 1;
                if !permanent {
                    break; // Preserve submission order after a lost response / outage.
                }
            }
            Err(e) => {
                with(outbox, move |o| o.set_error(seq, e.kind())).await?;
                errors += 1;
                break;
            }
        }
    }
    let accepted = with(outbox, |o| o.jobs("accepted")).await?;
    for job in accepted {
        let seq = job.seq;
        let request = json!({"requestId": job.request_id});
        let task = match sender.send("get_task", request, &job.platform).await {
            Ok(task) => task,
            Err(e) => {
                with(outbox, move |o| o.set_error(seq, e.kind())).await?;
                errors += 1;
                continue;
            }
        };
        let Some(status) = task
            .get("status")
            .and_then(Value::as_str)
            .filter(|s| TASK_STATUSES.contains(s))
        else {
            with(outbox, move |o| o.set_error(seq, "ValueError")).await?;
            errors += 1;
            continue;
        };
        // Fact receipts expose formation and index visibility; explicit instruction
        // receipts complete synchronously without these extra fields.
        let complete = status == "completed"
            && ["formed", "searchable"]
                .iter()
                .all(|k| task.get(*k).is_none_or(|v| v == &Value::Bool(true)));
        let failed = status == "failed";
        let (next, error) = if complete {
            ("completed", None)
        } else if failed {
            ("failed", Some("formation failed; inspect get_task"))
        } else {
            ("accepted", None)
        };
        with(outbox, move |o| o.set_status(seq, next, error)).await?;
        errors += usize::from(failed);
    }
    Ok(errors)
}

/// The cross-process advisory lock that excludes a second uploader (not hooks) from an outbox.
/// It is the same `flock` on `<outbox>.uploader.lock` that `harness_memory.py drain` takes.
pub struct UploaderLock(#[allow(dead_code)] File);

impl UploaderLock {
    /// The lock, or None while another uploader holds it.
    pub fn try_acquire(outbox: &Path) -> Result<Option<Self>, Error> {
        let mut path = expand(outbox).into_os_string();
        path.push(".uploader.lock");
        let path = PathBuf::from(path);
        private_file(&path)?;
        let file = std::fs::OpenOptions::new().append(true).open(&path)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self(file))),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(e)) => Err(e.into()),
        }
    }
}

/// Standalone uploader (`outbox drain`); returns the process exit code.
pub async fn drain(
    path: &Path,
    url: &str,
    timeout: Duration,
    interval: Duration,
    once: bool,
) -> Result<i32, Error> {
    let outbox = Arc::new(Mutex::new(Outbox::open(path)?));
    let Some(_lock) = UploaderLock::try_acquire(path)? else {
        return Err(invalid("another uploader holds this outbox"));
    };
    let daemon = Daemon::new(url, timeout).map_err(|e| invalid(format!("--url: {}", e.kind())))?;
    loop {
        let errors = drain_once(&outbox, &daemon).await?;
        if once {
            let statuses = with(&outbox, |o| o.counts()).await?;
            println!("{}", json!({"statuses": statuses, "errors": errors}));
            return Ok(i32::from(errors > 0));
        }
        tokio::time::sleep(interval).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn outbox(dir: &tempfile::TempDir) -> Outbox {
        Outbox::open(&dir.path().join("outbox.sqlite3")).unwrap()
    }

    fn bodies(o: &Outbox) -> Vec<Value> {
        o.jobs("pending")
            .unwrap()
            .into_iter()
            .map(|j| serde_json::from_str(&j.body).unwrap())
            .collect()
    }

    fn contents(body: &Value) -> Vec<&str> {
        body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["content"].as_str().unwrap())
            .collect()
    }

    fn notify(turn: &str, assistant: &str) -> Value {
        json!({"type": "agent-turn-complete", "thread-id": "s1", "turn-id": turn,
               "input-messages": ["Keep the source evidence."], "last-assistant-message": assistant})
    }

    #[test]
    fn turn_keys_quote_every_reserved_character() {
        assert_eq!(turn_key("p", "s 1", "a/b?"), "p/s%201/a%2Fb%3F");
        assert_eq!(turn_key("名", "-_.~", "x"), "%E5%90%8D/-_.~/x");
    }

    fn imported(run: &str, turn: &str) -> Value {
        json!({"scope": {"project": "project", "session": "s1"}, "turnId": format!("project/s1/{turn}"),
               "importRunId": run,
               "messages": [{"role": "user", "content": "Old prompt.", "timestamp": "2026-01-02T03:04:05Z"},
                            {"role": "assistant", "content": "Old reply.", "timestamp": "2026-01-02T03:04:09Z"}]})
    }

    #[test]
    fn imported_turns_never_replace_a_job_of_the_same_turn() {
        let dir = tempfile::tempdir().unwrap();
        let mut o = outbox(&dir);
        // Live capture queued t1 first.
        let hook = |name: &str, extra: Value| {
            let mut e = json!({"hook_event_name": name, "session_id": "s1", "prompt_id": "t1"});
            e.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            e
        };
        o.capture_hook(
            &hook("UserPromptSubmit", json!({"prompt": "live prompt"})),
            "project",
            "claude-code",
            None,
        )
        .unwrap();
        o.capture_hook(
            &hook("Stop", json!({"last_assistant_message": "live reply"})),
            "project",
            "claude-code",
            None,
        )
        .unwrap();
        let held = o
            .enqueue_import("claude-code", &imported("ir1", "t1"))
            .unwrap();
        assert_eq!(
            held,
            Queued::Held {
                run: None,
                status: "pending".into(),
                request_id: None
            }
        );
        assert_eq!(
            o.enqueue_import("claude-code", &imported("ir1", "t2"))
                .unwrap(),
            Queued::Added
        );
        // A resumed apply finds its own job; a later run finds the first run's.
        assert_eq!(
            o.enqueue_import("claude-code", &imported("ir1", "t2"))
                .unwrap(),
            Queued::Again
        );
        assert!(matches!(
            o.enqueue_import("claude-code", &imported("ir2", "t2")).unwrap(),
            Queued::Held { run: Some(run), .. } if run == "ir1"
        ));
        // Another platform's turn of the same id is a different turn.
        assert_eq!(
            o.enqueue_import("codex", &imported("ir1", "t1")).unwrap(),
            Queued::Added
        );
        let jobs = bodies(&o);
        assert_eq!(contents(&jobs[0]), ["live prompt", "live reply"]);
        assert_eq!(contents(&jobs[1]), ["Old prompt.", "Old reply."]);
        assert_eq!(jobs[1]["sync"], false);
        assert_eq!(o.backlog().unwrap(), 3);
        assert!(o.enqueue_import("codex", &json!({"turnId": "x"})).is_err());
    }

    #[test]
    fn rollback_cancels_only_the_runs_unsubmitted_jobs() {
        let dir = tempfile::tempdir().unwrap();
        let mut o = outbox(&dir);
        for (run, turn) in [("ir1", "t1"), ("ir1", "t2"), ("ir2", "t3")] {
            o.enqueue_import("codex", &imported(run, turn)).unwrap();
        }
        o.capture_notify(&notify("t4", "reply"), "project", "codex")
            .unwrap();
        let first = o.jobs("pending").unwrap()[0].seq;
        o.accept(first, "rq1").unwrap();
        assert_eq!(o.cancel_import("ir1").unwrap(), 1);
        let statuses: Vec<(String, String)> = o
            .status()
            .unwrap()
            .iter()
            .map(|j| {
                (
                    j["turn_id"].as_str().unwrap().into(),
                    j["status"].as_str().unwrap().into(),
                )
            })
            .collect();
        assert_eq!(
            statuses,
            [
                ("project/s1/t1".to_string(), "accepted".to_string()),
                ("project/s1/t2".into(), "cancelled".into()),
                ("project/s1/t3".into(), "pending".into()),
                ("project/s1/t4".into(), "pending".into()),
            ]
        );
        assert_eq!(o.backlog().unwrap(), 3);
        // The uploader never sends a cancelled job.
        assert_eq!(o.jobs("pending").unwrap().len(), 2);
    }

    #[test]
    fn notify_keeps_the_first_payload_across_reopening() {
        let dir = tempfile::tempdir().unwrap();
        outbox(&dir)
            .capture_notify(&notify("t1", "I saved the decision."), "project", "codex")
            .unwrap();
        let original = bodies(&outbox(&dir));
        let mut reopened = outbox(&dir);
        reopened
            .capture_notify(&notify("t1", "a changed retry"), "project", "codex")
            .unwrap();
        let after = bodies(&reopened);
        assert_eq!(after, original);
        assert_eq!(after[0]["turnId"], "project/s1/t1");
        assert_eq!(after[0]["sync"], false);
    }

    #[test]
    fn hook_pairs_prompt_and_final_text_and_discards_incomplete_turn() {
        let dir = tempfile::tempdir().unwrap();
        let mut o = outbox(&dir);
        let mut capture = |event: Value| {
            o.capture_hook(&event, "project", "claude-code", None)
                .unwrap()
        };
        let hook = |name: &str, turn: &str, extra: Value| {
            let mut e = json!({"hook_event_name": name, "session_id": "s1", "prompt_id": turn});
            e.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            e
        };
        capture(hook(
            "UserPromptSubmit",
            "p1",
            json!({"prompt": "interrupted prompt"}),
        ));
        capture(hook("StopFailure", "p1", json!({})));
        capture(hook(
            "UserPromptSubmit",
            "p2",
            json!({"prompt": "completed prompt"}),
        ));
        capture(hook(
            "Stop",
            "p2",
            json!({"last_assistant_message": "final reply"}),
        ));
        capture(hook("SessionEnd", "p2", json!({})));
        let jobs = bodies(&o);
        assert_eq!(contents(&jobs[0]), ["completed prompt", "final reply"]);
        assert_eq!(jobs[1]["sessionEnded"], true);
        assert_eq!(jobs.len(), 2);
    }

    #[test]
    fn hosts_without_turn_ids_pair_stop_with_the_open_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let mut o = outbox(&dir);
        let event = |name: &str, extra: Value| {
            let mut e = json!({"hook_event_name": name, "session_id": "s1"});
            e.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            e
        };
        for e in [
            event(
                "Stop",
                json!({"last_assistant_message": "nothing open yet"}),
            ),
            event("UserPromptSubmit", json!({"prompt": "interrupted prompt"})),
            event("StopFailure", json!({})),
            event(
                "Stop",
                json!({"last_assistant_message": "reply to a discarded prompt"}),
            ),
            event("UserPromptSubmit", json!({"prompt": "first prompt"})),
            event("Stop", json!({"last_assistant_message": "first reply"})),
            event(
                "Stop",
                json!({"stop_hook_active": true, "last_assistant_message": "continued reply"}),
            ),
            event("UserPromptSubmit", json!({"prompt": "second prompt"})),
            event("Stop", json!({"last_assistant_message": "second reply"})),
            event("SessionEnd", json!({})),
        ] {
            o.capture_hook(&e, "project", "workbuddy", Some(TurnIdField::None))
                .unwrap();
        }
        let jobs = bodies(&o);
        assert_eq!(contents(&jobs[0]), ["first prompt", "first reply"]);
        assert_eq!(contents(&jobs[1]), ["second prompt", "second reply"]);
        assert_ne!(jobs[0]["turnId"], jobs[1]["turnId"]);
        assert!(jobs[0]["turnId"]
            .as_str()
            .unwrap()
            .starts_with("project/s1/prompt-"));
        assert_eq!(jobs[2]["sessionEnded"], true);
        assert_eq!(jobs.len(), 3);
        // Hosts that do send ids keep the strict pairing contract.
        let error = o
            .capture_hook(
                &event("UserPromptSubmit", json!({"prompt": "x"})),
                "project",
                "claude-code",
                None,
            )
            .unwrap_err();
        assert!(error.to_string().contains("prompt_id"), "{error}");
    }

    #[test]
    fn codex_hooks_pair_by_turn_id_and_skip_a_turn_without_a_final_message() {
        let dir = tempfile::tempdir().unwrap();
        let mut o = outbox(&dir);
        let event = |name: &str, turn: &str, extra: Value| {
            let mut e = json!({"hook_event_name": name, "session_id": "s1", "turn_id": turn,
                               "transcript_path": null, "cwd": "/w", "model": "m",
                               "permission_mode": "default"});
            e.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            e
        };
        for e in [
            event(
                "UserPromptSubmit",
                "t1",
                json!({"prompt": "only run the tool"}),
            ),
            event(
                "Stop",
                "t1",
                json!({"stop_hook_active": false, "last_assistant_message": null}),
            ),
            event(
                "UserPromptSubmit",
                "t2",
                json!({"prompt": "Keep the source."}),
            ),
            event(
                "Stop",
                "t2",
                json!({"stop_hook_active": false, "last_assistant_message": "Kept."}),
            ),
            event("SessionEnd", "t2", json!({"reason": "other"})),
        ] {
            o.capture_hook(&e, "project", "codex", None).unwrap();
        }
        let jobs = bodies(&o);
        assert_eq!(contents(&jobs[0]), ["Keep the source.", "Kept."]);
        assert_eq!(jobs[0]["turnId"], "project/s1/t2");
        assert_eq!(jobs[1]["sessionEnded"], true);
        assert_eq!(jobs.len(), 2);
        // A Stop without the field at all is still a broken event.
        let missing = o.capture_hook(
            &event("Stop", "t3", json!({"stop_hook_active": false})),
            "project",
            "codex",
            None,
        );
        assert!(missing.is_err());
    }

    #[test]
    fn a_prompt_without_text_never_pairs_its_reply_with_an_older_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let mut o = outbox(&dir);
        for e in [
            json!({"hook_event_name": "UserPromptSubmit", "session_id": "s1", "prompt": "a turn that failed without Stop"}),
            json!({"hook_event_name": "UserPromptSubmit", "session_id": "s1", "prompt": "  "}),
            json!({"hook_event_name": "Stop", "session_id": "s1", "last_assistant_message": "reply to the image"}),
        ] {
            o.capture_hook(&e, "project", "workbuddy", Some(TurnIdField::None))
                .unwrap();
        }
        assert!(o.status().unwrap().is_empty());
        let open: i64 =
            o.db.query_row("SELECT count(*) FROM prompts", [], |r| r.get(0))
                .unwrap();
        assert_eq!(open, 0);
    }

    #[test]
    fn enqueue_refuses_explicit_text_and_empty_turns() {
        let dir = tempfile::tempdir().unwrap();
        let mut o = outbox(&dir);
        let scope = json!({"project": "p", "session": "s"});
        for (body, message) in [
            (
                json!({"scope": scope, "turnId": "t", "text": "x"}),
                "not explicit text",
            ),
            (
                json!({"scope": scope, "turnId": "t", "messages": []}),
                "messages or sessionEnded",
            ),
            (
                json!({"scope": {"project": "p"}, "turnId": "t", "sessionEnded": true}),
                "session",
            ),
            (json!([1]), "JSON object"),
        ] {
            let error = o.enqueue("codex", &body).unwrap_err();
            assert!(error.to_string().contains(message), "{error}");
        }
        o.enqueue(
            "codex",
            &json!({"scope": scope, "turnId": "t", "sessionEnded": true}),
        )
        .unwrap();
        assert_eq!(o.status().unwrap().len(), 1);
    }

    /// Answers add_memories and get_task like a daemon; the first add_memories response is lost.
    struct Fake {
        requests: Mutex<Vec<(String, Value)>>,
        lose_first: AtomicBool,
        task_status: Mutex<&'static str>,
        add_error: Option<u16>,
    }

    impl Fake {
        fn new(add_error: Option<u16>) -> Self {
            Self {
                requests: Mutex::default(),
                lose_first: AtomicBool::new(true),
                task_status: Mutex::new("indexing"),
                add_error,
            }
        }
    }

    impl Sender for Fake {
        async fn send(
            &self,
            name: &'static str,
            body: Value,
            _platform: &str,
        ) -> Result<Value, CallError> {
            self.requests.lock().unwrap().push((name.into(), body));
            if name == "add_memories" {
                if let Some(code) = self.add_error {
                    return Err(CallError::Status(code));
                }
                if self.lose_first.swap(false, Ordering::SeqCst) {
                    return Err(CallError::Connection); // persisted, but the response was lost
                }
                return Ok(json!({"requestId": "rq-test"}));
            }
            let status = *self.task_status.lock().unwrap();
            Ok(json!({"status": status, "formed": true, "searchable": status == "completed"}))
        }
    }

    fn status_of(o: &Arc<Mutex<Outbox>>) -> String {
        o.lock().unwrap().status().unwrap()[0]["status"]
            .as_str()
            .unwrap()
            .into()
    }

    #[tokio::test]
    async fn lost_response_resends_same_key_then_polls_without_resubmitting() {
        let dir = tempfile::tempdir().unwrap();
        let mut o = outbox(&dir);
        o.capture_notify(&notify("t1", "I saved the decision."), "p", "codex")
            .unwrap();
        let o = Arc::new(Mutex::new(o));
        let fake = Fake::new(None);
        assert_eq!(drain_once(&o, &fake).await.unwrap(), 1);
        assert_eq!(drain_once(&o, &fake).await.unwrap(), 0);
        assert_eq!(status_of(&o), "accepted");
        *fake.task_status.lock().unwrap() = "completed";
        drain_once(&o, &fake).await.unwrap();
        assert_eq!(status_of(&o), "completed");
        let requests = fake.requests.lock().unwrap();
        let adds: Vec<_> = requests.iter().filter(|r| r.0 == "add_memories").collect();
        assert_eq!(adds.len(), 2);
        assert_eq!(adds[0], adds[1]);
    }

    #[tokio::test]
    async fn http_errors_keep_the_input_locally() {
        for (code, expected) in [(400, "rejected"), (503, "pending"), (429, "pending")] {
            let dir = tempfile::tempdir().unwrap();
            let mut o = outbox(&dir);
            o.capture_notify(&notify("t1", "reply"), "p", "codex")
                .unwrap();
            let o = Arc::new(Mutex::new(o));
            assert_eq!(drain_once(&o, &Fake::new(Some(code))).await.unwrap(), 1);
            let row = o.lock().unwrap().status().unwrap()[0].clone();
            assert_eq!(row["status"], expected);
            assert_eq!(row["error"], format!("HTTP {code}"));
            let kept = o.lock().unwrap().jobs(expected).unwrap();
            let body: Value = serde_json::from_str(&kept[0].body).unwrap();
            assert_eq!(contents(&body), ["Keep the source evidence.", "reply"]);
        }
    }

    #[test]
    fn a_second_uploader_is_excluded_until_the_first_releases_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("outbox.sqlite3");
        let first = UploaderLock::try_acquire(&path).unwrap();
        assert!(first.is_some());
        assert!(UploaderLock::try_acquire(&path).unwrap().is_none());
        drop(first);
        assert!(UploaderLock::try_acquire(&path).unwrap().is_some());
    }
}
