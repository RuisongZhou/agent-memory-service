//! `serve --env-file`: provider settings from a local dotenv file, read as data (no shell
//! evaluation) under the rules of `scripts/serve.py`. A non-empty environment variable wins.
use anyhow::{Context, Result};
use std::path::Path;

/// The only keys read; anything else in the file is ignored.
pub const KEYS: [&str; 10] = [
    "QWEN_API_KEY",
    "QWEN_BASE_URL",
    "LLM_MODEL",
    "MEMORY_STRATEGY",
    "MEMORY_MODEL_PROVIDER",
    "MEMORY_MODEL_API_KEY",
    "MEMORY_EMBED_URL",
    "MEMORY_RERANK_URL",
    "MEMORY_EMBED_MODEL",
    "MEMORY_RERANK_MODEL",
];

/// (key, value) settings in `text`: the first non-empty value of each key, else its last empty one.
pub fn parse(text: &str) -> Vec<(&'static str, String)> {
    let mut found: Vec<(&'static str, String)> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let Some(key) = KEYS.iter().copied().find(|k| *k == key.trim()) else {
            continue;
        };
        let value = value
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .to_string();
        match found.iter_mut().find(|(k, _)| *k == key) {
            Some((_, current)) if current.is_empty() => *current = value,
            Some(_) => {}
            None => found.push((key, value)),
        }
    }
    found
}

/// Set each key from `path` that the environment leaves unset or empty; returns their names.
/// Call before the runtime starts: the environment must not change while other threads read it.
pub fn apply(path: &Path) -> Result<Vec<&'static str>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read --env-file {}", path.display()))?;
    let mut set = Vec::new();
    for (key, value) in parse(&text) {
        if std::env::var_os(key).is_none_or(|v| v.is_empty()) {
            std::env::set_var(key, value);
            set.push(key);
        }
    }
    Ok(set)
}

/// `text` with each of `values` set: the first line that assigns the key gets the new value, later ones are
/// dropped (so `parse` reads the new value), and a key the file does not assign is appended. Every other line,
/// comments included, stays as it is. The values are checked by the caller: one line, no surrounding quotes.
pub fn with_values(text: &str, values: &[(&str, &str)]) -> String {
    let assigned = |line: &str| {
        let line = line.trim();
        let line = line.strip_prefix("export ").unwrap_or(line);
        line.split_once('=').map(|(key, _)| key.trim().to_string())
    };
    let mut done = vec![false; values.len()];
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        match assigned(line).and_then(|key| values.iter().position(|(k, _)| *k == key)) {
            Some(i) if done[i] => continue,
            Some(i) => {
                done[i] = true;
                out.push_str(&format!("{}={}\n", values[i].0, values[i].1));
            }
            None => {
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    for (i, (key, value)) in values.iter().enumerate() {
        if !done[i] {
            out.push_str(&format!("{key}={value}\n"));
        }
    }
    out
}

/// Write `text` to `path` readable by the owner only, through a temporary file renamed over it.
pub fn write_private(path: &Path, text: &str) -> Result<()> {
    let dir = path.parent().context("the env file is in a directory")?;
    let name = path.file_name().context("the env file has a name")?;
    let temp = dir.join(format!(
        ".{}.{}.tmp",
        name.to_string_lossy(),
        std::process::id()
    ));
    let written = (|| -> std::io::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        use std::io::Write;
        let mut file = options.open(&temp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&temp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written.with_context(|| format!("cannot write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_replace_the_first_assignment_and_keep_every_other_line() {
        let text = "# keys\nQWEN_API_KEY=old\nexport LLM_MODEL = a\nOTHER=1\nLLM_MODEL=b\n";
        let out = with_values(
            text,
            &[
                ("LLM_MODEL", "qwen"),
                ("MEMORY_EMBED_URL", "http://127.0.0.1:8081"),
            ],
        );
        assert_eq!(
            out,
            "# keys\nQWEN_API_KEY=old\nLLM_MODEL=qwen\nOTHER=1\nMEMORY_EMBED_URL=http://127.0.0.1:8081\n"
        );
        let read = parse(&out);
        assert!(
            read.contains(&("LLM_MODEL", "qwen".to_string())),
            "{read:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_written_for_its_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");
        std::fs::write(&path, "x").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_private(&path, "QWEN_API_KEY=k\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "QWEN_API_KEY=k\n");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn reads_known_keys_as_data_like_serve_py() {
        let text = "# comment\nexport QWEN_API_KEY=\"sk-test\"\nQWEN_BASE_URL = 'http://127.0.0.1:9/v1'\n\
                    LLM_MODEL=\nLLM_MODEL=qwen\nLLM_MODEL=later\nOTHER=$(rm -rf /)\nMEMORY_STRATEGY\n\
                    MEMORY_MODEL_PROVIDER=self-hosted\n";
        assert_eq!(
            parse(text),
            [
                ("QWEN_API_KEY", "sk-test".to_string()),
                ("QWEN_BASE_URL", "http://127.0.0.1:9/v1".to_string()),
                ("LLM_MODEL", "qwen".to_string()),
                ("MEMORY_MODEL_PROVIDER", "self-hosted".to_string()),
            ]
        );
    }
}
