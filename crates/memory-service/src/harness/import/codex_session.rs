//! Codex session rollouts (docs/design/memory-import.md §4, §6.3, §6.10): `<codex home>/sessions/YYYY/MM/DD/
//! rollout-*.jsonl` and `<codex home>/archived_sessions/`, zstd-compressed as `rollout-*.jsonl.zst` once older. Each
//! record is `{timestamp, type, payload}`, and a rollout opens with its `session_meta` (`id`, `cwd`, `thread_source`,
//! `cli_version`; a later `session_meta` belongs to history the thread was forked from). Only a `thread_source:
//! user` thread whose `cwd` lies in a workspace is read whole; `subagent`, `guardian_review` and every other source
//! are skipped, and a rollout without `thread_source` (Codex before 0.131) is `unknown-format`.
//!
//! A turn is grouped by `turn_id`: the prompt is the text of its `item_completed` `UserMessage` items, the reply
//! `task_complete.last_agent_message`. As capture does, a turn that was aborted (`turn_aborted`) or ended without a
//! final message is not imported. Developer messages and the injected environment context are `response_item`s
//! and never read. The session id and turn id are the `session_meta.id` and `turn_id` capture files the turn under.
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::codex::HOST;
use super::project::{repository_root, within};
use super::session::{
    content_text, finish, read_lines, SessionAdapter, SessionDoc, SessionScan, Turn, MAX_TRANSCRIPT,
};
use super::source::{entries, timestamp, ScanContext};
use crate::harness::{invalid, Error};

pub const VIA: &str = "codex-rollout";
/// The `thread_source` of a thread a person runs.
const USER: &str = "user";
/// Directories below a sessions root: `YYYY/MM/DD`.
const MAX_DEPTH: usize = 4;

pub struct CodexSessions;

impl SessionAdapter for CodexSessions {
    fn scan_sessions(&self, ctx: &ScanContext) -> Result<SessionScan, Error> {
        ctx.require_workspaces(HOST)?;
        let codex = ctx
            .codex_home
            .clone()
            .unwrap_or_else(|| ctx.home.join(".codex"));
        let roots = [codex.join("sessions"), codex.join("archived_sessions")];
        if !roots.iter().any(|root| root.is_dir()) {
            return Err(invalid(format!(
                "no Codex sessions directory at {}",
                ctx.display(&roots[0])
            )));
        }
        let mut out = SessionScan::default();
        let mut files = Vec::new();
        for root in &roots {
            walk(ctx, root, 0, &mut files, &mut out);
        }
        for path in files {
            let shown = ctx.display(&path);
            match rollout(ctx, &path) {
                Ok(mut doc) => {
                    doc.display_path = shown;
                    out.sessions.push(doc);
                }
                Err(reason) => out.issue(shown, reason),
            }
        }
        Ok(out)
    }
}

/// The rollout files under `dir`, in name order, without following a symbolic link.
fn walk(
    ctx: &ScanContext,
    dir: &Path,
    depth: usize,
    files: &mut Vec<PathBuf>,
    out: &mut SessionScan,
) {
    match std::fs::symlink_metadata(dir) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return out.issue(ctx.display(dir), "symlink");
        }
        Ok(meta) if meta.is_dir() => {}
        _ => return,
    }
    let Ok(listing) = entries(dir) else {
        return out.issue(ctx.display(dir), "unreadable");
    };
    for (name, path) in listing {
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_dir() && depth < MAX_DEPTH {
            walk(ctx, &path, depth + 1, files, out);
        } else if name.starts_with("rollout-")
            && (name.ends_with(".jsonl") || name.ends_with(".jsonl.zst"))
        {
            match meta.file_type().is_symlink() {
                true => out.issue(ctx.display(&path), "symlink"),
                false => files.push(path),
            }
        }
    }
}

/// The rollout's own `session_meta`: its first record.
fn session_meta(path: &Path) -> Result<Value, String> {
    let mut first = None;
    read_lines(path, MAX_TRANSCRIPT, |line| {
        first = serde_json::from_str::<Value>(line).ok();
        false
    })?;
    match first {
        Some(record) if record["type"] == "session_meta" => Ok(record),
        Some(_) => Err("unknown-format".into()),
        None => Err("empty".into()),
    }
}

/// A turn being read.
#[derive(Default)]
struct Open {
    prompts: Vec<String>,
    prompt_at: String,
    reply: Option<String>,
    reply_at: String,
    completed: bool,
    aborted: bool,
}

/// One rollout of a person's thread in a workspace, read whole, or the reason it is not imported.
fn rollout(ctx: &ScanContext, path: &Path) -> Result<SessionDoc, String> {
    let meta = session_meta(path)?;
    let payload = &meta["payload"];
    let source = match payload["thread_source"].as_str() {
        None => return Err("unknown-format".into()),
        Some(source) => source,
    };
    if source != USER {
        return Err(format!("thread_source:{source}"));
    }
    let session = payload["id"].as_str().unwrap_or_default().to_string();
    let cwd = payload["cwd"]
        .as_str()
        .map(PathBuf::from)
        .unwrap_or_default();
    if session.is_empty() || !cwd.is_absolute() {
        return Err("unknown-format".into());
    }
    if !within(&repository_root(&cwd), &ctx.workspaces) {
        return Err("other-project".into());
    }
    let mut order: Vec<String> = Vec::new();
    let mut open: HashMap<String, Open> = HashMap::new();
    let file_hash = read_lines(path, MAX_TRANSCRIPT, |line| {
        if ![
            "\"item_completed\"",
            "\"task_complete\"",
            "\"turn_aborted\"",
        ]
        .iter()
        .any(|kind| line.contains(kind))
        {
            return true;
        }
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            return true;
        };
        let event = &record["payload"];
        let Some(turn_id) = event["turn_id"].as_str().filter(|id| !id.is_empty()) else {
            return true;
        };
        let at = record["timestamp"]
            .as_str()
            .and_then(timestamp)
            .unwrap_or_default();
        let turn = open.entry(turn_id.to_string()).or_insert_with(|| {
            order.push(turn_id.to_string());
            Open::default()
        });
        match event["type"].as_str() {
            Some("item_completed") if event["item"]["type"] == "UserMessage" => {
                let text = content_text(&event["item"]["content"]);
                if !text.trim().is_empty() {
                    if turn.prompts.is_empty() {
                        turn.prompt_at = at;
                    }
                    turn.prompts.push(text);
                }
            }
            Some("task_complete") => {
                turn.completed = true;
                turn.reply = event["last_agent_message"].as_str().map(str::to_string);
                turn.reply_at = at;
            }
            Some("turn_aborted") => turn.aborted = true,
            _ => {}
        }
        true
    })?;
    let turns = order
        .into_iter()
        .map(|id| {
            let o = open.remove(&id).unwrap_or_default();
            let reply = o.reply.unwrap_or_default();
            let skip = if o.aborted {
                Some("aborted")
            } else if o.prompts.is_empty() {
                Some("no-prompt")
            } else if !o.completed || reply.trim().is_empty() {
                Some("no-reply")
            } else if o.prompt_at.is_empty() {
                Some("no-time")
            } else {
                None
            };
            finish(Turn {
                native_id: id,
                prompt: o.prompts.join("\n\n"),
                reply,
                reply_at: match o.reply_at.is_empty() {
                    true => o.prompt_at.clone(),
                    false => o.reply_at,
                },
                prompt_at: o.prompt_at,
                skip: skip.map(str::to_string),
            })
        })
        .collect();
    Ok(SessionDoc {
        host: HOST.into(),
        via: VIA,
        path: path.to_path_buf(),
        display_path: String::new(),
        session,
        cwd,
        file_hash,
        started_at: meta["timestamp"]
            .as_str()
            .and_then(timestamp)
            .unwrap_or_default(),
        turns,
        extra: json!({"cliVersion": payload["cli_version"], "threadSource": source}),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::harness::import::source::Issue;
    use std::io::Write;

    pub(crate) fn meta(id: &str, at: &str, cwd: &Path, source: Option<&str>) -> String {
        let mut payload = json!({"id": id, "session_id": id, "timestamp": at, "cwd": cwd,
                                 "cli_version": "0.163.0", "source": "cli"});
        if let Some(source) = source {
            payload["thread_source"] = json!(source);
        }
        json!({"timestamp": at, "type": "session_meta", "payload": payload}).to_string()
    }

    fn event(at: &str, payload: Value) -> String {
        json!({"timestamp": at, "type": "event_msg", "payload": payload}).to_string()
    }

    pub(crate) fn user_message(at: &str, turn: &str, text: &str) -> String {
        event(
            at,
            json!({"type": "item_completed", "turn_id": turn, "thread_id": "t",
                   "item": {"type": "UserMessage", "id": "i", "content": [{"type": "text", "text": text, "text_elements": []}]}}),
        )
    }

    pub(crate) fn complete(at: &str, turn: &str, reply: Option<&str>) -> String {
        event(
            at,
            json!({"type": "task_complete", "turn_id": turn, "last_agent_message": reply}),
        )
    }

    /// A synthetic workspace and Codex home.
    pub(crate) struct Fixture {
        pub home: tempfile::TempDir,
        pub workspace: PathBuf,
    }

    impl Fixture {
        pub(crate) fn new() -> Self {
            let home = tempfile::tempdir().unwrap();
            let workspace = home.path().join("code/my-app");
            std::fs::create_dir_all(&workspace).unwrap();
            std::fs::create_dir_all(home.path().join(".codex/sessions")).unwrap();
            Fixture {
                workspace: repository_root(&workspace),
                home,
            }
        }

        pub(crate) fn ctx(&self) -> ScanContext {
            ScanContext {
                home: self.home.path().to_path_buf(),
                workspaces: vec![self.workspace.clone()],
                ..ScanContext::default()
            }
        }

        /// Write `<codex>/<rel>`, zstd-compressed when it ends in `.zst`.
        pub(crate) fn rollout(&self, rel: &str, lines: &[String]) -> PathBuf {
            let path = self.home.path().join(".codex").join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let text = lines.join("\n") + "\n";
            if rel.ends_with(".zst") {
                let mut encoder =
                    zstd::stream::write::Encoder::new(std::fs::File::create(&path).unwrap(), 3)
                        .unwrap();
                encoder.write_all(text.as_bytes()).unwrap();
                encoder.finish().unwrap();
            } else {
                std::fs::write(&path, text).unwrap();
            }
            path
        }
    }

    #[test]
    fn user_threads_pair_each_turns_prompt_with_its_final_message() {
        let f = Fixture::new();
        let w = f.workspace.clone();
        let lines = vec![
            meta("thr-1", "2026-04-01T09:00:00Z", &w, Some("user")),
            json!({"timestamp": "2026-04-01T09:00:00Z", "type": "response_item",
                   "payload": {"type": "message", "role": "developer", "content": [{"type": "input_text", "text": "<environment_context>x</environment_context>"}]}}).to_string(),
            event("2026-04-01T09:00:01Z", json!({"type": "task_started", "turn_id": "t1"})),
            user_message("2026-04-01T09:00:01Z", "t1", "Which branch ships?"),
            user_message("2026-04-01T09:00:03Z", "t1", "The release one, right?"),
            event("2026-04-01T09:00:04Z", json!({"type": "item_completed", "turn_id": "t1",
                   "item": {"type": "AgentMessage", "content": [{"type": "Text", "text": "Checking."}]}})),
            complete("2026-04-01T09:00:09Z", "t1", Some("The release branch ships.")),
            user_message("2026-04-01T09:01:00Z", "t2", "Run the tool only."),
            complete("2026-04-01T09:01:05Z", "t2", None),
            user_message("2026-04-01T09:02:00Z", "t3", "Start a long job."),
            event("2026-04-01T09:02:30Z", json!({"type": "turn_aborted", "turn_id": "t3", "reason": "interrupted"})),
            event("2026-04-01T09:03:00Z", json!({"type": "task_started", "turn_id": "t4"})),
            complete("2026-04-01T09:03:09Z", "t4", Some("Compacted.")),
            user_message("2026-04-01T09:04:00Z", "t5",
                         "<shared-memory project=\"p\" event=\"UserPromptSubmit\">\nold facts\n</shared-memory>\nAnd tests?"),
            complete("2026-04-01T09:04:09Z", "t5", Some("Tests pass.")),
            user_message("2026-04-01T09:05:00Z", "t6", "Still running?"),
        ];
        let path = f.rollout(
            "sessions/2026/04/01/rollout-2026-04-01T09-00-00-thr-1.jsonl.zst",
            &lines,
        );
        let out = CodexSessions.scan_sessions(&f.ctx()).unwrap();
        assert!(out.issues.is_empty(), "{:?}", out.issues);
        let doc = &out.sessions[0];
        assert_eq!(
            (doc.session.as_str(), doc.host.as_str(), doc.via),
            ("thr-1", "codex", VIA)
        );
        assert_eq!(doc.started_at, "2026-04-01T09:00:00Z");
        assert_eq!(
            doc.extra,
            json!({"cliVersion": "0.163.0", "threadSource": "user"})
        );
        assert_eq!(
            doc.file_hash,
            blake3::hash(&std::fs::read(&path).unwrap())
                .to_hex()
                .to_string()
        );
        let got: Vec<(&str, Option<&str>)> = doc
            .turns
            .iter()
            .map(|t| (t.native_id.as_str(), t.skip.as_deref()))
            .collect();
        assert_eq!(
            got,
            [
                ("t1", None),
                ("t2", Some("no-reply")),
                ("t3", Some("aborted")),
                ("t4", Some("no-prompt")),
                ("t5", None),
                ("t6", Some("no-reply")),
            ]
        );
        let t1 = &doc.turns[0];
        assert_eq!(t1.prompt, "Which branch ships?\n\nThe release one, right?");
        assert_eq!(t1.reply, "The release branch ships.");
        assert_eq!(
            (t1.prompt_at.as_str(), t1.reply_at.as_str()),
            ("2026-04-01T09:00:01Z", "2026-04-01T09:00:09Z")
        );
        assert_eq!(doc.turns[4].prompt, "And tests?");
    }

    #[test]
    fn threads_are_filtered_by_their_source_and_workspace() {
        let f = Fixture::new();
        let w = f.workspace.clone();
        let other = f.home.path().join("code/other");
        std::fs::create_dir_all(&other).unwrap();
        let turn = |id: &str| {
            vec![
                user_message("2026-04-01T09:00:01Z", id, "Keep it."),
                complete("2026-04-01T09:00:02Z", id, Some("Kept.")),
            ]
        };
        let with = |head: String, id: &str| [vec![head], turn(id)].concat();
        f.rollout(
            "sessions/2026/04/01/rollout-a.jsonl",
            &with(meta("a", "2026-04-01T09:00:00Z", &w, Some("user")), "ta"),
        );
        f.rollout(
            "archived_sessions/rollout-b.jsonl.zst",
            &with(meta("b", "2026-04-01T09:00:00Z", &w, Some("user")), "tb"),
        );
        f.rollout(
            "sessions/2026/04/01/rollout-c.jsonl",
            &with(
                meta("c", "2026-04-01T09:00:00Z", &w, Some("subagent")),
                "tc",
            ),
        );
        f.rollout(
            "sessions/2026/04/01/rollout-d.jsonl",
            &with(
                meta("d", "2026-04-01T09:00:00Z", &w, Some("guardian_review")),
                "td",
            ),
        );
        f.rollout(
            "sessions/2026/04/01/rollout-e.jsonl",
            &with(meta("e", "2026-04-01T09:00:00Z", &w, None), "te"),
        );
        f.rollout(
            "sessions/2026/04/01/rollout-f.jsonl",
            &with(
                meta("f", "2026-04-01T09:00:00Z", &other, Some("user")),
                "tf",
            ),
        );
        f.rollout("sessions/2026/04/01/rollout-g.jsonl", &turn("tg"));
        f.rollout("sessions/2026/04/01/notes.jsonl", &turn("th"));
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            f.home
                .path()
                .join(".codex/sessions/2026/04/01/rollout-a.jsonl"),
            f.home
                .path()
                .join(".codex/sessions/2026/04/01/rollout-z.jsonl"),
        )
        .unwrap();
        let out = CodexSessions.scan_sessions(&f.ctx()).unwrap();
        let mut found: Vec<&str> = out.sessions.iter().map(|d| d.session.as_str()).collect();
        found.sort();
        assert_eq!(found, ["a", "b"]);
        let mut issues: Vec<(String, String)> = out
            .issues
            .iter()
            .map(|Issue { path, reason }| (path.clone(), reason.clone()))
            .collect();
        issues.sort();
        let day = "~/.codex/sessions/2026/04/01";
        let mut expected = vec![
            (
                format!("{day}/rollout-c.jsonl"),
                "thread_source:subagent".to_string(),
            ),
            (
                format!("{day}/rollout-d.jsonl"),
                "thread_source:guardian_review".into(),
            ),
            (format!("{day}/rollout-e.jsonl"), "unknown-format".into()),
            (format!("{day}/rollout-f.jsonl"), "other-project".into()),
            (format!("{day}/rollout-g.jsonl"), "unknown-format".into()),
        ];
        #[cfg(unix)]
        expected.push((format!("{day}/rollout-z.jsonl"), "symlink".into()));
        expected.sort();
        assert_eq!(issues, expected);
        let mut ctx = f.ctx();
        ctx.codex_home = Some(f.home.path().join("nowhere"));
        assert!(CodexSessions.scan_sessions(&ctx).is_err());
    }
}
