//! WorkBuddy memory (docs/design/memory-import.md §4, §6.2): `MEMORY.md`, the user profile `USER.md`,
//! `memory/<uid>_memory.md` and `user-*-personal/MEMORY.md` under `~/.workbuddy`. They are user-level and
//! belong to no project, so no workspace is needed. `SOUL.md`, `IDENTITY.md` and `BOOTSTRAP.md` define the
//! agent's persona and are not imported.
use memory_rules::import::markdown::{sections, slug, split_frontmatter};
use memory_rules::import::units::content_hash;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use super::source::{
    entries, line_offset, mtime, read_text, Adapter, Locators, Policy, ScanContext, ScanOutput,
    Segment, SourceDoc,
};
use crate::harness::{invalid, Error};

pub const HOST: &str = "workbuddy";
const VIA: &str = "workbuddy-memory";
/// Largest memory file read.
const MAX_FILE: u64 = 256 * 1024;
const PERSONA: [&str; 3] = ["SOUL.md", "IDENTITY.md", "BOOTSTRAP.md"];

pub struct WorkBuddy;

impl Adapter for WorkBuddy {
    fn scan(&self, ctx: &ScanContext) -> Result<ScanOutput, Error> {
        let root = ctx
            .workbuddy_home
            .clone()
            .unwrap_or_else(|| ctx.home.join(".workbuddy"));
        if !root.is_dir() {
            return Err(invalid(format!(
                "no WorkBuddy directory at {}",
                ctx.display(&root)
            )));
        }
        let mut out = ScanOutput::default();
        for name in PERSONA {
            let path = root.join(name);
            if std::fs::symlink_metadata(&path).is_ok() {
                out.issue(ctx.display(&path), "persona-file");
            }
        }
        let mut files = vec![root.join("MEMORY.md"), root.join("USER.md")];
        if let Some(listing) = directory(ctx, &root.join("memory"), &mut out) {
            files.extend(
                listing
                    .into_iter()
                    .filter(|(name, _)| name.ends_with("_memory.md") && !name.contains(".bak"))
                    .map(|(_, path)| path),
            );
        }
        let listing =
            entries(&root).map_err(|e| invalid(format!("{}: {e}", ctx.display(&root))))?;
        for (name, dir) in listing {
            let personal = name.starts_with("user-") && name.ends_with("-personal");
            if personal && directory(ctx, &dir, &mut out).is_some() {
                files.push(dir.join("MEMORY.md"));
            }
        }
        for path in files {
            if std::fs::symlink_metadata(&path).is_ok() {
                document(ctx, &root, path, &mut out);
            }
        }
        Ok(out)
    }
}

/// The entries of a real directory; a symbolic link is an issue, a missing directory nothing.
fn directory(
    ctx: &ScanContext,
    dir: &Path,
    out: &mut ScanOutput,
) -> Option<Vec<(String, PathBuf)>> {
    let meta = std::fs::symlink_metadata(dir).ok()?;
    if meta.file_type().is_symlink() {
        out.issue(ctx.display(dir), "symlink");
        return None;
    }
    if !meta.is_dir() {
        return None;
    }
    match entries(dir) {
        Ok(listing) => Some(listing),
        Err(e) => {
            out.issue(ctx.display(dir), format!("unreadable: {e}"));
            None
        }
    }
}

/// One document per memory file: one segment per heading section with text, the text before the first
/// heading as `preamble`.
fn document(ctx: &ScanContext, root: &Path, path: PathBuf, out: &mut ScanOutput) {
    let shown = ctx.display(&path);
    let (raw, meta) = match read_text(&path, MAX_FILE) {
        Ok(read) => read,
        Err(reason) => return out.issue(shown, reason),
    };
    let (_, body) = split_frontmatter(&raw);
    let offset = line_offset(&raw, body);
    let mut locators = Locators::default();
    let mut segments = Vec::new();
    let mut title = None;
    for section in sections(body, 2) {
        if section.level == 1 {
            title.get_or_insert_with(|| section.heading.clone());
        }
        if section.body.trim().is_empty() {
            continue;
        }
        let (locator, text) = match section.level {
            0 => ("preamble".to_string(), section.body.clone()),
            _ => (
                section
                    .path
                    .iter()
                    .map(|heading| slug(heading))
                    .collect::<Vec<_>>()
                    .join("/"),
                format!("{}\n{}", section.heading, section.body),
            ),
        };
        let mut segment = Segment::new(
            locators.unique(locator),
            section.path.clone(),
            text,
            Policy::VerbatimIfFits,
        );
        segment.extra = json!({"line": section.line + offset});
        segments.push(segment);
    }
    if segments.is_empty() {
        return out.issue(shown, "empty");
    }
    let native_id = path
        .strip_prefix(root)
        .expect("WorkBuddy files are joined onto its root")
        .to_string_lossy()
        .into_owned();
    let profile = native_id == "USER.md";
    out.docs.push(SourceDoc {
        host: HOST.into(),
        via: VIA,
        path,
        display_path: shown,
        native_id,
        title,
        cwd: None,
        user_level: true,
        file_hash: content_hash(&raw),
        stated_at: mtime(&meta),
        kind_hint: profile.then(|| "user".to_string()),
        instruction_candidate: false,
        segments,
        extra: Value::Null,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::import::source::{timestamp, Issue};

    fn write(root: &Path, file: &str, text: &str) {
        let path = root.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn user_level_memory_files_and_persona_files() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join(".workbuddy");
        write(
            &root,
            "MEMORY.md",
            concat!(
                "Intro line before any heading.\n",
                "\n",
                "# Long-term memory\n",
                "\n",
                "## Preferences\n",
                "- Prefers concise answers.\n",
                "\n",
                "## Projects\n",
                "### Alpha\n",
                "Alpha uses Rust.\n",
                "\n",
                "## Preferences\n",
                "- Writes commit messages in English.\n",
            ),
        );
        write(
            &root,
            "USER.md",
            "---\nname: profile\n---\n## Profile\nWorks on backend services.\n",
        );
        write(
            &root,
            "memory/u1_memory.md",
            "## Habits\nReviews diffs before merging.\n",
        );
        write(&root, "memory/u1_memory.md.bak", "## Old\nstale\n");
        write(
            &root,
            "user-u1-personal/MEMORY.md",
            "## Personal\nUses a split keyboard.\n",
        );
        write(&root, "user-u2-personal/other.md", "## Ignored\nx\n");
        write(&root, "SOUL.md", "# Persona\nCalm.\n");

        let ctx = ScanContext {
            home: home.path().to_path_buf(),
            ..ScanContext::default()
        };
        let out = WorkBuddy.scan(&ctx).unwrap();
        let issues: Vec<_> = out
            .issues
            .iter()
            .map(|Issue { path, reason }| (path.as_str(), reason.as_str()))
            .collect();
        assert_eq!(issues, [("~/.workbuddy/SOUL.md", "persona-file")]);
        let ids: Vec<&str> = out.docs.iter().map(|d| d.native_id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "MEMORY.md",
                "USER.md",
                "memory/u1_memory.md",
                "user-u1-personal/MEMORY.md"
            ]
        );
        assert!(out.docs.iter().all(|d| d.user_level && d.cwd.is_none()));
        let memory = &out.docs[0];
        assert_eq!(memory.title.as_deref(), Some("Long-term memory"));
        assert_eq!(memory.kind_hint, None);
        let meta = std::fs::metadata(&memory.path).unwrap();
        assert_eq!(memory.stated_at, mtime(&meta));
        assert!(timestamp(&memory.stated_at).is_some());
        let segments: Vec<(&str, &str, &serde_json::Value)> = memory
            .segments
            .iter()
            .map(|s| (s.locator.as_str(), s.text.as_str(), &s.extra["line"]))
            .collect();
        assert_eq!(
            segments,
            vec![
                ("preamble", "Intro line before any heading.", &json!(1)),
                (
                    "long-term-memory/preferences",
                    "Preferences\n- Prefers concise answers.",
                    &json!(5)
                ),
                (
                    "long-term-memory/projects",
                    "Projects\n### Alpha\nAlpha uses Rust.",
                    &json!(8)
                ),
                (
                    "long-term-memory/preferences~2",
                    "Preferences\n- Writes commit messages in English.",
                    &json!(12)
                ),
            ]
        );
        assert_eq!(
            memory.segments[1].heading_path,
            ["Long-term memory", "Preferences"]
        );
        assert!(memory
            .segments
            .iter()
            .all(|s| s.policy == Policy::VerbatimIfFits));
        // The profile is the user's; its frontmatter is not part of any segment.
        let user = &out.docs[1];
        assert_eq!(user.kind_hint.as_deref(), Some("user"));
        assert_eq!(user.segments[0].text, "Profile\nWorks on backend services.");
        assert_eq!(user.segments[0].extra["line"], 4);
        assert_eq!(out.docs[2].kind_hint, None);
    }

    #[test]
    fn missing_directory_is_an_error_and_links_are_not_followed() {
        let home = tempfile::tempdir().unwrap();
        let ctx = ScanContext {
            home: home.path().to_path_buf(),
            ..ScanContext::default()
        };
        assert!(WorkBuddy
            .scan(&ctx)
            .unwrap_err()
            .to_string()
            .contains("~/.workbuddy"));
        let root = home.path().join(".workbuddy");
        write(&root, "MEMORY.md", "# Title only\n");
        #[cfg(unix)]
        {
            let elsewhere = home.path().join("elsewhere");
            write(&elsewhere, "u1_memory.md", "## Habit\nx\n");
            std::os::unix::fs::symlink(&elsewhere, root.join("memory")).unwrap();
            std::os::unix::fs::symlink(elsewhere.join("u1_memory.md"), root.join("USER.md"))
                .unwrap();
        }
        let out = WorkBuddy.scan(&ctx).unwrap();
        assert!(out.docs.is_empty());
        let mut issues: Vec<_> = out
            .issues
            .iter()
            .map(|i| (i.path.as_str(), i.reason.as_str()))
            .collect();
        issues.sort();
        let mut expected = vec![("~/.workbuddy/MEMORY.md", "empty")];
        #[cfg(unix)]
        expected.extend([
            ("~/.workbuddy/USER.md", "symlink"),
            ("~/.workbuddy/memory", "symlink"),
        ]);
        expected.sort();
        assert_eq!(issues, expected);
    }
}
