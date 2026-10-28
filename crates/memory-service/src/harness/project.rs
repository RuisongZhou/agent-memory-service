//! Which project a session's memories belong to (docs/design/project-routing.md §3): the session's folder,
//! given by the host (`CLAUDE_PROJECT_DIR`, a hook payload's `cwd`, the MCP bridge's working directory), is
//! mapped to the main repository around it, or to the folder itself, or to no project at all (global). The
//! project id is the canonical absolute path. Hooks and the bridge resolve it once, from the environment,
//! and pass the typed result on; [`session_folder`] and [`resolve`] take everything they read as arguments.
//! The repository mapping (`repository_root`) is the one `import` uses to match a host's directories to a
//! workspace (docs/design/memory-import.md §6.4): a worktree maps to its main repository, as Claude Code
//! shares one auto-memory directory across the worktrees of a repository.
use serde_json::{json, Value};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::import::claude;

/// What a session's memories are filed under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionProject {
    /// No project folder: memories every session sees.
    Global,
    /// One project, named by its canonical path.
    Project(String),
}

impl SessionProject {
    /// What the outbox and the recall record key a session by: the project id, `""` for a global session.
    pub fn key(&self) -> &str {
        match self {
            SessionProject::Global => "",
            SessionProject::Project(id) => id,
        }
    }

    /// The request scope without its session: `{"project": id}`, or `{"kind": "global"}`.
    pub fn scope(&self) -> Value {
        match self {
            SessionProject::Global => json!({"kind": "global"}),
            SessionProject::Project(id) => json!({"project": id}),
        }
    }
}

/// The last path segment of a project id that is a path, the id itself otherwise: its name in file names.
pub fn name(id: &str) -> &str {
    match id.starts_with('/') {
        true => id.rsplit('/').find(|s| !s.is_empty()).unwrap_or(id),
        false => id,
    }
}

/// What the process environment says about a session: the host's project directory and the home directory.
#[derive(Debug, Clone)]
pub struct Env {
    /// `CLAUDE_PROJECT_DIR`.
    pub project_dir: Option<OsString>,
    pub home: PathBuf,
}

impl Env {
    pub fn from_process() -> Self {
        Self {
            project_dir: std::env::var_os("CLAUDE_PROJECT_DIR"),
            home: super::home(),
        }
    }

    /// The project of a hook event: its payload's `cwd`, unless the host gives its own session folder.
    pub fn of_event(&self, platform: &str, event: &Value) -> SessionProject {
        let cwd = event.get("cwd").and_then(Value::as_str).map(Path::new);
        self.of_folder(platform, cwd)
    }

    /// The project of a session whose working directory is `cwd`.
    pub fn of_folder(&self, platform: &str, cwd: Option<&Path>) -> SessionProject {
        let folder = session_folder(platform, self.project_dir.as_deref(), cwd);
        resolve(folder.as_deref(), &self.home)
    }
}

/// The folder a session works in. `claude-code` names it itself: a non-empty `CLAUDE_PROJECT_DIR` is the
/// folder the session started in and stays through `cd` and worktrees, so it wins; a session of another
/// platform may inherit that variable from the Claude Code that started it and ignores it. Otherwise
/// `cwd`: a hook payload's field, or the bridge process's working directory.
pub fn session_folder(
    platform: &str,
    project_dir: Option<&OsStr>,
    cwd: Option<&Path>,
) -> Option<PathBuf> {
    project_dir
        .filter(|dir| platform == claude::HOST && !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            cwd.filter(|dir| !dir.as_os_str().is_empty())
                .map(Path::to_path_buf)
        })
}

/// The project of a session in `folder`, by the design's rules in order: no folder, the home directory
/// or `/` is global, and so are the folders WorkBuddy makes when no workspace is open; inside git it is the
/// main repository's root; otherwise the folder itself. A repository at the home directory or `/` (dotfiles)
/// names no project of its own, so a folder in it is its own project. The id has symbolic links resolved
/// and no trailing `/`; a folder that no longer exists is taken as given, made absolute.
pub fn resolve(folder: Option<&Path>, home: &Path) -> SessionProject {
    let Some(folder) = folder else {
        return SessionProject::Global;
    };
    let (folder, home) = (canonical(folder), canonical(home));
    if folder == home || folder.parent().is_none() || workbuddy_default_folder(&folder, &home) {
        return SessionProject::Global;
    }
    let root = repository_root(&folder);
    let root = match root == home || root.parent().is_none() {
        true => folder,
        false => root,
    };
    SessionProject::Project(root.to_string_lossy().into_owned())
}

/// Whether `folder` is one WorkBuddy makes by itself: `<home>/WorkBuddy` (its default workspace root), its
/// `Claw` folder, or a task folder named by the time it was made (`YYYY-MM-DD-HH-MM-SS`), and anything
/// inside those. A changed default workspace path is not recognized.
fn workbuddy_default_folder(folder: &Path, home: &Path) -> bool {
    let Ok(inside) = folder.strip_prefix(home.join("WorkBuddy")) else {
        return false;
    };
    match inside.components().next() {
        None => true,
        Some(first) => {
            let first = first.as_os_str();
            first == "Claw" || first.to_str().is_some_and(is_timestamp)
        }
    }
}

/// `YYYY-MM-DD-HH-MM-SS`.
fn is_timestamp(name: &str) -> bool {
    name.len() == 19
        && name.bytes().enumerate().all(|(at, byte)| match at {
            4 | 7 | 10 | 13 | 16 => byte == b'-',
            _ => byte.is_ascii_digit(),
        })
}

/// The main repository root of `path`, from the nearest `.git` at or above it: a directory names its
/// parent; a worktree's file `gitdir: <x>/.git/worktrees/<name>` names `<x>`; any other `.git` file is
/// left to `git rev-parse --git-common-dir` (the parent of a `…/.git` result), else its own directory.
/// Outside any repository the canonicalized path itself. Never fails: a path that no longer exists comes
/// back as it is. Standard layouts resolve without running git, as git resolves them.
pub fn repository_root(path: &Path) -> PathBuf {
    let Some((dir, marker)) = nearest_git(path) else {
        return canonical(path);
    };
    if marker.is_dir() {
        return canonical(&dir);
    }
    std::fs::read_to_string(&marker)
        .ok()
        .and_then(|gitfile| worktree_main(&dir, &gitfile))
        .or_else(|| git_common_root(path))
        .unwrap_or_else(|| canonical(&dir))
}

/// The nearest directory at or above `path` with a `.git` entry, and that entry.
fn nearest_git(path: &Path) -> Option<(PathBuf, PathBuf)> {
    let start = std::path::absolute(path).ok()?;
    start.ancestors().find_map(|dir| {
        let marker = dir.join(".git");
        std::fs::metadata(&marker)
            .is_ok()
            .then(|| (dir.to_path_buf(), marker))
    })
}

fn git_common_root(path: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        // The repository is the one around `path`, not one the environment points git at.
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let common = Path::new(text.trim_end_matches(['\n', '\r']));
    if common.file_name()? != ".git" {
        return None; // a bare repository or a submodule's modules directory
    }
    Some(canonical(common.parent()?))
}

/// The main repository of a worktree whose `.git` file (in `dir`) reads `gitdir: <x>/.git/worktrees/<name>`;
/// a relative `gitdir` is relative to `dir`. Any other `.git` file (a submodule) names no main repository.
pub fn worktree_main(dir: &Path, gitfile: &str) -> Option<PathBuf> {
    let target = gitfile
        .lines()
        .find_map(|line| line.trim().strip_prefix("gitdir:"))?
        .trim();
    if target.is_empty() {
        return None;
    }
    let target = dir.join(target);
    let worktrees = target.parent()?;
    let git = worktrees.parent()?;
    if worktrees.file_name()? != "worktrees" || git.file_name()? != ".git" {
        return None;
    }
    Some(canonical(git.parent()?))
}

/// `path` with symbolic links resolved, or absolute as it is, without a trailing `/`, when it does not exist.
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path)
        .or_else(|_| std::path::absolute(path).map(|path| path.components().collect()))
        .unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Set in the child process that runs the tests needing `git`.
    const GIT_CHILD: &str = "MEMORY_IMPORT_GIT_TESTS";

    /// The tests that run `git` are ignored here and run by this one in a child process. A process
    /// spawned while another test holds a file lock keeps that lock until it execs, which fails the
    /// outbox's uploader-lock test; the child's own spawns cannot hold this process's descriptors.
    #[test]
    fn tests_that_run_git_pass_in_a_child_process() {
        if std::env::var_os(GIT_CHILD).is_some() {
            return;
        }
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["harness", "--ignored", "--test-threads=1"])
            .env(GIT_CHILD, "1")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Whether `git` runs here; the tests that need it pass without it.
    pub(crate) fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    /// git in `dir` without the user's configuration, hooks or signing.
    pub(crate) fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=Import Test",
                "-c",
                "user.email=import@example.test",
            ])
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    #[test]
    #[ignore = "runs git; run by harness::project::tests::tests_that_run_git_pass_in_a_child_process"]
    fn repository_roots_of_a_repository_and_its_worktree() {
        if !git_available() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
        // A worktree outside the repository maps to it, and git itself gives the same root.
        let outside = tmp.path().join("outside-wt");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                outside.to_str().unwrap(),
            ],
        );
        let root = std::fs::canonicalize(&repo).unwrap();
        assert_eq!(repository_root(&outside), root);
        assert_eq!(git_common_root(&outside), Some(root.clone()));
        assert_eq!(git_common_root(&tmp.path().join("no-repo")), None);
        // A directory that no longer exists still maps to the repository around it.
        assert_eq!(repository_root(&repo.join("gone/away")), root);
        // A gitfile the parser cannot place goes to git; one git cannot read either is its own
        // directory's repository.
        let module = repo.join("lib");
        std::fs::create_dir_all(&module).unwrap();
        std::fs::write(module.join(".git"), "gitdir: ../.git/modules/lib\n").unwrap();
        assert_eq!(
            repository_root(&module),
            std::fs::canonicalize(&module).unwrap()
        );
        // The session's project is the same root, however the session reached the worktree.
        let home = tmp.path().join("home");
        let project = SessionProject::Project(root.to_string_lossy().into_owned());
        assert_eq!(resolve(Some(&outside.join("src")), &home), project);
    }

    #[test]
    fn outside_a_repository_the_path_is_its_own_root() {
        let tmp = tempfile::tempdir().unwrap();
        let plain = tmp.path().join("plain");
        std::fs::create_dir(&plain).unwrap();
        if nearest_git(tmp.path()).is_some() {
            return; // the temporary directory itself sits inside a repository
        }
        let real = std::fs::canonicalize(&plain).unwrap();
        assert_eq!(repository_root(&plain), real);
        // A path that no longer exists comes back as it is, and is inside no workspace.
        let gone = plain.join("missing");
        assert_eq!(repository_root(&gone), gone);
        assert!(!crate::harness::import::project::within(
            &gone,
            std::slice::from_ref(&real)
        ));
    }

    #[test]
    fn git_files_and_directories_name_the_main_repository() {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("main");
        std::fs::create_dir_all(main.join(".git/worktrees/w1")).unwrap();
        let real = std::fs::canonicalize(&main).unwrap();
        let wt = tmp.path().join("wt");
        let absolute = format!("gitdir: {}\n", main.join(".git/worktrees/w1").display());
        assert_eq!(worktree_main(&wt, &absolute), Some(real.clone()));
        // A relative gitdir is relative to the worktree directory.
        let nested = main.join(".claude/worktrees/w1");
        std::fs::create_dir_all(&nested).unwrap();
        assert_eq!(
            worktree_main(&nested, "gitdir: ../../../.git/worktrees/w1"),
            Some(real.clone())
        );
        // A submodule's gitdir names no main repository; neither does an empty file.
        assert_eq!(worktree_main(&wt, "gitdir: ../.git/modules/lib"), None);
        assert_eq!(worktree_main(&wt, ""), None);
        // repository_root reads the nearest marker without running git: the worktree's file, the
        // repository's directory.
        std::fs::write(nested.join(".git"), &absolute).unwrap();
        assert_eq!(repository_root(&nested.join("src")), real);
        assert_eq!(repository_root(&main.join("src/deep")), real);
    }

    /// The environment is an argument, so no test touches the process's: `CLAUDE_PROJECT_DIR` is the
    /// `claude-code` session's folder, and `cwd` the folder of every session without one.
    #[test]
    fn the_session_folder_is_claude_project_dir_only_for_claude_code() {
        let dir = OsString::from("/work/started-here");
        let cwd = Path::new("/work/after-cd");
        let folder = session_folder;
        let started = Some(PathBuf::from("/work/started-here"));
        assert_eq!(folder("claude-code", Some(&dir), Some(cwd)), started);
        assert_eq!(folder("claude-code", Some(&dir), None), started);
        // Another platform ignores a variable it inherited, and so does an empty one.
        assert_eq!(folder("codex", Some(&dir), Some(cwd)), Some(cwd.into()));
        assert_eq!(folder("workbuddy", Some(&dir), None), None);
        let empty = OsString::new();
        assert_eq!(
            folder("claude-code", Some(&empty), Some(cwd)),
            Some(cwd.into())
        );
        assert_eq!(folder("claude-code", None, Some(cwd)), Some(cwd.into()));
        // No folder at all, as a WorkBuddy SessionStart has none.
        assert_eq!(folder("claude-code", None, None), None);
        assert_eq!(folder("codex", None, Some(Path::new(""))), None);
        // A hook event's `cwd` field, read through the environment it is resolved in.
        let env = Env {
            project_dir: Some(dir),
            home: PathBuf::from("/home/test"),
        };
        let at = |platform: &str, event: Value| env.of_event(platform, &event);
        let project = |id: &str| SessionProject::Project(id.into());
        assert_eq!(
            at("claude-code", json!({"cwd": "/work/after-cd"})),
            project("/work/started-here")
        );
        assert_eq!(
            at("codex", json!({"cwd": "/work/after-cd"})),
            project("/work/after-cd")
        );
        assert_eq!(at("workbuddy", json!({})), SessionProject::Global);
        assert_eq!(at("workbuddy", json!({"cwd": 7})), SessionProject::Global);
    }

    #[test]
    fn session_folders_are_global_or_a_project_named_by_a_canonical_path() {
        let tmp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(tmp.path()).unwrap();
        if nearest_git(&base).is_some() {
            return; // the temporary directory itself sits inside a repository
        }
        let home = base.join("home");
        let wb = home.join("WorkBuddy");
        let task = wb.join("2026-10-28-09-15-30/notes");
        let plain = home.join("code/plain");
        let repo = home.join("code/repo");
        let worktree = home.join("code/repo/.claude/worktrees/w1");
        let dotfiles = home.join("dotfiles-user");
        for dir in [
            &task,
            &plain,
            &wb.join("Claw/job"),
            &wb.join("my-project"),
            &wb.join("2026-10-28"),
            &repo.join(".git/worktrees/w1"),
            &repo.join("src/deep"),
            &worktree,
            &dotfiles,
        ] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", repo.join(".git/worktrees/w1").display()),
        )
        .unwrap();
        let project = |path: &Path| SessionProject::Project(path.to_str().unwrap().into());
        let of = |folder: &Path| resolve(Some(folder), &home);
        // No project: no folder, the home directory, `/`, and what WorkBuddy makes without a workspace.
        let global = SessionProject::Global;
        assert_eq!(resolve(None, &home), global);
        for folder in [
            &home,
            Path::new("/"),
            &wb,
            &wb.join("Claw"),
            &wb.join("Claw/job"),
            &wb.join("2026-10-28-09-15-30"),
            &task,
        ] {
            assert_eq!(of(folder), global, "{}", folder.display());
        }
        // A folder of the user's own under WorkBuddy's root, or one that only looks like a stamp, is a project.
        for folder in [&wb.join("my-project"), &wb.join("2026-10-28")] {
            assert_eq!(of(folder), project(folder), "{}", folder.display());
        }
        // Inside a repository, the main repository's root, from a subdirectory and from a worktree.
        assert_eq!(of(&repo), project(&repo));
        assert_eq!(of(&repo.join("src/deep")), project(&repo));
        assert_eq!(of(&worktree), project(&repo));
        // Outside git, the folder itself: a symbolic link gives its target, a trailing `/` goes, and a
        // folder that is gone is taken as given.
        assert_eq!(of(&plain), project(&plain));
        assert_eq!(of(&plain.join("../plain/")), project(&plain));
        let link = base.join("link");
        std::os::unix::fs::symlink(&plain, &link).unwrap();
        assert_eq!(of(&link), project(&plain));
        let linked_home = base.join("home-link");
        std::os::unix::fs::symlink(&home, &linked_home).unwrap();
        assert_eq!(resolve(Some(&home), &linked_home), global);
        let gone = plain.join("gone/");
        assert_eq!(of(&gone), project(&plain.join("gone")));
        // A repository at the home directory is no project of its own: its folders are theirs.
        std::fs::create_dir(home.join(".git")).unwrap();
        assert_eq!(of(&home), global);
        assert_eq!(of(&dotfiles), project(&dotfiles));
        assert_eq!(of(&repo), project(&repo));
        assert_eq!(name("/Users/me/code/app/"), "app");
        assert_eq!((name("/"), name("atlas")), ("/", "atlas"));
    }
}
