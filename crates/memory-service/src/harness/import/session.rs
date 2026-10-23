//! Past session turns (docs/design/memory-import.md §6.3, §6.10): what the transcript adapters hand the planner.
//! Transcripts are private formats, so an adapter keeps a turn only when the host's own provenance fields say a
//! person asked it (Claude Code `turnOrigin: human`, Codex `thread_source: user`); a session without those fields
//! is `unknown-format`, never guessed at. A turn is the prompt and the final reply, without the blocks hosts and
//! this service inject. Reads never follow symbolic links and stop at a size cap; nothing here talks to the
//! network or the daemon.
use regex::Regex;
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use super::source::{Issue, ScanContext};
use crate::harness::Error;

/// Bytes of one transcript read, after decompression: a long session's rollout runs to tens of MB.
pub const MAX_TRANSCRIPT: u64 = 512 << 20;
/// Bytes of a turn's prompt and reply together; a longer turn (a pasted log) is skipped as `too-large`.
pub const MAX_TURN_BYTES: usize = 256 << 10;

/// Blocks a prompt or reply may carry that the person did not write: this service's recall block and
/// Claude Code's host context.
static INJECTED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?is)<shared-memory\b[^>]*>.*?</shared-memory\s*>|<system-reminder\b[^>]*>.*?</system-reminder\s*>")
        .expect("valid pattern")
});

/// One turn of a past session, as read; the plan redacts it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Turn {
    /// The host's id of the turn: Claude Code's `promptId`, Codex's `turn_id`.
    pub native_id: String,
    pub prompt: String,
    pub reply: String,
    /// RFC3339 UTC; empty when the transcript gives none.
    pub prompt_at: String,
    pub reply_at: String,
    /// Why the turn is not imported (`turnOrigin:sdk`, `no-reply`, ...); `None` to import it.
    pub skip: Option<String>,
}

/// One transcript.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionDoc {
    /// Platform label of the host, as capture files the session's turns.
    pub host: String,
    /// claude-transcript | codex-rollout
    pub via: &'static str,
    pub path: PathBuf,
    /// `path` in `~/…` form.
    pub display_path: String,
    /// The host's session id: Claude Code's `sessionId`, Codex's `session_meta.id`.
    pub session: String,
    pub cwd: PathBuf,
    /// blake3 hex of the file as stored.
    pub file_hash: String,
    /// RFC3339 UTC time the session started; a turn found in two sessions is the earlier one's.
    pub started_at: String,
    pub turns: Vec<Turn>,
    /// Provenance merged into each unit's `source`: the host version and the like.
    pub extra: Value,
}

#[derive(Debug, Default)]
pub struct SessionScan {
    pub sessions: Vec<SessionDoc>,
    pub issues: Vec<Issue>,
}

impl SessionScan {
    pub fn issue(&mut self, path: String, reason: impl Into<String>) {
        self.issues.push(Issue {
            path: path.into(),
            reason: reason.into(),
        });
    }
}

/// Reads one host's session transcripts of the workspaces. A transcript that cannot be imported is an issue,
/// never an error; an error means the scan cannot run (no host directory, no workspaces).
pub trait SessionAdapter {
    fn scan_sessions(&self, ctx: &ScanContext) -> Result<SessionScan, Error>;
}

/// `text` without injected blocks, trimmed.
pub fn strip_injected(text: &str) -> String {
    INJECTED.replace_all(text, "").trim().to_string()
}

/// The verdict on a turn read whole: too large, or empty once injected blocks are gone. The texts come back
/// stripped.
pub fn finish(mut turn: Turn) -> Turn {
    turn.prompt = strip_injected(&turn.prompt);
    turn.reply = strip_injected(&turn.reply);
    if turn.skip.is_none() {
        if turn.prompt.is_empty() {
            turn.skip = Some("empty".into());
        } else if turn.prompt.len() + turn.reply.len() > MAX_TURN_BYTES {
            turn.skip = Some("too-large".into());
        }
    }
    turn
}

/// The text blocks of a message `content`: the string itself, or the `text` of each block of type `text`
/// (`input_text` too, as Codex names some), joined by blank lines; images and tool blocks are left out.
pub fn content_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|b| matches!(b["type"].as_str(), Some("text" | "input_text")))
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => String::new(),
    }
}

/// Reads hash what they read.
struct Hashing<R> {
    inner: R,
    hash: blake3::Hasher,
}

impl<R: Read> Read for Hashing<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.hash.update(&buf[..n]);
        Ok(n)
    }
}

/// Read the regular file `path` line by line, zstd-compressed when its name ends in `.zst`, never through a
/// symbolic link and at most `max` bytes after decompression; `line` sees each line that is UTF-8 and may stop
/// the read by returning false. Returns the blake3 hex of the bytes as stored (of those read, when stopped).
/// The error is the skip reason: `symlink`, `not-a-file`, `too-large` or `unreadable: <error>`.
pub fn read_lines(
    path: &Path,
    max: u64,
    mut line: impl FnMut(&str) -> bool,
) -> Result<String, String> {
    let unreadable = |e: &dyn std::fmt::Display| format!("unreadable: {e}");
    let meta = std::fs::symlink_metadata(path).map_err(|e| unreadable(&e))?;
    if meta.file_type().is_symlink() {
        return Err("symlink".into());
    }
    if !meta.is_file() {
        return Err("not-a-file".into());
    }
    let compressed = path.extension().is_some_and(|ext| ext == "zst");
    if !compressed && meta.len() > max {
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
    let mut hashing = Hashing {
        inner: BufReader::new(file),
        hash: blake3::Hasher::new(),
    };
    let reader: Box<dyn Read + '_> = match compressed {
        true => {
            Box::new(zstd::stream::read::Decoder::new(&mut hashing).map_err(|e| unreadable(&e))?)
        }
        false => Box::new(&mut hashing),
    };
    let mut reader = BufReader::new(reader.take(max + 1));
    let (mut total, mut buf) = (0u64, Vec::new());
    loop {
        buf.clear();
        let n = reader
            .read_until(b'\n', &mut buf)
            .map_err(|e| unreadable(&e))?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > max {
            return Err("too-large".into());
        }
        if let Ok(text) = std::str::from_utf8(&buf) {
            if !line(text.trim_end_matches(['\n', '\r'])) {
                break;
            }
        }
    }
    drop(reader);
    Ok(hashing.hash.finalize().to_hex().to_string())
}

/// The first `cwd` field of the first lines of `path` (at most `bytes` of them), for a transcript whose records
/// carry one: which project a session belongs to, before it is read whole.
pub fn first_cwd(path: &Path, bytes: u64) -> Option<PathBuf> {
    let mut found = None;
    let mut seen = 0u64;
    read_lines(path, MAX_TRANSCRIPT, |line| {
        seen += line.len() as u64 + 1;
        if line.contains("\"cwd\"") {
            if let Ok(record) = serde_json::from_str::<Value>(line) {
                let cwd = record
                    .get("cwd")
                    .or_else(|| record.get("payload").and_then(|p| p.get("cwd")))
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                    .filter(|cwd| cwd.is_absolute());
                if cwd.is_some() {
                    found = cwd;
                    return false;
                }
            }
        }
        seen < bytes
    })
    .ok()?;
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn injected_blocks_are_stripped() {
        let prompt = "<system-reminder>\nAs you answer, use this context.\n</system-reminder>\nShip the fix.\n\
                      <shared-memory project=\"p\" event=\"UserPromptSubmit\">\nRecalled: x\n</shared-memory>";
        assert_eq!(strip_injected(prompt), "Ship the fix.");
        // An unclosed block is the person's text, not an injection.
        assert_eq!(
            strip_injected("<shared-memory> left open"),
            "<shared-memory> left open"
        );
        let turn = finish(Turn {
            prompt: "<system-reminder>only context</system-reminder>".into(),
            reply: "Done.".into(),
            ..Turn::default()
        });
        assert_eq!(turn.skip.as_deref(), Some("empty"));
        let big = finish(Turn {
            prompt: "x".repeat(MAX_TURN_BYTES),
            reply: "Done.".into(),
            ..Turn::default()
        });
        assert_eq!(big.skip.as_deref(), Some("too-large"));
        let kept = finish(Turn {
            prompt: "Ship it.".into(),
            reply: "Shipped.".into(),
            skip: Some("no-reply".into()),
            ..Turn::default()
        });
        assert_eq!(kept.skip.as_deref(), Some("no-reply"));
        assert_eq!(
            content_text(
                &serde_json::json!([{"type": "text", "text": "a"}, {"type": "image"},
                                             {"type": "input_text", "text": "b"}])
            ),
            "a\n\nb"
        );
        assert_eq!(content_text(&serde_json::json!("plain")), "plain");
    }

    #[test]
    fn lines_are_read_plain_or_compressed_within_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let text = "{\"a\":1}\n{\"cwd\":\"/w\"}\n\u{0}\u{ff}not json\n{\"b\":2}";
        let plain = dir.path().join("rollout-1.jsonl");
        std::fs::write(&plain, text).unwrap();
        let packed = dir.path().join("rollout-2.jsonl.zst");
        let mut encoder =
            zstd::stream::write::Encoder::new(File::create(&packed).unwrap(), 3).unwrap();
        encoder.write_all(text.as_bytes()).unwrap();
        encoder.finish().unwrap();
        for path in [&plain, &packed] {
            let mut lines = Vec::new();
            let hash = read_lines(path, 1 << 20, |l| {
                lines.push(l.to_string());
                true
            })
            .unwrap();
            assert_eq!(lines.len(), 4);
            assert_eq!(lines[3], "{\"b\":2}");
            let stored = std::fs::read(path).unwrap();
            assert_eq!(hash, blake3::hash(&stored).to_hex().to_string());
            // A cap below the decompressed size refuses the file.
            assert_eq!(read_lines(path, 10, |_| true).unwrap_err(), "too-large");
        }
        // The first cwd is found, but only in the first bytes.
        assert_eq!(first_cwd(&plain, 1 << 10), Some(PathBuf::from("/w")));
        assert_eq!(first_cwd(&plain, 4), None);
        #[cfg(unix)]
        {
            let link = dir.path().join("link.jsonl");
            std::os::unix::fs::symlink(&plain, &link).unwrap();
            assert_eq!(read_lines(&link, 1 << 20, |_| true).unwrap_err(), "symlink");
            assert_eq!(first_cwd(&link, 1 << 10), None);
        }
        assert_eq!(
            read_lines(dir.path(), 1 << 20, |_| true).unwrap_err(),
            "not-a-file"
        );
        let broken = dir.path().join("rollout-3.jsonl.zst");
        std::fs::write(&broken, "not zstd").unwrap();
        assert!(read_lines(&broken, 1 << 20, |_| true)
            .unwrap_err()
            .starts_with("unreadable"));
    }
}
