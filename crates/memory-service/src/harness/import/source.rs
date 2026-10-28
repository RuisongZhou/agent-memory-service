//! What a host adapter hands the planner (docs/design/memory-import.md §6.2–§6.5): documents read from
//! the memory files a host keeps, each cut into segments, and the files it skipped with a reason.
//! Adapters only read files; nothing here talks to the network or the daemon. Every path an adapter
//! reports is in `~/…` display form, never the absolute home path.
use chrono::{DateTime, Utc};
use memory_rules::util::{fmt_ts, parse_ts};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fs::{File, Metadata};
use std::io::Read;
use std::path::{Path, PathBuf};

use super::session::SessionDoc;
use crate::harness::{invalid, Error};

/// Where to look and what to keep.
#[derive(Debug, Clone, Default)]
pub struct ScanContext {
    /// The home directory host defaults and `~/` display paths are relative to.
    pub home: PathBuf,
    /// Claude Code's configuration directory (default `~/.claude`).
    pub claude_config_dir: Option<PathBuf>,
    /// Codex's home (default `~/.codex`).
    pub codex_home: Option<PathBuf>,
    /// WorkBuddy's data directory (default `~/.workbuddy`).
    pub workbuddy_home: Option<PathBuf>,
    /// The `--workspace` directories, already resolved to repository roots (`harness::project::repository_root`).
    pub workspaces: Vec<PathBuf>,
    /// Claude Code: every project directory whose transcripts name a workspace, not only the workspace's own.
    pub all: bool,
    /// Agent export: the JSONL file.
    pub from: Option<PathBuf>,
    /// Agent export: the platform label.
    pub label: Option<String>,
}

impl ScanContext {
    /// `path` in `~/…` form when it is under the home directory.
    pub fn display(&self, path: &Path) -> String {
        display(&self.home, path)
    }

    /// Hosts whose memories belong to projects need the workspaces that say which project is this one.
    pub fn require_workspaces(&self, host: &str) -> Result<(), Error> {
        if self.workspaces.is_empty() {
            return Err(invalid(format!(
                "--host {host} needs --workspace DIR: its memories are filed by project directory"
            )));
        }
        Ok(())
    }
}

/// How the planner treats a segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// One record when the text fits the unit cap, else extraction (`split` with `--no-extract`).
    VerbatimIfFits,
    /// Extraction whatever its size (`verbatim` when it fits, else `split`, with `--no-extract`).
    AlwaysExtract,
}

/// One piece of a document that becomes one import unit.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    /// Position in the document, unique within it; the last part of the unit key.
    pub locator: String,
    pub heading_path: Vec<String>,
    pub text: String,
    pub policy: Policy,
    /// Per-segment overrides of the document's values (an export line carries its own).
    pub stated_at: Option<String>,
    pub kind_hint: Option<String>,
    /// Merged into the unit's `source` after the document's `extra`.
    pub extra: Value,
}

impl Segment {
    /// A segment with the document's time and kind.
    pub fn new(locator: String, heading_path: Vec<String>, text: String, policy: Policy) -> Self {
        Segment {
            locator,
            heading_path,
            text,
            policy,
            stated_at: None,
            kind_hint: None,
            extra: Value::Null,
        }
    }
}

/// One memory file of a host.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceDoc {
    /// Platform label of the host the memory came from.
    pub host: String,
    /// claude-auto-memory | codex-rollout-summary | workbuddy-memory | agent-export
    pub via: &'static str,
    pub path: PathBuf,
    /// `path` in `~/…` form.
    pub display_path: String,
    pub native_id: String,
    pub title: Option<String>,
    pub cwd: Option<PathBuf>,
    /// User-level memory that belongs to no project (WorkBuddy).
    pub user_level: bool,
    /// `units::content_hash` of the raw file.
    pub file_hash: String,
    /// RFC3339 UTC, or "" when unknown.
    pub stated_at: String,
    pub kind_hint: Option<String>,
    pub segments: Vec<Segment>,
    pub extra: Value,
}

/// A file or line an adapter did not import, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Issue {
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Default)]
pub struct ScanOutput {
    pub docs: Vec<SourceDoc>,
    pub issues: Vec<Issue>,
    /// Past session transcripts (`--include sessions`).
    pub sessions: Vec<SessionDoc>,
}

impl ScanOutput {
    pub fn issue(&mut self, path: String, reason: impl Into<String>) {
        self.issues.push(Issue {
            path,
            reason: reason.into(),
        });
    }
}

/// Reads one host's memory files. A file that cannot be imported is an issue, never an error; an error
/// means the whole scan cannot run (no host directory, missing workspaces).
pub trait Adapter {
    fn scan(&self, ctx: &ScanContext) -> Result<ScanOutput, Error>;
}

/// `path` as `~/rest` when it is under `home` (as given or canonicalized), else as it is.
pub fn display(home: &Path, path: &Path) -> String {
    let real = std::fs::canonicalize(home).ok();
    for base in std::iter::once(home).chain(real.as_deref()) {
        if base.as_os_str().is_empty() {
            continue;
        }
        if let Ok(rest) = path.strip_prefix(base) {
            return match rest.as_os_str().is_empty() {
                true => "~".to_string(),
                false => format!("~/{}", rest.display()),
            };
        }
    }
    path.display().to_string()
}

/// A display path back to a file path: `~` and `~/rest` are relative to `home`.
pub fn expand_in(home: &Path, shown: &str) -> PathBuf {
    match shown.strip_prefix('~') {
        Some("") => home.to_path_buf(),
        Some(rest) if rest.starts_with('/') => home.join(&rest[1..]),
        _ => PathBuf::from(shown),
    }
}

/// A regular file of at most `max` bytes as UTF-8 text, never through a symbolic link. The error is the
/// skip reason: `symlink`, `not-a-file`, `too-large` or `unreadable: <error>`.
pub fn read_text(path: &Path, max: u64) -> Result<(String, Metadata), String> {
    let unreadable = |e: &dyn std::fmt::Display| format!("unreadable: {e}");
    let meta = std::fs::symlink_metadata(path).map_err(|e| unreadable(&e))?;
    if meta.file_type().is_symlink() {
        return Err("symlink".into());
    }
    if !meta.is_file() {
        return Err("not-a-file".into());
    }
    if meta.len() > max {
        return Err("too-large".into());
    }
    let file = File::open(path).map_err(|e| unreadable(&e))?;
    #[cfg(unix)]
    {
        // The file opened must be the one examined: a link swapped in between is not followed.
        use std::os::unix::fs::MetadataExt;
        let opened = file.metadata().map_err(|e| unreadable(&e))?;
        if (opened.dev(), opened.ino()) != (meta.dev(), meta.ino()) {
            return Err("unreadable: replaced while reading".into());
        }
    }
    let mut bytes = Vec::new();
    file.take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| unreadable(&e))?;
    if bytes.len() as u64 > max {
        return Err("too-large".into());
    }
    let text = String::from_utf8(bytes).map_err(|e| unreadable(&e))?;
    Ok((text, meta))
}

/// Modification time as RFC3339 UTC, or "" when the platform has none.
pub fn mtime(meta: &Metadata) -> String {
    meta.modified()
        .map(|t| fmt_ts(&DateTime::<Utc>::from(t)))
        .unwrap_or_default()
}

/// An RFC3339 timestamp or `YYYY-MM-DD` date normalized to RFC3339 UTC (`2026-01-02T03:04:05Z`).
pub fn timestamp(value: &str) -> Option<String> {
    parse_ts(value).map(|t| fmt_ts(&t))
}

/// Entries of `dir` as (file name, path), sorted by name; names that are not UTF-8 are left out.
pub fn entries(dir: &Path) -> std::io::Result<Vec<(String, PathBuf)>> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if let Ok(name) = entry.file_name().into_string() {
            found.push((name, entry.path()));
        }
    }
    found.sort();
    Ok(found)
}

/// Lines of `raw` before `body`, a suffix of it (the part `split_frontmatter` returns): add it to a
/// `markdown::Section.line` to get the line of the file.
pub fn line_offset(raw: &str, body: &str) -> usize {
    raw[..raw.len() - body.len()].lines().count()
}

/// Locators of one document made unique: a repeat gets `~2`, `~3`, ... in document order.
#[derive(Debug, Default)]
pub struct Locators(HashMap<String, usize>);

impl Locators {
    pub fn unique(&mut self, base: String) -> String {
        let seen = self.0.entry(base.clone()).or_insert(0);
        *seen += 1;
        match *seen {
            1 => base,
            n => format!("{base}~{n}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_paths_hide_the_home_directory() {
        let home = tempfile::tempdir().unwrap();
        let file = home.path().join(".claude/projects/x/memory/note.md");
        assert_eq!(
            display(home.path(), &file),
            "~/.claude/projects/x/memory/note.md"
        );
        // A canonicalized path (macOS temp directories sit behind /var -> /private/var) shows the same.
        let real = std::fs::canonicalize(home.path()).unwrap().join("a.md");
        assert_eq!(display(home.path(), &real), "~/a.md");
        assert_eq!(display(home.path(), home.path()), "~");
        assert_eq!(
            display(home.path(), Path::new("/elsewhere/a.md")),
            "/elsewhere/a.md"
        );
        assert_eq!(
            expand_in(home.path(), "~/a/b.md"),
            home.path().join("a/b.md")
        );
        assert_eq!(expand_in(home.path(), "~"), home.path());
        assert_eq!(
            expand_in(home.path(), "/abs/c.md"),
            PathBuf::from("/abs/c.md")
        );
        assert_eq!(
            expand_in(home.path(), "~other/x"),
            PathBuf::from("~other/x")
        );
    }

    #[test]
    fn read_text_refuses_links_large_and_binary_files() {
        let dir = tempfile::tempdir().unwrap();
        let note = dir.path().join("note.md");
        std::fs::write(&note, "Plain note.\n").unwrap();
        assert_eq!(read_text(&note, 64).unwrap().0, "Plain note.\n");
        assert_eq!(read_text(&note, 4).unwrap_err(), "too-large");
        #[cfg(unix)]
        {
            let link = dir.path().join("link.md");
            std::os::unix::fs::symlink(&note, &link).unwrap();
            assert_eq!(read_text(&link, 64).unwrap_err(), "symlink");
        }
        let binary = dir.path().join("binary.md");
        std::fs::write(&binary, [0x66, 0xff, 0xfe]).unwrap();
        assert!(read_text(&binary, 64)
            .unwrap_err()
            .starts_with("unreadable: "));
        assert_eq!(read_text(dir.path(), 64).unwrap_err(), "not-a-file");
        assert!(read_text(&dir.path().join("missing.md"), 64)
            .unwrap_err()
            .starts_with("unreadable: "));
    }

    #[test]
    fn timestamps_normalize_to_utc_seconds() {
        assert_eq!(
            timestamp("2026-10-01T10:00:00+02:00").as_deref(),
            Some("2026-10-01T08:00:00Z")
        );
        assert_eq!(
            timestamp("2026-10-01T08:00:00.123Z").as_deref(),
            Some("2026-10-01T08:00:00Z")
        );
        assert_eq!(
            timestamp("2026-10-01").as_deref(),
            Some("2026-10-01T00:00:00Z")
        );
        assert_eq!(timestamp("last tuesday"), None);
    }

    #[test]
    fn locators_number_repeats() {
        let mut locators = Locators::default();
        let got: Vec<String> = ["task-1", "intro", "task-1", "task-1"]
            .map(|l| locators.unique(l.to_string()))
            .to_vec();
        assert_eq!(got, ["task-1", "intro", "task-1~2", "task-1~3"]);
        let raw = "---\nname: x\n---\n\n# Title\n";
        let body = &raw[raw.find('#').unwrap()..];
        assert_eq!(line_offset(raw, body), 4);
    }
}
