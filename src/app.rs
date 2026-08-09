// SPDX-License-Identifier: MPL-2.0

use crate::config::Config;
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
    self, Toasts, about::About, button, divider, icon, list_column, markdown, menu, nav_bar,
    text_editor, text_input, toaster,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");
const APP_ICON: &[u8] = include_bytes!("../resources/icon.svg");

/// How often the autosave tick fires.
const AUTOSAVE_INTERVAL: Duration = Duration::from_millis(500);
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
    /// Contains items assigned to the nav bar panel.
    nav: nav_bar::Model,
    /// Key bindings for the application's menu bar.
    #[allow(clippy::zero_sized_map_values)]
    key_binds: HashMap<menu::KeyBind, MenuAction>,
    /// Configuration data that persists between application runs.
    config: Config,
    /// The note store.
    vault: Vault,
    /// The note currently open in the editor, if any.
    selected: Option<PathBuf>,
    /// The text editor buffer for the active note.
    editor: text_editor::Content,
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
    /// Permanently delete the trashed note with the given path.
    DeleteNote(PathBuf),
    /// Permanently delete all trashed notes.
    EmptyTrash,
    /// An action was performed in the text editor.
    Edit(text_editor::Action),
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
        // Create a nav bar with the note lists.
        let mut nav = nav_bar::Model::default();

        nav.insert()
            .text(fl!("all-notes"))
            .data::<Page>(Page::All)
            .icon(icon::from_name("document-text-symbolic"))
            .activate();

        nav.insert()
            .text(fl!("pinned"))
            .data::<Page>(Page::Pinned)
            .icon(icon::from_name("starred-symbolic"));

        nav.insert()
            .text(fl!("trash"))
            .data::<Page>(Page::Trash)
            .icon(icon::from_name("user-trash-symbolic"));

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
            nav,
            key_binds: Shortcut::key_binds(),
            config,
            vault,
            selected: None,
            editor: text_editor::Content::new(),
            title_draft: String::new(),
            dirty: false,
            preview: false,
            markdown: None,
            search: String::new(),
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
        vec![
            button::icon(icon::from_name("list-add-symbolic"))
                .on_press(Message::NewNote)
                .class(cosmic::theme::Button::Icon)
                .into(),
        ]
    }

    /// Enables the COSMIC application to create a nav bar with this model.
    fn nav_model(&self) -> Option<&nav_bar::Model> {
        Some(&self.nav)
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

    /// Describes the interface based on the current state of the application model.
    fn view(&self) -> Element<'_, Self::Message> {
        let content = widget::row::with_capacity(2)
            .push(self.note_pane())
            .push(divider::vertical::default())
            .push(self.editor_pane())
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

            Message::SelectNote(path) => {
                if self.selected.as_ref() == Some(&path) {
                    return Task::none();
                }
                self.commit_title_rename();
                let save = self.save_active(false);
                if let Some(note) = self.vault.notes().iter().find(|note| note.path() == path) {
                    let body = note.body().to_owned();
                    let title = note.title().to_owned();
                    self.selected = Some(path);
                    self.load_body(&body, &title);
                } else {
                    self.selected = Some(path);
                }
                Task::batch([save, self.update_title()])
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
                    Ok(Some(_)) => {
                        self.selected = None;
                        self.clear_editor();
                        Task::batch([save, self.toast(fl!("note-trashed")), self.update_title()])
                    }
                    Ok(None) => save,
                    Err(err) => {
                        eprintln!("failed to trash note: {err}");
                        Task::batch([save, self.toast(fl!("save-error"))])
                    }
                }
            }

            Message::RestoreNote(path) => match self.vault.restore(&path) {
                Ok(Some(_)) => {
                    let mut tasks = vec![self.toast(fl!("note-restored"))];
                    if self.selected.as_ref() == Some(&path) {
                        self.selected = None;
                        self.clear_editor();
                        tasks.push(self.update_title());
                    }
                    Task::batch(tasks)
                }
                Ok(None) => Task::none(),
                Err(err) => {
                    eprintln!("failed to restore note: {err}");
                    self.toast(fl!("save-error"))
                }
            },

            Message::DeleteNote(path) => match self.vault.delete(&path) {
                Ok(true) => {
                    let mut tasks = vec![self.toast(fl!("note-deleted"))];
                    if self.selected.as_ref() == Some(&path) {
                        self.selected = None;
                        self.clear_editor();
                        tasks.push(self.update_title());
                    }
                    Task::batch(tasks)
                }
                Ok(false) => Task::none(),
                Err(err) => {
                    eprintln!("failed to delete note: {err}");
                    self.toast(fl!("save-error"))
                }
            },

            Message::EmptyTrash => match self.vault.empty_trash() {
                Ok(count) if count > 0 => {
                    if let Some(path) = &self.selected
                        && self.vault.notes().iter().all(|note| note.path() != path)
                    {
                        self.selected = None;
                        self.clear_editor();
                    }
                    self.toast(fl!("trash-emptied"))
                }
                Ok(_) => Task::none(),
                Err(err) => {
                    eprintln!("failed to empty trash: {err}");
                    self.toast(fl!("save-error"))
                }
            },

            Message::Edit(action) => {
                let is_edit = action.is_edit();
                self.editor.perform(action);
                if is_edit {
                    self.dirty = true;
                    let text = self.editor_text();
                    self.markdown = Some(markdown::Content::parse(&text));
                }
                Task::none()
            }

            Message::TogglePreview => {
                self.preview = !self.preview;
                if self.preview && self.markdown.is_none() {
                    let text = self.editor_text();
                    self.markdown = Some(markdown::Content::parse(&text));
                }
                Task::none()
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

    /// Called when a nav item is selected.
    fn on_nav_select(&mut self, id: nav_bar::Id) -> Task<cosmic::Action<Self::Message>> {
        // Activate the page in the model.
        self.nav.activate(id);

        self.update_title()
    }

    /// Called when the search function is requested (Ctrl+F).
    fn on_search(&mut self) -> Task<cosmic::Action<Self::Message>> {
        text_input::focus(cosmic::widget::Id::new(SEARCH_ID)).map(cosmic::action::app)
    }

    /// Called when the escape key is pressed.
    fn on_escape(&mut self) -> Task<cosmic::Action<Self::Message>> {
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

    /// The current text of the editor.
    fn editor_text(&self) -> String {
        self.editor.text()
    }

    /// Loads the given note into the editor.
    fn load_note(&mut self, note: &Note) {
        let body = note.body().to_owned();
        let title = note.title().to_owned();
        self.load_body(&body, &title);
    }

    /// Loads the given body and title into the editor.
    fn load_body(&mut self, body: &str, title: &str) {
        self.editor = text_editor::Content::with_text(body);
        title.clone_into(&mut self.title_draft);
        self.dirty = false;
        self.markdown = Some(markdown::Content::parse(body));
    }

    /// Empties the editor and clears the title.
    fn clear_editor(&mut self) {
        self.editor = text_editor::Content::new();
        self.title_draft.clear();
        self.dirty = false;
        self.markdown = None;
    }

    /// Creates a new note and opens it in the editor.
    fn new_note(&mut self) -> Task<cosmic::Action<Message>> {
        let save = self.save_active(false);
        match self.vault.create(&fl!("untitled")) {
            Ok(Some(note)) => {
                let path = note.path().to_path_buf();
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
                    self.load_body(&body, &title);
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

    /// Pushes a toast notification.
    fn toast(&mut self, message: String) -> Task<cosmic::Action<Message>> {
        self.toasts
            .push(toaster::Toast::new(message).duration(Duration::from_secs(3)))
            .map(cosmic::action::app)
    }

    /// The notes shown in the middle pane, filtered by page and search query.
    fn visible_notes(&self) -> Vec<&Note> {
        let page = self.nav.active_data::<Page>().copied().unwrap_or(Page::All);
        let query = self.search.trim().to_lowercase();
        self.vault
            .notes()
            .iter()
            .filter(|note| {
                let in_trash = note.in_trash();
                let matches_page = match page {
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

    /// The middle pane: search box and the note list.
    fn note_pane(&self) -> Element<'_, Message> {
        let space_s = cosmic::theme::spacing().space_s;

        let search = text_input::search_input(fl!("search-placeholder"), &self.search)
            .id(cosmic::widget::Id::new(SEARCH_ID))
            .on_input(Message::SearchChanged)
            .width(Length::Fill);

        let notes = self.visible_notes();
        let list: Element<'_, Message> = if notes.is_empty() {
            self.list_empty_state()
        } else {
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
        };

        let mut column = widget::column::with_capacity(3)
            .push(widget::container(search).padding(space_s))
            .push(widget::scrollable(list).height(Length::Fill))
            .width(300)
            .height(Length::Fill);

        if self.nav.active_data::<Page>().copied() == Some(Page::Trash)
            && self.vault.notes().iter().any(Note::in_trash)
        {
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
        let page = self.nav.active_data::<Page>().copied().unwrap_or(Page::All);
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
            let theme = if cosmic::theme::active_type().is_dark() {
                cosmic::iced::highlighter::Theme::SolarizedDark
            } else {
                cosmic::iced::highlighter::Theme::InspiredGitHub
            };
            cosmic::widget::text_editor(&self.editor)
                .on_action(Message::Edit)
                .placeholder(fl!("note-placeholder"))
                .highlight("markdown", theme)
                .size(15)
                .padding([spacing.space_s, spacing.space_s])
                .height(Length::Fill)
                .into()
        };

        widget::column::with_capacity(2)
            .push(widget::container(header).padding([spacing.space_s, spacing.space_s]))
            .push(divider::horizontal::default())
            .push(body)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    /// The actions shown next to the title: trash, or restore and delete.
    fn note_actions(note: &Note) -> Element<'_, Message> {
        if note.in_trash() {
            widget::row::with_capacity(2)
                .push(
                    button::icon(icon::from_name("edit-undo-symbolic"))
                        .on_press(Message::RestoreNote(note.path().to_path_buf()))
                        .class(cosmic::theme::Button::Icon),
                )
                .push(
                    button::icon(icon::from_name("edit-delete-symbolic"))
                        .on_press(Message::DeleteNote(note.path().to_path_buf()))
                        .class(cosmic::theme::Button::Icon),
                )
                .spacing(cosmic::theme::spacing().space_xxs)
                .into()
        } else {
            button::icon(icon::from_name("user-trash-symbolic"))
                .on_press(Message::TrashNote)
                .class(cosmic::theme::Button::Icon)
                .into()
        }
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
