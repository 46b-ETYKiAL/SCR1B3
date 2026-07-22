//! Note-app foundation: pure, local-only parsers for a first-party PKM layer.
//!
//! These submodules are the untrusted-input-safe substrate the editor's note
//! features build on. Everything here is pure string/`Path` logic — no I/O, no
//! network, no telemetry — so it is exhaustively unit-testable and cannot leak.
//!
//! - [`wikilink`] — `[[wiki-link]]` extraction over note content.
//! - [`tags`] — inline `#tag` extraction, nesting-aware.
//! - [`frontmatter`] — minimal YAML frontmatter block extraction.
//! - [`vault_path`] — vault-relative path safety: a wiki-link / tag / title is
//!   UNTRUSTED, so every filesystem target it produces is rejected for traversal
//!   (`..`, absolute paths, drive letters) and confined to the vault before use.
//!
//! Wiki-link and tag targets MUST pass through [`vault_path::sanitize_relative`]
//! (or [`vault_path::resolve_in_vault`]) before touching the filesystem — the
//! parsers deliberately return raw strings so the safety gate is a single,
//! unavoidable chokepoint rather than something each call site re-implements.

pub mod frontmatter;
pub mod meta;
pub mod query;
pub mod tags;
pub mod vault_path;
pub mod wikilink;
