//! Notes / PKM vault configuration ([`NotesConfig`]).
//!
//! The vault is the folder the note-app features (note-list pane, cross-note
//! search, wiki-links, backlinks) operate over. It is a single opt-in path:
//! SCR1B3 never assumes a vault until the user picks one, so a fresh install has
//! `vault_dir == None` and the notes pane shows a "choose a folder" prompt rather
//! than scanning an arbitrary directory. Purely additive — a config written
//! before this section deserializes the whole section to its default (no vault).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Configuration for the first-party notes / PKM layer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct NotesConfig {
    /// The vault root: the folder the note-list pane lists and cross-note search
    /// / backlinks scan. `None` until the user explicitly chooses one (the notes
    /// pane offers a picker). Every wiki-link / attachment target is resolved
    /// RELATIVE to this root through `notes::vault_path::resolve_in_vault`, which
    /// is the single traversal-safety chokepoint — a link can never escape it.
    #[serde(default)]
    pub vault_dir: Option<PathBuf>,
}

impl NotesConfig {
    /// The configured vault root, if one is set. A thin accessor so call sites
    /// read intent (`vault()`) rather than poking the field.
    #[must_use]
    pub fn vault(&self) -> Option<&std::path::Path> {
        self.vault_dir.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn default_has_no_vault() {
        assert!(NotesConfig::default().vault_dir.is_none());
        assert!(NotesConfig::default().vault().is_none());
    }

    #[test]
    fn vault_accessor_returns_the_set_path() {
        let c = NotesConfig {
            vault_dir: Some(PathBuf::from("/vault")),
        };
        assert_eq!(c.vault(), Some(std::path::Path::new("/vault")));
    }

    #[test]
    fn round_trips_through_config_toml() {
        // A vault path set on the root Config survives a TOML round-trip, and a
        // config that predates the section backfills to "no vault".
        let mut c = Config::default();
        c.notes.vault_dir = Some(PathBuf::from("/home/user/notes"));
        let back = Config::from_toml_str(&c.to_toml_string()).unwrap();
        assert_eq!(back.notes.vault_dir, c.notes.vault_dir);

        let legacy = Config::from_toml_str("[editor]\ntab_width = 4\n").unwrap();
        assert!(
            legacy.notes.vault_dir.is_none(),
            "a config predating [notes] backfills to no vault"
        );
    }
}
