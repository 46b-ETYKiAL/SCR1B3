//! Default-app / file-association model (schema v4).
//!
//! The single source of truth for the file types SCR1B3 can register itself as
//! the default handler for, mapped to each OS's identifier scheme:
//!
//! - **Windows** — a file EXTENSION (no dot) plus the per-extension ProgID we
//!   register under `HKCU\Software\Classes`.
//! - **macOS** — a Uniform Type Identifier (UTI). System UTIs resolve through
//!   the conformance tree (`.rs`/`.c`/`.py` → `public.source-code` →
//!   `public.plain-text`); only Markdown/JSON need a distinct UTI claimed by name.
//! - **Linux** — a freedesktop MIME type, set as default via `xdg-mime` /
//!   `~/.config/mimeapps.list`.
//!
//! Shared by the Settings UI, the per-OS registration backends in
//! `scribe-app::integration`, and (eventually) the installer manifests, so the
//! claimed set can never drift between them. Pure data + mapping — fully
//! unit-tested below.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The document format SCR1B3 suggests when you save a not-yet-saved buffer via
/// "Save As". This is ONLY a default for the save dialog — you can still type
/// any name / extension you like in the dialog. Markdown is the default because
/// SCR1B3 is a note-first editor.
///
/// Deliberately small but extensible: add a variant plus its arm in each `match`
/// (and it is a member of [`ALL`](Self::ALL)) and the new format flows into both
/// the Settings chooser and the Save-As dialog with no other wiring.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum DefaultSaveFormat {
    /// Markdown notes (`.md`). The default.
    #[default]
    Markdown,
    /// Plain text (`.txt`).
    PlainText,
}

impl DefaultSaveFormat {
    /// Every format, in display order. Drives the Settings dropdown and the
    /// secondary Save-As dialog filters.
    pub const ALL: [DefaultSaveFormat; 2] =
        [DefaultSaveFormat::Markdown, DefaultSaveFormat::PlainText];

    /// File extension (NO leading dot) for this format — used as the primary
    /// Save-As filter and appended to a chosen name that has no extension.
    pub fn extension(self) -> &'static str {
        match self {
            DefaultSaveFormat::Markdown => "md",
            DefaultSaveFormat::PlainText => "txt",
        }
    }

    /// Short filter label shown in the Save-As dialog for this format.
    pub fn filter_label(self) -> &'static str {
        match self {
            DefaultSaveFormat::Markdown => "Markdown",
            DefaultSaveFormat::PlainText => "Plain Text",
        }
    }

    /// Human label for the Settings dropdown (name + extension).
    pub fn ui_label(self) -> &'static str {
        match self {
            DefaultSaveFormat::Markdown => "Markdown (.md)",
            DefaultSaveFormat::PlainText => "Plain Text (.txt)",
        }
    }

    /// The name SCR1B3 pre-fills in the Save-As dialog for a buffer: `<stem>.<ext>`,
    /// defaulting the stem to `untitled` when the buffer has never been named.
    /// A blank / whitespace-only stem also falls back to `untitled`.
    pub fn suggested_file_name(self, stem: Option<&str>) -> String {
        let stem = stem
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("untitled");
        format!("{stem}.{}", self.extension())
    }
}

/// Ensure a user-chosen Save-As path carries an extension: if the file name has
/// NO extension at all, append `default_ext` (so `notes` → `notes.md`); if it
/// already carries ANY extension — even a different one the user typed on
/// purpose (`notes.txt`) — leave it exactly as given. Pure + testable so the
/// Save-As codepath stays drivable without the rfd dialog.
pub fn ensure_extension(path: &Path, default_ext: &str) -> PathBuf {
    match path.extension() {
        Some(_) => path.to_path_buf(),
        None => {
            let mut p = path.to_path_buf();
            p.set_extension(default_ext);
            p
        }
    }
}

/// A logical group of file types the user can ask SCR1B3 to become the default
/// app for. Coarser than raw extensions so the Settings UI stays a short, legible
/// checklist while each group still expands to the right per-OS identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClaimType {
    /// `.txt` and the plain-text family (including logs).
    PlainText,
    /// Markdown notes (`.md`, `.markdown`).
    Markdown,
    /// JSON documents (`.json`).
    Json,
    /// Config & tabular data formats (`.toml`, `.yaml`, `.ini`, `.csv`, `.xml`).
    ConfigData,
    /// Source code across common languages.
    SourceCode,
}

impl ClaimType {
    /// Every claimable group, in display order.
    pub const ALL: [ClaimType; 5] = [
        ClaimType::PlainText,
        ClaimType::Markdown,
        ClaimType::Json,
        ClaimType::ConfigData,
        ClaimType::SourceCode,
    ];

    /// Stable serialization key (persisted in [`IntegrationConfig::claimed_types`]
    /// and used as a config/CLI token — NEVER change an existing value).
    pub fn key(self) -> &'static str {
        match self {
            ClaimType::PlainText => "plain_text",
            ClaimType::Markdown => "markdown",
            ClaimType::Json => "json",
            ClaimType::ConfigData => "config_data",
            ClaimType::SourceCode => "source_code",
        }
    }

    /// Human label for the Settings checklist.
    pub fn label(self) -> &'static str {
        match self {
            ClaimType::PlainText => "Plain text & logs",
            ClaimType::Markdown => "Markdown",
            ClaimType::Json => "JSON",
            ClaimType::ConfigData => "Config & data",
            ClaimType::SourceCode => "Source code",
        }
    }

    /// A short, human-readable sample of the extensions this group claims, for
    /// the Settings checklist's secondary line. Built from
    /// [`windows_extensions`](Self::windows_extensions) so it can never drift
    /// from the set actually registered: the first few, then "+N more".
    pub fn extension_summary(self) -> String {
        let exts = self.windows_extensions();
        const SHOWN: usize = 4;
        let head = exts
            .iter()
            .take(SHOWN)
            .map(|e| format!(".{e}"))
            .collect::<Vec<_>>()
            .join(", ");
        if exts.len() > SHOWN {
            format!("{head} +{} more", exts.len() - SHOWN)
        } else {
            head
        }
    }

    /// Resolve a persisted [`key`](Self::key) back to its group.
    pub fn from_key(key: &str) -> Option<ClaimType> {
        ClaimType::ALL.into_iter().find(|c| c.key() == key)
    }

    /// The Windows ProgID SCR1B3 registers for this group under
    /// `HKCU\Software\Classes\<ProgID>`. One ProgID per group; every extension in
    /// [`windows_extensions`](Self::windows_extensions) points its
    /// `OpenWithProgids` at it.
    pub fn windows_progid(self) -> &'static str {
        match self {
            ClaimType::PlainText => "SCR1B3.txt",
            ClaimType::Markdown => "SCR1B3.md",
            ClaimType::Json => "SCR1B3.json",
            ClaimType::ConfigData => "SCR1B3.config",
            ClaimType::SourceCode => "SCR1B3.source",
        }
    }

    /// File extensions (NO leading dot) this group claims on Windows.
    ///
    /// Every extension belongs to EXACTLY ONE group — the Default-Apps
    /// `Capabilities\FileAssociations` key maps one `.ext` value name to one
    /// ProgID, so a duplicate across groups would silently let the later group
    /// overwrite the earlier one. `every_extension_belongs_to_exactly_one_group`
    /// pins that invariant.
    pub fn windows_extensions(self) -> &'static [&'static str] {
        match self {
            ClaimType::PlainText => &["txt", "text", "log", "nfo"],
            ClaimType::Markdown => &["md", "markdown", "mdown", "mkd", "mdx"],
            ClaimType::Json => &["json", "jsonc", "json5"],
            ClaimType::ConfigData => &[
                "toml",
                "yaml",
                "yml",
                "ini",
                "cfg",
                "conf",
                "csv",
                "tsv",
                "xml",
                "env",
                "properties",
                "editorconfig",
            ],
            ClaimType::SourceCode => &[
                "rs", "c", "h", "cpp", "cc", "cxx", "hpp", "hh", "py", "pyi", "js", "mjs", "cjs",
                "ts", "tsx", "jsx", "go", "java", "rb", "php", "sh", "bash", "zsh", "ps1", "bat",
                "cmd", "css", "scss", "sass", "less", "html", "htm", "vue", "svelte", "lua", "sql",
                "kt", "kts", "swift", "dart", "zig", "pl", "pm", "r", "jl", "ex", "exs", "erl",
                "hs", "scala", "clj", "vim", "asm", "s", "cmake", "gradle", "tf", "proto",
            ],
        }
    }

    /// macOS Uniform Type Identifiers this group claims.
    ///
    /// System UTIs resolve through the conformance tree, so TOML/YAML/INI need
    /// no UTI of their own — they conform to `public.plain-text`, already
    /// claimed by [`PlainText`](Self::PlainText). Only the formats with a
    /// distinct system UTI are named here.
    pub fn macos_utis(self) -> &'static [&'static str] {
        match self {
            ClaimType::PlainText => &["public.plain-text"],
            ClaimType::Markdown => &["net.daringfireball.markdown"],
            ClaimType::Json => &["public.json"],
            ClaimType::ConfigData => &["public.xml", "public.comma-separated-values-text"],
            ClaimType::SourceCode => &["public.source-code"],
        }
    }

    /// freedesktop MIME types this group claims on Linux.
    pub fn linux_mimes(self) -> &'static [&'static str] {
        match self {
            ClaimType::PlainText => &["text/plain"],
            ClaimType::Markdown => &["text/markdown"],
            ClaimType::Json => &["application/json"],
            ClaimType::ConfigData => &[
                "application/toml",
                "application/yaml",
                "text/x-yaml",
                "application/xml",
                "text/xml",
                "text/csv",
                "text/tab-separated-values",
            ],
            ClaimType::SourceCode => &[
                "text/x-csrc",
                "text/x-c++src",
                "text/x-chdr",
                "text/x-rust",
                "text/x-python",
                "application/javascript",
                "application/typescript",
                "text/x-go",
                "text/x-java-source",
                "text/x-php",
                "text/x-ruby",
                "application/x-shellscript",
                "application/x-perl",
                "text/css",
                "text/html",
                "text/x-lua",
                "application/sql",
            ],
        }
    }
}

/// A stable fingerprint of "the registry writes a registration would make":
/// the executable path plus the claimed group keys, in canonical order.
///
/// This is what makes the startup re-registration converge instead of
/// re-running every launch. SCR1B3 ships an in-app updater and a portable zip,
/// so the executable path legitimately MOVES — and every registered
/// `shell\open\command` still points at the old location until it is rewritten.
/// Comparing this fingerprint against the one stamped by the last SUCCESSFUL
/// registration detects exactly the two cases that need a rewrite (the exe
/// moved, or the user changed which types they claim) and nothing else.
pub fn registration_fingerprint(exe: &str, types: &[ClaimType]) -> String {
    let mut keys: Vec<&str> = ClaimType::ALL
        .into_iter()
        .filter(|c| types.contains(c))
        .map(|c| c.key())
        .collect();
    keys.dedup();
    format!("{}|{}", exe, keys.join(","))
}

/// OS-integration preferences (schema v4). DEFAULTS OFF — SCR1B3 never registers
/// itself as a file handler without an explicit user action in Settings (mirrors
/// the opt-in `reporting` contract: no surprise OS-surface changes). A config
/// written before v4 reads this whole section as the all-off default via
/// `#[serde(default)]`.
///
/// The derived `Default` IS the opt-in-off state (`register_file_types = false`,
/// no claimed types, never registered) — the privacy default the contract
/// requires; the field defaults are asserted by `integration_config_defaults_off`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct IntegrationConfig {
    /// The user asked SCR1B3 to register as a default file-type handler. Until
    /// this is set true (via Settings), no registration is ever performed.
    pub register_file_types: bool,
    /// Persisted [`ClaimType::key`] tokens the user opted to claim.
    ///
    /// `None` means UNSET (never chosen) and resolves to the full default set.
    /// `Some(vec![])` means the user explicitly cleared every box and resolves
    /// to NOTHING. Those two states are genuinely different and a bare `Vec`
    /// cannot tell them apart: while this was a `Vec<String>`, clearing the last
    /// checkbox persisted an empty list, which
    /// [`claimed_types`](Self::claimed_types) then resolved back to ALL — so the
    /// boxes silently re-checked themselves on the next frame and the user could
    /// not opt out of a group. See `clearing_every_box_means_none_not_all`.
    pub claimed_types: Option<Vec<String>>,
    /// Unix seconds of the last successful registration (for the Settings status
    /// line). `None` until the first successful register.
    pub last_registration_unix: Option<u64>,
    /// The document format the "Save As" dialog defaults a not-yet-saved buffer
    /// to (extension + primary filter). Defaults to Markdown — SCR1B3 is a
    /// note-first editor, so new files suggest `untitled.md`. `#[serde(default)]`
    /// means a config written before this field existed backfills to Markdown
    /// automatically (serde-default IS the migration — no schema bump needed).
    #[serde(default)]
    pub default_save_format: DefaultSaveFormat,
}

impl IntegrationConfig {
    /// The resolved set of claim groups: the persisted keys parsed back to
    /// [`ClaimType`]s, or — when the selection is UNSET (`None`) — the full
    /// default set. An explicitly EMPTY selection resolves to an empty set, not
    /// to everything. Unknown / stale keys are ignored (forward-compatible).
    /// Order follows [`ClaimType::ALL`] and is de-duplicated.
    pub fn claimed_types(&self) -> Vec<ClaimType> {
        let Some(keys) = self.claimed_types.as_ref() else {
            return ClaimType::ALL.to_vec();
        };
        ClaimType::ALL
            .into_iter()
            .filter(|c| keys.iter().any(|k| k == c.key()))
            .collect()
    }

    /// Persist an explicit selection (canonical order, de-duplicated). Always
    /// records `Some`, so clearing every box is stored as "none chosen" rather
    /// than collapsing back to the unset-means-all default.
    pub fn set_claimed_types(&mut self, types: &[ClaimType]) {
        self.claimed_types = Some(
            ClaimType::ALL
                .into_iter()
                .filter(|c| types.contains(c))
                .map(|c| c.key().to_string())
                .collect(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_roundtrips_for_every_group() {
        for c in ClaimType::ALL {
            assert_eq!(ClaimType::from_key(c.key()), Some(c), "key {:?}", c.key());
        }
        assert_eq!(ClaimType::from_key("nope"), None);
    }

    #[test]
    fn keys_and_progids_are_unique() {
        let keys: Vec<_> = ClaimType::ALL.iter().map(|c| c.key()).collect();
        let progids: Vec<_> = ClaimType::ALL.iter().map(|c| c.windows_progid()).collect();
        for i in 0..ClaimType::ALL.len() {
            for j in (i + 1)..ClaimType::ALL.len() {
                assert_ne!(keys[i], keys[j], "duplicate key");
                assert_ne!(progids[i], progids[j], "duplicate progid");
            }
        }
    }

    #[test]
    fn every_group_maps_to_non_empty_identifiers_on_every_os() {
        for c in ClaimType::ALL {
            assert!(!c.windows_extensions().is_empty(), "win ext {:?}", c);
            assert!(!c.macos_utis().is_empty(), "mac uti {:?}", c);
            assert!(!c.linux_mimes().is_empty(), "linux mime {:?}", c);
            assert!(c.windows_progid().starts_with("SCR1B3."), "progid {:?}", c);
            // Extensions carry no dot (the registry key is built as `.<ext>`).
            for e in c.windows_extensions() {
                assert!(!e.starts_with('.') && !e.is_empty(), "ext {e:?}");
            }
        }
    }

    #[test]
    fn txt_is_claimed_and_reaches_the_plain_text_progid() {
        // The user-reported symptom was ".txt is not available as a default".
        // `.txt` must be claimed, by exactly one group, with a ProgID — this is
        // the config-layer half of that contract (the registry-entry half is
        // `plain_text_registration_covers_dot_txt` in windows_entries).
        let owners: Vec<ClaimType> = ClaimType::ALL
            .into_iter()
            .filter(|c| c.windows_extensions().contains(&"txt"))
            .collect();
        assert_eq!(
            owners,
            vec![ClaimType::PlainText],
            "`.txt` is claimed by exactly one group: PlainText"
        );
        assert_eq!(ClaimType::PlainText.windows_progid(), "SCR1B3.txt");
    }

    #[test]
    fn every_extension_belongs_to_exactly_one_group() {
        // `Capabilities\FileAssociations` maps ONE `.ext` value name to ONE
        // ProgID. If two groups claimed the same extension the later write would
        // silently clobber the earlier, and unchecking one group would strip an
        // association the other group still believes it holds.
        let mut seen: Vec<(&str, ClaimType)> = Vec::new();
        for c in ClaimType::ALL {
            for e in c.windows_extensions() {
                if let Some((_, other)) = seen.iter().find(|(s, _)| s == e) {
                    panic!("extension .{e} claimed by both {other:?} and {c:?}");
                }
                seen.push((e, c));
            }
        }
        // Sanity: the roster genuinely grew past the original four-group set.
        assert!(
            seen.len() >= 60,
            "expected a broad extension roster, got {}",
            seen.len()
        );
    }

    #[test]
    fn every_linux_mime_belongs_to_exactly_one_group() {
        let mut seen: Vec<(&str, ClaimType)> = Vec::new();
        for c in ClaimType::ALL {
            for m in c.linux_mimes() {
                if let Some((_, other)) = seen.iter().find(|(s, _)| s == m) {
                    panic!("mime {m} claimed by both {other:?} and {c:?}");
                }
                seen.push((m, c));
            }
        }
    }

    #[test]
    fn the_requested_config_and_data_formats_are_all_claimed() {
        // The concrete set the user asked for, each pinned to a real group.
        for ext in [
            "txt", "log", "md", "markdown", "json", "toml", "yaml", "yml", "ini", "csv", "xml",
        ] {
            assert!(
                ClaimType::ALL
                    .into_iter()
                    .any(|c| c.windows_extensions().contains(&ext)),
                ".{ext} is not claimed by any group"
            );
        }
    }

    #[test]
    fn extension_summary_is_derived_from_the_real_extension_list() {
        // Derived, never hand-written — so the Settings line cannot claim a set
        // the registration does not write.
        let s = ClaimType::PlainText.extension_summary();
        assert!(s.starts_with(".txt"), "summary leads with .txt: {s}");
        // PlainText has 4 extensions ⇒ shown in full, no "+N more".
        assert!(!s.contains("more"), "no overflow for a short list: {s}");
        // SourceCode is long ⇒ truncated with an honest remainder count.
        let src = ClaimType::SourceCode.extension_summary();
        let expected_more = ClaimType::SourceCode.windows_extensions().len() - 4;
        assert!(
            src.contains(&format!("+{expected_more} more")),
            "summary reports the real remainder: {src}"
        );
    }

    #[test]
    fn registration_fingerprint_changes_with_the_exe_path_and_the_claim_set() {
        let a = registration_fingerprint(r"C:\Old\scr1b3.exe", &[ClaimType::PlainText]);
        // Same inputs ⇒ same fingerprint (so startup converges instead of
        // re-registering every launch).
        assert_eq!(
            a,
            registration_fingerprint(r"C:\Old\scr1b3.exe", &[ClaimType::PlainText])
        );
        // A MOVED exe (in-app update / portable-zip relocation) ⇒ different,
        // which is what forces the stale `shell\open\command` to be rewritten.
        assert_ne!(
            a,
            registration_fingerprint(r"C:\New\scr1b3.exe", &[ClaimType::PlainText])
        );
        // A changed claim set ⇒ different.
        assert_ne!(
            a,
            registration_fingerprint(
                r"C:\Old\scr1b3.exe",
                &[ClaimType::PlainText, ClaimType::Markdown]
            )
        );
        // Order-insensitive: the same set in any order is the same fingerprint.
        assert_eq!(
            registration_fingerprint(
                r"C:\Old\scr1b3.exe",
                &[ClaimType::Markdown, ClaimType::PlainText]
            ),
            registration_fingerprint(
                r"C:\Old\scr1b3.exe",
                &[ClaimType::PlainText, ClaimType::Markdown]
            )
        );
    }

    #[test]
    fn integration_config_defaults_off() {
        let c = IntegrationConfig::default();
        assert!(!c.register_file_types);
        assert!(c.claimed_types.is_none(), "selection starts UNSET");
        assert!(c.last_registration_unix.is_none());
    }

    #[test]
    fn unset_claim_list_resolves_to_the_full_default_set() {
        let c = IntegrationConfig::default();
        assert_eq!(c.claimed_types.as_ref(), None);
        assert_eq!(c.claimed_types(), ClaimType::ALL.to_vec());
    }

    #[test]
    fn clearing_every_box_means_none_not_all() {
        // The bug this pins: unchecking every group used to persist an empty
        // list, which resolved back to ALL — so the boxes silently re-checked
        // themselves and the user could not opt out. UNSET (None) still means
        // "the default set"; an explicit empty selection means NOTHING.
        let mut c = IntegrationConfig::default();
        c.set_claimed_types(&[]);
        assert_eq!(
            c.claimed_types,
            Some(Vec::new()),
            "an explicit empty selection persists as Some([]), not None"
        );
        assert!(
            c.claimed_types().is_empty(),
            "an explicitly empty selection resolves to NO groups, never to ALL"
        );
        // …and it survives the round-trip through TOML, which is where the
        // silent re-check happened (the empty list read back as unset).
        let back: IntegrationConfig = toml::from_str(&toml::to_string(&c).unwrap()).unwrap();
        assert!(
            back.claimed_types().is_empty(),
            "empty stays empty on reload"
        );
    }

    #[test]
    fn set_claimed_types_normalises_to_canonical_order() {
        let mut c = IntegrationConfig::default();
        // Reverse order, with a duplicate.
        c.set_claimed_types(&[
            ClaimType::SourceCode,
            ClaimType::PlainText,
            ClaimType::PlainText,
        ]);
        assert_eq!(
            c.claimed_types,
            Some(vec!["plain_text".to_string(), "source_code".to_string()]),
            "stored in ClaimType::ALL order, de-duplicated"
        );
        assert_eq!(
            c.claimed_types(),
            vec![ClaimType::PlainText, ClaimType::SourceCode]
        );
    }

    #[test]
    fn explicit_claim_list_resolves_in_canonical_order_ignoring_unknown() {
        let c = IntegrationConfig {
            register_file_types: true,
            // out of order + an unknown key
            claimed_types: Some(vec!["json".into(), "bogus".into(), "plain_text".into()]),
            last_registration_unix: None,
            default_save_format: DefaultSaveFormat::Markdown,
        };
        assert_eq!(
            c.claimed_types(),
            vec![ClaimType::PlainText, ClaimType::Json],
            "resolves in ClaimType::ALL order, unknown keys dropped"
        );
    }

    #[test]
    fn integration_config_toml_roundtrip() {
        let c = IntegrationConfig {
            register_file_types: true,
            claimed_types: Some(vec!["plain_text".into(), "markdown".into()]),
            last_registration_unix: Some(1_700_000_000),
            default_save_format: DefaultSaveFormat::PlainText,
        };
        let s = toml::to_string(&c).unwrap();
        let back: IntegrationConfig = toml::from_str(&s).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn default_save_format_defaults_to_markdown() {
        // A fresh format value is Markdown, and Markdown's extension is `md`.
        assert_eq!(DefaultSaveFormat::default(), DefaultSaveFormat::Markdown);
        assert_eq!(DefaultSaveFormat::default().extension(), "md");
        // And it is the default on a fresh IntegrationConfig too.
        assert_eq!(
            IntegrationConfig::default().default_save_format,
            DefaultSaveFormat::Markdown
        );
    }

    #[test]
    fn existing_config_without_the_key_backfills_to_markdown() {
        // A config (here, the IntegrationConfig section) written before this
        // field existed omits `default_save_format` entirely. Serde-default is
        // the migration: it backfills to Markdown with NO schema bump.
        let older = "register_file_types = true\n";
        let cfg: IntegrationConfig = toml::from_str(older).unwrap();
        assert_eq!(cfg.default_save_format, DefaultSaveFormat::Markdown);
        // Even a totally empty section backfills to Markdown.
        let empty: IntegrationConfig = toml::from_str("").unwrap();
        assert_eq!(empty.default_save_format, DefaultSaveFormat::Markdown);
    }

    #[test]
    fn suggested_file_name_follows_the_configured_format() {
        // Save-path contract, unit-tested at the pure-helper layer (the rfd
        // dialog can't be driven headlessly): an untitled buffer suggests
        // `untitled.<ext>`, and switching the format switches the extension.
        assert_eq!(
            DefaultSaveFormat::Markdown.suggested_file_name(None),
            "untitled.md"
        );
        assert_eq!(
            DefaultSaveFormat::PlainText.suggested_file_name(None),
            "untitled.txt"
        );
        // A blank / whitespace stem also falls back to `untitled`.
        assert_eq!(
            DefaultSaveFormat::Markdown.suggested_file_name(Some("   ")),
            "untitled.md"
        );
        // A named buffer keeps its stem, gaining the configured extension.
        assert_eq!(
            DefaultSaveFormat::Markdown.suggested_file_name(Some("notes")),
            "notes.md"
        );
        assert_eq!(
            DefaultSaveFormat::PlainText.suggested_file_name(Some("notes")),
            "notes.txt"
        );
    }

    #[test]
    fn suggested_name_follows_config_field_end_to_end() {
        // Drive the suggested name straight off a Config's integration field,
        // proving the Settings choice reaches the save dialog's pre-fill.
        use crate::config::Config;
        let mut c = Config::default();
        assert_eq!(
            c.integration.default_save_format.suggested_file_name(None),
            "untitled.md",
            "the default config suggests a Markdown name"
        );
        c.integration.default_save_format = DefaultSaveFormat::PlainText;
        assert_eq!(
            c.integration.default_save_format.suggested_file_name(None),
            "untitled.txt",
            "switching the config to Plain Text switches the suggestion"
        );
        // And it survives a TOML round-trip (persisted like every other setting).
        let back: Config = toml::from_str(&c.to_toml_string()).expect("config round-trip");
        assert_eq!(
            back.integration.default_save_format,
            DefaultSaveFormat::PlainText
        );
    }

    #[test]
    fn ensure_extension_appends_only_when_missing() {
        // No extension → the configured default is appended.
        assert_eq!(
            ensure_extension(Path::new("notes"), "md"),
            PathBuf::from("notes.md")
        );
        assert_eq!(
            ensure_extension(Path::new("notes"), "txt"),
            PathBuf::from("notes.txt")
        );
        // An explicit DIFFERENT extension the user typed on purpose is respected.
        assert_eq!(
            ensure_extension(Path::new("notes.txt"), "md"),
            PathBuf::from("notes.txt")
        );
        // A matching extension is likewise left untouched (idempotent).
        assert_eq!(
            ensure_extension(Path::new("notes.md"), "md"),
            PathBuf::from("notes.md")
        );
        // A path with directories is preserved; only the leaf gains the ext.
        assert_eq!(
            ensure_extension(Path::new("/tmp/sub/report"), "md"),
            PathBuf::from("/tmp/sub/report.md")
        );
    }

    #[test]
    fn claim_type_labels_are_distinct_and_name_their_group() {
        // The Settings checklist renders one row per group from `label()`.
        // Nothing asserted it, so a blank or single-constant label produced five
        // identical rows with no way to tell which checkbox toggles what.
        for c in ClaimType::ALL {
            assert!(!c.label().is_empty(), "empty label for {c:?}");
        }
        let labels: Vec<&str> = ClaimType::ALL.iter().map(|c| c.label()).collect();
        for i in 0..labels.len() {
            for j in (i + 1)..labels.len() {
                assert_ne!(labels[i], labels[j], "duplicate ClaimType label");
            }
        }
        // Pin the user-visible text so a label cannot become a placeholder.
        assert_eq!(ClaimType::Markdown.label(), "Markdown");
        assert_eq!(ClaimType::Json.label(), "JSON");
    }

    #[test]
    fn macos_utis_are_reverse_dns_and_unique_per_group() {
        // `macos_utis` was only ever checked for non-emptiness, so any
        // placeholder string passed. A UTI is a reverse-DNS identifier and — like
        // the Windows extensions and Linux MIME types — must belong to exactly
        // one group, or one group's registration silently shadows another's.
        let mut seen: Vec<(&str, ClaimType)> = Vec::new();
        for c in ClaimType::ALL {
            for u in c.macos_utis() {
                assert!(
                    u.contains('.') && !u.starts_with('.') && !u.ends_with('.'),
                    "UTI {u:?} for {c:?} is not a reverse-DNS identifier"
                );
                if let Some((_, other)) = seen.iter().find(|(s, _)| s == u) {
                    panic!("UTI {u} claimed by both {other:?} and {c:?}");
                }
                seen.push((u, c));
            }
        }
        // The two anchor UTIs the plain-text / Markdown claims depend on.
        assert!(
            ClaimType::PlainText
                .macos_utis()
                .contains(&"public.plain-text"),
            "PlainText must claim the system plain-text UTI"
        );
        assert!(
            ClaimType::Markdown
                .macos_utis()
                .contains(&"net.daringfireball.markdown"),
            "Markdown must claim the system Markdown UTI"
        );
    }

    #[test]
    fn save_format_labels_are_distinct_and_carry_the_extension() {
        // `filter_label` names the Save-As filter and `ui_label` the Settings
        // dropdown row. Both were only checked for non-emptiness, so a single
        // constant made the two formats indistinguishable in the UI.
        let filters: Vec<&str> = DefaultSaveFormat::ALL
            .iter()
            .map(|f| f.filter_label())
            .collect();
        let uis: Vec<&str> = DefaultSaveFormat::ALL
            .iter()
            .map(|f| f.ui_label())
            .collect();
        assert_ne!(filters[0], filters[1], "filter labels must differ");
        assert_ne!(uis[0], uis[1], "ui labels must differ");
        // The dropdown row names the extension it will actually write.
        for f in DefaultSaveFormat::ALL {
            assert!(
                f.ui_label().contains(&format!(".{}", f.extension())),
                "ui_label {:?} must name its extension",
                f.ui_label()
            );
        }
        assert_eq!(DefaultSaveFormat::Markdown.filter_label(), "Markdown");
        assert_eq!(DefaultSaveFormat::PlainText.filter_label(), "Plain Text");
    }

    #[test]
    fn format_labels_are_distinct_and_nonempty() {
        for f in DefaultSaveFormat::ALL {
            assert!(!f.extension().is_empty());
            assert!(!f.filter_label().is_empty());
            assert!(!f.ui_label().is_empty());
        }
        assert_ne!(
            DefaultSaveFormat::Markdown.extension(),
            DefaultSaveFormat::PlainText.extension()
        );
        assert_eq!(DefaultSaveFormat::ALL.len(), 2);
    }
}
