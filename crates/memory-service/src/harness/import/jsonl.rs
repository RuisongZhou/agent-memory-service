//! Agent exports (docs/design/memory-import.md §6.11): the JSONL a host's own agent writes when asked
//! with the prompt of `agent-memoryd import prompt`, for hosts whose memory cannot be read from disk. One
//! memory per line; the answer may wrap the lines in a code fence and pause with `{"continued": true}`.
//! Every line is one segment of one document for the file. An export is untrusted data like any host
//! file: the planner redacts it, and an exported instruction is only ever a candidate.
use memory_rules::import::units::content_hash;
use memory_rules::util::is_valid_date;
use serde_json::{Map, Value};

use super::source::{
    read_text, Adapter, Locators, Policy, ScanContext, ScanOutput, Segment, SourceDoc,
};
use crate::harness::{invalid, Error};

/// Platform label of an export without `--label`.
pub const DEFAULT_LABEL: &str = "agent-export";
const VIA: &str = "agent-export";
/// Largest export read.
const MAX_EXPORT: u64 = 8 << 20;
/// Kinds an export line may name; `fact` when it names none.
pub const KINDS: [&str; 4] = ["instruction", "preference", "fact", "reference"];
/// Scopes an export line may name; `project` when it names none. Only a hint: facts are project-scoped.
const SCOPES: [&str; 2] = ["project", "global"];

pub struct Export;

impl Adapter for Export {
    fn scan(&self, ctx: &ScanContext) -> Result<ScanOutput, Error> {
        let from = ctx
            .from
            .as_deref()
            .ok_or_else(|| invalid("--host jsonl needs --from FILE, the export to read"))?;
        let label = ctx.label.as_deref().unwrap_or(DEFAULT_LABEL);
        let path = std::fs::canonicalize(from)
            .map_err(|e| invalid(format!("--from {}: {e}", ctx.display(from))))?;
        let shown = ctx.display(&path);
        let (raw, _) = read_text(&path, MAX_EXPORT)
            .map_err(|reason| invalid(format!("--from {shown}: {reason}")))?;
        let mut out = ScanOutput::default();
        let mut locators = Locators::default();
        let mut segments = Vec::new();
        for (index, line) in raw.lines().enumerate() {
            match parse_line(line) {
                Line::Ignored => {}
                Line::Invalid(why) => {
                    out.issue(shown.clone(), format!("invalid-line {}: {why}", index + 1))
                }
                Line::Memory(entry) => segments.push(entry.segment(index + 1, &mut locators)),
            }
        }
        if segments.is_empty() {
            out.issue(shown, "empty");
            return Ok(out);
        }
        out.docs.push(SourceDoc {
            host: label.to_string(),
            via: VIA,
            path,
            display_path: shown,
            native_id: "export".into(),
            title: None,
            cwd: None,
            user_level: false,
            file_hash: content_hash(&raw),
            // Only a dated line has a time: an undated one must not look newer than dated memories.
            stated_at: String::new(),
            kind_hint: None,
            instruction_candidate: false,
            segments,
            extra: Value::Null,
        });
        Ok(out)
    }
}

#[derive(Debug, PartialEq)]
enum Line {
    /// Blank, a code fence or the `{"continued": true}` page marker.
    Ignored,
    Invalid(String),
    Memory(Entry),
}

#[derive(Debug, PartialEq)]
struct Entry {
    text: String,
    kind: String,
    date: Option<String>,
    source: Option<String>,
    scope: String,
    /// Values that were given but not understood, kept for the reviewer.
    notes: Map<String, Value>,
}

impl Entry {
    /// The locator is the start of the text's hash, so it stays the same when an agent exports again in
    /// another order; a repeated text gets `~2`.
    fn segment(self, line: usize, locators: &mut Locators) -> Segment {
        let locator = locators.unique(content_hash(&self.text)[..16].to_string());
        let mut extra = Map::new();
        extra.insert("line".into(), line.into());
        extra.insert("source".into(), self.source.into());
        extra.insert("scopeHint".into(), self.scope.into());
        extra.extend(self.notes);
        let instruction = self.kind == "instruction";
        Segment {
            locator,
            heading_path: Vec::new(),
            text: self.text,
            policy: Policy::VerbatimIfFits,
            stated_at: self.date.map(|date| format!("{date}T00:00:00Z")),
            kind_hint: Some(self.kind),
            instruction_candidate: Some(instruction),
            extra: Value::Object(extra),
        }
    }
}

/// A string field, trimmed; None when it is absent, null or blank.
fn given<'a>(fields: &'a Map<String, Value>, key: &str) -> Result<Option<&'a str>, &'a Value> {
    match fields.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.trim())),
        Some(other) => Err(other),
    }
}

/// A value for an issue or note: a string as it is, anything else as JSON, cut to 40 characters.
fn shown(value: &Value) -> String {
    let text = match value {
        Value::String(s) => s.trim().to_string(),
        other => other.to_string(),
    };
    match text.chars().count() > 40 {
        true => text.chars().take(40).collect::<String>() + "…",
        false => text,
    }
}

fn parse_line(line: &str) -> Line {
    let line = line.trim();
    if line.is_empty() || line.starts_with("```") || line.starts_with("~~~") {
        return Line::Ignored;
    }
    let Ok(Value::Object(fields)) = serde_json::from_str::<Value>(line) else {
        return Line::Invalid("not a JSON object".into());
    };
    if fields.get("continued") == Some(&Value::Bool(true)) && !fields.contains_key("text") {
        return Line::Ignored;
    }
    let Ok(Some(text)) = given(&fields, "text") else {
        return Line::Invalid("no text".into());
    };
    let kind = match given(&fields, "kind") {
        Ok(None) => "fact",
        Ok(Some(kind)) if KINDS.contains(&kind) => kind,
        Ok(Some(_)) | Err(_) => {
            return Line::Invalid(format!("unknown kind {:?}", shown(&fields["kind"])))
        }
    };
    let mut notes = Map::new();
    let date = match given(&fields, "date") {
        Ok(Some(date)) if is_valid_date(date) => Some(date.to_string()),
        Ok(None) => None,
        Ok(Some(_)) | Err(_) => {
            notes.insert("dateIgnored".into(), shown(&fields["date"]).into());
            None
        }
    };
    let source = given(&fields, "source").ok().flatten().map(String::from);
    let scope = match given(&fields, "scope") {
        Ok(Some(scope)) if SCOPES.contains(&scope) => scope,
        Ok(None) => "project",
        Ok(Some(_)) | Err(_) => {
            notes.insert("scopeIgnored".into(), shown(&fields["scope"]).into());
            "project"
        }
    };
    Line::Memory(Entry {
        text: text.to_string(),
        kind: kind.to_string(),
        date,
        source,
        scope: scope.to_string(),
        notes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const EXPORT: &str = r#"Here is everything I remember:
```json
{"text": "Run the smoke checks before tagging.", "kind": "instruction", "date": "2026-09-01", "source": "CLAUDE.md", "scope": "project"}
{"text": "Prefers concise answers", "kind": "preference", "date": "", "scope": "global"}
{"text": " The staging gate sits before the deploy step. ", "date": "Sept 2026"}

not json at all
{"kind": "fact"}
{"text": "Docs live in docs/.", "kind": "opinion"}
{"text": "Release notes are kept in CHANGELOG.md.", "kind": "reference", "scope": "team", "source": 7}
{"continued": true}
```
{"text": "Run the smoke checks before tagging.", "kind": "fact"}
"#;

    fn scan(dir: &tempfile::TempDir, text: &str, label: Option<&str>) -> ScanOutput {
        let file = dir.path().join("export.jsonl");
        std::fs::write(&file, text).unwrap();
        let ctx = ScanContext {
            home: dir.path().to_path_buf(),
            from: Some(file),
            label: label.map(String::from),
            ..ScanContext::default()
        };
        Export.scan(&ctx).unwrap()
    }

    #[test]
    fn export_lines_become_segments_and_bad_lines_issues() {
        let dir = tempfile::tempdir().unwrap();
        let out = scan(&dir, EXPORT, None);
        let reasons: Vec<(&str, &str)> = out
            .issues
            .iter()
            .map(|i| (i.path.as_str(), i.reason.as_str()))
            .collect();
        assert_eq!(
            reasons,
            [
                ("~/export.jsonl", "invalid-line 1: not a JSON object"),
                ("~/export.jsonl", "invalid-line 7: not a JSON object"),
                ("~/export.jsonl", "invalid-line 8: no text"),
                ("~/export.jsonl", "invalid-line 9: unknown kind \"opinion\""),
            ]
        );
        let doc = &out.docs[0];
        assert_eq!(
            (doc.host.as_str(), doc.via, doc.native_id.as_str()),
            ("agent-export", "agent-export", "export")
        );
        assert_eq!(doc.stated_at, "");
        assert_eq!(doc.file_hash, content_hash(EXPORT));
        type Seen<'a> = (&'a str, Option<&'a str>, Option<&'a str>, Option<bool>);
        let got: Vec<Seen> = doc
            .segments
            .iter()
            .map(|s| {
                (
                    s.text.as_str(),
                    s.kind_hint.as_deref(),
                    s.stated_at.as_deref(),
                    s.instruction_candidate,
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                (
                    "Run the smoke checks before tagging.",
                    Some("instruction"),
                    Some("2026-09-01T00:00:00Z"),
                    Some(true)
                ),
                (
                    "Prefers concise answers",
                    Some("preference"),
                    None,
                    Some(false)
                ),
                (
                    "The staging gate sits before the deploy step.",
                    Some("fact"),
                    None,
                    Some(false)
                ),
                (
                    "Release notes are kept in CHANGELOG.md.",
                    Some("reference"),
                    None,
                    Some(false)
                ),
                (
                    "Run the smoke checks before tagging.",
                    Some("fact"),
                    None,
                    Some(false)
                ),
            ]
        );
        assert_eq!(
            doc.segments[0].extra,
            json!({"line": 3, "source": "CLAUDE.md", "scopeHint": "project"})
        );
        assert_eq!(doc.segments[1].extra["scopeHint"], "global");
        assert_eq!(doc.segments[2].extra["dateIgnored"], "Sept 2026");
        assert_eq!(
            doc.segments[3].extra,
            json!({"line": 10, "source": null, "scopeHint": "project", "scopeIgnored": "team"})
        );
        // Locators are the start of the text's hash; the repeated text is told apart.
        let hash = content_hash("Run the smoke checks before tagging.");
        assert_eq!(doc.segments[0].locator, hash[..16]);
        assert_eq!(doc.segments[4].locator, format!("{}~2", &hash[..16]));
        assert!(doc
            .segments
            .iter()
            .all(|s| s.policy == Policy::VerbatimIfFits));
    }

    #[test]
    fn locators_survive_a_reexport_in_another_order() {
        let dir = tempfile::tempdir().unwrap();
        let first = scan(
            &dir,
            "{\"text\": \"A\"}\n{\"text\": \"B\"}\n",
            Some("chatgpt"),
        );
        let second = scan(
            &dir,
            "{\"text\": \"B\", \"kind\": \"fact\"}\n\n{\"text\": \"A\"}\n",
            Some("chatgpt"),
        );
        let locators = |out: &ScanOutput| {
            let mut found: Vec<(String, String)> = out.docs[0]
                .segments
                .iter()
                .map(|s| (s.text.clone(), s.locator.clone()))
                .collect();
            found.sort();
            found
        };
        assert_eq!(locators(&first), locators(&second));
        assert_eq!(first.docs[0].host, "chatgpt");
        // An export without a memory line is reported, not planned.
        let empty = scan(&dir, "```\n{\"continued\": true}\n```\n", None);
        assert!(empty.docs.is_empty());
        assert_eq!(empty.issues[0].reason, "empty");
    }

    #[test]
    fn the_export_file_is_required_and_must_be_readable() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ScanContext {
            home: dir.path().to_path_buf(),
            ..ScanContext::default()
        };
        assert!(Export
            .scan(&ctx)
            .unwrap_err()
            .to_string()
            .contains("--from"));
        let missing = ScanContext {
            from: Some(dir.path().join("none.jsonl")),
            ..ctx.clone()
        };
        assert!(Export
            .scan(&missing)
            .unwrap_err()
            .to_string()
            .contains("~/none.jsonl"));
        std::fs::write(dir.path().join("binary.jsonl"), [0xff, 0xfe]).unwrap();
        let binary = ScanContext {
            from: Some(dir.path().join("binary.jsonl")),
            ..ctx
        };
        assert!(Export
            .scan(&binary)
            .unwrap_err()
            .to_string()
            .contains("unreadable"));
    }
}
