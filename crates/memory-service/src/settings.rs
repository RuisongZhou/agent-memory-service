//! The model settings the web UI saves (docs/design/model-settings.md): `serve --env-file` is read once at startup,
//! so a save writes that file and restarts the daemon in place, as the same process (`exec`), under whichever service
//! manager or terminal started it. A start that fails with the saved file restores the one before it and starts again.
use crate::envfile;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Set while a save restarts the daemon: the store's database stops on purpose then, and the daemon must not exit
/// for it before it has replaced itself.
pub static RESTARTING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// How `serve` was started: what a restart repeats.
#[derive(Debug, Clone, Default)]
pub struct Launch {
    /// `--env-file`, absolute; without one the web UI shows the settings and cannot save them.
    pub env_file: Option<PathBuf>,
    /// Keys `envfile::apply` set from the file. A restart removes them from its environment, or they would
    /// override the file it is about to read.
    pub from_file: Vec<&'static str>,
    /// The command line, program name first.
    pub argv: Vec<OsString>,
    /// `mem0RecallFloor` as configured, before the connected reranker settled it: a settings test settles it again
    /// for the reranker it tries, as the restart will.
    pub recall_floor: Option<f32>,
}

impl Launch {
    pub fn new(env_file: Option<&Path>, from_file: Vec<&'static str>) -> Result<Self> {
        Ok(Self {
            env_file: env_file.map(std::path::absolute).transpose()?,
            from_file,
            argv: std::env::args_os().collect(),
            recall_floor: None,
        })
    }

    /// Whether the command line gives `flag` (`--embed-url U` or `--embed-url=U`).
    pub fn has_flag(&self, flag: &str) -> bool {
        self.argv.iter().skip(1).any(|a| {
            a.to_str().is_some_and(|a| {
                a == flag || a.strip_prefix(flag).is_some_and(|r| r.starts_with('='))
            })
        })
    }

    /// Replace this process with a new `serve` started the same way, reading the env file afresh. Returns only
    /// when the new program could not be started.
    #[cfg(unix)]
    pub fn restart(&self) -> anyhow::Error {
        use std::os::unix::process::CommandExt;
        let program = match std::env::current_exe() {
            Ok(program) => program,
            Err(e) => {
                return anyhow::Error::new(e).context("cannot find the daemon's own executable")
            }
        };
        let mut command = std::process::Command::new(program);
        command.args(self.argv.iter().skip(1));
        for key in &self.from_file {
            command.env_remove(key);
        }
        anyhow::Error::new(command.exec()).context("cannot restart the daemon")
    }

    #[cfg(not(unix))]
    pub fn restart(&self) -> anyhow::Error {
        anyhow::anyhow!("restarting from the web UI needs a Unix system")
    }
}

/// The file kept beside the env file while a saved one has not started yet: the env file as it was before.
pub fn previous_path(env_file: &Path) -> PathBuf {
    sibling(env_file, "previous")
}

/// What the last save ended in when its settings did not start: `{at, error}`.
pub fn rollback_path(env_file: &Path) -> PathBuf {
    sibling(env_file, "rolled-back.json")
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{suffix}"));
    path.with_file_name(name)
}

/// Save `text` as the env file, keeping the current one as the previous file until the daemon starts with it.
pub fn save(env_file: &Path, text: &str) -> Result<()> {
    let current = std::fs::read_to_string(env_file)
        .with_context(|| format!("cannot read {}", env_file.display()))?;
    envfile::write_private(&previous_path(env_file), &current)?;
    let _ = std::fs::remove_file(rollback_path(env_file));
    envfile::write_private(env_file, text)
}

/// The daemon has started: the saved env file holds.
pub fn started(launch: &Launch) {
    if let Some(env_file) = &launch.env_file {
        let _ = std::fs::remove_file(previous_path(env_file));
    }
}

/// The daemon could not start with its settings. After a save, put the previous env file back, note why and
/// start again with it; otherwise, or when even that fails, return the error.
pub fn failed(launch: &Launch, error: anyhow::Error) -> anyhow::Error {
    let Some(env_file) = &launch.env_file else {
        return error;
    };
    let previous = previous_path(env_file);
    let Ok(text) = std::fs::read_to_string(&previous) else {
        return error;
    };
    let note = json!({"at": memory_rules::util::now().to_rfc3339(), "error": format!("{error:#}")});
    let restored = envfile::write_private(env_file, &text)
        .and_then(|()| envfile::write_private(&rollback_path(env_file), &note.to_string()))
        .and_then(|()| {
            std::fs::remove_file(&previous).context("cannot remove the previous env file")
        });
    if let Err(e) = restored {
        return error.context(format!(
            "and the previous settings could not be restored: {e:#}"
        ));
    }
    tracing::error!(
        "the saved model settings did not start ({error:#}); restored the previous ones"
    );
    launch.restart().context(format!("{error:#}"))
}

/// The note `failed` left, if the last save was rolled back.
pub fn rolled_back(env_file: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(rollback_path(env_file)).ok()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flag_counts_in_both_forms_and_never_as_a_value() {
        let launch = Launch {
            argv: [
                "agent-memoryd",
                "serve",
                "--embed-url=http://x",
                "--llm-model",
                "--rerank-url",
            ]
            .map(OsString::from)
            .to_vec(),
            ..Default::default()
        };
        assert!(launch.has_flag("--embed-url"));
        assert!(launch.has_flag("--llm-model"));
        assert!(!launch.has_flag("--embed-model"));
        assert!(!launch.has_flag("--rerank"));
    }

    #[test]
    fn a_failed_start_after_a_save_restores_the_previous_file() {
        let dir = tempfile::tempdir().unwrap();
        let env_file = dir.path().join(".env");
        std::fs::write(&env_file, "LLM_MODEL=old\n").unwrap();
        save(&env_file, "LLM_MODEL=new\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(&env_file).unwrap(),
            "LLM_MODEL=new\n"
        );
        assert_eq!(
            std::fs::read_to_string(previous_path(&env_file)).unwrap(),
            "LLM_MODEL=old\n"
        );

        // Without the file the start is not after a save: the error is returned and nothing changes.
        let other = Launch::default();
        assert_eq!(failed(&other, anyhow::anyhow!("boom")).to_string(), "boom");

        // A start that holds drops the previous file; a later failure has nothing to restore.
        let launch = Launch {
            env_file: Some(env_file.clone()),
            ..Default::default()
        };
        started(&launch);
        assert!(!previous_path(&env_file).exists());
        assert_eq!(failed(&launch, anyhow::anyhow!("boom")).to_string(), "boom");
        assert_eq!(
            std::fs::read_to_string(&env_file).unwrap(),
            "LLM_MODEL=new\n"
        );
        assert!(rolled_back(&env_file).is_none());
    }
}
