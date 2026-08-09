// SPDX-License-Identifier: MPL-2.0

use cosmic::cosmic_config::{self, CosmicConfigEntry, cosmic_config_derive::CosmicConfigEntry};

/// Persistent application settings.
#[derive(Debug, Default, Clone, CosmicConfigEntry, Eq, PartialEq)]
#[version = 1]
pub struct Config {
    /// Directory where notes are stored as markdown files.
    ///
    /// When `None`, the default of `~/Documents/cosmic-notes` is used.
    vault_path: Option<String>,
}

impl Config {
    /// Returns the configured vault directory, if any.
    #[must_use]
    pub fn vault_path(&self) -> Option<&str> {
        self.vault_path.as_deref()
    }
}
