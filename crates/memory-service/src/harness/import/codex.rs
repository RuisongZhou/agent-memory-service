//! Codex memories (docs/design/memory-import.md §4, §6.2): one rollout summary per thread in
//! `<codex home>/memories/rollout_summaries/*.md`. A summary opens with `key: value` header lines
//! (`thread_id`, `updated_at`, `rollout_path`, `cwd`), then a level-1 title and `## Task N` sections that mix
//! durable knowledge with task progress, so every section goes to extraction. `MEMORY.md`,
//! `memory_summary.md`, `raw_memories.md` and `skills/` restate the summaries and are not imported. The
//! format is undocumented: a summary without its header fields is skipped as `unknown-format`.
use memory_rules::import::markdown::{sections, slug};
use memory_rules::import::units::content_hash;
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::project::within;
use super::source::{
    entries, line_offset, read_text, timestamp, Adapter, Locators, Policy, ScanContext, ScanOutput,
    Segment, SourceDoc,
};
use crate::harness::project::repository_root;
use crate::harness::{invalid, Error};

pub const HOST: &str = "codex";
const VIA: &str = "codex-rollout-summary";
/// Largest summary read.
const MAX_SUMMARY: u64 = 256 * 1024;
/// Files derived from the summaries, reported once each when present.
const DERIVED: [&str; 4] = [
    "MEMORY.md",
    "memory_summary.md",
    "raw_memories.md",
    "skills",
];

pub struct Codex;

impl Adapter for Codex {
    fn scan(&self, ctx: &ScanContext) -> Result<ScanOutput, Error> {
        ctx.require_workspaces(HOST)?;
        let codex = ctx
            .codex_home
            .clone()
            .unwrap_or_else(|| ctx.home.join(".codex"));
        let memories = codex.join("memories");
        if !memories.is_dir() {
            return Err(invalid(format!(
                "no Codex memories directory at {}",
                ctx.display(&memories)
            )));
        }
        let mut out = ScanOutput::default();
        for name in DERIVED {
            let path = memories.join(name);
            if let Ok(meta) = std::fs::symlink_metadata(&path) {
                let shown = ctx.display(&path);
                let shown = if meta.is_dir() { shown + "/" } else { shown };
                out.issue(shown, "derived");
            }
        }
        let summaries = memories.join("rollout_summaries");
        match std::fs::symlink_metadata(&summaries) {
            Ok(meta) if meta.is_dir() => {}
            Ok(meta) if meta.file_type().is_symlink() => {
                out.issue(ctx.display(&summaries), "symlink");
                return Ok(out);
            }
            _ => {
                out.issue(ctx.display(&summaries), "no-rollout-summaries");
                return Ok(out);
            }
        }
        let head = memories_head(&memories);
        let listing = entries(&summaries)
            .map_err(|e| invalid(format!("{}: {e}", ctx.display(&summaries))))?;
        for (name, path) in listing {
            if !name.ends_with(".md") || name.starts_with('.') {
                continue;
            }
            let shown = ctx.display(&path);
            let raw = match read_text(&path, MAX_SUMMARY) {
                Ok((raw, _)) => raw,
                Err(reason) => {
                    out.issue(shown, reason);
                    continue;
                }
            };
            let Some(summary) = parse(&raw) else {
                out.issue(shown, "unknown-format");
                continue;
            };
            if !within(&repository_root(&summary.cwd), &ctx.workspaces) {
                out.issue(shown, "other-project");
                continue;
            }
            if summary.segments.is_empty() {
                out.issue(shown, "empty");
                continue;
            }
            let rollout = summary
                .rollout_path
                .as_deref()
                .map(|p| ctx.display(Path::new(p)));
            out.docs.push(SourceDoc {
                host: HOST.into(),
                via: VIA,
                path,
                display_path: shown,
                native_id: summary.thread_id.clone(),
                title: summary.title,
                cwd: Some(summary.cwd),
                user_level: false,
                file_hash: content_hash(&raw),
                // A summary that does not say when is not dated by its file: Codex rewrites the files.
                stated_at: summary
                    .updated_at
                    .as_deref()
                    .and_then(timestamp)
                    .unwrap_or_default(),
                kind_hint: None,
                segments: summary.segments,
                extra: json!({"threadId": summary.thread_id, "rolloutPath": rollout, "memoriesHead": head}),
            });
        }
        Ok(out)
    }
}

/// One parsed rollout summary.
#[derive(Debug)]
struct Summary {
    thread_id: String,
    updated_at: Option<String>,
    rollout_path: Option<String>,
    cwd: PathBuf,
    title: Option<String>,
    segments: Vec<Segment>,
}

/// The header fields and sections of a summary; None without `thread_id` or an absolute `cwd`. The
/// level-1 section's own text is the `intro` segment; every level-2 section is one segment headed by its
/// heading (a section without text gives an empty segment, which the plan lists as skipped).
fn parse(raw: &str) -> Option<Summary> {
    let text = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    let mut fields: HashMap<&str, &str> = HashMap::new();
    let mut header = 0;
    for line in text.split_inclusive('\n') {
        if is_heading(line) {
            break;
        }
        header += line.len();
        if let Some((key, value)) = line.split_once(':') {
            let value = value.trim();
            if !value.is_empty() {
                fields.entry(key.trim()).or_insert(value);
            }
        }
    }
    let thread_id = fields.get("thread_id")?.to_string();
    let cwd = PathBuf::from(fields.get("cwd")?);
    if !cwd.is_absolute() {
        return None;
    }
    let rest = &text[header..];
    let offset = line_offset(raw, rest);
    let mut locators = Locators::default();
    let mut segments = Vec::new();
    let mut title = None;
    for section in sections(rest, 2) {
        let (locator, text) = match section.level {
            1 => {
                title.get_or_insert_with(|| section.heading.clone());
                if section.body.trim().is_empty() {
                    continue;
                }
                ("intro".to_string(), section.body.clone())
            }
            2 => {
                let text = match section.body.trim().is_empty() {
                    true => String::new(),
                    false => format!("{}\n{}", section.heading, section.body),
                };
                (slug(&section.heading), text)
            }
            _ => continue, // the rest starts at a heading: no text comes before it
        };
        let path = match section.level {
            1 => vec![section.heading.clone()],
            _ => section.path.clone(),
        };
        let mut segment = Segment::new(locators.unique(locator), path, text, Policy::AlwaysExtract);
        segment.extra = json!({"line": section.line + offset});
        segments.push(segment);
    }
    Some(Summary {
        thread_id,
        updated_at: fields.get("updated_at").map(|v| v.to_string()),
        rollout_path: fields.get("rollout_path").map(|v| v.to_string()),
        cwd,
        title,
        segments,
    })
}

/// An ATX heading line, as `markdown::sections` reads one.
fn is_heading(line: &str) -> bool {
    let marks = line.bytes().take_while(|&b| b == b'#').count();
    (1..=6).contains(&marks) && line[marks..].starts_with([' ', '\t'])
}

/// HEAD of the memories directory when it is a git repository of its own, as Codex keeps it.
fn memories_head(memories: &Path) -> Option<String> {
    if !memories.join(".git").exists() {
        return None;
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(memories)
        .args(["rev-parse", "HEAD"])
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let head = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (!head.is_empty()).then_some(head)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::import::source::Issue;
    use serde_json::Value;

    struct Fixture {
        home: tempfile::TempDir,
        workspace: PathBuf,
        summaries: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let home = tempfile::tempdir().unwrap();
            let workspace = home.path().join("code/my-app");
            std::fs::create_dir_all(&workspace).unwrap();
            let summaries = home.path().join(".codex/memories/rollout_summaries");
            std::fs::create_dir_all(&summaries).unwrap();
            Fixture {
                workspace: repository_root(&workspace),
                home,
                summaries,
            }
        }

        fn ctx(&self) -> ScanContext {
            ScanContext {
                home: self.home.path().to_path_buf(),
                workspaces: vec![self.workspace.clone()],
                ..ScanContext::default()
            }
        }

        fn summary(&self, name: &str, header: &str, body: &str) {
            std::fs::write(self.summaries.join(name), format!("{header}\n{body}")).unwrap();
        }

        fn header(&self, thread: &str, cwd: &Path) -> String {
            format!(
                "thread_id: {thread}\nupdated_at: 2026-01-02T03:04:05Z\nrollout_path: {}\ncwd: {}\n",
                self.home
                    .path()
                    .join(".codex/sessions/2026/01/02/rollout-demo.jsonl")
                    .display(),
                cwd.display()
            )
        }
    }

    fn issues(out: &ScanOutput) -> Vec<(String, String)> {
        let mut found: Vec<_> = out
            .issues
            .iter()
            .map(|Issue { path, reason }| (path.clone(), reason.clone()))
            .collect();
        found.sort();
        found
    }

    const BODY: &str = concat!(
        "# Fix the flaky build\n",
        "\n",
        "The build failed on CI twice this week.\n",
        "\n",
        "## Task 1: Find the cause\n",
        "- The cache key ignored the lockfile.\n",
        "\n",
        "## Task 1: Find the cause\n",
        "- A second pass found a race in the test runner.\n",
        "\n",
        "## Notes\n",
    );

    #[test]
    fn summaries_become_an_intro_and_one_segment_per_task() {
        let f = Fixture::new();
        f.summary(
            "2026-01-02-demo.md",
            &f.header("019a-demo-thread", &f.workspace),
            BODY,
        );
        let out = Codex.scan(&f.ctx()).unwrap();
        assert_eq!(issues(&out), []);
        let doc = &out.docs[0];
        assert_eq!(doc.native_id, "019a-demo-thread");
        assert_eq!(doc.stated_at, "2026-01-02T03:04:05Z");
        assert_eq!(doc.title.as_deref(), Some("Fix the flaky build"));
        assert_eq!(doc.cwd.as_deref(), Some(f.workspace.as_path()));
        assert_eq!(
            doc.display_path,
            "~/.codex/memories/rollout_summaries/2026-01-02-demo.md"
        );
        assert_eq!(
            doc.extra,
            json!({"threadId": "019a-demo-thread",
                   "rolloutPath": "~/.codex/sessions/2026/01/02/rollout-demo.jsonl",
                   "memoriesHead": null})
        );
        let got: Vec<(&str, Vec<&str>, &str, &Value)> = doc
            .segments
            .iter()
            .map(|s| {
                (
                    s.locator.as_str(),
                    s.heading_path.iter().map(String::as_str).collect(),
                    s.text.as_str(),
                    &s.extra["line"],
                )
            })
            .collect();
        let title = "Fix the flaky build";
        assert_eq!(
            got,
            vec![
                (
                    "intro",
                    vec![title],
                    "The build failed on CI twice this week.",
                    &json!(6)
                ),
                (
                    "task-1-find-the-cause",
                    vec![title, "Task 1: Find the cause"],
                    "Task 1: Find the cause\n- The cache key ignored the lockfile.",
                    &json!(10)
                ),
                (
                    "task-1-find-the-cause~2",
                    vec![title, "Task 1: Find the cause"],
                    "Task 1: Find the cause\n- A second pass found a race in the test runner.",
                    &json!(13)
                ),
                ("notes", vec![title, "Notes"], "", &json!(16)),
            ]
        );
        assert!(doc
            .segments
            .iter()
            .all(|s| s.policy == Policy::AlwaysExtract));
        // `line` is the line of the file: the header comes first.
        let raw = std::fs::read_to_string(&doc.path).unwrap();
        assert_eq!(raw.lines().nth(9), Some("## Task 1: Find the cause"));
        assert_eq!(doc.file_hash, content_hash(&raw));
    }

    #[test]
    fn other_projects_unknown_formats_and_derived_files_are_reported() {
        let f = Fixture::new();
        let memories = f.home.path().join(".codex/memories");
        std::fs::write(memories.join("MEMORY.md"), "# Derived\n").unwrap();
        std::fs::create_dir_all(memories.join("skills/deploy")).unwrap();
        let other = f.home.path().join("code/other");
        std::fs::create_dir_all(&other).unwrap();
        f.summary("a-other.md", &f.header("t-other", &other), BODY);
        f.summary(
            "b-no-thread.md",
            &format!("cwd: {}\n", f.workspace.display()),
            BODY,
        );
        f.summary(
            "c-relative.md",
            "thread_id: t-rel\ncwd: code/my-app\n",
            BODY,
        );
        f.summary("d-header-only.md", &f.header("t-empty", &f.workspace), "");
        f.summary(
            "e-title-first.md",
            "",
            &format!("# Title\n{}", f.header("t-late", &f.workspace)),
        );
        std::fs::write(f.summaries.join("f-large.md"), "x".repeat(256 * 1024 + 1)).unwrap();
        std::fs::write(f.summaries.join("notes.txt"), "ignored").unwrap();

        let out = Codex.scan(&f.ctx()).unwrap();
        let summary = |name: &str| format!("~/.codex/memories/rollout_summaries/{name}");
        let expected = vec![
            ("~/.codex/memories/MEMORY.md".to_string(), "derived"),
            ("~/.codex/memories/skills/".to_string(), "derived"),
            (summary("a-other.md"), "other-project"),
            (summary("b-no-thread.md"), "unknown-format"),
            (summary("c-relative.md"), "unknown-format"),
            (summary("d-header-only.md"), "empty"),
            (summary("e-title-first.md"), "unknown-format"),
            (summary("f-large.md"), "too-large"),
        ];
        let mut expected: Vec<(String, String)> = expected
            .into_iter()
            .map(|(p, r)| (p, r.to_string()))
            .collect();
        expected.sort();
        assert_eq!(issues(&out), expected);
        assert!(out.docs.is_empty());
    }

    #[test]
    fn missing_memories_and_workspaces_are_errors() {
        let f = Fixture::new();
        let mut ctx = f.ctx();
        ctx.codex_home = Some(f.home.path().join("no-codex"));
        let error = Codex.scan(&ctx).unwrap_err().to_string();
        assert!(error.contains("~/no-codex/memories"), "{error}");
        let mut ctx = f.ctx();
        ctx.workspaces.clear();
        assert!(Codex
            .scan(&ctx)
            .unwrap_err()
            .to_string()
            .contains("--workspace"));
        std::fs::remove_dir(&f.summaries).unwrap();
        let out = Codex.scan(&f.ctx()).unwrap();
        assert_eq!(
            issues(&out),
            [(
                "~/.codex/memories/rollout_summaries".to_string(),
                "no-rollout-summaries".to_string()
            )]
        );
    }

    #[test]
    #[ignore = "runs git; run by harness::project::tests::tests_that_run_git_pass_in_a_child_process"]
    fn a_memories_repository_records_its_head() {
        use crate::harness::project::tests::{git, git_available};
        if !git_available() {
            return;
        }
        let f = Fixture::new();
        f.summary("demo.md", &f.header("t-head", &f.workspace), BODY);
        let memories = f.home.path().join(".codex/memories");
        git(&memories, &["init", "-q"]);
        git(&memories, &["add", "-A"]);
        git(&memories, &["commit", "-q", "-m", "memories"]);
        let out = Codex.scan(&f.ctx()).unwrap();
        let head = out.docs[0].extra["memoriesHead"].as_str().unwrap();
        assert!(
            head.len() >= 40 && head.chars().all(|c| c.is_ascii_hexdigit()),
            "{head}"
        );
    }
}
