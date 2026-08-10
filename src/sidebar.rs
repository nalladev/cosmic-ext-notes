// SPDX-License-Identifier: MPL-2.0

//! The collapsible directory-tree sidebar widget.
//!
//! This widget renders the vault's notes as a directory tree with folders
//! and files, supporting expansion/collapse of folders, search filtering,
//! and pinned note display.

use crate::app::{Message, Page};
use crate::fl;
use crate::notes::{Note, Tree, TreeNode, Vault};
use cosmic::iced::{Alignment, Length};
use cosmic::prelude::*;
use cosmic::widget::{self, button, icon};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Renders the collapsible directory tree of the vault.
///
/// - `vault` supplies the tree structure and note metadata.
/// - `expanded` holds the paths of folders whose children are shown.
/// - `active` is the path of the note currently open, if any.
/// - `page` is the current sidebar page (All, Pinned, Trash).
/// - `search_query` is the current search query for filtering.
///
/// Emits `Message::ToggleFolder(path)` when a folder row is clicked and
/// `Message::SelectNote(path)` when a note row is clicked.
#[must_use]
pub fn tree_view<'a>(
    vault: &'a Vault,
    expanded: &'a HashSet<PathBuf>,
    active: Option<&'a Path>,
    page: Page,
    search_query: &'a str,
) -> Element<'a, Message> {
    let view = TreeView {
        tree: vault.tree(),
        vault,
        expanded,
        active,
        page,
        query: search_query.trim().to_lowercase(),
    };
    let rows = view.collect_rows(view.tree.root(), 0);

    if rows.is_empty() {
        return empty_state(page);
    }

    let mut column = widget::column::with_capacity(rows.len());
    for (node, depth) in rows {
        column = column.push(view.row(node, depth));
    }

    widget::scrollable(column).height(Length::Fill).into()
}

/// The shared context used to render the tree.
struct TreeView<'a> {
    tree: &'a Tree,
    vault: &'a Vault,
    expanded: &'a HashSet<PathBuf>,
    active: Option<&'a Path>,
    page: Page,
    query: String,
}

impl<'a> TreeView<'a> {
    /// Collects the visible rows of a set of tree nodes recursively.
    fn collect_rows(&self, nodes: &'a [TreeNode], depth: usize) -> Vec<(&'a TreeNode, usize)> {
        let mut rows = Vec::new();
        for node in nodes {
            match node {
                TreeNode::File { .. } => {
                    let path = node.path();
                    let matches_search =
                        self.query.is_empty() || node.name().to_lowercase().contains(&self.query);
                    let matches_page = self.page != Page::Pinned || self.is_pinned(path);
                    if matches_search && matches_page {
                        rows.push((node, depth));
                    }
                }
                TreeNode::Folder { .. } => {
                    let path = node.path();
                    let has_visible = self.folder_has_visible(path);
                    if (!self.query.is_empty() || self.page == Page::Pinned) && !has_visible {
                        continue;
                    }
                    rows.push((node, depth));
                    // While searching, expand every folder to reveal matches.
                    let is_expanded = !self.query.is_empty() || self.expanded.contains(path);
                    if is_expanded && let Some(children) = self.tree.children_of(path) {
                        rows.extend(self.collect_rows(children, depth + 1));
                    }
                }
            }
        }
        rows
    }

    /// Whether the folder contains at least one note matching the current
    /// page and search filters.
    fn folder_has_visible(&self, path: &Path) -> bool {
        let Some(children) = self.tree.children_of(path) else {
            return false;
        };
        children.iter().any(|node| match node {
            TreeNode::File { .. } => {
                let path = node.path();
                let matches_search =
                    self.query.is_empty() || node.name().to_lowercase().contains(&self.query);
                let matches_page = self.page != Page::Pinned || self.is_pinned(path);
                matches_search && matches_page
            }
            TreeNode::Folder { .. } => self.folder_has_visible(node.path()),
        })
    }

    /// Whether the note with the given path is pinned.
    fn is_pinned(&self, path: &Path) -> bool {
        self.vault
            .notes()
            .iter()
            .find(|note| note.path() == path)
            .is_some_and(Note::pinned)
    }

    /// A single tree row: a collapsible folder or a clickable note.
    fn row(&self, node: &'a TreeNode, depth: usize) -> Element<'a, Message> {
        let spacing = cosmic::theme::spacing();
        let indent = f32::from(u16::try_from(depth).unwrap_or(0)) * 20.0;
        let mut row = widget::row::with_capacity(4)
            .push(cosmic::widget::Space::new().width(Length::Fixed(indent)))
            .spacing(spacing.space_xs)
            .align_y(Alignment::Center);

        match node {
            TreeNode::Folder { .. } => {
                let path = node.path();
                let chevron = if self.expanded.contains(path) {
                    "go-down-symbolic"
                } else {
                    "go-next-symbolic"
                };
                row = row
                    .push(icon::from_name(chevron).size(12))
                    .push(icon::from_name("folder-symbolic").size(16))
                    .push(widget::text::body(node.name()));
                button::custom(row)
                    .on_press(Message::ToggleFolder(path.to_path_buf()))
                    .class(cosmic::theme::Button::ListItem([0.0, 0.0, 0.0, 0.0]))
                    .width(Length::Fill)
                    .into()
            }
            TreeNode::File { .. } => {
                let path = node.path();
                let selected = self.active == Some(path);
                row = row
                    .push(cosmic::widget::Space::new().width(Length::Fixed(12.0)))
                    .push(icon::from_name("document-text-symbolic").size(16))
                    .push(widget::text::body(node.name()));
                if self.is_pinned(path) {
                    row = row.push(icon::from_name("starred-symbolic").size(12));
                }
                button::custom(row)
                    .selected(selected)
                    .on_press(Message::SelectNote(path.to_path_buf()))
                    .class(cosmic::theme::Button::ListItem([0.0, 0.0, 0.0, 0.0]))
                    .width(Length::Fill)
                    .into()
            }
        }
    }
}

/// Empty state for when no notes match the current filters.
fn empty_state(page: Page) -> Element<'static, Message> {
    let space_s = cosmic::theme::spacing().space_s;
    let (message, detail) = match page {
        Page::Trash => (fl!("trash-empty"), String::new()),
        _ => (fl!("no-notes"), fl!("no-notes-desc")),
    };

    widget::container(
        widget::column::with_capacity(2)
            .push(icon::from_name("document-text-symbolic").size(48))
            .push(widget::text::title3(message))
            .push(widget::text::body(detail))
            .spacing(space_s)
            .align_x(Alignment::Center),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .align_x(Alignment::Center)
    .align_y(Alignment::Center)
    .into()
}
