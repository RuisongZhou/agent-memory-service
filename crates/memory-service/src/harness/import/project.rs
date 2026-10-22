//! Project mapping (docs/design/memory-import.md §6.4): a host files memories by directory, and a
//! directory belongs to a workspace when its repository root is the workspace's. A worktree maps to its
//! main repository, as Claude Code shares one auto-memory directory across the worktrees of a repository.
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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

/// `path` with symbolic links resolved, or absolute as it is when it does not exist.
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path)
        .or_else(|_| std::path::absolute(path))
        .unwrap_or_else(|_| path.to_path_buf())
}

/// Claude Code's directory name for a project under `~/.claude/projects`: every character that is not
/// an ASCII letter or digit becomes `-`, per UTF-16 code unit as JavaScript's
/// `replace(/[^a-zA-Z0-9]/g, "-")` counts them (`/Users/a/my-app` → `-Users-a-my-app`).
pub fn claude_dir_name(path: &Path) -> String {
    let mut name = String::new();
    for c in path.to_string_lossy().chars() {
        if c.is_ascii_alphanumeric() {
            name.push(c);
        } else {
            name.extend(std::iter::repeat_n('-', c.len_utf16()));
        }
    }
    name
}

/// Whether `path` is one of `roots` or inside one, both canonicalized. A path that cannot be
/// canonicalized (it no longer exists) is inside none.
pub fn within(path: &Path, roots: &[PathBuf]) -> bool {
    let Ok(path) = std::fs::canonicalize(path) else {
        return false;
    };
    roots.iter().any(|root| {
        std::fs::canonicalize(root)
            .map(|root| path.starts_with(root))
            .unwrap_or(false)
    })
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
            .args(["harness::import", "--ignored", "--test-threads=1"])
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
    #[ignore = "runs git; run by harness::import::project::tests::tests_that_run_git_pass_in_a_child_process"]
    fn repository_roots_of_a_repository_and_its_worktree() {
        if !git_available() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(repo.join("src/deep")).unwrap();
        git(&repo, &["init", "-q"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
        let worktree = repo.join(".claude/worktrees/w1");
        let add = |path: &Path| {
            git(
                &repo,
                &["worktree", "add", "-q", "--detach", path.to_str().unwrap()],
            )
        };
        add(&worktree);
        let root = std::fs::canonicalize(&repo).unwrap();
        assert_eq!(repository_root(&repo), root);
        assert_eq!(repository_root(&repo.join("src/deep")), root);
        assert_eq!(repository_root(&worktree), root);
        // A worktree outside the repository maps to it as well.
        let outside = tmp.path().join("outside-wt");
        add(&outside);
        assert_eq!(repository_root(&outside), root);
        // A directory that no longer exists still maps to the repository around it.
        assert_eq!(repository_root(&repo.join("gone/away")), root);
        // git itself gives the same roots for what the gitfile parser answers.
        for path in [&repo, &worktree, &outside] {
            assert_eq!(git_common_root(path), Some(root.clone()), "{path:?}");
        }
        assert_eq!(git_common_root(&tmp.path().join("no-repo")), None);
        // A gitfile the parser cannot place goes to git; one git cannot read either is its own
        // directory's repository.
        let module = repo.join("lib");
        std::fs::create_dir_all(&module).unwrap();
        std::fs::write(module.join(".git"), "gitdir: ../.git/modules/lib\n").unwrap();
        assert_eq!(
            repository_root(&module),
            std::fs::canonicalize(&module).unwrap()
        );
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
        assert!(!within(&gone, std::slice::from_ref(&real)));
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

    #[test]
    fn claude_directory_names_encode_every_other_character() {
        assert_eq!(
            claude_dir_name(Path::new("/Users/a/code/my-app")),
            "-Users-a-code-my-app"
        );
        assert_eq!(claude_dir_name(Path::new("/x/.claude/w")), "-x--claude-w");
        assert_eq!(claude_dir_name(Path::new("/srv/项目_1")), "-srv----1");
        // Characters outside the Basic Multilingual Plane are two UTF-16 code units.
        assert_eq!(claude_dir_name(Path::new("/a/\u{1F600}")), "-a---");
    }

    #[test]
    fn within_compares_canonical_paths_by_component() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("app");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::create_dir_all(tmp.path().join("app-other")).unwrap();
        let roots = [root.clone()];
        assert!(within(&root, &roots));
        assert!(within(&root.join("sub"), &roots));
        assert!(within(&root.join("sub/.."), &roots));
        assert!(!within(&tmp.path().join("app-other"), &roots));
        assert!(!within(&root.join("sub/../../app-other"), &roots));
        assert!(!within(tmp.path(), &roots));
    }
}
