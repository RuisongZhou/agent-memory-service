//! Provenance and size checks of the items an extraction model returns for one import unit (design §6.6). The
//! checks never change an item: it passes, or it is rejected with a reason while the rest of the response stays.
//! Names and numbers are checked as consolidation checks a merged text (`consolidation.rs`).
use super::units::{record_cost, UnitLimits};
use crate::consolidation::{unsupported_entities, unsupported_numbers};
use crate::util::is_cjk;
use std::collections::BTreeSet;

/// Kinds an extracted item may have. Every one becomes an imported fact; a rule about how to work is a preference.
pub const ITEM_KINDS: [&str; 3] = ["fact", "preference", "reference"];
/// Scope hints an extracted item may carry.
pub const ITEM_SCOPES: [&str; 2] = ["project", "global"];
/// Longest evidence quote, in characters.
pub const MAX_EVIDENCE_CHARS: usize = 600;
/// Shortest evidence quote, in characters, unless it is the whole unit.
const MIN_EVIDENCE_CHARS: usize = 8;
/// Share of the text's content words, in percent (rounded up), that its evidence must contain.
const MIN_SHARED_PERCENT: usize = 30;

/// One item as the extraction model returned it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemRef<'a> {
    /// The self-contained memory text.
    pub text: &'a str,
    /// One of `ITEM_KINDS`.
    pub kind: &'a str,
    /// One of `ITEM_SCOPES`.
    pub scope: &'a str,
    /// The quote of the unit that supports the item.
    pub evidence: &'a str,
}

/// Provenance and size checks of one extracted item (design §6.6), first failure wins, in this order:
/// empty text; unknown kind; unknown scope; cost over `limits.hard_cap()` ("text costs N, more than the unit cap M");
/// empty evidence; evidence longer than MAX_EVIDENCE_CHARS; evidence not a quote of `unit_text` (`evidence_in`);
/// evidence shorter than 8 characters unless it is the whole unit; evidence holding fewer than 30% (rounded up) of
/// the text's content words ("evidence shares N of M content words with the text"); numbers in text found in neither `unit_text` nor `context`
/// (consolidation::unsupported_numbers, list them); named entities likewise (unsupported_entities).
/// `context` holds the unit's metadata strings (project, host, heading path, statedAt) that a self-contained text may name.
/// Error strings are short, lowercase, and name the offending values.
///
/// Text and evidence that are only whitespace count as empty. Kind and scope match exactly, so `transient`
/// (progress notes the design drops before review) and an empty scope fail here: drop or default them first.
///
/// The evidence length counts characters after `evidence_in`'s normalization. Content words are lowercase ASCII
/// words of four or more letters and digits plus each pair of consecutive CJK letters, counted once; a text with
/// none passes the share rule, and a text equal to its evidence always does. The share rule rejects a quote that
/// only names the subject ("atlas") of a claim the unit never makes, while 30% leaves room for a self-contained
/// rewrite that adds the project's name and other words of its own.
pub fn check_item(
    item: &ItemRef<'_>,
    unit_text: &str,
    context: &[&str],
    limits: &UnitLimits,
) -> Result<(), String> {
    if item.text.trim().is_empty() {
        return Err("empty text".into());
    }
    if !ITEM_KINDS.contains(&item.kind) {
        return Err(format!("unknown kind {:?}", item.kind));
    }
    if !ITEM_SCOPES.contains(&item.scope) {
        return Err(format!("unknown scope {:?}", item.scope));
    }
    let (cost, cap) = (record_cost(item.text), limits.hard_cap());
    if cost > cap {
        return Err(format!("text costs {cost}, more than the unit cap {cap}"));
    }
    if item.evidence.trim().is_empty() {
        return Err("empty evidence".into());
    }
    let quoted = item.evidence.chars().count();
    if quoted > MAX_EVIDENCE_CHARS {
        return Err(format!(
            "evidence has {quoted} characters, more than {MAX_EVIDENCE_CHARS}"
        ));
    }
    if !evidence_in(unit_text, item.evidence) {
        return Err("evidence is not quoted from the unit".into());
    }
    // A quote of the unit is no longer than the unit, so a short one passes only as the whole unit.
    let quoted = plain(item.evidence).chars().count();
    if quoted < MIN_EVIDENCE_CHARS && quoted < plain(unit_text).chars().count() {
        return Err(format!(
            "evidence is shorter than {MIN_EVIDENCE_CHARS} characters"
        ));
    }
    let words = content_words(item.text);
    let shared = words.intersection(&content_words(item.evidence)).count();
    if shared < (words.len() * MIN_SHARED_PERCENT).div_ceil(100) {
        return Err(format!(
            "evidence shares {shared} of {} content words with the text",
            words.len()
        ));
    }
    let sources: Vec<&str> = std::iter::once(unit_text)
        .chain(context.iter().copied())
        .collect();
    let numbers = unsupported_numbers(item.text, &sources);
    if !numbers.is_empty() {
        return Err(format!("numbers not in the unit: {}", numbers.join(", ")));
    }
    let names = unsupported_entities(item.text, &sources);
    if !names.is_empty() {
        return Err(format!("names not in the unit: {}", names.join(", ")));
    }
    Ok(())
}

/// Whether `evidence` occurs in `unit_text` after collapsing whitespace runs to single spaces and trimming both.
/// Markdown emphasis markers (`**`, `__`, backticks) are ignored on both sides.
///
/// The match is case-sensitive, and evidence that is empty after this normalization supports nothing.
pub fn evidence_in(unit_text: &str, evidence: &str) -> bool {
    let evidence = plain(evidence);
    !evidence.is_empty() && plain(unit_text).contains(&evidence)
}

/// Share (0.0..=1.0) of `unit_text`'s characters, after the same normalization, covered by the first occurrence of
/// each evidence found in it; overlapping spans count once; 0.0 for an empty unit.
pub fn coverage(unit_text: &str, evidences: &[&str]) -> f64 {
    let unit = plain(unit_text);
    let total = unit.chars().count();
    if total == 0 {
        return 0.0;
    }
    let mut spans: Vec<(usize, usize)> = evidences
        .iter()
        .map(|evidence| plain(evidence))
        .filter(|evidence| !evidence.is_empty())
        .filter_map(|evidence| unit.find(&evidence).map(|at| (at, at + evidence.len())))
        .collect();
    spans.sort_unstable();
    let (mut covered, mut reached) = (0, 0);
    for (start, end) in spans {
        let start = start.max(reached);
        if end > start {
            covered += unit[start..end].chars().count();
            reached = end;
        }
    }
    covered as f64 / total as f64
}

/// The words that carry a text's content, each once: lowercase ASCII words of four or more letters and digits, and
/// every pair of consecutive CJK letters. CJK punctuation (`。`, `，`, `：`) splits a run as a space does.
fn content_words(text: &str) -> BTreeSet<String> {
    let mut words = BTreeSet::new();
    let mut ascii = String::new();
    let mut previous_cjk = None;
    // A trailing space ends the last ASCII word.
    for c in text.chars().chain([' ']) {
        if c.is_ascii_alphanumeric() {
            ascii.push(c.to_ascii_lowercase());
        } else if !ascii.is_empty() {
            if ascii.len() >= 4 {
                words.insert(ascii.clone());
            }
            ascii.clear();
        }
        if is_cjk(c) && c.is_alphanumeric() {
            if let Some(previous) = previous_cjk {
                words.insert(format!("{previous}{c}"));
            }
            previous_cjk = Some(c);
        } else {
            previous_cjk = None;
        }
    }
    words
}

/// Text as a quote is compared: emphasis markers removed, whitespace runs collapsed to one space, trimmed.
fn plain(text: &str) -> String {
    text.replace("**", "")
        .replace("__", "")
        .replace('`', "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const UNIT: &str = "## Deploy\n- The **release** job runs `make deploy` on 2026-05-03.\n- Alice Smith owns   the payments service.";

    fn good() -> ItemRef<'static> {
        ItemRef {
            text: "The release job runs make deploy on 2026-05-03.",
            kind: "fact",
            scope: "project",
            evidence: "The release job runs make deploy on 2026-05-03.",
        }
    }

    fn check(item: ItemRef<'_>) -> Result<(), String> {
        check_item(&item, UNIT, &[], &UnitLimits::default())
    }

    #[test]
    fn supported_items_pass() {
        // The unit's metadata may supply a name the text needs to stand alone.
        let named = ItemRef {
            text: "User says Alice Smith works with Bob Jones on payments.",
            kind: "preference",
            scope: "global",
            evidence: "Alice Smith owns the payments service.",
        };
        let limits = UnitLimits::default();
        assert_eq!(
            check_item(&named, UNIT, &["Bob Jones", "codex"], &limits),
            Ok(())
        );
    }

    #[test]
    fn each_rule_rejects_with_a_reason() {
        let item = good();
        let wide = "很".repeat(193);
        let cases = [
            (
                ItemRef {
                    text: " \n",
                    ..item
                },
                "empty text",
            ),
            (
                ItemRef {
                    kind: "transient",
                    ..item
                },
                "unknown kind \"transient\"",
            ),
            (ItemRef { scope: "", ..item }, "unknown scope \"\""),
            (
                ItemRef {
                    text: &wide,
                    ..item
                },
                "text costs 201, more than the unit cap 200",
            ),
            (
                ItemRef {
                    evidence: "  ",
                    ..item
                },
                "empty evidence",
            ),
            (
                ItemRef {
                    evidence: "release jobs",
                    ..item
                },
                "evidence is not quoted from the unit",
            ),
            (
                ItemRef {
                    evidence: "release",
                    ..item
                },
                "evidence is shorter than 8 characters",
            ),
            (
                ItemRef {
                    evidence: "Alice Smith owns the payments service.",
                    ..item
                },
                "evidence shares 0 of 5 content words with the text",
            ),
            (
                ItemRef {
                    text: "The release job runs make deploy on 2026-05-04 at 7.",
                    ..item
                },
                "numbers not in the unit: 4, 7",
            ),
            (
                ItemRef {
                    text: "The release job is owned by Bob Jones.",
                    ..item
                },
                "names not in the unit: Bob Jones",
            ),
        ];
        for (bad, reason) in cases {
            assert_eq!(check(bad), Err(reason.to_string()), "{bad:?}");
        }
        let long_quote = "x".repeat(MAX_EVIDENCE_CHARS + 1);
        assert_eq!(
            check(ItemRef {
                evidence: &long_quote,
                ..item
            }),
            Err("evidence has 601 characters, more than 600".into())
        );
    }

    #[test]
    fn evidence_must_hold_a_share_of_the_claim() {
        let unit = concat!(
            "- Alice Smith owns the payments service.\n",
            "- Use pnpm, never npm, for installs.",
        );
        let limits = UnitLimits::default();
        let check = |text, evidence| {
            let item = ItemRef {
                text,
                kind: "fact",
                scope: "project",
                evidence,
            };
            check_item(&item, unit, &[], &limits)
        };
        // A paraphrase that adds a claim to a short quote fails: 2 of 8 words, 3 needed.
        assert_eq!(
            check(
                "Alice Smith is responsible for maintaining and operating the payments service in production.",
                "Alice Smith owns",
            ),
            Err("evidence shares 2 of 8 content words with the text".into())
        );
        // A self-contained rewrite that names the project passes at 30%: 3 of 7 words, 3 needed.
        assert_eq!(
            check(
                "In the orchard project, installs of packages always use pnpm and never npm.",
                "Use pnpm, never npm, for installs.",
            ),
            Ok(())
        );
        // A text equal to its evidence always passes, and a unit shorter than 8 characters may be quoted whole.
        let quote = "Alice Smith owns the payments service.";
        assert_eq!(check(quote, quote), Ok(()));
        let whole = ItemRef {
            text: "Always use pnpm.",
            kind: "preference",
            scope: "project",
            evidence: "pnpm",
        };
        assert_eq!(check_item(&whole, "pnpm", &[], &limits), Ok(()));
    }

    #[test]
    fn content_words_are_long_ascii_words_and_cjk_pairs() {
        let words = |text| content_words(text).into_iter().collect::<Vec<_>>();
        assert_eq!(
            words("Use **pnpm** in 2026, not npm; use_pnpm!"),
            ["2026", "pnpm"]
        );
        assert_eq!(words("共享记忆，库"), ["享记", "共享", "记忆"]);
    }

    #[test]
    fn evidence_matches_across_whitespace_and_emphasis() {
        assert!(evidence_in(UNIT, "runs make deploy"));
        assert!(evidence_in(
            UNIT,
            " The **release**   job\nruns `make deploy` "
        ));
        assert!(evidence_in("Use __bold__ here", "Use bold here"));
        assert!(!evidence_in(UNIT, "alice smith"));
        assert!(!evidence_in(UNIT, " ** "));
    }

    #[test]
    fn coverage_counts_overlapping_quotes_once() {
        let unit = "abcdefghij";
        assert_eq!(coverage(unit, &["abcde", "cdefg"]), 0.7);
        assert_eq!(coverage(unit, &["abc", "abc", "zzz", ""]), 0.3);
        assert_eq!(coverage(unit, &["abcdefghij", "def", "hij"]), 1.0);
        assert_eq!(coverage(" ** ", &["a"]), 0.0);
        // Characters, not bytes, of the normalized unit: "共享 记忆库" has six.
        assert_eq!(coverage("共享  **记忆**库", &["记忆库"]), 0.5);
    }
}
