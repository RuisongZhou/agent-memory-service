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

#[cfg(test)]
mod tests {
    use super::*;

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
