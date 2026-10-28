//! Where a host files memories (docs/design/memory-import.md §6.4): by directory, and a directory belongs to a
//! workspace when its repository root (`harness::project::repository_root`) is the workspace's.
use std::path::{Path, PathBuf};

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
mod tests {
    use super::*;

    #[test]
    fn claude_directory_names_encode_every_other_character() {
        assert_eq!(
            claude_dir_name(Path::new("/Users/a/code/my-app")),
            "-Users-a-code-my-app"
        );
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
        assert!(!within(&tmp.path().join("app-other"), &roots));
        assert!(!within(&root.join("sub/../../app-other"), &roots));
    }
}
