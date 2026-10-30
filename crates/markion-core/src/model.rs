//! Public domain types shared between the core library and the UI layer.
//!
//! These are plain data structures (with only trivial constructors / trait
//! impls). Behavior on [`MarkdownDocument`](crate::MarkdownDocument) lives in
//! the crate root and the `document` module group.

use crate::inline_edit::ImagePresentation;
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Heading {
    pub level: u8,
    pub title: String,
    pub anchor: String,
    pub offset: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchMatch {
    pub line: usize,
    pub column: usize,
    pub snippet: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Footnote {
    pub label: String,
    pub text: String,
    pub references: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchOptions {
    pub query: String,
    pub case_sensitive: bool,
    pub regex: bool,
}

impl SearchOptions {
    pub fn literal(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            case_sensitive: false,
            regex: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchMatchRange {
    pub range: Range<usize>,
    pub line: usize,
    pub column: usize,
    pub snippet: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplaceResult {
    pub replacements: usize,
    pub selected_range: Option<Range<usize>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MathExpression {
    pub latex: String,
    pub display: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedMath {
    pub latex: String,
    pub display: bool,
    pub text: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchError {
    pub(crate) message: String,
}

impl SearchError {
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for SearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SearchError {}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentStats {
    pub bytes: usize,
    pub chars: usize,
    pub words: usize,
    pub lines: usize,
    pub headings: usize,
}

/// The cheap subset of [`DocumentStats`]: byte/char/word/line counters with no
/// heading count. Computing headings needs a Markdown parse (the dominant
/// per-keystroke cost), so per-frame readers such as the status bar use this
/// instead of `stats()`. Version-cached on the document like the other
/// derived values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocumentBasicStats {
    pub bytes: usize,
    pub chars: usize,
    pub words: usize,
    pub lines: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HighlightKind {
    Plain,
    Keyword,
    String,
    Number,
    Comment,
    Type,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighlightedSpan {
    pub text: String,
    pub kind: HighlightKind,
}

/// Inline formatting flags carried by a [`InlineSpan`]. Multiple flags can be
/// active at once (e.g. bold italic inside a highlight).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct InlineStyle {
    pub bold: bool,
    pub italic: bool,
    pub strikethrough: bool,
    pub code: bool,
    pub highlight: bool,
    pub superscript: bool,
    pub subscript: bool,
    pub underline: bool,
    pub color: Option<u32>,
}

impl InlineStyle {
    pub fn is_plain(&self) -> bool {
        *self == Self::default()
    }
}

/// Bytes hashed while deriving complete data-URI identities. Tests reset this
/// around a derivation and assert that later repaint/claim work does not add
/// to it.
pub static DATA_URI_IDENTITY_HASH_BYTES: AtomicU64 = AtomicU64::new(0);
/// Bytes copied while retaining a pending data-URI payload for decode.
pub static DATA_URI_PAYLOAD_CLONE_BYTES: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static DATA_URI_IDENTITY_INTERN: RefCell<Option<HashMap<(*const u8, usize), ImageSourceIdentity>>> =
        const { RefCell::new(None) };
}

/// Run `f` with a derivation-local interner so identical data-URI `&str`
/// values in one document version are hashed only once.
pub fn with_image_identity_interner<T>(f: impl FnOnce() -> T) -> T {
    DATA_URI_IDENTITY_INTERN.with(|slot| {
        let previous = slot.replace(Some(HashMap::new()));
        let result = f();
        slot.replace(previous);
        result
    })
}

pub fn reset_data_uri_work_counters() {
    DATA_URI_IDENTITY_HASH_BYTES.store(0, Ordering::Relaxed);
    DATA_URI_PAYLOAD_CLONE_BYTES.store(0, Ordering::Relaxed);
}

pub fn record_data_uri_payload_clone(bytes: usize) {
    DATA_URI_PAYLOAD_CLONE_BYTES.fetch_add(bytes as u64, Ordering::Relaxed);
}

/// Render-cache identity for an image source. Local and remote locators are
/// resolved at claim time (cheap). Data URIs carry a complete SHA-256 digest
/// computed during per-document-version derivation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ImageSourceIdentity {
    /// Non-data locator; `PreviewImageKey` resolves it with `document_dir`.
    FromUrl,
    Data {
        byte_len: usize,
        sha256: [u8; 32],
    },
}

impl Default for ImageSourceIdentity {
    fn default() -> Self {
        Self::FromUrl
    }
}

impl ImageSourceIdentity {
    /// Identity for one authored URL. Data URIs hash the complete URI; other
    /// schemes defer locator normalization to claim/paint time.
    pub fn for_url(url: &str) -> Self {
        if !url.starts_with("data:") {
            return Self::FromUrl;
        }
        DATA_URI_IDENTITY_INTERN.with(|slot| {
            if let Some(map) = slot.borrow_mut().as_mut() {
                let key = (url.as_ptr(), url.len());
                if let Some(existing) = map.get(&key) {
                    return existing.clone();
                }
                let identity = Self::from_data_uri(url);
                map.insert(key, identity.clone());
                identity
            } else {
                Self::from_data_uri(url)
            }
        })
    }

    pub fn from_data_uri(url: &str) -> Self {
        DATA_URI_IDENTITY_HASH_BYTES.fetch_add(url.len() as u64, Ordering::Relaxed);
        let digest = Sha256::digest(url.as_bytes());
        let mut sha256 = [0u8; 32];
        sha256.copy_from_slice(&digest);
        Self::Data {
            byte_len: url.len(),
            sha256,
        }
    }

    pub fn data_sha256(&self) -> Option<[u8; 32]> {
        match self {
            Self::Data { sha256, .. } => Some(*sha256),
            Self::FromUrl => None,
        }
    }

    pub fn is_data_uri(&self) -> bool {
        matches!(self, Self::Data { .. })
    }
}

/// One HTML `<img>` extracted during per-version derivation so paint/claim
/// paths can reuse a constant-size identity without rescanning the payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HtmlImageDescriptor {
    pub url: Arc<str>,
    pub identity: ImageSourceIdentity,
}

impl HtmlImageDescriptor {
    pub fn from_url(url: &str) -> Self {
        Self {
            identity: ImageSourceIdentity::for_url(url),
            url: Arc::from(url),
        }
    }
}

/// A Markdown `![alt](url)` (or equivalent) kept inside a prose construct
/// instead of being extracted as a block-level [`PreviewBlock::Image`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineImage {
    pub alt: String,
    pub url: String,
    pub title: Option<String>,
    pub presentation: Option<ImagePresentation>,
    pub source_range: Range<usize>,
    pub identity: ImageSourceIdentity,
}

/// A run of preview text sharing one inline style (and optional link target).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InlineSpan {
    /// An authored line break that must survive whitespace normalization.
    pub hard_break: bool,
    pub text: String,
    pub style: InlineStyle,
    pub link: Option<String>,
    pub math: Option<MathSource>,
    pub image: Option<InlineImage>,
    /// Footnote reference label (`[^label]`). Distinct from superscript.
    pub footnote: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MathLayoutStyle {
    Text,
    Display,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MathDelimiter {
    InlineDollar,
    DisplayDollar,
    Fenced,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MathSource {
    pub latex: String,
    pub authored: String,
    pub style: MathLayoutStyle,
    pub delimiter: MathDelimiter,
    pub source_range: Range<usize>,
}

/// Block-level preview text with resolved inline styling. `text` is always the
/// concatenation of `spans`, so consumers that only need plain text (LaTeX,
/// DOCX, tests) can keep using `text`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RichText {
    pub text: String,
    pub spans: Vec<InlineSpan>,
}

/// Source-backed block rendered by Visual Edit. Unlike [`PreviewBlock`], this
/// representation keeps the byte ranges needed to send edits back to the
/// canonical Markdown text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisualBlock {
    /// Ephemeral identity used to retain row-local UI state across source
    /// versions when this block is proven to be unchanged.
    pub id: VisualBlockId,
    pub kind: VisualBlockKind,
    pub source_range: Range<usize>,
    pub editable_runs: Vec<VisualInlineRun>,
    /// Byte-exact inline constructs that may be temporarily emitted as source
    /// while the rest of the block remains rendered.
    pub reveal_groups: Vec<VisualRevealGroup>,
    pub marker_ranges: Vec<Range<usize>>,
    /// Exact structural prefix for supported line-oriented blocks.
    pub block_prefix: Option<VisualBlockPrefix>,
    /// Rendered-geometry provenance for rows whose height depends on source
    /// content that can change while the block identity stays stable. A
    /// whitespace row carries its covered newline count; every other kind is
    /// `None` because identity already proves its geometry unchanged. The
    /// virtualized list compares this alongside `id` so a height-mutable row
    /// is re-measured instead of reusing a stale cached height.
    pub height_signature: Option<u32>,
    /// Quote decoration and exact source markers inherited from an enclosing
    /// blockquote. The visual row remains a paragraph/list leaf.
    pub quote_context: Option<VisualQuoteContext>,
    /// Number of list containers enclosing the preview block this row
    /// projects; rows for nested code/math/table/HTML blocks indent to their
    /// owning item's content column. Synthesized structural rows keep 0.
    pub list_depth: usize,
    pub source_island: Option<VisualSourceIslandKind>,
    /// Exact editable fields for a dedicated complex-block editor. Absent
    /// when the authored syntax cannot be mapped losslessly.
    pub editor: Option<VisualBlockEditor>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VisualBlockKind {
    Heading {
        level: u8,
    },
    Paragraph,
    ListItem {
        level: usize,
        ordered: bool,
        index: Option<u64>,
        checked: Option<bool>,
    },
    BlockQuote,
    /// Leading `[!NOTE]`-style marker line of a GFM alert quote. Owns exactly
    /// the marker-line bytes with no editable runs: the marker text stays
    /// hidden until focus reveals it through the quote marker mechanism.
    CalloutTitle {
        kind: AlertKind,
        /// Custom title authored after the marker (`> [!NOTE] Title`).
        title: Option<String>,
    },
    CodeBlock {
        language: Option<String>,
    },
    MathBlock {
        latex: String,
        authored: String,
        delimiter: MathDelimiter,
    },
    Image {
        alt: String,
        url: String,
        title: Option<String>,
        presentation: Option<ImagePresentation>,
        identity: ImageSourceIdentity,
    },
    /// Raw HTML block rendered read-only through the shared HTML-parts pipeline
    /// (so `<table>` blocks, text, and images appear). No editable runs and no
    /// source-island — Visual Edit shows the rendered view, not a raw-source box.
    Html {
        html: String,
        /// Image sources extracted once per document version so claim/paint
        /// reuse constant-size identities instead of rescanning data URIs.
        images: Vec<HtmlImageDescriptor>,
    },
    /// Leading YAML `---` / `...` document header with a collapsible source
    /// payload. Parsed `title` is presentation-only for the collapsed chrome.
    FrontMatter {
        title: Option<String>,
    },
    Rule,
    Table {
        rows: Vec<Vec<RichText>>,
        alignments: Vec<TableAlignment>,
    },
    /// Footnote definition (`[^label]: …`) covering marker and body.
    FootnoteDefinition {
        label: String,
    },
    /// Term line of a definition list.
    DefinitionTerm,
    /// One `: definition` of a definition list; the `:` marker is the row's
    /// block prefix.
    DefinitionDetail,
    /// Typora-style `[TOC]` paragraph rendered as a live outline.
    TableOfContents,
    /// Standalone link reference definition line(s) (`[label]: url`).
    ReferenceDefinition,
    /// Whitespace-only source not owned by a parsed preview block. Visual Edit
    /// keeps it as a compact row so blank lines and trailing whitespace remain
    /// valid caret positions without showing raw source until focused.
    Whitespace,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisualInlineRun {
    pub visible_text: String,
    /// Source extent emitted by the Markdown parser for this run.
    pub source_range: Range<usize>,
    /// Exact editable content within `source_range` when it can be identified.
    pub content_range: Range<usize>,
    pub style: InlineStyle,
    pub link_target_range: Option<Range<usize>>,
    /// Resolved navigation destination for an adjacent Visual Edit icon.
    /// Presentation-only: never mutates canonical source.
    pub navigation: Option<VisualNavigationTarget>,
    pub math: Option<MathSource>,
    /// Raw-HTML `<img>` tag presented as an inline image atom. The authored
    /// tag source stays the canonical editable range; this payload is
    /// presentation-only derived data.
    pub html_image: Option<VisualHtmlImage>,
    /// True when the parser's visible text does not map byte-for-byte to source.
    pub conservative_fallback: bool,
}

/// A CSS length authored on an HTML `<img>` `width` or `height` attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HtmlImgLength {
    Px(u32),
    Percent(u16),
}

/// Attributes of one raw-HTML `<img>` tag recognized inside prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisualHtmlImage {
    pub alt: String,
    pub url: String,
    pub title: Option<String>,
    pub width: Option<HtmlImgLength>,
    pub height: Option<HtmlImgLength>,
    pub identity: ImageSourceIdentity,
}

/// Destination opened or jumped to from a Visual Edit navigation icon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VisualNavigationTarget {
    Url(String),
    Footnote { label: String },
    Heading { anchor: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisualRevealKind {
    Strong,
    Emphasis,
    Strikethrough,
    InlineCode,
    Link,
    Highlight,
    Superscript,
    Subscript,
    Math,
    /// One complete raw-HTML `<img …>` tag rendered as an inline image atom.
    HtmlImage,
    /// One backslash-escaped ASCII punctuation character (`\*`, `\\`): the
    /// escaped character renders literally while the backslash stays hidden.
    Escape,
    /// One decoded HTML entity reference (`&amp;`, `&#39;`, `&#x2014;`): the
    /// decoded character renders while the complete authored `&…;` token
    /// stays hidden until the caret enters it.
    Entity,
    /// One `:shortcode:` emoji token: the glyph renders while the complete
    /// authored `:name:` stays hidden until the caret enters it.
    Emoji,
    /// One supported inline-HTML element (style pair or `<br>`): the tags stay
    /// hidden markers while the content renders with the mapped style.
    InlineHtml,
}

/// Opaque, non-persisted identity of a source-backed Visual Edit block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct VisualBlockId(pub(crate) u64);

impl VisualBlockId {
    pub(crate) fn fresh() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }

    /// Stable widget key for this process-local derived block identity.
    pub fn as_u64(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VisualBlockEditor {
    Code {
        opening_fence: Range<usize>,
        payload: VisualEditorField,
        info_range: Option<Range<usize>>,
        /// First info-string token — the language identifier. Empty range at
        /// `opening_fence.end` when the fence carries no info string; the
        /// caret-owned header language field edits exactly this range.
        info: VisualEditorField,
        closing_fence: Range<usize>,
    },
    Math {
        opening_delimiter: Range<usize>,
        payload: VisualEditorField,
        closing_delimiter: Range<usize>,
    },
    Table {
        cells: Vec<VisualTableCell>,
    },
    Html {
        payload: VisualEditorField,
    },
    FrontMatter {
        payload: VisualEditorField,
    },
    /// One complete proven inline-image span, presented by Visual Edit as an
    /// image with an on-demand whole-span source payload. Unlike the removed
    /// structured field editor, this payload covers the complete authored
    /// `![alt](destination "title")` bytes as a single field.
    Image {
        payload: VisualEditorField,
        /// Complete SHA-256 of a data-URI destination, computed once per
        /// derivation. The render path matches it against the failed-decode
        /// set to decide forced expansion without re-deriving a
        /// multi-megabyte cache key every frame.
        data_uri_fingerprint: Option<[u8; 32]>,
    },
}

impl VisualBlockEditor {
    pub fn fields(&self) -> Vec<&VisualEditorField> {
        match self {
            Self::Code { payload, info, .. } => vec![payload, info],
            Self::Math { payload, .. } => vec![payload],
            Self::Table { cells } => cells.iter().map(|cell| &cell.field).collect(),
            Self::Html { payload }
            | Self::FrontMatter { payload }
            | Self::Image { payload, .. } => vec![payload],
        }
    }

    pub fn field_containing(&self, range: &Range<usize>) -> Option<&VisualEditorField> {
        self.fields().into_iter().find(|field| {
            range.start >= field.source_range.start && range.end <= field.source_range.end
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VisualEditorFieldKind {
    CodePayload,
    /// Body of an indented code block. The field's source keeps each body
    /// line's four-column indentation; Visual Edit hides it and re-adds it
    /// on Enter and paste.
    IndentedCodePayload,
    MathPayload,
    HtmlSource,
    /// Complete authored YAML front-matter span, including `---` delimiters.
    FrontMatterSource,
    ImageAlt,
    ImageDestination,
    ImageTitle,
    /// Complete authored Markdown image span behind the image source toggle.
    ImageSource,
    /// First fenced info-string token (the language identifier).
    CodeInfo,
    TableCell {
        row: usize,
        column: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisualEditorField {
    pub kind: VisualEditorFieldKind,
    pub source_range: Range<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisualTableCell {
    pub row: usize,
    pub column: usize,
    pub field: VisualEditorField,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisualBlockEdit {
    pub document_version: u64,
    pub block_id: VisualBlockId,
    /// The exact current-version field that originated this edit. This is
    /// revalidated immediately before mutation, including for table edits
    /// whose canonical replacement range is the complete table block.
    pub field: VisualEditorField,
    pub range: Range<usize>,
    pub replacement: String,
    /// Exact inserted/composing bytes after applying `replacement`.
    pub inserted_range_after: Range<usize>,
    pub selection_after: Range<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisualRevealGroup {
    pub kind: VisualRevealKind,
    /// Complete source syntax, including opening/closing markers and, for a
    /// link, its destination and optional title.
    pub source_range: Range<usize>,
    /// Exact rendered-content ranges contained by this syntax group.
    pub content_ranges: Vec<Range<usize>>,
    pub link_target_range: Option<Range<usize>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisualProjectionSegment {
    pub display_range: Range<usize>,
    pub source_range: Range<usize>,
    /// Atomic segments collapse opaque runs (e.g. an elided data-URI payload)
    /// into one unit: the caret may rest on either boundary but never inside,
    /// and any edit intersecting the display range replaces the whole
    /// `source_range` in one canonical replacement.
    pub atomic: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisualProjectionSpan {
    pub display_range: Range<usize>,
    pub style: InlineStyle,
    pub link: bool,
    pub source: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisualCaretAffinity {
    Upstream,
    Downstream,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisualBoundaryCandidates {
    pub display_offset: usize,
    pub upstream_source: usize,
    pub downstream_source: usize,
}

impl VisualBoundaryCandidates {
    pub fn is_ambiguous(self) -> bool {
        self.upstream_source != self.downstream_source
    }

    pub fn resolve(self, affinity: VisualCaretAffinity) -> usize {
        match affinity {
            VisualCaretAffinity::Upstream => self.upstream_source,
            VisualCaretAffinity::Downstream => self.downstream_source,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisualProjection {
    pub text: String,
    pub segments: Vec<VisualProjectionSegment>,
    pub spans: Vec<VisualProjectionSpan>,
    pub revealed_source_ranges: Vec<Range<usize>>,
    pub source_anchor: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisualBlockPrefixKind {
    Heading {
        level: u8,
    },
    BlockQuote {
        depth: usize,
    },
    UnorderedList {
        level: usize,
    },
    OrderedList {
        level: usize,
        index: u64,
    },
    TaskList {
        level: usize,
        checked: bool,
    },
    /// The `:` marker (plus spacing) opening a definition-list definition.
    DefinitionMarker,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisualBlockPrefix {
    pub kind: VisualBlockPrefixKind,
    /// Indentation only. Empty for a top-level block.
    pub indentation_range: Range<usize>,
    /// Indentation plus the complete structural marker and following spacing.
    pub source_range: Range<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisualQuoteGroupEdge {
    Only,
    First,
    Middle,
    Last,
}

/// Source-backed quote metadata attached to an ordinary visual leaf row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisualQuoteContext {
    pub depth: usize,
    /// Every quote-marker prefix intersecting this row, one per physical line.
    pub marker_ranges: Vec<Range<usize>>,
    /// Semantic source emitted for the underlying paragraph or list item.
    pub leaf_source_range: Range<usize>,
    /// Source range of the containing quote, used to keep adjacent rows in one
    /// visual group without reparsing during rendering.
    pub group_source_range: Range<usize>,
    pub edge: VisualQuoteGroupEdge,
    /// GFM alert kind of the containing quote group, so every row of an
    /// alert (title and body) can carry its callout styling.
    pub alert: Option<AlertKind>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisualStructuralEdit {
    pub range: Range<usize>,
    pub replacement: String,
    pub selection_after: Range<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisualSourceIslandKind {
    FrontMatter,
    Code,
    Math,
    Html,
    Image,
    Table,
    Unsupported,
}

impl RichText {
    /// The same text with every span (or the whole unstyled text) bold, for
    /// exporters that render definition-list terms as bold paragraphs.
    pub fn emboldened(&self) -> RichText {
        let mut rich = self.clone();
        if rich.spans.is_empty() && !rich.text.is_empty() {
            rich.spans.push(InlineSpan {
                text: rich.text.clone(),
                ..InlineSpan::default()
            });
        }
        for span in &mut rich.spans {
            span.style.bold = true;
        }
        rich
    }

    pub fn plain(text: impl Into<String>) -> Self {
        let text = text.into();
        if text.is_empty() {
            return Self::default();
        }
        Self {
            spans: vec![InlineSpan {
                hard_break: false,
                text: text.clone(),
                style: InlineStyle::default(),
                link: None,
                math: None,
                image: None,
                footnote: None,
            }],
            text,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
}

impl From<&str> for RichText {
    fn from(text: &str) -> Self {
        Self::plain(text)
    }
}

impl From<String> for RichText {
    fn from(text: String) -> Self {
        Self::plain(text)
    }
}

impl std::fmt::Display for RichText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

/// GFM alert kind reported by the parser when a blockquote opens with a
/// `[!NOTE]`-style marker line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertKind {
    Note,
    Tip,
    Important,
    Warning,
    Caution,
}

impl AlertKind {
    /// Canonical label shown when an alert has no custom title.
    pub fn label(self) -> &'static str {
        match self {
            Self::Note => "Note",
            Self::Tip => "Tip",
            Self::Important => "Important",
            Self::Warning => "Warning",
            Self::Caution => "Caution",
        }
    }

    /// Lowercase kind name used in `markdown-alert-<kind>` HTML classes.
    pub fn class_name(self) -> &'static str {
        match self {
            Self::Note => "note",
            Self::Tip => "tip",
            Self::Important => "important",
            Self::Warning => "warning",
            Self::Caution => "caution",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewBlock {
    Heading {
        level: u8,
        text: RichText,
        source_range: Range<usize>,
    },
    Paragraph {
        text: RichText,
        source_range: Range<usize>,
    },
    ListItem {
        level: usize,
        ordered: bool,
        /// 1-based number shown for ordered list items, honoring the list's
        /// start attribute (e.g. `3.` continues as 4, 5, ...).
        index: Option<u64>,
        checked: Option<bool>,
        text: RichText,
        source_range: Range<usize>,
    },
    BlockQuote {
        /// Ordered leaf blocks authored inside the quote. Paragraphs and list
        /// items remain distinct so every source byte has exactly one visual
        /// owner and consumers preserve the authored block order.
        children: Vec<PreviewBlock>,
        /// GFM alert kind when the quote opens with a `[!NOTE]`-style marker
        /// line; `None` for plain blockquotes. The marker line is block
        /// structure (no inline events own its bytes), so consumers must
        /// account for it separately from `children`.
        alert: Option<AlertKind>,
        /// Custom title of an alert whose marker line carries one
        /// (`> [!NOTE] Title`); the marker line is then not part of
        /// `children`.
        alert_title: Option<String>,
        source_range: Range<usize>,
    },
    CodeBlock {
        language: Option<String>,
        code: String,
        source_range: Range<usize>,
        /// Number of list containers enclosing this block when it nests
        /// inside a list item; 0 for a top-level block.
        list_depth: usize,
        /// `true` for a backtick/tilde fence, `false` for an indented code
        /// block (whose body lines carry the four-column indentation).
        fenced: bool,
    },
    MathBlock {
        latex: String,
        /// Byte-identical authored syntax, including delimiters or fence.
        authored: String,
        delimiter: MathDelimiter,
        error: Option<String>,
        source_range: Range<usize>,
        /// Number of list containers enclosing this block when it nests
        /// inside a list item; 0 for a top-level block.
        list_depth: usize,
    },
    Html {
        html: String,
        source_range: Range<usize>,
        images: Vec<HtmlImageDescriptor>,
        /// Number of list containers enclosing this block when it nests
        /// inside a list item; 0 for a top-level block.
        list_depth: usize,
    },
    Image {
        alt: String,
        url: String,
        title: Option<String>,
        presentation: Option<ImagePresentation>,
        source_range: Range<usize>,
        identity: ImageSourceIdentity,
    },
    Rule {
        source_range: Range<usize>,
    },
    Table {
        rows: Vec<Vec<RichText>>,
        /// Per-column alignment from the separator row, as parsed upstream.
        alignments: Vec<TableAlignment>,
        source_range: Range<usize>,
        /// Number of list containers enclosing this block when it nests
        /// inside a list item; 0 for a top-level block.
        list_depth: usize,
    },
    FootnoteDefinition {
        label: String,
        text: RichText,
        source_range: Range<usize>,
    },
    /// Term line of a definition list (`Term` followed by `: definition`).
    DefinitionTerm {
        text: RichText,
        source_range: Range<usize>,
    },
    /// One definition of a term. Inline content (directly or from the
    /// definition's paragraphs) is flattened into `text`; the range starts at
    /// the `:` marker and stops before any nested non-paragraph block.
    DefinitionDetail {
        text: RichText,
        source_range: Range<usize>,
    },
    /// Standalone `[TOC]` / `[toc]` paragraph. Source stays the token.
    TableOfContents {
        source_range: Range<usize>,
    },
}

impl PreviewBlock {
    /// Byte range of this block in the original document text.
    pub fn source_range(&self) -> &Range<usize> {
        match self {
            Self::Heading { source_range, .. }
            | Self::Paragraph { source_range, .. }
            | Self::ListItem { source_range, .. }
            | Self::BlockQuote { source_range, .. }
            | Self::CodeBlock { source_range, .. }
            | Self::MathBlock { source_range, .. }
            | Self::Html { source_range, .. }
            | Self::Image { source_range, .. }
            | Self::Rule { source_range }
            | Self::Table { source_range, .. }
            | Self::FootnoteDefinition { source_range, .. }
            | Self::DefinitionTerm { source_range, .. }
            | Self::DefinitionDetail { source_range, .. }
            | Self::TableOfContents { source_range } => source_range,
        }
    }

    /// Number of list containers enclosing this block. Only the block kinds
    /// that can nest inside a list item (code, math, table, HTML) carry a
    /// depth; everything else reports 0.
    pub fn list_depth(&self) -> usize {
        match self {
            Self::CodeBlock { list_depth, .. }
            | Self::MathBlock { list_depth, .. }
            | Self::Html { list_depth, .. }
            | Self::Table { list_depth, .. } => *list_depth,
            _ => 0,
        }
    }

    /// Plain-text content of the block, including text nested inside child
    /// blocks (currently list items inside a blockquote).
    pub fn plain_text(&self) -> String {
        match self {
            Self::Heading { text, .. }
            | Self::Paragraph { text, .. }
            | Self::ListItem { text, .. }
            | Self::FootnoteDefinition { text, .. }
            | Self::DefinitionTerm { text, .. }
            | Self::DefinitionDetail { text, .. } => text.text.clone(),
            Self::BlockQuote { children, .. } => children
                .iter()
                .map(PreviewBlock::plain_text)
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n"),
            Self::CodeBlock { code, .. } => code.clone(),
            Self::MathBlock { latex, .. } => latex.clone(),
            Self::Html { html, .. } => html.clone(),
            Self::Image { alt, .. } => alt.clone(),
            Self::Rule { .. } | Self::TableOfContents { .. } => String::new(),
            Self::Table { rows, .. } => rows
                .iter()
                .flat_map(|row| row.iter())
                .map(|cell| cell.text.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
}

/// Column alignment declared by a Markdown table separator row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableAlignment {
    Default,
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkdownFormat {
    Bold,
    Italic,
    InlineCode,
    Link,
    Image,
    Paragraph,
    Heading(u8),
    UnorderedList,
    OrderedList,
    TaskList,
    BlockQuote,
    CodeFence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableEdit {
    Format,
    AddRow,
    DeleteRow,
    MoveRowUp,
    MoveRowDown,
    AddColumn,
    DeleteColumn,
}

/// Index-addressed structural table edit. Row 0 is the GFM header row; row
/// insert/duplicate/delete/move only accept body rows so the header survives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableStructureEdit {
    /// Insert an empty body row so it ends up at index `at` (`1..=rows`).
    InsertRow {
        at: usize,
    },
    DuplicateRow(usize),
    ClearRow(usize),
    DeleteRow(usize),
    /// Remove body row `from` and reinsert it at body index `to`.
    MoveRow {
        from: usize,
        to: usize,
    },
    /// Insert an empty column so it ends up at index `at` (`0..=columns`).
    InsertColumn {
        at: usize,
    },
    DuplicateColumn(usize),
    ClearColumn(usize),
    DeleteColumn(usize),
    /// Remove column `from` and reinsert it at index `to`.
    MoveColumn {
        from: usize,
        to: usize,
    },
    AlignColumn {
        column: usize,
        alignment: TableAlignment,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableEditResult {
    pub table_range: Range<usize>,
    pub selected_range: Range<usize>,
    pub row: usize,
    pub column: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct YamlFrontMatter {
    pub raw: String,
    pub title: Option<String>,
    pub author: Option<String>,
    pub date: Option<String>,
    pub values: serde_yaml::Mapping,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrontMatterError {
    pub(crate) message: String,
}

impl FrontMatterError {
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for FrontMatterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for FrontMatterError {}
