//! Claude Code session transcripts (docs/design/memory-import.md §4, §6.3, §6.10):
//! `<config>/projects/<encoded dir>/<session>.jsonl`, one JSON record per line, appended while the session runs.
//! Each worktree and subdirectory has a directory of its own, so every directory is looked at: it belongs to a
//! workspace when the first `cwd` in the first 16 KiB of its first transcript lies in the workspace's repository,
//! and only then are its transcripts read whole (each checked the same way). Subdirectories (subagent
//! transcripts, tool output) are never read.
//!
//! A turn opens at a `user` record with `turnOrigin` (`origin.kind` where only that is given) and runs to the next
//! one: the prompt is that record's text, the reply the text blocks of the turn's last assistant message that has
//! any. Only `human` turns are imported. Tool results, hook context (`attachment` records), meta and command
//! records carry no `turnOrigin` and open no turn; sidechain records are left out. A transcript whose prompts carry
//! no `turnOrigin` at all predates the field and is `unknown-format`. The session id and turn id are the
//! `sessionId` and `promptId` capture files the same turn under.
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use super::claude::{HOST, SNIFF_BYTES};
use super::project::{repository_root, within};
use super::session::{
    content_text, finish, first_cwd, read_lines, SessionAdapter, SessionDoc, SessionScan, Turn,
    MAX_TRANSCRIPT,
};
use super::source::{entries, timestamp, ScanContext};
use crate::harness::{invalid, Error};

pub const VIA: &str = "claude-transcript";
/// The `turnOrigin` of a turn a person asked.
const HUMAN: &str = "human";

pub struct ClaudeSessions;

impl SessionAdapter for ClaudeSessions {
    fn scan_sessions(&self, ctx: &ScanContext) -> Result<SessionScan, Error> {
        ctx.require_workspaces(HOST)?;
        let config = ctx
            .claude_config_dir
            .clone()
            .unwrap_or_else(|| ctx.home.join(".claude"));
        let projects = config.join("projects");
        let root = std::fs::canonicalize(&projects).map_err(|_| {
            invalid(format!(
                "no Claude Code projects directory at {}",
                ctx.display(&projects)
            ))
        })?;
        let mut out = SessionScan::default();
        let listing =
            entries(&projects).map_err(|e| invalid(format!("{}: {e}", ctx.display(&projects))))?;
        for (_, dir) in listing {
            let Ok(meta) = std::fs::symlink_metadata(&dir) else {
                continue;
            };
            let shown = ctx.display(&dir) + "/";
            if meta.file_type().is_symlink() {
                out.issue(shown, "symlink");
                continue;
            }
            if !meta.is_dir() {
                continue;
            }
            match std::fs::canonicalize(&dir) {
                Ok(real) if real.starts_with(&root) && real != root => {}
                _ => {
                    out.issue(shown, "outside-root");
                    continue;
                }
            }
            let transcripts: Vec<PathBuf> = match entries(&dir) {
                Ok(listing) => listing
                    .into_iter()
                    .filter(|(name, _)| name.ends_with(".jsonl") && !name.starts_with('.'))
                    .map(|(_, path)| path)
                    .collect(),
                Err(e) => {
                    out.issue(shown, format!("unreadable: {e}"));
                    continue;
                }
            };
            // A directory is named after the directory its sessions started in: one look decides for all of them.
            match transcripts.iter().find_map(|t| first_cwd(t, SNIFF_BYTES)) {
                Some(cwd) if within(&repository_root(&cwd), &ctx.workspaces) => {}
                Some(_) => {
                    out.issue(shown, "other-project");
                    continue;
                }
                None if transcripts.is_empty() => continue,
                None => {
                    out.issue(shown, "cwd-unresolved");
                    continue;
                }
            }
            for path in transcripts {
                let shown = ctx.display(&path);
                match transcript(ctx, &path) {
                    Ok(mut doc) => {
                        doc.display_path = shown;
                        out.sessions.push(doc);
                    }
                    Err(reason) => out.issue(shown, reason),
                }
            }
        }
        Ok(out)
    }
}

/// A turn being read.
struct Open {
    turn: Turn,
    origin: String,
    cwd: Option<PathBuf>,
    /// `message.id` of the assistant message the reply was taken from.
    reply_message: Option<String>,
    api_error: bool,
}

impl Open {
    /// The turn with its verdict: only a person's turn in a workspace, with a reply that is no API error.
    fn close(self, ctx: &ScanContext) -> Turn {
        let mut turn = self.turn;
        turn.skip = if self.origin != HUMAN {
            Some(format!("turnOrigin:{}", self.origin))
        } else if turn.native_id.is_empty() {
            Some("unknown-format".into())
        } else if self
            .cwd
            .is_some_and(|cwd| !within(&repository_root(&cwd), &ctx.workspaces))
        {
            Some("other-project".into())
        } else if turn.reply.trim().is_empty() {
            Some("no-reply".into())
        } else if self.api_error {
            Some("api-error".into())
        } else if turn.prompt_at.is_empty() {
            Some("no-time".into())
        } else {
            None
        };
        finish(turn)
    }
}

/// `origin.kind` spelled as `turnOrigin` spells it.
fn origin_of(record: &Value) -> Option<String> {
    record["turnOrigin"]
        .as_str()
        .or_else(|| record["origin"]["kind"].as_str())
        .map(|origin| origin.replace('-', "_"))
}

/// Whether a `user` record without `turnOrigin` reads like a prompt: text a person may have typed, not a tool
/// result or a meta record. Only counted, to tell a transcript older than `turnOrigin`.
fn untagged_prompt(record: &Value) -> bool {
    record.get("toolUseResult").is_none()
        && record["isMeta"] != true
        && !content_text(&record["message"]["content"])
            .trim()
            .is_empty()
}

/// One transcript, read whole, or the reason it is not imported.
fn transcript(ctx: &ScanContext, path: &Path) -> Result<SessionDoc, String> {
    let mut session: Option<String> = None;
    let mut version: Option<String> = None;
    let mut cwd: Option<PathBuf> = None;
    let mut started_at = String::new();
    let mut turns = Vec::new();
    let mut open: Option<Open> = None;
    let (mut tagged, mut untagged) = (false, 0usize);
    let file_hash = read_lines(path, MAX_TRANSCRIPT, |line| {
        if !line.contains("\"user\"") && !line.contains("\"assistant\"") {
            return true;
        }
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            return true; // a line cut while the session was writing it
        };
        let kind = record["type"].as_str().unwrap_or_default();
        if kind != "user" && kind != "assistant" {
            return true;
        }
        if session.is_none() {
            session = record["sessionId"].as_str().map(str::to_string);
            version = record["version"].as_str().map(str::to_string);
        }
        let at = record["timestamp"]
            .as_str()
            .and_then(timestamp)
            .unwrap_or_default();
        if started_at.is_empty() {
            started_at = at.clone();
        }
        if cwd.is_none() {
            cwd = record["cwd"].as_str().map(PathBuf::from);
        }
        if record["isSidechain"] == true {
            return true;
        }
        if kind == "user" {
            match origin_of(&record) {
                Some(origin) => {
                    tagged = true;
                    if let Some(done) = open.take() {
                        turns.push(done.close(ctx));
                    }
                    open = Some(Open {
                        turn: Turn {
                            native_id: record["promptId"].as_str().unwrap_or_default().into(),
                            prompt: content_text(&record["message"]["content"]),
                            prompt_at: at,
                            ..Turn::default()
                        },
                        origin,
                        cwd: record["cwd"].as_str().map(PathBuf::from),
                        reply_message: None,
                        api_error: false,
                    });
                }
                None => untagged += usize::from(untagged_prompt(&record)),
            }
            return true;
        }
        let Some(turn) = open.as_mut() else {
            return true;
        };
        let text = content_text(&record["message"]["content"]);
        if text.trim().is_empty() {
            return true;
        }
        let message = record["message"]["id"].as_str().map(str::to_string);
        if message.is_some() && message == turn.reply_message {
            turn.turn.reply = format!("{}\n\n{text}", turn.turn.reply);
        } else {
            turn.turn.reply = text;
        }
        turn.reply_message = message;
        turn.turn.reply_at = at;
        turn.api_error = record["isApiErrorMessage"] == true;
        true
    })?;
    if let Some(done) = open.take() {
        turns.push(done.close(ctx));
    }
    let Some(session) = session.filter(|s| !s.is_empty()) else {
        return Err(match untagged {
            0 => "empty".into(),
            _ => "unknown-format".into(),
        });
    };
    if !tagged {
        return Err(match untagged {
            0 => "empty".into(),
            _ => "unknown-format".into(),
        });
    }
    Ok(SessionDoc {
        host: HOST.into(),
        via: VIA,
        path: path.to_path_buf(),
        display_path: String::new(),
        session,
        cwd: cwd.unwrap_or_default(),
        file_hash,
        started_at,
        turns,
        extra: json!({ "version": version }),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::harness::import::project::claude_dir_name;
    use crate::harness::import::source::Issue;

    /// A transcript record: `user` or `assistant`, with the session, time and cwd capture would see.
    pub(crate) fn record(kind: &str, at: &str, cwd: &Path, fields: Value) -> String {
        let mut r = json!({"type": kind, "sessionId": "sess-1", "timestamp": at, "cwd": cwd,
                           "isSidechain": false, "version": "2.1.306", "uuid": "u"});
        r.as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        r.to_string()
    }

    pub(crate) fn prompt(at: &str, cwd: &Path, origin: &str, id: &str, text: &str) -> String {
        record(
            "user",
            at,
            cwd,
            json!({"turnOrigin": origin, "promptId": id,
                   "message": {"role": "user", "content": text}}),
        )
    }

    pub(crate) fn reply(at: &str, cwd: &Path, message: &str, blocks: Value) -> String {
        record(
            "assistant",
            at,
            cwd,
            json!({"message": {"id": message, "role": "assistant", "content": blocks}}),
        )
    }

    pub(crate) fn tool_result(at: &str, cwd: &Path, id: &str) -> String {
        record(
            "user",
            at,
            cwd,
            json!({"promptId": id, "toolUseResult": {"stdout": "ok"},
                   "message": {"role": "user", "content": [{"type": "tool_result", "content": "ok"}]}}),
        )
    }

    /// A synthetic workspace with its Claude Code projects directory.
    pub(crate) struct Fixture {
        pub home: tempfile::TempDir,
        pub workspace: PathBuf,
        pub projects: PathBuf,
    }

    impl Fixture {
        pub(crate) fn new() -> Self {
            let home = tempfile::tempdir().unwrap();
            let workspace = home.path().join("code/my-app");
            std::fs::create_dir_all(&workspace).unwrap();
            let projects = home.path().join(".claude/projects");
            std::fs::create_dir_all(&projects).unwrap();
            Fixture {
                workspace: repository_root(&workspace),
                home,
                projects,
            }
        }

        pub(crate) fn ctx(&self) -> ScanContext {
            ScanContext {
                home: self.home.path().to_path_buf(),
                workspaces: vec![self.workspace.clone()],
                ..ScanContext::default()
            }
        }

        /// Write `<projects>/<dir of cwd>/<name>.jsonl`.
        pub(crate) fn transcript(&self, cwd: &Path, name: &str, lines: &[String]) -> PathBuf {
            let dir = self.projects.join(claude_dir_name(cwd));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join(format!("{name}.jsonl"));
            std::fs::write(&path, lines.join("\n") + "\n").unwrap();
            path
        }
    }

    fn issues(out: &SessionScan) -> Vec<(String, String)> {
        let mut found: Vec<_> = out
            .issues
            .iter()
            .map(|Issue { path, reason }| (path.clone(), reason.clone()))
            .collect();
        found.sort();
        found
    }

    #[test]
    fn human_turns_pair_the_prompt_with_the_last_reply_and_the_rest_are_skipped() {
        let f = Fixture::new();
        let w = f.workspace.clone();
        let text = |t: &str| json!([{"type": "text", "text": t}]);
        let lines = vec![
            json!({"type": "queue-operation", "sessionId": "sess-1"}).to_string(),
            // A person's turn: tool use in between, the reply is the last message with text, in two records.
            prompt("2026-03-01T10:00:00Z", &w, "human", "p1",
                   "<system-reminder>\nhost context\n</system-reminder>\nWhere do releases go?"),
            json!({"type": "attachment", "sessionId": "sess-1", "attachment": {"content": "<shared-memory>x</shared-memory>"}}).to_string(),
            reply("2026-03-01T10:00:02Z", &w, "msg-1", json!([{"type": "thinking", "thinking": "hmm"}])),
            reply("2026-03-01T10:00:03Z", &w, "msg-1", text("Let me check.")),
            reply("2026-03-01T10:00:04Z", &w, "msg-1", json!([{"type": "tool_use", "name": "Bash"}])),
            tool_result("2026-03-01T10:00:05Z", &w, "p1"),
            reply("2026-03-01T10:00:06Z", &w, "msg-2", text("Releases go to the staging bucket.")),
            reply("2026-03-01T10:00:07Z", &w, "msg-2", text("Tagged builds go to production.")),
            // A sidechain's records belong to no turn of the main conversation.
            record("assistant", "2026-03-01T10:00:08Z", &w,
                   json!({"isSidechain": true, "message": {"id": "msg-s", "content": text("Subagent says hi.")}})),
            // Not a person's turn: a scripted prompt and a background task's notice.
            prompt("2026-03-01T10:01:00Z", &w, "sdk", "p2", "Run the nightly job."),
            reply("2026-03-01T10:01:05Z", &w, "msg-3", text("Ran it.")),
            record("user", "2026-03-01T10:02:00Z", &w,
                   json!({"origin": {"kind": "task-notification"}, "promptId": "p3",
                          "message": {"content": "<task-notification>done</task-notification>"}})),
            reply("2026-03-01T10:02:05Z", &w, "msg-4", text("The task finished.")),
            // A person's turn without a reply, one ending in an API error, and an image-only prompt.
            prompt("2026-03-01T10:03:00Z", &w, "human", "p4", "Are you there?"),
            reply("2026-03-01T10:03:01Z", &w, "msg-5", json!([{"type": "tool_use", "name": "Read"}])),
            prompt("2026-03-01T10:04:00Z", &w, "human", "p5", "Try again."),
            record("assistant", "2026-03-01T10:04:01Z", &w,
                   json!({"isApiErrorMessage": true, "message": {"id": "msg-6", "content": text("API Error: overloaded")}})),
            record("user", "2026-03-01T10:05:00Z", &w,
                   json!({"turnOrigin": "human", "promptId": "p6",
                          "message": {"content": [{"type": "image", "source": {}}]}})),
            reply("2026-03-01T10:05:01Z", &w, "msg-7", text("A cat.")),
            // A command record opens no turn; the reply after it still belongs to p7.
            prompt("2026-03-01T10:06:00Z", &w, "human", "p7", "Summarize the plan."),
            record("user", "2026-03-01T10:06:01Z", &w,
                   json!({"message": {"content": "<command-name>/compact</command-name>"}})),
            reply("2026-03-01T10:06:02Z", &w, "msg-8", text("The plan has two steps.")),
            "{\"type\":\"user\",\"cut".to_string(),
        ];
        let path = f.transcript(&w, "sess-1", &lines);
        let out = ClaudeSessions.scan_sessions(&f.ctx()).unwrap();
        assert!(out.issues.is_empty(), "{:?}", out.issues);
        let doc = &out.sessions[0];
        assert_eq!(doc.session, "sess-1");
        assert_eq!(doc.via, VIA);
        assert_eq!(doc.host, "claude-code");
        assert_eq!(doc.cwd, w);
        assert_eq!(doc.started_at, "2026-03-01T10:00:00Z");
        assert_eq!(
            doc.display_path,
            format!("~/.claude/projects/{}/sess-1.jsonl", claude_dir_name(&w))
        );
        assert_eq!(
            doc.file_hash,
            blake3::hash(&std::fs::read(&path).unwrap())
                .to_hex()
                .to_string()
        );
        assert_eq!(doc.extra, json!({"version": "2.1.306"}));
        let got: Vec<(&str, Option<&str>)> = doc
            .turns
            .iter()
            .map(|t| (t.native_id.as_str(), t.skip.as_deref()))
            .collect();
        assert_eq!(
            got,
            [
                ("p1", None),
                ("p2", Some("turnOrigin:sdk")),
                ("p3", Some("turnOrigin:task_notification")),
                ("p4", Some("no-reply")),
                ("p5", Some("api-error")),
                ("p6", Some("empty")),
                ("p7", None),
            ]
        );
        let first = &doc.turns[0];
        assert_eq!(first.prompt, "Where do releases go?");
        assert_eq!(
            first.reply,
            "Releases go to the staging bucket.\n\nTagged builds go to production."
        );
        assert_eq!(
            (first.prompt_at.as_str(), first.reply_at.as_str()),
            ("2026-03-01T10:00:00Z", "2026-03-01T10:00:07Z")
        );
        assert_eq!(doc.turns[6].reply, "The plan has two steps.");
    }

    #[test]
    fn only_workspace_transcripts_of_a_known_format_are_read() {
        let f = Fixture::new();
        let w = f.workspace.clone();
        let turn = |cwd: &Path, id: &str| {
            vec![
                prompt("2026-03-01T10:00:00Z", cwd, "human", id, "Keep it."),
                reply(
                    "2026-03-01T10:00:01Z",
                    cwd,
                    "m",
                    json!([{"type": "text", "text": "Kept."}]),
                ),
            ]
        };
        // A worktree of the workspace's repository has its own directory.
        let worktree = w.join(".claude/worktrees/w1");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", w.join(".git/worktrees/w1").display()),
        )
        .unwrap();
        f.transcript(&worktree, "wt", &turn(&worktree, "pw"));
        // Another project, an old transcript without turnOrigin, an empty one and one that is no transcript.
        let other = f.home.path().join("code/other");
        std::fs::create_dir_all(&other).unwrap();
        f.transcript(&other, "o", &turn(&other, "po"));
        let old = record(
            "user",
            "2026-01-01T00:00:00Z",
            &w,
            json!({"message": {"content": "An old prompt."}}),
        );
        f.transcript(&w, "old", &[old]);
        f.transcript(
            &w,
            "blank",
            &[json!({"type": "summary", "summary": "x"}).to_string()],
        );
        f.transcript(&w, "new", &turn(&w, "pn"));
        let subagents = f.projects.join(claude_dir_name(&w)).join("sess/subagents");
        std::fs::create_dir_all(&subagents).unwrap();
        std::fs::write(subagents.join("agent.jsonl"), turn(&w, "pa").join("\n")).unwrap();
        // A directory without a cwd in its first 16 KiB.
        let mystery = f.projects.join("-mystery");
        std::fs::create_dir_all(&mystery).unwrap();
        let filler =
            json!({"type": "summary", "summary": "x".repeat(SNIFF_BYTES as usize)}).to_string();
        std::fs::write(
            mystery.join("m.jsonl"),
            filler + "\n" + &turn(&w, "pm").join("\n"),
        )
        .unwrap();
        #[cfg(unix)]
        {
            let elsewhere = f.home.path().join("elsewhere");
            std::fs::create_dir_all(&elsewhere).unwrap();
            std::os::unix::fs::symlink(&elsewhere, f.projects.join("-linked")).unwrap();
            std::os::unix::fs::symlink(
                f.projects.join(claude_dir_name(&w)).join("new.jsonl"),
                f.projects.join(claude_dir_name(&w)).join("link.jsonl"),
            )
            .unwrap();
        }
        let out = ClaudeSessions.scan_sessions(&f.ctx()).unwrap();
        let mut found: Vec<&str> = out
            .sessions
            .iter()
            .map(|d| d.turns[0].native_id.as_str())
            .collect();
        found.sort();
        assert_eq!(found, ["pn", "pw"]);
        let enc = claude_dir_name(&w);
        let mut expected = vec![
            (
                format!("~/.claude/projects/{enc}/blank.jsonl"),
                "empty".to_string(),
            ),
            (
                format!("~/.claude/projects/{enc}/old.jsonl"),
                "unknown-format".into(),
            ),
            (
                format!("~/.claude/projects/{}/", claude_dir_name(&other)),
                "other-project".into(),
            ),
            (
                "~/.claude/projects/-mystery/".into(),
                "cwd-unresolved".into(),
            ),
        ];
        #[cfg(unix)]
        {
            expected.push((
                format!("~/.claude/projects/{enc}/link.jsonl"),
                "symlink".into(),
            ));
            expected.push(("~/.claude/projects/-linked/".into(), "symlink".into()));
        }
        expected.sort();
        assert_eq!(issues(&out), expected);
        let mut ctx = f.ctx();
        ctx.workspaces.clear();
        assert!(ClaudeSessions.scan_sessions(&ctx).is_err());
    }
}
