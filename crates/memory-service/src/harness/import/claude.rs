//! Claude Code auto memory (docs/design/memory-import.md §4, §6.2): `<config>/projects/<encoded dir>/memory/*.md`,
//! one note per file with frontmatter (`name`, `description`, `type`, `modified`). As claude-mem's
//! `memory ingest` does it (thedotmack/claude-mem#2829): sources stay inside the projects directory by
//! realpath, symbolic links are never followed, the `MEMORY.md` link index, empty notes and notes over
//! 64 KiB are skipped, and a directory's project comes from the first `cwd` in the first 16 KiB of a
//! sibling transcript, whose content is otherwise never read.
use memory_rules::import::markdown::split_frontmatter;
use memory_rules::import::units::content_hash;
use serde_json::json;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::project::{claude_dir_name, within};
use super::session::first_cwd;
use super::source::{
    entries, mtime, read_text, timestamp, Adapter, Policy, ScanContext, ScanOutput, Segment,
    SourceDoc,
};
use crate::harness::project::repository_root;
use crate::harness::{invalid, Error};

pub const HOST: &str = "claude-code";
const VIA: &str = "claude-auto-memory";
/// Largest note read.
const MAX_NOTE: u64 = 64 * 1024;
/// Transcript bytes read to find a directory's `cwd`.
pub(super) const SNIFF_BYTES: u64 = 16 * 1024;

pub struct Claude;

/// A project directory whose notes belong to a workspace.
struct ProjectDir {
    /// The encoded directory name, part of each note's native id.
    name: String,
    dir: PathBuf,
    cwd: PathBuf,
}

impl Adapter for Claude {
    fn scan(&self, ctx: &ScanContext) -> Result<ScanOutput, Error> {
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
        let mut out = ScanOutput::default();
        let dirs = match ctx.all {
            true => all_dirs(ctx, &projects, &root, &mut out)?,
            false => workspace_dirs(ctx, &projects, &mut out),
        };
        let mut seen = HashSet::new();
        for project in dirs {
            let memory = project.dir.join("memory");
            match std::fs::canonicalize(&memory) {
                Ok(real) if real.starts_with(&root) && real != root => {
                    if seen.insert(real) {
                        notes(ctx, &project, &memory, &mut out);
                    }
                }
                Ok(_) => out.issue(ctx.display(&memory), "outside-root"),
                Err(e) => out.issue(ctx.display(&memory), format!("unreadable: {e}")),
            }
        }
        Ok(out)
    }
}

/// The directory Claude Code keeps for each workspace root.
fn workspace_dirs(ctx: &ScanContext, projects: &Path, out: &mut ScanOutput) -> Vec<ProjectDir> {
    let mut dirs = Vec::new();
    for workspace in &ctx.workspaces {
        let name = claude_dir_name(workspace);
        let dir = projects.join(&name);
        if dir.join("memory").is_dir() {
            dirs.push(ProjectDir {
                name,
                dir,
                cwd: workspace.clone(),
            });
        } else {
            out.issue(ctx.display(&dir.join("memory")), "no-auto-memory");
        }
    }
    dirs
}

/// Every project directory with notes whose transcripts name a directory of a workspace's repository,
/// or that is a workspace's own directory when no transcript names one.
fn all_dirs(
    ctx: &ScanContext,
    projects: &Path,
    root: &Path,
    out: &mut ScanOutput,
) -> Result<Vec<ProjectDir>, Error> {
    let listing =
        entries(projects).map_err(|e| invalid(format!("{}: {e}", ctx.display(projects))))?;
    let mut dirs = Vec::new();
    for (name, dir) in listing {
        let memory = dir.join("memory");
        if !memory.is_dir() {
            continue;
        }
        let shown = ctx.display(&memory);
        // Transcripts are read only inside the projects directory.
        match std::fs::canonicalize(&dir) {
            Ok(real) if real.starts_with(root) && real != root => {}
            _ => {
                out.issue(shown, "outside-root");
                continue;
            }
        }
        match sniff_cwd(&dir) {
            Some(cwd) if within(&repository_root(&cwd), &ctx.workspaces) => {
                dirs.push(ProjectDir { name, dir, cwd })
            }
            Some(_) => out.issue(shown, "other-project"),
            None => match ctx.workspaces.iter().find(|w| claude_dir_name(w) == name) {
                Some(workspace) => dirs.push(ProjectDir {
                    name,
                    dir,
                    cwd: workspace.clone(),
                }),
                None => out.issue(shown, "cwd-unresolved"),
            },
        }
    }
    Ok(dirs)
}

/// The first `cwd` the directory's transcripts name in their first 16 KiB, in file name order.
fn sniff_cwd(dir: &Path) -> Option<PathBuf> {
    entries(dir)
        .ok()?
        .into_iter()
        .filter(|(name, _)| name.ends_with(".jsonl"))
        .find_map(|(_, path)| first_cwd(&path, SNIFF_BYTES))
}

/// One document per note of `memory`.
fn notes(ctx: &ScanContext, project: &ProjectDir, memory: &Path, out: &mut ScanOutput) {
    let listing = match entries(memory) {
        Ok(listing) => listing,
        Err(e) => return out.issue(ctx.display(memory), format!("unreadable: {e}")),
    };
    for (name, path) in listing {
        if !name.ends_with(".md") || name.starts_with('.') {
            continue;
        }
        let shown = ctx.display(&path);
        if name == "MEMORY.md" {
            out.issue(shown, "index");
            continue;
        }
        let (raw, meta) = match read_text(&path, MAX_NOTE) {
            Ok(read) => read,
            Err(reason) => {
                out.issue(shown, reason);
                continue;
            }
        };
        let (front, body) = split_frontmatter(&raw);
        let body = body.trim();
        if body.is_empty() {
            out.issue(shown, "empty");
            continue;
        }
        let description = front
            .description
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty());
        let text = match description {
            Some(description) => format!("{description}\n\n{body}"),
            None => body.to_string(),
        };
        let stated_at = front
            .modified
            .as_deref()
            .and_then(timestamp)
            .unwrap_or_else(|| mtime(&meta));
        let heading_path = front.name.iter().cloned().collect();
        out.docs.push(SourceDoc {
            host: HOST.into(),
            via: VIA,
            path,
            display_path: shown,
            native_id: format!("{}/{name}", project.name),
            title: front.name.clone(),
            cwd: Some(project.cwd.clone()),
            user_level: false,
            file_hash: content_hash(&raw),
            stated_at,
            kind_hint: front.kind,
            segments: vec![Segment::new(
                "note".into(),
                heading_path,
                text,
                Policy::VerbatimIfFits,
            )],
            extra: json!({"originSessionId": front.origin_session_id, "encodedDir": project.name}),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::import::source::Issue;
    use std::fs::File;
    use std::time::{Duration, SystemTime};

    struct Fixture {
        home: tempfile::TempDir,
        /// Repository root of the workspace.
        workspace: PathBuf,
        projects: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
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

        fn ctx(&self, all: bool) -> ScanContext {
            ScanContext {
                home: self.home.path().to_path_buf(),
                workspaces: vec![self.workspace.clone()],
                all,
                ..ScanContext::default()
            }
        }

        /// `<projects>/<name>/memory`, created.
        fn memory(&self, name: &str) -> PathBuf {
            let dir = self.projects.join(name).join("memory");
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }

        fn note(&self, project: &str, file: &str, text: &str) {
            std::fs::write(self.memory(project).join(file), text).unwrap();
        }

        fn transcript(&self, project: &str, lines: &[String]) {
            let path = self.projects.join(project).join("session-0001.jsonl");
            std::fs::write(path, lines.join("\n") + "\n").unwrap();
        }

        fn shown(&self, project: &str, file: &str) -> String {
            format!("~/.claude/projects/{project}/memory/{file}")
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

    fn set_mtime(path: &Path, secs: u64) {
        let file = File::options().write(true).open(path).unwrap();
        file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
            .unwrap();
    }

    #[test]
    fn notes_of_the_workspace_directory_and_what_is_skipped() {
        let f = Fixture::new();
        let enc = claude_dir_name(&f.workspace);
        f.note(&enc, "MEMORY.md", "- [Deploy](deploy.md) release steps\n");
        f.note(
            &enc,
            "deploy.md",
            concat!(
                "---\n",
                "name: Deploy notes\n",
                "description: How releases reach staging\n",
                "metadata:\n",
                "  type: feedback\n",
                "  modified: 2026-10-01T10:00:00+02:00\n",
                "  originSessionId: session-0001\n",
                "---\n",
                "\n",
                "Always run the smoke checks before tagging.\n",
            ),
        );
        f.note(&enc, "plain.md", "Builds use the fast profile.\n");
        set_mtime(&f.memory(&enc).join("plain.md"), 1_790_000_000);
        f.note(&enc, "empty.md", "---\nname: Nothing yet\n---\n\n  \n");
        f.note(&enc, "big.md", &"a".repeat(64 * 1024 + 1));
        f.note(&enc, "readme.txt", "not a note");
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            f.memory(&enc).join("plain.md"),
            f.memory(&enc).join("link.md"),
        )
        .unwrap();

        let out = Claude.scan(&f.ctx(false)).unwrap();
        let found = issues(&out);
        let reason = |file: &str| {
            found
                .iter()
                .find(|(p, _)| *p == f.shown(&enc, file))
                .map(|(_, r)| r.as_str())
        };
        assert_eq!(reason("MEMORY.md"), Some("index"));
        assert_eq!(reason("empty.md"), Some("empty"));
        assert_eq!(reason("big.md"), Some("too-large"));
        #[cfg(unix)]
        assert_eq!(reason("link.md"), Some("symlink"));
        assert_eq!(reason("readme.txt"), None);

        assert_eq!(out.docs.len(), 2);
        let deploy = &out.docs[0];
        assert_eq!(deploy.display_path, f.shown(&enc, "deploy.md"));
        assert_eq!(deploy.native_id, format!("{enc}/deploy.md"));
        assert_eq!(deploy.title.as_deref(), Some("Deploy notes"));
        assert_eq!(deploy.stated_at, "2026-10-01T08:00:00Z");
        assert_eq!(deploy.kind_hint.as_deref(), Some("feedback"));
        assert_eq!(deploy.cwd.as_deref(), Some(f.workspace.as_path()));
        assert_eq!(
            deploy.segments[0].text,
            "How releases reach staging\n\nAlways run the smoke checks before tagging."
        );
        assert_eq!(deploy.segments[0].locator, "note");
        assert_eq!(deploy.segments[0].heading_path, ["Deploy notes"]);
        assert_eq!(deploy.segments[0].policy, Policy::VerbatimIfFits);
        assert_eq!(
            deploy.extra,
            json!({"originSessionId": "session-0001", "encodedDir": enc})
        );
        assert_eq!(
            deploy.file_hash,
            content_hash(&std::fs::read_to_string(&deploy.path).unwrap())
        );
        // Without `modified`, the note is as old as its file.
        let plain = &out.docs[1];
        assert_eq!(plain.stated_at, "2026-09-21T14:13:20Z");
        assert_eq!(plain.kind_hint, None);
        assert_eq!(plain.segments[0].text, "Builds use the fast profile.");
        assert!(plain.segments[0].heading_path.is_empty());
    }

    #[test]
    fn missing_directories_are_issues_or_errors() {
        let f = Fixture::new();
        let out = Claude.scan(&f.ctx(false)).unwrap();
        assert!(out.docs.is_empty());
        let enc = claude_dir_name(&f.workspace);
        assert_eq!(
            issues(&out),
            [(
                format!("~/.claude/projects/{enc}/memory"),
                "no-auto-memory".to_string()
            )]
        );
        let mut ctx = f.ctx(false);
        ctx.workspaces.clear();
        assert!(Claude
            .scan(&ctx)
            .unwrap_err()
            .to_string()
            .contains("--workspace"));
        let mut ctx = f.ctx(false);
        ctx.claude_config_dir = Some(f.home.path().join("nowhere"));
        let error = Claude.scan(&ctx).unwrap_err().to_string();
        assert!(error.contains("~/nowhere/projects"), "{error}");
    }

    #[test]
    fn all_keeps_directories_whose_transcripts_name_the_workspace() {
        let f = Fixture::new();
        let record = |cwd: &Path| json!({"type": "user", "cwd": cwd, "message": "x"}).to_string();
        // A session in a subdirectory: its first cwd lies after a record without one.
        let sub = f.workspace.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let sub_dir = claude_dir_name(&sub);
        f.note(&sub_dir, "a.md", "Sub note.");
        f.transcript(
            &sub_dir,
            &[json!({"type": "summary"}).to_string(), record(&sub)],
        );
        // The workspace's own directory needs no transcript.
        f.note(&claude_dir_name(&f.workspace), "c.md", "Own note.");
        // Another project, and one whose transcript names its cwd only after the first 16 KiB.
        let other = f.home.path().join("code/other");
        std::fs::create_dir_all(&other).unwrap();
        f.note("-other", "d.md", "Other project note.");
        f.transcript("-other", &[record(&other)]);
        f.note("-late", "e.md", "Late note.");
        let filler =
            json!({"type": "summary", "summary": "x".repeat(SNIFF_BYTES as usize)}).to_string();
        f.transcript("-late", &[filler, record(&f.workspace)]);

        let out = Claude.scan(&f.ctx(true)).unwrap();
        let mut texts: Vec<&str> = out
            .docs
            .iter()
            .map(|d| d.segments[0].text.as_str())
            .collect();
        texts.sort();
        assert_eq!(texts, ["Own note.", "Sub note."]);
        let sub_doc = out
            .docs
            .iter()
            .find(|d| d.segments[0].text == "Sub note.")
            .unwrap();
        assert_eq!(sub_doc.cwd.as_deref(), Some(sub.as_path()));
        assert_eq!(sub_doc.native_id, format!("{sub_dir}/a.md"));
        assert_eq!(
            issues(&out),
            [
                (
                    "~/.claude/projects/-late/memory".to_string(),
                    "cwd-unresolved".to_string()
                ),
                (
                    "~/.claude/projects/-other/memory".to_string(),
                    "other-project".to_string()
                ),
            ]
        );
        // Without --all only the workspace's own directory is read.
        let out = Claude.scan(&f.ctx(false)).unwrap();
        assert_eq!(out.docs.len(), 1);
        assert_eq!(out.docs[0].segments[0].text, "Own note.");
    }

    #[cfg(unix)]
    #[test]
    fn memory_directories_must_stay_inside_the_projects_directory() {
        let f = Fixture::new();
        let outside = f.home.path().join("elsewhere/memory");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("x.md"), "Outside note.").unwrap();
        let enc = claude_dir_name(&f.workspace);
        std::fs::create_dir_all(f.projects.join(&enc)).unwrap();
        std::os::unix::fs::symlink(&outside, f.projects.join(&enc).join("memory")).unwrap();
        let out = Claude.scan(&f.ctx(false)).unwrap();
        assert!(out.docs.is_empty());
        assert_eq!(
            issues(&out),
            [(
                format!("~/.claude/projects/{enc}/memory"),
                "outside-root".to_string()
            )]
        );
        // With --all, a project directory linked from elsewhere is refused before its transcripts are read.
        std::fs::remove_file(f.projects.join(&enc).join("memory")).unwrap();
        std::os::unix::fs::symlink(f.home.path().join("elsewhere"), f.projects.join("-linked"))
            .unwrap();
        let out = Claude.scan(&f.ctx(true)).unwrap();
        assert!(out.docs.is_empty());
        assert_eq!(
            issues(&out),
            [(
                "~/.claude/projects/-linked/memory".to_string(),
                "outside-root".to_string()
            )]
        );
    }
}
