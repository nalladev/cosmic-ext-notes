// SPDX-License-Identifier: MPL-2.0

//! Plain-markdown note storage.
//!
//! Notes are stored as individual `.md` files in a "vault" directory (by default
//! `~/Documents/cosmic-notes`, like Futo Notes). The title of a note is the file
//! name without the extension. Metadata such as the pinned state and creation
//! time live in a small YAML-like frontmatter block at the top of the file.
//!
//! This module is intentionally free of any UI or COSMIC dependencies so that it
//! can be unit-tested in isolation.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Name of the directory inside the vault that holds trashed notes.
pub const TRASH_DIR: &str = ".trash";

/// A single note backed by one markdown file on disk.
#[derive(Debug, Clone)]
pub struct Note {
    /// Absolute path to the `.md` file (inside the vault or its trash dir).
    path: PathBuf,
    /// The title of the note, i.e. the file stem.
    title: String,
    /// Whether the note is pinned.
    pinned: bool,
    /// Creation timestamp (RFC 3339) from the frontmatter, if any.
    created: Option<String>,
    /// Whether the note should open in preview mode.
    preview: bool,
    /// Last modification time of the file, if known.
    modified: Option<SystemTime>,
    /// The full text of the file as it was loaded from disk.
    loaded: String,
    /// The note body: everything after the frontmatter block.
    body: String,
}

impl Note {
    /// Absolute path of the backing file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The title of the note (file stem).
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Whether the note is pinned.
    #[must_use]
    pub fn pinned(&self) -> bool {
        self.pinned
    }

    /// Whether the note should open in preview mode.
    #[must_use]
    pub fn preview(&self) -> bool {
        self.preview
    }

    /// Last modification time of the file, if known.
    #[must_use]
    pub fn modified(&self) -> Option<SystemTime> {
        self.modified
    }

    /// The note body: everything after the frontmatter block.
    #[must_use]
    pub fn body(&self) -> &str {
        &self.body
    }

    /// A short preview of the note: the first non-empty, non-heading line.
    #[must_use]
    pub fn snippet(&self) -> String {
        self.body
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty() && !line.starts_with('#'))
            .unwrap_or_default()
            .chars()
            .take(140)
            .collect()
    }

    /// Whether the note currently lives in the trash directory.
    #[must_use]
    pub fn in_trash(&self) -> bool {
        self.path
            .parent()
            .is_some_and(|parent| parent.file_name().is_some_and(|name| name == TRASH_DIR))
    }

    /// The full file text (frontmatter plus body).
    #[must_use]
    fn full_text(&self) -> String {
        let mut frontmatter = Frontmatter::new();
        frontmatter.set_pinned(self.pinned);
        frontmatter.set_preview(self.preview);
        if let Some(created) = &self.created {
            frontmatter.set_created(created.clone());
        }
        frontmatter.render() + &self.body
    }

    /// Builds a note from the given path and full file text.
    fn from_text(path: PathBuf, text: String) -> Self {
        let title = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        let (frontmatter, body) = Frontmatter::parse(&text);
        let body = body.to_owned();
        let modified = fs::metadata(&path).and_then(|meta| meta.modified()).ok();
        Self {
            path,
            title,
            pinned: frontmatter.pinned(),
            created: frontmatter.created().map(str::to_owned),
            preview: frontmatter.preview(),
            modified,
            loaded: text,
            body,
        }
    }

    /// Reloads the in-memory fields from fresh file text, keeping the path.
    fn reload_from(&mut self, text: &str) {
        let (frontmatter, body) = Frontmatter::parse(text);
        self.title = self
            .path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.pinned = frontmatter.pinned();
        self.created = frontmatter.created().map(str::to_owned);
        self.preview = frontmatter.preview();
        self.modified = fs::metadata(&self.path)
            .and_then(|meta| meta.modified())
            .ok();
        text.clone_into(&mut self.loaded);
        body.clone_into(&mut self.body);
    }
}

/// The result of saving a note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveOutcome {
    /// The note was written back to its file.
    Saved,
    /// The file changed on disk since it was loaded; the local changes were
    /// written to a conflict copy instead, and the note was reloaded from disk.
    ConflictCopy {
        /// Path of the conflict copy that holds the local changes.
        path: PathBuf,
    },
}

/// An in-memory store of all notes in a vault directory.
#[derive(Debug)]
pub struct Vault {
    /// The root directory that holds the notes.
    root: PathBuf,
    /// The trash directory inside the vault.
    trash: PathBuf,
    /// All notes (including trashed ones), sorted: pinned first, then by
    /// modification time descending, then by title.
    notes: Vec<Note>,
}

impl Vault {
    /// Opens (creating if necessary) the vault at the given directory and
    /// scans it for notes.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be created or read.
    pub fn open(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        let trash = root.join(TRASH_DIR);
        fs::create_dir_all(&root)?;
        fs::create_dir_all(&trash)?;

        let mut notes = Vec::new();
        Self::scan_dir(&root, &mut notes)?;
        Self::scan_dir(&trash, &mut notes)?;

        notes.sort_by(|a, b| {
            b.pinned
                .cmp(&a.pinned)
                .then_with(|| b.modified.cmp(&a.modified))
                .then_with(|| a.title.cmp(&b.title))
        });

        Ok(Self { root, trash, notes })
    }

    /// All notes, including trashed ones.
    #[must_use]
    pub fn notes(&self) -> &[Note] {
        &self.notes
    }

    /// Creates a new note with the given title. The title is sanitized and made
    /// unique (e.g. `Untitled 2.md` if `Untitled.md` already exists).
    ///
    /// Returns `None` if the title is not usable.
    ///
    /// # Errors
    ///
    /// Returns an error if the note file cannot be written.
    pub fn create(&mut self, title: &str) -> io::Result<Option<Note>> {
        let Some(stem) = sanitize_title(title) else {
            return Ok(None);
        };
        let path = unique_path(&self.root, &stem);
        let mut frontmatter = Frontmatter::new();
        frontmatter.set_pinned(false);
        frontmatter.set_created(now_rfc3339());
        let text = frontmatter.render();
        write_atomic(&path, &text)?;
        let note = Note::from_text(path, text);
        self.notes.push(note.clone());
        self.sort_notes();
        Ok(Some(note))
    }

    /// Saves the body of the note with the given path, writing it atomically.
    ///
    /// If the file changed on disk since it was loaded, the local changes are
    /// written to a conflict copy and the note is reloaded from disk instead.
    ///
    /// Returns `None` if no note with the given path is known.
    ///
    /// # Errors
    ///
    /// Returns an error if the note file cannot be read or written.
    pub fn save(&mut self, id: &Path, body: &str) -> io::Result<Option<SaveOutcome>> {
        let Some(index) = self.notes.iter().position(|note| note.path == id) else {
            return Ok(None);
        };
        let note = &mut self.notes[index];
        body.clone_into(&mut note.body);
        let new_text = note.full_text();

        let disk_text = fs::read_to_string(&note.path)?;
        if disk_text != note.loaded {
            let conflict = conflict_path(&note.path);
            write_atomic(&conflict, &new_text)?;
            note.reload_from(&disk_text);
            return Ok(Some(SaveOutcome::ConflictCopy { path: conflict }));
        }

        write_atomic(&note.path, &new_text)?;
        note.loaded = new_text;
        note.modified = fs::metadata(&note.path)
            .and_then(|meta| meta.modified())
            .ok();
        Ok(Some(SaveOutcome::Saved))
    }

    /// Renames the note with the given path, moving it to a sanitized, unique
    /// name. Returns the updated note, or `None` if the note is unknown or the
    /// new title is not usable.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be moved.
    pub fn rename(&mut self, id: &Path, new_title: &str) -> io::Result<Option<Note>> {
        let Some(stem) = sanitize_title(new_title) else {
            return Ok(None);
        };
        let Some(index) = self.notes.iter().position(|note| note.path == id) else {
            return Ok(None);
        };
        let note = &mut self.notes[index];
        let dir = note.path.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "note path has no parent")
        })?;
        let new_path = unique_path(dir, &stem);
        fs::rename(&note.path, &new_path)?;
        note.path = new_path;
        note.title = note
            .path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        let updated = note.clone();
        self.sort_notes();
        Ok(Some(updated))
    }

    /// Toggles the pinned state of the note with the given path. The frontmatter
    /// is updated in place, preserving any changes that happened on disk.
    /// Returns the updated note, or `None` if the note is unknown.
    ///
    /// # Errors
    ///
    /// Returns an error if the note file cannot be read or written.
    pub fn toggle_pin(&mut self, id: &Path) -> io::Result<Option<Note>> {
        let Some(index) = self.notes.iter().position(|note| note.path == id) else {
            return Ok(None);
        };
        let note = &mut self.notes[index];
        note.pinned = !note.pinned;

        let disk_text = fs::read_to_string(&note.path)?;
        let (mut frontmatter, body) = Frontmatter::parse(&disk_text);
        frontmatter.set_pinned(note.pinned);
        let text = frontmatter.render() + body;
        write_atomic(&note.path, &text)?;
        note.reload_from(&text);
        let updated = note.clone();
        self.sort_notes();
        Ok(Some(updated))
    }

    /// Sets the preview preference of the note with the given path. The
    /// frontmatter is updated in place, and the file's modification time is
    /// preserved so the note does not reorder in the list. Returns the updated
    /// note, or `None` if the note is unknown.
    ///
    /// # Errors
    ///
    /// Returns an error if the note file cannot be read or written.
    pub fn set_preview(&mut self, id: &Path, preview: bool) -> io::Result<Option<Note>> {
        let Some(index) = self.notes.iter().position(|note| note.path == id) else {
            return Ok(None);
        };
        let note = &mut self.notes[index];
        note.preview = preview;

        let disk_text = fs::read_to_string(&note.path)?;
        let (mut frontmatter, body) = Frontmatter::parse(&disk_text);
        frontmatter.set_preview(preview);
        let text = frontmatter.render() + body;
        write_atomic(&note.path, &text)?;
        if let Some(modified) = note.modified {
            set_modified_time(&note.path, modified)?;
        }
        note.reload_from(&text);
        let updated = note.clone();
        Ok(Some(updated))
    }

    /// Moves the note with the given path into the trash. Returns the updated
    /// note, or `None` if the note is unknown or already trashed.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be moved.
    pub fn trash(&mut self, id: &Path) -> io::Result<Option<Note>> {
        let Some(index) = self.notes.iter().position(|note| note.path == id) else {
            return Ok(None);
        };
        let note = &mut self.notes[index];
        if note.in_trash() {
            return Ok(None);
        }
        let new_path = unique_path(&self.trash, &note.title);
        fs::rename(&note.path, &new_path)?;
        note.path = new_path;
        let updated = note.clone();
        self.sort_notes();
        Ok(Some(updated))
    }

    /// Moves a trashed note back to the vault root. Returns the updated note,
    /// or `None` if the note is unknown or not in the trash.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be moved.
    pub fn restore(&mut self, id: &Path) -> io::Result<Option<Note>> {
        let Some(index) = self.notes.iter().position(|note| note.path == id) else {
            return Ok(None);
        };
        let note = &mut self.notes[index];
        if !note.in_trash() {
            return Ok(None);
        }
        let new_path = unique_path(&self.root, &note.title);
        fs::rename(&note.path, &new_path)?;
        note.path = new_path;
        let updated = note.clone();
        self.sort_notes();
        Ok(Some(updated))
    }

    /// Permanently deletes the note with the given path. Returns `false` if the
    /// note is unknown.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be removed.
    pub fn delete(&mut self, id: &Path) -> io::Result<bool> {
        let Some(index) = self.notes.iter().position(|note| note.path == id) else {
            return Ok(false);
        };
        let path = self.notes[index].path.clone();
        fs::remove_file(&path)?;
        self.notes.remove(index);
        Ok(true)
    }

    /// Permanently deletes every trashed note. Returns the number of deleted
    /// notes.
    ///
    /// # Errors
    ///
    /// Returns an error if any file cannot be removed.
    pub fn empty_trash(&mut self) -> io::Result<usize> {
        let mut deleted = 0;
        let mut index = 0;
        while index < self.notes.len() {
            let in_trash = self.notes[index].in_trash();
            if in_trash {
                let path = self.notes[index].path.clone();
                fs::remove_file(&path)?;
                self.notes.remove(index);
                deleted += 1;
            } else {
                index += 1;
            }
        }
        Ok(deleted)
    }

    /// Scans a directory for markdown files and appends them as notes.
    fn scan_dir(dir: &Path, notes: &mut Vec<Note>) -> io::Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() || !is_markdown(&path) || is_hidden(&path) {
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            notes.push(Note::from_text(path, text));
        }
        Ok(())
    }

    /// Re-sorts the notes: pinned first, then by modification time descending,
    /// then by title.
    fn sort_notes(&mut self) {
        self.notes.sort_by(|a, b| {
            b.pinned
                .cmp(&a.pinned)
                .then_with(|| b.modified.cmp(&a.modified))
                .then_with(|| a.title.cmp(&b.title))
        });
    }
}

/// A minimal YAML-like frontmatter block delimited by `---` lines.
#[derive(Debug, Clone, Default)]
struct Frontmatter {
    pinned: Option<bool>,
    created: Option<String>,
    /// Whether the note should open in preview mode.
    preview: Option<bool>,
    /// Lines that are not recognized key/value pairs, preserved verbatim.
    extra: Vec<String>,
}

impl Frontmatter {
    /// Creates an empty frontmatter block.
    fn new() -> Self {
        Self::default()
    }

    /// The pinned state, defaulting to `false`.
    fn pinned(&self) -> bool {
        self.pinned.unwrap_or(false)
    }

    /// The creation timestamp, if any.
    fn created(&self) -> Option<&str> {
        self.created.as_deref()
    }

    /// The preview preference, defaulting to `false`.
    fn preview(&self) -> bool {
        self.preview.unwrap_or(false)
    }

    /// Sets the pinned state.
    fn set_pinned(&mut self, pinned: bool) {
        self.pinned = Some(pinned);
    }

    /// Sets the creation timestamp.
    fn set_created(&mut self, created: String) {
        self.created = Some(created);
    }

    /// Sets the preview preference.
    fn set_preview(&mut self, preview: bool) {
        self.preview = Some(preview);
    }

    /// Parses a frontmatter block from the start of `text`. Returns the parsed
    /// frontmatter and the body (everything after the closing `---` line).
    /// If the text does not start with a well-formed frontmatter block, an empty
    /// frontmatter is returned together with the unchanged text.
    fn parse(text: &str) -> (Self, &str) {
        let mut frontmatter = Self::default();
        let Some(rest) = text.strip_prefix("---\n") else {
            return (frontmatter, text);
        };
        let Some(end) = rest.find("\n---\n") else {
            return (frontmatter, text);
        };
        let block = &rest[..end];
        let body = &rest[end + 5..];
        for line in block.split('\n') {
            if let Some((key, value)) = line.split_once(':') {
                match key.trim() {
                    "pinned" => frontmatter.pinned = Some(value.trim() == "true"),
                    "created" => frontmatter.created = Some(unquote(value.trim())),
                    "preview" => frontmatter.preview = Some(value.trim() == "true"),
                    _ => frontmatter.extra.push(line.to_owned()),
                }
            } else {
                frontmatter.extra.push(line.to_owned());
            }
        }
        (frontmatter, body)
    }

    /// Renders the frontmatter block, including the closing `---` line and a
    /// trailing blank line.
    fn render(&self) -> String {
        use std::fmt::Write;
        let mut out = String::from("---\n");
        writeln!(out, "pinned: {}", self.pinned.unwrap_or(false)).expect("write to string");
        if let Some(created) = &self.created {
            writeln!(out, "created: {created}").expect("write to string");
        }
        writeln!(out, "preview: {}", self.preview.unwrap_or(false)).expect("write to string");
        for line in &self.extra {
            out.push_str(line);
            out.push('\n');
        }
        out.push_str("---\n\n");
        out
    }
}

/// Strips surrounding quotes from a YAML scalar.
fn unquote(value: &str) -> String {
    let trimmed = value.trim();
    if (trimmed.starts_with('"') && trimmed.ends_with('"'))
        || (trimmed.starts_with('\'') && trimmed.ends_with('\''))
    {
        trimmed[1..trimmed.len().saturating_sub(1)].to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Sanitizes a title into a usable file stem.
///
/// Returns `None` if the result would be empty, `.`, or `..`.
#[must_use]
pub fn sanitize_title(title: &str) -> Option<String> {
    let cleaned: String = title
        .chars()
        .filter(|c| {
            !matches!(
                c,
                '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0'
            )
        })
        .collect();
    let cleaned = cleaned.trim().to_owned();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        None
    } else {
        Some(cleaned)
    }
}

/// Returns a path in `dir` with the given stem that does not exist yet, by
/// appending ` 2`, ` 3`, … when necessary.
fn unique_path(dir: &Path, stem: &str) -> PathBuf {
    let mut candidate = dir.join(format!("{stem}.md"));
    let mut n = 2;
    while candidate.exists() {
        candidate = dir.join(format!("{stem} {n}.md"));
        n += 1;
    }
    candidate
}

/// Returns a path for a conflict copy of the note at `path`.
fn conflict_path(path: &Path) -> PathBuf {
    let stem = path.file_stem().map_or_else(
        || "note".to_owned(),
        |stem| stem.to_string_lossy().into_owned(),
    );
    let ts = now_rfc3339().replace([':', '-'], "");
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    parent.join(format!("{stem}.conflict-{ts}.md"))
}

/// Writes `text` to `path` atomically via a temporary file and rename.
fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Restores the modification time of a file.
fn set_modified_time(path: &Path, time: SystemTime) -> io::Result<()> {
    let file = fs::File::options().write(true).open(path)?;
    file.set_times(fs::FileTimes::new().set_modified(time))?;
    Ok(())
}

/// Whether the file name has a markdown extension.
fn is_markdown(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
}

/// Whether the file name starts with a dot (hidden file or directory).
fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name.to_string_lossy().starts_with('.'))
}

/// Returns the current time in RFC 3339 UTC format.
#[must_use]
pub fn now_rfc3339() -> String {
    timestamp_rfc3339(SystemTime::now())
}

/// Formats a [`SystemTime`] as an RFC 3339 UTC timestamp.
#[must_use]
pub fn timestamp_rfc3339(time: SystemTime) -> String {
    let secs = time
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    let (year, month, day) = unix_secs_to_date(secs);
    let seconds_of_day = secs % 86_400;
    let (hour, minute, second) = (
        seconds_of_day / 3_600,
        (seconds_of_day % 3_600) / 60,
        seconds_of_day % 60,
    );
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Converts seconds since the Unix epoch to a calendar date `(year, month, day)`.
#[must_use]
pub fn unix_secs_to_date(secs: u64) -> (i64, u32, u32) {
    civil_from_days(i64::try_from(secs / 86_400).unwrap_or(0))
}

/// Converts a count of days since the Unix epoch to a calendar date.
/// Uses the civil-from-days algorithm (Howard Hinnant).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    } as u32;
    if month <= 2 {
        year += 1;
    }
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static TEMP_COUNTER: AtomicU32 = AtomicU32::new(0);

    /// Creates a fresh, empty temporary directory for a test.
    fn temp_dir() -> PathBuf {
        let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("cosmic-notes-test-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("failed to create temp dir");
        path
    }

    #[test]
    fn sanitize_title_filters_bad_characters() {
        assert_eq!(
            sanitize_title("My/Note: *draft*?"),
            Some("MyNote draft".to_owned())
        );
        assert_eq!(sanitize_title("  hello  "), Some("hello".to_owned()));
        assert_eq!(sanitize_title(""), None);
        assert_eq!(sanitize_title("   "), None);
        assert_eq!(sanitize_title("."), None);
        assert_eq!(sanitize_title(".."), None);
        assert_eq!(sanitize_title("///"), None);
    }

    #[test]
    fn frontmatter_round_trips() {
        let text =
            "---\npinned: true\ncreated: 2026-08-09T12:00:00Z\nfoo: bar\n---\n\n# Hi\n\nBody";
        let (frontmatter, body) = Frontmatter::parse(text);
        assert!(frontmatter.pinned());
        assert_eq!(frontmatter.created(), Some("2026-08-09T12:00:00Z"));
        assert_eq!(body, "\n# Hi\n\nBody");
        let rendered = frontmatter.render();
        let (again, body2) = Frontmatter::parse(&rendered);
        assert!(again.pinned());
        assert_eq!(again.created(), frontmatter.created());
        assert_eq!(body2, "\n");
    }

    #[test]
    fn frontmatter_absent_and_unterminated() {
        let plain = "Just text";
        let (frontmatter, body) = Frontmatter::parse(plain);
        assert!(!frontmatter.pinned());
        assert_eq!(body, plain);

        let unterminated = "---\npinned: true\nno closing marker";
        let (frontmatter, body) = Frontmatter::parse(unterminated);
        assert!(!frontmatter.pinned());
        assert_eq!(body, unterminated);
    }

    #[test]
    fn open_creates_directories() {
        let dir = temp_dir().join("vault");
        let vault = Vault::open(&dir).expect("vault should open");
        assert!(dir.is_dir());
        assert!(dir.join(TRASH_DIR).is_dir());
        assert!(vault.notes().is_empty());
    }

    #[test]
    fn create_and_scan() {
        let dir = temp_dir();
        let mut vault = Vault::open(&dir).expect("open");
        let note = vault
            .create("Shopping")
            .expect("create")
            .expect("usable title");
        assert_eq!(note.title(), "Shopping");
        assert!(note.path().is_file());
        assert!(!note.pinned());

        // A second vault instance sees the note.
        let reopened = Vault::open(&dir).expect("reopen");
        assert_eq!(reopened.notes().len(), 1);
        assert_eq!(reopened.notes()[0].title(), "Shopping");
        assert_eq!(reopened.notes()[0].snippet(), "");
    }

    #[test]
    fn unique_names() {
        let dir = temp_dir();
        let mut vault = Vault::open(&dir).expect("open");
        let first = vault.create("Untitled").expect("create").expect("title");
        let second = vault.create("Untitled").expect("create").expect("title");
        let third = vault.create("Untitled").expect("create").expect("title");
        assert_eq!(first.path().file_stem().unwrap(), "Untitled");
        assert_eq!(second.path().file_stem().unwrap(), "Untitled 2");
        assert_eq!(third.path().file_stem().unwrap(), "Untitled 3");
        assert_eq!(vault.notes().len(), 3);
    }

    #[test]
    fn save_updates_file_and_snippet() {
        let dir = temp_dir();
        let mut vault = Vault::open(&dir).expect("open");
        let note = vault.create("Draft").expect("create").expect("title");
        let path = note.path().to_path_buf();

        let outcome = vault
            .save(&path, "# Heading\n\nFirst real line\nSecond line")
            .expect("save");
        assert_eq!(outcome, Some(SaveOutcome::Saved));

        let on_disk = fs::read_to_string(&path).expect("read");
        assert!(on_disk.contains("First real line"));
        assert!(on_disk.starts_with("---\npinned: false\n"));

        let updated = vault
            .notes()
            .iter()
            .find(|note| note.path() == path)
            .expect("note present");
        assert_eq!(updated.snippet(), "First real line");
    }

    #[test]
    fn save_conflict_writes_copy_and_reloads() {
        let dir = temp_dir();
        let mut vault = Vault::open(&dir).expect("open");
        let note = vault.create("Shared").expect("create").expect("title");
        let path = note.path().to_path_buf();

        vault.save(&path, "my local edits").expect("initial save");

        // Simulate an external change to the file after it was loaded.
        fs::write(&path, "external change").expect("write");

        let outcome = vault.save(&path, "my local edits, take two").expect("save");
        let SaveOutcome::ConflictCopy { path: conflict } = outcome.expect("conflict outcome")
        else {
            panic!("expected a conflict copy");
        };
        assert!(conflict.is_file());
        let conflict_text = fs::read_to_string(&conflict).expect("read conflict");
        assert!(conflict_text.contains("my local edits, take two"));

        // The original file keeps the external change and the note reloads it.
        assert_eq!(fs::read_to_string(&path).expect("read"), "external change");
        let reloaded = vault
            .notes()
            .iter()
            .find(|note| note.path() == path)
            .expect("note present");
        assert_eq!(reloaded.body(), "external change");
    }

    #[test]
    fn rename_moves_file() {
        let dir = temp_dir();
        let mut vault = Vault::open(&dir).expect("open");
        let note = vault.create("Old name").expect("create").expect("title");
        let old_path = note.path().to_path_buf();

        let renamed = vault
            .rename(&old_path, "New/name: v2")
            .expect("rename")
            .expect("note");
        assert_eq!(renamed.title(), "Newname v2");
        assert!(!old_path.exists());
        assert!(renamed.path().is_file());

        // Renaming to an existing name picks a unique one.
        vault.create("Newname v2").expect("create").expect("title");
        let again = vault
            .rename(renamed.path(), "Newname v2")
            .expect("rename")
            .expect("note");
        assert_eq!(again.title(), "Newname v2 3");
        assert!(again.path().is_file());
    }

    #[test]
    fn toggle_pin_preserves_body_and_extras() {
        let dir = temp_dir();
        let mut vault = Vault::open(&dir).expect("open");
        let note = vault.create("Ideas").expect("create").expect("title");
        let path = note.path().to_path_buf();
        vault.save(&path, "# Ideas\n\nSome body").expect("save");

        let pinned = vault.toggle_pin(&path).expect("toggle").expect("note");
        assert!(pinned.pinned());

        let unpinned = vault.toggle_pin(&path).expect("toggle").expect("note");
        assert!(!unpinned.pinned());

        let on_disk = fs::read_to_string(&path).expect("read");
        assert!(on_disk.contains("Some body"));
    }

    #[test]
    fn set_preview_persists_and_preserves_mtime() {
        let dir = temp_dir();
        let mut vault = Vault::open(&dir).expect("open");
        let note = vault.create("Preview me").expect("create").expect("title");
        let path = note.path().to_path_buf();
        let mtime = note.modified().expect("mtime");

        // Toggling preview must not reorder the list, so the mtime is preserved.
        let updated = vault
            .set_preview(&path, true)
            .expect("set preview")
            .expect("note");
        assert!(updated.preview());
        assert_eq!(updated.modified(), Some(mtime));

        // The preference is stored in the frontmatter and survives a reopen.
        let on_disk = fs::read_to_string(&path).expect("read");
        assert!(on_disk.contains("preview: true"));
        let reopened = Vault::open(&dir).expect("reopen");
        let reloaded = reopened
            .notes()
            .iter()
            .find(|note| note.path() == path)
            .expect("note present");
        assert!(reloaded.preview());

        // Turning it off writes `preview: false`.
        let off = vault
            .set_preview(&path, false)
            .expect("set preview")
            .expect("note");
        assert!(!off.preview());
        assert!(fs::read_to_string(&path).expect("read").contains("preview: false"));
    }

    #[test]
    fn trash_restore_and_delete() {
        let dir = temp_dir();
        let mut vault = Vault::open(&dir).expect("open");
        let note = vault.create("Temp").expect("create").expect("title");
        let path = note.path().to_path_buf();

        let trashed = vault.trash(&path).expect("trash").expect("note");
        assert!(trashed.in_trash());
        assert!(!path.exists());

        // Trashing again is a no-op.
        assert!(vault.trash(trashed.path()).expect("trash").is_none());

        // Trashed notes are still scanned after reopening.
        let reopened = Vault::open(&dir).expect("reopen");
        assert_eq!(reopened.notes().len(), 1);
        assert!(reopened.notes()[0].in_trash());

        let restored = vault
            .restore(trashed.path())
            .expect("restore")
            .expect("note");
        assert!(!restored.in_trash());
        assert!(restored.path().is_file());

        let path = restored.path().to_path_buf();
        let trashed_again = vault.trash(&path).expect("trash").expect("note");
        assert!(vault.delete(trashed_again.path()).expect("delete"));
        assert!(!trashed_again.path().exists());
        assert_eq!(vault.notes().len(), 0);
    }

    #[test]
    fn empty_trash_deletes_only_trashed() {
        let dir = temp_dir();
        let mut vault = Vault::open(&dir).expect("open");
        let keep = vault.create("Keep").expect("create").expect("title");
        let gone = vault.create("Gone").expect("create").expect("title");
        let keep_path = keep.path().to_path_buf();
        vault.trash(gone.path()).expect("trash");

        let deleted = vault.empty_trash().expect("empty trash");
        assert_eq!(deleted, 1);
        assert!(keep_path.is_file());
        assert_eq!(vault.notes().len(), 1);
    }

    #[test]
    fn hidden_files_and_non_markdown_are_ignored() {
        let dir = temp_dir();
        fs::write(dir.join(".hidden.md"), "ignored").expect("write");
        fs::write(dir.join("readme.txt"), "ignored").expect("write");
        let vault = Vault::open(&dir).expect("open");
        assert!(vault.notes().is_empty());
    }

    #[test]
    fn snippet_skips_frontmatter_headings_and_blank_lines() {
        let text = "---\npinned: true\n---\n\n# Only a heading\n\n- a list item\n\nBody line";
        let note = Note::from_text(PathBuf::from("/tmp/Note.md"), text.to_owned());
        assert_eq!(note.snippet(), "- a list item");
    }

    #[test]
    #[allow(clippy::duration_suboptimal_units)]
    fn dates_are_correct() {
        assert_eq!(unix_secs_to_date(0), (1970, 1, 1));
        assert_eq!(unix_secs_to_date(86_400), (1970, 1, 2));
        assert_eq!(unix_secs_to_date(946_684_800), (2000, 1, 1));
        assert_eq!(unix_secs_to_date(1_752_825_600), (2025, 7, 18));
        assert_eq!(
            timestamp_rfc3339(UNIX_EPOCH + std::time::Duration::from_secs(946_684_800)),
            "2000-01-01T00:00:00Z"
        );
    }

    #[test]
    fn pinned_notes_sort_first() {
        let dir = temp_dir();
        let mut vault = Vault::open(&dir).expect("open");
        let a = vault.create("A").expect("create").expect("title");
        let b = vault.create("B").expect("create").expect("title");
        let c = vault.create("C").expect("create").expect("title");

        // Give the notes distinct modification times so the sort is deterministic.
        let set_mtime = |path: &Path, secs: u64| {
            let file = fs::File::options().write(true).open(path).expect("open");
            let times = fs::FileTimes::new()
                .set_modified(UNIX_EPOCH + std::time::Duration::from_secs(secs));
            file.set_times(times).expect("set times");
        };
        set_mtime(a.path(), 100);
        set_mtime(b.path(), 200);
        set_mtime(c.path(), 300);
        let mut vault = Vault::open(&dir).expect("reopen");
        vault.toggle_pin(b.path()).expect("pin");

        let titles: Vec<&str> = vault.notes().iter().map(Note::title).collect();
        assert_eq!(titles, vec!["B", "C", "A"]);
    }
}
