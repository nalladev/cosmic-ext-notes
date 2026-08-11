// SPDX-License-Identifier: MPL-2.0

use crate::config::Config;
use crate::editor::{self, BlockKind};
use crate::fl;
use crate::notes::{Note, SaveOutcome, Tree, TreeNode, Vault};
use crate::sidebar_area::SidebarMouseArea;
use cosmic::app::context_drawer;
use cosmic::cosmic_config::{self, CosmicConfigEntry};
use cosmic::iced::alignment::{Horizontal, Vertical};
use cosmic::iced::futures::SinkExt;
use cosmic::iced::keyboard::Key;
use cosmic::iced::{Alignment, Length, Point, Subscription, event, keyboard};
use cosmic::prelude::*;
use cosmic::widget::menu::{ItemHeight, ItemWidth};
use cosmic::widget::{
    self, Toasts, about::About, button, divider, icon, menu, nav_bar, popover, responsive_menu_bar,
    segmented_button, text_editor, text_input, toaster, tooltip,
};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");
const APP_ICON: &[u8] = include_bytes!("../resources/icon.svg");

/// The id of the responsive menu bar, used to track its collapsed size.
static MENU_ID: std::sync::LazyLock<cosmic::widget::Id> =
    std::sync::LazyLock::new(|| cosmic::widget::Id::new("responsive-menu"));

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
/// Widget id of the note title input.
const TITLE_ID: &str = "note-title";
/// Widget id of the folder name input in the sidebar dialog.
const DIALOG_NAME_ID: &str = "sidebar-folder-name";

/// The application model stores app-specific state used to describe its interface and
/// drive its logic.
pub struct AppModel {
    /// Application state which is managed by the COSMIC runtime.
    core: cosmic::Core,
    /// Display a context drawer with the designated page if defined.
    context_page: ContextPage,
    /// The about page for this app.
    about: About,
    /// Enables the standard COSMIC navigation sidebar and its header toggle.
    nav_model: nav_bar::Model,
    /// Directories currently expanded in the sidebar tree.
    expanded: HashSet<PathBuf>,
    /// Key bindings for the application's menu bar.
    #[allow(clippy::zero_sized_map_values)]
    key_binds: HashMap<menu::KeyBind, MenuAction>,
    /// Configuration data that persists between application runs.
    config: Config,
    /// The note store.
    vault: Vault,
    /// The open editor tabs, keyed by the segmented button model: each tab
    /// carries its note path and editor state.
    tab_model: segmented_button::SingleSelectModel,
    /// The config handler used to persist settings when they change.
    config_handler: Option<cosmic_config::Config>,
    /// Toast notifications.
    toasts: Toasts<Message>,
    /// The open sidebar folder dialog (create or rename), if any.
    sidebar_dialog: Option<SidebarDialog>,
    /// Where the sidebar's empty-area context menu is open, if any.
    sidebar_context_menu_point: Option<Point>,
}

/// Messages emitted by the application and its widgets.
#[derive(Debug, Clone)]
pub enum Message {
    /// A surface action from the responsive menu bar.
    Surface(cosmic::surface::Action),
    LaunchUrl(String),
    ToggleContextPage(ContextPage),
    UpdateConfig(Config),
    ToastClosed(toaster::ToastId),
    /// A keyboard shortcut was pressed.
    KeyboardShortcut(Shortcut),
    /// Open the note with the given path.
    SelectNote(PathBuf),
    /// Create a new note.
    NewNote,
    /// The tab with the given id was activated in the tab bar.
    TabActivate(segmented_button::Entity),
    /// The tab with the given id should be closed.
    TabClose(segmented_button::Entity),
    /// Create a new tab.
    TabNew,
    /// The title input changed.
    EditTitle(String),
    /// Commit the current title as a rename.
    RenameSubmit,
    /// An action from a sidebar context menu.
    NavMenuAction(NavMenuAction),
    /// The sidebar folder dialog's name input changed.
    SidebarDialogUpdate(String),
    /// The sidebar folder dialog's primary action was pressed.
    SidebarDialogComplete,
    /// The sidebar folder dialog was dismissed.
    SidebarDialogCancel,
    /// A right click on the sidebar's empty area toggled its context menu,
    /// at the cursor position relative to the sidebar.
    SidebarContextMenuPoint(Point),
    /// Dismiss the sidebar's empty-area context menu.
    SidebarContextMenuClose,
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
    /// Periodic autosave tick.
    AutosaveTick,
    /// Save the active note now.
    SaveNow,
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

/// A block rendered in the editor: the block kind plus the editable text
/// buffer, which keeps the cursor and selection state between frames.
struct EditableBlock {
    /// What kind of block this is.
    kind: BlockKind,
    /// The editable text buffer for the block content.
    content: text_editor::Content,
}

/// An item displayed by COSMIC's navigation sidebar.
#[derive(Clone)]
enum NavEntry {
    Folder(PathBuf),
    Note(PathBuf),
}

/// The open sidebar dialog: creates or renames a folder.
struct SidebarDialog {
    /// What the dialog does and where it operates.
    kind: SidebarDialogKind,
    /// The current folder name in the input.
    name: String,
}

/// The operation a [`SidebarDialog`] performs on completion.
enum SidebarDialogKind {
    /// Create a new folder inside `parent`.
    NewFolder { parent: PathBuf },
    /// Rename the folder at `path`.
    RenameFolder { path: PathBuf },
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

/// A tab in the editor: the open note plus its editor state. A tab with no
/// note path is a fresh "+" tab: the editor pane shows the empty state until
/// a note is picked from the sidebar, which loads into it.
struct Tab {
    /// The path of the note this tab edits, if any.
    path: Option<PathBuf>,
    /// The blocks of the note, each with an editable text buffer.
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
    /// The title being edited for this tab.
    title_draft: String,
    /// Whether this tab has unsaved changes.
    dirty: bool,
}

impl Tab {
    /// Builds an editor tab for the note at `path`, loaded from `body` and
    /// `title`. An empty note still gets one empty block so the editor has a
    /// focusable surface that shows the placeholder.
    fn new(path: PathBuf, body: &str, title: &str) -> Self {
        let mut blocks: Vec<EditableBlock> = editor::parse_markdown(body)
            .into_iter()
            .map(EditableBlock::from_block)
            .collect();
        if blocks.is_empty() {
            blocks.push(EditableBlock::from_block(editor::Block::paragraph("")));
        }
        Self {
            path: Some(path),
            blocks,
            selected_block: 0,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            last_undo_at: None,
            slash_menu: None,
            slash_selection: 0,
            title_draft: title.to_owned(),
            dirty: false,
        }
    }

    /// Loads the given body and title into this tab, resetting its editor
    /// state (undo history, slash menu, dirty flag).
    fn load_body(&mut self, body: &str, title: &str) {
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
        self.dirty = false;
    }

    /// A fresh, empty tab with no note behind it yet. The editor pane shows
    /// the empty state until a note is picked from the sidebar.
    fn blank() -> Self {
        Self {
            path: None,
            blocks: Vec::new(),
            selected_block: 0,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            last_undo_at: None,
            slash_menu: None,
            slash_selection: 0,
            title_draft: String::new(),
            dirty: false,
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

    /// Supplies the standard COSMIC sidebar affordance in the header bar.
    fn nav_model(&self) -> Option<&nav_bar::Model> {
        Some(&self.nav_model)
    }

    /// Handles COSMIC navigation sidebar selections.
    fn on_nav_select(&mut self, id: nav_bar::Id) -> Task<cosmic::Action<Self::Message>> {
        match self.nav_model.data::<NavEntry>(id).cloned() {
            Some(NavEntry::Folder(path)) => {
                if !self.expanded.insert(path.clone()) {
                    self.expanded.remove(&path);
                }
                self.rebuild_nav_model();
                self.core.nav_bar_set_toggled(true);
                Task::none()
            }
            Some(NavEntry::Note(path)) => self.open_tab(&path, false),
            None => Task::none(),
        }
    }

    /// Handles right clicks on a sidebar item. The item's context menu is
    /// rendered by the nav bar itself; the model is left untouched so the
    /// popup stays open (a rebuild would invalidate the item's id).
    fn on_nav_context(&mut self, _id: nav_bar::Id) -> Task<cosmic::Action<Self::Message>> {
        // An item's own menu takes precedence: dismiss the empty-area menu.
        self.sidebar_context_menu_point = None;
        Task::none()
    }

    /// Supplies the navigation sidebar, wrapped so a right click on its
    /// empty area opens a folder/note creation menu.
    fn nav_bar(&self) -> Option<Element<'_, cosmic::Action<Self::Message>>> {
        if !self.core().nav_bar_active() {
            return None;
        }

        let nav_model = self.nav_model()?;

        let mut nav = cosmic::widget::nav_bar(nav_model, |id| {
            cosmic::Action::Cosmic(cosmic::app::Action::NavBar(id))
        })
        .on_context(|id| cosmic::Action::Cosmic(cosmic::app::Action::NavBarContext(id)))
        .context_menu(self.nav_context_menu());

        // Context menus are rendered in separate popup surfaces on Wayland, the
        // same as every other COSMIC app (files, store, settings, ...). Without
        // this, libcosmic falls back to an in-window overlay menu whose bounds
        // keep the base widget from receiving the cursor, so right-clicking
        // another item would close the open menu without opening the new one.
        #[cfg(feature = "wayland")]
        {
            nav = nav
                .window_id_maybe(self.core().main_window_id())
                .on_surface_action(|m| cosmic::Action::Cosmic(cosmic::app::Action::Surface(m)));
        }

        let mut nav = nav
            .into_container()
            .width(Length::Shrink)
            .height(Length::Fill);

        if !self.core().is_condensed() {
            nav = nav.max_width(280);
        }

        // A right click on the empty area of the sidebar opens the creation
        // menu at the cursor: the same `mouse_area` + `popover` pattern
        // cosmic-files uses for its file view, with the click position
        // reported by the release itself so no per-move tracking is needed.
        // Right clicks on sidebar items open the items' own menus instead:
        // the nav bar handles the release first and captures it, and
        // [`Self::on_nav_context`] dismisses this menu.
        let mouse_area = SidebarMouseArea::new(nav)
            .on_press(cosmic::Action::App(Message::SidebarContextMenuClose))
            .on_right_release(|point| cosmic::Action::App(Message::SidebarContextMenuPoint(point)));

        let mut popover = popover::popover(mouse_area)
            .on_close(cosmic::Action::App(Message::SidebarContextMenuClose));
        if let Some(point) = self.sidebar_context_menu_point {
            popover = popover
                .popup(Self::sidebar_context_menu())
                .position(popover::Position::Point(point));
        }

        Some(popover.into())
    }

    /// The context menu entries for each sidebar item: folders get
    /// new-folder-within, rename and delete; notes get delete. The order must
    /// match the navigation model's items.
    fn nav_context_menu(&self) -> Option<Vec<menu::Tree<cosmic::Action<Self::Message>>>> {
        let children = self
            .nav_model
            .iter()
            .map(|entity| {
                let mut items: Vec<menu::Item<NavMenuAction, String>> = Vec::with_capacity(4);
                match self.nav_model.data::<NavEntry>(entity).cloned() {
                    Some(NavEntry::Folder(_)) => {
                        items.push(menu::Item::Button(
                            fl!("new-note"),
                            None,
                            NavMenuAction::NewNoteIn(entity),
                        ));
                        items.push(menu::Item::Button(
                            fl!("new-folder"),
                            None,
                            NavMenuAction::NewFolderIn(entity),
                        ));
                        items.push(menu::Item::Button(
                            fl!("rename-folder"),
                            None,
                            NavMenuAction::RenameFolder(entity),
                        ));
                        items.push(menu::Item::Divider);
                        items.push(menu::Item::Button(
                            fl!("delete-folder"),
                            None,
                            NavMenuAction::DeleteItem(entity),
                        ));
                    }
                    Some(NavEntry::Note(_)) => {
                        items.push(menu::Item::Button(
                            fl!("open-in-new-tab"),
                            None,
                            NavMenuAction::OpenInNewTab(entity),
                        ));
                        items.push(menu::Item::Divider);
                        items.push(menu::Item::Button(
                            fl!("delete"),
                            None,
                            NavMenuAction::DeleteItem(entity),
                        ));
                    }
                    None => {}
                }
                items
            })
            .collect();
        Some(menu::nav_context(&HashMap::new(), children))
    }

    /// Displays the sidebar folder dialog (create or rename) when open.
    fn dialog(&self) -> Option<Element<'_, Self::Message>> {
        let dialog_state = self.sidebar_dialog.as_ref()?;
        let spacing = cosmic::theme::spacing();
        let title = match &dialog_state.kind {
            SidebarDialogKind::NewFolder { .. } => fl!("new-folder"),
            SidebarDialogKind::RenameFolder { .. } => fl!("rename-folder"),
        };
        let mut dialog = widget::dialog().title(title);

        let name = &dialog_state.name;
        let complete_maybe = if name.is_empty() {
            None
        } else if name == "." || name == ".." {
            dialog = dialog.tertiary_action(widget::text::body(fl!(
                "name-invalid",
                name = name.as_str()
            )));
            None
        } else if name.contains('/') {
            dialog = dialog.tertiary_action(widget::text::body(fl!("name-no-slashes")));
            None
        } else if Self::dialog_target_exists(&dialog_state.kind, name) {
            dialog = dialog.tertiary_action(widget::text::body(fl!("folder-already-exists")));
            None
        } else {
            if name.starts_with('.') {
                dialog = dialog.tertiary_action(widget::text::body(fl!("name-hidden")));
            }
            Some(Message::SidebarDialogComplete)
        };

        Some(
            dialog
                .primary_action(
                    widget::button::suggested(fl!("save")).on_press_maybe(complete_maybe.clone()),
                )
                .secondary_action(
                    widget::button::standard(fl!("cancel")).on_press(Message::SidebarDialogCancel),
                )
                .control(
                    widget::column::with_children([
                        widget::text::body(fl!("folder-name")).into(),
                        widget::text_input("", name)
                            .id(cosmic::widget::Id::new(DIALOG_NAME_ID))
                            .on_input(Message::SidebarDialogUpdate)
                            .on_submit_maybe(complete_maybe.map(|maybe| move |_| maybe.clone()))
                            .into(),
                    ])
                    .spacing(spacing.space_xxs),
                )
                .into(),
        )
    }

    /// Initializes the application with any given flags and startup commands.
    fn init(
        core: cosmic::Core,
        _flags: Self::Flags,
    ) -> (Self, Task<cosmic::Action<Self::Message>>) {
        // Create the about widget
        let about = About::default()
            .name(fl!("app-title"))
            .icon(widget::icon::from_svg_bytes(APP_ICON))
            .version(env!("CARGO_PKG_VERSION"))
            .links([(fl!("repository"), REPOSITORY)])
            .license(env!("CARGO_PKG_LICENSE"));

        let config_handler = cosmic_config::Config::new(Self::APP_ID, Config::VERSION).ok();
        let config = config_handler
            .as_ref()
            .map(|context| match Config::get_entry(context) {
                Ok(config) => config,
                Err((_errors, config)) => config,
            })
            .unwrap_or_default();

        let vault = Self::open_vault(&config);

        let mut app = AppModel {
            core,
            context_page: ContextPage::default(),
            about,
            nav_model: nav_bar::Model::default(),
            expanded: HashSet::new(),
            key_binds: Shortcut::key_binds(),
            config,
            config_handler,
            vault,
            tab_model: segmented_button::SingleSelectModel::default(),
            toasts: Toasts::new(Message::ToastClosed),
            sidebar_dialog: None,
            sidebar_context_menu_point: None,
        };
        app.rebuild_nav_model();

        // Restore the note that was open when the app last closed as a tab.
        if let Some(relative) = app.config.last_note() {
            let path = app.vault.root().join(relative);
            let note = app
                .vault
                .notes()
                .iter()
                .find(|note| note.path() == path && !note.in_trash());
            if let Some(note) = note {
                let body = note.body().to_owned();
                let title = note.title().to_owned();
                app.new_tab(path, &body, &title);
                app.rebuild_nav_model();
            }
        }

        // Create a startup command that sets the window title.
        let command = app.update_title();

        (app, command)
    }

    /// Elements to pack at the start of the header bar.
    fn header_start(&self) -> Vec<Element<'_, Self::Message>> {
        vec![
            responsive_menu_bar()
                .item_height(ItemHeight::Dynamic(40))
                .item_width(ItemWidth::Uniform(360))
                .spacing(cosmic::theme::spacing().space_xxxs.into())
                .into_element(
                    &self.core,
                    &self.key_binds,
                    MENU_ID.clone(),
                    Message::Surface,
                    vec![(
                        fl!("file"),
                        vec![
                            menu::Item::Button(fl!("new-note"), None, MenuAction::NewNote),
                            menu::Item::Button(fl!("new-folder"), None, MenuAction::NewFolder),
                        ],
                    )],
                ),
        ]
    }

    /// Elements to pack at the end of the header bar.
    fn header_end(&self) -> Vec<Element<'_, Self::Message>> {
        let settings = button::icon(icon::from_name("emblem-system-symbolic"))
            .on_press(Message::ToggleContextPage(ContextPage::Settings))
            .class(cosmic::theme::Button::Icon);
        vec![Self::tooltip(settings.into(), fl!("settings"))]
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
            ContextPage::Settings => context_drawer::context_drawer(
                self.settings(),
                Message::ToggleContextPage(ContextPage::Settings),
            )
            .title(fl!("settings")),
        })
    }

    /// Describes the interface based on the current state of the application model.
    fn view(&self) -> Element<'_, Self::Message> {
        toaster(
            &self.toasts,
            widget::container(self.editor_pane())
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
            // Keyboard shortcuts not handled by the window's keyboard
            // navigation. Events that do not map to a shortcut return `None`
            // so no message is produced: the runtime synthesizes a
            // `RequestResize` platform event on every update pass, and turning
            // each one into a message would keep the app redrawing in an
            // endless loop. Only `Status::Ignored` events are considered, the
            // same filter `event::listen()` applied.
            event::listen_with(|event, status, _window| match event {
                event::Event::Keyboard(keyboard::Event::KeyPressed {
                    key,
                    modifiers,
                    ..
                }) => {
                    if status != event::Status::Ignored {
                        return None;
                    }
                    let primary = modifiers.logo() || modifiers.control();
                    if primary && key == keyboard::Key::Character("n".into()) {
                        Some(Message::KeyboardShortcut(Shortcut::NewNote))
                    } else if primary && key == keyboard::Key::Character("s".into()) {
                        Some(Message::KeyboardShortcut(Shortcut::Save))
                    } else {
                        None
                    }
                }
                _ => None,
            }),
        ];

        // Conditionally enables a timer that emits a message every second.
        if self.active_tab().is_some_and(|tab| tab.dirty) {
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
            Message::Surface(action) => {
                cosmic::task::message(cosmic::Action::Cosmic(cosmic::app::Action::Surface(action)))
            }

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

            Message::SelectNote(path) => self.open_tab(&path, false),

            Message::NewNote => {
                // A "New Note" chosen from the sidebar menu dismisses it.
                self.sidebar_context_menu_point = None;
                self.new_note()
            }

            Message::TabActivate(entity) => {
                // Switching tabs commits the current title and autosaves the
                // outgoing tab's changes, then activates the new one.
                self.commit_title_rename();
                let save = self.save_active(false);
                self.tab_model.activate(entity);
                let path = self.active_tab().and_then(|tab| tab.path.clone());
                if let Some(path) = &path {
                    self.remember_last_tab(path);
                }
                self.rebuild_nav_model();
                Task::batch([save, self.update_title()])
            }

            Message::TabClose(entity) => {
                self.close_tab(entity);
                self.update_title()
            }

            Message::TabNew => {
                // The "+" button opens a fresh, blank tab showing the empty
                // state; the note is created (or picked) afterwards. A tab
                // with no note open is labeled "Home".
                self.sidebar_context_menu_point = None;
                let save = self.save_active(false);
                self.tab_model
                    .insert()
                    .text(fl!("home"))
                    .data::<Tab>(Tab::blank())
                    .closable()
                    .activate();
                self.refresh_tab_closability();
                self.rebuild_nav_model();
                Task::batch([save, self.update_title()])
            }

            Message::EditTitle(title) => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.title_draft = title;
                }
                Task::none()
            }

            Message::RenameSubmit => {
                self.commit_title_rename();
                self.update_title()
            }

            Message::NavMenuAction(action) => {
                // Every sidebar menu action dismisses the empty-area menu.
                self.sidebar_context_menu_point = None;
                match action {
                    NavMenuAction::NewFolder => self.open_folder_dialog(None),
                    NavMenuAction::NewFolderIn(entity) => self.open_folder_dialog(Some(entity)),
                    NavMenuAction::NewNoteIn(entity) => self.new_note_in_folder(entity),
                    NavMenuAction::RenameFolder(entity) => self.open_rename_dialog(entity),
                    NavMenuAction::DeleteItem(entity) => self.delete_sidebar_item(entity),
                    NavMenuAction::OpenInNewTab(entity) => self.open_sidebar_item_new_tab(entity),
                }
            }

            Message::SidebarContextMenuPoint(point) => {
                // A right click toggles the empty-area menu like cosmic-files:
                // it opens at the cursor, or dismisses when already open.
                self.sidebar_context_menu_point = if self.sidebar_context_menu_point.is_some() {
                    None
                } else {
                    Some(point)
                };
                Task::none()
            }

            Message::SidebarContextMenuClose => {
                self.sidebar_context_menu_point = None;
                Task::none()
            }

            Message::SidebarDialogUpdate(name) => {
                if let Some(dialog_state) = &mut self.sidebar_dialog {
                    dialog_state.name = name;
                }
                Task::none()
            }

            Message::SidebarDialogComplete => {
                let Some(dialog_state) = self.sidebar_dialog.take() else {
                    return Task::none();
                };
                // The dialog only enables its primary action when valid, but
                // keep the same guard here as defense in depth.
                if !Self::dialog_is_valid(&dialog_state) {
                    return Task::none();
                }
                match dialog_state.kind {
                    SidebarDialogKind::NewFolder { parent } => match self
                        .vault
                        .create_folder(&parent, &dialog_state.name)
                    {
                        Ok(Some(_path)) => {
                            self.expanded.insert(parent);
                            self.rebuild_nav_model();
                            Task::batch([self.toast(fl!("folder-created")), self.update_title()])
                        }
                        Ok(None) => Task::none(),
                        Err(err) => {
                            eprintln!("failed to create folder: {err}");
                            Task::none()
                        }
                    },
                    SidebarDialogKind::RenameFolder { path } => match self
                        .vault
                        .rename_folder(&path, &dialog_state.name)
                    {
                        Ok(Some(new_path)) => {
                            self.remap_expanded(&path, &new_path);
                            // Keep the tabs under the renamed folder pointing
                            // at the notes they were already editing.
                            let entities: Vec<_> = self.tab_model.iter().collect();
                            for entity in entities {
                                if let Some(tab) = self.tab_model.data_mut::<Tab>(entity)
                                    && let Some(tab_path) = tab.path.as_ref()
                                    && tab_path.starts_with(&path)
                                {
                                    let relative = tab_path
                                        .strip_prefix(&path)
                                        .expect("path was checked with starts_with");
                                    tab.path = Some(new_path.join(relative));
                                }
                            }
                            self.rebuild_nav_model();
                            Task::batch([self.toast(fl!("folder-renamed")), self.update_title()])
                        }
                        Ok(None) => Task::none(),
                        Err(err) => {
                            eprintln!("failed to rename folder: {err}");
                            Task::none()
                        }
                    },
                }
            }

            Message::SidebarDialogCancel => {
                self.sidebar_dialog = None;
                Task::none()
            }

            Message::BlockAction { index, action } => {
                let Some(tab) = self.active_tab_mut() else {
                    return Task::none();
                };
                tab.selected_block = index;
                // A multi-line paste splits into blocks, like Futo; the
                // editor's default behavior would embed raw newlines into
                // the block.
                if let Some(task) = Self::intercept_paste(tab, index, &action) {
                    return Task::batch([task, self.update_title()]);
                }
                let is_edit = action.is_edit();
                if is_edit {
                    Self::push_undo(tab, index);
                }
                let Some(block) = tab.blocks.get_mut(index) else {
                    return Task::none();
                };
                block.content.perform(action);
                if is_edit {
                    tab.dirty = true;
                }
                self.update_title()
            }

            Message::ToggleTodo(index) => {
                let Some(tab) = self.active_tab_mut() else {
                    return Task::none();
                };
                tab.selected_block = index;
                Self::push_undo_step(tab);
                let Some(block) = tab.blocks.get_mut(index) else {
                    return Task::none();
                };
                if let BlockKind::Todo(_, done) = &mut block.kind {
                    *done = !*done;
                    tab.dirty = true;
                }
                self.update_title()
            }

            Message::SplitBlock {
                index,
                kind,
                before,
                after,
            } => {
                let Some(tab) = self.active_tab_mut() else {
                    return Task::none();
                };
                Self::push_undo_step(tab);
                let Some(block) = tab.blocks.get_mut(index) else {
                    return Task::none();
                };
                block.content = text_editor::Content::with_text(&before);
                tab.blocks.insert(
                    index + 1,
                    EditableBlock {
                        kind,
                        content: text_editor::Content::with_text(&after),
                    },
                );
                renumber_blocks(&mut tab.blocks);
                tab.selected_block = index + 1;
                tab.dirty = true;
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
                let Some(tab) = self.active_tab_mut() else {
                    return Task::none();
                };
                tab.selected_block = index;
                Self::push_undo_step(tab);
                let Some(block) = tab.blocks.get_mut(index) else {
                    return Task::none();
                };
                block.kind = kind;
                block.content = text_editor::Content::with_text(&content);
                renumber_blocks(&mut tab.blocks);
                tab.dirty = true;
                self.update_title()
            }

            Message::ChangeLevel { index, delta } => {
                let Some(tab) = self.active_tab_mut() else {
                    return Task::none();
                };
                tab.selected_block = index;
                Self::push_undo_step(tab);
                let Some(block) = tab.blocks.get_mut(index) else {
                    return Task::none();
                };
                let Some(kind) = Self::shift_level(&block.kind, delta) else {
                    return Task::none();
                };
                block.kind = kind;
                renumber_blocks(&mut tab.blocks);
                tab.dirty = true;
                self.update_title()
            }

            Message::MergeUp { index } => {
                let Some(tab) = self.active_tab_mut() else {
                    return Task::none();
                };
                if index == 0 || index >= tab.blocks.len() {
                    return Task::none();
                }
                Self::push_undo_step(tab);
                let merged = tab.blocks.remove(index);
                let Some(prev) = tab.blocks.get_mut(index - 1) else {
                    return Task::none();
                };
                let text = prev.content.text() + &merged.content.text();
                prev.content = text_editor::Content::with_text(&text);
                let cursor = Self::end_cursor(&prev.content);
                prev.content.move_to(cursor);
                renumber_blocks(&mut tab.blocks);
                tab.selected_block = index - 1;
                tab.dirty = true;
                Task::batch([
                    Self::focus_block(index - 1).map(cosmic::action::app),
                    self.update_title(),
                ])
            }

            Message::FocusPrevious(index) => {
                let Some(tab) = self.active_tab_mut() else {
                    return Task::none();
                };
                let Some(target) = Self::previous_editable(&tab.blocks, index) else {
                    return Task::none();
                };
                tab.selected_block = target;
                let Some(block) = tab.blocks.get_mut(target) else {
                    return Task::none();
                };
                let cursor = Self::end_cursor(&block.content);
                block.content.move_to(cursor);
                Self::focus_block(target).map(cosmic::action::app)
            }

            Message::FocusNext(index) => {
                let Some(tab) = self.active_tab_mut() else {
                    return Task::none();
                };
                let Some(target) = Self::next_editable(&tab.blocks, index) else {
                    return Task::none();
                };
                tab.selected_block = target;
                let Some(block) = tab.blocks.get_mut(target) else {
                    return Task::none();
                };
                block.content.move_to(text_editor::Cursor {
                    position: text_editor::Position { line: 0, column: 0 },
                    selection: None,
                });
                Self::focus_block(target).map(cosmic::action::app)
            }

            Message::FormatBlock { index, kind } => {
                let Some(tab) = self.active_tab_mut() else {
                    return Task::none();
                };
                tab.selected_block = index;
                Task::batch([Self::format_block(tab, index, kind), self.update_title()])
            }

            Message::WrapInline { index, marker } => {
                let Some(tab) = self.active_tab_mut() else {
                    return Task::none();
                };
                tab.selected_block = index;
                Self::push_undo_step(tab);
                let Some(block) = tab.blocks.get_mut(index) else {
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
                let result = editor::wrap_inline(
                    &line.text,
                    selection,
                    cursor.position.column,
                    open,
                    close,
                );
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
                tab.dirty = true;
                Task::batch([
                    Self::focus_block(index).map(cosmic::action::app),
                    self.update_title(),
                ])
            }

            Message::InsertDivider(index) => {
                let Some(tab) = self.active_tab_mut() else {
                    return Task::none();
                };
                tab.selected_block = index;
                Task::batch([
                    Self::format_block(tab, index, BlockKind::Divider),
                    self.update_title(),
                ])
            }

            Message::Undo => {
                let Some(tab) = self.active_tab_mut() else {
                    return Task::none();
                };
                let Some(previous) = tab.undo_stack.pop() else {
                    return Task::none();
                };
                let current = Self::snapshot_tab(tab);
                tab.redo_stack.push(current);
                Task::batch([Self::restore_blocks(tab, previous), self.update_title()])
            }

            Message::Redo => {
                let Some(tab) = self.active_tab_mut() else {
                    return Task::none();
                };
                let Some(next) = tab.redo_stack.pop() else {
                    return Task::none();
                };
                let current = Self::snapshot_tab(tab);
                tab.undo_stack.push(current);
                Task::batch([Self::restore_blocks(tab, next), self.update_title()])
            }

            Message::SlashMenuOpen(index) => {
                let Some(tab) = self.active_tab_mut() else {
                    return Task::none();
                };
                tab.slash_menu = Some(index);
                tab.slash_selection = 0;
                tab.selected_block = index;
                Task::none()
            }

            Message::SlashMenuClose => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.slash_menu = None;
                }
                Task::none()
            }

            Message::SlashMenuMove(delta) => {
                let Some(tab) = self.active_tab_mut() else {
                    return Task::none();
                };
                if tab.slash_menu.is_none() {
                    return Task::none();
                }
                tab.slash_selection = if delta > 0 {
                    (tab.slash_selection + 1).min(SLASH_ITEMS.len() - 1)
                } else {
                    tab.slash_selection.saturating_sub(1)
                };
                Task::none()
            }

            Message::SlashMenuSelect => {
                let Some(tab) = self.active_tab_mut() else {
                    return Task::none();
                };
                let Some(index) = tab.slash_menu.take() else {
                    return Task::none();
                };
                let kind = SLASH_ITEMS[tab.slash_selection].kind.clone();
                tab.selected_block = index;
                Task::batch([Self::format_block(tab, index, kind), self.update_title()])
            }

            Message::SlashMenuPick { index, kind } => {
                let Some(tab) = self.active_tab_mut() else {
                    return Task::none();
                };
                tab.slash_menu = None;
                tab.selected_block = index;
                Task::batch([Self::format_block(tab, index, kind), self.update_title()])
            }

            Message::AutosaveTick => self.save_active(false),

            Message::SaveNow => self.save_active(true),
        }
    }

    /// Called when the escape key is pressed.
    fn on_escape(&mut self) -> Task<cosmic::Action<Self::Message>> {
        self.sidebar_dialog = None;
        self.sidebar_context_menu_point = None;
        if let Some(tab) = self.active_tab_mut() {
            tab.slash_menu = None;
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
    /// The active tab, if any.
    fn active_tab(&self) -> Option<&Tab> {
        self.tab_model.active_data::<Tab>()
    }

    /// The active tab, if any.
    fn active_tab_mut(&mut self) -> Option<&mut Tab> {
        self.tab_model.active_data_mut::<Tab>()
    }

    /// The note the active tab edits, if any.
    fn active_note(&self) -> Option<&Note> {
        let tab = self.active_tab()?;
        let path = tab.path.as_ref()?;
        self.vault.notes().iter().find(|note| note.path() == path)
    }

    /// The current markdown text of a tab, serialized from its blocks.
    fn editor_text(tab: &Tab) -> String {
        let blocks: Vec<editor::Block> = tab.blocks.iter().map(EditableBlock::to_block).collect();
        editor::serialize_markdown(&blocks)
    }

    /// Inserts a new tab for the note at `path` and makes it active.
    fn new_tab(&mut self, path: PathBuf, body: &str, title: &str) {
        self.tab_model
            .insert()
            .text(title.to_owned())
            .data::<Tab>(Tab::new(path.clone(), body, title))
            .closable()
            .activate();
        self.remember_last_tab(&path);
        self.refresh_tab_closability();
    }

    /// Opens the note at `path` in a tab: activating the existing tab for it
    /// when one is open, loading it into the active blank tab when there is
    /// one, or creating a new tab (`force_new` always inserts a fresh tab, for
    /// "open in new tab").
    fn open_tab(&mut self, path: &Path, force_new: bool) -> Task<cosmic::Action<Message>> {
        self.commit_title_rename();
        let save = self.save_active(false);
        if !force_new {
            // A note can only be open in one tab unless opened in a new tab
            // explicitly, so switching re-activates the existing tab.
            let mut activate_opt = None;
            for entity in self.tab_model.iter() {
                if self
                    .tab_model
                    .data::<Tab>(entity)
                    .is_some_and(|tab| tab.path.as_deref() == Some(path))
                {
                    activate_opt = Some(entity);
                    break;
                }
            }
            if let Some(entity) = activate_opt {
                self.tab_model.activate(entity);
                self.rebuild_nav_model();
                return Task::batch([save, self.update_title()]);
            }
            // A fresh "+" tab has no note yet; picking a note from the sidebar
            // loads it into that tab instead of stacking another empty one.
            if self.active_tab().is_some_and(|tab| tab.path.is_none()) {
                let Some(note) = self.vault.notes().iter().find(|note| note.path() == path) else {
                    return Task::batch([save, self.update_title()]);
                };
                let body = note.body().to_owned();
                let title = note.title().to_owned();
                if let Some(tab) = self.active_tab_mut() {
                    tab.path = Some(path.to_path_buf());
                    tab.load_body(&body, &title);
                }
                let entity = self.tab_model.active();
                self.tab_model.text_set(entity, title);
                self.remember_last_tab(path);
                self.refresh_tab_closability();
                self.rebuild_nav_model();
                return Task::batch([
                    save,
                    Self::focus_block(0).map(cosmic::action::app),
                    self.update_title(),
                ]);
            }
        }
        let Some(note) = self.vault.notes().iter().find(|note| note.path() == path) else {
            return Task::batch([save, self.update_title()]);
        };
        let body = note.body().to_owned();
        let title = note.title().to_owned();
        self.new_tab(path.to_path_buf(), &body, &title);
        self.rebuild_nav_model();
        Task::batch([
            save,
            Self::focus_block(0).map(cosmic::action::app),
            self.update_title(),
        ])
    }

    /// Opens the sidebar note with the given id in a new tab.
    fn open_sidebar_item_new_tab(&mut self, entity: nav_bar::Id) -> Task<cosmic::Action<Message>> {
        let Some(NavEntry::Note(path)) = self.nav_model.data::<NavEntry>(entity).cloned() else {
            return Task::none();
        };
        self.open_tab(&path, true)
    }

    /// Closes the tab with the given entity, saving its changes. When the
    /// active tab is closed, the nearest neighbor (to the left, or the right
    /// for the first tab) becomes active, like cosmic-edit. The last tab is
    /// never removed: closing it resets it to the blank "Home" tab, so there
    /// is always at least one tab left open.
    fn close_tab(&mut self, entity: segmented_button::Entity) {
        self.commit_title_rename();
        let _ = self.save_active(false);

        // Keep at least one tab open: closing the only tab (note or Home)
        // replaces it with the blank Home tab instead of removing it.
        if self.tab_model.len() <= 1 {
            if let Some(tab) = self.tab_model.data_mut::<Tab>(entity) {
                *tab = Tab::blank();
            }
            self.tab_model.text_set(entity, fl!("home"));
            self.remember_no_tabs();
        } else {
            if self.tab_model.is_active(entity)
                && let Some(position) = self.tab_model.position(entity)
            {
                if position > 0 {
                    self.tab_model.activate_position(position - 1);
                } else {
                    self.tab_model.activate_position(position + 1);
                }
            }
            self.tab_model.remove(entity);
            match self.active_tab().and_then(|tab| tab.path.clone()) {
                Some(path) => self.remember_last_tab(&path),
                None => self.remember_no_tabs(),
            }
        }
        self.refresh_tab_closability();
        self.rebuild_nav_model();
    }

    /// Shows a close button on every tab except the last remaining one when
    /// it has no note open (the Home tab): that one cannot be closed.
    fn refresh_tab_closability(&mut self) {
        let entities: Vec<_> = self.tab_model.iter().collect();
        let is_only_tab = entities.len() == 1;
        for entity in entities {
            let is_home = self
                .tab_model
                .data::<Tab>(entity)
                .is_some_and(|tab| tab.path.is_none());
            self.tab_model.closable_set(entity, !(is_only_tab && is_home));
        }
    }

    /// Persists `path` (relative to the vault root) as the note to restore on
    /// the next launch.
    fn remember_last_tab(&mut self, path: &Path) {
        let Some(handler) = self.config_handler.as_ref() else {
            return;
        };
        let relative = path
            .strip_prefix(self.vault.root())
            .map(|relative| relative.to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.to_string_lossy().into_owned());
        let _ = self.config.set_last_note(handler, Some(relative));
    }

    /// Clears the persisted "last tab" when the last tab closes.
    fn remember_no_tabs(&mut self) {
        if let Some(handler) = &self.config_handler {
            let _ = self.config.set_last_note(handler, None);
        }
    }

    /// The settings panel: an empty column for now, matching the layout
    /// cosmic-store gives its (also empty) settings page.
    fn settings(&self) -> Element<'_, Message> {
        widget::settings::view_column(vec![]).into()
    }

    /// Creates a new note and opens it in a new tab.
    fn new_note(&mut self) -> Task<cosmic::Action<Message>> {
        let save = self.save_active(false);
        match self.vault.create(&fl!("untitled")) {
            Ok(Some(note)) => {
                let path = note.path().to_path_buf();
                let body = note.body().to_owned();
                let title = note.title().to_owned();
                self.new_tab(path, &body, &title);
                self.rebuild_nav_model();
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

    /// Creates a new note inside the folder with the given sidebar id and
    /// opens it in a new tab, expanding the folder to reveal it.
    fn new_note_in_folder(&mut self, entity: nav_bar::Id) -> Task<cosmic::Action<Message>> {
        let Some(NavEntry::Folder(parent)) = self.nav_model.data::<NavEntry>(entity).cloned() else {
            return Task::none();
        };
        let save = self.save_active(false);
        match self.vault.create_in(&parent, &fl!("untitled")) {
            Ok(Some(note)) => {
                let path = note.path().to_path_buf();
                let body = note.body().to_owned();
                let title = note.title().to_owned();
                self.expanded.insert(parent);
                self.new_tab(path, &body, &title);
                self.rebuild_nav_model();
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
        let Some(tab) = self.active_tab() else {
            return Task::none();
        };
        if !tab.dirty {
            return Task::none();
        }
        let Some(path) = tab.path.clone() else {
            // A blank tab has nothing to save yet.
            return Task::none();
        };
        let body = Self::editor_text(tab);
        match self.vault.save(&path, &body) {
            Ok(Some(SaveOutcome::Saved)) => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.dirty = false;
                }
                if notify {
                    self.toast(fl!("note-saved"))
                } else {
                    Task::none()
                }
            }
            Ok(Some(SaveOutcome::ConflictCopy { path: conflict })) => {
                // The file on disk changed since it was loaded, so the vault
                // wrote the local changes to a conflict copy; reload the tab
                // from what the disk now has for the note.
                let reload = {
                    let tab_path = self.active_tab().and_then(|tab| tab.path.clone());
                    match tab_path {
                        Some(tab_path) => self
                            .vault
                            .notes()
                            .iter()
                            .find(|note| note.path() == tab_path)
                            .map(|note| (note.body().to_owned(), note.title().to_owned())),
                        None => None,
                    }
                };
                if let Some(tab) = self.active_tab_mut() {
                    tab.dirty = false;
                    if let Some((body, title)) = reload {
                        tab.load_body(&body, &title);
                    }
                }
                let name = conflict
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.toast(fl!("conflict-copy", name = name))
            }
            Ok(None) => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.dirty = false;
                }
                Task::none()
            }
            Err(err) => {
                eprintln!("failed to save note: {err}");
                if let Some(tab) = self.active_tab_mut() {
                    tab.dirty = false;
                }
                self.toast(fl!("save-error"))
            }
        }
    }

    /// Commits the title draft as a rename, if it changed.
    fn commit_title_rename(&mut self) {
        let Some((path, title_draft)) = self
            .active_tab()
            .and_then(|tab| tab.path.clone().map(|path| (path, tab.title_draft.clone())))
        else {
            return;
        };
        let Some(note) = self.vault.notes().iter().find(|note| note.path() == path) else {
            return;
        };
        if note.title() == title_draft {
            return;
        }
        match self.vault.rename(&path, &title_draft) {
            Ok(Some(updated)) => {
                let entity = self.tab_model.active();
                let new_path = updated.path().to_path_buf();
                let new_title = updated.title().to_owned();
                if let Some(tab) = self.tab_model.active_data_mut::<Tab>() {
                    tab.path = Some(new_path);
                    tab.title_draft = new_title.clone();
                }
                // Keep the tab button label in sync with the new title.
                self.tab_model.text_set(entity, new_title);
                self.rebuild_nav_model();
            }
            Ok(None) => {}
            Err(err) => {
                eprintln!("failed to rename note: {err}");
            }
        }
    }

    /// Rebuilds the native COSMIC navigation model from the vault tree,
    /// highlighting the active tab's note.
    fn rebuild_nav_model(&mut self) {
        let selected = self.active_tab().and_then(|tab| tab.path.clone());
        let mut model = nav_bar::Model::default();
        Self::append_nav_nodes(
            &mut model,
            self.vault.tree(),
            self.vault.tree().root(),
            &self.expanded,
            selected.as_deref(),
            0,
        );
        self.nav_model = model;
    }

    /// Adds a folder's visible children to the native navigation model.
    fn append_nav_nodes(
        model: &mut nav_bar::Model,
        tree: &Tree,
        nodes: &[TreeNode],
        expanded: &HashSet<PathBuf>,
        selected: Option<&Path>,
        indent: u16,
    ) {
        for node in nodes {
            match node {
                TreeNode::Folder { .. } => {
                    let path = node.path().to_path_buf();
                    let open = expanded.contains(&path);
                    model
                        .insert()
                        .indent(indent)
                        .icon(icon::from_name(if open {
                            "folder-open-symbolic"
                        } else {
                            "folder-symbolic"
                        }))
                        .text(node.name().to_owned())
                        .data(NavEntry::Folder(path.clone()));
                    if open && let Some(children) = tree.children_of(&path) {
                        Self::append_nav_nodes(
                            model,
                            tree,
                            children,
                            expanded,
                            selected,
                            indent + 1,
                        );
                    }
                }
                TreeNode::File { .. } => {
                    let path = node.path().to_path_buf();
                    let entity = model
                        .insert()
                        .indent(indent)
                        .text(node.name().to_owned())
                        .data(NavEntry::Note(path.clone()))
                        .id();
                    if selected == Some(path.as_path()) {
                        model.activate(entity);
                    }
                }
            }
        }
    }

    /// Pushes a toast notification.
    fn toast(&mut self, message: String) -> Task<cosmic::Action<Message>> {
        self.toasts
            .push(toaster::Toast::new(message).duration(Duration::from_secs(3)))
            .map(cosmic::action::app)
    }

    /// The context menu for the empty area of the sidebar: create a note or a
    /// folder in the vault root. Right clicks on sidebar items open the
    /// items' own menus instead, which take precedence.
    ///
    /// The layout copies cosmic-files' menus: a [`menu::menu_column::MenuColumn`]
    /// of 24 px `MenuItem` buttons inside a 1 px padded, bordered container.
    fn sidebar_context_menu() -> Element<'static, cosmic::Action<Message>> {
        let menu_button = |label: String, message| {
            button::custom(
                cosmic::widget::Row::with_children([cosmic::widget::text::body(label).into()])
                    .height(Length::Fixed(24.0))
                    .align_y(Alignment::Center),
            )
            .padding([cosmic::theme::spacing().space_xxs, 16])
            .width(Length::Fill)
            .class(cosmic::theme::Button::MenuItem)
            .on_press(message)
        };
        let children = [
            menu_button(fl!("new-note"), cosmic::Action::App(Message::NewNote)).into(),
            menu_button(
                fl!("new-folder"),
                cosmic::Action::App(Message::NavMenuAction(NavMenuAction::NewFolder)),
            )
            .into(),
        ];
        cosmic::widget::container(menu::menu_column::MenuColumn::with_children(children))
            .padding(1)
            .style(|theme| {
                let cosmic = theme.cosmic();
                let component = &cosmic.background(theme.transparent);
                cosmic::widget::container::Style {
                    icon_color: Some(component.on.into()),
                    text_color: Some(component.on.into()),
                    background: Some(cosmic::iced::Background::Color(component.base.into())),
                    border: cosmic::iced::Border {
                        radius: cosmic.radius_s().map(|x| x + 1.0).into(),
                        width: 1.0,
                        color: component.divider.into(),
                    },
                    ..Default::default()
                }
            })
            .width(Length::Fixed(240.0))
            .into()
    }

    /// Opens the new-folder dialog for the given sidebar entity (or the vault
    /// root when `entity` is `None`), focusing the name input.
    fn open_folder_dialog(&mut self, entity: Option<nav_bar::Id>) -> Task<cosmic::Action<Message>> {
        let parent = match entity {
            Some(id) => match self.nav_model.data::<NavEntry>(id).cloned() {
                Some(NavEntry::Folder(path)) => path,
                _ => return Task::none(),
            },
            None => self.vault.root().to_path_buf(),
        };
        self.sidebar_dialog = Some(SidebarDialog {
            kind: SidebarDialogKind::NewFolder { parent },
            name: String::new(),
        });
        text_input::focus(cosmic::widget::Id::new(DIALOG_NAME_ID)).map(cosmic::action::app)
    }

    /// Opens the rename dialog for the folder with the given sidebar id,
    /// pre-filled with its current name.
    fn open_rename_dialog(&mut self, entity: nav_bar::Id) -> Task<cosmic::Action<Message>> {
        let Some(NavEntry::Folder(path)) = self.nav_model.data::<NavEntry>(entity).cloned() else {
            return Task::none();
        };
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.sidebar_dialog = Some(SidebarDialog {
            kind: SidebarDialogKind::RenameFolder { path },
            name,
        });
        text_input::focus(cosmic::widget::Id::new(DIALOG_NAME_ID)).map(cosmic::action::app)
    }

    /// Deletes the note or folder with the given sidebar id, closing any tab
    /// that edits the deleted note (or a note inside the deleted folder).
    fn delete_sidebar_item(&mut self, entity: nav_bar::Id) -> Task<cosmic::Action<Message>> {
        let Some(entry) = self.nav_model.data::<NavEntry>(entity).cloned() else {
            return Task::none();
        };
        match entry {
            NavEntry::Note(path) => match self.vault.delete(&path) {
                Ok(true) => {
                    let entities: Vec<_> = self.tab_model.iter().collect();
                    for entity in entities {
                        if self
                            .tab_model
                            .data::<Tab>(entity)
                            .is_some_and(|tab| tab.path.as_deref() == Some(path.as_path()))
                        {
                            self.close_tab(entity);
                        }
                    }
                    self.rebuild_nav_model();
                    Task::batch([self.toast(fl!("note-deleted")), self.update_title()])
                }
                Ok(false) => Task::none(),
                Err(err) => {
                    eprintln!("failed to delete note: {err}");
                    Task::none()
                }
            },
            NavEntry::Folder(path) => match self.vault.delete_folder(&path) {
                Ok(_) => {
                    self.expanded
                        .retain(|expanded| !expanded.starts_with(&path));
                    let entities: Vec<_> = self.tab_model.iter().collect();
                    for entity in entities {
                        if self
                            .tab_model
                            .data::<Tab>(entity)
                            .is_some_and(|tab| {
                                tab.path.as_deref().is_some_and(|p| p.starts_with(&path))
                            })
                        {
                            self.close_tab(entity);
                        }
                    }
                    self.rebuild_nav_model();
                    Task::batch([self.toast(fl!("folder-deleted")), self.update_title()])
                }
                Err(err) => {
                    eprintln!("failed to delete folder: {err}");
                    Task::none()
                }
            },
        }
    }

    /// Whether a folder with `name` already exists at the dialog's target.
    fn dialog_target_exists(kind: &SidebarDialogKind, name: &str) -> bool {
        match kind {
            SidebarDialogKind::NewFolder { parent } => parent.join(name).exists(),
            SidebarDialogKind::RenameFolder { path } => path
                .parent()
                .is_some_and(|parent| parent.join(name).exists() && parent.join(name) != *path),
        }
    }

    /// Whether the current sidebar dialog input passes the cosmic-files name
    /// rules, so the primary action may complete.
    fn dialog_is_valid(dialog_state: &SidebarDialog) -> bool {
        let name = dialog_state.name.as_str();
        !name.is_empty()
            && name != "."
            && name != ".."
            && !name.contains('/')
            && !Self::dialog_target_exists(&dialog_state.kind, name)
    }

    /// Rewrites expanded folder paths that live under `old` to their new
    /// location under `new`, so expansion state survives a folder rename.
    fn remap_expanded(&mut self, old: &Path, new: &Path) {
        self.expanded = self
            .expanded
            .drain()
            .map(|path| {
                if path.starts_with(old) {
                    new.join(
                        path.strip_prefix(old)
                            .expect("path was checked with starts_with"),
                    )
                } else {
                    path
                }
            })
            .collect();
    }

    /// The tab bar row: the open tabs plus a button that creates a new tab.
    /// This is the cosmic-edit layout: a shrink-wrapped tab bar with a small
    /// `+` button right next to it, above the editor content.
    fn tab_bar(model: &segmented_button::SingleSelectModel) -> Element<'_, Message> {
        widget::row::with_capacity(2)
            .align_y(Alignment::Center)
            .push(
                widget::tab_bar::horizontal(model)
                    .button_height(32)
                    .button_spacing(cosmic::theme::spacing().space_xxs)
                    .close_icon(icon::from_name("window-close-symbolic").size(16).icon())
                    .on_activate(Message::TabActivate)
                    .on_close(Message::TabClose)
                    .width(Length::Shrink),
            )
            .push(
                button::custom(icon::from_name("list-add-symbolic").size(16).icon())
                    .on_press(Message::TabNew)
                    .padding(cosmic::theme::spacing().space_xxs)
                    .class(cosmic::theme::Button::Icon),
            )
            .into()
    }

    /// The editor for the active tab: the tab bar, title input, toolbar, and
    /// block editor. Shows the empty state when there are no tabs or when the
    /// active tab is a fresh "+" tab with no note yet.
    fn editor_pane(&self) -> Element<'_, Message> {
        let Some(tab) = self.active_tab() else {
            return Self::editor_empty_state();
        };
        // A blank tab has no note yet: keep the tab bar so it can be switched
        // away from, and show the empty state below it.
        if tab.path.is_none() {
            return widget::column::with_capacity(2)
                .push(Self::tab_bar(&self.tab_model))
                .push(Self::editor_empty_state())
                .width(Length::Fill)
                .height(Length::Fill)
                .into();
        }
        let spacing = cosmic::theme::spacing();
        let title_input = text_input::text_input(fl!("note-title-placeholder"), &tab.title_draft)
            .id(cosmic::widget::Id::new(TITLE_ID))
            .on_input(Message::EditTitle)
            .on_submit(|_| Message::RenameSubmit)
            .width(Length::Fill);

        let mut column = widget::column::with_capacity(5)
            .push(Self::tab_bar(&self.tab_model))
            .push(widget::container(title_input).padding([spacing.space_s, spacing.space_s]))
            .push(divider::horizontal::default())
            .push(self.block_toolbar())
            .push(self.block_editor());
        if let Some(panel) = self.backlinks_panel() {
            column = column.push(divider::horizontal::default()).push(panel);
        }
        column.width(Length::Fill).height(Length::Fill).into()
    }

    /// A panel listing the notes that link to the active note, if any.
    fn backlinks_panel(&self) -> Option<Element<'_, Message>> {
        let path = self.active_tab().and_then(|tab| tab.path.clone())?;
        let links = crate::backlinks::backlinks(&self.vault, &path);
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

    /// The block editor: a scrollable column of per-block text editors for
    /// the active tab.
    fn block_editor(&self) -> Element<'_, Message> {
        let Some(tab) = self.active_tab() else {
            return widget::scrollable(widget::space()).height(Length::Fill).into();
        };
        let spacing = cosmic::theme::spacing();
        let mut column = widget::column::with_capacity(tab.blocks.len());
        for (index, block) in tab.blocks.iter().enumerate() {
            column = column.push(Self::block_element(
                index,
                block,
                &tab.blocks,
                tab.slash_menu == Some(index),
                tab.slash_selection,
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

    /// The formatting toolbar for the active tab's selected block: block
    /// style conversions and inline markdown wrapping. The active style is
    /// highlighted.
    fn block_toolbar(&self) -> Element<'_, Message> {
        let Some(tab) = self.active_tab() else {
            return widget::container(widget::space()).into();
        };
        let spacing = cosmic::theme::spacing();
        let index = tab.selected_block.min(tab.blocks.len().saturating_sub(1));
        let kind = tab
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

    /// A snapshot of a tab's whole document for undo/redo, rebuilt from its
    /// editable blocks as pure model blocks.
    fn snapshot_tab(tab: &Tab) -> Vec<editor::Block> {
        tab.blocks.iter().map(EditableBlock::to_block).collect()
    }

    /// Splits a multi-line paste into blocks (Futo behavior): the text
    /// before the caret stays in the block, each pasted line becomes its own
    /// block, and the text after the caret continues in a new block. Returns
    /// a focus task when the paste was handled here, skipping the editor's
    /// default behavior. Code blocks and single-line pastes keep the default.
    fn intercept_paste(
        tab: &mut Tab,
        index: usize,
        action: &text_editor::Action,
    ) -> Option<Task<cosmic::Action<Message>>> {
        let text_editor::Action::Edit(text_editor::Edit::Paste(text)) = action else {
            return None;
        };
        if !text.contains('\n') {
            return None;
        }
        let block = tab.blocks.get(index)?;
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
        Self::push_undo_step(tab);
        tab.blocks.splice(index..=index, replacement);
        renumber_blocks(&mut tab.blocks);
        tab.selected_block = caret;
        if let Some(block) = tab.blocks.get_mut(caret) {
            let cursor = Self::end_cursor(&block.content);
            block.content.move_to(cursor);
        }
        tab.dirty = true;
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

    /// Pushes a snapshot of the tab's current document and clears its redo
    /// stack.
    fn record_snapshot(tab: &mut Tab) {
        tab.undo_stack.push(Self::snapshot_tab(tab));
        if tab.undo_stack.len() > UNDO_LIMIT {
            tab.undo_stack.remove(0);
        }
        tab.redo_stack.clear();
    }

    /// Records the tab's current document as an undo step before an edit in
    /// the given block, coalescing bursts of typing into a single step.
    fn push_undo(tab: &mut Tab, block_index: usize) {
        let now = Instant::now();
        if !Self::should_snapshot(tab.last_undo_at, now, block_index) {
            return;
        }
        Self::record_snapshot(tab);
        tab.last_undo_at = Some((now, block_index));
    }

    /// Records the tab's current document as its own undo step for a
    /// structural change (split, convert, ...) and breaks the typing
    /// coalescing window so the following keystrokes start a fresh step.
    fn push_undo_step(tab: &mut Tab) {
        Self::record_snapshot(tab);
        tab.last_undo_at = None;
    }

    /// Replaces the tab's document with the given snapshot, places the caret
    /// at the end of `selected_block` (clamped to the restored document), and
    /// returns a task that refocuses the editor. The caller refreshes the
    /// title.
    fn restore_blocks(tab: &mut Tab, snapshot: Vec<editor::Block>) -> Task<cosmic::Action<Message>> {
        tab.blocks = snapshot
            .into_iter()
            .map(EditableBlock::from_block)
            .collect();
        tab.last_undo_at = None;
        let index = tab.selected_block.min(tab.blocks.len().saturating_sub(1));
        tab.selected_block = index;
        if let Some(block) = tab.blocks.get_mut(index) {
            let cursor = Self::end_cursor(&block.content);
            block.content.move_to(cursor);
        }
        tab.dirty = true;
        Self::focus_block(index).map(cosmic::action::app)
    }

    /// Converts the block at `index` to `kind`, keeping its text (dividers
    /// drop it), and returns a task that moves focus back into the document:
    /// the converted block, or the next editable block after a divider
    /// (inserting an empty paragraph when there is nowhere to type). The
    /// caller refreshes the title.
    fn format_block(tab: &mut Tab, index: usize, kind: BlockKind) -> Task<cosmic::Action<Message>> {
        if tab
            .blocks
            .get(index)
            .is_none_or(|block| block.kind == kind)
        {
            return Task::none();
        }
        Self::push_undo_step(tab);
        let Some(block) = tab.blocks.get_mut(index) else {
            return Task::none();
        };
        let is_divider = matches!(kind, BlockKind::Divider);
        let text = block.content.text();
        block.kind = kind;
        block.content = text_editor::Content::with_text(if is_divider { "" } else { &text });
        renumber_blocks(&mut tab.blocks);
        tab.dirty = true;
        if !is_divider {
            return Self::focus_block(index).map(cosmic::action::app);
        }
        if let Some(next) = Self::next_editable(&tab.blocks, index) {
            return Self::focus_block(next).map(cosmic::action::app);
        }
        let next = index + 1;
        tab.blocks.insert(
            next,
            EditableBlock::from_block(editor::Block::paragraph("")),
        );
        Self::focus_block(next).map(cosmic::action::app)
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
                .push(icon::from_name("text-x-generic-symbolic").size(48))
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

    /// Updates the header and window titles from the active tab.
    pub fn update_title(&mut self) -> Task<cosmic::Action<Message>> {
        let mut window_title = fl!("app-title");

        if let Some(note) = self.active_note() {
            window_title.push_str(" — ");
            window_title.push_str(note.title());
        }

        if self.active_tab().is_some_and(|tab| tab.dirty) {
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

/// The context page to display in the context drawer.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum ContextPage {
    #[default]
    About,
    /// The application settings, opened from the gear in the header bar.
    Settings,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MenuAction {
    NewNote,
    NewFolder,
    Save,
}

impl menu::action::MenuAction for MenuAction {
    type Message = Message;

    fn message(&self) -> Self::Message {
        match self {
            MenuAction::NewNote => Message::NewNote,
            MenuAction::NewFolder => Message::NavMenuAction(NavMenuAction::NewFolder),
            MenuAction::Save => Message::SaveNow,
        }
    }
}

/// An action from a sidebar context menu. The nav bar's context menu system
/// publishes [`cosmic::Action`], so these map through [`Message::NavMenuAction`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NavMenuAction {
    /// Create a folder at the vault root.
    NewFolder,
    /// Create a folder inside the folder with the given sidebar id.
    NewFolderIn(nav_bar::Id),
    /// Create a note inside the folder with the given sidebar id.
    NewNoteIn(nav_bar::Id),
    /// Rename the folder with the given sidebar id.
    RenameFolder(nav_bar::Id),
    /// Delete the note or folder with the given sidebar id.
    DeleteItem(nav_bar::Id),
    /// Open the note with the given sidebar id in a new tab.
    OpenInNewTab(nav_bar::Id),
}

impl menu::action::MenuAction for NavMenuAction {
    type Message = cosmic::Action<Message>;

    fn message(&self) -> Self::Message {
        cosmic::Action::App(Message::NavMenuAction(*self))
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
