//! In-daemon dialogue uploader (`serve --outbox`): drains the local outbox through the public
//! interface in-process. It holds the outbox's uploader lock while it runs, so a standalone
//! `outbox drain` (or the Python uploader) on the same outbox waits, and vice versa.
use serde_json::Value;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::outbox::{drain_once, Outbox, Sender, UploaderLock};
use super::CallError;
use crate::api::http;
use memory_domain::types::{Caller, ClientKind};
use memory_engine::runtime::engine::Engine;

/// Pause between passes. A pass over an idle outbox is two indexed SQLite reads.
pub const INTERVAL: Duration = Duration::from_secs(1);

/// Calls the interface in this process; an API error answers with its HTTP status.
pub struct InProcess(pub Arc<Engine>);

impl Sender for InProcess {
    async fn send(
        &self,
        name: &'static str,
        body: Value,
        platform: &str,
    ) -> Result<Value, CallError> {
        let caller = Caller {
            platform: platform.into(),
            client: ClientKind::Hook,
        };
        http::call(&self.0, &caller, name, body)
            .await
            .map_err(|e| CallError::Status(e.status_code()))
    }
}

/// Drain `outbox` (already opened, so a bad path fails startup) for as long as the daemon runs.
pub async fn run(eng: Arc<Engine>, path: PathBuf, outbox: Outbox) {
    let outbox = Arc::new(Mutex::new(outbox));
    let sender = InProcess(eng);
    let mut lock = None;
    let mut last_problem = String::new();
    loop {
        if lock.is_none() {
            match UploaderLock::try_acquire(&path) {
                Ok(Some(acquired)) => {
                    tracing::info!(outbox = %path.display(), "dialogue uploader started");
                    lock = Some(acquired);
                }
                Ok(None) => note(
                    &mut last_problem,
                    "another uploader holds the outbox lock; retrying",
                ),
                Err(e) => note(&mut last_problem, &format!("cannot lock the outbox: {e}")),
            }
        }
        if lock.is_some() {
            match drain_once(&outbox, &sender).await {
                Ok(0) => last_problem.clear(),
                // Per-job errors stay in the outbox; `agent-memoryd outbox status` shows them.
                Ok(errors) => tracing::debug!(errors, "dialogue upload pass reported errors"),
                Err(e) => note(&mut last_problem, &format!("upload pass failed: {e}")),
            }
        }
        tokio::time::sleep(INTERVAL).await;
    }
}

/// Warn once per distinct problem rather than every pass.
fn note(last: &mut String, problem: &str) {
    if last != problem {
        tracing::warn!("dialogue uploader: {problem}");
        *last = problem.to_string();
    }
}
