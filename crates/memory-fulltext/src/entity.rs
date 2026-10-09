//! Entity alias dictionary (design doc 5.2 实体索引): normalised exact lookup and leftmost-longest
//! alias matching over free text, with word boundaries for ASCII alphanumeric alias ends.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use aho_corasick::{AhoCorasick, MatchKind};

use crate::analyzer::{halfwidth, is_cjk};

/// Normalises an entity name or alias: full-width → half-width, lowercase, all whitespace (and
/// zero-width characters) removed. `"Ｓｕｒｒｅａｌ ＤＢ"` → `"surrealdb"`.
pub fn normalize_name(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let h = halfwidth(c);
        if skip_in_name(h) {
            continue;
        }
        out.extend(h.to_lowercase());
    }
    out
}

#[inline]
fn skip_in_name(c: char) -> bool {
    c.is_whitespace() || matches!(c, '\u{200B}'..='\u{200D}' | '\u{2060}' | '\u{FEFF}')
}

/// A character that glues to an ASCII alias end, i.e. the alias would be part of a longer word.
#[inline]
fn is_wordish(c: char) -> bool {
    c == '_' || (c.is_alphanumeric() && !is_cjk(c))
}

#[derive(Debug, Clone)]
struct Matcher {
    ac: AhoCorasick,
    /// pattern id → normalised alias
    aliases: Vec<String>,
    /// pattern id → (left end needs a word boundary, right end needs a word boundary)
    bounds: Vec<(bool, bool)>,
}

/// In-memory alias dictionary. Not internally synchronised for writes (`&mut self`); reads are `&self`
/// and safe to share (`Send + Sync`). The matcher is rebuilt lazily on the first lookup after a change.
#[derive(Debug, Clone, Default)]
pub struct EntityDict {
    /// entity id → its normalised names (canonical name + aliases), insertion order
    names: HashMap<String, Vec<String>>,
    /// normalised alias → entity ids sharing it (oldest first; the last one is the most recent)
    alias_ids: HashMap<String, Vec<String>>,
    matcher: OnceLock<Option<Matcher>>,
}

impl EntityDict {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `names` (canonical name and/or aliases) to `entity_id`. Names already registered for the
    /// entity are kept (union semantics); use [`EntityDict::replace`] to set the exact name list.
    /// An alias shared by several entities maps to all of them; [`EntityDict::resolve`] returns the
    /// most recently upserted one.
    pub fn upsert(&mut self, entity_id: &str, names: &[String]) {
        let list = self.names.entry(entity_id.to_string()).or_default();
        for n in names {
            let nn = normalize_name(n);
            if nn.is_empty() {
                continue;
            }
            if !list.contains(&nn) {
                list.push(nn.clone());
            }
            let ids = self.alias_ids.entry(nn).or_default();
            ids.retain(|i| i != entity_id);
            ids.push(entity_id.to_string());
        }
        self.matcher = OnceLock::new();
    }

    /// Sets the exact name list of `entity_id` (drops names not in `names`).
    pub fn replace(&mut self, entity_id: &str, names: &[String]) {
        self.remove(entity_id);
        self.upsert(entity_id, names);
    }

    pub fn remove(&mut self, entity_id: &str) {
        if let Some(list) = self.names.remove(entity_id) {
            for a in list {
                if let Some(ids) = self.alias_ids.get_mut(&a) {
                    ids.retain(|i| i != entity_id);
                    if ids.is_empty() {
                        self.alias_ids.remove(&a);
                    }
                }
            }
        }
        self.matcher = OnceLock::new();
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    /// Normalised names registered for an entity.
    pub fn names_of(&self, entity_id: &str) -> Option<&[String]> {
        self.names.get(entity_id).map(|v| v.as_slice())
    }

    /// Exact lookup after normalisation. For an alias shared by several entities, the most recently
    /// upserted entity wins; see [`EntityDict::resolve_all`].
    pub fn resolve(&self, name: &str) -> Option<String> {
        self.alias_ids
            .get(&normalize_name(name))
            .and_then(|v| v.last().cloned())
    }

    /// All entities registered under the (normalised) name, most recent first.
    pub fn resolve_all(&self, name: &str) -> Vec<String> {
        self.alias_ids
            .get(&normalize_name(name))
            .map(|v| v.iter().rev().cloned().collect())
            .unwrap_or_default()
    }

    fn matcher(&self) -> Option<&Matcher> {
        self.matcher
            .get_or_init(|| {
                if self.alias_ids.is_empty() {
                    return None;
                }
                let mut aliases: Vec<String> = self.alias_ids.keys().cloned().collect();
                aliases.sort(); // deterministic pattern ids
                let bounds = aliases
                    .iter()
                    .map(|a| {
                        let first = a.chars().next().unwrap_or(' ');
                        let last = a.chars().last().unwrap_or(' ');
                        (first.is_ascii_alphanumeric(), last.is_ascii_alphanumeric())
                    })
                    .collect();
                let ac = AhoCorasick::builder()
                    .match_kind(MatchKind::Standard) // overlapping search; longest choice is done below
                    .build(&aliases)
                    .ok()?;
                Some(Matcher {
                    ac,
                    aliases,
                    bounds,
                })
            })
            .as_ref()
    }

    /// Entity ids mentioned in `text`, de-duplicated in order of appearance.
    ///
    /// The text is normalised like the names (half-width, lowercase, whitespace removed), so
    /// "Surreal DB" matches alias "surrealdb". Scanning left to right, at each position the longest alias
    /// that passes the boundary check is taken and the scan continues after it. An alias end that is an
    /// ASCII letter/digit must not be glued to another letter/digit/underscore in the original text
    /// ("Al" does not match in "also", "Mel" not in "Melanie"); CJK neighbours never block.
    /// An alias shared by several entities yields all of them (most recent first).
    pub fn longest_match(&self, text: &str) -> Vec<String> {
        let Some(m) = self.matcher() else {
            return Vec::new();
        };
        // original chars (half-width mapped) and normalised text with byte → original-char-index map
        let orig: Vec<char> = text.chars().map(halfwidth).collect();
        let mut norm = String::with_capacity(text.len());
        let mut byte_to_orig: Vec<u32> = Vec::with_capacity(text.len());
        for (i, &c) in orig.iter().enumerate() {
            if skip_in_name(c) {
                continue;
            }
            for lc in c.to_lowercase() {
                norm.push(lc);
                for _ in 0..lc.len_utf8() {
                    byte_to_orig.push(i as u32);
                }
            }
        }
        // all (overlapping) matches, ordered by start asc, length desc
        let mut cands: Vec<(usize, usize, usize)> =
            m.ac.find_overlapping_iter(&norm)
                .map(|x| (x.start(), x.end(), x.pattern().as_usize()))
                .collect();
        if cands.is_empty() {
            return Vec::new();
        }
        cands.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));

        let glued = |idx: isize| -> bool {
            idx >= 0 && (idx as usize) < orig.len() && is_wordish(orig[idx as usize])
        };
        let mut out: Vec<String> = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        let mut cursor = 0usize;
        for &(s, e, pid) in &cands {
            if s < cursor {
                continue;
            }
            let (need_l, need_r) = m.bounds[pid];
            let os = byte_to_orig[s] as isize;
            let oe = byte_to_orig[e - 1] as isize;
            if (need_l && glued(os - 1)) || (need_r && glued(oe + 1)) {
                continue; // try the next (shorter, or later) candidate
            }
            cursor = e;
            if let Some(ids) = self.alias_ids.get(&m.aliases[pid]) {
                for id in ids.iter().rev() {
                    if seen.insert(id.as_str()) {
                        out.push(id.clone());
                    }
                }
            }
        }
        out
    }
}
