//! Markdown structure of host memory files: frontmatter, ATX sections, list items, paragraphs and
//! sentences. Hand-rolled for the shapes host notes use, not a YAML or CommonMark parser. Fenced code
//! blocks are respected everywhere; lines split as `str::lines` does, so `\r\n` endings are accepted.
use crate::util::is_cjk;

/// Frontmatter fields of a host note; every other key is ignored.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frontmatter {
    /// `name`, top level only.
    pub name: Option<String>,
    /// `description`, top level only.
    pub description: Option<String>,
    /// `type`, at the top level or under `metadata:` (Claude Code auto memory: user | feedback | project | reference).
    pub kind: Option<String>,
    /// `modified` (ISO 8601 write time Claude Code records), top level or under `metadata:`.
    pub modified: Option<String>,
    /// `originSessionId`, top level or under `metadata:`.
    pub origin_session_id: Option<String>,
}

/// Split a leading `---` … `---` block from `raw`. Hand-rolled for exactly this shape (no YAML): `key: value` lines,
/// one indentation level under `metadata:`, surrounding single or double quotes removed, unknown keys ignored.
/// No opening line, or no closing `---` line: empty frontmatter and the whole text as body.
/// The body excludes the block and its leading blank lines. Accept `\r\n` line endings.
///
/// A top-level key wins over the same key under `metadata:` and a repeated key keeps its first value; empty
/// values are `None`. Quoted values undo YAML's `\"`/`\\` (double quotes) and `''` (single quotes) escapes.
/// Comment lines are skipped and a leading byte-order mark is ignored.
pub fn split_frontmatter(raw: &str) -> (Frontmatter, &str) {
    let text = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    let mut offset = 0;
    let mut block = Vec::new();
    let mut closed = false;
    for (index, line) in text.split_inclusive('\n').enumerate() {
        offset += line.len();
        if index == 0 {
            if !is_delimiter(line) {
                break;
            }
        } else if is_delimiter(line) {
            closed = true;
            break;
        } else {
            block.push(line);
        }
    }
    if !closed {
        return (Frontmatter::default(), raw);
    }
    let rest = &text[offset..];
    let blank: usize = rest
        .split_inclusive('\n')
        .take_while(|line| line.trim().is_empty())
        .map(str::len)
        .sum();
    (parse_block(&block), &rest[blank..])
}

fn is_delimiter(line: &str) -> bool {
    line.trim_end() == "---"
}

fn parse_block(lines: &[&str]) -> Frontmatter {
    let (mut top, mut meta) = (Frontmatter::default(), Frontmatter::default());
    let mut in_metadata = false;
    // The first indented line under `metadata:` fixes its one accepted indentation level.
    let mut metadata_indent = None;
    for line in lines {
        let entry = line.trim();
        if entry.is_empty() || entry.starts_with('#') {
            continue;
        }
        let Some((key, value)) = entry.split_once(':') else {
            continue;
        };
        let (key, value) = (key.trim(), unquote(value.trim()));
        let indent = line.len() - line.trim_start().len();
        if indent == 0 {
            in_metadata = key == "metadata" && value.is_empty();
            metadata_indent = None;
            set_field(&mut top, key, value, true);
        } else if in_metadata && *metadata_indent.get_or_insert(indent) == indent {
            set_field(&mut meta, key, value, false);
        }
    }
    Frontmatter {
        name: top.name,
        description: top.description,
        kind: top.kind.or(meta.kind),
        modified: top.modified.or(meta.modified),
        origin_session_id: top.origin_session_id.or(meta.origin_session_id),
    }
}

fn set_field(fields: &mut Frontmatter, key: &str, value: String, top_level: bool) {
    let slot = match key {
        "name" if top_level => &mut fields.name,
        "description" if top_level => &mut fields.description,
        "type" => &mut fields.kind,
        "modified" => &mut fields.modified,
        "originSessionId" => &mut fields.origin_session_id,
        _ => return,
    };
    if slot.is_none() && !value.is_empty() {
        *slot = Some(value);
    }
}

/// `value` without one pair of surrounding quotes.
fn unquote(value: &str) -> String {
    let quoted = |q: char| value.len() >= 2 && value.starts_with(q) && value.ends_with(q);
    if quoted('"') {
        let mut out = String::new();
        let mut chars = value[1..value.len() - 1].chars();
        while let Some(c) = chars.next() {
            match (c, chars.clone().next()) {
                ('\\', Some(escaped @ ('"' | '\\'))) => {
                    out.push(escaped);
                    chars.next();
                }
                _ => out.push(c),
            }
        }
        out
    } else if quoted('\'') {
        value[1..value.len() - 1].replace("''", "'")
    } else {
        value.to_string()
    }
}

/// One heading section of a Markdown body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// 0 for text before the first heading, else the heading level (1..=max_level).
    pub level: usize,
    /// Heading text without `#` marks and surrounding whitespace; empty for level 0.
    pub heading: String,
    /// Headings of the enclosing sections, outermost first, ending with this one; empty for level 0.
    pub path: Vec<String>,
    /// Text after the heading line up to the next heading of level <= max_level (deeper headings stay inside),
    /// trimmed of leading/trailing blank lines.
    pub body: String,
    /// 1-based line number of the heading line (1 for level 0).
    pub line: usize,
}

/// Sections in document order, split at ATX headings (`#`..`######` followed by a space) of level <= max_level.
/// Lines inside fenced code blocks (``` or ~~~) are never headings. A level-0 section is returned only when the
/// text before the first heading is not blank. Sections with an empty body are still returned.
///
/// Headings start in the first column; a closing run of `#` is dropped. Duplicate headings give sections with
/// equal paths, so a locator has to add something else (an index or `line`) to tell them apart. `line` counts
/// lines of `body`, not of the file the frontmatter was split from. Bodies use `\n` line endings.
pub fn sections(body: &str, max_level: usize) -> Vec<Section> {
    let mut out = Vec::new();
    let mut open: Vec<(usize, String)> = Vec::new();
    let mut current = Section {
        level: 0,
        heading: String::new(),
        path: Vec::new(),
        body: String::new(),
        line: 1,
    };
    let mut lines: Vec<&str> = Vec::new();
    let mut fence: Option<Fence> = None;
    for (index, line) in body.lines().enumerate() {
        if let Some(open_fence) = fence {
            if open_fence.closed_by(line) {
                fence = None;
            }
        } else if let Some(opened) = Fence::opened_by(line) {
            fence = Some(opened);
        } else if let Some((level, heading)) = atx_heading(line).filter(|(l, _)| *l <= max_level) {
            close_section(&mut out, current, &lines);
            lines.clear();
            while open.last().is_some_and(|(l, _)| *l >= level) {
                open.pop();
            }
            open.push((level, heading.clone()));
            current = Section {
                level,
                heading,
                path: open.iter().map(|(_, h)| h.clone()).collect(),
                body: String::new(),
                line: index + 1,
            };
            continue;
        }
        lines.push(line);
    }
    close_section(&mut out, current, &lines);
    out
}

fn close_section(out: &mut Vec<Section>, mut section: Section, lines: &[&str]) {
    section.body = trim_blank_lines(&lines.join("\n")).to_string();
    if section.level > 0 || !section.body.is_empty() {
        out.push(section);
    }
}

/// Level and text of an ATX heading line.
fn atx_heading(line: &str) -> Option<(usize, String)> {
    let level = line.bytes().take_while(|&b| b == b'#').count();
    if !(1..=6).contains(&level) || !line[level..].starts_with([' ', '\t']) {
        return None;
    }
    let text = line[level..].trim();
    // A closing sequence is a run of `#` that is the whole text or follows whitespace ("C#" keeps its mark).
    let open = text.trim_end_matches('#');
    let text = if open.is_empty() || open.ends_with([' ', '\t']) {
        open.trim_end()
    } else {
        text
    };
    Some((level, text.to_string()))
}

/// `text` without leading and trailing blank lines; the first kept line keeps its indentation.
fn trim_blank_lines(text: &str) -> &str {
    let text = text.trim_end();
    let lead: usize = text
        .split_inclusive('\n')
        .take_while(|line| line.trim().is_empty())
        .map(str::len)
        .sum();
    &text[lead..]
}

/// An open fenced code block: its marker byte and how many of them opened it.
#[derive(Debug, Clone, Copy)]
struct Fence {
    marker: u8,
    len: usize,
}

impl Fence {
    /// Three or more backticks or tildes, at any indentation since fences inside list items are indented.
    /// A backtick fence's info string cannot contain a backtick (CommonMark), so inline code is no fence.
    fn opened_by(line: &str) -> Option<Fence> {
        let text = line.trim_start();
        let marker = *text.as_bytes().first()?;
        if marker != b'`' && marker != b'~' {
            return None;
        }
        let len = text.bytes().take_while(|&b| b == marker).count();
        (len >= 3 && !(marker == b'`' && text[len..].contains('`')))
            .then_some(Fence { marker, len })
    }

    /// Only a run of the same marker, at least as long, closes the fence.
    fn closed_by(&self, line: &str) -> bool {
        let text = line.trim();
        text.len() >= self.len && text.bytes().all(|b| b == self.marker)
    }
}

/// Top-level list items (`- `, `* `, `+ `, `1. `/`1) `) of `text`, each with its continuation and nested lines,
/// markers of the top-level item removed, trimmed. Text outside lists is not returned.
///
/// A line indented past the item's marker belongs to the item, also after blank lines (nested items, further
/// paragraphs, indented code); continuation lines lose the item's content indentation, so nested markers stay
/// relative. An unindented line directly after an item line continues it lazily (CommonMark) unless it starts a
/// block (heading, fence, thematic break, quote) or another item. After a blank line an unindented line that is no
/// item marker ends the list, while another marker continues it. Items that are empty after trimming are skipped.
pub fn list_items(text: &str) -> Vec<String> {
    walk(text)
        .into_iter()
        .filter_map(|(item, block)| item.then_some(block))
        .collect()
}

/// Blank-line separated blocks, trimmed; a fenced code block stays one block even if it contains blank lines.
pub fn paragraphs(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut paragraph = Paragraph::default();
    for line in text.lines() {
        if let Some(done) = paragraph.take(line) {
            push_block(&mut out, done);
        }
    }
    push_block(&mut out, paragraph.finish());
    out
}

/// `list_items` and the paragraphs of the text around the lists, in document order: every non-blank line of
/// `text` is in exactly one block. The deterministic splitter cuts at these blocks.
pub(crate) fn blocks(text: &str) -> Vec<String> {
    walk(text).into_iter().map(|(_, block)| block).collect()
}

/// List items (`true`) and paragraphs outside lists (`false`), in document order, without empty blocks.
fn walk(text: &str) -> Vec<(bool, String)> {
    let mut out = Vec::new();
    let mut push = |item: bool, block: String| {
        if !block.is_empty() {
            out.push((item, block));
        }
    };
    let mut paragraph = Paragraph::default();
    let mut item: Option<Item> = None;
    for line in text.lines() {
        if let Some(open) = item.as_mut() {
            if open.take(line) {
                continue;
            }
            push(true, open.text());
            item = None;
        }
        if !paragraph.in_fence() {
            if let Some(marker) = list_marker(line).filter(|m| m.indent <= 3) {
                push(false, paragraph.finish());
                item = Some(Item::new(&marker));
                continue;
            }
        }
        if let Some(done) = paragraph.take(line) {
            push(false, done);
        }
    }
    if let Some(open) = item {
        push(true, open.text());
    }
    push(false, paragraph.finish());
    out
}

fn push_block(out: &mut Vec<String>, block: String) {
    if !block.is_empty() {
        out.push(block);
    }
}

/// The paragraph being collected: a blank line outside a fence ends it.
#[derive(Default)]
struct Paragraph<'a> {
    lines: Vec<&'a str>,
    fence: Option<Fence>,
}

impl<'a> Paragraph<'a> {
    fn in_fence(&self) -> bool {
        self.fence.is_some()
    }

    /// Add `line`; a blank line outside a fence returns the finished paragraph instead.
    fn take(&mut self, line: &'a str) -> Option<String> {
        match self.fence {
            Some(fence) => {
                if fence.closed_by(line) {
                    self.fence = None;
                }
            }
            None if line.trim().is_empty() => return Some(self.finish()),
            None => self.fence = Fence::opened_by(line),
        }
        self.lines.push(line);
        None
    }

    fn finish(&mut self) -> String {
        let text = self.lines.join("\n").trim().to_string();
        self.lines.clear();
        text
    }
}

/// A list item marker line.
struct Marker<'a> {
    /// Indentation of the marker, in columns.
    indent: usize,
    /// Column where the item's content starts.
    content: usize,
    /// The content on the marker line.
    rest: &'a str,
}

fn list_marker(line: &str) -> Option<Marker<'_>> {
    if is_thematic_break(line) {
        return None;
    }
    let body = line.trim_start_matches([' ', '\t']);
    let bytes = body.as_bytes();
    let width = match bytes.first()? {
        b'-' | b'*' | b'+' => 1,
        b'0'..=b'9' => {
            let digits = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
            if digits > 9 || !matches!(bytes.get(digits), Some(b'.' | b')')) {
                return None;
            }
            digits + 1
        }
        _ => return None,
    };
    let after = &body[width..];
    let rest = after.trim_start_matches([' ', '\t']);
    let gap = after.len() - rest.len();
    if gap == 0 {
        return None;
    }
    // Past four spaces the content is indented code, which starts one column after the marker.
    let gap = if gap > 4 || rest.is_empty() { 1 } else { gap };
    let indent = indent_width(line);
    Some(Marker {
        indent,
        content: indent + width + gap,
        rest,
    })
}

/// Three or more `-`, `*` or `_` (the same one), optionally spaced: `* * *` is a rule, not an item.
fn is_thematic_break(line: &str) -> bool {
    let mut marks = line.chars().filter(|c| !matches!(c, ' ' | '\t'));
    let Some(first @ ('-' | '*' | '_')) = marks.next() else {
        return false;
    };
    let mut count = 1;
    for c in marks {
        if c != first {
            return false;
        }
        count += 1;
    }
    count >= 3
}

/// Lines that start a block of their own and so cannot continue a list item lazily.
fn starts_block(line: &str) -> bool {
    let text = line.trim_start();
    atx_heading(text).is_some()
        || Fence::opened_by(text).is_some()
        || is_thematic_break(text)
        || text.starts_with('>')
}

/// Columns of leading whitespace; a tab advances to the next multiple of four.
fn indent_width(line: &str) -> usize {
    let mut col = 0;
    for c in line.chars() {
        match c {
            ' ' => col += 1,
            '\t' => col += 4 - col % 4,
            _ => break,
        }
    }
    col
}

/// `line` without up to `columns` columns of leading whitespace.
fn dedent(line: &str, columns: usize) -> &str {
    let mut col = 0;
    for (i, c) in line.char_indices() {
        if col >= columns {
            return &line[i..];
        }
        match c {
            ' ' => col += 1,
            '\t' => col += 4 - col % 4,
            _ => return &line[i..],
        }
    }
    ""
}

/// The list item being collected.
struct Item {
    /// Indentation of its marker: lines indented deeper belong to the item.
    base: usize,
    /// Column of its content, removed from continuation lines.
    content: usize,
    lines: Vec<String>,
    /// Blank lines since its last line, kept only if the item continues after them.
    blanks: usize,
    fence: Option<Fence>,
}

impl Item {
    fn new(marker: &Marker<'_>) -> Item {
        Item {
            base: marker.indent,
            content: marker.content,
            lines: vec![marker.rest.to_string()],
            blanks: 0,
            fence: Fence::opened_by(marker.rest),
        }
    }

    /// Add `line` if it belongs to the item; false ends the item without consuming the line.
    fn take(&mut self, line: &str) -> bool {
        if let Some(fence) = self.fence {
            if fence.closed_by(line) {
                self.fence = None;
            }
            self.push(dedent(line, self.content));
            return true;
        }
        if line.trim().is_empty() {
            self.blanks += 1;
            return true;
        }
        if indent_width(line) > self.base {
            self.fence = Fence::opened_by(line);
            self.push(dedent(line, self.content));
            return true;
        }
        if self.blanks == 0 && list_marker(line).is_none() && !starts_block(line) {
            self.push(line.trim_start());
            return true;
        }
        false
    }

    fn push(&mut self, line: &str) {
        self.lines
            .extend(std::iter::repeat_n(String::new(), self.blanks));
        self.blanks = 0;
        self.lines.push(line.to_string());
    }

    fn text(&self) -> String {
        self.lines.join("\n").trim().to_string()
    }
}

/// Sentence terminators that need no following space.
const WIDE_STOPS: [char; 4] = ['。', '！', '？', '；'];
/// Sentence terminators that count only before whitespace or the end ("v1.2" and "e.g.," stay whole).
const STOPS: [char; 4] = ['.', '!', '?', ';'];
/// Closing quotes and brackets that stay with the terminator before them.
const CLOSERS: [char; 16] = [
    ')', ']', '}', '"', '\'', '”', '’', '」', '』', '）', '》', '】', '〉', '〕', '］', '｝',
];

/// Sentences of `text`: split after `。！？；` (no space needed) and after `.!?;` followed by whitespace or end.
/// Keeps the punctuation, trims, drops empty pieces; text without terminators is one sentence.
///
/// A run of terminators and closing quotes or brackets ("。”", "?!", "...", ".)") ends one sentence together.
/// Line breaks without a terminator do not split.
pub fn sentences(text: &str) -> Vec<String> {
    sentence_spans(text)
        .into_iter()
        .map(|(start, end)| text[start..end].to_string())
        .collect()
}

/// Byte ranges of `sentences(text)` in `text`, so callers can rejoin neighbours with the original separators.
pub(crate) fn sentence_spans(text: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let mut wide = WIDE_STOPS.contains(&c);
        if !wide && !STOPS.contains(&c) {
            continue;
        }
        let mut end = i + c.len_utf8();
        while let Some(&(j, d)) = chars.peek() {
            if WIDE_STOPS.contains(&d) {
                wide = true;
            } else if !STOPS.contains(&d) && !CLOSERS.contains(&d) {
                break;
            }
            end = j + d.len_utf8();
            chars.next();
        }
        if wide || text[end..].chars().next().is_none_or(char::is_whitespace) {
            push_span(&mut spans, text, start, end);
            start = end;
        }
    }
    push_span(&mut spans, text, start, text.len());
    spans
}

fn push_span(spans: &mut Vec<(usize, usize)>, text: &str, start: usize, end: usize) {
    let piece = &text[start..end];
    let trimmed = piece.trim();
    if !trimmed.is_empty() {
        let lead = piece.len() - piece.trim_start().len();
        spans.push((start + lead, start + lead + trimmed.len()));
    }
}

/// Longest slug, in characters.
const MAX_SLUG_CHARS: usize = 48;

/// Stable locator slug of a heading path component: lowercase; ASCII letters/digits and CJK characters kept; every
/// other run of characters becomes one `-`; trimmed of `-`; at most 48 characters; empty input gives "section".
///
/// CJK characters are ideographs, kana and hangul: `util::is_cjk` without its CJK punctuation and full-width
/// forms blocks, so `（`, `。` and `！` separate words like ASCII punctuation. Input that keeps no character (only
/// punctuation or whitespace) also gives "section".
pub fn slug(text: &str) -> String {
    let mut out = String::new();
    let mut gap = false;
    for c in text.chars().map(|c| c.to_ascii_lowercase()) {
        if c.is_ascii_alphanumeric() || is_cjk_letter(c) {
            if gap && !out.is_empty() {
                out.push('-');
            }
            gap = false;
            out.push(c);
        } else {
            gap = true;
        }
    }
    let cut: String = out.chars().take(MAX_SLUG_CHARS).collect();
    match cut.trim_end_matches('-') {
        "" => "section".to_string(),
        slug => slug.to_string(),
    }
}

/// `util::is_cjk` without CJK punctuation (keeping 々〆〇), full-width forms and the kana punctuation ゠・.
fn is_cjk_letter(c: char) -> bool {
    is_cjk(c)
        && !matches!(
            c as u32,
            0x3000..=0x3004 | 0x3008..=0x303F | 0xFF00..=0xFFEF | 0x30A0 | 0x30FB
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_reads_top_level_and_metadata_keys() {
        let raw = concat!(
            "---\n",
            "name: Deploy notes\n",
            "description: \"Release steps, \\\"quoted\\\" \\\\ kept\"\n",
            "# a comment line\n",
            "metadata:\n",
            "  nested:\n",
            "    type: deeper levels are ignored\n",
            "  node_type: memory\n",
            "  type: feedback\n",
            "  originSessionId: 'session-0001'\n",
            "  modified: 2026-10-01T08:00:00Z\n",
            "unknown: ignored\n",
            "---\n",
            "\n",
            "  \n",
            "Body line.\n",
        );
        let (fm, body) = split_frontmatter(raw);
        assert_eq!(
            fm,
            Frontmatter {
                name: Some("Deploy notes".into()),
                description: Some("Release steps, \"quoted\" \\ kept".into()),
                kind: Some("feedback".into()),
                modified: Some("2026-10-01T08:00:00Z".into()),
                origin_session_id: Some("session-0001".into()),
            }
        );
        assert_eq!(body, "Body line.\n");
    }

    #[test]
    fn top_level_keys_win_and_first_values_stay() {
        let raw = concat!(
            "---\n",
            "type: project\n",
            "metadata:\n",
            "  type: user\n",
            "  modified: 2026-01-02\n",
            "  name: not a metadata field\n",
            "modified: 2026-03-04\n",
            "name: 'it''s'\n",
            "name: second\n",
            "description:\n",
            "---\n",
            "body",
        );
        let (fm, body) = split_frontmatter(raw);
        assert_eq!(fm.kind.as_deref(), Some("project"));
        assert_eq!(fm.modified.as_deref(), Some("2026-03-04"));
        assert_eq!(fm.name.as_deref(), Some("it's"));
        assert_eq!(fm.description, None);
        assert_eq!(fm.origin_session_id, None);
        assert_eq!(body, "body");
    }

    #[test]
    fn frontmatter_needs_both_delimiters_and_accepts_crlf() {
        let unclosed = "---\nname: x\nno closing line\n";
        assert_eq!(
            split_frontmatter(unclosed),
            (Frontmatter::default(), unclosed)
        );
        let late = "# Title\n---\nname: x\n---\n";
        assert_eq!(split_frontmatter(late), (Frontmatter::default(), late));
        assert_eq!(split_frontmatter(""), (Frontmatter::default(), ""));
        let crlf = "\u{feff}---\r\nname: \"Windows note\"\r\nmetadata:\r\n  type: reference\r\n---  \r\n\r\nLine one.\r\nLine two.\r\n";
        let (fm, body) = split_frontmatter(crlf);
        assert_eq!(fm.name.as_deref(), Some("Windows note"));
        assert_eq!(fm.kind.as_deref(), Some("reference"));
        assert_eq!(body, "Line one.\r\nLine two.\r\n");
        assert_eq!(
            split_frontmatter("---\n---\n\n"),
            (Frontmatter::default(), "")
        );
    }

    #[test]
    fn sections_fold_deeper_headings_and_skip_code_fences() {
        let body = concat!(
            "# Summary\n",
            "intro line\n",
            "\n",
            "```sh\n",
            "# not a heading\n",
            "## nor this\n",
            "```\n",
            "## Task 1: Build\n",
            "### Steps\n",
            "- run make\n",
            "\n",
            "## Task 2 ##\n",
            "## Task 2 ##\n",
        );
        let found = sections(body, 2);
        let heads: Vec<_> = found
            .iter()
            .map(|s| (s.level, s.heading.as_str(), s.line))
            .collect();
        assert_eq!(
            heads,
            vec![
                (1, "Summary", 1),
                (2, "Task 1: Build", 8),
                (2, "Task 2", 12),
                (2, "Task 2", 13)
            ]
        );
        assert_eq!(
            found[0].body,
            "intro line\n\n```sh\n# not a heading\n## nor this\n```"
        );
        assert_eq!(found[1].body, "### Steps\n- run make");
        assert_eq!(found[1].path, vec!["Summary", "Task 1: Build"]);
        // Duplicate headings stay separate sections with equal paths; empty bodies are kept.
        assert_eq!(found[2].path, found[3].path);
        assert_eq!((found[2].body.as_str(), found[3].body.as_str()), ("", ""));
    }

    #[test]
    fn sections_return_a_preamble_and_nested_paths() {
        let body = "\nthread_id: demo-0001\nupdated_at: 2026-01-02\n\n# Rollout\n## Task 1\n### Detail\ntext\n# Other\n";
        let found = sections(body, 3);
        assert_eq!(
            found[0],
            Section {
                level: 0,
                heading: String::new(),
                path: vec![],
                body: "thread_id: demo-0001\nupdated_at: 2026-01-02".into(),
                line: 1,
            }
        );
        let paths: Vec<_> = found[1..].iter().map(|s| s.path.join(" > ")).collect();
        assert_eq!(
            paths,
            vec![
                "Rollout",
                "Rollout > Task 1",
                "Rollout > Task 1 > Detail",
                "Other"
            ]
        );
        assert_eq!(found[3].body, "text");
        assert_eq!(found[3].line, 7);
        // A blank preamble is no section; max_level 0 keeps everything in one level-0 section.
        assert_eq!(
            sections("\n  \n# Only\n", 2)
                .iter()
                .map(|s| s.level)
                .collect::<Vec<_>>(),
            vec![1]
        );
        assert!(sections("", 2).is_empty());
        let flat = sections("# A\ntext\n", 0);
        assert_eq!(flat.len(), 1);
        assert_eq!((flat[0].level, flat[0].body.as_str()), (0, "# A\ntext"));
    }

    #[test]
    fn heading_syntax_and_fence_markers() {
        let text = "#hashtag\n####### seven\n# C# notes #\n~~~\n# in tilde fence\n```\n# still fenced\n~~~\n## After ##\r\nbody\r\n";
        let found = sections(text, 6);
        let heads: Vec<_> = found
            .iter()
            .map(|s| (s.level, s.heading.as_str()))
            .collect();
        assert_eq!(heads, vec![(0, ""), (1, "C# notes"), (2, "After")]);
        assert_eq!(found[0].body, "#hashtag\n####### seven");
        // Backticks do not close a tilde fence.
        assert_eq!(
            found[1].body,
            "~~~\n# in tilde fence\n```\n# still fenced\n~~~"
        );
        assert_eq!(found[2].body, "body");
        // Inline code with three backticks opens no fence.
        let inline = sections("```code``` inline\n# Real\n", 1);
        assert_eq!(inline.last().map(|s| s.heading.as_str()), Some("Real"));
    }

    #[test]
    fn list_items_keep_continuations_and_nested_lines() {
        let text = concat!(
            "Intro paragraph.\n",
            "- first item\n",
            "  continues here\n",
            "  - nested one\n",
            "    - nested two\n",
            "* second\n",
            "wrapped lazily\n",
            "\n",
            "1. third\n",
            "\n",
            "   second paragraph of third\n",
            "2) fourth\n",
            "\n",
            "Outside text.\n",
            "- [ ] task item\n",
            "- \n",
        );
        assert_eq!(
            list_items(text),
            vec![
                "first item\ncontinues here\n- nested one\n  - nested two",
                "second\nwrapped lazily",
                "third\n\nsecond paragraph of third",
                "fourth",
                "[ ] task item",
            ]
        );
        assert_eq!(
            blocks(text),
            vec![
                "Intro paragraph.",
                "first item\ncontinues here\n- nested one\n  - nested two",
                "second\nwrapped lazily",
                "third\n\nsecond paragraph of third",
                "fourth",
                "Outside text.",
                "[ ] task item",
            ]
        );
    }

    #[test]
    fn list_items_respect_fences_rules_and_block_starts() {
        let text = concat!(
            "```\n",
            "- not an item\n",
            "\n",
            "- still code\n",
            "```\n",
            "* * *\n",
            "---\n",
            "**bold** line\n",
            "- code:\n",
            "  ```\n",
            "  - inside the fence\n",
            "\n",
            "  ```\n",
            "- before a heading\n",
            "# Heading\n",
            "+ last\n",
        );
        assert_eq!(
            list_items(text),
            vec![
                "code:\n```\n- inside the fence\n\n```",
                "before a heading",
                "last"
            ]
        );
        assert!(list_items("plain text\n-1 is negative\n+1 agreed\n").is_empty());
    }

    #[test]
    fn paragraphs_keep_fenced_blocks_whole() {
        let text = "First line\nsecond line\n\n\n```text\nalpha\n\nbeta\n```\n  \nlast";
        assert_eq!(
            paragraphs(text),
            vec![
                "First line\nsecond line",
                "```text\nalpha\n\nbeta\n```",
                "last"
            ]
        );
        assert!(paragraphs(" \n\n").is_empty());
    }

    #[test]
    fn sentences_split_chinese_english_and_mixed_text() {
        assert_eq!(
            sentences("今天下雨。明天晴！后天呢？先这样；好"),
            vec!["今天下雨。", "明天晴！", "后天呢？", "先这样；", "好"]
        );
        assert_eq!(
            sentences("Use v1.2 now. Then run it!  Done? yes; e.g.,x ok"),
            vec![
                "Use v1.2 now.",
                "Then run it!",
                "Done?",
                "yes;",
                "e.g.,x ok"
            ]
        );
        assert_eq!(
            sentences("完成了。Next step is X. 然后继续"),
            vec!["完成了。", "Next step is X.", "然后继续"]
        );
        // Terminator runs and closing quotes or brackets stay with their sentence.
        assert_eq!(
            sentences("他说：“好。”然后走了。。Wait... (see A.) Next?!"),
            vec![
                "他说：“好。”",
                "然后走了。。",
                "Wait...",
                "(see A.)",
                "Next?!"
            ]
        );
        assert_eq!(
            sentences("  no terminator\nacross lines  "),
            vec!["no terminator\nacross lines"]
        );
        assert!(sentences(" \n ").is_empty());
        let text = "One. Two。三";
        let spans = sentence_spans(text);
        assert_eq!(
            spans.iter().map(|&(a, b)| &text[a..b]).collect::<Vec<_>>(),
            vec!["One.", "Two。", "三"]
        );
    }

    #[test]
    fn slugs_are_stable_locators() {
        assert_eq!(slug("Task 1: Fix the Build!"), "task-1-fix-the-build");
        assert_eq!(slug("发布流程（第二版）"), "发布流程-第二版");
        assert_eq!(slug("  メモ・메모 "), "メモ-메모");
        assert_eq!(slug("佐々木 notes"), "佐々木-notes");
        assert_eq!(slug("!!! --- ？"), "section");
        assert_eq!(slug(""), "section");
        // Cut at 48 characters, then the dash the cut exposed is dropped.
        assert_eq!(slug(&"abcdefg ".repeat(10)), ["abcdefg"; 6].join("-"));
        assert_eq!(slug(&"word ".repeat(20)).chars().count(), 48);
    }
}
