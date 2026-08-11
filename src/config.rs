// SPDX-License-Identifier: MPL-2.0

use cosmic::cosmic_config::{self, CosmicConfigEntry, cosmic_config_derive::CosmicConfigEntry};

/// Persistent application settings.
#[derive(Debug, Default, Clone, CosmicConfigEntry, Eq, PartialEq)]
#[version = 2]
pub struct Config {
    /// Directory where notes are stored as markdown files.
    ///
    /// When `None`, the default of `~/Documents/cosmic-notes` is used.
    vault_path: Option<String>,
    /// The note that was open when the app last closed, as a path relative to
    /// the vault root. Restored as a tab on the next launch.
    last_note: Option<String>,
}

impl Config {
    /// Returns the configured vault directory, if any.
    #[must_use]
    pub fn vault_path(&self) -> Option<&str> {
        self.vault_path.as_deref()
    }

    /// Returns the path (relative to the vault root) of the last opened note,
    /// if any.
    #[must_use]
    pub fn last_note(&self) -> Option<&str> {
        self.last_note.as_deref()
    }
}
