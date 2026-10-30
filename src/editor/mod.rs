#![allow(clippy::collapsible_if)]

//! Source-backed native Markdown editing. Only `EditorState` owns document text;
//! the visual surface and native navigation buffer are disposable projections.
mod projection;
mod widget;

#[cfg(test)]
mod tests;

use cosmic::iced::widget::text_editor::{self, Action, Cursor, Edit, Motion, Position};
use markion_core::{
    MarkdownDocument, MutationOrigin,
    history::{EditorState, UndoCaptureKind},
};
use std::{ops::Range, time::Instant};

pub use markion_core::model::VisualCaretAffinity;
pub use markion_core::model::{MarkdownFormat, TableEdit};
use projection::Surface;

/// App commands and widget input deliberately use the same routing boundary.
#[derive(Debug, Clone)]
pub enum Message {
    Action(Action),
    Insert(String),
    Format(MarkdownFormat),
    Table(TableEdit),
    Undo,
    Redo,
    /// Toggle the complete authored block (or inline link/image) at the caret.
    ToggleSource,
    ToggleTask,
    /// Source byte offsets, useful for search results and toolbar integration.
    Select {
        anchor: usize,
        cursor: usize,
    },
    /// Version-stamped native paragraph hit. Stale geometry cannot edit a new document.
    Hit {
        version: u64,
        revision: usize,
        display: usize,
        extend: bool,
        word: bool,
        affinity: VisualCaretAffinity,
    },
    /// IME preedit stays outside the document; only a commit enters history.
    Composition(Option<String>),
    Commit(String),
    /// Internal native IME events carry the source owner captured at session start.
    Ime {
        owner: ImeOwner,
        event: ImeEvent,
    },
    Indent(bool),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EditorOwner {
    widget: cosmic::iced::core::widget::Id,
    document: markion_core::DocumentInstanceId,
}

/// Opaque session stamp; created by the native widget, not the app integration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImeOwner {
    editor: EditorOwner,
    version: u64,
    selection: Range<usize>,
    reversed: bool,
    epoch: u64,
}
#[derive(Debug, Clone)]
pub enum ImeEvent {
    Opened,
    Preedit(String),
    Commit(String),
    Cancel,
}

pub struct Editor {
    state: EditorState,
    surface: Surface,
    navigation: text_editor::Content,
    source_mode: Option<Range<usize>>,
    composition: bool,
    ime_session: Option<ImeOwner>,
    ime_epoch: u64,
    last_rejection: Option<String>,
    image_directory: Option<std::path::PathBuf>,
    image_handles: std::collections::HashMap<String, cosmic::iced::core::image::Handle>,
    image_revision: usize,
    surface_revision: usize,
    id: cosmic::iced::core::widget::Id,
}

impl Editor {
    pub fn new(text: &str) -> Self {
        let state = EditorState::new(MarkdownDocument::from_text(text));
        let surface = Surface::new(&state, None);
        let navigation = text_editor::Content::with_text(&surface.projection.text);
        let mut editor = Self {
            state,
            surface,
            navigation,
            source_mode: None,
            composition: false,
            ime_session: None,
            ime_epoch: 0,
            last_rejection: None,
            image_directory: None,
            image_handles: Default::default(),
            image_revision: 0,
            surface_revision: 0,
            id: cosmic::iced::core::widget::Id::unique(),
        };
        // Start at the first editable position, not in a hidden heading/list marker.
        let first = editor.surface.source_at(0, VisualCaretAffinity::Downstream);
        editor.select_source(first, first);
        editor
    }

    #[allow(dead_code)]
    pub fn id(&self) -> cosmic::iced::core::widget::Id {
        self.id.clone()
    }
    pub fn text(&self) -> &str {
        self.state.document.text()
    }
    pub fn selection(&self) -> Range<usize> {
        self.state.selected_range.clone()
    }
    #[allow(dead_code)]
    pub fn can_undo(&self) -> bool {
        self.state.undo_len() != 0
    }
    #[allow(dead_code)]
    pub fn can_redo(&self) -> bool {
        self.state.redo_len() != 0
    }
    #[allow(dead_code)]
    pub fn last_rejection(&self) -> Option<&str> {
        self.last_rejection.as_deref()
    }

    /// Resolve relative image destinations against the note's directory. Remote
    /// images are never fetched by this adapter; the app can supply decoded handles.
    pub fn set_image_directory(&mut self, directory: impl Into<std::path::PathBuf>) {
        self.image_directory = Some(directory.into());
        self.image_revision = self.image_revision.wrapping_add(1);
    }
    #[allow(dead_code)]
    pub fn set_image_handle(
        &mut self,
        destination: impl Into<String>,
        handle: cosmic::iced::core::image::Handle,
    ) {
        self.image_handles.insert(destination.into(), handle);
        self.image_revision = self.image_revision.wrapping_add(1);
    }
    fn image_handle(&self, destination: &str) -> Option<cosmic::iced::core::image::Handle> {
        if let Some(handle) = self.image_handles.get(destination) {
            return Some(handle.clone());
        }
        let path = destination.strip_prefix("file://").unwrap_or(destination);
        if path.contains("://") || path.starts_with("data:") {
            return None;
        }
        let path = std::path::Path::new(path);
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.image_directory.as_ref()?.join(path)
        };
        Some(cosmic::iced::core::image::Handle::from_path(path))
    }

    pub fn view(
        &self,
        link_color: cosmic::iced::Color,
        accent_revision: u64,
    ) -> cosmic::Element<'_, Message> {
        widget::VisualEditor::new(self, link_color, accent_revision).into()
    }

    pub fn undo(&mut self) {
        self.invalidate_composition();
        if self.state.apply_undo() {
            self.source_mode = None;
            self.rebuild();
        }
    }
    pub fn redo(&mut self) {
        self.invalidate_composition();
        if self.state.apply_redo() {
            self.source_mode = None;
            self.rebuild();
        }
    }

    pub fn update(&mut self, message: Message) {
        match &message {
            Message::Composition(_) | Message::Commit(_) | Message::Ime { .. } => {}
            Message::Hit {
                version, revision, ..
            } if *version != self.state.document.version()
                || *revision != self.surface_revision =>
            {
                return;
            }
            _ => self.invalidate_composition(),
        }
        match message {
            Message::Undo => self.undo(),
            Message::Redo => self.redo(),
            Message::Action(action) => self.action(action),
            Message::Insert(text) => self.insert_text(&text),
            Message::Commit(text) => {
                if let Some(owner) = self.ime_session.clone() {
                    self.ime(owner, ImeEvent::Commit(text));
                }
            }
            Message::Composition(Some(text)) => self.ime(self.ime_owner(), ImeEvent::Preedit(text)),
            Message::Composition(None) => self.invalidate_composition(),
            Message::Ime { owner, event } => self.ime(owner, event),
            Message::Select { anchor, cursor } => self.select_source(anchor, cursor),
            Message::Hit {
                version,
                revision,
                display,
                extend,
                word,
                affinity,
            } => {
                if version != self.state.document.version() || revision != self.surface_revision {
                    return;
                }
                self.state.finish_undo_capture();
                if word {
                    if let Some(range) = self.surface.projection.word_selection_range(display) {
                        self.select_source(range.start, range.end);
                    }
                } else {
                    let cursor = self.surface.source_at(display, affinity);
                    let anchor = if extend { self.anchor() } else { cursor };
                    self.select_source(anchor, cursor);
                }
            }
            Message::Format(format) => {
                let snapshot = self.state.snapshot();
                let version = self.state.document.version();
                let selection = self
                    .state
                    .document
                    .apply_markdown_format(self.selection(), format);
                if self.state.document.version() != version {
                    self.state.commit_undo_snapshot(snapshot);
                }
                self.set_selection(selection, false);
                self.source_mode = None;
                self.rebuild();
            }
            Message::Table(edit) => {
                let snapshot = self.state.snapshot();
                let version = self.state.document.version();
                if let Some(selection) = self.state.document.edit_table_at(self.caret(), edit) {
                    if self.state.document.version() != version {
                        self.state.commit_undo_snapshot(snapshot);
                    }
                    self.set_selection(selection.selected_range, false);
                    self.rebuild();
                }
            }
            Message::ToggleSource => {
                self.state.finish_undo_capture();
                self.source_mode = if self.source_mode.is_some() {
                    None
                } else {
                    let caret = self.caret();
                    let blocks = self.state.document.visual_blocks_shared();
                    blocks
                        .iter()
                        .find(|b| b.source_range.contains(&caret) || b.source_range.end == caret)
                        .map(|b| {
                            b.reveal_groups
                                .iter()
                                .find(|g| g.source_range.contains(&caret))
                                .map_or_else(|| b.source_range.clone(), |g| g.source_range.clone())
                        })
                };
                self.rebuild();
            }
            Message::ToggleTask => {
                let blocks = self.state.document.visual_blocks_shared();
                if let Some(block) = blocks
                    .iter()
                    .find(|b| b.source_range.contains(&self.caret()))
                {
                    if let Some((range, replacement)) =
                        block.block_prefix.as_ref().and_then(|prefix| {
                            markion_core::visual::task_checkbox_toggle(self.text(), prefix)
                        })
                    {
                        self.mutate(
                            range,
                            &replacement,
                            self.selection(),
                            UndoCaptureKind::Atomic,
                            MutationOrigin::StructuralEdit,
                        );
                    }
                }
            }
            Message::Indent(forward) => {
                if let Some(target) = (!self.in_source_mode())
                    .then(|| {
                        self.state
                            .document
                            .visual_editor_tab_target(&self.selection(), forward)
                    })
                    .flatten()
                {
                    self.select_source(target.start, target.end);
                } else {
                    let snapshot = self.state.snapshot();
                    let version = self.state.document.version();
                    let range = self.selection();
                    let selection = if forward {
                        self.state.document.indent_lines(range)
                    } else {
                        self.state.document.outdent_lines(range)
                    };
                    if self.state.document.version() != version {
                        self.state.commit_undo_snapshot(snapshot);
                    }
                    self.set_selection(selection, false);
                    self.rebuild();
                }
            }
        }
    }

    fn owner(&self) -> EditorOwner {
        EditorOwner {
            widget: self.id.clone(),
            document: self.state.document.instance_id(),
        }
    }
    fn ime_owner(&self) -> ImeOwner {
        ImeOwner {
            editor: self.owner(),
            version: self.state.document.version(),
            selection: self.selection(),
            reversed: self.state.selection_reversed,
            epoch: self.ime_epoch,
        }
    }
    fn ime_owner_is_current(&self, owner: &ImeOwner) -> bool {
        *owner == self.ime_owner()
    }
    fn invalidate_composition(&mut self) {
        if self.ime_session.take().is_some() {
            self.state.finish_undo_capture();
        }
        self.ime_epoch = self.ime_epoch.wrapping_add(1);
        self.composition = false;
        self.state.marked_range = None;
    }
    fn ime(&mut self, owner: ImeOwner, event: ImeEvent) {
        if !self.ime_owner_is_current(&owner) {
            return;
        }
        match event {
            ImeEvent::Opened | ImeEvent::Preedit(_) => {
                if self
                    .ime_session
                    .as_ref()
                    .is_some_and(|session| session != &owner)
                {
                    return;
                }
                if self.ime_session.is_none() {
                    self.state.finish_undo_capture();
                }
                self.ime_session = Some(owner);
                if let ImeEvent::Preedit(text) = event {
                    self.composition = !text.is_empty();
                    self.state.marked_range = self.composition.then(|| self.selection());
                }
            }
            ImeEvent::Commit(text) => {
                if self.ime_session.as_ref() != Some(&owner) {
                    return;
                }
                self.replace_range(
                    owner.selection,
                    &text,
                    UndoCaptureKind::Ime,
                    MutationOrigin::ImeComposition,
                );
                self.invalidate_composition();
                self.state.finish_undo_capture();
            }
            ImeEvent::Cancel => self.invalidate_composition(),
        }
    }
    fn in_source_mode(&self) -> bool {
        self.source_mode.as_ref().is_some_and(|mode| {
            let selected = self.selection();
            self.state
                .document
                .visual_blocks_shared()
                .iter()
                .any(|block| {
                    mode.start < block.source_range.end
                        && mode.end > block.source_range.start
                        && selected.start >= block.source_range.start
                        && selected.end <= block.source_range.end
                })
        })
    }
    fn delete_boundary(&mut self, boundary: projection::Boundary, forward: bool) {
        let handoff = if forward {
            boundary.after
        } else {
            boundary.before
        };
        if boundary.before > boundary.after {
            self.select_source(handoff, handoff);
            return;
        }
        let gap = boundary.before..boundary.after;
        let left = self
            .state
            .document
            .visual_editor_field_at(&(boundary.before..boundary.before));
        let right = self
            .state
            .document
            .visual_editor_field_at(&(boundary.after..boundary.after));
        // Complex editor edges hand off exactly as upstream's field helpers do.
        // A raw source view instead edits its literal line endings.
        let same_field = left.is_some() && left == right;
        if !self.in_source_mode() && (left.is_some() || right.is_some()) && !same_field {
            self.select_source(handoff, handoff);
            return;
        }
        let blocks = self.state.document.visual_blocks_shared();
        let closing_syntax = blocks
            .iter()
            .flat_map(|block| &block.reveal_groups)
            .filter(|group| {
                group.source_range.start < boundary.before
                    && boundary.before <= group.source_range.end
                    && group
                        .content_ranges
                        .iter()
                        .all(|content| content.end <= boundary.before)
            })
            .map(|group| boundary.before..group.source_range.end)
            .collect::<Vec<_>>();
        let proven = self.in_source_mode()
            || same_field
            || self.text()[gap.clone()].char_indices().all(|(index, ch)| {
                ch.is_whitespace()
                    || closing_syntax
                        .iter()
                        .any(|range| range.contains(&(gap.start + index)))
            });
        if proven {
            let newline = if forward {
                self.text()[gap.clone()].find('\n')
            } else {
                self.text()[gap.clone()].rfind('\n')
            };
            if let Some(index) = newline {
                let end = gap.start + index + 1;
                let start = if end >= 2 && self.text().as_bytes()[end - 2] == b'\r' {
                    end - 2
                } else {
                    end - 1
                };
                // Never include hidden inline suffixes, destinations or prefixes.
                let range = start.max(gap.start)..end;
                let caret = self.caret();
                let after = if caret <= range.start {
                    caret
                } else {
                    caret.saturating_sub(range.len())
                };
                self.mutate(
                    range,
                    "",
                    after..after,
                    UndoCaptureKind::Atomic,
                    MutationOrigin::StructuralEdit,
                );
                return;
            }
        }
        self.select_source(handoff, handoff);
    }
    fn caret(&self) -> usize {
        if self.state.selection_reversed {
            self.state.selected_range.start
        } else {
            self.state.selected_range.end
        }
    }
    fn anchor(&self) -> usize {
        if self.state.selection_reversed {
            self.state.selected_range.end
        } else {
            self.state.selected_range.start
        }
    }
    fn set_selection(&mut self, range: Range<usize>, reversed: bool) {
        self.state.selected_range = range;
        self.state.selection_reversed = reversed;
    }
    fn select_source(&mut self, anchor: usize, cursor: usize) {
        self.invalidate_composition();
        let anchor = markion_core::text_util::clamp_to_char_boundary(self.text(), anchor);
        let cursor = markion_core::text_util::clamp_to_char_boundary(self.text(), cursor);
        self.state.finish_undo_capture();
        self.set_selection(anchor.min(cursor)..anchor.max(cursor), cursor < anchor);
        self.sync_navigation();
    }
    fn rebuild(&mut self) {
        self.surface_revision = self.surface_revision.wrapping_add(1);
        self.surface = Surface::new(&self.state, self.source_mode.clone());
        self.navigation = text_editor::Content::with_text(&self.surface.projection.text);
        self.sync_navigation();
    }
    fn sync_navigation(&mut self) {
        let text = &self.surface.projection.text;
        let cursor = position(text, self.surface.display_at(self.caret()));
        let anchor = position(text, self.surface.display_at(self.anchor()));
        self.navigation.move_to(Cursor {
            position: cursor,
            selection: (cursor != anchor).then_some(anchor),
        });
    }
    fn native_selection(&self) -> (usize, usize) {
        let cursor = self.navigation.cursor();
        let text = &self.surface.projection.text;
        (
            offset(text, cursor.selection.unwrap_or(cursor.position)),
            offset(text, cursor.position),
        )
    }
    fn select_display(&mut self, anchor: usize, cursor: usize) {
        if anchor == cursor {
            // Retain the source side of a collapsed marker boundary when nothing moved.
            if self.surface.display_at(self.caret()) == cursor {
                return;
            }
            let source = self
                .surface
                .source_at(cursor, VisualCaretAffinity::Downstream);
            self.select_source(source, source);
        } else {
            let start = self
                .surface
                .source_at(anchor.min(cursor), VisualCaretAffinity::Downstream);
            let end = self
                .surface
                .source_at(anchor.max(cursor), VisualCaretAffinity::Upstream);
            self.select_source(
                if cursor < anchor { end } else { start },
                if cursor < anchor { start } else { end },
            );
        }
    }
    fn action(&mut self, action: Action) {
        match action {
            Action::Edit(Edit::Insert(c)) => self.insert_text(&c.to_string()),
            Action::Edit(Edit::Paste(text)) => self.replace_selection(
                &text,
                UndoCaptureKind::Atomic,
                MutationOrigin::PlatformTextInput,
            ),
            Action::Edit(Edit::Enter) => {
                if !self.in_source_mode() {
                    if let Some(target) = self
                        .state
                        .document
                        .visual_editor_tab_target(&self.selection(), true)
                    {
                        self.select_source(target.start, target.end);
                        return;
                    }
                }
                if !self.in_source_mode() && self.selection().is_empty() {
                    if let Some(edit) = self.state.document.visual_enter_edit(self.caret()) {
                        self.mutate(
                            edit.range,
                            &edit.replacement,
                            edit.selection_after,
                            UndoCaptureKind::Atomic,
                            MutationOrigin::StructuralEdit,
                        );
                        return;
                    }
                }
                self.replace_selection(
                    "\n",
                    UndoCaptureKind::Atomic,
                    MutationOrigin::StructuralEdit,
                );
            }
            Action::Edit(Edit::Backspace) => self.delete(false),
            Action::Edit(Edit::Delete) => self.delete(true),
            Action::Edit(Edit::Indent) => self.update(Message::Indent(true)),
            Action::Edit(Edit::Unindent) => self.update(Message::Indent(false)),
            Action::SelectAll => self.select_source(0, self.text().len()),
            action => {
                self.state.finish_undo_capture();
                self.navigation.perform(action);
                let (anchor, cursor) = self.native_selection();
                self.select_display(anchor, cursor);
            }
        }
    }
    fn delete(&mut self, forward: bool) {
        if self.selection().is_empty() {
            if !forward {
                let caret = self.caret();
                if let Some((previous, _)) = self.text()[..caret].char_indices().next_back() {
                    if self.apply_auto_pair(previous..caret, "") {
                        return;
                    }
                }
            }
            if !self.in_source_mode() && !forward {
                if let Some(edit) = self.state.document.visual_backspace_edit(self.caret()) {
                    self.mutate(
                        edit.range,
                        &edit.replacement,
                        edit.selection_after,
                        UndoCaptureKind::Atomic,
                        MutationOrigin::StructuralEdit,
                    );
                    return;
                }
            }
            if let Some(target) = (!self.in_source_mode())
                .then(|| {
                    self.state
                        .document
                        .visual_editor_edge_target(self.caret(), forward)
                })
                .flatten()
            {
                self.select_source(target, target);
                return;
            }
            self.navigation.perform(Action::Select(if forward {
                Motion::Right
            } else {
                Motion::Left
            }));
            let (anchor, cursor) = self.native_selection();
            let start = anchor.min(cursor);
            let end = anchor.max(cursor);
            if start == end {
                return;
            }
            if let Some(boundary) = self.surface.boundary(&(start..end)).cloned() {
                self.delete_boundary(boundary, forward);
                return;
            }
            // Only real atoms (entities, emoji, images) can expand an edit.
            let Some(range) = self.surface.edit_range(start..end) else {
                return;
            };
            self.replace_range(
                range,
                "",
                UndoCaptureKind::Delete,
                MutationOrigin::PlatformTextInput,
            );
        } else {
            self.replace_selection(
                "",
                UndoCaptureKind::Delete,
                MutationOrigin::PlatformTextInput,
            );
        }
    }
    fn auto_pair_allowed(&self) -> bool {
        if self.in_source_mode() {
            return false;
        }
        if let Some(field) = self
            .state
            .document
            .visual_editor_field_at(&self.selection())
        {
            return !markion_core::auto_pair::is_auto_pair_restricted_field(field.kind);
        }
        !self
            .state
            .document
            .visual_blocks_shared()
            .iter()
            .any(|block| {
                block.source_range.contains(&self.caret()) && block.source_island.is_some()
            })
    }
    fn apply_auto_pair(&mut self, range: Range<usize>, text: &str) -> bool {
        use markion_core::auto_pair::{AutoPairAction, auto_pair_action};
        if !self.auto_pair_allowed() {
            return false;
        }
        match auto_pair_action(self.text(), range, text) {
            Some(AutoPairAction::Skip { caret }) => {
                self.select_source(caret, caret);
                self.rebuild();
            }
            Some(AutoPairAction::Replace {
                range,
                replacement,
                selection_after,
            }) => {
                self.replace_range_selected(
                    range,
                    &replacement,
                    UndoCaptureKind::Atomic,
                    MutationOrigin::PlatformTextInput,
                    Some(selection_after),
                );
            }
            None => return false,
        }
        true
    }
    fn insert_text(&mut self, text: &str) {
        if !self.apply_auto_pair(self.selection(), text) {
            self.replace_selection(
                text,
                UndoCaptureKind::Insert,
                MutationOrigin::PlatformTextInput,
            );
        }
    }
    fn replace_selection(&mut self, text: &str, kind: UndoCaptureKind, origin: MutationOrigin) {
        self.replace_range(self.selection(), text, kind, origin);
    }
    fn replace_range(
        &mut self,
        range: Range<usize>,
        text: &str,
        kind: UndoCaptureKind,
        origin: MutationOrigin,
    ) {
        self.replace_range_selected(range, text, kind, origin, None);
    }
    fn replace_range_selected(
        &mut self,
        range: Range<usize>,
        text: &str,
        kind: UndoCaptureKind,
        origin: MutationOrigin,
        selected: Option<Range<usize>>,
    ) {
        if let Some(edit) = (!self.in_source_mode())
            .then(|| {
                self.state
                    .document
                    .direct_visual_block_edit(range.clone(), text)
            })
            .flatten()
        {
            if !self.state.document.validate_visual_block_edit(&edit) {
                return;
            }
            let kind = if edit.range == range && edit.replacement == text {
                kind
            } else {
                UndoCaptureKind::Atomic
            };
            self.mutate(
                edit.range,
                &edit.replacement,
                selected.filter(|_| edit.replacement == text).map_or(
                    edit.selection_after,
                    |selection| {
                        let start = edit.inserted_range_after.start;
                        start + selection.start - range.start..start + selection.end - range.start
                    },
                ),
                kind,
                origin,
            );
        } else {
            let end = range.start + text.len();
            self.mutate(range, text, selected.unwrap_or(end..end), kind, origin);
        }
    }
    fn mutate(
        &mut self,
        range: Range<usize>,
        text: &str,
        selection: Range<usize>,
        kind: UndoCaptureKind,
        origin: MutationOrigin,
    ) {
        let Some(expected) = self.text().get(range.clone()) else {
            return;
        };
        if expected == text {
            return;
        }
        // The range and UTF-8 boundaries are proven above; this current-version
        // mutation is applied synchronously, with no intervening document access.
        let mutation = self
            .state
            .document
            .prepare_range_mutation(origin, range.clone(), text);
        self.state
            .prepare_undo_capture(kind, &range, text, Instant::now());
        match self.state.document.apply_checked_mutation(mutation) {
            Ok(receipt) if receipt.changed => {
                self.set_selection(selection, false);
                self.state.marked_range = None;
                self.last_rejection = None;
                self.source_mode = self.source_mode.take().and_then(|mut mode| {
                    if range.start >= mode.start && range.end <= mode.end {
                        mode.end = mode.end - range.len() + text.len();
                        Some(mode)
                    } else {
                        None
                    }
                });

                self.rebuild();
            }
            Ok(_) => {}
            Err(error) => self.last_rejection = Some(error.to_string()),
        }
    }
}

fn position(text: &str, byte: usize) -> Position {
    let byte = markion_core::text_util::clamp_to_char_boundary(text, byte);
    let start = text[..byte].rfind('\n').map_or(0, |i| i + 1);
    Position {
        line: text[..byte].bytes().filter(|b| *b == b'\n').count(),
        column: byte - start,
    }
}
fn offset(text: &str, pos: Position) -> usize {
    let start = if pos.line == 0 {
        0
    } else {
        text.match_indices('\n')
            .nth(pos.line - 1)
            .map_or(text.len(), |(i, _)| i + 1)
    };
    let end = text[start..].find('\n').map_or(text.len(), |i| start + i);
    markion_core::text_util::clamp_to_char_boundary(text, (start + pos.column).min(end))
}
