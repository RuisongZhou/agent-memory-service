//! Deterministic semantic-only ranking used by the pinned Mem0 strategy.

pub fn rank(
    mut candidates: Vec<(String, f32)>,
    threshold: f32,
    limit: usize,
) -> Vec<(String, f32)> {
    candidates.retain(|(_, score)| score.is_finite() && *score >= threshold);
    candidates.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    candidates.truncate(limit);
    candidates
}

/// Content identity includes the visibility scope and ingestion mode.
pub fn content_key(project: &str, text: &str, inferred: bool) -> String {
    let mut hash = blake3::Hasher::new();
    for part in [
        project.as_bytes(),
        text.as_bytes(),
        if inferred { b"inferred" } else { b"raw" },
    ] {
        hash.update(&(part.len() as u64).to_le_bytes());
        hash.update(part);
    }
    hash.finalize().to_hex().to_string()
}

/// Current extraction input remains complete; clipping applies only to historical prompt views.
pub fn format_new_messages<'a>(messages: impl IntoIterator<Item = (&'a str, &'a str)>) -> String {
    messages
        .into_iter()
        .map(|(role, content)| format!("{role}: {content}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A segment's turns as one New Messages section: each turn's messages after a `[Turn i of n]` line. A single turn
/// keeps the per-turn form unchanged.
pub fn format_turns(turns: &[Vec<(&str, &str)>]) -> String {
    if let [turn] = turns {
        return format_new_messages(turn.iter().copied());
    }
    turns
        .iter()
        .enumerate()
        .map(|(i, turn)| {
            format!(
                "[Turn {} of {}]\n{}",
                i + 1,
                turns.len(),
                format_new_messages(turn.iter().copied())
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Format the most recent ten messages in chronological order. Input must already be
/// filtered to eligible conversation batches and ordered oldest first. Unicode scalar
/// truncation follows Node SDK's 300 UTF-16-unit limit, keeping complete scalars when
/// the limit falls between a surrogate pair. Persisted source input is unchanged.
pub fn format_history_messages<'a>(
    messages: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> String {
    let messages: Vec<_> = messages.into_iter().collect();
    messages
        .iter()
        .skip(messages.len().saturating_sub(10))
        .filter(|(role, content)| !role.is_empty() && !content.is_empty())
        .map(|(role, content)| {
            let mut units = 0;
            let clipped: String = content
                .chars()
                .take_while(|ch| {
                    units += ch.len_utf16();
                    units <= 300
                })
                .collect();
            let suffix = if clipped.len() < content.len() {
                "..."
            } else {
                ""
            };
            format!("{role}: {clipped}{suffix}\n")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_history_messages_consume_window_slots_but_emit_no_line() {
        let messages = [
            ("user", "outside-window"),
            ("user", "oldest-in-window"),
            ("user", ""),
            ("", "missing-role"),
            ("user", "4"),
            ("user", "5"),
            ("user", "6"),
            ("user", "7"),
            ("user", "8"),
            ("user", "9"),
            ("assistant", "10"),
        ];
        assert_eq!(format_history_messages(messages),
            "user: oldest-in-window\nuser: 4\nuser: 5\nuser: 6\nuser: 7\nuser: 8\nuser: 9\nassistant: 10\n");
        // Current input is parsed in full even when textual content is empty.
        assert_eq!(format_new_messages([("user", "")]), "user: ");
        // A segment numbers its turns; one turn reads as it does when formed alone.
        assert_eq!(
            format_turns(&[vec![("user", "a"), ("assistant", "b")]]),
            "user: a\nassistant: b"
        );
        assert_eq!(
            format_turns(&[
                vec![("user", "a"), ("assistant", "b")],
                vec![("assistant", "c")]
            ]),
            "[Turn 1 of 2]\nuser: a\nassistant: b\n[Turn 2 of 2]\nassistant: c"
        );
    }

    #[test]
    fn current_unicode_message_is_complete_but_history_is_clipped() {
        let text = format!("{}尾部", "😀汉".repeat(160));
        let original = text.clone();
        assert_eq!(
            format_new_messages([("user", text.as_str())]),
            format!("user: {text}")
        );
        assert_eq!(
            format_history_messages([("user", text.as_str())]),
            format!("user: {}...\n", "😀汉".repeat(100))
        );
        assert_eq!(text, original);
        let boundary = "汉".repeat(300);
        assert_eq!(
            format_history_messages([("assistant", boundary.as_str())]),
            format!("assistant: {boundary}\n")
        );
    }

    #[test]
    fn utf16_boundary_never_emits_a_partial_surrogate() {
        let text = format!("{}😀tail", "a".repeat(299));
        assert_eq!(
            format_history_messages([("user", text.as_str())]),
            format!("user: {}...\n", "a".repeat(299))
        );
        let text = "😀".repeat(150);
        assert_eq!(
            format_history_messages([("user", text.as_str())]),
            format!("user: {text}\n")
        );
        assert_eq!(
            format_new_messages([("user", "a"), ("assistant", "b")]),
            "user: a\nassistant: b"
        );
    }

    #[test]
    fn history_keeps_last_ten_messages_in_chronological_order() {
        let contents: Vec<_> = (0..14).map(|i| format!("message-{i}")).collect();
        let formatted = format_history_messages(contents.iter().map(|s| ("user", s.as_str())));
        let expected = contents[4..]
            .iter()
            .map(|s| format!("user: {s}\n"))
            .collect::<String>();
        assert_eq!(formatted, expected);
        assert_eq!(format_history_messages([]), "");
    }

    #[test]
    fn ranking_filters_before_limiting_and_has_stable_ties() {
        let result = rank(
            vec![("b".into(), 0.8), ("a".into(), 0.8), ("c".into(), 0.09)],
            0.1,
            10,
        );
        assert_eq!(result, vec![("a".into(), 0.8), ("b".into(), 0.8)]);
    }

    #[test]
    fn identity_separates_scopes_and_inferred_records() {
        assert_ne!(
            content_key("a", "same", true),
            content_key("b", "same", true)
        );
        assert_ne!(
            content_key("a", "same", true),
            content_key("a", "same", false)
        );
    }
}
