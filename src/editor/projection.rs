#![allow(clippy::too_many_lines, clippy::needless_pass_by_value, clippy::struct_excessive_bools)]

use super::{Range, EditorState, MarkdownDocument};
use markion_core::{
    model::{VisualProjection, VisualBlockKind, VisualBlockEditor, VisualEditorFieldKind, VisualProjectionSegment, VisualProjectionSpan, VisualCaretAffinity, VisualBlock, InlineStyle},
    visual::{build_visual_projection_with_marked_range, indented_code_display},
};

#[derive(Clone, Debug, Default)]
pub(super) struct Presentation {
    pub size: f32,
    pub bold: bool,
    pub code: bool,
    pub source: bool,
    pub marker: String,
    pub quote: bool,
    pub indent: usize,
    pub table: bool,
    pub image: Option<String>,
}
#[derive(Clone, Debug)]
pub(super) struct Cell {
    pub display: Range<usize>,
    pub presentation: Presentation,
}
#[derive(Clone, Debug, Default)]
pub(super) struct Row {
    pub cells: Vec<Cell>,
}

/// A layout boundary, not a source atom. Its gap can include hidden syntax.
#[derive(Clone, Debug)]
pub(super) struct Boundary {
    pub display: Range<usize>,
    pub before: usize,
    pub after: usize,
}
pub(super) struct Surface {
    pub projection: VisualProjection,
    pub rows: Vec<Row>,
    boundaries: Vec<Boundary>,
    appended_cells: usize,
}
impl Surface {
    pub fn new(state: &EditorState, source_mode: Option<Range<usize>>) -> Self {
        let document = &state.document;
        let source = document.text();
        let mut surface = Self {
            projection: VisualProjection {
                text: String::new(),
                segments: vec![],
                spans: vec![],
                revealed_source_ranges: vec![],
                source_anchor: 0,
            },
            rows: vec![],
            boundaries: vec![],
            appended_cells: 0,
        };
        for block in document.visual_blocks_shared().iter() {
            let mut presentation = Presentation {
                size: 15.0,
                quote: block.quote_context.is_some(),
                indent: block.list_depth.saturating_sub(1),
                ..Presentation::default()
            };
            match &block.kind {
                VisualBlockKind::Heading { level } => {
                    presentation.size = [27.0, 23.0, 20.0, 18.0, 17.0, 16.0]
                        [usize::from(level.saturating_sub(1)).min(5)];
                    presentation.bold = true;
                }
                VisualBlockKind::ListItem {
                    level,
                    index,
                    ordered,
                    checked,
                } => {
                    presentation.indent = level.saturating_sub(1);
                    presentation.marker = match checked {
                        Some(true) => "☑".into(),
                        Some(false) => "☐".into(),
                        None if *ordered => format!("{}.", index.unwrap_or(1)),
                        None => "•".into(),
                    };
                }
                VisualBlockKind::DefinitionTerm => presentation.bold = true,
                VisualBlockKind::DefinitionDetail => presentation.indent += 1,
                _ => {}
            }
            let expanded = source_mode.as_ref().is_some_and(|range| {
                range.start < block.source_range.end && range.end > block.source_range.start
            });
            let projection = if expanded {
                presentation.source = true;
                identity(source, block.source_range.clone(), true)
            } else if matches!(block.kind, VisualBlockKind::Whitespace) {
                let mut range = block.source_range.clone();
                // Interior/leading separators share their final newline with the
                // following row. A terminal gap retains every insertion position.
                if range.end < source.len() && source[range.clone()].ends_with('\n') {
                    range.end -= 1;
                    if source[range.clone()].ends_with('\r') {
                        range.end -= 1;
                    }
                }
                identity(source, range, false)
            } else if let Some(VisualBlockEditor::Table { cells }) = &block.editor {
                let mut row = Row::default();
                let mut row_index = 0;
                for cell in cells {
                    if !row.cells.is_empty() && cell.row != row_index {
                        surface.rows.push(row);
                        row = Row::default();
                    }
                    row_index = cell.row;
                    let range = cell.field.source_range.clone();
                    // Reuse the upstream parser/projection inside a proven cell field;
                    // never infer source offsets from rendered label lengths.
                    let projected = inline_cell(source, range);
                    let style = Presentation {
                        table: true,
                        bold: cell.row == 0,
                        ..presentation.clone()
                    };
                    let cell = surface.append_cell(
                        projected,
                        style,
                        if row.cells.is_empty() { '\n' } else { '\t' },
                    );
                    row.cells.push(cell);
                }
                if !row.cells.is_empty() {
                    surface.rows.push(row);
                }
                continue;
            } else if let Some(
                VisualBlockEditor::Code { payload, .. } | VisualBlockEditor::Math { payload, .. },
            ) = &block.editor
            {
                presentation.code = true;
                if payload.kind == VisualEditorFieldKind::IndentedCodePayload {
                    let (text, pairs) = indented_code_display(
                        &source[payload.source_range.clone()],
                        payload.source_range.start,
                    );
                    let mut projection = identity("", 0..0, false);
                    projection.text = text;
                    projection.source_anchor = payload.source_range.start;
                    projection.segments = pairs
                        .into_iter()
                        .map(|(display_range, source_range)| VisualProjectionSegment {
                            display_range,
                            source_range,
                            atomic: false,
                        })
                        .collect();
                    projection
                } else {
                    identity(source, payload.source_range.clone(), false)
                }
            } else {
                match &block.kind {
                    VisualBlockKind::Image { alt, url, .. } => {
                        presentation.image = Some(url.clone());
                        atom(
                            format!(
                                "▧ {}",
                                if alt.is_empty() {
                                    "Image (Ctrl+E to edit source)"
                                } else {
                                    alt
                                }
                            ),
                            block.source_range.clone(),
                        )
                    }
                    VisualBlockKind::Rule => {
                        atom("────────────────".into(), block.source_range.clone())
                    }
                    VisualBlockKind::CalloutTitle { kind, title } => {
                        presentation.bold = true;
                        atom(
                            title.clone().unwrap_or_else(|| format!("{kind:?}")),
                            block.source_range.clone(),
                        )
                    }
                    VisualBlockKind::Html { .. }
                    | VisualBlockKind::FrontMatter { .. }
                    | VisualBlockKind::Unsupported
                    | VisualBlockKind::ReferenceDefinition => {
                        presentation.source = true;
                        identity(source, block.source_range.clone(), true)
                    }
                    _ if block.source_island.is_some() => {
                        presentation.source = true;
                        identity(source, block.source_range.clone(), true)
                    }
                    _ => stable(
                        source,
                        block,
                        state.selected_range.clone(),
                        if state.selection_reversed {
                            state.selected_range.start
                        } else {
                            state.selected_range.end
                        },
                    ),
                }
            };
            surface.append_lines(projection, presentation);
        }
        if surface.rows.is_empty() {
            surface.append_lines(
                identity(source, 0..source.len(), false),
                Presentation {
                    size: 15.0,
                    ..Presentation::default()
                },
            );
        }
        surface
    }
    fn append_cell(
        &mut self,
        mut projection: VisualProjection,
        presentation: Presentation,
        separator: char,
    ) -> Cell {
        if projection.segments.is_empty() {
            projection.segments.push(VisualProjectionSegment {
                display_range: 0..0,
                source_range: projection.source_anchor..projection.source_anchor,
                atomic: false,
            });
        }
        if self.appended_cells != 0 {
            let previous = self
                .projection
                .segments
                .last()
                .map_or(0, |s| s.source_range.end);
            let next = projection
                .segments
                .first()
                .map_or(projection.source_anchor, |s| s.source_range.start);
            let start = self.projection.text.len();
            self.projection.text.push(separator);
            let end = self.projection.text.len();
            self.boundaries.push(Boundary {
                display: start..end,
                before: previous,
                after: next,
            });
            // Only endpoint anchors belong to the projection. The synthetic
            // newline/tab must never masquerade as an editable source token.
            self.projection.segments.extend([
                VisualProjectionSegment {
                    display_range: start..start,
                    source_range: previous..previous,
                    atomic: false,
                },
                VisualProjectionSegment {
                    display_range: end..end,
                    source_range: next..next,
                    atomic: false,
                },
            ]);
        }
        self.appended_cells += 1;
        let start = self.projection.text.len();
        self.projection.text.push_str(&projection.text);
        for segment in &mut projection.segments {
            segment.display_range =
                segment.display_range.start + start..segment.display_range.end + start;
        }
        for span in &mut projection.spans {
            span.display_range = span.display_range.start + start..span.display_range.end + start;
        }
        self.projection.segments.extend(projection.segments);
        self.projection.spans.extend(projection.spans);
        Cell {
            display: start..self.projection.text.len(),
            presentation,
        }
    }
    fn append_lines(&mut self, projection: VisualProjection, presentation: Presentation) {
        // Native paragraph hit_test returns a byte offset within a logical line,
        // so each paragraph owns exactly one logical line (wrapping is native).
        let mut start = 0;
        for line in projection.text.split('\n') {
            let end = start + line.len();
            let mut part = VisualProjection {
                text: line.to_owned(),
                segments: vec![],
                spans: vec![],
                revealed_source_ranges: vec![],
                source_anchor: projection.source_for_display(start),
            };
            for segment in &projection.segments {
                let a = start.max(segment.display_range.start);
                let b = end.min(segment.display_range.end);
                if a < b || (a == b && segment.display_range.is_empty() && a >= start && a <= end) {
                    let source_range = if !segment.atomic
                        && segment.display_range.len() == segment.source_range.len()
                    {
                        segment.source_range.start + a - segment.display_range.start
                            ..segment.source_range.start + b - segment.display_range.start
                    } else {
                        segment.source_range.clone()
                    };
                    part.segments.push(VisualProjectionSegment {
                        display_range: a - start..b - start,
                        source_range,
                        atomic: segment.atomic,
                    });
                }
            }
            if part.segments.is_empty() {
                part.segments.push(VisualProjectionSegment {
                    display_range: 0..0,
                    source_range: part.source_anchor..part.source_anchor,
                    atomic: false,
                });
            }
            for span in &projection.spans {
                let a = start.max(span.display_range.start);
                let b = end.min(span.display_range.end);
                if a < b {
                    part.spans.push(VisualProjectionSpan {
                        display_range: a - start..b - start,
                        ..span.clone()
                    });
                }
            }
            let mut style = presentation.clone();
            if start != 0 {
                style.marker.clear();
            }
            let cell = self.append_cell(part, style, '\n');
            self.rows.push(Row { cells: vec![cell] });
            start = end + 1;
        }
    }
    pub fn source_at(&self, display: usize, affinity: VisualCaretAffinity) -> usize {
        self.projection
            .source_for_display_with_affinity(display, affinity)
    }
    pub fn display_at(&self, source: usize) -> usize {
        self.projection.display_for_source(source).unwrap_or(0)
    }
    pub fn boundary(&self, display: &Range<usize>) -> Option<&Boundary> {
        self.boundaries.iter().find(|boundary| {
            boundary.display.start < display.end && boundary.display.end > display.start
        })
    }
    pub fn edit_range(&self, display: Range<usize>) -> Option<Range<usize>> {
        if self.boundary(&display).is_some() {
            return None;
        }
        let mut start = self.source_at(display.start, VisualCaretAffinity::Downstream);
        let mut end = self.source_at(display.end, VisualCaretAffinity::Upstream);
        for segment in &self.projection.segments {
            if segment.display_range.start < display.end
                && segment.display_range.end > display.start
                && (segment.atomic || segment.display_range.len() != segment.source_range.len())
            {
                start = start.min(segment.source_range.start);
                end = end.max(segment.source_range.end);
            }
        }
        Some(start.min(end)..start.max(end))
    }
}
fn stable(
    source: &str,
    block: &VisualBlock,
    selection: Range<usize>,
    cursor: usize,
) -> VisualProjection {
    let mut block = block.clone();
    // Structural prefixes are decorations, not editable glyphs. Empty inline
    // delimiter groups still use upstream's stable-projection reveal policy.
    block.block_prefix = None;
    block.quote_context = None;
    build_visual_projection_with_marked_range(source, &block, selection, cursor, None, false)
}
fn identity(source: &str, range: Range<usize>, raw: bool) -> VisualProjection {
    if raw {
        let tokens = markion_core::visual::data_uri_payload_ranges(source, range.clone());
        if !tokens.is_empty() {
            let mut projection = VisualProjection {
                text: String::new(),
                segments: vec![],
                spans: vec![],
                revealed_source_ranges: vec![],
                source_anchor: range.start,
            };
            let mut cursor = range.start;
            for token in tokens {
                for (part, atomic) in [(cursor..token.start, false), (token.clone(), true)] {
                    let start = projection.text.len();
                    if atomic {
                        projection
                            .text
                            .push_str(&markion_core::visual::elided_payload_token(part.len()));
                    } else {
                        projection.text.push_str(&source[part.clone()]);
                    }
                    let display_range = start..projection.text.len();
                    projection.segments.push(VisualProjectionSegment {
                        display_range: display_range.clone(),
                        source_range: part,
                        atomic,
                    });
                    projection.spans.push(VisualProjectionSpan {
                        display_range,
                        style: InlineStyle {
                            code: atomic,
                            ..InlineStyle::default()
                        },
                        link: false,
                        source: true,
                    });
                }
                cursor = token.end;
            }
            let start = projection.text.len();
            projection.text.push_str(&source[cursor..range.end]);
            projection.segments.push(VisualProjectionSegment {
                display_range: start..projection.text.len(),
                source_range: cursor..range.end,
                atomic: false,
            });
            projection.spans.push(VisualProjectionSpan {
                display_range: start..projection.text.len(),
                style: InlineStyle::default(),
                link: false,
                source: true,
            });
            return projection;
        }
    }

    VisualProjection {
        text: source[range.clone()].to_owned(),
        segments: vec![VisualProjectionSegment {
            display_range: 0..range.len(),
            source_range: range.clone(),
            atomic: false,
        }],
        spans: vec![VisualProjectionSpan {
            display_range: 0..range.len(),
            style: InlineStyle::default(),
            link: false,
            source: raw,
        }],
        revealed_source_ranges: vec![],
        source_anchor: range.start,
    }
}
fn atom(text: String, source: Range<usize>) -> VisualProjection {
    VisualProjection {
        segments: vec![VisualProjectionSegment {
            display_range: 0..text.len(),
            source_range: source.clone(),
            atomic: true,
        }],
        spans: vec![],
        text,
        revealed_source_ranges: vec![],
        source_anchor: source.start,
    }
}

fn inline_cell(source: &str, range: Range<usize>) -> VisualProjection {
    // A plain prefix forces inline parsing: `# title`, `- text`, or a lone
    // image in a table cell must not become a heading/list/block image.
    const PREFIX: &str = "x ";
    let local = MarkdownDocument::from_text(format!("{PREFIX}{}", &source[range.clone()]));
    let blocks = local.visual_blocks_shared();
    let Some(block) = blocks.first() else {
        return identity(source, range, false);
    };
    let mut projection = stable(local.text(), block, usize::MAX..usize::MAX, usize::MAX);
    if !projection.text.starts_with(PREFIX) {
        return identity(source, range, true);
    }
    projection.text.drain(..PREFIX.len());
    projection
        .segments
        .retain(|s| s.display_range.end > PREFIX.len());
    for segment in &mut projection.segments {
        segment.display_range = segment.display_range.start.saturating_sub(PREFIX.len())
            ..segment.display_range.end - PREFIX.len();
        segment.source_range = segment.source_range.start.max(PREFIX.len()) - PREFIX.len()
            + range.start
            ..segment.source_range.end - PREFIX.len() + range.start;
    }
    projection
        .spans
        .retain(|s| s.display_range.end > PREFIX.len());
    for span in &mut projection.spans {
        span.display_range = span.display_range.start.saturating_sub(PREFIX.len())
            ..span.display_range.end - PREFIX.len();
    }
    projection.source_anchor = range.start;
    projection
}
