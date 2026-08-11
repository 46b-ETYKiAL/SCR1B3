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

/// The vault-relative folder new daily notes are created in.
pub const DEFAULT_DAILY_FOLDER: &str = "daily";
/// The vault-relative folder pasted image attachments are written to.
pub const DEFAULT_ATTACHMENTS_FOLDER: &str = "attachments";

/// Configuration for the first-party notes / PKM layer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct NotesConfig {
    /// The vault root: the folder the note-list pane lists and cross-note search
    /// / backlinks scan. `None` until the user explicitly chooses one (the notes
    /// pane offers a picker). Every wiki-link / attachment target is resolved
    /// RELATIVE to this root through `notes::vault_path::resolve_in_vault`, which
    /// is the single traversal-safety chokepoint — a link can never escape it.
    #[serde(default)]
    pub vault_dir: Option<PathBuf>,

    /// Vault-relative folder for dated daily notes (`daily/2026-08-04.md`).
    /// Created on demand. A value that would escape the vault (`..`, absolute,
    /// drive-lettered) is rejected by `vault_path::resolve_in_vault` at use
    /// time, not here — the config is untrusted input like any other.
    pub daily_folder: String,

    /// Vault-relative folder for pasted image attachments. Same containment
    /// contract as [`NotesConfig::daily_folder`].
    pub attachments_folder: String,
}

impl Default for NotesConfig {
    fn default() -> Self {
        Self {
            vault_dir: None,
            daily_folder: DEFAULT_DAILY_FOLDER.to_string(),
            attachments_folder: DEFAULT_ATTACHMENTS_FOLDER.to_string(),
        }
    }
}

impl NotesConfig {
    /// The configured vault root, if one is set. A thin accessor so call sites
    /// read intent (`vault()`) rather than poking the field.
    #[must_use]
    pub fn vault(&self) -> Option<&std::path::Path> {
        self.vault_dir.as_deref()
    }

    /// The daily-note folder, falling back to the default when the user blanked
    /// it. An empty folder would make `resolve_in_vault` reject the whole
    /// target as `Empty`, so a blank value must never reach it.
    #[must_use]
    pub fn daily_folder(&self) -> &str {
        let t = self.daily_folder.trim();
        if t.is_empty() {
            DEFAULT_DAILY_FOLDER
        } else {
            t
        }
    }

    /// The attachments folder, with the same blank-value fallback as
    /// [`NotesConfig::daily_folder`].
    #[must_use]
    pub fn attachments_folder(&self) -> &str {
        let t = self.attachments_folder.trim();
        if t.is_empty() {
            DEFAULT_ATTACHMENTS_FOLDER
        } else {
            t
        }
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
            ..NotesConfig::default()
        };
        assert_eq!(c.vault(), Some(std::path::Path::new("/vault")));
    }

    #[test]
    fn round_trips_through_config_toml() {
        // A vault path set on the root Config survives a TOML round-trip, and a
        // config that predates the section backfills to "no vault".
        let mut c = Config::default();
        c.notes.vault_dir = Some(PathBuf::from("/vault/notes"));
        let back = Config::from_toml_str(&c.to_toml_string()).unwrap();
        assert_eq!(back.notes.vault_dir, c.notes.vault_dir);

        let legacy = Config::from_toml_str("[editor]\ntab_width = 4\n").unwrap();
        assert!(
            legacy.notes.vault_dir.is_none(),
            "a config predating [notes] backfills to no vault"
        );
    }

    #[test]
    fn default_folders_are_the_documented_names() {
        let c = NotesConfig::default();
        assert_eq!(c.daily_folder, "daily");
        assert_eq!(c.attachments_folder, "attachments");
        // The accessors agree with the raw fields when they are set.
        assert_eq!(c.daily_folder(), "daily");
        assert_eq!(c.attachments_folder(), "attachments");
    }

    #[test]
    fn a_config_predating_the_folder_keys_backfills_to_the_defaults() {
        // `#[serde(default)]` on the container fills missing fields from
        // `Default`, so an old scr1b3.toml with only `vault_dir` must NOT
        // deserialize the folders as empty strings (which would make every
        // resolve reject as `Empty`).
        let legacy = Config::from_toml_str("[notes]\nvault_dir = \"/vault\"\n").unwrap();
        assert_eq!(legacy.notes.vault_dir, Some(PathBuf::from("/vault")));
        assert_eq!(legacy.notes.daily_folder, "daily");
        assert_eq!(legacy.notes.attachments_folder, "attachments");
    }

    #[test]
    fn folders_round_trip_and_custom_values_are_kept() {
        let mut c = Config::default();
        c.notes.daily_folder = "journal/2026".to_string();
        c.notes.attachments_folder = "media".to_string();
        let back = Config::from_toml_str(&c.to_toml_string()).unwrap();
        assert_eq!(back.notes.daily_folder, "journal/2026");
        assert_eq!(back.notes.attachments_folder, "media");
        assert_eq!(back.notes.daily_folder(), "journal/2026");
        assert_eq!(back.notes.attachments_folder(), "media");
    }

    #[test]
    fn blank_or_whitespace_folders_fall_back_rather_than_resolving_empty() {
        // A user who clears the key in scr1b3.toml must still get daily notes,
        // not an `Empty` path rejection. The two accessors must fall back to
        // their OWN default — a shared fallback would put attachments in the
        // daily folder.
        let c = NotesConfig {
            vault_dir: None,
            daily_folder: "   ".to_string(),
            attachments_folder: String::new(),
        };
        assert_eq!(c.daily_folder(), "daily");
        assert_eq!(c.attachments_folder(), "attachments");
        assert_ne!(c.daily_folder(), c.attachments_folder());
    }

    #[test]
    fn accessors_trim_surrounding_whitespace() {
        let c = NotesConfig {
            vault_dir: None,
            daily_folder: "  journal  ".to_string(),
            attachments_folder: " media ".to_string(),
        };
        assert_eq!(c.daily_folder(), "journal");
        assert_eq!(c.attachments_folder(), "media");
    }
}
