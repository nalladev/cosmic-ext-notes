// SPDX-License-Identifier: MPL-2.0

use crate::config::Config;
use crate::editor;
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
    segmented_button, text_input, toaster, tooltip,
};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");
const APP_ICON: &[u8] = include_bytes!("../resources/icon.svg");

/// The id of the responsive menu bar, used to track its collapsed size.
static MENU_ID: std::sync::LazyLock<cosmic::widget::Id> =
    std::sync::LazyLock::new(|| cosmic::widget::Id::new("responsive-menu"));

/// How often the autosave tick fires.
const AUTOSAVE_INTERVAL: Duration = Duration::from_millis(500);
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
    /// The active COSMIC accent used when shaping Markdown link spans.
    editor_link_color: cosmic::iced::Color,
    /// Changes whenever the link color changes, invalidating editor paragraphs.
    editor_accent_revision: u64,
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
    /// Add a tag to the active note.
    AddTag,
    /// Edit the pending tag text.
    EditTag(String),
    /// Commit the pending tag.
    CommitTag,

    /// Remove a tag from the active note.
    RemoveTag(String),
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
    /// Native source-mapped Markdown editor input.
    Editor(editor::Message),
    /// Periodic autosave tick.
    AutosaveTick,
    /// Save the active note now.
    SaveNow,
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

/// Each tab owns one authored Markdown document and its cursor/history.
struct Tab {
    path: Option<PathBuf>,
    editor: editor::Editor,
    saved_body: String,
    title_draft: String,
    tags: Vec<String>,
    tag_draft: String,
    tag_editing: bool,
    dirty: bool,
}

impl Tab {
    fn new(path: PathBuf, body: &str, title: &str, tags: &[String]) -> Self {
        let mut editor = editor::Editor::new(body);
        if let Some(parent) = path.parent() {
            editor.set_image_directory(parent);
        }
        Self {
            path: Some(path),
            editor,
            saved_body: body.to_owned(),
            title_draft: title.to_owned(),
            tags: tags.to_vec(),
            tag_draft: String::new(),
            tag_editing: false,
            dirty: false,
        }
    }

    fn load_body(&mut self, body: &str, title: &str, tags: &[String]) {
        self.editor = editor::Editor::new(body);
        if let Some(parent) = self.path.as_ref().and_then(|path| path.parent()) {
            self.editor.set_image_directory(parent);
        }
        self.saved_body = body.to_owned();
        title.clone_into(&mut self.title_draft);
        self.tags = tags.to_vec();
        self.tag_draft.clear();
        self.tag_editing = false;
        self.dirty = false;
    }

    fn blank() -> Self {
        Self {
            path: None,
            editor: editor::Editor::new(""),
            saved_body: String::new(),
            title_draft: String::new(),
            tags: Vec::new(),
            tag_draft: String::new(),
            tag_editing: false,
            dirty: false,
        }
    }
}

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

    fn system_theme_update(
        &mut self,
        _keys: &[&'static str],
        new_theme: &cosmic::cosmic_theme::Theme,
    ) -> Task<cosmic::Action<Self::Message>> {
        let link_color = cosmic::iced::Color::from(*new_theme.accent_color());
        if self.editor_link_color != link_color {
            self.editor_link_color = link_color;
            self.editor_accent_revision = self.editor_accent_revision.wrapping_add(1);
        }
        Task::none()
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

        let editor_link_color =
            cosmic::iced::Color::from(*core.system_theme().cosmic().accent_color());
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
            editor_link_color,
            editor_accent_revision: 0,
            sidebar_dialog: None,
            sidebar_context_menu_point: None,
        };
        app.rebuild_nav_model();

        // Restore the tabs that were open when the app last closed, and the
        // tab that was active. Older configs only remember a single note;
        // those restore as a single tab.
        let mut restore_paths: Vec<String> = app.config.open_tabs().to_vec();
        if restore_paths.is_empty()
            && let Some(relative) = app.config.last_note()
        {
            restore_paths.push(relative.to_owned());
        }
        for relative in &restore_paths {
            let path = app.vault.root().join(relative);
            let Some(note) = app
                .vault
                .notes()
                .iter()
                .find(|note| note.path() == path && !note.in_trash())
            else {
                continue;
            };
            let body = note.body().to_owned();
            let title = note.title().to_owned();
            app.tab_model
                .insert()
                .text(title.clone())
                .data::<Tab>(Tab::new(path, &body, &title, note.tags()))
                .closable()
                .activate();
        }
        // The last closed state had no notes open: restore a single Home tab.
        if app.tab_model.len() == 0 {
            app.tab_model
                .insert()
                .text(fl!("home"))
                .data::<Tab>(Tab::blank())
                .closable()
                .activate();
        } else if let Some(relative) = app.config.last_note() {
            // Re-activate the tab that was active when the app closed.
            let path = app.vault.root().join(relative);
            let active_entity = app.tab_model.iter().find(|entity| {
                app.tab_model
                    .data::<Tab>(*entity)
                    .is_some_and(|tab| tab.path.as_deref() == Some(path.as_path()))
            });
            if let Some(entity) = active_entity {
                app.tab_model.activate(entity);
            }
        }
        app.refresh_tab_closability();
        app.rebuild_nav_model();

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
                Self::settings(),
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
                event::Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) => {
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
                self.persist_tabs();
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

            Message::AddTag => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.tag_editing = true;
                    tab.tag_draft.clear();
                }
                Task::none()
            }

            Message::EditTag(value) => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.tag_draft = value;
                }
                Task::none()
            }

            Message::CommitTag => {
                self.commit_tag();
                Task::none()
            }

            Message::RemoveTag(tag) => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.tags.retain(|existing| existing != &tag);
                    tab.dirty = true;
                }
                Task::none()
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

            Message::Editor(message) => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.editor.update(message);
                    tab.dirty = tab.editor.text() != tab.saved_body;
                }
                self.update_title()
            }

            Message::AutosaveTick => self.save_active(false),

            Message::SaveNow => self.save_active(true),
        }
    }

    /// Called when the escape key is pressed.
    fn on_escape(&mut self) -> Task<cosmic::Action<Self::Message>> {
        self.sidebar_dialog = None;
        self.sidebar_context_menu_point = None;
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

    /// Saving uses the authored source, never a serialization of rendered blocks.
    fn editor_text(tab: &Tab) -> String {
        tab.editor.text().to_owned()
    }

    /// Inserts a new tab for the note at `path` and makes it active.
    fn new_tab(&mut self, path: &Path, body: &str, title: &str, tags: &[String]) {
        self.tab_model
            .insert()
            .text(title.to_owned())
            .data::<Tab>(Tab::new(path.to_path_buf(), body, title, tags))
            .closable()
            .activate();
        self.persist_tabs();
        self.refresh_tab_closability();
    }

    /// Opens the note at `path` in a tab: activating the existing tab for it
    /// when one is open, loading it into the active tab otherwise (`force_new`
    /// always inserts a fresh tab, for "open in new tab").
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
            // Picking a note from the sidebar (or a backlink) loads it into
            // the active tab — whether that tab is a fresh "+" Home tab or
            // already shows another note — instead of stacking a new tab. The
            // previous note was saved above, so its tab is reused in place.
            let Some(note) = self.vault.notes().iter().find(|note| note.path() == path) else {
                return Task::batch([save, self.update_title()]);
            };
            let body = note.body().to_owned();
            let title = note.title().to_owned();
            let tags = note.tags().to_vec();
            if let Some(tab) = self.active_tab_mut() {
                tab.path = Some(path.to_path_buf());
                tab.load_body(&body, &title, &tags);
            }
            let entity = self.tab_model.active();
            self.tab_model.text_set(entity, title);
            self.persist_tabs();
            self.refresh_tab_closability();
            self.rebuild_nav_model();
            return Task::batch([save, self.update_title()]);
        }
        let Some(note) = self.vault.notes().iter().find(|note| note.path() == path) else {
            return Task::batch([save, self.update_title()]);
        };
        let body = note.body().to_owned();
        let title = note.title().to_owned();
        let tags = note.tags().to_vec();
        self.new_tab(path, &body, &title, &tags);
        self.rebuild_nav_model();
        Task::batch([save, self.update_title()])
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
        }
        self.persist_tabs();
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
            self.tab_model
                .closable_set(entity, !(is_only_tab && is_home));
        }
    }

    /// Persists the open tabs, in order, plus the active one to config so the
    /// next launch restores the same set of tabs. Blank "Home" tabs have no
    /// path and are not stored; an empty list means no notes were open.
    fn persist_tabs(&mut self) {
        let Some(handler) = self.config_handler.as_ref() else {
            return;
        };
        let relative = |path: &Path| {
            path.strip_prefix(self.vault.root()).map_or_else(
                |_| path.to_string_lossy().into_owned(),
                |relative| relative.to_string_lossy().into_owned(),
            )
        };
        let open_tabs = self
            .tab_model
            .iter()
            .filter_map(|entity| self.tab_model.data::<Tab>(entity))
            .filter_map(|tab| tab.path.as_ref())
            .map(|path| relative(path.as_path()))
            .collect();
        let active = self
            .active_tab()
            .and_then(|tab| tab.path.as_ref())
            .map(|path| relative(path.as_path()));
        let _ = self.config.set_open_tabs(handler, open_tabs);
        let _ = self.config.set_last_note(handler, active);
    }

    /// The settings panel: an empty column for now, matching the layout
    /// cosmic-store gives its (also empty) settings page.
    fn settings() -> Element<'static, Message> {
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
                self.new_tab(&path, &body, &title, note.tags());
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
        let Some(NavEntry::Folder(parent)) = self.nav_model.data::<NavEntry>(entity).cloned()
        else {
            return Task::none();
        };
        let save = self.save_active(false);
        match self.vault.create_in(&parent, &fl!("untitled")) {
            Ok(Some(note)) => {
                let path = note.path().to_path_buf();
                let body = note.body().to_owned();
                let title = note.title().to_owned();
                let tags = note.tags().to_vec();
                self.expanded.insert(parent);
                self.new_tab(&path, &body, &title, &tags);
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
        let tags = tab.tags.clone();
        match self.vault.save_with_tags(&path, &body, &tags) {
            Ok(Some(SaveOutcome::Saved)) => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.saved_body = body.clone();
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
                            .map(|note| {
                                (
                                    note.body().to_owned(),
                                    note.title().to_owned(),
                                    note.tags().to_vec(),
                                )
                            }),
                        None => None,
                    }
                };
                if let Some(tab) = self.active_tab_mut() {
                    tab.dirty = false;
                    if let Some((body, title, tags)) = reload {
                        tab.load_body(&body, &title, &tags);
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

    fn commit_tag(&mut self) {
        let Some(tab) = self.active_tab_mut() else {
            return;
        };
        let tag = tab.tag_draft.trim().trim_start_matches('#').trim();
        if !tag.is_empty()
            && !tab
                .tags
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(tag))
        {
            tab.tags.push(tag.to_owned());
            tab.tags.sort_unstable_by_key(|value| value.to_lowercase());
            tab.dirty = true;
        }
        tab.tag_draft.clear();
        tab.tag_editing = false;
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
                    new_title.clone_into(&mut tab.title_draft);
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
                        if self.tab_model.data::<Tab>(entity).is_some_and(|tab| {
                            tab.path.as_deref().is_some_and(|p| p.starts_with(&path))
                        }) {
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

    /// The note content column is at most this wide so that lines don't
    /// stretch across wide windows; it shrinks when the window is narrower.
    const EDITOR_MAX_WIDTH: f32 = 718.0;

    /// Space between the tab bar and the note content.
    const EDITOR_TOP_PADDING: f32 = 16.0;

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
                .push(Self::centered_tab_content(Self::editor_empty_state()))
                .width(Length::Fill)
                .height(Length::Fill)
                .into();
        }
        let spacing = cosmic::theme::spacing();
        let title_input = text_input::text_input(fl!("note-title-placeholder"), &tab.title_draft)
            .id(cosmic::widget::Id::new(TITLE_ID))
            .on_input(Message::EditTitle)
            .on_submit(|_| Message::RenameSubmit)
            .style(cosmic::theme::TextInput::EditableText)
            .size(27)
            .font(cosmic::iced::Font {
                weight: cosmic::iced::font::Weight::Semibold,
                ..Default::default()
            })
            .padding([0, 0])
            .width(Length::Fill);

        let mut content = widget::column::with_capacity(4)
            .push(widget::container(title_input).padding([
                spacing.space_s,
                spacing.space_s,
                0,
                spacing.space_s,
            ]))
            .push(self.tag_row())
            .push(self.tag_line())
            .push(
                widget::container(self.block_editor())
                    .width(Length::Fill)
                    .padding([0, 44, 0, 44]),
            );
        if let Some(panel) = self.backlinks_panel() {
            content = content.push(divider::horizontal::default()).push(panel);
        }
        let content = content.width(Length::Fill).height(Length::Fill);
        widget::column::with_capacity(2)
            .push(Self::tab_bar(&self.tab_model))
            .push(Self::centered_tab_content(content))
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    /// Wraps a tab's content so it is at most [`Self::EDITOR_MAX_WIDTH`] wide
    /// and centered below the tab bar, with [`Self::EDITOR_TOP_PADDING`] of
    /// space above it. The inner column's cross alignment centers the content
    /// container, so it stays centered when the window grows.
    fn centered_tab_content<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
        widget::column::with_capacity(1)
            .push(
                widget::container(content)
                    .width(Length::Fill)
                    .max_width(Self::EDITOR_MAX_WIDTH)
                    .height(Length::Fill)
                    .padding([Self::EDITOR_TOP_PADDING, 0.0, 0.0, 0.0]),
            )
            .align_x(Horizontal::Center)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
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

    fn tag_row(&self) -> Element<'_, Message> {
        let Some(tab) = self.active_tab() else {
            return widget::space().into();
        };
        let mut row = widget::row::with_capacity(tab.tags.len() + 1)
            .spacing(cosmic::theme::spacing().space_xxs)
            .align_y(Alignment::Center);
        for tag in &tab.tags {
            let chip = widget::row::with_capacity(2)
                .spacing(2)
                .align_y(Alignment::Center)
                .push(widget::text::caption(tag.clone()))
                .push(
                    button::icon(icon::from_name("window-close-symbolic").handle())
                        .on_press(Message::RemoveTag(tag.clone()))
                        .class(cosmic::theme::Button::Icon)
                        .padding(0),
                );
            row = row.push(
                widget::container(chip)
                    .padding([2, 6])
                    .class(cosmic::theme::Container::Secondary),
            );
        }
        if tab.tag_editing {
            row = row.push(
                text_input::inline_input("Tag", &tab.tag_draft)
                    .on_input(Message::EditTag)
                    .on_submit(|_| Message::CommitTag)
                    .size(13)
                    .width(Length::Fixed(120.0)),
            );
        } else {
            row = row.push(
                widget::container(
                    button::custom(widget::text::caption("+ Tag"))
                        .on_press(Message::AddTag)
                        .padding([2, 8])
                        .class(cosmic::theme::Button::Text),
                )
                .style(|theme| {
                    let cosmic = theme.cosmic();
                    let component = cosmic.background(theme.transparent);
                    cosmic::widget::container::Style {
                        icon_color: Some(component.on.into()),
                        text_color: Some(component.on.into()),
                        background: None,
                        border: cosmic::iced::Border {
                            radius: cosmic.radius_s().into(),
                            width: 1.0,
                            color: component.divider.into(),
                        },
                        ..Default::default()
                    }
                }),
            );
        }
        widget::container(row)
            .padding([
                0,
                cosmic::theme::spacing().space_s,
                cosmic::theme::spacing().space_xxs,
                cosmic::theme::spacing().space_s,
            ])
            .width(Length::Fill)
            .into()
    }

    fn tag_line(&self) -> Element<'_, Message> {
        let Some(tab) = self.active_tab() else {
            return widget::space().into();
        };
        if tab.tags.is_empty() {
            return widget::space().into();
        }
        let link_color = self.editor_link_color;
        let spacing = cosmic::theme::spacing();
        let mut row = widget::row::with_capacity(tab.tags.len())
            .spacing(spacing.space_xxs)
            .align_y(Alignment::Center);
        for tag in &tab.tags {
            row = row.push(widget::text::body(format!("#{}", tag)).class(link_color));
        }
        widget::container(row)
            .padding([0, 44, spacing.space_xxs, 44])
            .width(Length::Fill)
            .into()
    }

    fn block_editor(&self) -> Element<'_, Message> {
        match self.active_tab() {
            Some(tab) => tab
                .editor
                .view(self.editor_link_color, self.editor_accent_revision)
                .map(Message::Editor),
            None => widget::space().into(),
        }
    }

    /* REMOVED persistent formatting toolbar
    fn block_toolbar(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let mut row = widget::row::with_capacity(18).spacing(spacing.space_xxs);
        for (icon, format, label) in [
            (
                "format-justify-left-symbolic",
                MarkdownFormat::Paragraph,
                fl!("paragraph"),
            ),
            (
                "format-text-heading-1-symbolic",
                MarkdownFormat::Heading(1),
                fl!("heading-1"),
            ),
            (
                "format-text-heading-2-symbolic",
                MarkdownFormat::Heading(2),
                fl!("heading-2"),
            ),
            (
                "format-text-heading-3-symbolic",
                MarkdownFormat::Heading(3),
                fl!("heading-3"),
            ),
            (
                "format-list-unordered-symbolic",
                MarkdownFormat::UnorderedList,
                fl!("bullet-list"),
            ),
            (
                "format-list-ordered-symbolic",
                MarkdownFormat::OrderedList,
                fl!("numbered-list"),
            ),
            (
                "format-list-checkbox-symbolic",
                MarkdownFormat::TaskList,
                fl!("todo-list"),
            ),
            (
                "format-quote-block-symbolic",
                MarkdownFormat::BlockQuote,
                fl!("quote"),
            ),
            (
                "format-text-code-symbolic",
                MarkdownFormat::CodeFence,
                fl!("code-block"),
            ),
            (
                "format-text-bold-symbolic",
                MarkdownFormat::Bold,
                fl!("bold"),
            ),
            (
                "format-text-italic-symbolic",
                MarkdownFormat::Italic,
                fl!("italic"),
            ),
            (
                "format-text-code-symbolic",
                MarkdownFormat::InlineCode,
                fl!("inline-code"),
            ),
        ] {
            row = row.push(Self::editor_button(
                icon,
                editor::Message::Format(format),
                label,
            ));
        }
        row = row
            .push(Self::editor_button(
                "format-indent-less-symbolic",
                editor::Message::Indent(false),
                fl!("outdent"),
            ))
            .push(Self::editor_button(
                "format-indent-more-symbolic",
                editor::Message::Indent(true),
                fl!("indent"),
            ))
            .push(Self::editor_button(
                "edit-undo-symbolic",
                editor::Message::Undo,
                "Undo (Ctrl+Z)".into(),
            ))
            .push(Self::editor_button(
                "edit-redo-symbolic",
                editor::Message::Redo,
                "Redo (Ctrl+Shift+Z)".into(),
            ));
        widget::container(row)
            .width(Length::Fill)
            .padding([
                            0.0,
                            f32::from(spacing.space_s),
                            f32::from(spacing.space_xxs),
                            f32::from(spacing.space_s),
                        ])
            .into()
    }

    fn editor_button(
        name: &'static str,
        command: editor::Message,
        label: String,
    ) -> Element<'static, Message> {
        Self::tooltip(
            button::icon(Self::toolbar_icon(name))
                .on_press(Message::Editor(command))
                .class(cosmic::theme::Button::Icon)
                .into(),
            label,
        )
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

    */

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
