//! Import unit sizes, the deterministic split and unit identities (design §6.5 and §6.9). Sizes are record
//! costs as search packing charges them, and the limits follow from the automatic-recall budget, so a unit that
//! fits can always be injected beside another record. None of them is picked from pilot or benchmark scores.
use super::markdown;
use crate::util::est_tokens;

/// Tokens the daemon adds to every record when packing search results (`retrieval.rs`: est_tokens(text) + 8).
pub const RECORD_OVERHEAD: usize = 8;

/// The record cost search packing charges: `util::est_tokens(text) + RECORD_OVERHEAD`.
pub fn record_cost(text: &str) -> usize {
    est_tokens(text) + RECORD_OVERHEAD
}

/// Unit size limits derived from the automatic-recall budget (design §6.5). Not tunables picked from scores.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnitLimits {
    /// `tokenBudget` of the recall hook's search (`--recall-budget`).
    pub recall_budget: usize,
    /// `topK` of the recall hook's search (`--recall-top-k`).
    pub recall_top_k: usize,
}

impl Default for UnitLimits {
    /// recall_budget 400, recall_top_k 5: the installed recall hook's defaults (`harness/recall.rs`).
    fn default() -> Self {
        UnitLimits {
            recall_budget: 400,
            recall_top_k: 5,
        }
    }
}

impl UnitLimits {
    /// Largest record cost of one unit: half the recall budget, so another record still fits beside it.
    /// Below `RECORD_OVERHEAD + 1` (a recall budget under 18) no text fits.
    pub fn hard_cap(&self) -> usize {
        self.recall_budget / 2
    }

    /// Record cost extraction aims for: recall_budget / recall_top_k, at least RECORD_OVERHEAD + 1 and
    /// never above `hard_cap` (with recall_top_k 1 the share would be the whole budget).
    pub fn target(&self) -> usize {
        (self.recall_budget / self.recall_top_k)
            .min(self.hard_cap())
            .max(RECORD_OVERHEAD + 1)
    }

    /// The smallest recall budget a unit can be cut for: a hard cap that leaves room for some text.
    pub const MIN_RECALL_BUDGET: usize = 40;

    /// Whether `text` fits the hard cap.
    pub fn fits(&self, text: &str) -> bool {
        record_cost(text) <= self.hard_cap()
    }
}

/// Deterministic split for the no-model path (`--no-extract`): pieces of `text` that each fit `limits`.
/// Order of attempts: if the whole text (with context) fits, one piece; else top-level list items and paragraphs
/// (markdown::list_items for list blocks, paragraphs otherwise, in document order); a piece still too long is split
/// into sentences, consecutive sentences are packed greedily while they fit; a single sentence still too long is cut
/// at character boundaries into the longest prefixes that fit. When `context` is nonempty every piece is
/// `"{context}: {piece}"` and the prefix counts toward the cost (if the context alone leaves no room, drop the
/// context). Empty pieces are dropped. Every returned piece fits.
///
/// `text` and `context` are trimmed first. Packed sentences keep the original text between them, so a piece of a
/// paragraph is a substring of it; list items lose their marker and continuation indentation, and the text around a
/// list splits into its own paragraphs (`markdown::list_items`). "No room" means not even one more character fits
/// beside the context. With a cap below `RECORD_OVERHEAD + 1` nothing fits and nothing is returned.
pub fn split_to_fit(text: &str, context: &str, limits: &UnitLimits) -> Vec<String> {
    let text = text.trim();
    if text.is_empty() {
        return Vec::new();
    }
    let cap = limits.hard_cap();
    let context = context.trim();
    // A CJK character is the costliest single character (one token), so it probes the room left.
    let prefix = if !context.is_empty() && record_cost(&format!("{context}: 中")) <= cap {
        format!("{context}: ")
    } else {
        String::new()
    };
    let packer = Packer {
        prefix: &prefix,
        cap,
    };
    if packer.fits(text) {
        return vec![packer.piece(text)];
    }
    let mut out = Vec::new();
    for block in markdown::blocks(text) {
        if packer.fits(&block) {
            out.push(packer.piece(&block));
        } else {
            packer.pack_sentences(&block, &mut out);
        }
    }
    out
}

/// Builds pieces with the context prefix and measures them whole: `est_tokens` is not additive, since the
/// non-CJK characters of prefix and body round up together.
struct Packer<'a> {
    prefix: &'a str,
    cap: usize,
}

impl Packer<'_> {
    fn piece(&self, body: &str) -> String {
        format!("{}{body}", self.prefix)
    }

    fn fits(&self, body: &str) -> bool {
        record_cost(&self.piece(body)) <= self.cap
    }

    /// Greedy runs of consecutive sentences, cut from `block` so the separators between them stay as written.
    fn pack_sentences(&self, block: &str, out: &mut Vec<String>) {
        let spans = markdown::sentence_spans(block);
        let mut next = 0;
        while next < spans.len() {
            let (start, mut end) = spans[next];
            next += 1;
            if !self.fits(&block[start..end]) {
                self.cut(&block[start..end], out);
                continue;
            }
            while next < spans.len() && self.fits(&block[start..spans[next].1]) {
                end = spans[next].1;
                next += 1;
            }
            out.push(self.piece(&block[start..end]));
        }
    }

    /// The longest prefixes that fit, at character boundaries; stops if not even one character fits.
    fn cut(&self, sentence: &str, out: &mut Vec<String>) {
        // A fitting body costs at most `cap` tokens and every character at least a quarter token.
        let longest = self.cap.saturating_mul(4);
        let mut rest = sentence.trim();
        while !rest.is_empty() {
            let ends: Vec<usize> = rest
                .char_indices()
                .map(|(i, c)| i + c.len_utf8())
                .take(longest)
                .collect();
            // The cost never falls as a prefix grows, so the fitting ends come first.
            let fitting = ends.partition_point(|&end| self.fits(&rest[..end]));
            let Some(&end) = fitting.checked_sub(1).and_then(|last| ends.get(last)) else {
                return;
            };
            let head = rest[..end].trim_end();
            if !head.is_empty() {
                out.push(self.piece(head));
            }
            rest = rest[end..].trim_start();
        }
    }
}

/// Extraction inputs of at most `max_bytes` bytes covering `text` in order (design §6.6: one extraction call
/// takes a bounded text). Blocks (`markdown::blocks`: paragraphs, list blocks, code fences) are packed greedily
/// with a blank line between them; a longer block becomes greedy runs of its sentences, cut from it so their
/// separators stay as written; a longer sentence is cut at character boundaries. `text` is trimmed; only the
/// blank space between pieces is lost. Every returned piece is nonempty and at most `max_bytes` long (a single
/// character longer than that stays whole).
pub fn chunk_bytes(text: &str, max_bytes: usize) -> Vec<String> {
    let text = text.trim();
    if text.is_empty() {
        return Vec::new();
    }
    if text.len() <= max_bytes {
        return vec![text.to_string()];
    }
    let mut parts = Vec::new();
    for block in markdown::blocks(text) {
        if block.len() <= max_bytes {
            parts.push(block);
            continue;
        }
        let spans = markdown::sentence_spans(&block);
        let mut next = 0;
        while next < spans.len() {
            let (start, mut end) = spans[next];
            next += 1;
            if end - start > max_bytes {
                parts.extend(cut_bytes(block[start..end].trim(), max_bytes));
                continue;
            }
            while next < spans.len() && spans[next].1 - start <= max_bytes {
                end = spans[next].1;
                next += 1;
            }
            parts.push(block[start..end].trim().to_string());
        }
    }
    let mut chunks: Vec<String> = Vec::new();
    for part in parts.into_iter().filter(|p| !p.is_empty()) {
        match chunks.last_mut() {
            Some(last) if last.len() + 2 + part.len() <= max_bytes => {
                last.push_str("\n\n");
                last.push_str(&part);
            }
            _ => chunks.push(part),
        }
    }
    chunks
}

/// The longest prefixes of `text` of at most `max_bytes` bytes, at character boundaries.
fn cut_bytes(text: &str, max_bytes: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let mut end = rest.len().min(max_bytes);
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        if end == 0 {
            end = rest.chars().next().map_or(rest.len(), char::len_utf8);
        }
        out.push(rest[..end].to_string());
        rest = &rest[end..];
    }
    out
}

/// blake3 hex of the exact text.
pub fn content_hash(text: &str) -> String {
    crate::util::hex_hash(text)
}

/// Exact-deduplication key of an imported record: blake3 hex over length-prefixed `project`, the text trimmed with
/// internal whitespace runs collapsed to one space, and the literal "imported" (same length-prefix scheme as
/// `mem0::content_key`, so it can never equal a Mem0 key).
pub fn import_key(project: &str, text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut hash = blake3::Hasher::new();
    for part in [project.as_bytes(), text.as_bytes(), b"imported"] {
        hash.update(&(part.len() as u64).to_le_bytes());
        hash.update(part);
    }
    hash.finalize().to_hex().to_string()
}

/// Stable unit identity `host:native_id#locator`. Each component is percent-encoded like the dialogue outbox turn keys
/// (keep ASCII letters, digits, `-`, `_`, `.`, `~`; encode every other byte as `%XX` uppercase), so `:`, `#` and `/`
/// inside a component cannot collide with the separators.
pub fn unit_key(host: &str, native_id: &str, locator: &str) -> String {
    format!("{}:{}#{}", quote(host), quote(native_id), quote(locator))
}

/// The turn id dialogue capture gives a host's turn, `project/session/native_id` with each part quoted as in
/// `unit_key`: the dialogue outbox and session import (design §6.10) share it, so the daemon's `platform +
/// turnId` deduplication finds an imported turn that capture already sent.
pub fn turn_key(project: &str, session: &str, native_id: &str) -> String {
    [project, session, native_id].map(quote).join("/")
}

/// Python's `quote(part, safe='')`, as the dialogue outbox quotes turn keys.
fn quote(part: &str) -> String {
    let mut out = String::with_capacity(part.len());
    for byte in part.bytes() {
        if byte.is_ascii_alphanumeric() || b"_.-~".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mem0::content_key;

    /// Non-whitespace characters, to compare content across re-wrapped pieces.
    fn dense(text: &str) -> String {
        text.chars().filter(|c| !c.is_whitespace()).collect()
    }

    /// Piece bodies after the context prefix, which every piece must carry.
    fn bodies<'a>(pieces: &'a [String], context: &str) -> Vec<&'a str> {
        let prefix = format!("{context}: ");
        pieces
            .iter()
            .map(|p| p.strip_prefix(prefix.as_str()).expect("context prefix"))
            .collect()
    }

    #[test]
    fn limits_follow_the_recall_budget() {
        let limits = UnitLimits::default();
        assert_eq!((limits.recall_budget, limits.recall_top_k), (400, 5));
        assert_eq!((limits.hard_cap(), limits.target()), (200, 80));
        assert_eq!(record_cost("共享记忆库"), 13);
        assert!(limits.fits(&"汉".repeat(192)));
        assert!(!limits.fits(&"汉".repeat(193)));
        // A single recall slot would make the share the whole budget: the target stays within the cap.
        let single = UnitLimits {
            recall_budget: 400,
            recall_top_k: 1,
        };
        assert_eq!(single.target(), single.hard_cap());
        let tiny = UnitLimits {
            recall_budget: 20,
            recall_top_k: 5,
        };
        assert_eq!(tiny.target(), RECORD_OVERHEAD + 1);
    }

    #[test]
    fn short_text_stays_one_piece_with_its_context() {
        let limits = UnitLimits::default();
        assert_eq!(
            split_to_fit("  Use tabs.  ", " Style ", &limits),
            vec!["Style: Use tabs."]
        );
        assert_eq!(split_to_fit("Use tabs.", "", &limits), vec!["Use tabs."]);
        assert!(split_to_fit(" \n ", "Style", &limits).is_empty());
    }

    #[test]
    fn long_chinese_section_packs_whole_sentences_in_order() {
        let limits = UnitLimits::default();
        let text: String = (0..40)
            .map(|i| format!("第{i}条记录说明部署流程需要先检查配置文件。"))
            .collect();
        let context = "部署笔记 > 发布";
        let pieces = split_to_fit(&text, context, &limits);
        assert!(pieces.len() > 3);
        assert!(pieces.iter().all(|p| limits.fits(p)));
        let bodies = bodies(&pieces, context);
        // No separator is invented between Chinese sentences: the pieces rebuild the text exactly.
        assert_eq!(bodies.concat(), text);
        assert!(bodies
            .iter()
            .all(|b| b.starts_with('第') && b.ends_with('。')));
        // Greedy: the first sentence of the next piece would not have fitted beside the context and this piece.
        for (body, next) in bodies.iter().zip(&bodies[1..]) {
            let first = &next[..next.find('。').expect("sentence") + '。'.len_utf8()];
            assert!(!limits.fits(&format!("{context}: {body}{first}")));
        }
    }

    #[test]
    fn long_english_list_splits_at_items_then_sentences() {
        let limits = UnitLimits::default();
        let items: Vec<String> = (0..30)
            .map(|i| format!("Item {i} notes that the release checklist runs the integration suite before tagging"))
            .collect();
        // About 280 tokens: over the cap, so this item is the one packed by sentences.
        let long: String = (0..20)
            .map(|i| format!("Long item sentence {i} explains one more release detail. "))
            .collect();
        let mut lines: Vec<String> = items.iter().map(|item| format!("- {item}")).collect();
        lines.insert(10, format!("- {}", long.trim()));
        let text = format!(
            "Checklist intro.\n{}\n\nClosing paragraph.",
            lines.join("\n")
        );
        let context = "Release > Checklist";
        let pieces = split_to_fit(&text, context, &limits);
        assert!(pieces.iter().all(|p| limits.fits(p)));
        let bodies = bodies(&pieces, context);
        assert_eq!(bodies[0], "Checklist intro.");
        assert_eq!(bodies[1..11], items[..10]);
        // The one item too long for the cap is packed by sentences; its neighbours stay whole and in order.
        let split = bodies.len() - 1 - items.len() + 10;
        assert!(split >= 13, "the long item became at least two pieces");
        assert_eq!(bodies[11..split].join(" "), long.trim());
        assert!(bodies[11..split]
            .iter()
            .all(|b| b.starts_with("Long item sentence") && b.ends_with('.')));
        assert_eq!(bodies[split..bodies.len() - 1], items[10..]);
        assert_eq!(bodies[bodies.len() - 1], "Closing paragraph.");
    }

    #[test]
    fn one_huge_sentence_is_cut_at_character_boundaries() {
        let limits = UnitLimits::default();
        let text = "alpha beta gamma delta ".repeat(60);
        let pieces = split_to_fit(&text, "Notes", &limits);
        assert!(pieces.len() >= 2);
        assert!(pieces.iter().all(|p| limits.fits(p)));
        let bodies = bodies(&pieces, "Notes");
        assert_eq!(dense(&bodies.concat()), dense(&text));
        // Longest prefixes: every piece but the last is at the cap, less a trimmed trailing space.
        assert!(pieces[..pieces.len() - 1]
            .iter()
            .all(|p| record_cost(p) >= limits.hard_cap() - 1));
        // Character boundaries hold for multibyte text too.
        let wide = "汉字".repeat(300);
        let pieces = split_to_fit(&wide, "", &limits);
        assert_eq!(pieces.concat(), wide);
        assert!(pieces.iter().all(|p| limits.fits(p)));
    }

    #[test]
    fn context_counts_toward_the_cost_and_is_dropped_without_room() {
        let limits = UnitLimits::default();
        // 182 tokens: fits alone (cost 190), not with a 16-token prefix (cost 206).
        let first = format!("{}。", "汉".repeat(150));
        let second = format!("{}。", "字".repeat(30));
        let text = format!("{first}{second}");
        assert!(limits.fits(&text));
        let context = "上下文".repeat(5);
        assert_eq!(
            split_to_fit(&text, &context, &limits),
            vec![
                format!("{context}: {first}"),
                format!("{context}: {second}")
            ]
        );
        // A context that leaves no room for one more character is dropped.
        let huge = "标".repeat(200);
        assert_eq!(
            split_to_fit("Short note.", &huge, &limits),
            vec!["Short note."]
        );
        // A cap below one record's overhead fits nothing.
        let none = UnitLimits {
            recall_budget: 16,
            recall_top_k: 5,
        };
        assert!(split_to_fit("text", "", &none).is_empty());
    }

    #[test]
    fn hashes_and_dedup_keys() {
        assert_eq!(
            content_hash("a b"),
            blake3::hash(b"a b").to_hex().to_string()
        );
        assert_ne!(content_hash("a b"), content_hash("a  b"));
        let key = import_key("proj", "Use  tabs\n for Go.");
        assert_eq!(key, import_key("proj", " Use tabs for Go. "));
        assert_ne!(key, import_key("other", "Use tabs for Go."));
        assert_ne!(key, import_key("proj", "Use tabs for go."));
        // Never a Mem0 content key, raw or inferred, of the same project and text.
        for inferred in [false, true] {
            assert_ne!(key, content_key("proj", "Use tabs for Go.", inferred));
        }
        // Length prefixes keep the parts apart.
        assert_ne!(import_key("ab", "c"), import_key("a", "bc"));
    }

    #[test]
    fn unit_keys_escape_their_components() {
        assert_eq!(
            unit_key("claude-code", "memory/deploy notes.md", "deploy#2"),
            "claude-code:memory%2Fdeploy%20notes.md#deploy%232"
        );
        assert_eq!(
            unit_key("a:b", "c#d", "发布~"),
            "a%3Ab:c%23d#%E5%8F%91%E5%B8%83~"
        );
        assert_eq!(turn_key("p", "s 1", "a/b?"), "p/s%201/a%2Fb%3F");
    }

    #[test]
    fn extraction_inputs_are_bounded_and_keep_the_text_in_order() {
        assert!(chunk_bytes("  \n ", 64).is_empty());
        assert_eq!(chunk_bytes(" short ", 64), ["short"]);
        // Paragraphs pack greedily with a blank line between them.
        let text = "First paragraph here.\n\nSecond one.\n\nThird paragraph, a little longer.";
        let chunks = chunk_bytes(text, 40);
        assert_eq!(
            chunks,
            [
                "First paragraph here.\n\nSecond one.",
                "Third paragraph, a little longer."
            ]
        );
        // A long Chinese paragraph splits into runs of whole sentences, then a long sentence by characters.
        let long = "第一句很短。".repeat(20) + &"长".repeat(100);
        let chunks = chunk_bytes(&long, 90);
        assert!(chunks.iter().all(|c| !c.is_empty() && c.len() <= 90));
        assert!(chunks[0].ends_with('。'));
        assert_eq!(chunks.concat(), long);
    }
}
