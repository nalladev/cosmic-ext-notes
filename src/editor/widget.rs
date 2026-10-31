use super::*;
use cosmic::iced::core::image;
use cosmic::{
    Renderer, Theme,
    iced::{
        self,
        core::{
            self, Border, Clipboard, Color, Event, Font, InputMethod, Length, Pixels, Point,
            Rectangle, Shell, Size, Vector, alignment, clipboard, font, input_method, keyboard,
            layout::{self, Layout},
            mouse, renderer,
            text::{self, Paragraph as _, Renderer as _},
            widget::{self, Tree, Widget},
        },
    },
};
use std::time::Duration;

type Decorations = Vec<(usize, bool, bool, bool, bool)>;

type Paragraph = <Renderer as text::Renderer>::Paragraph;

pub(super) struct VisualEditor<'a> {
    editor: &'a Editor,
    link_color: Color,
    accent_revision: u64,
}
impl<'a> VisualEditor<'a> {
    pub fn new(editor: &'a Editor, link_color: Color, accent_revision: u64) -> Self {
        Self {
            editor,
            link_color,
            accent_revision,
        }
    }
}

struct LaidCell {
    paragraph: Paragraph,
    display: Range<usize>,
    bounds: Rectangle,
    text_origin: Point,
    marker: String,
    quote: bool,
    shaded: bool,
    table: bool,
    size: f32,
    decorations: Decorations,
    image: Option<(image::Handle, Rectangle)>,
}
#[derive(Default)]
struct State {
    cells: Vec<LaidCell>,
    owner: Option<EditorOwner>,
    cache: Option<(EditorOwner, u64, usize, usize, u32, u64)>,
    focused: bool,
    window_focused: bool,
    dragging: bool,
    modifiers: keyboard::Modifiers,
    click: Option<mouse::Click>,
    preedit: Option<input_method::Preedit>,
    ime_session: Option<ImeOwner>,
    ime_blocked: bool,
    desired_x: Option<f32>,
    blink: bool,
    blink_at: Option<iced::time::Instant>,
    scroll: f32,
    content_height: f32,
    viewport_height: f32,
    followed: Option<(EditorOwner, u64, usize)>,
}
impl State {
    fn bind(&mut self, editor: &Editor) -> bool {
        let owner = editor.owner();
        if self.owner.as_ref() == Some(&owner) {
            return false;
        }
        let window_focused = self.window_focused;
        *self = Self {
            owner: Some(owner),
            window_focused,
            blink: true,
            ..Self::default()
        };
        true
    }
    fn invalidate_ime(&mut self) {
        self.preedit = None;
        self.ime_session = None;
        self.ime_blocked = true;
    }
    fn hit(&self, point: Point) -> (usize, VisualCaretAffinity) {
        let Some(cell) = self.cells.iter().min_by(|a, b| {
            let (ay, ax) = distance(a.bounds, point);
            let (by, bx) = distance(b.bounds, point);
            ay.total_cmp(&by).then_with(|| ax.total_cmp(&bx))
        }) else {
            return (0, VisualCaretAffinity::Downstream);
        };
        let local = Point::new(point.x - cell.text_origin.x, point.y - cell.text_origin.y);
        let hit = cell.paragraph.hit_test(local);
        let byte = cell.display.start
            + hit.map_or_else(
                || {
                    if local.y <= 0.0 {
                        0
                    } else {
                        cell.display.len()
                    }
                },
                |hit| hit.cursor().min(cell.display.len()),
            );
        let affinity = match hit.map(|hit| hit.affinity()) {
            Some(text::Affinity::Before) => VisualCaretAffinity::Upstream,
            _ => VisualCaretAffinity::Downstream,
        };
        (byte, affinity)
    }
    fn caret(&self, display: usize) -> Option<(Point, f32)> {
        let cell = self
            .cells
            .iter()
            .find(|cell| display >= cell.display.start && display <= cell.display.end)?;
        let point = cell
            .paragraph
            .cursor_position(0, display - cell.display.start, text::Affinity::After)
            .unwrap_or(Point::ORIGIN);
        Some((
            point + Vector::new(cell.text_origin.x, cell.text_origin.y),
            cell.size * 1.4,
        ))
    }
}
fn distance(bounds: Rectangle, point: Point) -> (f32, f32) {
    let dx = (bounds.x - point.x)
        .max(0.0)
        .max(point.x - bounds.x - bounds.width);
    let dy = (bounds.y - point.y)
        .max(0.0)
        .max(point.y - bounds.y - bounds.height);
    (dy, dx)
}

impl Widget<Message, Theme, Renderer> for VisualEditor<'_> {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }
    fn tag(&self) -> widget::tree::Tag {
        widget::tree::Tag::of::<State>()
    }
    fn state(&self) -> widget::tree::State {
        widget::tree::State::new(State {
            window_focused: true,
            blink: true,
            ..State::default()
        })
    }
    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        _renderer: &Renderer,
        operation: &mut dyn widget::Operation,
    ) {
        let state = tree.state.downcast_mut::<State>();
        state.bind(self.editor);
        operation.focusable(Some(&self.editor.id), layout.bounds(), state);
    }
    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout_surface(
            self.editor,
            tree.state.downcast_mut::<State>(),
            renderer.default_font(),
            limits,
            self.link_color,
            self.accent_revision,
        )
    }
    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        defaults: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        use core::Renderer as _;
        let state = tree.state.downcast_ref::<State>();
        let origin = layout.bounds().position();
        let shift = Vector::new(origin.x, origin.y - state.scroll);
        let Some(clip) = viewport.intersection(&layout.bounds()) else {
            return;
        };
        let accent = theme.cosmic().accent_color();
        let selected = self.editor.selection();
        let selection = self.editor.surface.display_at(selected.start)
            ..self.editor.surface.display_at(selected.end);
        renderer.with_layer(clip, |renderer| {
            for cell in &state.cells {
                let bounds = cell.bounds + shift;
                if !bounds.intersects(&clip) {
                    continue;
                }
                if cell.shaded || cell.table {
                    renderer.fill_quad(
                        renderer::Quad {
                            bounds,
                            border: Border {
                                color: defaults.text_color.scale_alpha(0.18),
                                width: if cell.table { 1.0 } else { 0.0 },
                                radius: 3.0.into(),
                            },
                            ..renderer::Quad::default()
                        },
                        defaults.text_color.scale_alpha(0.045),
                    );
                }
                if cell.quote {
                    renderer.fill_quad(
                        renderer::Quad {
                            bounds: Rectangle::new(
                                Point::new(bounds.x + 3.0, bounds.y),
                                Size::new(3.0, bounds.height),
                            ),
                            ..renderer::Quad::default()
                        },
                        defaults.text_color.scale_alpha(0.3),
                    );
                }
                if let Some((handle, image_bounds)) = &cell.image {
                    use image::Renderer as _;
                    let bounds = *image_bounds + shift;
                    let measured = renderer.measure_image(handle);
                    if let Some(size) = measured.filter(|s| s.width > 0 && s.height > 0) {
                        let scale = (bounds.width / size.width as f32)
                            .min(bounds.height / size.height as f32)
                            .min(1.0);
                        let fitted = Rectangle::new(
                            bounds.position(),
                            Size::new(size.width as f32 * scale, size.height as f32 * scale),
                        );
                        renderer.draw_image(image::Image::new(handle), fitted, clip);
                    } else {
                        renderer.draw_image(image::Image::new(handle), bounds, clip);
                    }
                }
                let position = cell.text_origin + shift;
                if !cell.marker.is_empty() {
                    renderer.fill_text(
                        text::Text {
                            content: cell.marker.clone(),
                            bounds: Size::new(27.0, cell.size * 1.4),
                            size: Pixels(cell.size),
                            line_height: text::LineHeight::Relative(1.32),
                            font: renderer.default_font(),
                            align_x: text::Alignment::Default,
                            align_y: alignment::Vertical::Top,
                            shaping: text::Shaping::Advanced,
                            wrapping: text::Wrapping::None,
                            ellipsize: text::Ellipsize::None,
                        },
                        Point::new(position.x - 26.0, position.y),
                        defaults.text_color,
                        clip,
                    );
                }
                let a = selection.start.max(cell.display.start);
                let b = selection.end.min(cell.display.end);
                if a < b {
                    for rectangle in cell.paragraph.highlight(
                        0,
                        (a - cell.display.start, text::Affinity::After),
                        (b - cell.display.start, text::Affinity::Before),
                    ) {
                        renderer.fill_quad(
                            renderer::Quad {
                                bounds: rectangle + Vector::new(position.x, position.y),
                                ..renderer::Quad::default()
                            },
                            Color::from(accent).scale_alpha(if state.focused {
                                0.28
                            } else {
                                0.15
                            }),
                        );
                    }
                }
                // Native spans carry fonts, sizes and colors. Decorations use the
                // exact native span bounds rather than estimated character widths.
                for (index, underline, strike, highlight, link) in &cell.decorations {
                    for rectangle in cell.paragraph.span_bounds(*index) {
                        let bounds = rectangle + Vector::new(position.x, position.y);
                        if *highlight {
                            renderer.fill_quad(
                                renderer::Quad {
                                    bounds,
                                    ..renderer::Quad::default()
                                },
                                defaults.text_color.scale_alpha(0.12),
                            );
                        }
                        for y in [
                            underline.then_some(bounds.y + bounds.height - 2.0),
                            strike.then_some(bounds.y + bounds.height * 0.54),
                        ]
                        .into_iter()
                        .flatten()
                        {
                            renderer.fill_quad(
                                renderer::Quad {
                                    bounds: Rectangle::new(
                                        Point::new(bounds.x, y),
                                        Size::new(bounds.width, 1.0),
                                    ),
                                    ..renderer::Quad::default()
                                },
                                if *link {
                                    self.link_color
                                } else {
                                    defaults.text_color
                                }
                                .scale_alpha(0.7),
                            );
                        }
                    }
                }
                renderer.fill_paragraph(&cell.paragraph, position, defaults.text_color, clip);
            }
            #[allow(clippy::collapsible_if)]
            if state.focused && state.window_focused && state.blink {
                if let Some((point, height)) =
                    state.caret(self.editor.surface.display_at(self.editor.caret()))
                {
                    renderer.fill_quad(
                        renderer::Quad {
                            bounds: Rectangle::new(point + shift, Size::new(1.5, height)),
                            ..renderer::Quad::default()
                        },
                        defaults.text_color,
                    );
                }
            }
        });
    }
    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        route(
            self.editor,
            tree.state.downcast_mut::<State>(),
            event,
            layout,
            cursor,
            clipboard,
            shell,
            viewport,
        );
    }
    fn mouse_interaction(
        &self,
        _tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        if cursor.is_over(layout.bounds()) {
            mouse::Interaction::Text
        } else {
            mouse::Interaction::default()
        }
    }
}
impl<'a> From<VisualEditor<'a>> for cosmic::Element<'a, Message> {
    fn from(widget: VisualEditor<'a>) -> Self {
        Self::new(widget)
    }
}

fn header_tag_ranges(line: &str) -> Vec<std::ops::Range<usize>> {
    let tokens: Vec<_> = line.split_whitespace().collect();
    if tokens.is_empty()
        || !tokens
            .iter()
            .all(|token| token.starts_with('#') && token.len() > 1)
    {
        return Vec::new();
    }

    let mut ranges = Vec::with_capacity(tokens.len());
    let mut offset = 0;
    for token in tokens {
        let start = line[offset..].find(token).expect("split token is in line") + offset;
        ranges.push(start..start + token.len());
        offset = start + token.len();
    }
    ranges
}

fn spans<'a>(
    editor: &'a Editor,
    cell: &projection::Cell,
    base_font: Font,
    link_color: Color,
) -> (Vec<text::Span<'a>>, Decorations) {
    let range = &cell.display;
    let mut boundaries = vec![range.start, range.end];
    for span in &editor.surface.projection.spans {
        if span.display_range.start < range.end && span.display_range.end > range.start {
            boundaries.push(span.display_range.start.max(range.start));
            boundaries.push(span.display_range.end.min(range.end));
        }
    }
    let first_line_end = editor
        .surface
        .projection
        .text
        .find('\n')
        .unwrap_or(editor.surface.projection.text.len());
    let header_tags = header_tag_ranges(&editor.surface.projection.text[..first_line_end]);
    for tag in &header_tags {
        if tag.start < range.end && tag.end > range.start {
            boundaries.push(tag.start.max(range.start));
            boundaries.push(tag.end.min(range.end));
        }
    }
    boundaries.sort_unstable();
    boundaries.dedup();
    let mut spans = vec![];
    let mut decorations = vec![];
    for pair in boundaries.windows(2) {
        let start = pair[0];
        let end = pair[1];
        let highlight = editor
            .surface
            .projection
            .spans
            .iter()
            .find(|s| s.display_range.start <= start && s.display_range.end >= end);
        let style = highlight.map(|s| s.style).unwrap_or_default();
        let raw = cell.presentation.source || highlight.is_some_and(|s| s.source);
        let link = highlight.is_some_and(|s| s.link);
        let tag = header_tags
            .iter()
            .any(|tag| tag.start <= start && tag.end >= end);
        let font = Font {
            family: if style.code || raw || cell.presentation.code {
                font::Family::Monospace
            } else {
                base_font.family
            },
            weight: if style.bold || cell.presentation.bold {
                font::Weight::Bold
            } else {
                base_font.weight
            },
            style: if style.italic {
                font::Style::Italic
            } else {
                font::Style::Normal
            },
            ..base_font
        };
        let mut span = text::Span::new(&editor.surface.projection.text[start..end]).font(font);
        if link || tag {
            span.color = Some(link_color);
        }
        if !link {
            if let Some(color) = style.color {
                span.color = Some(Color::from_rgb8(
                    (color >> 16) as u8,
                    (color >> 8) as u8,
                    color as u8,
                ));
            }
        }
        if style.superscript || style.subscript {
            span.size = Some(Pixels(cell.presentation.size * 0.8));
        }
        decorations.push((
            spans.len(),
            style.underline || link,
            style.strikethrough,
            style.highlight || style.code,
            link,
        ));
        spans.push(span);
    }
    if spans.is_empty() {
        spans.push(text::Span::new(""));
    }
    (spans, decorations)
}

fn layout_surface(
    editor: &Editor,
    state: &mut State,
    base_font: Font,
    limits: &layout::Limits,
    link_color: Color,
    accent_revision: u64,
) -> layout::Node {
    state.bind(editor);
    let width = limits.max().width.max(80.0);
    let key = (
        editor.owner(),
        editor.state.document.version(),
        editor.surface_revision,
        editor.image_revision,
        width.to_bits(),
        accent_revision,
    );
    if state
        .ime_session
        .as_ref()
        .is_some_and(|owner| !editor.ime_owner_is_current(owner))
    {
        state.invalidate_ime();
    }
    if !editor.composition {
        state.preedit = None;
    }
    if state.cache.as_ref() != Some(&key) {
        state.cells.clear();
        let mut y = 4.0;
        for row in &editor.surface.rows {
            let count = row.cells.len().max(1);
            let column_width = (width - 48.0) / count as f32;
            let mut height: f32 = 0.0;
            let row_start = state.cells.len();
            for (column, cell) in row.cells.iter().enumerate() {
                let p = &cell.presentation;
                let x = 24.0 + column as f32 * column_width;
                let inset = if p.table {
                    9.0
                } else {
                    p.indent as f32 * 20.0
                        + if p.marker.is_empty() && !p.quote {
                            0.0
                        } else {
                            28.0
                        }
                };
                let image_handle = p.image.as_ref().and_then(|url| editor.image_handle(url));
                let image_height = if image_handle.is_some() { 188.0 } else { 0.0 };
                let text_origin = Point::new(
                    x + inset,
                    y + image_height
                        + if p.table || p.code || p.source {
                            7.0
                        } else {
                            0.0
                        },
                );
                let available = (column_width - inset - 12.0).max(20.0);
                let (spans, decorations) = spans(editor, cell, base_font, link_color);
                let paragraph = Paragraph::with_spans(text::Text {
                    content: &spans,
                    bounds: Size::new(available, f32::INFINITY),
                    size: Pixels(p.size),
                    line_height: text::LineHeight::Relative(1.32),
                    font: base_font,
                    align_x: text::Alignment::Default,
                    align_y: alignment::Vertical::Top,
                    shaping: text::Shaping::Advanced,
                    wrapping: text::Wrapping::WordOrGlyph,
                    ellipsize: text::Ellipsize::None,
                });
                let cell_height = image_height
                    + paragraph.min_height().max(p.size * 1.4)
                    + if p.table || p.code || p.source {
                        14.0
                    } else {
                        7.0
                    };
                height = height.max(cell_height);
                state.cells.push(LaidCell {
                    paragraph,
                    display: cell.display.clone(),
                    bounds: Rectangle::new(Point::new(x, y), Size::new(column_width, cell_height)),
                    text_origin,
                    marker: p.marker.clone(),
                    quote: p.quote,
                    shaded: p.code || p.source,
                    table: p.table,
                    size: p.size,
                    decorations,
                    image: image_handle.map(|handle| {
                        (
                            handle,
                            Rectangle::new(
                                Point::new(x + inset, y + 4.0),
                                Size::new(available, 180.0),
                            ),
                        )
                    }),
                });
            }
            for cell in &mut state.cells[row_start..] {
                cell.bounds.height = height;
            }
            y += height;
        }
        state.cache = Some(key);
    }
    let height = state
        .cells
        .last()
        .map_or(48.0, |cell| cell.bounds.y + cell.bounds.height + 24.0);
    state.content_height = height;
    let height = if limits.max().height.is_finite() {
        limits.max().height
    } else {
        height
    }
    .max(limits.min().height);
    state.viewport_height = height;
    let followed = (
        editor.owner(),
        editor.state.document.version(),
        editor.caret(),
    );
    if state.followed.as_ref() != Some(&followed) {
        if let Some((point, caret_height)) = state.caret(editor.surface.display_at(editor.caret()))
        {
            if point.y < state.scroll + 8.0 {
                state.scroll = (point.y - 8.0).max(0.0);
            }
            if point.y + caret_height > state.scroll + height - 8.0 {
                state.scroll = point.y + caret_height - height + 8.0;
            }
        }
        state.followed = Some(followed);
    }
    state.scroll = state
        .scroll
        .clamp(0.0, (state.content_height - height).max(0.0));
    layout::Node::new(Size::new(width, height))
}

#[allow(clippy::too_many_arguments)]
fn route(
    editor: &Editor,
    state: &mut State,
    event: &Event,
    layout: Layout<'_>,
    cursor: mouse::Cursor,
    clipboard: &mut dyn Clipboard,
    shell: &mut Shell<'_, Message>,
    viewport: &Rectangle,
) {
    if state.bind(editor) {
        shell.invalidate_layout();
        shell.request_redraw();
        return;
    }
    let origin = layout.bounds().position();
    let scroll = state.scroll;
    let relative = |point: Point| Point::new(point.x - origin.x, point.y - origin.y + scroll);
    let version = editor.state.document.version();
    let revision = editor.surface_revision;
    match event {
        Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) => {
            state.modifiers = *modifiers
        }
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
            if let Some(point) = cursor
                .position_over(layout.bounds())
                .filter(|point| viewport.contains(*point))
            {
                state.focused = true;
                state.window_focused = true;
                state.dragging = true;
                state.desired_x = None;
                let click = mouse::Click::new(point, mouse::Button::Left, state.click);
                state.click = Some(click);
                let (display, affinity) = state.hit(relative(point));
                shell.publish(Message::Hit {
                    version,
                    revision,
                    display,
                    affinity,
                    extend: state.modifiers.shift(),
                    word: click.kind() == mouse::click::Kind::Double,
                });
                if click.kind() == mouse::click::Kind::Triple {
                    shell.publish(Message::Action(Action::SelectLine));
                }
                // Task checkbox is decoration; target its row before toggling.
                if state.cells.iter().any(|cell| {
                    cell.marker.starts_with(['☑', '☐'])
                        && cell.bounds.contains(relative(point))
                        && relative(point).x < cell.text_origin.x
                }) {
                    shell.publish(Message::ToggleTask);
                }
                shell.capture_event();
            } else {
                state.focused = false;
                state.dragging = false;
                state.invalidate_ime();
                shell.publish(Message::Composition(None));
            }
            state.blink = true;
            state.blink_at = None;
            shell.request_redraw();
        }
        Event::Mouse(mouse::Event::CursorMoved { position }) if state.dragging => {
            let (display, affinity) = state.hit(relative(*position));
            shell.publish(Message::Hit {
                version,
                revision,
                display,
                affinity,
                extend: true,
                word: false,
            });
            shell.capture_event();
        }
        Event::Mouse(mouse::Event::WheelScrolled { delta }) if cursor.is_over(layout.bounds()) => {
            let dy = match delta {
                mouse::ScrollDelta::Lines { y, .. } => y * 40.0,
                mouse::ScrollDelta::Pixels { y, .. } => *y,
            };
            state.scroll = (state.scroll - dy)
                .clamp(0.0, (state.content_height - state.viewport_height).max(0.0));
            shell.capture_event();
            shell.request_redraw();
        }
        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => state.dragging = false,
        Event::Window(iced::window::Event::Unfocused) => {
            state.window_focused = false;
            state.dragging = false;
            state.invalidate_ime();
            shell.publish(Message::Composition(None));
        }
        Event::Window(iced::window::Event::Focused) => state.window_focused = true,
        Event::Window(iced::window::Event::RedrawRequested(now))
            if state.focused && state.window_focused =>
        {
            let last = state.blink_at.get_or_insert(*now);
            if now.saturating_duration_since(*last) >= Duration::from_millis(500) {
                state.blink = !state.blink;
                *last = *now;
            }
            shell.request_redraw_at(*last + Duration::from_millis(500));
            if let Some((point, height)) = state.caret(editor.surface.display_at(editor.caret())) {
                shell.request_input_method(&InputMethod::Enabled {
                    cursor: Rectangle::new(
                        point + Vector::new(origin.x, origin.y - state.scroll),
                        Size::new(1.5, height),
                    ),
                    purpose: input_method::Purpose::Normal,
                    preedit: state.preedit.as_ref().map(input_method::Preedit::as_ref),
                });
            }
        }
        Event::InputMethod(event) if state.focused => {
            match event {
                input_method::Event::Opened => {
                    let owner = editor.ime_owner();
                    state.ime_session = Some(owner.clone());
                    state.ime_blocked = false;
                    state.preedit = Some(input_method::Preedit::new());
                    shell.publish(Message::Ime {
                        owner,
                        event: ImeEvent::Opened,
                    });
                }
                input_method::Event::Preedit(content, selection) => {
                    if state.ime_session.is_none() && !state.ime_blocked && !content.is_empty() {
                        state.ime_session = Some(editor.ime_owner());
                    }
                    if let Some(owner) = state.ime_session.clone() {
                        if editor.ime_owner_is_current(&owner) {
                            state.preedit = Some(input_method::Preedit {
                                content: content.clone(),
                                selection: selection.clone(),
                                text_size: Some(Pixels(16.0)),
                            });
                            shell.publish(Message::Ime {
                                owner,
                                event: ImeEvent::Preedit(content.clone()),
                            });
                        } else {
                            state.invalidate_ime();
                        }
                    }
                }
                input_method::Event::Commit(text) => {
                    if let Some(owner) = state.ime_session.take() {
                        shell.publish(Message::Ime {
                            owner,
                            event: ImeEvent::Commit(text.clone()),
                        });
                    }
                    state.invalidate_ime();
                }
                input_method::Event::Closed => {
                    // Some platforms close the candidate window before sending
                    // the committed string. Keep the owner until Commit so a
                    // valid composition cannot be lost during that transition.
                    state.preedit = None;
                    state.ime_blocked = true;
                }
            }
            shell.capture_event();
            shell.request_redraw();
        }
        Event::Keyboard(keyboard::Event::KeyPressed {
            key,
            modifiers,
            text,
            ..
        }) if state.focused && state.window_focused => {
            if state
                .preedit
                .as_ref()
                .is_some_and(|p| !p.content.is_empty())
                && !matches!(
                    key.as_ref(),
                    keyboard::Key::Named(keyboard::key::Named::Escape)
                )
            {
                return;
            }
            state.blink = true;
            state.blink_at = None;
            let command = modifiers.command();
            let shift = modifiers.shift();
            let mut message = None;
            use keyboard::{Key, key::Named};
            match key.as_ref() {
                Key::Character("c" | "C") if command => {
                    if !editor.selection().is_empty() {
                        clipboard.write(
                            clipboard::Kind::Standard,
                            editor.text()[editor.selection()].to_owned(),
                        );
                    }
                }
                Key::Character("x" | "X") if command => {
                    if !editor.selection().is_empty() {
                        clipboard.write(
                            clipboard::Kind::Standard,
                            editor.text()[editor.selection()].to_owned(),
                        );
                        message = Some(Message::Action(Action::Edit(Edit::Delete)));
                    }
                }
                Key::Character("v" | "V") if command => {
                    message = clipboard.read(clipboard::Kind::Standard).map(|text| {
                        Message::Action(Action::Edit(Edit::Paste(std::sync::Arc::new(text))))
                    });
                }
                Key::Character("z" | "Z") if command => {
                    message = Some(if shift { Message::Redo } else { Message::Undo })
                }
                Key::Character("y" | "Y") if command => message = Some(Message::Redo),
                Key::Character("a" | "A") if command => {
                    message = Some(Message::Action(Action::SelectAll))
                }
                Key::Character("b" | "B") if command => {
                    message = Some(Message::Format(MarkdownFormat::Bold))
                }
                Key::Character("i" | "I") if command => {
                    message = Some(Message::Format(MarkdownFormat::Italic))
                }
                Key::Character("k" | "K") if command => {
                    message = Some(Message::Format(MarkdownFormat::Link))
                }
                Key::Character("e" | "E") if command => message = Some(Message::ToggleSource),
                Key::Named(Named::Tab) => message = Some(Message::Indent(!shift)),
                Key::Named(Named::Enter) => {
                    message = Some(Message::Action(Action::Edit(Edit::Enter)))
                }
                Key::Named(Named::Backspace | Named::Delete) => {
                    let forward = matches!(key.as_ref(), Key::Named(Named::Delete));
                    if command && editor.selection().is_empty() {
                        shell.publish(Message::Action(Action::Select(if forward {
                            Motion::WordRight
                        } else {
                            Motion::WordLeft
                        })));
                    }
                    message = Some(Message::Action(Action::Edit(if forward {
                        Edit::Delete
                    } else {
                        Edit::Backspace
                    })));
                }
                Key::Named(
                    Named::ArrowUp
                    | Named::ArrowDown
                    | Named::PageUp
                    | Named::PageDown
                    | Named::Home
                    | Named::End,
                ) if !command => {
                    if let Some((point, height)) =
                        state.caret(editor.surface.display_at(editor.caret()))
                    {
                        let x = *state.desired_x.get_or_insert(point.x);
                        let target = match key.as_ref() {
                            Key::Named(Named::ArrowUp) => Point::new(x, point.y - height * 0.5),
                            Key::Named(Named::ArrowDown) => Point::new(x, point.y + height * 1.5),
                            Key::Named(Named::PageUp) => Point::new(x, point.y - viewport.height),
                            Key::Named(Named::PageDown) => Point::new(x, point.y + viewport.height),
                            Key::Named(Named::Home) => {
                                state.desired_x = None;
                                Point::new(-10000.0, point.y + height * 0.5)
                            }
                            _ => {
                                state.desired_x = None;
                                Point::new(10000.0, point.y + height * 0.5)
                            }
                        };
                        let (display, affinity) = state.hit(target);
                        message = Some(Message::Hit {
                            version,
                            revision,
                            display,
                            affinity,
                            extend: shift,
                            word: false,
                        });
                    }
                }
                Key::Named(Named::ArrowLeft | Named::ArrowRight | Named::Home | Named::End) => {
                    state.desired_x = None;
                    let motion = match key.as_ref() {
                        Key::Named(Named::ArrowLeft) => {
                            if command {
                                Motion::WordLeft
                            } else {
                                Motion::Left
                            }
                        }
                        Key::Named(Named::ArrowRight) => {
                            if command {
                                Motion::WordRight
                            } else {
                                Motion::Right
                            }
                        }
                        Key::Named(Named::Home) => Motion::DocumentStart,
                        _ => Motion::DocumentEnd,
                    };
                    message = Some(Message::Action(if shift {
                        Action::Select(motion)
                    } else {
                        Action::Move(motion)
                    }));
                }
                Key::Named(Named::Escape) => {
                    state.invalidate_ime();
                    message = Some(Message::Composition(None));
                }
                _ if !command
                    && !modifiers.alt()
                    && state.preedit.as_ref().is_none_or(|p| p.content.is_empty()) =>
                {
                    message = text
                        .as_ref()
                        .filter(|s| !s.chars().any(char::is_control))
                        .map(|text| Message::Insert(text.to_string()));
                }
                _ => return,
            }
            if let Some(message) = message {
                shell.publish(message);
            }
            shell.capture_event();
            shell.request_redraw();
        }
        _ => {}
    }
}

/// Headless event harness: native paragraph layout and the exact Widget::update
/// router, without creating a window, GPU device, or alternate editing model.
#[cfg(test)]
pub(super) struct Harness {
    state: State,
    clipboard: TestClipboard,
    size: Size,
}
#[cfg(test)]
#[derive(Default)]
struct TestClipboard(Option<String>);
#[cfg(test)]
impl Clipboard for TestClipboard {
    fn read(&self, _: clipboard::Kind) -> Option<String> {
        self.0.clone()
    }
    fn write(&mut self, _: clipboard::Kind, text: String) {
        self.0 = Some(text);
    }
}
#[cfg(test)]
impl Harness {
    pub fn new(editor: &Editor, width: f32, height: f32) -> Self {
        let mut harness = Self {
            state: State {
                window_focused: true,
                ..State::default()
            },
            clipboard: TestClipboard::default(),
            size: Size::new(width, height),
        };
        harness.layout(editor);
        harness
    }
    fn layout(&mut self, editor: &Editor) -> layout::Node {
        layout_surface(
            editor,
            &mut self.state,
            Font::default(),
            &layout::Limits::new(Size::ZERO, self.size),
            Color::from_rgb(0.22, 0.52, 0.88),
            0,
        )
    }
    pub fn event(&mut self, editor: &mut Editor, event: Event, pointer: Option<Point>) {
        let node = self.layout(editor);
        let mut messages = vec![];
        let mut shell = Shell::new(&mut messages);
        route(
            editor,
            &mut self.state,
            &event,
            Layout::new(&node),
            pointer.map_or(mouse::Cursor::Unavailable, mouse::Cursor::Available),
            &mut self.clipboard,
            &mut shell,
            &Rectangle::new(Point::ORIGIN, self.size),
        );
        for message in messages {
            editor.update(message);
        }
        self.layout(editor);
    }
    pub fn point(&mut self, editor: &Editor, source: usize) -> Point {
        self.layout(editor);
        let (point, height) = self
            .state
            .caret(editor.surface.display_at(source))
            .expect("native caret geometry");
        Point::new(point.x, point.y + height * 0.5 - self.state.scroll)
    }
    pub fn click(&mut self, editor: &mut Editor, source: usize) {
        self.state.click = None;
        let point = self.point(editor, source);
        self.event(
            editor,
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            Some(point),
        );
        self.event(
            editor,
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            Some(point),
        );
    }
    pub fn key(
        &mut self,
        editor: &mut Editor,
        key: keyboard::Key,
        text: Option<&str>,
        modifiers: keyboard::Modifiers,
    ) {
        self.event(
            editor,
            Event::Keyboard(keyboard::Event::KeyPressed {
                modified_key: key.clone(),
                key,
                physical_key: keyboard::key::Physical::Unidentified(
                    keyboard::key::NativeCode::Unidentified,
                ),
                location: keyboard::Location::Standard,
                modifiers,
                text: text.map(Into::into),
                repeat: false,
            }),
            None,
        );
    }
    pub fn clipboard(&self) -> Option<&str> {
        self.clipboard.0.as_deref()
    }
    pub fn set_clipboard(&mut self, text: &str) {
        self.clipboard.0 = Some(text.into());
    }
    pub fn scroll(&self) -> f32 {
        self.state.scroll
    }
    pub fn native_highlights(&self, editor: &Editor) -> usize {
        let selected = editor.selection();
        let start = editor.surface.display_at(selected.start);
        let end = editor.surface.display_at(selected.end);
        self.state
            .cells
            .iter()
            .map(|cell| {
                let a = start.max(cell.display.start);
                let b = end.min(cell.display.end);
                if a < b {
                    cell.paragraph
                        .highlight(
                            0,
                            (a - cell.display.start, text::Affinity::After),
                            (b - cell.display.start, text::Affinity::Before),
                        )
                        .len()
                } else {
                    0
                }
            })
            .sum()
    }
}

impl widget::operation::Focusable for State {
    fn is_focused(&self) -> bool {
        self.focused
    }
    fn focus(&mut self) {
        self.focused = true;
        self.blink = true;
        self.blink_at = None;
    }
    fn unfocus(&mut self) {
        self.focused = false;
        self.dragging = false;
        self.invalidate_ime();
    }
}
