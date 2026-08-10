// SPDX-License-Identifier: MPL-2.0

//! Block-based note editing model.
//!
//! A note is modeled as a sequence of [`Block`]s, each holding one paragraph,
//! heading, list item, quote, fenced code block, or thematic break. The model
//! is pure logic: it never touches widgets or the filesystem, which keeps the
//! parsing, serialization, and editing rules easy to unit test.
//!
//! Markdown is the storage format. [`parse_markdown`] converts a note body
//! into blocks and [`serialize_markdown`] converts blocks back into markdown.
//! Blank lines are structural separators and never produce blocks; consecutive
//! non-blank lines become separate blocks (a soft line break is treated as a
//! new block, matching how block editors such as Futo Notes behave).

/// The kind of a block, including the data that kind carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockKind {
    /// A plain paragraph.
    Paragraph,
    /// A heading with the given level (1–6).
    Heading(u8),
    /// A blockquote, nested `level` times.
    Quote(u8),
    /// An unordered list item, nested `level` times.
    Bullet(u8),
    /// An ordered list item, nested `level` times.
    Numbered(u8, u32),
    /// A task list item, nested `level` times; the bool is the checked state.
    Todo(u8, bool),
    /// A fenced code block with the given language tag (empty when none).
    Code(String),
    /// A thematic break (`---`).
    Divider,
}

/// A single block of note content.
///
/// [`Block::content`] holds the raw text of the block without its markdown
/// prefix, so the editor only ever edits plain text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    /// What kind of block this is.
    pub kind: BlockKind,
    /// The raw text of the block, without its markdown prefix.
    pub content: String,
}

impl BlockKind {
    /// The list/quote nesting level of the block, if any.
    #[must_use]
    pub fn level(&self) -> u8 {
        match self {
            Self::Quote(level)
            | Self::Bullet(level)
            | Self::Numbered(level, _)
            | Self::Todo(level, _) => *level,
            Self::Paragraph
            | Self::Heading(_)
            | Self::Code(_)
            | Self::Divider => 0,
        }
    }
}

impl Block {
    /// Creates a plain paragraph block.
    #[must_use]
    pub fn paragraph(text: impl Into<String>) -> Self {
        Self {
            kind: BlockKind::Paragraph,
            content: text.into(),
        }
    }

    /// Creates a heading block with the given level.
    #[must_use]
    pub fn heading(level: u8, text: impl Into<String>) -> Self {
        Self {
            kind: BlockKind::Heading(level),
            content: text.into(),
        }
    }

    /// Creates a blockquote block nested `level` times.
    #[must_use]
    pub fn quote(level: u8, text: impl Into<String>) -> Self {
        Self {
            kind: BlockKind::Quote(level),
            content: text.into(),
        }
    }

    /// Creates an unordered list item nested `level` times.
    #[must_use]
    pub fn bullet(level: u8, text: impl Into<String>) -> Self {
        Self {
            kind: BlockKind::Bullet(level),
            content: text.into(),
        }
    }

    /// Creates an ordered list item with the given start number.
    #[must_use]
    pub fn numbered(level: u8, number: u32, text: impl Into<String>) -> Self {
        Self {
            kind: BlockKind::Numbered(level, number),
            content: text.into(),
        }
    }

    /// Creates a task list item with the given checked state.
    #[must_use]
    pub fn todo(level: u8, done: bool, text: impl Into<String>) -> Self {
        Self {
            kind: BlockKind::Todo(level, done),
            content: text.into(),
        }
    }

    /// Creates a fenced code block with the given language tag.
    #[must_use]
    pub fn code(lang: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            kind: BlockKind::Code(lang.into()),
            content: text.into(),
        }
    }

    /// Creates a thematic break.
    #[must_use]
    pub fn divider() -> Self {
        Self {
            kind: BlockKind::Divider,
            content: String::new(),
        }
    }
}

/// Parses markdown text into blocks.
///
/// Blank lines are structural separators and do not produce blocks. Fenced
/// code blocks keep their internal newlines; every other block is a single
/// line. A line that is not recognized as any structured kind becomes a plain
/// paragraph with the inline markdown preserved verbatim.
#[must_use]
pub fn parse_markdown(text: &str) -> Vec<Block> {
    let all_lines: Vec<&str> = text.split('\n').collect();
    let mut blocks = Vec::new();
    let mut i = 0;
    while i < all_lines.len() {
        let line = all_lines[i].trim_end_matches('\r');
        let (indent_level, rest) = indent_level(line);
        let trimmed = rest.trim();
        if trimmed.is_empty() {
            i += 1;
            continue;
        }
        if let Some((fence, lang)) = open_fence(trimmed) {
            blocks.push(parse_code_fence(&all_lines, &mut i, fence, lang));
            continue;
        }
        if is_divider(trimmed) {
            blocks.push(Block::divider());
        } else if let Some((level, text)) = heading(trimmed) {
            blocks.push(Block::heading(level, text));
        } else if let Some((done, text)) = todo_prefix(trimmed) {
            blocks.push(Block::todo(indent_level, done, text));
        } else if let Some(text) = bullet_prefix(trimmed) {
            blocks.push(Block::bullet(indent_level, text));
        } else if let Some((number, text)) = numbered_prefix(trimmed) {
            blocks.push(Block::numbered(indent_level, number, text));
        } else if let Some((count, text)) = quote_prefix(trimmed) {
            let level = u8::try_from(count - 1).unwrap_or(u8::MAX);
            blocks.push(Block::quote(level, text));
        } else {
            blocks.push(Block::paragraph(trimmed));
        }
        i += 1;
    }
    blocks
}

/// Serializes blocks back into markdown text.
///
/// Consecutive blocks of the same list family (bullets, numbered items,
/// tasks, quotes) are joined with a single newline so they render as one
/// list; every other boundary is a blank line. The output ends with a single
/// trailing newline, except when there are no blocks.
#[must_use]
pub fn serialize_markdown(blocks: &[Block]) -> String {
    let mut out = String::new();
    for (idx, block) in blocks.iter().enumerate() {
        if idx > 0 {
            if joins_with(&blocks[idx - 1], block) {
                out.push('\n');
            } else {
                out.push_str("\n\n");
            }
        }
        write_block(block, &mut out);
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

/// Whether `next` continues the same list or quote as `prev` without a blank
/// line, so that consecutive list items render as one list and nesting via
/// [`Block::level`] is preserved in previews.
fn joins_with(prev: &Block, next: &Block) -> bool {
    matches!(
        (&prev.kind, &next.kind),
        (BlockKind::Bullet(_), BlockKind::Bullet(_))
            | (BlockKind::Numbered(..), BlockKind::Numbered(..))
            | (BlockKind::Todo(..), BlockKind::Todo(..))
            | (BlockKind::Quote(_), BlockKind::Quote(_))
    )
}

/// Writes a single block in canonical markdown form.
fn write_block(block: &Block, out: &mut String) {
    match &block.kind {
        BlockKind::Paragraph => out.push_str(&block.content),
        BlockKind::Heading(level) => {
            out.push_str(&"#".repeat(usize::from(*level)));
            out.push(' ');
            out.push_str(&block.content);
        }
        BlockKind::Quote(level) => {
            out.push_str(&"> ".repeat(usize::from(*level) + 1));
            out.push_str(&block.content);
        }
        BlockKind::Bullet(level) => {
            out.push_str(&"  ".repeat(usize::from(*level)));
            out.push_str("- ");
            out.push_str(&block.content);
        }
        BlockKind::Numbered(level, number) => {
            out.push_str(&"  ".repeat(usize::from(*level)));
            out.push_str(&number.to_string());
            out.push_str(". ");
            out.push_str(&block.content);
        }
        BlockKind::Todo(level, done) => {
            out.push_str(&"  ".repeat(usize::from(*level)));
            out.push_str(if *done { "- [x] " } else { "- [ ] " });
            out.push_str(&block.content);
        }
        BlockKind::Code(lang) => {
            out.push_str("```");
            out.push_str(lang);
            out.push('\n');
            out.push_str(&block.content);
            out.push_str("\n```");
        }
        BlockKind::Divider => out.push_str("---"),
    }
}

/// Splits off leading indentation, returning the nesting level (two spaces per
/// level; a tab counts as two) and the remaining line.
fn indent_level(line: &str) -> (u8, &str) {
    let mut width = 0usize;
    let mut bytes = 0usize;
    for c in line.chars() {
        match c {
            ' ' => width += 1,
            '\t' => width += 2,
            _ => break,
        }
        bytes += c.len_utf8();
    }
    (u8::try_from(width / 2).unwrap_or(u8::MAX), &line[bytes..])
}

/// The run of a leading fence character (`` ` `` or `~`), if the line starts
/// with one.
fn fence_run(line: &str) -> Option<(char, usize)> {
    let first = line.chars().next()?;
    if first != '`' && first != '~' {
        return None;
    }
    Some((first, line.chars().take_while(|&c| c == first).count()))
}

/// If the line opens a fenced code block, returns the fence character and the
/// language tag.
fn open_fence(line: &str) -> Option<(char, String)> {
    let (first, count) = fence_run(line)?;
    if count < 3 {
        return None;
    }
    Some((first, line[count..].trim().to_owned()))
}

/// Collects the body of a fenced code block, starting on the line after the
/// opening fence. Advances `i` past the closing fence.
fn parse_code_fence(all_lines: &[&str], i: &mut usize, fence: char, lang: String) -> Block {
    let mut code = String::new();
    *i += 1;
    while *i < all_lines.len() {
        let line = all_lines[*i].trim_end_matches('\r');
        if fence_run(line.trim()).is_some_and(|(c, n)| c == fence && n >= 3) {
            break;
        }
        if !code.is_empty() {
            code.push('\n');
        }
        code.push_str(line);
        *i += 1;
    }
    *i += 1;
    Block::code(lang, code)
}

/// Whether the line is a thematic break (`---`, `***`, or `___`).
pub(crate) fn is_divider(line: &str) -> bool {
    let mut chars = line.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !matches!(first, '-' | '*' | '_') {
        return false;
    }
    let mut count = 1;
    for c in chars {
        if c != first {
            return false;
        }
        count += 1;
    }
    count >= 3
}

/// Parses an ATX heading, returning the level and the content.
fn heading(line: &str) -> Option<(u8, &str)> {
    let hashes = line.chars().take_while(|&c| c == '#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let rest = &line[hashes..];
    if !rest.is_empty() && !rest.starts_with(' ') {
        return None;
    }
    let level = u8::try_from(hashes).ok()?;
    Some((level, rest.trim()))
}

/// Parses a task list item prefix, returning the checked state and content.
/// Accepts `- [ ]`, `* [ ]`, `+ [ ]`, and bare `[ ]` forms.
fn todo_prefix(line: &str) -> Option<(bool, &str)> {
    let rest = match line
        .strip_prefix("- [")
        .or_else(|| line.strip_prefix("* ["))
        .or_else(|| line.strip_prefix("+ ["))
    {
        Some(rest) => rest,
        None => line.strip_prefix("[")?,
    };
    let done = match rest.chars().next()? {
        'x' | 'X' => true,
        ' ' => false,
        _ => return None,
    };
    let rest = &rest[1..];
    let rest = rest.strip_prefix(']')?;
    if !rest.is_empty() && !rest.starts_with(' ') {
        return None;
    }
    Some((done, rest.trim()))
}

/// Parses an unordered list marker, returning the content.
fn bullet_prefix(line: &str) -> Option<&str> {
    let first = line.chars().next()?;
    if !matches!(first, '-' | '*' | '+') {
        return None;
    }
    let rest = &line[first.len_utf8()..];
    if rest.is_empty() {
        return Some("");
    }
    let rest = rest.strip_prefix(' ')?;
    Some(rest.trim())
}

/// Whether the string is a valid ordered-list number (`0` or a digit sequence
/// not starting with `0`).
fn is_ordered_number(digits: &str) -> bool {
    !digits.is_empty()
        && digits.chars().all(|c| c.is_ascii_digit())
        && (digits == "0" || !digits.starts_with('0'))
}

/// Parses an ordered list marker, returning the number and the content.
fn numbered_prefix(line: &str) -> Option<(u32, &str)> {
    let dot = line.find('.')?;
    let digits = &line[..dot];
    if !is_ordered_number(digits) {
        return None;
    }
    let rest = &line[dot + 1..];
    if !rest.is_empty() && !rest.starts_with(' ') {
        return None;
    }
    let number = digits.parse::<u32>().ok()?;
    Some((number, rest.trim()))
}

/// Parses blockquote markers, returning the number of `>` markers consumed and
/// the content.
fn quote_prefix(line: &str) -> Option<(usize, &str)> {
    let mut rest = line;
    let mut count = 0;
    while let Some(after) = rest.strip_prefix('>') {
        count += 1;
        rest = after.strip_prefix(' ').unwrap_or(after);
    }
    if count == 0 {
        return None;
    }
    Some((count, rest.trim()))
}

/// The result of a [`wrap_inline`] operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrapResult {
    /// The text with the markers applied.
    pub text: String,
    /// The byte column of the caret in the resulting text.
    pub caret: usize,
}

/// Wraps the selected byte range of a single line of `text` with `open` and
/// `close` markers, or inserts the markers at `caret` when there is no
/// selection (or the range is empty), leaving the caret between them.
///
/// This is how the toolbar applies inline bold, italic, and code formatting:
/// the markers are stored as visible markdown characters, since this editor
/// has no rich text spans.
#[must_use]
pub fn wrap_inline(
    text: &str,
    selection: Option<(usize, usize)>,
    caret: usize,
    open: &str,
    close: &str,
) -> WrapResult {
    let (start, end) = match selection {
        Some((a, b)) => (a.min(b), a.max(b)),
        None => (caret, caret),
    };
    // Columns are byte offsets from cosmic-text; clamp to character
    // boundaries so the string slices never panic.
    let start = text.floor_char_boundary(start);
    let end = text.floor_char_boundary(end);
    let mut wrapped = String::with_capacity(text.len() + open.len() + close.len());
    wrapped.push_str(&text[..start]);
    wrapped.push_str(open);
    wrapped.push_str(&text[start..end]);
    wrapped.push_str(close);
    wrapped.push_str(&text[end..]);
    let caret = if selection.is_some() {
        end + open.len() + close.len()
    } else {
        start + open.len()
    };
    WrapResult {
        text: wrapped,
        caret,
    }
}

/// The block kind that a line of exactly this text would become, used to
/// auto-convert a block as the user types a markdown prefix.
///
/// The prefix characters are stored in the paragraph's content; when the user
/// types the space that completes a prefix, the editor converts the block
/// instead of inserting the space.
#[must_use]
pub(crate) fn prefix_kind(text: &str) -> Option<BlockKind> {
    if let Some((level, _)) = heading(text) {
        return Some(BlockKind::Heading(level));
    }
    if let Some((done, _)) = todo_prefix(text) {
        return Some(BlockKind::Todo(0, done));
    }
    if bullet_prefix(text).is_some() {
        return Some(BlockKind::Bullet(0));
    }
    if let Some((number, _)) = numbered_prefix(text) {
        return Some(BlockKind::Numbered(0, number));
    }
    if quote_prefix(text).is_some() {
        return Some(BlockKind::Quote(0));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{is_divider, parse_markdown, prefix_kind, serialize_markdown, wrap_inline, Block, BlockKind};

    /// Asserts that parsing and re-serializing is the identity for text that
    /// is already in canonical markdown form.
    fn assert_round_trip(text: &str) {
        let blocks = parse_markdown(text);
        assert_eq!(serialize_markdown(&blocks), text, "round trip failed");
    }

    fn paragraph(text: &str) -> Block {
        Block::paragraph(text)
    }

    #[test]
    fn empty_and_blank_text_parse_to_no_blocks() {
        assert_eq!(parse_markdown(""), Vec::<Block>::new());
        assert_eq!(parse_markdown("  \n\t\n"), Vec::<Block>::new());
    }

    #[test]
    fn paragraphs_split_on_lines() {
        assert_eq!(parse_markdown("hello"), vec![paragraph("hello")]);
        assert_eq!(
            parse_markdown("line one\nline two"),
            vec![paragraph("line one"), paragraph("line two")]
        );
        assert_round_trip("a\n\nb\n");
    }

    #[test]
    fn headings_parse_with_level() {
        assert_eq!(
            parse_markdown("# One"),
            vec![Block::heading(1, "One")]
        );
        assert_eq!(
            parse_markdown("###### Six"),
            vec![Block::heading(6, "Six")]
        );
        assert_round_trip("# One\n\n###### Six\n");
    }

    #[test]
    fn heading_lookalikes_stay_paragraphs() {
        assert_eq!(parse_markdown("####### Seven"), vec![paragraph("####### Seven")]);
        assert_eq!(parse_markdown("#NoSpace"), vec![paragraph("#NoSpace")]);
        assert_eq!(parse_markdown("a#b"), vec![paragraph("a#b")]);
    }

    #[test]
    fn empty_heading_round_trips_with_space() {
        let blocks = parse_markdown("#");
        assert_eq!(blocks, vec![Block::heading(1, "")]);
        assert_eq!(serialize_markdown(&blocks), "# \n");
    }

    #[test]
    fn bullets_parse_any_marker_and_canonicalize() {
        assert_eq!(parse_markdown("- a"), vec![Block::bullet(0, "a")]);
        assert_eq!(parse_markdown("* b"), vec![Block::bullet(0, "b")]);
        assert_eq!(parse_markdown("+ c"), vec![Block::bullet(0, "c")]);
        assert_eq!(parse_markdown("-"), vec![Block::bullet(0, "")]);
        let blocks = parse_markdown("* a\n+ b");
        assert_eq!(
            serialize_markdown(&blocks),
            "- a\n- b\n",
            "star and plus markers canonicalize to a dash"
        );
        assert_round_trip("- a\n- b\n");
    }

    #[test]
    fn nested_lists_preserve_level() {
        assert_eq!(
            parse_markdown("  - child"),
            vec![Block::bullet(1, "child")]
        );
        assert_eq!(
            parse_markdown("    - grand"),
            vec![Block::bullet(2, "grand")]
        );
        assert_eq!(
            parse_markdown("\t- tabbed"),
            vec![Block::bullet(1, "tabbed")]
        );
        assert_round_trip("  - child\n    - grand\n");
    }

    #[test]
    fn numbered_lists_preserve_number() {
        assert_eq!(
            parse_markdown("1. a"),
            vec![Block::numbered(0, 1, "a")]
        );
        assert_eq!(
            parse_markdown("3. c"),
            vec![Block::numbered(0, 3, "c")]
        );
        assert_eq!(
            parse_markdown("0. z"),
            vec![Block::numbered(0, 0, "z")]
        );
        assert_round_trip("3. c\n");
    }

    #[test]
    fn numbered_lookalikes_stay_paragraphs() {
        assert_eq!(parse_markdown("1.x"), vec![paragraph("1.x")]);
        assert_eq!(parse_markdown("1.2"), vec![paragraph("1.2")]);
        assert_eq!(parse_markdown("01. x"), vec![paragraph("01. x")]);
    }

    #[test]
    fn nested_numbered_lists_preserve_level() {
        assert_eq!(
            parse_markdown("  2. b"),
            vec![Block::numbered(1, 2, "b")]
        );
        assert_round_trip("  2. b\n");
    }

    #[test]
    fn todos_parse_checked_state() {
        assert_eq!(
            parse_markdown("- [ ] a"),
            vec![Block::todo(0, false, "a")]
        );
        assert_eq!(
            parse_markdown("- [x] b"),
            vec![Block::todo(0, true, "b")]
        );
        assert_eq!(
            parse_markdown("- [X] c"),
            vec![Block::todo(0, true, "c")]
        );
        assert_eq!(
            parse_markdown("[ ] e"),
            vec![Block::todo(0, false, "e")]
        );
        assert_eq!(
            parse_markdown("[x] f"),
            vec![Block::todo(0, true, "f")]
        );
    }

    #[test]
    fn todos_serialize_canonically() {
        let blocks = parse_markdown("[ ] e");
        assert_eq!(serialize_markdown(&blocks), "- [ ] e\n");
        assert_round_trip("- [ ] a\n- [x] b\n");
    }

    #[test]
    fn quotes_parse_nesting() {
        assert_eq!(parse_markdown("> q"), vec![Block::quote(0, "q")]);
        assert_eq!(
            parse_markdown("> > nested"),
            vec![Block::quote(1, "nested")]
        );
        assert_eq!(
            parse_markdown(">> tight"),
            vec![Block::quote(1, "tight")]
        );
        assert_eq!(parse_markdown(">"), vec![Block::quote(0, "")]);
    }

    #[test]
    fn quotes_serialize_canonically() {
        assert_round_trip("> q\n> > nested\n");
    }

    #[test]
    fn same_family_blocks_join_into_one_list() {
        let blocks = parse_markdown("- a\n- b");
        assert_eq!(serialize_markdown(&blocks), "- a\n- b\n");
        let blocks = parse_markdown("1. a\n2. b");
        assert_eq!(serialize_markdown(&blocks), "1. a\n2. b\n");
        let blocks = parse_markdown("> a\n> b");
        assert_eq!(serialize_markdown(&blocks), "> a\n> b\n");
    }

    #[test]
    fn different_families_separate_with_blank_line() {
        let blocks = parse_markdown("- a\n1. b");
        assert_eq!(serialize_markdown(&blocks), "- a\n\n1. b\n");
        let blocks = parse_markdown("- a\ntext");
        assert_eq!(serialize_markdown(&blocks), "- a\n\ntext\n");
    }

    #[test]
    fn nested_list_serialization_preserves_nesting() {
        let blocks = vec![Block::bullet(0, "parent"), Block::bullet(1, "child")];
        assert_eq!(serialize_markdown(&blocks), "- parent\n  - child\n");
    }

    #[test]
    fn code_blocks_preserve_newlines_and_lang() {
        assert_eq!(
            parse_markdown("```\na\nb\n```"),
            vec![Block::code("", "a\nb")]
        );
        assert_eq!(
            parse_markdown("```rust\nlet x = 1;\n```"),
            vec![Block::code("rust", "let x = 1;")]
        );
        assert_round_trip("```\na\nb\n```\n");
        assert_round_trip("```rust\nlet x = 1;\n```\n");
    }

    #[test]
    fn empty_code_block_round_trips() {
        let blocks = parse_markdown("```\n\n```");
        assert_eq!(blocks, vec![Block::code("", "")]);
        assert_eq!(serialize_markdown(&blocks), "```\n\n```\n");
    }

    #[test]
    fn unterminated_fence_consumes_the_rest() {
        assert_eq!(
            parse_markdown("```\na\nb"),
            vec![Block::code("", "a\nb")]
        );
    }

    #[test]
    fn tilde_fences_canonicalize_to_backticks() {
        let blocks = parse_markdown("~~~\nx\n~~~");
        assert_eq!(blocks, vec![Block::code("", "x")]);
        assert_eq!(serialize_markdown(&blocks), "```\nx\n```\n");
    }

    #[test]
    fn dividers_parse_from_any_marker() {
        assert_eq!(parse_markdown("---"), vec![Block::divider()]);
        assert_eq!(parse_markdown("***"), vec![Block::divider()]);
        assert_eq!(parse_markdown("___"), vec![Block::divider()]);
        assert_eq!(parse_markdown("----"), vec![Block::divider()]);
        assert_round_trip("---\n");
    }

    #[test]
    fn divider_between_paragraphs() {
        assert_eq!(
            parse_markdown("a\n---\nb"),
            vec![paragraph("a"), Block::divider(), paragraph("b")]
        );
    }

    #[test]
    fn lone_marker_is_not_a_divider() {
        assert_eq!(parse_markdown("-"), vec![Block::bullet(0, "")]);
        assert_eq!(parse_markdown("*"), vec![Block::bullet(0, "")]);
    }

    #[test]
    fn crlf_lines_are_handled() {
        let blocks = parse_markdown("a\r\n\r\n- b\r\n");
        assert_eq!(blocks, vec![paragraph("a"), Block::bullet(0, "b")]);
        assert_eq!(serialize_markdown(&blocks), "a\n\n- b\n");
    }

    #[test]
    fn inline_markdown_in_paragraphs_is_preserved() {
        assert_eq!(
            parse_markdown("**bold** and `code`"),
            vec![paragraph("**bold** and `code`")]
        );
        assert_round_trip("**bold** and `code`\n");
    }

    #[test]
    fn mixed_document_round_trips() {
        assert_round_trip(
            "# Title\n\nSome intro text.\n\n- first\n- second\n\n1. one\n2. two\n\n> a quote\n\n\
             - [x] done task\n- [ ] pending task\n\n```rust\nfn main() {}\n```\n\n---\n",
        );
    }

    #[test]
    fn trailing_newlines_are_normalized() {
        assert_eq!(parse_markdown("a\n"), vec![paragraph("a")]);
        assert_eq!(parse_markdown("a\n\n"), vec![paragraph("a")]);
        assert_round_trip("a\n");
        assert_eq!(serialize_markdown(&[paragraph("a")]), "a\n");
    }

    #[test]
    fn serializing_no_blocks_yields_empty_text() {
        assert_eq!(serialize_markdown(&[]), "");
    }

    #[test]
    fn block_kind_level_reports_nesting() {
        assert_eq!(BlockKind::Bullet(2).level(), 2);
        assert_eq!(BlockKind::Numbered(1, 4).level(), 1);
        assert_eq!(BlockKind::Todo(3, false).level(), 3);
        assert_eq!(BlockKind::Quote(1).level(), 1);
        assert_eq!(BlockKind::Paragraph.level(), 0);
        assert_eq!(BlockKind::Heading(2).level(), 0);
        assert_eq!(BlockKind::Code(String::new()).level(), 0);
        assert_eq!(BlockKind::Divider.level(), 0);
    }

    #[test]
    fn prefix_kind_converts_typed_markers() {
        assert_eq!(prefix_kind("#"), Some(BlockKind::Heading(1)));
        assert_eq!(prefix_kind("###"), Some(BlockKind::Heading(3)));
        assert_eq!(prefix_kind("-"), Some(BlockKind::Bullet(0)));
        assert_eq!(prefix_kind("*"), Some(BlockKind::Bullet(0)));
        assert_eq!(prefix_kind("1."), Some(BlockKind::Numbered(0, 1)));
        assert_eq!(prefix_kind("12."), Some(BlockKind::Numbered(0, 12)));
        assert_eq!(prefix_kind(">"), Some(BlockKind::Quote(0)));
        assert_eq!(prefix_kind("[ ]"), Some(BlockKind::Todo(0, false)));
        assert_eq!(prefix_kind("[x]"), Some(BlockKind::Todo(0, true)));
        assert_eq!(prefix_kind("[X]"), Some(BlockKind::Todo(0, true)));
        // Incomplete or unrelated text stays a paragraph.
        assert_eq!(prefix_kind("#"), Some(BlockKind::Heading(1)));
        assert_eq!(prefix_kind("##x"), None);
        assert_eq!(prefix_kind("text"), None);
        assert_eq!(prefix_kind("1"), None);
        assert_eq!(prefix_kind(""), None);
    }

    #[test]
    fn divider_markers_are_detected() {
        assert!(is_divider("---"));
        assert!(is_divider("***"));
        assert!(is_divider("___"));
        assert!(!is_divider("--"));
        assert!(!is_divider("---- text"));
        assert!(!is_divider(""));
    }

    #[test]
    fn wrap_inline_wraps_the_selection() {
        let result = wrap_inline("hello world", Some((0, 5)), 5, "**", "**");
        assert_eq!(result.text, "**hello** world");
        assert_eq!(result.caret, 9);
    }

    #[test]
    fn wrap_inline_handles_reversed_selection() {
        let result = wrap_inline("hello", Some((5, 0)), 5, "*", "*");
        assert_eq!(result.text, "*hello*");
        assert_eq!(result.caret, 7);
    }

    #[test]
    fn wrap_inline_inserts_markers_at_the_caret() {
        let result = wrap_inline("abc", None, 1, "`", "`");
        assert_eq!(result.text, "a``bc");
        assert_eq!(result.caret, 2);
    }

    #[test]
    fn wrap_inline_clamps_to_char_boundaries() {
        // "é" is two bytes; a column inside it clamps to its start.
        let result = wrap_inline("aé", Some((1, 2)), 1, "**", "**");
        assert_eq!(result.text, "a****é");
        assert_eq!(result.caret, 5);
    }
}
