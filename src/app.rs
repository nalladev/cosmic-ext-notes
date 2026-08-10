// SPDX-License-Identifier: MPL-2.0

use crate::config::Config;
use crate::editor::{self, BlockKind};
use crate::fl;
use crate::notes::{self, Note, SaveOutcome, Vault};
use cosmic::app::context_drawer;
use cosmic::cosmic_config::{self, CosmicConfigEntry};
use cosmic::iced::alignment::{Horizontal, Vertical};
use cosmic::iced::futures::SinkExt;
use cosmic::iced::keyboard::Key;
use cosmic::iced::{Alignment, Length, Subscription, event, keyboard};
use cosmic::prelude::*;
use cosmic::widget::{
    self, Toasts, about::About, button, dialog, divider, icon, list_column, markdown, menu,
    popover, segmented_button, tab_bar, text_editor, text_input, toaster, tooltip,
};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");
const APP_ICON: &[u8] = include_bytes!("../resources/icon.svg");

/// How often the autosave tick fires.
const AUTOSAVE_INTERVAL: Duration = Duration::from_millis(500);
/// How long consecutive typing in one block stays coalesced into a single
/// undo step.
const UNDO_COALESCE: Duration = Duration::from_millis(800);
/// The maximum number of undo snapshots kept per note.
const UNDO_LIMIT: usize = 100;
/// An entry in the slash command menu.
struct SlashItem {
    /// The kind the block converts to.
    kind: BlockKind,
    /// The toolbar icon name for the entry.
    icon: &'static str,
    /// The i18n key of the entry label.
    label: &'static str,
}

/// The slash command menu entries, in display order.
const SLASH_ITEMS: &[SlashItem] = &[
    SlashItem {
        kind: BlockKind::Paragraph,
        icon: "format-justify-left-symbolic",
        label: "paragraph",
    },
    SlashItem {
        kind: BlockKind::Heading(1),
        icon: "format-text-heading-1-symbolic",
        label: "heading-1",
    },
    SlashItem {
        kind: BlockKind::Heading(2),
        icon: "format-text-heading-2-symbolic",
        label: "heading-2",
    },
    SlashItem {
        kind: BlockKind::Heading(3),
        icon: "format-text-heading-3-symbolic",
        label: "heading-3",
    },
    SlashItem {
        kind: BlockKind::Bullet(0),
        icon: "format-list-unordered-symbolic",
        label: "bullet-list",
    },
    SlashItem {
        kind: BlockKind::Numbered(0, 1),
        icon: "format-list-ordered-symbolic",
        label: "numbered-list",
    },
    SlashItem {
        kind: BlockKind::Todo(0, false),
        icon: "format-list-checkbox-symbolic",
        label: "todo-list",
    },
    SlashItem {
        kind: BlockKind::Quote(0),
        icon: "format-quote-block-symbolic",
        label: "quote",
    },
    SlashItem {
        kind: BlockKind::Code(String::new()),
        icon: "format-text-code-symbolic",
        label: "code-block",
    },
    SlashItem {
        kind: BlockKind::Divider,
        icon: "insert-horizontal-rule-symbolic",
        label: "divider",
    },
];
/// Widget id of the search input.
const SEARCH_ID: &str = "notes-search";
/// Widget id of the note title input.
const TITLE_ID: &str = "note-title";

/// The application model stores app-specific state used to describe its interface and
/// drive its logic.
pub struct AppModel {
    /// Application state which is managed by the COSMIC runtime.
    core: cosmic::Core,
    /// Display a context drawer with the designated page if defined.
    context_page: ContextPage,
    /// The about page for this app.
    about: About,
    /// The active page: all notes, pinned, or trash.
    page: Page,
    /// The page selector buttons at the top of the sidebar.
    page_buttons: segmented_button::Model<segmented_button::SingleSelect>,
    /// Notes currently open in tabs, in tab order.
    open_tabs: Vec<PathBuf>,
    /// The tab bar model, kept in sync with [`Self::open_tabs`].
    tabs: segmented_button::Model<segmented_button::SingleSelect>,
    /// Directories currently expanded in the sidebar tree.
    expanded: HashSet<PathBuf>,
    /// Key bindings for the application's menu bar.
    #[allow(clippy::zero_sized_map_values)]
    key_binds: HashMap<menu::KeyBind, MenuAction>,
    /// Configuration data that persists between application runs.
    config: Config,
    /// The note store.
    vault: Vault,
    /// The note currently open in the editor, if any.
    selected: Option<PathBuf>,
    /// The blocks of the active note, each with an editable text buffer.
    blocks: Vec<EditableBlock>,
    /// The index of the block the toolbar acts on (the last one focused).
    selected_block: usize,
    /// Undo history: snapshots of the whole document, newest last.
    undo_stack: Vec<Vec<editor::Block>>,
    /// Redo history, cleared by any new edit.
    redo_stack: Vec<Vec<editor::Block>>,
    /// When the last undo snapshot was taken and in which block, so that
    /// bursts of typing collapse into a single undo step.
    last_undo_at: Option<(Instant, usize)>,
    /// The block index whose slash command menu is open, if any.
    slash_menu: Option<usize>,
    /// The highlighted entry of the open slash menu.
    slash_selection: usize,
    /// The title being edited for the active note.
    title_draft: String,
    /// Whether the editor has unsaved changes.
    dirty: bool,
    /// Whether to show the markdown preview instead of the editor.
    preview: bool,
    /// Parsed markdown of the active note, kept in sync with the editor.
    markdown: Option<markdown::Content>,
    /// The note search query.
    search: String,
    /// A pending destructive action awaiting confirmation.
    confirm: Option<Confirm>,
    /// Toast notifications.
    toasts: Toasts<Message>,
}

/// Messages emitted by the application and its widgets.
#[derive(Debug, Clone)]
pub enum Message {
    LaunchUrl(String),
    ToggleContextPage(ContextPage),
    UpdateConfig(Config),
    ToastClosed(toaster::ToastId),
    /// A keyboard shortcut was pressed.
    KeyboardShortcut(Shortcut),
    /// The search query changed.
    SearchChanged(String),
    /// The sidebar page changed (all notes, pinned, or trash).
    PageChanged(Page),
    /// Expand or collapse the folder with the given path in the sidebar tree.
    ToggleFolder(PathBuf),
    /// Close the tab of the note with the given path.
    CloseTab(PathBuf),
    /// Make the tab of the note with the given path active.
    ActivateTab(PathBuf),
    /// Open the note with the given path.
    SelectNote(PathBuf),
    /// Create a new note.
    NewNote,
    /// The title input changed.
    EditTitle(String),
    /// Commit the current title as a rename.
    RenameSubmit,
    /// Toggle the pinned state of the active note.
    TogglePin,
    /// Move the active note to the trash.
    TrashNote,
    /// Restore the trashed note with the given path.
    RestoreNote(PathBuf),
    /// Permanently delete the trashed note with the given path. Opens a
    /// confirmation dialog first.
    DeleteNote(PathBuf),
    /// Permanently delete all trashed notes. Opens a confirmation dialog first.
    EmptyTrash,
    /// Confirm the pending destructive action.
    ConfirmDialog,
    /// Dismiss the confirmation dialog without acting.
    DismissDialog,
    /// An action was performed in the text editor of the block with the given
    /// index.
    BlockAction {
        index: usize,
        action: text_editor::Action,
    },
    /// Toggle the checked state of the todo block with the given index.
    ToggleTodo(usize),
    /// Split the block at the cursor (Enter): the text after the cursor moves
    /// into a new block of `kind` inserted after it.
    SplitBlock {
        index: usize,
        kind: BlockKind,
        before: String,
        after: String,
    },
    /// Replace the block with the given kind and content (typed markdown
    /// prefix, or backspace that removes a list marker).
    ConvertBlock {
        index: usize,
        kind: BlockKind,
        content: String,
    },
    /// Shift the nesting level of the block (Tab, Shift+Tab, or backspace on a
    /// nested item).
    ChangeLevel {
        index: usize,
        delta: i8,
    },
    /// Merge the block into the previous one (backspace at the start).
    MergeUp {
        index: usize,
    },
    /// Move focus to the editable block before the given index, placing the
    /// caret at its end.
    FocusPrevious(usize),
    /// Move focus to the editable block after the given index, placing the
    /// caret at its start.
    FocusNext(usize),
    /// Convert the block at the given index to the given kind, keeping its
    /// text when the kind has one (toolbar block-style buttons).
    FormatBlock {
        index: usize,
        kind: BlockKind,
    },
    /// Wrap the block's selection with the given inline markdown markers.
    WrapInline {
        index: usize,
        marker: InlineMarker,
    },
    /// Convert the block into a divider and move focus below it (typed
    /// `---` followed by Enter).
    InsertDivider(usize),
    /// Undo the last document-wide change (Ctrl+Z).
    Undo,
    /// Redo the last undone change (Ctrl+Shift+Z or Ctrl+Y).
    Redo,
    /// Open the slash command menu for the block with the given index
    /// (typed `/` in an empty paragraph).
    SlashMenuOpen(usize),
    /// Close the slash command menu without applying anything.
    SlashMenuClose,
    /// Move the slash menu highlight by the given delta (-1 or 1).
    SlashMenuMove(i8),
    /// Apply the highlighted slash menu entry.
    SlashMenuSelect,
    /// Apply the slash menu entry for the given block kind, closing the menu.
    SlashMenuPick {
        /// The block to convert.
        index: usize,
        /// The kind to convert it to.
        kind: BlockKind,
    },
    /// Toggle between the editor and the markdown preview.
    TogglePreview,
    /// A link was clicked in the markdown preview.
    LinkClicked(String),
    /// Periodic autosave tick.
    AutosaveTick,
    /// Save the active note now.
    SaveNow,
    /// A message that does nothing.
    Ignored,
}

/// Inline markdown formatting applied to a block's selection by the toolbar.
/// The markers are inserted as visible characters; this editor has no rich
/// text spans, so the raw markdown stays in the buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InlineMarker {
    /// `**bold**`
    Bold,
    /// `*italic*`
    Italic,
    /// `` `code` ``
    Code,
    /// `~~strikethrough~~`
    Strikethrough,
}

impl InlineMarker {
    /// The delimiters that open and close this marker.
    fn delimiters(self) -> (&'static str, &'static str) {
        match self {
            Self::Bold => ("**", "**"),
            Self::Italic => ("*", "*"),
            Self::Code => ("`", "`"),
            Self::Strikethrough => ("~~", "~~"),
        }
    }
}

/// Keyboard shortcuts handled by the application.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Shortcut {
    NewNote,
    Save,
}

impl Shortcut {
    /// The key bindings shown in the menu.
    #[must_use]
    pub fn key_binds() -> HashMap<menu::KeyBind, MenuAction> {
        let mut binds = HashMap::new();
        binds.insert(
            menu::KeyBind {
                modifiers: vec![menu::key_bind::Modifier::Super],
                key: Key::Character("n".into()),
            },
            MenuAction::NewNote,
        );
        binds.insert(
            menu::KeyBind {
                modifiers: vec![menu::key_bind::Modifier::Super],
                key: Key::Character("s".into()),
            },
            MenuAction::Save,
        );
        binds
    }
}

/// A pending destructive action awaiting confirmation.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Confirm {
    /// Permanently delete the note with the given path.
    Delete(PathBuf),
    /// Permanently delete all trashed notes.
    EmptyTrash,
}

/// A block rendered in the editor: the block kind plus the editable text
/// buffer, which keeps the cursor and selection state between frames.
struct EditableBlock {
    /// What kind of block this is.
    kind: BlockKind,
    /// The editable text buffer for the block content.
    content: text_editor::Content,
}

impl EditableBlock {
    /// Builds an editable block from a pure model block.
    fn from_block(block: editor::Block) -> Self {
        Self {
            kind: block.kind,
            content: text_editor::Content::with_text(&block.content),
        }
    }

    /// Converts this editable block back into a pure model block.
    fn to_block(&self) -> editor::Block {
        editor::Block {
            kind: self.kind.clone(),
            content: self.content.text(),
        }
    }
}

/// Create a COSMIC application from the app model
impl cosmic::Application for AppModel {
    /// The async executor that will be used to run your application's commands.
    type Executor = cosmic::executor::Default;

    /// Data that your application receives to its init method.
    type Flags = ();

    /// Messages which the application and its widgets will emit.
    type Message = Message;

    /// Unique identifier in RDNN (reverse domain name notation) format.
    const APP_ID: &'static str = "com.github.nalladev.cosmic-notes";

    fn core(&self) -> &cosmic::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut cosmic::Core {
        &mut self.core
    }

    /// Initializes the application with any given flags and startup commands.
    fn init(
        core: cosmic::Core,
        _flags: Self::Flags,
    ) -> (Self, Task<cosmic::Action<Self::Message>>) {
        // The sidebar page selector: all notes, pinned, or trash.
        let mut page_buttons = segmented_button::Model::default();
        for (page, label) in [
            (Page::All, fl!("all-notes")),
            (Page::Pinned, fl!("pinned")),
            (Page::Trash, fl!("trash")),
        ] {
            page_buttons.insert().text(label).data(page);
        }
        page_buttons.activate_position(0);

        // Create the about widget
        let about = About::default()
            .name(fl!("app-title"))
            .icon(widget::icon::from_svg_bytes(APP_ICON))
            .version(env!("CARGO_PKG_VERSION"))
            .links([(fl!("repository"), REPOSITORY)])
            .license(env!("CARGO_PKG_LICENSE"));

        let config = cosmic_config::Config::new(Self::APP_ID, Config::VERSION)
            .map(|context| match Config::get_entry(&context) {
                Ok(config) => config,
                Err((_errors, config)) => config,
            })
            .unwrap_or_default();

        let vault = Self::open_vault(&config);

        let mut app = AppModel {
            core,
            context_page: ContextPage::default(),
            about,
            page: Page::All,
            page_buttons,
            open_tabs: Vec::new(),
            tabs: segmented_button::Model::default(),
            expanded: HashSet::new(),
            key_binds: Shortcut::key_binds(),
            config,
            vault,
            selected: None,
            blocks: Vec::new(),
            selected_block: 0,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            last_undo_at: None,
            slash_menu: None,
            slash_selection: 0,
            title_draft: String::new(),
            dirty: false,
            preview: false,
            markdown: None,
            search: String::new(),
            confirm: None,
            toasts: Toasts::new(Message::ToastClosed),
        };

        // Create a startup command that sets the window title.
        let command = app.update_title();

        (app, command)
    }

    /// Elements to pack at the start of the header bar.
    fn header_start(&self) -> Vec<Element<'_, Self::Message>> {
        let menu_bar = menu::bar(vec![menu::Tree::with_children(
            menu::root(fl!("file")).apply(Element::from),
            menu::items(
                &self.key_binds,
                vec![
                    menu::Item::Button(fl!("new-note"), None, MenuAction::NewNote),
                    menu::Item::Button(fl!("save"), None, MenuAction::Save),
                    menu::Item::Divider,
                    menu::Item::Button(fl!("about"), None, MenuAction::About),
                ],
            ),
        )]);

        vec![menu_bar.into()]
    }

    /// Elements to pack at the end of the header bar.
    fn header_end(&self) -> Vec<Element<'_, Self::Message>> {
        let new_note = button::icon(icon::from_name("list-add-symbolic"))
            .on_press(Message::NewNote)
            .class(cosmic::theme::Button::Icon);
        vec![Self::tooltip(new_note.into(), fl!("new-note"))]
    }

    /// Display a context drawer if the context page is requested.
    fn context_drawer(&self) -> Option<context_drawer::ContextDrawer<'_, Self::Message>> {
        if !self.core.window.show_context {
            return None;
        }

        Some(match self.context_page {
            ContextPage::About => context_drawer::about(
                &self.about,
                |url| Message::LaunchUrl(url.to_string()),
                Message::ToggleContextPage(ContextPage::About),
            ),
        })
    }

    /// Displays a confirmation dialog for destructive actions.
    fn dialog(&self) -> Option<Element<'_, Self::Message>> {
        let confirm = self.confirm.as_ref()?;
        let (title, body, confirm_label) = match confirm {
            Confirm::Delete(path) => {
                let name = self
                    .vault
                    .notes()
                    .iter()
                    .find(|note| note.path() == path)
                    .map_or_else(String::new, |note| note.title().to_owned());
                (
                    fl!("delete-note-title"),
                    fl!("delete-note-body", name = name),
                    fl!("delete"),
                )
            }
            Confirm::EmptyTrash => (
                fl!("empty-trash-title"),
                fl!("empty-trash-body"),
                fl!("empty-trash"),
            ),
        };
        let cancel = button::text(fl!("cancel"))
            .on_press(Message::DismissDialog)
            .class(cosmic::theme::Button::Text);
        let confirm = button::text(confirm_label)
            .on_press(Message::ConfirmDialog)
            .class(cosmic::theme::Button::Destructive);
        Some(
            dialog::dialog()
                .title(title)
                .body(body)
                .secondary_action(cancel)
                .primary_action(confirm)
                .into(),
        )
    }

    /// Describes the interface based on the current state of the application model.
    fn view(&self) -> Element<'_, Self::Message> {
        let mut right = widget::column::with_capacity(2);
        if let Some(tabs) = self.tab_bar() {
            right = right.push(tabs);
        }
        right = right.push(self.editor_pane());

        let content = widget::row::with_capacity(2)
            .push(self.sidebar_pane())
            .push(divider::vertical::default())
            .push(
                widget::container(right)
                    .width(Length::Fill)
                    .height(Length::Fill),
            )
            .height(Length::Fill);

        toaster(
            &self.toasts,
            widget::container(content)
                .height(Length::Fill)
                .width(Length::Fill),
        )
    }

    /// Register subscriptions for this application.
    fn subscription(&self) -> Subscription<Self::Message> {
        let mut subscriptions = vec![
            // Watch for application configuration changes.
            self.core()
                .watch_config::<Config>(Self::APP_ID)
                .map(|update| Message::UpdateConfig(update.config)),
            // Keyboard shortcuts not handled by the window's keyboard navigation.
            event::listen().map(|event| match event {
                event::Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) => {
                    let primary = modifiers.logo() || modifiers.control();
                    if primary && key == keyboard::Key::Character("n".into()) {
                        Message::KeyboardShortcut(Shortcut::NewNote)
                    } else if primary && key == keyboard::Key::Character("s".into()) {
                        Message::KeyboardShortcut(Shortcut::Save)
                    } else {
                        Message::Ignored
                    }
                }
                _ => Message::Ignored,
            }),
        ];

        // Conditionally enables a timer that emits a message every second.
        if self.dirty {
            subscriptions.push(Subscription::run(|| {
                cosmic::iced::stream::channel(1, |mut emitter: cosmic::iced::futures::channel::mpsc::Sender<Message>| async move {
                    let mut interval = tokio::time::interval(AUTOSAVE_INTERVAL);
                    loop {
                        interval.tick().await;
                        _ = emitter.send(Message::AutosaveTick).await;
                    }
                })
            }));
        }

        Subscription::batch(subscriptions)
    }

    /// Handles messages emitted by the application and its widgets.
    #[allow(clippy::too_many_lines)]
    fn update(&mut self, message: Self::Message) -> Task<cosmic::Action<Self::Message>> {
        match message {
            Message::Ignored => Task::none(),

            Message::LaunchUrl(url) => {
                match open::that_detached(&url) {
                    Ok(()) => {}
                    Err(err) => {
                        eprintln!("failed to open {url:?}: {err}");
                    }
                }
                Task::none()
            }

            Message::ToggleContextPage(context_page) => {
                if self.context_page == context_page {
                    // Close the context drawer if the toggled context page is the same.
                    self.core.window.show_context = !self.core.window.show_context;
                } else {
                    // Open the context drawer to display the requested context page.
                    self.context_page = context_page;
                    self.core.window.show_context = true;
                }
                Task::none()
            }

            Message::UpdateConfig(config) => {
                self.config = config;
                Task::none()
            }

            Message::ToastClosed(id) => {
                self.toasts.remove(id);
                Task::none()
            }

            Message::KeyboardShortcut(shortcut) => match shortcut {
                Shortcut::NewNote => self.new_note(),
                Shortcut::Save => self.save_active(true),
            },

            Message::SearchChanged(query) => {
                self.search = query;
                Task::none()
            }

            Message::SelectNote(path) | Message::ActivateTab(path) => self.open_tab(&path),

            Message::CloseTab(path) => {
                self.commit_title_rename();
                let save = self.save_active(false);
                Task::batch([save, self.close_tab(&path)])
            }

            Message::ToggleFolder(path) => {
                if self.expanded.contains(&path) {
                    self.expanded.remove(&path);
                } else {
                    self.expanded.insert(path);
                }
                Task::none()
            }

            Message::PageChanged(page) => {
                self.set_page(page);
                self.update_title()
            }

            Message::NewNote => self.new_note(),

            Message::EditTitle(title) => {
                self.title_draft = title;
                Task::none()
            }

            Message::RenameSubmit => {
                self.commit_title_rename();
                self.update_title()
            }

            Message::TogglePin => {
                let Some(path) = self.selected.clone() else {
                    return Task::none();
                };
                match self.vault.toggle_pin(&path) {
                    Ok(Some(note)) => self.toast(if note.pinned() {
                        fl!("note-pinned")
                    } else {
                        fl!("note-unpinned")
                    }),
                    Ok(None) => Task::none(),
                    Err(err) => {
                        eprintln!("failed to toggle pin: {err}");
                        self.toast(fl!("save-error"))
                    }
                }
            }

            Message::TrashNote => {
                self.commit_title_rename();
                let save = self.save_active(false);
                let Some(path) = self.selected.clone() else {
                    return save;
                };
                match self.vault.trash(&path) {
                    Ok(Some(_)) => Task::batch([
                        save,
                        self.close_tab(&path),
                        self.toast(fl!("note-trashed")),
                        self.update_title(),
                    ]),
                    Ok(None) => save,
                    Err(err) => {
                        eprintln!("failed to trash note: {err}");
                        Task::batch([save, self.toast(fl!("save-error"))])
                    }
                }
            }

            Message::RestoreNote(path) => match self.vault.restore(&path) {
                Ok(Some(note)) => {
                    // Make the restored note visible again and open it.
                    self.set_page(Page::All);
                    let restored_path = note.path().to_path_buf();
                    let open = self.open_tab(&restored_path);
                    Task::batch([self.toast(fl!("note-restored")), open])
                }
                Ok(None) => Task::none(),
                Err(err) => {
                    eprintln!("failed to restore note: {err}");
                    self.toast(fl!("save-error"))
                }
            },

            Message::DeleteNote(path) => {
                self.confirm = Some(Confirm::Delete(path));
                Task::none()
            }

            Message::EmptyTrash => {
                self.confirm = Some(Confirm::EmptyTrash);
                Task::none()
            }

            Message::ConfirmDialog => {
                let Some(confirm) = self.confirm.take() else {
                    return Task::none();
                };
                match confirm {
                    Confirm::Delete(path) => self.delete_note(&path),
                    Confirm::EmptyTrash => self.empty_trash(),
                }
            }

            Message::DismissDialog => {
                self.confirm = None;
                Task::none()
            }

            Message::BlockAction { index, action } => {
                self.selected_block = index;
                // A multi-line paste splits into blocks, like Futo; the
                // editor's default behavior would embed raw newlines into
                // the block.
                if let Some(task) = self.intercept_paste(index, &action) {
                    return Task::batch([task, self.update_title()]);
                }
                let is_edit = action.is_edit();
                if is_edit {
                    self.push_undo(index);
                }
                let Some(block) = self.blocks.get_mut(index) else {
                    return Task::none();
                };
                block.content.perform(action);
                if is_edit {
                    self.mark_dirty();
                }
                self.update_title()
            }

            Message::ToggleTodo(index) => {
                self.selected_block = index;
                self.push_undo_step();
                let Some(block) = self.blocks.get_mut(index) else {
                    return Task::none();
                };
                if let BlockKind::Todo(_, done) = &mut block.kind {
                    *done = !*done;
                    self.mark_dirty();
                }
                self.update_title()
            }

            Message::SplitBlock {
                index,
                kind,
                before,
                after,
            } => {
                self.push_undo_step();
                let Some(block) = self.blocks.get_mut(index) else {
                    return Task::none();
                };
                block.content = text_editor::Content::with_text(&before);
                self.blocks.insert(
                    index + 1,
                    EditableBlock {
                        kind,
                        content: text_editor::Content::with_text(&after),
                    },
                );
                renumber_blocks(&mut self.blocks);
                self.selected_block = index + 1;
                self.mark_dirty();
                Task::batch([
                    Self::focus_block(index + 1).map(cosmic::action::app),
                    self.update_title(),
                ])
            }

            Message::ConvertBlock {
                index,
                kind,
                content,
            } => {
                self.selected_block = index;
                self.push_undo_step();
                let Some(block) = self.blocks.get_mut(index) else {
                    return Task::none();
                };
                block.kind = kind;
                block.content = text_editor::Content::with_text(&content);
                renumber_blocks(&mut self.blocks);
                self.mark_dirty();
                self.update_title()
            }

            Message::ChangeLevel { index, delta } => {
                self.selected_block = index;
                self.push_undo_step();
                let Some(block) = self.blocks.get_mut(index) else {
                    return Task::none();
                };
                let Some(kind) = Self::shift_level(&block.kind, delta) else {
                    return Task::none();
                };
                block.kind = kind;
                renumber_blocks(&mut self.blocks);
                self.mark_dirty();
                self.update_title()
            }

            Message::MergeUp { index } => {
                if index == 0 || index >= self.blocks.len() {
                    return Task::none();
                }
                self.push_undo_step();
                let merged = self.blocks.remove(index);
                let Some(prev) = self.blocks.get_mut(index - 1) else {
                    return Task::none();
                };
                let text = prev.content.text() + &merged.content.text();
                prev.content = text_editor::Content::with_text(&text);
                let cursor = Self::end_cursor(&prev.content);
                prev.content.move_to(cursor);
                renumber_blocks(&mut self.blocks);
                self.selected_block = index - 1;
                self.mark_dirty();
                Task::batch([
                    Self::focus_block(index - 1).map(cosmic::action::app),
                    self.update_title(),
                ])
            }

            Message::FocusPrevious(index) => {
                let Some(target) = Self::previous_editable(&self.blocks, index) else {
                    return Task::none();
                };
                self.selected_block = target;
                let Some(block) = self.blocks.get_mut(target) else {
                    return Task::none();
                };
                let cursor = Self::end_cursor(&block.content);
                block.content.move_to(cursor);
                Self::focus_block(target).map(cosmic::action::app)
            }

            Message::FocusNext(index) => {
                let Some(target) = Self::next_editable(&self.blocks, index) else {
                    return Task::none();
                };
                self.selected_block = target;
                let Some(block) = self.blocks.get_mut(target) else {
                    return Task::none();
                };
                block.content.move_to(text_editor::Cursor {
                    position: text_editor::Position { line: 0, column: 0 },
                    selection: None,
                });
                Self::focus_block(target).map(cosmic::action::app)
            }

            Message::FormatBlock { index, kind } => {
                self.selected_block = index;
                Task::batch([self.format_block(index, kind), self.update_title()])
            }

            Message::WrapInline { index, marker } => {
                self.selected_block = index;
                self.push_undo_step();
                let Some(block) = self.blocks.get_mut(index) else {
                    return Task::none();
                };
                // Code and divider blocks keep their own layout; inline
                // formatting only applies to regular content.
                if matches!(block.kind, BlockKind::Code(_) | BlockKind::Divider) {
                    return Task::none();
                }
                let (open, close) = marker.delimiters();
                let cursor = block.content.cursor();
                // Wrap the caret's line in place: `Position::column` is an
                // offset within the line, so operating on the line avoids
                // mixing up offsets across line breaks.
                let line_index = cursor
                    .position
                    .line
                    .min(block.content.line_count().saturating_sub(1));
                let Some(line) = block.content.line(line_index) else {
                    return Task::none();
                };
                // Only a selection within the caret's line can be wrapped in
                // place; anything else inserts the markers at the caret.
                let selection = cursor.selection.and_then(|anchor| {
                    (anchor.line == cursor.position.line)
                        .then_some((anchor.column, cursor.position.column))
                });
                let result =
                    editor::wrap_inline(&line.text, selection, cursor.position.column, open, close);
                let mut lines: Vec<String> = block
                    .content
                    .lines()
                    .map(|line| line.text.into_owned())
                    .collect();
                lines[line_index] = result.text;
                block.content = text_editor::Content::with_text(&lines.join("\n"));
                block.content.move_to(text_editor::Cursor {
                    position: text_editor::Position {
                        line: line_index,
                        column: result.caret,
                    },
                    selection: None,
                });
                self.mark_dirty();
                Task::batch([
                    Self::focus_block(index).map(cosmic::action::app),
                    self.update_title(),
                ])
            }

            Message::InsertDivider(index) => {
                self.selected_block = index;
                Task::batch([
                    self.format_block(index, BlockKind::Divider),
                    self.update_title(),
                ])
            }

            Message::Undo => {
                let Some(previous) = self.undo_stack.pop() else {
                    return Task::none();
                };
                let current = self.snapshot_blocks();
                self.redo_stack.push(current);
                self.restore_blocks(previous)
            }

            Message::Redo => {
                let Some(next) = self.redo_stack.pop() else {
                    return Task::none();
                };
                let current = self.snapshot_blocks();
                self.undo_stack.push(current);
                self.restore_blocks(next)
            }

            Message::SlashMenuOpen(index) => {
                self.slash_menu = Some(index);
                self.slash_selection = 0;
                self.selected_block = index;
                Task::none()
            }

            Message::SlashMenuClose => {
                self.slash_menu = None;
                Task::none()
            }

            Message::SlashMenuMove(delta) => {
                if self.slash_menu.is_none() {
                    return Task::none();
                }
                self.slash_selection = if delta > 0 {
                    (self.slash_selection + 1).min(SLASH_ITEMS.len() - 1)
                } else {
                    self.slash_selection.saturating_sub(1)
                };
                Task::none()
            }

            Message::SlashMenuSelect => {
                let Some(index) = self.slash_menu.take() else {
                    return Task::none();
                };
                let kind = SLASH_ITEMS[self.slash_selection].kind.clone();
                self.selected_block = index;
                Task::batch([self.format_block(index, kind), self.update_title()])
            }

            Message::SlashMenuPick { index, kind } => {
                self.slash_menu = None;
                self.selected_block = index;
                Task::batch([self.format_block(index, kind), self.update_title()])
            }

            Message::TogglePreview => {
                self.preview = !self.preview;
                if self.preview && self.markdown.is_none() {
                    let text = self.editor_text();
                    self.markdown = Some(markdown::Content::parse(&text));
                }
                let Some(path) = self.selected.clone() else {
                    return Task::none();
                };
                match self.vault.set_preview(&path, self.preview) {
                    Ok(_) => Task::none(),
                    Err(err) => {
                        eprintln!("failed to save preview preference: {err}");
                        self.toast(fl!("save-error"))
                    }
                }
            }

            Message::LinkClicked(url) => {
                if url.starts_with("http://") || url.starts_with("https://") {
                    match open::that_detached(&url) {
                        Ok(()) => {}
                        Err(err) => {
                            eprintln!("failed to open {url:?}: {err}");
                        }
                    }
                }
                Task::none()
            }

            Message::AutosaveTick => self.save_active(false),

            Message::SaveNow => self.save_active(true),
        }
    }

    /// Called when the search function is requested (Ctrl+F).
    fn on_search(&mut self) -> Task<cosmic::Action<Self::Message>> {
        text_input::focus(cosmic::widget::Id::new(SEARCH_ID)).map(cosmic::action::app)
    }

    /// Called when the escape key is pressed.
    fn on_escape(&mut self) -> Task<cosmic::Action<Self::Message>> {
        // A confirmation dialog takes precedence over clearing the search.
        if self.confirm.is_some() {
            self.confirm = None;
            return Task::none();
        }
        if !self.search.is_empty() {
            self.search.clear();
        }
        Task::none()
    }

    /// Called before closing the application.
    fn on_app_exit(&mut self) -> Option<Self::Message> {
        let _ = self.save_active(false);
        None
    }
}

impl AppModel {
    /// The note currently selected, if any.
    fn selected_note(&self) -> Option<&Note> {
        let path = self.selected.as_ref()?;
        self.vault.notes().iter().find(|note| note.path() == path)
    }

    /// The current markdown text of the editor, serialized from its blocks.
    fn editor_text(&self) -> String {
        let blocks: Vec<editor::Block> = self.blocks.iter().map(EditableBlock::to_block).collect();
        editor::serialize_markdown(&blocks)
    }

    /// Marks the editor as having unsaved changes and refreshes the preview
    /// markdown from the current blocks.
    fn mark_dirty(&mut self) {
        self.dirty = true;
        let text = self.editor_text();
        self.markdown = Some(markdown::Content::parse(&text));
    }

    /// Loads the given note into the editor.
    fn load_note(&mut self, note: &Note) {
        let body = note.body().to_owned();
        let title = note.title().to_owned();
        let preview = note.preview();
        self.load_body(&body, &title, preview);
    }

    /// Loads the given body, title, and preview preference into the editor.
    fn load_body(&mut self, body: &str, title: &str, preview: bool) {
        let mut blocks: Vec<EditableBlock> = editor::parse_markdown(body)
            .into_iter()
            .map(EditableBlock::from_block)
            .collect();
        // An empty note still gets one empty block so the editor has a
        // focusable surface that shows the placeholder.
        if blocks.is_empty() {
            blocks.push(EditableBlock::from_block(editor::Block::paragraph("")));
        }
        self.blocks = blocks;
        self.selected_block = 0;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.last_undo_at = None;
        self.slash_menu = None;
        title.clone_into(&mut self.title_draft);
        self.preview = preview;
        self.dirty = false;
        self.markdown = Some(markdown::Content::parse(body));
    }

    /// Empties the editor and clears the title.
    fn clear_editor(&mut self) {
        self.blocks.clear();
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.last_undo_at = None;
        self.slash_menu = None;
        self.title_draft.clear();
        self.dirty = false;
        self.markdown = None;
    }

    /// Creates a new note and opens it in the editor.
    fn new_note(&mut self) -> Task<cosmic::Action<Message>> {
        let save = self.save_active(false);
        match self.vault.create(&fl!("untitled")) {
            Ok(Some(note)) => {
                // Make sure the new note is visible, even from the trash view.
                self.set_page(Page::All);
                let path = note.path().to_path_buf();
                self.open_tabs.push(path.clone());
                self.selected = Some(path);
                self.load_note(&note);
                let focus =
                    text_input::focus(cosmic::widget::Id::new(TITLE_ID)).map(cosmic::action::app);
                Task::batch([
                    save,
                    focus,
                    self.toast(fl!("note-created")),
                    self.update_title(),
                ])
            }
            Ok(None) => save,
            Err(err) => {
                eprintln!("failed to create note: {err}");
                Task::batch([save, self.toast(fl!("save-error"))])
            }
        }
    }

    /// Saves the active note, optionally notifying the user.
    fn save_active(&mut self, notify: bool) -> Task<cosmic::Action<Message>> {
        let Some(path) = self.selected.clone() else {
            self.dirty = false;
            return Task::none();
        };
        if !self.dirty {
            return Task::none();
        }
        let body = self.editor_text();
        match self.vault.save(&path, &body) {
            Ok(Some(SaveOutcome::Saved)) => {
                self.dirty = false;
                if notify {
                    self.toast(fl!("note-saved"))
                } else {
                    Task::none()
                }
            }
            Ok(Some(SaveOutcome::ConflictCopy { path: conflict })) => {
                self.dirty = false;
                if let Some(note) = self.vault.notes().iter().find(|note| note.path() == path) {
                    let body = note.body().to_owned();
                    let title = note.title().to_owned();
                    let preview = note.preview();
                    self.load_body(&body, &title, preview);
                }
                let name = conflict
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.toast(fl!("conflict-copy", name = name))
            }
            Ok(None) => {
                self.dirty = false;
                Task::none()
            }
            Err(err) => {
                eprintln!("failed to save note: {err}");
                self.dirty = false;
                self.toast(fl!("save-error"))
            }
        }
    }

    /// Commits the title draft as a rename, if it changed.
    fn commit_title_rename(&mut self) {
        let Some(path) = self.selected.clone() else {
            return;
        };
        let Some(note) = self.vault.notes().iter().find(|note| note.path() == path) else {
            return;
        };
        if note.title() == self.title_draft {
            return;
        }
        match self.vault.rename(&path, &self.title_draft) {
            Ok(Some(updated)) => {
                updated.title().clone_into(&mut self.title_draft);
            }
            Ok(None) => {}
            Err(err) => {
                eprintln!("failed to rename note: {err}");
            }
        }
    }

    /// Permanently deletes the note with the given path.
    fn delete_note(&mut self, path: &Path) -> Task<cosmic::Action<Message>> {
        match self.vault.delete(path) {
            Ok(true) => {
                let mut tasks = vec![self.toast(fl!("note-deleted"))];
                tasks.push(self.close_tab(path));
                Task::batch(tasks)
            }
            Ok(false) => Task::none(),
            Err(err) => {
                eprintln!("failed to delete note: {err}");
                self.toast(fl!("save-error"))
            }
        }
    }

    /// Permanently deletes every trashed note.
    fn empty_trash(&mut self) -> Task<cosmic::Action<Message>> {
        match self.vault.empty_trash() {
            Ok(count) if count > 0 => {
                let mut tasks = Vec::new();
                let active_gone = self.selected.as_ref().is_some_and(|selected| {
                    self.vault
                        .notes()
                        .iter()
                        .all(|note| note.path() != selected)
                });
                self.open_tabs
                    .retain(|tab| self.vault.notes().iter().any(|note| note.path() == tab));
                if active_gone {
                    self.selected = None;
                    self.clear_editor();
                    tasks.push(self.update_title());
                }
                tasks.push(self.toast(fl!("trash-emptied")));
                Task::batch(tasks)
            }
            Ok(_) => Task::none(),
            Err(err) => {
                eprintln!("failed to empty trash: {err}");
                self.toast(fl!("save-error"))
            }
        }
    }

    /// Activates the given sidebar page in the page selector.
    fn set_page(&mut self, page: Page) {
        self.page = page;
        let entity = self
            .page_buttons
            .iter()
            .find(|entity| self.page_buttons.data::<Page>(*entity).copied() == Some(page));
        if let Some(entity) = entity {
            self.page_buttons.activate(entity);
        }
    }

    /// Opens the note with the given path in a tab, switching to it.
    fn open_tab(&mut self, path: &Path) -> Task<cosmic::Action<Message>> {
        self.commit_title_rename();
        let save = self.save_active(false);
        if !self.open_tabs.iter().any(|tab| tab == path) {
            self.open_tabs.push(path.to_path_buf());
        }
        self.selected = Some(path.to_path_buf());
        if let Some(note) = self.vault.notes().iter().find(|note| note.path() == path) {
            let body = note.body().to_owned();
            let title = note.title().to_owned();
            let preview = note.preview();
            self.load_body(&body, &title, preview);
            // Drop straight into the first block, like Futo does.
            self.sync_tabs();
            Task::batch([
                save,
                Self::focus_block(0).map(cosmic::action::app),
                self.update_title(),
            ])
        } else {
            self.sync_tabs();
            Task::batch([save, self.update_title()])
        }
    }

    /// Closes the tab of the note with the given path, activating a neighbor
    /// when the active tab is closed.
    fn close_tab(&mut self, path: &Path) -> Task<cosmic::Action<Message>> {
        let was_active = self.selected.as_deref() == Some(path);
        let index = self.open_tabs.iter().position(|tab| tab == path);
        if let Some(index) = index {
            self.open_tabs.remove(index);
        }
        if !was_active {
            self.sync_tabs();
            return Task::none();
        }
        self.selected = None;
        // Prefer the tab after the closed one, falling back to the previous.
        let next = self
            .open_tabs
            .get(index.unwrap_or(0))
            .or_else(|| self.open_tabs.last())
            .cloned();
        if let Some(path) = next {
            return self.open_tab(&path);
        }
        self.sync_tabs();
        self.clear_editor();
        self.update_title()
    }

    /// Rebuilds the tab bar model from [`Self::open_tabs`], activating the
    /// tab of the selected note (or the first tab).
    fn sync_tabs(&mut self) {
        let active = self
            .selected
            .as_ref()
            .filter(|path| self.open_tabs.contains(path));
        let mut tabs = segmented_button::Model::default();
        for path in &self.open_tabs {
            let title = self
                .vault
                .notes()
                .iter()
                .find(|note| note.path() == path)
                .map_or_else(|| fl!("untitled"), |note| note.title().to_owned());
            tabs.insert().text(title).data(path.clone()).closable();
        }
        let entity = active
            .and_then(|path| {
                tabs.iter()
                    .find(|entity| tabs.data::<PathBuf>(*entity) == Some(path))
            })
            .or_else(|| tabs.iter().next());
        if let Some(entity) = entity {
            tabs.activate(entity);
        }
        self.tabs = tabs;
    }

    /// The tab bar over the open notes, if any.
    fn tab_bar(&self) -> Option<Element<'_, Message>> {
        if self.open_tabs.is_empty() {
            return None;
        }
        let paths: HashMap<segmented_button::Entity, PathBuf> = self
            .tabs
            .iter()
            .filter_map(|entity| {
                self.tabs
                    .data::<PathBuf>(entity)
                    .cloned()
                    .map(|path| (entity, path))
            })
            .collect();
        let activate_paths = paths.clone();
        Some(
            tab_bar::horizontal(&self.tabs)
                .on_activate(move |entity| {
                    activate_paths
                        .get(&entity)
                        .cloned()
                        .map_or(Message::Ignored, Message::ActivateTab)
                })
                .on_close(move |entity| {
                    paths
                        .get(&entity)
                        .cloned()
                        .map_or(Message::Ignored, Message::CloseTab)
                })
                .into(),
        )
    }

    /// Pushes a toast notification.
    fn toast(&mut self, message: String) -> Task<cosmic::Action<Message>> {
        self.toasts
            .push(toaster::Toast::new(message).duration(Duration::from_secs(3)))
            .map(cosmic::action::app)
    }

    /// The notes shown in the middle pane, filtered by page and search query.
    fn visible_notes(&self) -> Vec<&Note> {
        let query = self.search.trim().to_lowercase();
        self.vault
            .notes()
            .iter()
            .filter(|note| {
                let in_trash = note.in_trash();
                let matches_page = match self.page {
                    Page::All => !in_trash,
                    Page::Pinned => !in_trash && note.pinned(),
                    Page::Trash => in_trash,
                };
                matches_page
                    && (query.is_empty()
                        || note.title().to_lowercase().contains(&query)
                        || note.snippet().to_lowercase().contains(&query)
                        || note.body().to_lowercase().contains(&query))
            })
            .collect()
    }

    /// The left sidebar: page selector, search, and the note tree (or the
    /// trashed-note list on the trash page).
    fn sidebar_pane(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let space_s = spacing.space_s;

        // Page selector: all notes, pinned, or trash.
        let pages: HashMap<segmented_button::Entity, Page> = self
            .page_buttons
            .iter()
            .filter_map(|entity| {
                self.page_buttons
                    .data::<Page>(entity)
                    .copied()
                    .map(|page| (entity, page))
            })
            .collect();
        let page_selector: Element<'_, Message> = segmented_button::vertical(&self.page_buttons)
            .on_activate(move |entity| {
                pages
                    .get(&entity)
                    .copied()
                    .map_or(Message::Ignored, Message::PageChanged)
            })
            .into();

        let search = text_input::search_input(fl!("search-placeholder"), &self.search)
            .id(cosmic::widget::Id::new(SEARCH_ID))
            .on_input(Message::SearchChanged)
            .width(Length::Fill);

        let body: Element<'_, Message> = if self.page == Page::Trash {
            self.trash_list()
        } else {
            crate::sidebar::tree_view(
                &self.vault,
                &self.expanded,
                self.selected.as_deref(),
                self.page,
                &self.search,
            )
        };

        let mut column = widget::column::with_capacity(4)
            .push(
                widget::container(page_selector)
                    .padding([space_s, space_s, 0, space_s])
                    .width(Length::Fill),
            )
            .push(widget::container(search).padding([space_s, space_s, 0, space_s]))
            .push(widget::scrollable(body).height(Length::Fill))
            .width(300)
            .height(Length::Fill);

        if self.page == Page::Trash && self.vault.notes().iter().any(Note::in_trash) {
            column = column.push(
                widget::container(
                    button::text(fl!("empty-trash"))
                        .on_press(Message::EmptyTrash)
                        .class(cosmic::theme::Button::Destructive),
                )
                .padding([0, space_s, space_s, space_s])
                .width(Length::Fill)
                .align_x(Horizontal::Center),
            );
        }

        column.into()
    }

    /// The flat list of trashed notes (the tree hides the trash directory).
    fn trash_list(&self) -> Element<'_, Message> {
        let notes = self.visible_notes();
        if notes.is_empty() {
            return self.list_empty_state();
        }
        let mut list = list_column::with_capacity(notes.len());
        for note in notes {
            let selected = self.selected.as_deref() == Some(note.path());
            list = list.add(
                list_column::button(Self::note_row(note))
                    .selected(selected)
                    .on_press(Message::SelectNote(note.path().to_path_buf())),
            );
        }
        list.into_element()
    }

    /// A single note row in the list.
    fn note_row(note: &Note) -> Element<'_, Message> {
        let space_xxs = cosmic::theme::spacing().space_xxs;

        let title_row = if note.pinned() {
            widget::row::with_capacity(2)
                .push(icon::from_name("starred-symbolic").size(12))
                .push(widget::text::title4(note.title()))
                .spacing(space_xxs)
        } else {
            widget::row::with_capacity(1).push(widget::text::title4(note.title()))
        };

        widget::column::with_capacity(3)
            .push(title_row)
            .push(widget::text::caption(note.snippet()))
            .push(widget::text::caption(relative_time(note.modified())))
            .spacing(space_xxs)
            .into()
    }

    /// The empty state shown in the note list.
    fn list_empty_state(&self) -> Element<'_, Message> {
        let space_s = cosmic::theme::spacing().space_s;
        let page = self.page;
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
        .align_x(Horizontal::Center)
        .align_y(Vertical::Center)
        .into()
    }

    /// The right pane: the editor or preview for the active note.
    fn editor_pane(&self) -> Element<'_, Message> {
        let Some(note) = self.selected_note() else {
            return Self::editor_empty_state();
        };
        let spacing = cosmic::theme::spacing();

        let title_input = text_input::text_input(fl!("note-title-placeholder"), &self.title_draft)
            .id(cosmic::widget::Id::new(TITLE_ID))
            .on_input(Message::EditTitle)
            .on_submit(|_| Message::RenameSubmit)
            .width(Length::Fill);

        let pin_icon = if note.pinned() {
            "starred-symbolic"
        } else {
            "non-starred-symbolic"
        };
        let pin = button::icon(icon::from_name(pin_icon))
            .on_press(Message::TogglePin)
            .selected(note.pinned())
            .class(cosmic::theme::Button::Icon);
        let pin = Self::tooltip(
            pin.into(),
            if note.pinned() {
                fl!("unpin")
            } else {
                fl!("pin")
            },
        );

        let edit = button::text(fl!("edit"))
            .on_press(Message::TogglePreview)
            .class(if self.preview {
                cosmic::theme::Button::Text
            } else {
                cosmic::theme::Button::Suggested
            });
        let preview = button::text(fl!("preview"))
            .on_press(Message::TogglePreview)
            .class(if self.preview {
                cosmic::theme::Button::Suggested
            } else {
                cosmic::theme::Button::Text
            });

        let header = widget::row::with_capacity(4)
            .push(title_input)
            .push(divider::vertical::default())
            .push(edit)
            .push(preview)
            .push(divider::vertical::default())
            .push(pin)
            .push(Self::note_actions(note))
            .spacing(spacing.space_xs)
            .align_y(Alignment::Center);

        let body: Element<'_, Message> = if self.preview {
            let settings = Self::markdown_settings();
            let items: &[markdown::Item] =
                self.markdown.as_ref().map_or(&[], markdown::Content::items);
            widget::scrollable(
                widget::container(markdown::view(items, settings).map(Message::LinkClicked))
                    .padding([spacing.space_s, spacing.space_s])
                    .width(Length::Fill),
            )
            .height(Length::Fill)
            .into()
        } else {
            self.block_editor()
        };

        let mut column = widget::column::with_capacity(4)
            .push(widget::container(header).padding([spacing.space_s, spacing.space_s]))
            .push(divider::horizontal::default());
        if !self.preview {
            column = column.push(self.block_toolbar());
        }
        column = column.push(body);
        if let Some(panel) = self.backlinks_panel() {
            column = column.push(divider::horizontal::default()).push(panel);
        }
        column.width(Length::Fill).height(Length::Fill).into()
    }

    /// A panel listing the notes that link to the active note, if any.
    fn backlinks_panel(&self) -> Option<Element<'_, Message>> {
        let path = self.selected.as_ref()?;
        let links = crate::backlinks::backlinks(&self.vault, path);
        if links.is_empty() {
            return None;
        }
        let spacing = cosmic::theme::spacing();
        let mut column = widget::column::with_capacity(links.len() + 1)
            .push(widget::text::title4(fl!("backlinks")))
            .spacing(spacing.space_xxs);
        for link in &links {
            let title = self
                .vault
                .notes()
                .iter()
                .find(|note| note.path() == link.from)
                .map_or_else(String::new, |note| note.title().to_owned());
            let row: Element<'_, Message> = button::custom(widget::text::body(title))
                .on_press(Message::SelectNote(link.from.clone()))
                .class(cosmic::theme::Button::ListItem([0.0, 0.0, 0.0, 0.0]))
                .width(Length::Fill)
                .into();
            column = column.push(row);
        }
        Some(
            widget::container(column)
                .padding([spacing.space_s, spacing.space_s])
                .width(Length::Fill)
                .into(),
        )
    }

    /// The block editor: a scrollable column of per-block text editors.
    fn block_editor(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let mut column = widget::column::with_capacity(self.blocks.len());
        for (index, block) in self.blocks.iter().enumerate() {
            column = column.push(Self::block_element(
                index,
                block,
                &self.blocks,
                self.slash_menu == Some(index),
                self.slash_selection,
            ));
        }
        widget::scrollable(
            widget::container(column.spacing(0))
                .padding([spacing.space_s, spacing.space_s])
                .width(Length::Fill),
        )
        .height(Length::Fill)
        .into()
    }

    /// Renders a single block as an editable widget, wrapped in the slash
    /// command popover when it is open for that block.
    fn block_element<'a>(
        index: usize,
        block: &'a EditableBlock,
        blocks: &'a [EditableBlock],
        slash_open: bool,
        slash_selection: usize,
    ) -> Element<'a, Message> {
        let spacing = cosmic::theme::spacing();
        let mut editor = cosmic::widget::text_editor(&block.content)
            .id(cosmic::widget::Id::new(format!("block-{index}")))
            .on_action(move |action| Message::BlockAction { index, action })
            .key_binding(move |key_press| {
                Self::block_key_binding(index, block, blocks, key_press, slash_open)
            })
            .highlight("markdown", Self::highlight_theme())
            .size(15)
            .padding([spacing.space_xxs, 0]);
        if index == 0 {
            editor = editor.placeholder(fl!("note-placeholder"));
        }
        let element: Element<'a, Message> = match &block.kind {
            BlockKind::Paragraph => editor.into(),
            BlockKind::Heading(level) => editor
                .font(cosmic::font::semibold())
                .size(Self::heading_size(*level))
                .into(),
            BlockKind::Quote(_level) => widget::row::with_capacity(2)
                .push(Self::quote_stripe())
                .push(editor)
                .align_y(Alignment::Center)
                .into(),
            BlockKind::Bullet(_level) => {
                Self::list_item(Self::indent(block.kind.level()), "•", editor)
            }
            BlockKind::Numbered(_level, number) => {
                Self::list_item(Self::indent(block.kind.level()), number.to_string(), editor)
            }
            BlockKind::Todo(_level, done) => {
                let checkbox = cosmic::widget::checkbox(*done)
                    .size(16)
                    .on_toggle(move |_| Message::ToggleTodo(index));
                widget::row::with_capacity(3)
                    .push(widget::text(Self::indent(block.kind.level())))
                    .push(checkbox)
                    .push(editor)
                    .align_y(Alignment::Center)
                    .into()
            }
            BlockKind::Code(_lang) => widget::container(editor.font(cosmic::font::mono()))
                .class(cosmic::theme::Container::Card)
                .padding([spacing.space_xxs, spacing.space_xs])
                .width(Length::Fill)
                .into(),
            BlockKind::Divider => widget::container(divider::horizontal::default())
                .padding([spacing.space_s, 0])
                .width(Length::Fill)
                .into(),
        };
        if slash_open {
            let popup = Self::slash_menu_element(index, slash_selection);
            return popover::popover(element)
                .position(popover::Position::Bottom)
                .on_close(Message::SlashMenuClose)
                .popup(popup)
                .into();
        }
        element
    }

    /// The slash command menu popup: the block-style conversions, with the
    /// highlighted entry marked.
    fn slash_menu_element(index: usize, selection: usize) -> Element<'static, Message> {
        let spacing = cosmic::theme::spacing();
        let mut column =
            widget::column::with_capacity(SLASH_ITEMS.len()).spacing(spacing.space_xxs);
        for (i, item) in SLASH_ITEMS.iter().enumerate() {
            let content = widget::row::with_capacity(2)
                .push(icon::icon(Self::toolbar_icon(item.icon)).size(16))
                .push(widget::text(Self::slash_label(item.label)))
                .spacing(spacing.space_s)
                .align_y(Alignment::Center);
            column = column.push(
                button::custom(content)
                    .on_press(Message::SlashMenuPick {
                        index,
                        kind: item.kind.clone(),
                    })
                    .selected(i == selection)
                    .class(cosmic::theme::Button::Text)
                    .width(Length::Fill),
            );
        }
        widget::container(column)
            .class(cosmic::theme::Container::Card)
            .padding([spacing.space_xs, spacing.space_s])
            .into()
    }

    /// The localized label for a slash menu entry key.
    #[must_use]
    fn slash_label(key: &str) -> String {
        match key {
            "paragraph" => fl!("paragraph"),
            "heading-1" => fl!("heading-1"),
            "heading-2" => fl!("heading-2"),
            "heading-3" => fl!("heading-3"),
            "bullet-list" => fl!("bullet-list"),
            "numbered-list" => fl!("numbered-list"),
            "todo-list" => fl!("todo-list"),
            "quote" => fl!("quote"),
            "code-block" => fl!("code-block"),
            "divider" => fl!("divider"),
            _ => key.to_owned(),
        }
    }

    /// Renders a list marker followed by the block's editor.
    fn list_item<'a, E>(
        indent: String,
        marker: impl Into<String>,
        editor: E,
    ) -> Element<'a, Message>
    where
        E: Into<Element<'a, Message>>,
    {
        let mut label = indent;
        label.push_str(&marker.into());
        widget::row::with_capacity(2)
            .push(widget::text(label))
            .push(editor)
            .align_y(Alignment::Center)
            .into()
    }

    /// The indentation prefix for a nesting level (two spaces per level).
    #[must_use]
    fn indent(level: u8) -> String {
        "  ".repeat(usize::from(level))
    }

    /// The font size for a heading of the given level.
    #[must_use]
    fn heading_size(level: u8) -> f32 {
        match level {
            1 => 24.0,
            2 => 21.0,
            3 => 18.0,
            4 => 16.0,
            _ => 15.0,
        }
    }

    /// The syntax highlighting theme for the current color scheme.
    #[must_use]
    fn highlight_theme() -> cosmic::iced::highlighter::Theme {
        if cosmic::theme::active_type().is_dark() {
            cosmic::iced::highlighter::Theme::SolarizedDark
        } else {
            cosmic::iced::highlighter::Theme::InspiredGitHub
        }
    }

    /// A thin vertical accent stripe used as the quote block marker.
    fn quote_stripe() -> Element<'static, Message> {
        let mut color: cosmic::iced::Color = cosmic::theme::active().cosmic().accent.base.into();
        color.a = 0.6;
        widget::container(
            widget::space()
                .width(Length::Fixed(3.0))
                .height(Length::Fill),
        )
        .style(move |_theme| cosmic::iced::widget::container::Style {
            background: Some(cosmic::iced::Background::Color(color)),
            ..Default::default()
        })
        .into()
    }

    /// The formatting toolbar for the selected block: block-style conversions
    /// and inline markdown wrapping. The active style is highlighted.
    fn block_toolbar(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let index = self.selected_block.min(self.blocks.len().saturating_sub(1));
        let kind = self
            .blocks
            .get(index)
            .map_or(&BlockKind::Paragraph, |block| &block.kind);

        let mut row = Self::style_buttons(index, kind);
        row = row.push(divider::vertical::default());
        row = row.push(Self::inline_button(
            index,
            Self::toolbar_icon("format-text-bold-symbolic"),
            InlineMarker::Bold,
            fl!("bold"),
        ));
        row = row.push(Self::inline_button(
            index,
            Self::toolbar_icon("format-text-italic-symbolic"),
            InlineMarker::Italic,
            fl!("italic"),
        ));
        row = row.push(Self::inline_button(
            index,
            Self::toolbar_icon("format-text-strikethrough-symbolic"),
            InlineMarker::Strikethrough,
            fl!("strikethrough"),
        ));
        row = row.push(Self::inline_button(
            index,
            Self::toolbar_icon("format-text-code-symbolic"),
            InlineMarker::Code,
            fl!("inline-code"),
        ));

        row = row.push(divider::vertical::default());
        row = row.push(Self::level_button(
            index,
            Self::toolbar_icon("format-indent-less-symbolic"),
            -1,
            fl!("outdent"),
        ));
        row = row.push(Self::level_button(
            index,
            Self::toolbar_icon("format-indent-more-symbolic"),
            1,
            fl!("indent"),
        ));

        widget::container(row)
            .width(Length::Fill)
            .padding([spacing.space_xxs, spacing.space_s])
            .into()
    }

    /// The block-style conversion buttons for the selected block, with the
    /// active style highlighted.
    fn style_buttons(
        index: usize,
        kind: &BlockKind,
    ) -> cosmic::widget::Row<'static, Message, cosmic::Theme, cosmic::Renderer> {
        let spacing = cosmic::theme::spacing();
        let mut row = widget::row::with_capacity(10).spacing(spacing.space_xxs);
        row = row.push(Self::style_button(
            index,
            *kind == BlockKind::Paragraph,
            Self::toolbar_icon("format-justify-left-symbolic"),
            BlockKind::Paragraph,
            fl!("paragraph"),
        ));
        row = row.push(Self::style_button(
            index,
            *kind == BlockKind::Heading(1),
            Self::toolbar_icon("format-text-heading-1-symbolic"),
            BlockKind::Heading(1),
            fl!("heading-1"),
        ));
        row = row.push(Self::style_button(
            index,
            *kind == BlockKind::Heading(2),
            Self::toolbar_icon("format-text-heading-2-symbolic"),
            BlockKind::Heading(2),
            fl!("heading-2"),
        ));
        row = row.push(Self::style_button(
            index,
            *kind == BlockKind::Heading(3),
            Self::toolbar_icon("format-text-heading-3-symbolic"),
            BlockKind::Heading(3),
            fl!("heading-3"),
        ));
        row = row.push(Self::style_button(
            index,
            *kind == BlockKind::Bullet(0),
            Self::toolbar_icon("format-list-unordered-symbolic"),
            BlockKind::Bullet(0),
            fl!("bullet-list"),
        ));
        row = row.push(Self::style_button(
            index,
            *kind == BlockKind::Numbered(0, 1),
            Self::toolbar_icon("format-list-ordered-symbolic"),
            BlockKind::Numbered(0, 1),
            fl!("numbered-list"),
        ));
        row = row.push(Self::style_button(
            index,
            matches!(kind, BlockKind::Todo(..)),
            Self::toolbar_icon("format-list-checkbox-symbolic"),
            BlockKind::Todo(0, false),
            fl!("todo-list"),
        ));
        row = row.push(Self::style_button(
            index,
            matches!(kind, BlockKind::Quote(_)),
            Self::toolbar_icon("format-quote-block-symbolic"),
            BlockKind::Quote(0),
            fl!("quote"),
        ));
        row = row.push(Self::style_button(
            index,
            matches!(kind, BlockKind::Code(_)),
            Self::toolbar_icon("format-text-code-symbolic"),
            BlockKind::Code(String::new()),
            fl!("code-block"),
        ));
        row = row.push(Self::style_button(
            index,
            *kind == BlockKind::Divider,
            Self::toolbar_icon("insert-horizontal-rule-symbolic"),
            BlockKind::Divider,
            fl!("divider"),
        ));

        row
    }

    /// A toolbar button that converts the selected block to the given kind.
    fn style_button(
        index: usize,
        active: bool,
        handle: icon::Handle,
        kind: BlockKind,
        label: String,
    ) -> Element<'static, Message> {
        let button = button::icon(handle)
            .on_press(Message::FormatBlock { index, kind })
            .selected(active)
            .class(cosmic::theme::Button::Icon);
        Self::tooltip(button.into(), label)
    }

    /// A toolbar button that wraps the selected block's selection in the
    /// given inline markdown markers.
    fn inline_button(
        index: usize,
        handle: icon::Handle,
        marker: InlineMarker,
        label: String,
    ) -> Element<'static, Message> {
        let button = button::icon(handle)
            .on_press(Message::WrapInline { index, marker })
            .class(cosmic::theme::Button::Icon);
        Self::tooltip(button.into(), label)
    }

    /// A toolbar button that shifts the nesting level of the selected block.
    fn level_button(
        index: usize,
        handle: icon::Handle,
        delta: i8,
        label: String,
    ) -> Element<'static, Message> {
        let button = button::icon(handle)
            .on_press(Message::ChangeLevel { index, delta })
            .class(cosmic::theme::Button::Icon);
        Self::tooltip(button.into(), label)
    }

    /// Loads a toolbar icon, using bundled SVGs for glyphs that the icon
    /// theme does not provide (headings, lists, quote, code, divider).
    fn toolbar_icon(name: &'static str) -> icon::Handle {
        match name {
            "format-text-heading-1-symbolic" => icon::from_svg_bytes(include_bytes!(
                "../resources/icons/format-text-heading-1-symbolic.svg"
            ))
            .symbolic(true),
            "format-text-heading-2-symbolic" => icon::from_svg_bytes(include_bytes!(
                "../resources/icons/format-text-heading-2-symbolic.svg"
            ))
            .symbolic(true),
            "format-text-heading-3-symbolic" => icon::from_svg_bytes(include_bytes!(
                "../resources/icons/format-text-heading-3-symbolic.svg"
            ))
            .symbolic(true),
            "format-list-unordered-symbolic" => icon::from_svg_bytes(include_bytes!(
                "../resources/icons/format-list-unordered-symbolic.svg"
            ))
            .symbolic(true),
            "format-list-ordered-symbolic" => icon::from_svg_bytes(include_bytes!(
                "../resources/icons/format-list-ordered-symbolic.svg"
            ))
            .symbolic(true),
            "format-list-checkbox-symbolic" => icon::from_svg_bytes(include_bytes!(
                "../resources/icons/format-list-checkbox-symbolic.svg"
            ))
            .symbolic(true),
            "format-quote-block-symbolic" => icon::from_svg_bytes(include_bytes!(
                "../resources/icons/format-quote-block-symbolic.svg"
            ))
            .symbolic(true),
            "format-text-code-symbolic" => icon::from_svg_bytes(include_bytes!(
                "../resources/icons/format-text-code-symbolic.svg"
            ))
            .symbolic(true),
            "insert-horizontal-rule-symbolic" => icon::from_svg_bytes(include_bytes!(
                "../resources/icons/insert-horizontal-rule-symbolic.svg"
            ))
            .symbolic(true),
            _ => icon::from_name(name).handle(),
        }
    }

    /// The document-wide undo/redo binding for a key press, if it matches
    /// Ctrl+Z (undo) or Ctrl+Shift+Z / Ctrl+Y (redo). Returns `None` for
    /// anything else.
    fn undo_redo_binding(
        key_press: &text_editor::KeyPress,
    ) -> Option<text_editor::Binding<Message>> {
        use cosmic::widget::text_editor::Binding;
        if !key_press.modifiers.control() {
            return None;
        }
        match (key_press.modified_key.as_ref(), key_press.modifiers.shift()) {
            (Key::Character("z"), false) => Some(Binding::Custom(Message::Undo)),
            (Key::Character("z"), true) | (Key::Character("y"), false) => {
                Some(Binding::Custom(Message::Redo))
            }
            _ => None,
        }
    }

    /// The binding while the slash menu is open: navigation and selection,
    /// swallowing all other keys so they do not reach the block editor.
    fn slash_menu_binding(
        key_press: &text_editor::KeyPress,
    ) -> Option<text_editor::Binding<Message>> {
        use cosmic::iced::keyboard::key::Named;
        use cosmic::widget::text_editor::Binding;
        match key_press.modified_key.as_ref() {
            Key::Named(Named::Escape) => Some(Binding::Custom(Message::SlashMenuClose)),
            Key::Named(Named::ArrowUp) => Some(Binding::Custom(Message::SlashMenuMove(-1))),
            Key::Named(Named::ArrowDown) => Some(Binding::Custom(Message::SlashMenuMove(1))),
            Key::Named(Named::Enter | Named::Tab) => {
                Some(Binding::Custom(Message::SlashMenuSelect))
            }
            _ => None,
        }
    }

    /// Whether typing the given character should open the slash menu: a `/`
    /// at the start of an empty paragraph.
    fn slash_trigger(block: &EditableBlock, text: Option<&str>) -> bool {
        matches!(block.kind, BlockKind::Paragraph)
            && block.content.text().is_empty()
            && text == Some("/")
    }

    /// Maps a key press in a block's editor to a block-level behavior, falling
    /// back to the default text editor binding.
    ///
    /// This is where Futo-style editing lives: typed markdown prefixes convert
    /// the block, Enter splits and continues lists, Backspace at the start
    /// merges or de-lists, and the arrow keys move the caret across blocks.
    fn block_key_binding(
        index: usize,
        block: &EditableBlock,
        blocks: &[EditableBlock],
        key_press: text_editor::KeyPress,
        slash_open: bool,
    ) -> Option<text_editor::Binding<Message>> {
        use cosmic::iced::keyboard::key::Named;
        use cosmic::widget::text_editor::{Binding, Status};

        // Only intercept keys while the editor is focused.
        if !matches!(key_press.status, Status::Focused { .. }) {
            return Binding::from_key_press(key_press);
        }

        // While the slash menu is open for this block, it owns the keyboard.
        if slash_open {
            return Self::slash_menu_binding(&key_press);
        }

        // Document-wide undo/redo: the editor's own Ctrl+Z only rewinds the
        // focused block, so intercept it here (code blocks included, which
        // would otherwise keep their per-block history).
        if let Some(binding) = Self::undo_redo_binding(&key_press) {
            return Some(binding);
        }

        // Code blocks keep their own newlines; dividers have no editor. In
        // code, Tab inserts a literal tab.
        if matches!(block.kind, BlockKind::Code(_) | BlockKind::Divider) {
            if matches!(block.kind, BlockKind::Code(_))
                && matches!(key_press.modified_key.as_ref(), Key::Named(Named::Tab))
                && !key_press.modifiers.shift()
            {
                return Some(text_editor::Binding::Insert('\t'));
            }
            return Binding::from_key_press(key_press);
        }

        let cursor = block.content.cursor();
        let position = cursor.position;

        match key_press.modified_key.as_ref() {
            Key::Named(Named::Enter) => Some(Self::enter_binding(index, block)),
            Key::Named(Named::Backspace) => {
                if position.line != 0 || position.column != 0 {
                    // Not at the start of the block: default backspace.
                    return Binding::from_key_press(key_press);
                }
                Self::backspace_binding(index, block, key_press)
            }
            Key::Named(Named::Tab) => {
                let delta = if key_press.modifiers.shift() { -1 } else { 1 };
                Some(Binding::Custom(Message::ChangeLevel { index, delta }))
            }
            Key::Named(Named::ArrowUp) if !key_press.modifiers.shift() => {
                if position.line == 0
                    && position.column == 0
                    && Self::previous_editable(blocks, index).is_some()
                {
                    Some(Binding::Custom(Message::FocusPrevious(index)))
                } else {
                    Binding::from_key_press(key_press)
                }
            }
            Key::Named(Named::ArrowDown) if !key_press.modifiers.shift() => {
                let last_line = block.content.line_count().saturating_sub(1);
                let end = block
                    .content
                    .line(last_line)
                    .map_or(0, |line| line.text.len());
                if position.line == last_line
                    && position.column == end
                    && Self::next_editable(blocks, index).is_some()
                {
                    Some(Binding::Custom(Message::FocusNext(index)))
                } else {
                    Binding::from_key_press(key_press)
                }
            }
            Key::Character(" ")
                if !key_press.modifiers.control()
                    && !key_press.modifiers.alt()
                    && !key_press.modifiers.shift() =>
            {
                // Space on a todo checkbox toggles it instead of inserting a
                // space at the start of the content.
                if editor::space_toggles_todo(
                    &block.kind,
                    position.line,
                    position.column,
                    block.content.cursor().selection.is_some(),
                ) {
                    Some(Binding::Custom(Message::ToggleTodo(index)))
                } else {
                    Binding::from_key_press(key_press)
                }
            }
            _ if key_press.modifiers.control() && !key_press.modifiers.shift() => {
                // Inline formatting shortcuts, matching common editors.
                match key_press.modified_key.as_ref() {
                    Key::Character("b") => Some(Binding::Custom(Message::WrapInline {
                        index,
                        marker: InlineMarker::Bold,
                    })),
                    Key::Character("i") => Some(Binding::Custom(Message::WrapInline {
                        index,
                        marker: InlineMarker::Italic,
                    })),
                    Key::Character("k") => Some(Binding::Custom(Message::WrapInline {
                        index,
                        marker: InlineMarker::Code,
                    })),
                    Key::Character("x") => Some(Binding::Custom(Message::WrapInline {
                        index,
                        marker: InlineMarker::Strikethrough,
                    })),
                    _ => Binding::from_key_press(key_press),
                }
            }
            _ => Self::prefix_binding(index, block, &key_press)
                .or_else(|| Binding::from_key_press(key_press)),
        }
    }

    /// Shifts the nesting level of a structured block by `delta`, clamped to
    /// `0..=6`. Returns `None` for kinds without a nesting level.
    fn shift_level(kind: &BlockKind, delta: i8) -> Option<BlockKind> {
        let level = kind.level().saturating_add_signed(delta).clamp(0, 6);
        match kind {
            BlockKind::Quote(_) => Some(BlockKind::Quote(level)),
            BlockKind::Bullet(_) => Some(BlockKind::Bullet(level)),
            BlockKind::Numbered(_, number) => Some(BlockKind::Numbered(level, *number)),
            BlockKind::Todo(_, done) => Some(BlockKind::Todo(level, *done)),
            BlockKind::Paragraph
            | BlockKind::Heading(_)
            | BlockKind::Code(_)
            | BlockKind::Divider => None,
        }
    }

    /// The binding for a typed character that may start a block conversion:
    /// a space completing a markdown prefix, three backticks opening a code
    /// fence, or a slash opening the command menu. Returns `None` for any
    /// other input.
    fn prefix_binding(
        index: usize,
        block: &EditableBlock,
        key_press: &text_editor::KeyPress,
    ) -> Option<text_editor::Binding<Message>> {
        use cosmic::widget::text_editor::Binding;
        if key_press.text.as_deref() == Some(" ") {
            // A space may complete a typed markdown prefix.
            if let Some(kind) = editor::prefix_kind(&block.content.text()) {
                return Some(Binding::Custom(Message::ConvertBlock {
                    index,
                    kind,
                    content: String::new(),
                }));
            }
        } else if key_press.text.as_deref() == Some("`") && block.content.text() == "``" {
            // Three backticks open a fenced code block.
            return Some(Binding::Custom(Message::ConvertBlock {
                index,
                kind: BlockKind::Code(String::new()),
                content: String::new(),
            }));
        } else if Self::slash_trigger(block, key_press.text.as_deref()) {
            // A slash in an empty paragraph opens the command menu.
            return Some(Binding::Custom(Message::SlashMenuOpen(index)));
        }
        None
    }

    /// The binding for Backspace at the very start of a block: un-nests a
    /// nested item, removes the marker of a top-level item, or merges the
    /// block into the previous one.
    fn backspace_binding(
        index: usize,
        block: &EditableBlock,
        key_press: text_editor::KeyPress,
    ) -> Option<text_editor::Binding<Message>> {
        use cosmic::widget::text_editor::Binding;
        let level = block.kind.level();
        match &block.kind {
            BlockKind::Quote(_)
            | BlockKind::Bullet(_)
            | BlockKind::Numbered(..)
            | BlockKind::Todo(..)
                if level > 0 =>
            {
                // Nested item: backspace un-nests it.
                Some(Binding::Custom(Message::ChangeLevel { index, delta: -1 }))
            }
            BlockKind::Quote(_)
            | BlockKind::Bullet(_)
            | BlockKind::Numbered(..)
            | BlockKind::Todo(..) => {
                // Top-level item: backspace removes the marker.
                let text = block.content.text();
                Some(Binding::Custom(Message::ConvertBlock {
                    index,
                    kind: BlockKind::Paragraph,
                    content: text,
                }))
            }
            _ if index > 0 => {
                // At the very start: merge into the previous block.
                Some(Binding::Custom(Message::MergeUp { index }))
            }
            _ => Binding::from_key_press(key_press),
        }
    }

    /// The binding for the Enter key in a single-line block: the text after
    /// the caret moves into a new block below, which continues list items and
    /// quotes of the same kind. Entering an empty list item exits the list.
    fn enter_binding(index: usize, block: &EditableBlock) -> text_editor::Binding<Message> {
        use cosmic::widget::text_editor::Binding;
        let cursor = block.content.cursor();
        let text = block.content.text();
        // Typing a divider marker (`---`, `***`, `___`) and pressing Enter
        // inserts a divider and moves the caret below it.
        if matches!(block.kind, BlockKind::Paragraph) && editor::is_divider(&text) {
            return Binding::Custom(Message::InsertDivider(index));
        }
        let (before, after) = split_at(&text, cursor.position.column);
        let continuation = match &block.kind {
            BlockKind::Bullet(level) => Some(BlockKind::Bullet(*level)),
            BlockKind::Numbered(level, number) => Some(BlockKind::Numbered(*level, *number)),
            BlockKind::Todo(level, done) => Some(BlockKind::Todo(*level, *done)),
            BlockKind::Quote(level) => Some(BlockKind::Quote(*level)),
            BlockKind::Paragraph
            | BlockKind::Heading(_)
            | BlockKind::Code(_)
            | BlockKind::Divider => None,
        };
        if let Some(kind) = continuation {
            if before.is_empty() && after.is_empty() {
                // An empty item: Enter exits the list.
                return Binding::Custom(Message::ConvertBlock {
                    index,
                    kind: BlockKind::Paragraph,
                    content: String::new(),
                });
            }
            return Binding::Custom(Message::SplitBlock {
                index,
                kind,
                before,
                after,
            });
        }
        // Heading or paragraph: split into a paragraph.
        Binding::Custom(Message::SplitBlock {
            index,
            kind: BlockKind::Paragraph,
            before,
            after,
        })
    }

    /// The cursor positioned at the end of the block's content.
    fn end_cursor(content: &text_editor::Content) -> text_editor::Cursor {
        let line = content.line_count().saturating_sub(1);
        let column = content.line(line).map_or(0, |l| l.text.len());
        text_editor::Cursor {
            position: text_editor::Position { line, column },
            selection: None,
        }
    }

    /// The nearest editable block before `index`, skipping dividers.
    fn previous_editable(blocks: &[EditableBlock], index: usize) -> Option<usize> {
        (0..index)
            .rev()
            .find(|&i| !matches!(blocks[i].kind, BlockKind::Divider))
    }

    /// The nearest editable block after `index`, skipping dividers.
    fn next_editable(blocks: &[EditableBlock], index: usize) -> Option<usize> {
        (index + 1..blocks.len()).find(|&i| !matches!(blocks[i].kind, BlockKind::Divider))
    }

    /// A task that focuses the block editor with the given index.
    fn focus_block(index: usize) -> cosmic::Task<Message> {
        cosmic::iced::runtime::task::widget(
            cosmic::iced::runtime::core::widget::operation::focusable::focus(
                cosmic::widget::Id::new(format!("block-{index}")),
            ),
        )
    }

    /// A snapshot of the whole document for undo/redo, rebuilt from the
    /// editable blocks as pure model blocks.
    fn snapshot_blocks(&self) -> Vec<editor::Block> {
        self.blocks.iter().map(EditableBlock::to_block).collect()
    }

    /// Splits a multi-line paste into blocks (Futo behavior): the text
    /// before the caret stays in the block, each pasted line becomes its own
    /// block, and the text after the caret continues in a new block. Returns
    /// a focus task when the paste was handled here, skipping the editor's
    /// default behavior. Code blocks and single-line pastes keep the default.
    fn intercept_paste(
        &mut self,
        index: usize,
        action: &text_editor::Action,
    ) -> Option<Task<cosmic::Action<Message>>> {
        let text_editor::Action::Edit(text_editor::Edit::Paste(text)) = action else {
            return None;
        };
        if !text.contains('\n') {
            return None;
        }
        let block = self.blocks.get(index)?;
        if matches!(block.kind, BlockKind::Code(_) | BlockKind::Divider)
            || block.content.text().contains('\n')
        {
            return None;
        }
        let cursor = block.content.cursor();
        // An inline selection (or caret) within the single line.
        let (start, end) = match cursor.selection {
            Some(anchor) if anchor.line == cursor.position.line => (
                anchor.column.min(cursor.position.column),
                anchor.column.max(cursor.position.column),
            ),
            _ => (cursor.position.column, cursor.position.column),
        };
        let pasted = editor::parse_markdown(text);
        if pasted.is_empty() {
            return None;
        }
        let line = block.content.text();
        let kind = block.kind.clone();
        let (replacement, caret_in_replacement) = paste_blocks(kind, &line, start, end, pasted);
        let caret = index + caret_in_replacement;
        self.push_undo_step();
        self.blocks.splice(index..=index, replacement);
        renumber_blocks(&mut self.blocks);
        self.selected_block = caret;
        if let Some(block) = self.blocks.get_mut(caret) {
            let cursor = Self::end_cursor(&block.content);
            block.content.move_to(cursor);
        }
        self.mark_dirty();
        Some(Self::focus_block(caret).map(cosmic::action::app))
    }

    /// Whether a new edit should start an undo step, given when the previous
    /// one was recorded. Typing bursts in the same block collapse into one
    /// step; anything else (time passing, another block) starts a new one.
    fn should_snapshot(last: Option<(Instant, usize)>, now: Instant, index: usize) -> bool {
        last.is_none_or(|(at, last_index)| {
            now.duration_since(at) >= UNDO_COALESCE || last_index != index
        })
    }

    /// Pushes a snapshot of the current document and clears the redo stack.
    fn record_snapshot(&mut self) {
        self.undo_stack.push(self.snapshot_blocks());
        if self.undo_stack.len() > UNDO_LIMIT {
            self.undo_stack.remove(0);
        }
        self.redo_stack.clear();
    }

    /// Records the current document as an undo step before an edit in the
    /// given block, coalescing bursts of typing into a single step.
    fn push_undo(&mut self, block_index: usize) {
        let now = Instant::now();
        if !Self::should_snapshot(self.last_undo_at, now, block_index) {
            return;
        }
        self.record_snapshot();
        self.last_undo_at = Some((now, block_index));
    }

    /// Records the current document as its own undo step for a structural
    /// change (split, convert, ...) and breaks the typing coalescing window
    /// so the following keystrokes start a fresh step.
    fn push_undo_step(&mut self) {
        self.record_snapshot();
        self.last_undo_at = None;
    }

    /// Replaces the document with the given snapshot, places the caret at
    /// the end of `selected_block` (clamped to the restored document), and
    /// returns a task that refocuses the editor and refreshes the title.
    fn restore_blocks(&mut self, snapshot: Vec<editor::Block>) -> Task<cosmic::Action<Message>> {
        self.blocks = snapshot
            .into_iter()
            .map(EditableBlock::from_block)
            .collect();
        self.last_undo_at = None;
        let index = self.selected_block.min(self.blocks.len().saturating_sub(1));
        self.selected_block = index;
        if let Some(block) = self.blocks.get_mut(index) {
            let cursor = Self::end_cursor(&block.content);
            block.content.move_to(cursor);
        }
        self.mark_dirty();
        Task::batch([
            Self::focus_block(index).map(cosmic::action::app),
            self.update_title(),
        ])
    }

    /// Converts the block at `index` to `kind`, keeping its text (dividers
    /// drop it), and returns a task that moves focus back into the document:
    /// the converted block, or the next editable block after a divider
    /// (inserting an empty paragraph when there is nowhere to type).
    fn format_block(&mut self, index: usize, kind: BlockKind) -> Task<cosmic::Action<Message>> {
        if self
            .blocks
            .get(index)
            .is_none_or(|block| block.kind == kind)
        {
            return Task::none();
        }
        self.push_undo_step();
        let Some(block) = self.blocks.get_mut(index) else {
            return Task::none();
        };
        let is_divider = matches!(kind, BlockKind::Divider);
        let text = block.content.text();
        block.kind = kind;
        block.content = text_editor::Content::with_text(if is_divider { "" } else { &text });
        renumber_blocks(&mut self.blocks);
        self.mark_dirty();
        if !is_divider {
            return Self::focus_block(index).map(cosmic::action::app);
        }
        if let Some(next) = Self::next_editable(&self.blocks, index) {
            return Self::focus_block(next).map(cosmic::action::app);
        }
        let next = index + 1;
        self.blocks.insert(
            next,
            EditableBlock::from_block(editor::Block::paragraph("")),
        );
        Self::focus_block(next).map(cosmic::action::app)
    }

    /// The actions shown next to the title: trash, or restore and delete.
    fn note_actions(note: &Note) -> Element<'_, Message> {
        if note.in_trash() {
            let restore = Self::tooltip(
                button::icon(icon::from_name("edit-undo-symbolic"))
                    .on_press(Message::RestoreNote(note.path().to_path_buf()))
                    .class(cosmic::theme::Button::Icon)
                    .into(),
                fl!("restore"),
            );
            let delete = Self::tooltip(
                button::icon(icon::from_name("edit-delete-symbolic"))
                    .on_press(Message::DeleteNote(note.path().to_path_buf()))
                    .class(cosmic::theme::Button::Icon)
                    .into(),
                fl!("delete"),
            );
            widget::row::with_capacity(2)
                .push(restore)
                .push(delete)
                .spacing(cosmic::theme::spacing().space_xxs)
                .into()
        } else {
            Self::tooltip(
                button::icon(icon::from_name("user-trash-symbolic"))
                    .on_press(Message::TrashNote)
                    .class(cosmic::theme::Button::Icon)
                    .into(),
                fl!("move-to-trash"),
            )
        }
    }

    /// Wraps content in a tooltip with the given text.
    fn tooltip(content: Element<'static, Message>, text: String) -> Element<'static, Message> {
        tooltip::tooltip(content, widget::text(text), tooltip::Position::FollowCursor).into()
    }

    /// The empty state shown when no note is selected.
    fn editor_empty_state() -> Element<'static, Message> {
        let spacing = cosmic::theme::spacing();

        widget::container(
            widget::column::with_capacity(2)
                .push(icon::from_name("document-edit-symbolic").size(48))
                .push(widget::text::body(fl!("no-note-selected")))
                .spacing(spacing.space_s)
                .align_x(Alignment::Center),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Horizontal::Center)
        .align_y(Vertical::Center)
        .into()
    }

    /// Builds the markdown preview settings from the active theme.
    fn markdown_settings() -> markdown::Settings {
        let theme = cosmic::theme::active();
        let cosmic = theme.cosmic();
        let palette = cosmic::iced::theme::Palette {
            background: cosmic.bg_color().into(),
            text: cosmic.on_bg_color().into(),
            primary: cosmic.accent.base.into(),
            success: cosmic.success.base.into(),
            danger: cosmic.destructive.base.into(),
            warning: cosmic.warning.base.into(),
        };
        markdown::Settings::with_text_size(16, markdown::Style::from_palette(palette))
    }

    /// Opens the vault from the config, falling back to the default location.
    fn open_vault(config: &Config) -> Vault {
        let default = default_vault_path();
        let path = config
            .vault_path()
            .map(PathBuf::from)
            .unwrap_or(default.clone());
        match Vault::open(&path) {
            Ok(vault) => vault,
            Err(err) => {
                eprintln!(
                    "failed to open vault at {}: {err}; using default",
                    path.display()
                );
                Vault::open(default).unwrap_or_else(|err| {
                    eprintln!("failed to open default vault: {err}");
                    // An in-memory-only vault keeps the app usable if the disk is broken.
                    Vault::open(temp_vault_path()).expect("failed to create fallback vault")
                })
            }
        }
    }

    /// Updates the header and window titles.
    pub fn update_title(&mut self) -> Task<cosmic::Action<Message>> {
        let mut window_title = fl!("app-title");

        if let Some(note) = self.selected_note() {
            window_title.push_str(" — ");
            window_title.push_str(note.title());
        }

        if self.dirty {
            window_title = format!("● {window_title}");
        }

        if let Some(id) = self.core.main_window_id() {
            self.set_window_title(window_title, id)
        } else {
            Task::none()
        }
    }
}

/// The default vault directory: `~/Documents/cosmic-notes`.
#[must_use]
fn default_vault_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Documents")
        .join("cosmic-notes")
}

/// A fallback vault under the system temp directory.
fn temp_vault_path() -> PathBuf {
    std::env::temp_dir().join("cosmic-notes-vault")
}

/// A human-readable relative date for the note list.
#[must_use]
fn relative_time(modified: Option<SystemTime>) -> String {
    let Some(modified) = modified else {
        return String::new();
    };
    let Ok(duration) = SystemTime::now().duration_since(modified) else {
        return String::new();
    };
    let days = duration.as_secs() / 86_400;
    match days {
        0 => fl!("today"),
        1 => fl!("yesterday"),
        2..=6 => fl!("days-ago", num = days),
        _ => {
            let secs = modified
                .duration_since(UNIX_EPOCH)
                .map_or(0, |duration| duration.as_secs());
            let (year, month, day) = notes::unix_secs_to_date(secs);
            format!("{year}-{month:02}-{day:02}")
        }
    }
}

/// The page to display in the application.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Page {
    All,
    Pinned,
    Trash,
}

/// The context page to display in the context drawer.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum ContextPage {
    #[default]
    About,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MenuAction {
    NewNote,
    Save,
    About,
}

impl menu::action::MenuAction for MenuAction {
    type Message = Message;

    fn message(&self) -> Self::Message {
        match self {
            MenuAction::NewNote => Message::NewNote,
            MenuAction::Save => Message::SaveNow,
            MenuAction::About => Message::ToggleContextPage(ContextPage::About),
        }
    }
}

/// The kind a block continues as when split by a paste: structured kinds
/// keep their marker, plain kinds fall back to a paragraph.
#[must_use]
fn continuation_kind(kind: &BlockKind) -> BlockKind {
    match kind {
        BlockKind::Bullet(level) => BlockKind::Bullet(*level),
        BlockKind::Numbered(level, number) => BlockKind::Numbered(*level, *number),
        BlockKind::Todo(level, done) => BlockKind::Todo(*level, *done),
        BlockKind::Quote(level) => BlockKind::Quote(*level),
        BlockKind::Paragraph | BlockKind::Heading(_) | BlockKind::Code(_) | BlockKind::Divider => {
            BlockKind::Paragraph
        }
    }
}

/// The blocks that replace a single-line block when `pasted` (already
/// parsed) is inserted over the byte range `start..end` of `line`, plus the
/// index within the replacement of the block that receives the caret.
///
/// The text before the range stays in the original block (keeping its
/// kind), the pasted blocks follow it, and the text after the range becomes
/// a continuation of the original kind. Pasting into an entirely empty
/// block replaces it with the pasted blocks.
#[must_use]
fn paste_blocks(
    kind: BlockKind,
    line: &str,
    start: usize,
    end: usize,
    pasted: Vec<editor::Block>,
) -> (Vec<EditableBlock>, usize) {
    let before = line[..start].to_owned();
    let after = line[end..].to_owned();
    if before.is_empty() && after.is_empty() {
        let blocks: Vec<EditableBlock> =
            pasted.into_iter().map(EditableBlock::from_block).collect();
        let caret = blocks.len().saturating_sub(1);
        return (blocks, caret);
    }
    let mut blocks = Vec::with_capacity(2 + pasted.len());
    let continuation = continuation_kind(&kind);
    blocks.push(EditableBlock {
        kind,
        content: text_editor::Content::with_text(&before),
    });
    // The caret lands at the end of the last pasted block, just before the
    // continuation of the text that followed the caret.
    let caret = blocks.len() + pasted.len() - 1;
    blocks.extend(pasted.into_iter().map(EditableBlock::from_block));
    blocks.push(EditableBlock {
        kind: continuation,
        content: text_editor::Content::with_text(&after),
    });
    (blocks, caret)
}

/// Re-numbers every consecutive run of ordered list items so each run
/// starts at 1 and increments by one, matching how Futo displays lists.
/// Runs are reset by any non-numbered block or a level change.
fn renumber_blocks(blocks: &mut [EditableBlock]) {
    let mut previous: Option<(u8, u32)> = None;
    for block in blocks {
        match &mut block.kind {
            BlockKind::Numbered(level, number) => {
                *number = match previous {
                    Some((prev_level, prev_number)) if prev_level == *level => prev_number + 1,
                    _ => 1,
                };
                previous = Some((*level, *number));
            }
            _ => previous = None,
        }
    }
}

/// Splits the text at the given byte offset, clamped down to a character
/// boundary so multi-byte characters are never cut in half.
#[must_use]
fn split_at(text: &str, column: usize) -> (String, String) {
    let boundary = text.floor_char_boundary(column);
    (text[..boundary].to_owned(), text[boundary..].to_owned())
}

#[cfg(test)]
mod tests {
    use super::{AppModel, EditableBlock, UNDO_COALESCE, paste_blocks, renumber_blocks, split_at};
    use crate::editor::{self, BlockKind};
    use std::time::Instant;

    #[test]
    fn split_at_clamps_to_char_boundaries() {
        assert_eq!(split_at("hello", 3), ("hel".to_owned(), "lo".to_owned()));
        // The column is a byte offset: splitting inside a multi-byte char
        // clamps down to the start of the char.
        assert_eq!(split_at("héllo", 1), ("h".to_owned(), "éllo".to_owned()));
        assert_eq!(split_at("héllo", 3), ("hé".to_owned(), "llo".to_owned()));
        assert_eq!(split_at("hello", 99), ("hello".to_owned(), String::new()));
        assert_eq!(split_at("", 0), (String::new(), String::new()));
    }

    #[test]
    fn shift_level_clamps_nesting() {
        assert_eq!(
            AppModel::shift_level(&BlockKind::Bullet(1), 1),
            Some(BlockKind::Bullet(2))
        );
        assert_eq!(
            AppModel::shift_level(&BlockKind::Bullet(0), -1),
            Some(BlockKind::Bullet(0))
        );
        assert_eq!(
            AppModel::shift_level(&BlockKind::Bullet(5), 5),
            Some(BlockKind::Bullet(6))
        );
        assert_eq!(
            AppModel::shift_level(&BlockKind::Todo(2, true), 1),
            Some(BlockKind::Todo(3, true))
        );
        assert_eq!(
            AppModel::shift_level(&BlockKind::Numbered(1, 4), -1),
            Some(BlockKind::Numbered(0, 4))
        );
        assert_eq!(
            AppModel::shift_level(&BlockKind::Quote(0), 1),
            Some(BlockKind::Quote(1))
        );
        assert_eq!(AppModel::shift_level(&BlockKind::Paragraph, 1), None);
        assert_eq!(AppModel::shift_level(&BlockKind::Heading(2), 1), None);
        assert_eq!(AppModel::shift_level(&BlockKind::Divider, 1), None);
    }

    #[test]
    fn undo_coalesces_typing_in_one_block() {
        let now = Instant::now();
        // No previous snapshot: start a step.
        assert!(AppModel::should_snapshot(None, now, 0));
        // A recent snapshot in the same block coalesces.
        assert!(!AppModel::should_snapshot(Some((now, 0)), now, 0));
        // A recent snapshot in another block starts a new step.
        assert!(AppModel::should_snapshot(Some((now, 0)), now, 1));
        // Enough time passing starts a new step even in the same block.
        assert!(AppModel::should_snapshot(
            Some((now, 0)),
            now + UNDO_COALESCE,
            0
        ));
    }

    #[test]
    fn paste_blocks_splits_multi_line_paste() {
        let pasted = vec![editor::Block::paragraph("X"), editor::Block::paragraph("Y")];
        let (blocks, caret) = paste_blocks(BlockKind::Bullet(0), "ab", 1, 1, pasted);
        let kinds: Vec<_> = blocks.iter().map(|b| b.kind.clone()).collect();
        let text: Vec<_> = blocks.iter().map(|b| b.content.text()).collect();
        assert_eq!(
            kinds,
            vec![
                BlockKind::Bullet(0),
                BlockKind::Paragraph,
                BlockKind::Paragraph,
                BlockKind::Bullet(0)
            ]
        );
        assert_eq!(text, vec!["a", "X", "Y", "b"]);
        // The caret lands at the end of the last pasted block.
        assert_eq!(caret, 2);
    }

    #[test]
    fn paste_blocks_replaces_empty_block_and_selection() {
        // Pasting into an empty block replaces it with the pasted blocks.
        let pasted = vec![editor::Block::paragraph("hello")];
        let (blocks, caret) = paste_blocks(BlockKind::Todo(0, false), "", 0, 0, pasted);
        assert_eq!(blocks[0].kind, BlockKind::Paragraph);
        assert_eq!(caret, 0);
        // Pasting over a selection drops the selected text.
        let pasted = vec![editor::Block::paragraph("Z")];
        let (blocks, caret) = paste_blocks(BlockKind::Bullet(1), "abcdef", 1, 4, pasted);
        let text: Vec<_> = blocks.iter().map(|b| b.content.text()).collect();
        assert_eq!(text, vec!["a", "Z", "ef"]);
        assert_eq!(caret, 1);
    }

    #[test]
    fn renumber_blocks_sequences_each_list_run() {
        let mut blocks = vec![
            EditableBlock::from_block(editor::Block::paragraph("intro")),
            EditableBlock::from_block(editor::Block {
                kind: BlockKind::Numbered(0, 7),
                content: "a".to_owned(),
            }),
            EditableBlock::from_block(editor::Block {
                kind: BlockKind::Numbered(0, 9),
                content: "b".to_owned(),
            }),
            EditableBlock::from_block(editor::Block::paragraph("gap")),
            EditableBlock::from_block(editor::Block {
                kind: BlockKind::Numbered(1, 3),
                content: "c".to_owned(),
            }),
            EditableBlock::from_block(editor::Block {
                kind: BlockKind::Numbered(1, 5),
                content: "d".to_owned(),
            }),
            EditableBlock::from_block(editor::Block {
                kind: BlockKind::Numbered(0, 99),
                content: "e".to_owned(),
            }),
        ];
        renumber_blocks(&mut blocks);
        let numbers: Vec<_> = blocks
            .iter()
            .map(|block| match &block.kind {
                BlockKind::Numbered(_, number) => *number,
                _ => 0,
            })
            .collect();
        // Each run starts at 1; a level change resets the run.
        assert_eq!(numbers, vec![0, 1, 2, 0, 1, 2, 1]);
    }
}
