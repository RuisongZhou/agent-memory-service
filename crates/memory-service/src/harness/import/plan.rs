//! The plan (docs/design/memory-import.md §6.5, §6.12, §6.13): every segment the adapters found becomes
//! one import unit, redacted, with the mode the unit cap decides, in the order apply should write them.
//! Host-agnostic: host knowledge ends at a segment's policy. The plan is a private JSONL file that a
//! person may edit before proposing it: delete unit lines, or set a `decision` to `skip`.
use chrono::{DateTime, Utc};
use memory_domain::import::{
    session_turn_problem, ImportLimits, ImportUnit, SessionTurn, MAX_EXTRACT_BYTES, MODE_EXTRACT,
    MODE_SESSION_TURN, MODE_SPLIT, MODE_VERBATIM,
};
use memory_domain::types::InMessage;
use memory_rules::import::redact::redact;
use memory_rules::import::units::{
    chunk_bytes, content_hash, record_cost, split_to_fit, turn_key, unit_key, UnitLimits,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use super::session::{SessionDoc, Turn};
use super::source::{display, Issue, Policy, ScanOutput, Segment, SourceDoc};
use super::{shown, write_private};
use crate::harness::{invalid, Error};

pub const VERSION: u64 = 1;
pub const IMPORT: &str = "import";
pub const SKIP: &str = "skip";
/// Plan and review files, relative to the home directory.
pub const IMPORTS_DIR: &str = ".local/state/agent-memory-service/imports";
/// Characters of a text the daemon's embedding call keeps (the model API clips longer queries); an
/// extract unit's text is embedded once, so clipped, to find the memories its extraction is told of.
pub const EMBED_QUERY_CHARS: usize = 6000;

/// What a plan is for.
#[derive(Debug, Clone)]
pub struct PlanOptions {
    /// Platform label of the host.
    pub host: String,
    pub project: String,
    /// The home directory display paths are relative to.
    pub home: PathBuf,
    /// Repository roots of the `--workspace` directories.
    pub workspaces: Vec<PathBuf>,
    pub limits: ImportLimits,
    pub no_extract: bool,
    /// RFC3339 UTC.
    pub created_at: String,
    /// Which session turns to plan, when sessions are included.
    pub sessions: Option<SessionOptions>,
}

/// The session turns a plan takes (design §6.14): from `since` on, at most `limit`, oldest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionOptions {
    /// RFC3339 UTC.
    pub since: Option<String>,
    pub limit: usize,
}

/// What the plan did with the sessions it read; turns before `since`, over the limit and the copies of a turn
/// another session holds get no line.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionsSummary {
    pub sessions: usize,
    pub since: Option<String>,
    pub limit: usize,
    pub before_since: usize,
    pub over_limit: usize,
    /// Turns a forked or resumed session repeats from the session it continues.
    pub copies: usize,
    /// The `--since` that plans the turns over the limit.
    pub next_since: Option<String>,
}

/// The recall parameters and the unit sizes they give (design §6.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanLimits {
    #[serde(flatten)]
    pub wire: ImportLimits,
    pub hard_cap: usize,
    pub target: usize,
}

impl From<ImportLimits> for PlanLimits {
    fn from(wire: ImportLimits) -> Self {
        let limits = UnitLimits::from(wire);
        PlanLimits {
            wire,
            hard_cap: limits.hard_cap(),
            target: limits.target(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counts {
    pub docs: usize,
    pub units: usize,
    pub verbatim: usize,
    pub split: usize,
    pub extract: usize,
    pub skipped: usize,
    #[serde(default, rename = "sessionTurns")]
    pub session_turns: usize,
}

/// What proposing and applying the plan send to the model services. Propose embeds each verbatim text
/// and split piece once to find similar memories, and apply embeds it again when it is written: two
/// embeddings each. An extract unit's text is embedded once for its extraction context, clipped to
/// `EMBED_QUERY_CHARS`. Each extracted item is embedded twice too, but their number is unknown before the
/// generation call.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Estimates {
    pub generation_calls: usize,
    pub embedding_texts: usize,
    pub chars_to_generation: usize,
    pub chars_to_embedding: usize,
}

/// The first line of a plan file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    #[serde(rename = "type")]
    pub line: String,
    pub version: u64,
    pub host: String,
    pub project: String,
    /// Display form.
    pub workspaces: Vec<String>,
    pub created_at: String,
    pub limits: PlanLimits,
    pub no_extract: bool,
    pub counts: Counts,
    pub skip_reasons: BTreeMap<String, usize>,
    pub redactions: BTreeMap<String, usize>,
    pub estimates: Estimates,
    pub issues: Vec<Issue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sessions: Option<SessionsSummary>,
    /// blake3 of the unit lines as written.
    pub plan_hash: String,
}

/// One unit line of a plan file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedUnit {
    #[serde(rename = "type")]
    pub line: String,
    /// `import` or `skip`.
    pub decision: String,
    /// Why this mode (`fits`, `over-cap`, `always-extract`, `session-turn`) or why skipped.
    pub reason: String,
    /// Record cost of the unit text.
    pub cost: usize,
    /// Replacements per kind.
    #[serde(default)]
    pub redactions: BTreeMap<String, usize>,
    #[serde(flatten)]
    pub unit: ImportUnit,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub summary: Summary,
    pub units: Vec<PlannedUnit>,
}

impl Plan {
    /// The units to propose.
    pub fn import_units(&self) -> impl Iterator<Item = &ImportUnit> {
        self.units
            .iter()
            .filter(|u| u.decision == IMPORT)
            .map(|u| &u.unit)
    }
}

/// The mode of a unit and the reason for it: a text that fits the cap stays one record unless its source
/// needs extraction; `--no-extract` splits what does not fit instead of extracting it.
pub fn decide(policy: Policy, fits: bool, no_extract: bool) -> (&'static str, &'static str) {
    match (policy, fits, no_extract) {
        (Policy::VerbatimIfFits, true, _) | (Policy::AlwaysExtract, true, true) => {
            (MODE_VERBATIM, "fits")
        }
        (_, false, true) => (MODE_SPLIT, "over-cap"),
        (Policy::VerbatimIfFits, false, false) => (MODE_EXTRACT, "over-cap"),
        (Policy::AlwaysExtract, _, false) => (MODE_EXTRACT, "always-extract"),
    }
}

/// Plan every segment of `scan`, oldest statement first so apply writes older statements first, then the
/// session turns it selects, oldest first.
pub fn build(scan: ScanOutput, opts: &PlanOptions) -> Result<Plan, Error> {
    let limits = UnitLimits::from(opts.limits);
    let mut keyed = Vec::new();
    for doc in &scan.docs {
        for segment in &doc.segments {
            for (part, unit) in plan_units(doc, segment, opts, &limits)
                .into_iter()
                .enumerate()
            {
                let stated = unit.unit.stated_at.clone();
                let order = (
                    stated.is_empty(),
                    stated,
                    doc.display_path.clone(),
                    segment.locator.clone(),
                    part,
                );
                keyed.push((order, unit));
            }
        }
    }
    keyed.sort_by(|a, b| a.0.cmp(&b.0));
    let mut sessions = None;
    let turns = match &opts.sessions {
        Some(options) => {
            let mut summary = SessionsSummary {
                sessions: scan.sessions.len(),
                since: options.since.clone(),
                limit: options.limit,
                ..SessionsSummary::default()
            };
            let turns = session_units(&scan.sessions, opts, options, &mut summary);
            sessions = Some(summary);
            turns
        }
        None => vec![],
    };
    let mut seen = HashSet::new();
    let mut units = Vec::with_capacity(keyed.len() + turns.len());
    for mut unit in keyed.into_iter().map(|(_, unit)| unit).chain(turns) {
        if !seen.insert(unit.unit.unit.clone()) && unit.decision == IMPORT {
            unit.decision = SKIP.into();
            unit.reason = "duplicate-unit".into();
        }
        units.push(unit);
    }
    let mut counts = Counts {
        docs: scan.docs.len(),
        units: units.len(),
        ..Counts::default()
    };
    let mut estimates = Estimates::default();
    let mut skip_reasons = BTreeMap::new();
    let mut redactions = BTreeMap::new();
    for planned in &units {
        for (kind, n) in &planned.redactions {
            *redactions.entry(kind.clone()).or_insert(0) += n;
        }
        if planned.decision == SKIP {
            counts.skipped += 1;
            *skip_reasons.entry(planned.reason.clone()).or_insert(0) += 1;
            continue;
        }
        let unit = &planned.unit;
        match unit.mode.as_str() {
            MODE_SESSION_TURN => {
                // Mem0 formation: one generation call per turn; the facts it writes are embedded then.
                counts.session_turns += 1;
                estimates.generation_calls += 1;
                let turn = unit
                    .turn
                    .as_ref()
                    .expect("a session-turn unit carries its turn");
                estimates.chars_to_generation += turn
                    .messages
                    .iter()
                    .map(|m| m.content.chars().count())
                    .sum::<usize>();
            }
            MODE_VERBATIM => {
                counts.verbatim += 1;
                estimates.embedding_texts += 2;
                estimates.chars_to_embedding += 2 * unit.text.chars().count();
            }
            MODE_SPLIT => {
                counts.split += 1;
                estimates.embedding_texts += 2 * unit.pieces.len();
                estimates.chars_to_embedding +=
                    2 * unit.pieces.iter().map(|p| p.chars().count()).sum::<usize>();
            }
            _ => {
                let chars = unit.text.chars().count();
                counts.extract += 1;
                estimates.generation_calls += 1;
                estimates.chars_to_generation += chars;
                estimates.embedding_texts += 1;
                estimates.chars_to_embedding += chars.min(EMBED_QUERY_CHARS);
            }
        }
    }
    let summary = Summary {
        line: "summary".into(),
        version: VERSION,
        host: opts.host.clone(),
        project: opts.project.clone(),
        workspaces: opts
            .workspaces
            .iter()
            .map(|w| display(&opts.home, w))
            .collect(),
        created_at: opts.created_at.clone(),
        limits: PlanLimits::from(opts.limits),
        no_extract: opts.no_extract,
        counts,
        skip_reasons,
        redactions,
        estimates,
        issues: scan.issues,
        sessions,
        plan_hash: content_hash(&unit_lines(&units)?),
    };
    Ok(Plan { summary, units })
}

/// Redacts every text of a unit that leaves the machine, counting what it replaced.
#[derive(Default)]
struct Redactor {
    counts: BTreeMap<String, usize>,
    private_key: bool,
}

impl Redactor {
    fn clean(&mut self, text: &str) -> String {
        let redaction = redact(text);
        self.private_key |= redaction.private_key;
        for (kind, n) in redaction.counts {
            *self.counts.entry(kind.to_string()).or_insert(0) += n;
        }
        redaction.text
    }
}

/// A segment's units: one, or for an extraction input longer than `MAX_EXTRACT_BYTES` one per part
/// (`chunk_bytes`, locator `<locator>~part<n>`), since one extraction call takes at most that much text.
/// The redaction counts stay on the first part, so the summary counts each replacement once.
fn plan_units(
    doc: &SourceDoc,
    segment: &Segment,
    opts: &PlanOptions,
    limits: &UnitLimits,
) -> Vec<PlannedUnit> {
    let unit = plan_unit(doc, segment, opts, limits);
    if unit.decision != IMPORT
        || unit.unit.mode != MODE_EXTRACT
        || unit.unit.text.len() <= MAX_EXTRACT_BYTES
    {
        return vec![unit];
    }
    chunk_bytes(&unit.unit.text, MAX_EXTRACT_BYTES)
        .into_iter()
        .enumerate()
        .map(|(i, text)| {
            let mut part = unit.clone();
            let locator = format!("{}~part{}", segment.locator, i + 1);
            part.unit.unit = unit_key(&doc.host, &doc.native_id, &locator);
            part.unit.unit_hash = content_hash(&text);
            part.cost = record_cost(&text);
            part.unit.text = text;
            if i > 0 {
                part.redactions.clear();
            }
            part
        })
        .collect()
}

fn plan_unit(
    doc: &SourceDoc,
    segment: &Segment,
    opts: &PlanOptions,
    limits: &UnitLimits,
) -> PlannedUnit {
    // Headings travel with the text (as the context of split pieces and of extraction), so they are redacted too.
    let mut redactor = Redactor::default();
    let text = redactor.clean(&segment.text).trim().to_string();
    let heading_path: Vec<String> = segment
        .heading_path
        .iter()
        .map(|heading| redactor.clean(heading))
        .collect();
    let source = source(doc, segment, &opts.home, &mut redactor);
    let (mode, why) = decide(segment.policy, limits.fits(&text), opts.no_extract);
    let mut pieces = Vec::new();
    let (decision, reason) = if redactor.private_key {
        (SKIP, "private-key")
    } else if text.is_empty() {
        (SKIP, "empty")
    } else if mode == MODE_SPLIT {
        let context = heading_path.last().map_or("", String::as_str);
        pieces = split_to_fit(&text, context, limits);
        match pieces.is_empty() {
            true => (SKIP, "unsplittable"),
            false => (IMPORT, why),
        }
    } else {
        (IMPORT, why)
    };
    PlannedUnit {
        line: "unit".into(),
        decision: decision.into(),
        reason: reason.into(),
        cost: record_cost(&text),
        redactions: redactor.counts,
        unit: ImportUnit {
            unit: unit_key(&doc.host, &doc.native_id, &segment.locator),
            host: doc.host.clone(),
            mode: mode.into(),
            unit_hash: content_hash(&text),
            text,
            pieces,
            stated_at: segment
                .stated_at
                .clone()
                .unwrap_or_else(|| doc.stated_at.clone()),
            kind_hint: segment.kind_hint.clone().or_else(|| doc.kind_hint.clone()),
            heading_path,
            file_hash: doc.file_hash.clone(),
            source,
            turn: None,
        },
    }
}

/// Provenance for the record's `detail.import`: where the unit came from, then the document's and the
/// segment's own fields, their strings redacted; null fields are left out. The path and native id stay as
/// they are: propose reads the file there again, and the id is part of the unit key.
fn source(doc: &SourceDoc, segment: &Segment, home: &Path, redactor: &mut Redactor) -> Value {
    let mut map = Map::new();
    map.insert("via".into(), json!(doc.via));
    map.insert("path".into(), json!(doc.display_path));
    map.insert("nativeId".into(), json!(doc.native_id));
    if let Some(title) = &doc.title {
        map.insert("title".into(), json!(redactor.clean(title)));
    }
    if let Some(cwd) = &doc.cwd {
        map.insert("cwd".into(), json!(display(home, cwd)));
    }
    map.insert("userLevel".into(), json!(doc.user_level));
    for extra in [&doc.extra, &segment.extra] {
        if let Value::Object(fields) = extra {
            for (key, value) in fields.iter().filter(|(_, v)| !v.is_null()) {
                let value = match value {
                    Value::String(text) => Value::String(redactor.clean(text)),
                    other => other.clone(),
                };
                map.insert(key.clone(), value);
            }
        }
    }
    Value::Object(map)
}

/// The units of the session turns (design §6.10, §6.14): every turn a person asked, oldest first from
/// `--since`, at most `--limit`, with lines for the turns of that window the adapters skipped. A turn found in
/// two sessions (a forked or resumed session repeats the turns it continues) belongs to the session that
/// started first, as capture filed it there.
fn session_units(
    sessions: &[SessionDoc],
    opts: &PlanOptions,
    options: &SessionOptions,
    summary: &mut SessionsSummary,
) -> Vec<PlannedUnit> {
    let mut docs: Vec<&SessionDoc> = sessions.iter().collect();
    docs.sort_by(|a, b| {
        (&a.started_at, &a.session, &a.display_path).cmp(&(
            &b.started_at,
            &b.session,
            &b.display_path,
        ))
    });
    type Key = (String, String, String, usize);
    let mut seen = HashSet::new();
    let (mut eligible, mut skipped): (Vec<(Key, &SessionDoc, &Turn)>, Vec<_>) = (vec![], vec![]);
    for doc in docs {
        for (order, turn) in doc.turns.iter().enumerate() {
            if !turn.native_id.is_empty() && !seen.insert(turn.native_id.as_str()) {
                summary.copies += 1;
                continue;
            }
            let key = (
                turn.prompt_at.clone(),
                doc.started_at.clone(),
                doc.session.clone(),
                order,
            );
            match turn.skip {
                None => eligible.push((key, doc, turn)),
                Some(_) => skipped.push((key, doc, turn)),
            }
        }
    }
    let since = options.since.as_deref().unwrap_or("");
    let before = eligible.len();
    eligible.retain(|(key, ..)| key.0.as_str() >= since);
    summary.before_since = before - eligible.len();
    eligible.sort_by(|a, b| a.0.cmp(&b.0));
    if eligible.len() > options.limit {
        summary.over_limit = eligible.len() - options.limit;
        summary.next_since = Some(eligible[options.limit].0 .0.clone());
        eligible.truncate(options.limit);
    }
    let until = summary.next_since.clone();
    skipped.retain(|(key, ..)| {
        key.0.as_str() >= since && until.as_ref().is_none_or(|until| key.0 < *until)
    });
    let mut all: Vec<_> = eligible.into_iter().chain(skipped).collect();
    all.sort_by(|a, b| a.0.cmp(&b.0));
    all.into_iter()
        .map(|(key, doc, turn)| turn_unit(doc, turn, key.3, opts))
        .collect()
}

/// One session turn as a unit: its prompt is the unit text, and the turn goes with it as capture would queue it,
/// both messages redacted. Skipped when the adapter said so, or when it holds a private key.
fn turn_unit(doc: &SessionDoc, turn: &Turn, order: usize, opts: &PlanOptions) -> PlannedUnit {
    let mut redactor = Redactor::default();
    let prompt = redactor.clean(&turn.prompt).trim().to_string();
    let reply = redactor.clean(&turn.reply).trim().to_string();
    let locator = match turn.native_id.is_empty() {
        true => format!("turn-{}", order + 1),
        false => turn.native_id.clone(),
    };
    let (decision, reason) = match &turn.skip {
        Some(reason) => (SKIP, reason.clone()),
        None if redactor.private_key => (SKIP, "private-key".into()),
        None if prompt.is_empty() || reply.is_empty() => (SKIP, "empty".into()),
        None => (IMPORT, MODE_SESSION_TURN.into()),
    };
    let message = |role: &str, content: &str, timestamp: &str| InMessage {
        role: role.into(),
        content: content.into(),
        timestamp: timestamp.into(),
    };
    // Formation takes a batch's messages in time order; a reply never predates its prompt.
    let reply_at = turn.reply_at.as_str().max(turn.prompt_at.as_str());
    let mut source = Map::new();
    source.insert("via".into(), json!(doc.via));
    source.insert("path".into(), json!(doc.display_path));
    source.insert("nativeId".into(), json!(turn.native_id));
    source.insert("session".into(), json!(doc.session));
    source.insert("cwd".into(), json!(display(&opts.home, &doc.cwd)));
    if let Value::Object(fields) = &doc.extra {
        for (key, value) in fields.iter().filter(|(_, v)| !v.is_null()) {
            source.insert(key.clone(), value.clone());
        }
    }
    PlannedUnit {
        line: "unit".into(),
        decision: decision.into(),
        reason,
        cost: record_cost(&prompt),
        redactions: redactor.counts,
        unit: ImportUnit {
            unit: unit_key(&doc.host, &format!("session:{}", doc.session), &locator),
            host: doc.host.clone(),
            mode: MODE_SESSION_TURN.into(),
            unit_hash: content_hash(&prompt),
            stated_at: turn.prompt_at.clone(),
            file_hash: doc.file_hash.clone(),
            source: Value::Object(source),
            turn: Some(SessionTurn {
                turn_id: turn_key(&opts.project, &doc.session, &turn.native_id),
                session: doc.session.clone(),
                native_id: turn.native_id.clone(),
                messages: vec![
                    message("user", &prompt, &turn.prompt_at),
                    message("assistant", &reply, reply_at),
                ],
            }),
            text: prompt,
            ..ImportUnit::default()
        },
    }
}

fn unit_lines(units: &[PlannedUnit]) -> Result<String, Error> {
    let mut out = String::new();
    for unit in units {
        out.push_str(&serde_json::to_string(unit).map_err(|_| Error::Json)?);
        out.push('\n');
    }
    Ok(out)
}

/// The plan file: the summary line, then one line per unit, skipped units included.
pub fn render(plan: &Plan) -> Result<String, Error> {
    let mut out = serde_json::to_string(&plan.summary).map_err(|_| Error::Json)?;
    out.push('\n');
    out.push_str(&unit_lines(&plan.units)?);
    Ok(out)
}

/// Write the plan owner-only (directories 0700, file 0600), replacing an earlier file at `path`.
pub fn write(plan: &Plan, path: &Path) -> Result<(), Error> {
    write_private(path, &render(plan)?)
}

/// `<home>/.local/state/agent-memory-service/imports/<host>-<project>-<UTC stamp>.plan.jsonl`, numbered
/// when a plan of the same second exists.
pub fn default_path(home: &Path, host: &str, project: &str, now: DateTime<Utc>) -> PathBuf {
    let dir = home.join(IMPORTS_DIR);
    let base = format!(
        "{}-{}-{}",
        file_part(host),
        file_part(project),
        now.format("%Y%m%dT%H%M%SZ")
    );
    let mut path = dir.join(format!("{base}.plan.jsonl"));
    for n in 2.. {
        if !path.exists() {
            break;
        }
        path = dir.join(format!("{base}-{n}.plan.jsonl"));
    }
    path
}

/// A free-form name as part of a file name: ASCII letters, digits, `.`, `_` and `-` stay, every other
/// character becomes `_`; at most 64 characters.
pub fn file_part(name: &str) -> String {
    let part: String = name
        .chars()
        .take(64)
        .map(|c| match c.is_ascii_alphanumeric() || "._-".contains(c) {
            true => c,
            false => '_',
        })
        .collect();
    match part.chars().all(|c| c == '.') {
        true => "_".into(),
        false => part,
    }
}

pub fn read(path: &Path) -> Result<Plan, Error> {
    let text =
        std::fs::read_to_string(path).map_err(|e| invalid(format!("{}: {e}", shown(path))))?;
    parse(&text)
}

/// A plan file, after any hand edits: unit lines may be gone and decisions set to `skip`, but every line
/// must still be a unit of this format; errors name the line.
pub fn parse(text: &str) -> Result<Plan, Error> {
    let mut lines = text
        .lines()
        .enumerate()
        .map(|(i, line)| (i + 1, line))
        .filter(|(_, line)| !line.trim().is_empty());
    let Some((n, first)) = lines.next() else {
        return Err(invalid("the plan file is empty"));
    };
    let head: Value = serde_json::from_str(first).map_err(|e| invalid(format!("line {n}: {e}")))?;
    if head.get("type").and_then(Value::as_str) != Some("summary") {
        return Err(invalid(format!(
            "line {n}: a plan starts with its summary line; make one with `agent-memoryd import plan`"
        )));
    }
    if head.get("version").and_then(Value::as_u64) != Some(VERSION) {
        return Err(invalid(format!(
            "line {n}: plan version {} is not supported (expected {VERSION}); make a new plan",
            head.get("version").unwrap_or(&Value::Null)
        )));
    }
    let summary: Summary =
        serde_json::from_value(head).map_err(|e| invalid(format!("line {n}: {e}")))?;
    let mut units = Vec::new();
    let mut proposed = HashSet::new();
    for (n, line) in lines {
        let unit: PlannedUnit =
            serde_json::from_str(line).map_err(|e| invalid(format!("line {n}: {e}")))?;
        if unit.line != "unit" {
            return Err(invalid(format!(
                "line {n}: expected a unit line, not type {:?}",
                unit.line
            )));
        }
        if unit.decision != IMPORT && unit.decision != SKIP {
            return Err(invalid(format!(
                "line {n}: decision must be \"import\" or \"skip\", not {:?}",
                unit.decision
            )));
        }
        if ![MODE_VERBATIM, MODE_SPLIT, MODE_EXTRACT, MODE_SESSION_TURN]
            .contains(&unit.unit.mode.as_str())
        {
            return Err(invalid(format!(
                "line {n}: unknown mode {:?}",
                unit.unit.mode
            )));
        }
        if unit.decision == IMPORT {
            if unit.unit.mode == MODE_SPLIT && unit.unit.pieces.is_empty() {
                return Err(invalid(format!("line {n}: a split unit needs its pieces")));
            }
            if unit.unit.mode == MODE_SESSION_TURN {
                let problem = match &unit.unit.turn {
                    Some(turn) => session_turn_problem(turn, &summary.project),
                    None => Some("a session turn needs its turn".into()),
                };
                if let Some(problem) = problem {
                    return Err(invalid(format!("line {n}: {problem}")));
                }
            }
            if !proposed.insert(unit.unit.unit.clone()) {
                return Err(invalid(format!(
                    "line {n}: unit {} is imported twice",
                    unit.unit.unit
                )));
            }
        }
        units.push(unit);
    }
    Ok(Plan { summary, units })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(path: &str, stated_at: &str, segments: Vec<Segment>) -> SourceDoc {
        SourceDoc {
            host: "codex".into(),
            via: "codex-rollout-summary",
            path: PathBuf::from(format!("/home/u/{path}")),
            display_path: format!("~/{path}"),
            native_id: format!("id-{path}"),
            title: Some("Title".into()),
            cwd: Some(PathBuf::from("/home/u/code/app")),
            user_level: false,
            file_hash: content_hash(path),
            stated_at: stated_at.into(),
            kind_hint: Some("project".into()),
            segments,
            extra: json!({"threadId": "t-1", "memoriesHead": null}),
        }
    }

    fn segment(locator: &str, text: &str, policy: Policy) -> Segment {
        Segment::new(
            locator.into(),
            vec!["Title".into(), "Section".into()],
            text.into(),
            policy,
        )
    }

    fn options(no_extract: bool) -> PlanOptions {
        PlanOptions {
            host: "codex".into(),
            project: "my-project".into(),
            home: PathBuf::from("/home/u"),
            workspaces: vec![PathBuf::from("/home/u/code/app")],
            limits: ImportLimits::default(),
            no_extract,
            created_at: "2026-10-22T12:00:00Z".into(),
            sessions: None,
        }
    }

    fn scan(docs: Vec<SourceDoc>) -> ScanOutput {
        ScanOutput {
            docs,
            issues: vec![Issue {
                path: "~/MEMORY.md".into(),
                reason: "derived".into(),
            }],
            sessions: vec![],
        }
    }

    /// A long English section well over the default cap (cost 200): about 330 tokens.
    fn long_text() -> String {
        (0..30)
            .map(|i| format!("- Step {i} runs the release checks before tagging the build."))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn long_extraction_inputs_become_parts_in_order() {
        // About 20 KB of paragraphs: one extraction call takes at most MAX_EXTRACT_BYTES.
        let text = (0..40)
            .map(|i| {
                format!(
                    "Paragraph {i}: {}",
                    "the release checks run before tagging. ".repeat(12)
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        let plan = build(
            scan(vec![doc(
                "a.md",
                "2026-01-01T00:00:00Z",
                vec![segment("task-1", &text, Policy::AlwaysExtract)],
            )]),
            &options(false),
        )
        .unwrap();
        let parts: Vec<&PlannedUnit> = plan.units.iter().collect();
        assert!(parts.len() >= 3, "{}", parts.len());
        for (i, part) in parts.iter().enumerate() {
            assert_eq!(part.decision, IMPORT);
            assert_eq!(part.unit.mode, MODE_EXTRACT);
            assert!(part.unit.text.len() <= MAX_EXTRACT_BYTES);
            assert_eq!(part.unit.unit_hash, content_hash(&part.unit.text));
            assert!(
                part.unit.unit.ends_with(&format!("~part{}", i + 1)),
                "{}",
                part.unit.unit
            );
        }
        assert!(parts[0].unit.text.starts_with("Paragraph 0:"));
        assert!(parts.last().unwrap().unit.text.contains("Paragraph 39:"));
        assert_eq!(plan.summary.counts.extract, parts.len());
        let e = &plan.summary.estimates;
        assert_eq!(e.generation_calls, parts.len());
        // Each part is embedded once for its extraction context, clipped as the daemon clips a query.
        let chars: Vec<usize> = parts.iter().map(|p| p.unit.text.chars().count()).collect();
        assert!(chars.iter().any(|n| *n > EMBED_QUERY_CHARS), "{chars:?}");
        assert_eq!(e.chars_to_generation, chars.iter().sum::<usize>());
        assert_eq!(e.embedding_texts, parts.len());
        assert_eq!(
            e.chars_to_embedding,
            chars
                .iter()
                .map(|n| (*n).min(EMBED_QUERY_CHARS))
                .sum::<usize>()
        );
    }

    #[test]
    fn modes_follow_the_policy_the_cap_and_no_extract() {
        use Policy::*;
        let table = [
            ((VerbatimIfFits, true, false), (MODE_VERBATIM, "fits")),
            ((VerbatimIfFits, false, false), (MODE_EXTRACT, "over-cap")),
            ((VerbatimIfFits, false, true), (MODE_SPLIT, "over-cap")),
            (
                (AlwaysExtract, true, false),
                (MODE_EXTRACT, "always-extract"),
            ),
            ((AlwaysExtract, true, true), (MODE_VERBATIM, "fits")),
        ];
        for ((policy, fits, no_extract), expected) in table {
            assert_eq!(
                decide(policy, fits, no_extract),
                expected,
                "{policy:?} {fits} {no_extract}"
            );
        }
    }

    #[test]
    fn units_come_oldest_first_then_by_path_and_locator_with_stable_keys() {
        let docs = vec![
            doc(
                "b.md",
                "2026-02-01T00:00:00Z",
                vec![
                    segment("task-2", "Second task text.", Policy::VerbatimIfFits),
                    segment("intro", "Intro text.", Policy::VerbatimIfFits),
                ],
            ),
            doc(
                "c.md",
                "",
                vec![segment("note", "Undated note.", Policy::VerbatimIfFits)],
            ),
            doc(
                "a.md",
                "2026-02-01T00:00:00Z",
                vec![segment(
                    "note",
                    "Same time, earlier path.",
                    Policy::VerbatimIfFits,
                )],
            ),
            doc(
                "z.md",
                "2026-01-01T00:00:00Z",
                vec![segment("note", "Oldest statement.", Policy::VerbatimIfFits)],
            ),
        ];
        let plan = build(scan(docs.clone()), &options(false)).unwrap();
        let order: Vec<(&str, &str)> = plan
            .units
            .iter()
            .map(|u| {
                (
                    u.unit.source["path"].as_str().unwrap(),
                    u.unit.text.as_str(),
                )
            })
            .collect();
        assert_eq!(
            order,
            [
                ("~/z.md", "Oldest statement."),
                ("~/a.md", "Same time, earlier path."),
                ("~/b.md", "Intro text."),
                ("~/b.md", "Second task text."),
                ("~/c.md", "Undated note."),
            ]
        );
        assert_eq!(plan.units[2].unit.unit, "codex:id-b.md#intro");
        assert_eq!(
            plan.units[2].unit.unit,
            unit_key("codex", "id-b.md", "intro")
        );
        // The same input plans the same units, byte for byte.
        let again = build(scan(docs), &options(false)).unwrap();
        assert_eq!(render(&plan).unwrap(), render(&again).unwrap());
        assert_eq!(plan.summary.plan_hash, again.summary.plan_hash);
    }

    #[test]
    fn segment_overrides_and_provenance_reach_the_unit() {
        let mut line = segment("0123abcd", "Use tabs in Go files.", Policy::VerbatimIfFits);
        line.stated_at = Some("2026-03-04T00:00:00Z".into());
        line.kind_hint = Some("reference".into());
        line.extra = json!({"line": 7, "source": null});
        let plan = build(
            scan(vec![doc("x.md", "2026-01-01T00:00:00Z", vec![line])]),
            &options(false),
        )
        .unwrap();
        let unit = &plan.units[0].unit;
        assert_eq!(unit.stated_at, "2026-03-04T00:00:00Z");
        assert_eq!(unit.kind_hint.as_deref(), Some("reference"));
        assert_eq!(unit.unit_hash, content_hash("Use tabs in Go files."));
        assert_eq!(unit.file_hash, content_hash("x.md"));
        assert_eq!(
            unit.source,
            json!({"via": "codex-rollout-summary", "path": "~/x.md", "nativeId": "id-x.md",
                   "title": "Title", "cwd": "~/code/app", "userLevel": false,
                   "threadId": "t-1", "line": 7})
        );
    }

    #[test]
    fn private_keys_skip_the_unit_and_redactions_are_counted() {
        let pem = |edge: &str| ["-----", edge, " RSA PRIVATE KEY-----"].concat();
        let key_note = format!(
            "Server notes.\n{}\nMIIfakeBody\n{}",
            pem("BEGIN"),
            pem("END")
        );
        let token: String = "Fake0Key9".chars().cycle().take(30).collect();
        let api_note = format!("Deploy with sk-{token} on staging.");
        let docs = vec![doc(
            "n.md",
            "2026-01-01T00:00:00Z",
            vec![
                segment("a", &key_note, Policy::VerbatimIfFits),
                segment("b", &api_note, Policy::VerbatimIfFits),
                segment("c", " \n ", Policy::VerbatimIfFits),
            ],
        )];
        let plan = build(scan(docs), &options(false)).unwrap();
        let (a, b, c) = (&plan.units[0], &plan.units[1], &plan.units[2]);
        assert_eq!(
            (a.decision.as_str(), a.reason.as_str()),
            (SKIP, "private-key")
        );
        assert!(!a.unit.text.contains("MIIfakeBody"));
        assert_eq!(
            a.redactions,
            BTreeMap::from([("private-key".to_string(), 1)])
        );
        assert_eq!(
            (b.decision.as_str(), b.unit.mode.as_str()),
            (IMPORT, MODE_VERBATIM)
        );
        assert_eq!(b.unit.text, "Deploy with [redacted:api-key] on staging.");
        assert_eq!(b.unit.unit_hash, content_hash(&b.unit.text));
        assert_eq!((c.decision.as_str(), c.reason.as_str()), (SKIP, "empty"));
        let summary = &plan.summary;
        assert_eq!(
            summary.redactions,
            BTreeMap::from([("api-key".to_string(), 1), ("private-key".to_string(), 1)])
        );
        assert_eq!(
            summary.skip_reasons,
            BTreeMap::from([("empty".to_string(), 1), ("private-key".to_string(), 1)])
        );
        assert!(!render(&plan).unwrap().contains(&token));
    }

    #[test]
    fn headings_and_provenance_are_redacted_with_the_text() {
        let token: String = "Fake0Key9".chars().cycle().take(36).collect();
        let heading = format!("Rotate ghp_{token}");
        let mut seg = Segment::new(
            "rotate".into(),
            vec!["Title".into(), heading.clone()],
            long_text(),
            Policy::VerbatimIfFits,
        );
        seg.extra = json!({"source": format!("notes with ghp_{token}"), "line": 3});
        let plan = build(scan(vec![doc("h.md", "", vec![seg])]), &options(true)).unwrap();
        let unit = &plan.units[0];
        assert_eq!(unit.unit.mode, MODE_SPLIT);
        assert_eq!(
            unit.unit.heading_path,
            ["Title", "Rotate [redacted:api-key]"]
        );
        assert!(unit
            .unit
            .pieces
            .iter()
            .all(|p| p.starts_with("Rotate [redacted:api-key]: ")));
        assert_eq!(unit.unit.source["source"], "notes with [redacted:api-key]");
        assert_eq!(
            unit.redactions,
            BTreeMap::from([("api-key".to_string(), 2)])
        );
        assert!(!render(&plan).unwrap().contains(&token));
    }

    #[test]
    fn summary_counts_and_estimates_follow_the_modes() {
        let long = long_text();
        let docs = vec![doc(
            "s.md",
            "2026-01-01T00:00:00Z",
            vec![
                segment("fits", "A short note that fits.", Policy::VerbatimIfFits),
                segment("long", &long, Policy::VerbatimIfFits),
                segment(
                    "task",
                    "Task progress mixed with facts.",
                    Policy::AlwaysExtract,
                ),
            ],
        )];
        let plan = build(scan(docs.clone()), &options(false)).unwrap();
        let s = &plan.summary;
        assert_eq!(
            s.counts,
            Counts {
                docs: 1,
                units: 3,
                verbatim: 1,
                split: 0,
                extract: 2,
                skipped: 0,
                session_turns: 0
            }
        );
        let long_chars = long.chars().count();
        let task_chars = "Task progress mixed with facts.".len();
        // The verbatim text is embedded twice (propose and apply), each extract unit once.
        assert_eq!(
            s.estimates,
            Estimates {
                generation_calls: 2,
                embedding_texts: 2 + 2,
                chars_to_generation: long_chars + task_chars,
                chars_to_embedding: 2 * "A short note that fits.".len() + long_chars + task_chars,
            }
        );
        assert_eq!(
            s.limits,
            PlanLimits {
                wire: ImportLimits::default(),
                hard_cap: 200,
                target: 80
            }
        );
        assert_eq!(s.issues.len(), 1);
        // Without the model: the long section splits into pieces that each fit, headed by its section.
        let plan = build(scan(docs), &options(true)).unwrap();
        let split = plan
            .units
            .iter()
            .find(|u| u.unit.mode == MODE_SPLIT)
            .unwrap();
        assert_eq!(split.reason, "over-cap");
        assert!(split.unit.pieces.len() >= 2);
        let limits = UnitLimits::default();
        assert!(split
            .unit
            .pieces
            .iter()
            .all(|p| limits.fits(p) && p.starts_with("Section: ")));
        let s = &plan.summary;
        assert_eq!(
            (s.counts.verbatim, s.counts.split, s.counts.extract),
            (2, 1, 0)
        );
        assert_eq!(s.estimates.generation_calls, 0);
        let pieces = &split.unit.pieces;
        assert_eq!(s.estimates.embedding_texts, 2 * (2 + pieces.len()));
        let piece_chars: usize = pieces.iter().map(|p| p.chars().count()).sum();
        let verbatim_chars =
            "A short note that fits.".len() + "Task progress mixed with facts.".len();
        assert_eq!(
            s.estimates.chars_to_embedding,
            2 * (verbatim_chars + piece_chars)
        );
        assert!(s.no_extract);
    }

    #[test]
    fn duplicate_unit_keys_import_once() {
        let docs = vec![
            doc(
                "d.md",
                "2026-01-01T00:00:00Z",
                vec![segment("note", "First.", Policy::VerbatimIfFits)],
            ),
            doc(
                "d.md",
                "2026-01-02T00:00:00Z",
                vec![segment("note", "Second.", Policy::VerbatimIfFits)],
            ),
        ];
        let plan = build(scan(docs), &options(false)).unwrap();
        assert_eq!(plan.units[0].decision, IMPORT);
        assert_eq!(
            (
                plan.units[1].decision.as_str(),
                plan.units[1].reason.as_str()
            ),
            (SKIP, "duplicate-unit")
        );
        // The plan with its skipped duplicate reads back.
        assert_eq!(parse(&render(&plan).unwrap()).unwrap(), plan);
    }

    #[test]
    fn plan_files_round_trip_and_accept_hand_edits() {
        let dir = tempfile::tempdir().unwrap();
        let docs = vec![doc(
            "r.md",
            "2026-01-01T00:00:00Z",
            vec![
                segment("a", "First unit.", Policy::VerbatimIfFits),
                segment("b", "Second unit.", Policy::VerbatimIfFits),
                segment("c", &long_text(), Policy::VerbatimIfFits),
            ],
        )];
        let plan = build(scan(docs), &options(true)).unwrap();
        let path = dir.path().join("state/imports/p.plan.jsonl");
        write(&plan, &path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        assert_eq!(read(&path).unwrap(), plan);
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            content_hash(&(lines[1..].join("\n") + "\n")),
            plan.summary.plan_hash
        );
        // A person deletes the first unit and skips the second; the plan still reads.
        let edited = [
            lines[0],
            &lines[2].replace("\"decision\":\"import\"", "\"decision\":\"skip\""),
            lines[3],
        ]
        .join("\n");
        let read_back = parse(&edited).unwrap();
        let imported: Vec<&str> = read_back.import_units().map(|u| u.text.as_str()).collect();
        assert_eq!(imported.len(), 1);
        assert_eq!(read_back.import_units().next().unwrap().mode, MODE_SPLIT);
        // Writing again replaces the file rather than appending to it.
        write(&read_back, &path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 3);

        let bad = [
            lines[0],
            &lines[1].replace("\"decision\":\"import\"", "\"decision\":\"maybe\""),
        ]
        .join("\n");
        let error = parse(&bad).unwrap_err().to_string();
        assert!(
            error.contains("line 2") && error.contains("maybe"),
            "{error}"
        );
        let old = lines[0].replace("\"version\":1", "\"version\":9");
        assert!(parse(&old).unwrap_err().to_string().contains("version 9"));
        assert!(parse(lines[1]).unwrap_err().to_string().contains("summary"));
        assert!(parse("").is_err());
        let twice = [lines[0], lines[1], lines[1]].join("\n");
        assert!(parse(&twice).unwrap_err().to_string().contains("line 3"));
    }

    #[test]
    fn default_paths_are_private_and_safe_file_names() {
        let home = tempfile::tempdir().unwrap();
        let now = DateTime::parse_from_rfc3339("2026-10-22T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let path = default_path(home.path(), "claude-code", "team/app x", now);
        assert_eq!(
            path,
            home.path()
                .join(IMPORTS_DIR)
                .join("claude-code-team_app_x-20261022T120000Z.plan.jsonl")
        );
        write_private(&path, "{}\n").unwrap();
        let next = default_path(home.path(), "claude-code", "team/app x", now);
        assert!(next.ends_with("claude-code-team_app_x-20261022T120000Z-2.plan.jsonl"));
        assert_eq!(file_part(".."), "_");
        assert_eq!(file_part("项目"), "__");
    }

    fn session(id: &str, started: &str, turns: Vec<Turn>) -> SessionDoc {
        SessionDoc {
            host: "claude-code".into(),
            via: "claude-transcript",
            path: PathBuf::from(format!("/home/u/.claude/projects/-app/{id}.jsonl")),
            display_path: format!("~/.claude/projects/-app/{id}.jsonl"),
            session: id.into(),
            cwd: PathBuf::from("/home/u/code/app"),
            file_hash: content_hash(id),
            started_at: started.into(),
            turns,
            extra: json!({"version": "2.1.306", "unset": null}),
        }
    }

    fn turn(native: &str, at: &str, skip: Option<&str>) -> Turn {
        Turn {
            native_id: native.into(),
            prompt: format!("Prompt {native}."),
            reply: format!("Reply {native}."),
            prompt_at: at.into(),
            reply_at: at.into(),
            skip: skip.map(str::to_string),
        }
    }

    fn with_sessions(sessions: Vec<SessionDoc>, since: Option<&str>, limit: usize) -> Plan {
        let mut opts = options(false);
        opts.host = "claude-code".into();
        opts.sessions = Some(SessionOptions {
            since: since.map(str::to_string),
            limit,
        });
        let scan = ScanOutput {
            sessions,
            ..ScanOutput::default()
        };
        build(scan, &opts).unwrap()
    }

    #[test]
    fn session_turns_come_oldest_first_from_since_up_to_the_limit() {
        let at = |t: &str| format!("2026-01-01T{t}:00Z");
        let sessions = vec![
            // A fork of s1 repeats its turn t2, then goes on.
            session(
                "s2",
                &at("03:00"),
                vec![
                    turn("t2", &at("02:00"), None),
                    turn("t4", &at("03:30"), None),
                    turn("x2", &at("05:00"), Some("no-reply")),
                ],
            ),
            session(
                "s1",
                &at("01:00"),
                vec![
                    turn("t1", &at("01:00"), None),
                    turn("t2", &at("02:00"), None),
                    turn("x1", &at("02:30"), Some("turnOrigin:sdk")),
                    turn("t3", &at("04:00"), None),
                ],
            ),
            session(
                "s0",
                "2025-12-31T22:00:00Z",
                vec![turn("t0", "2025-12-31T23:00:00Z", None)],
            ),
        ];
        let plan = with_sessions(sessions, Some("2026-01-01T00:00:00Z"), 3);
        let got: Vec<(&str, &str, &str)> = plan
            .units
            .iter()
            .map(|u| {
                (
                    u.unit.turn.as_ref().unwrap().native_id.as_str(),
                    u.decision.as_str(),
                    u.reason.as_str(),
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                ("t1", IMPORT, MODE_SESSION_TURN),
                ("t2", IMPORT, MODE_SESSION_TURN),
                ("x1", SKIP, "turnOrigin:sdk"),
                ("t4", IMPORT, MODE_SESSION_TURN),
            ]
        );
        // The repeated turn is filed under the session that started first, as capture filed it.
        let t2 = &plan.units[1].unit;
        assert_eq!(t2.turn.as_ref().unwrap().session, "s1");
        assert_eq!(t2.unit, "claude-code:session%3As1#t2");
        assert_eq!(
            plan.summary.sessions,
            Some(SessionsSummary {
                sessions: 3,
                since: Some("2026-01-01T00:00:00Z".into()),
                limit: 3,
                before_since: 1,
                over_limit: 1,
                copies: 1,
                next_since: Some(at("04:00")),
            })
        );
        let s = &plan.summary;
        assert_eq!((s.counts.session_turns, s.counts.skipped), (3, 1));
        assert_eq!(
            s.skip_reasons,
            BTreeMap::from([("turnOrigin:sdk".to_string(), 1)])
        );
        assert_eq!(s.estimates.generation_calls, 3);
        let chars: usize = ["t1", "t2", "t4"]
            .iter()
            .map(|t| format!("Prompt {t}.").len() + format!("Reply {t}.").len())
            .sum();
        assert_eq!(s.estimates.chars_to_generation, chars);
        assert_eq!(s.estimates.embedding_texts, 0);
        // The unit carries the turn as capture would queue it, under capture's turn id.
        let t1 = &plan.units[0].unit;
        assert_eq!(t1.mode, MODE_SESSION_TURN);
        assert_eq!(t1.text, "Prompt t1.");
        assert_eq!(t1.unit_hash, content_hash("Prompt t1."));
        assert_eq!(t1.stated_at, at("01:00"));
        assert_eq!(t1.file_hash, content_hash("s1"));
        let queued = t1.turn.as_ref().unwrap();
        assert_eq!(queued.turn_id, turn_key("my-project", "s1", "t1"));
        assert_eq!(
            queued.turn_id,
            crate::harness::outbox::turn_key("my-project", "s1", "t1")
        );
        let roles: Vec<(&str, &str)> = queued
            .messages
            .iter()
            .map(|m| (m.role.as_str(), m.content.as_str()))
            .collect();
        assert_eq!(roles, [("user", "Prompt t1."), ("assistant", "Reply t1.")]);
        assert_eq!(
            t1.source,
            json!({"via": "claude-transcript", "path": "~/.claude/projects/-app/s1.jsonl",
                   "nativeId": "t1", "session": "s1", "cwd": "~/code/app", "version": "2.1.306"})
        );
        // Without --since and with room, every turn is planned and none is left out.
        let all = with_sessions(
            vec![session(
                "s1",
                &at("01:00"),
                vec![turn("t1", &at("01:00"), None)],
            )],
            None,
            200,
        );
        let summary = all.summary.sessions.unwrap();
        assert_eq!((summary.over_limit, summary.next_since), (0, None));
        assert_eq!(parse(&render(&plan).unwrap()).unwrap(), plan);
    }

    #[test]
    fn session_turns_are_redacted_and_checked_when_read_back() {
        let token: String = "Fake0Key9".chars().cycle().take(30).collect();
        let pem = |edge: &str| ["-----", edge, " RSA PRIVATE KEY-----"].concat();
        let mut leaky = turn("t1", "2026-01-01T01:00:00Z", None);
        leaky.reply = format!("Use sk-{token} for staging.");
        let mut keyed = turn("t2", "2026-01-01T02:00:00Z", None);
        keyed.prompt = format!("Store this:\n{}\nMIIfake\n{}", pem("BEGIN"), pem("END"));
        let plan = with_sessions(
            vec![session("s1", "2026-01-01T01:00:00Z", vec![leaky, keyed])],
            None,
            200,
        );
        let (a, b) = (&plan.units[0], &plan.units[1]);
        assert_eq!(a.decision, IMPORT);
        assert_eq!(
            a.unit.turn.as_ref().unwrap().messages[1].content,
            "Use [redacted:api-key] for staging."
        );
        assert_eq!(a.redactions, BTreeMap::from([("api-key".to_string(), 1)]));
        assert_eq!(
            (b.decision.as_str(), b.reason.as_str()),
            (SKIP, "private-key")
        );
        let text = render(&plan).unwrap();
        assert!(!text.contains(&token) && !text.contains("MIIfake"));
        // A session line edited to another project's turn id, or stripped of its turn, is refused by line.
        let lines: Vec<&str> = text.lines().collect();
        let moved = lines[1].replace("my-project/s1/t1", "other/s1/t1");
        let error = parse(&[lines[0], &moved].join("\n"))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("line 2") && error.contains("capture turn key"),
            "{error}"
        );
        let mut bare: Value = serde_json::from_str(lines[1]).unwrap();
        bare.as_object_mut().unwrap().remove("turn");
        let error = parse(&[lines[0].to_string(), bare.to_string()].join("\n"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("needs its turn"), "{error}");
        // A skipped line may lack a reply; it is never proposed.
        assert!(parse(&[lines[0], lines[2]].join("\n")).is_ok());
    }

    #[test]
    fn notes_come_before_session_turns() {
        let mut opts = options(false);
        opts.sessions = Some(SessionOptions {
            since: None,
            limit: 10,
        });
        let mut scan = scan(vec![doc(
            "n.md",
            "2026-06-01T00:00:00Z",
            vec![segment("note", "A later note.", Policy::VerbatimIfFits)],
        )]);
        scan.sessions = vec![session(
            "s1",
            "2026-01-01T01:00:00Z",
            vec![turn("t1", "2026-01-01T01:00:00Z", None)],
        )];
        let plan = build(scan, &opts).unwrap();
        let modes: Vec<&str> = plan.units.iter().map(|u| u.unit.mode.as_str()).collect();
        assert_eq!(modes, [MODE_VERBATIM, MODE_SESSION_TURN]);
        assert_eq!(plan.summary.counts.units, 2);
        assert_eq!(plan.summary.issues.len(), 1);
    }
}
