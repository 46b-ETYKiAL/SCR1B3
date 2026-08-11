//! Shared source scanner for the "nothing is silently dormant" guards.
//!
//! Two integration tests need the same thing: a corpus of **production Rust
//! code** for `scribe-app` + `scribe-core`, with comments, string literals and
//! test code removed, so that "does anything read / call this?" can be answered
//! honestly.
//!
//! - `settings_wiring_guard.rs` — is every Settings control and every
//!   `scr1b3.toml` section actually consumed?
//! - `public_api_dormancy.rs` — does every entry in the dormant-API ledger still
//!   have zero production callers?
//!
//! Both live in `scribe-app/tests/` because that is the only test directory that
//! sits above BOTH crate source roots; a `scribe-core` API can be called from
//! either crate, so a corpus that omitted `scribe-app/src` could report a live
//! API as dormant.
//!
//! # Why raw `contains` is not good enough
//!
//! The guard this module replaced was `src.contains(field)` over the raw
//! concatenation of every `.rs` file. That corpus included **comments** (a field
//! named in a `//` note read as "wired"), **string literals**, and **all test
//! code** — `e2e.rs`, every `qa_*_tests.rs`, and 184 inline `#[cfg(test)]`
//! modules. So the three cheapest ways to look wired — mention it in a doc
//! comment, mention it in a test, or put it in a string — all passed, and a
//! guard that had degraded to `true` for every input would have looked identical
//! to a working one. Everything below exists to make those three stop counting.

// Each integration-test crate that includes this module uses a different subset
// of it; the unused remainder is not dead code in the project, only in that one
// crate's view.
#![allow(dead_code)]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Lexical stripping
// ---------------------------------------------------------------------------

pub fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Blank `ch[from..to]`, preserving newlines so a stripped span never joins two
/// lines into one token.
fn blank_range(ch: &[char], from: usize, to: usize, out: &mut Vec<char>) {
    for &c in &ch[from..to.min(ch.len())] {
        out.push(if c == '\n' { '\n' } else { ' ' });
    }
}

/// End (exclusive) of the normal string literal opening at `i` (`ch[i] == '"'`).
fn string_end(ch: &[char], i: usize) -> usize {
    let n = ch.len();
    let mut j = i + 1;
    while j < n {
        match ch[j] {
            '\\' => j += 2,
            '"' => return j + 1,
            _ => j += 1,
        }
    }
    n
}

/// End (exclusive) of the raw string opening at `i`, if one opens there.
/// Handles `r"…"`, `r#"…"#`, `r##"…"##` and the byte forms `br"…"` / `br#"…"#`.
fn raw_string_end(ch: &[char], i: usize) -> Option<usize> {
    let n = ch.len();
    let mut j = i;
    if ch[j] == 'b' {
        j += 1;
        if j >= n || ch[j] != 'r' {
            return None;
        }
    }
    if ch[j] != 'r' {
        return None;
    }
    j += 1;
    let mut hashes = 0usize;
    while j < n && ch[j] == '#' {
        hashes += 1;
        j += 1;
    }
    if j >= n || ch[j] != '"' {
        return None;
    }
    j += 1;
    while j < n {
        if ch[j] == '"' {
            let mut k = j + 1;
            let mut seen = 0usize;
            while k < n && seen < hashes && ch[k] == '#' {
                seen += 1;
                k += 1;
            }
            if seen == hashes {
                return Some(k);
            }
        }
        j += 1;
    }
    Some(n)
}

/// End (exclusive) of the char literal opening at `i`, or `None` when the `'` is
/// a lifetime (`&'a str`) rather than a literal.
fn char_literal_end(ch: &[char], i: usize) -> Option<usize> {
    let n = ch.len();
    if i + 1 >= n {
        return None;
    }
    if ch[i + 1] == '\\' {
        let mut j = i + 2;
        while j < n && ch[j] != '\'' {
            j += 1;
        }
        return if j < n { Some(j + 1) } else { None };
    }
    if i + 2 < n && ch[i + 2] == '\'' {
        return Some(i + 3);
    }
    None
}

/// Blank every comment and literal in `src`, leaving only code. Newlines survive.
///
/// This is what makes a mention in a `//` note, a `///` rustdoc, or a `"string"`
/// stop counting as evidence that something is read or called.
pub fn strip_comments_and_literals(src: &str) -> String {
    let ch: Vec<char> = src.chars().collect();
    let n = ch.len();
    let mut out: Vec<char> = Vec::with_capacity(n);
    let mut i = 0usize;
    while i < n {
        let c = ch[i];
        // Line comment (covers `//`, `///`, `//!`).
        if c == '/' && i + 1 < n && ch[i + 1] == '/' {
            while i < n && ch[i] != '\n' {
                out.push(' ');
                i += 1;
            }
            continue;
        }
        // Block comment — Rust nests them.
        if c == '/' && i + 1 < n && ch[i + 1] == '*' {
            let mut depth = 1usize;
            out.push(' ');
            out.push(' ');
            i += 2;
            while i < n && depth > 0 {
                if ch[i] == '/' && i + 1 < n && ch[i + 1] == '*' {
                    depth += 1;
                    out.push(' ');
                    out.push(' ');
                    i += 2;
                } else if ch[i] == '*' && i + 1 < n && ch[i + 1] == '/' {
                    depth -= 1;
                    out.push(' ');
                    out.push(' ');
                    i += 2;
                } else {
                    out.push(if ch[i] == '\n' { '\n' } else { ' ' });
                    i += 1;
                }
            }
            continue;
        }
        // Raw / byte-raw string. The `r` must open a token, not end an identifier.
        if (c == 'r' || c == 'b') && (i == 0 || !is_ident_char(ch[i - 1])) {
            if let Some(end) = raw_string_end(&ch, i) {
                blank_range(&ch, i, end, &mut out);
                i = end;
                continue;
            }
        }
        if c == '"' {
            let end = string_end(&ch, i);
            blank_range(&ch, i, end, &mut out);
            i = end;
            continue;
        }
        if c == '\'' {
            if let Some(end) = char_literal_end(&ch, i) {
                blank_range(&ch, i, end, &mut out);
                i = end;
                continue;
            }
            // Otherwise a lifetime — real code, keep it.
        }
        out.push(c);
        i += 1;
    }
    out.into_iter().collect()
}

// ---------------------------------------------------------------------------
// `#[cfg(test)]` stripping
// ---------------------------------------------------------------------------

/// The ONLY test-only `cfg` form this scanner strips.
///
/// The tree also carries `#[cfg(not(test))]` (production-only — must be KEPT)
/// and `#[cfg(any(test, windows))]` / `#[cfg(any(test, fuzzing))]` (enabled in a
/// real build — also KEPT). Matching this exact spelling is what keeps those
/// three cases correct; `no_unhandled_test_only_cfg_form` in
/// `settings_wiring_guard.rs` fails if a new test-only spelling ever appears and
/// would otherwise slip past.
pub const CFG_TEST: &str = "#[cfg(test)]";

/// Index just past the `]` that closes the bracket opening at `open`.
pub fn bracket_end(ch: &[char], open: usize) -> usize {
    let n = ch.len();
    let mut depth = 0usize;
    let mut j = open;
    while j < n {
        match ch[j] {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return j + 1;
                }
            }
            _ => {}
        }
        j += 1;
    }
    n
}

/// Index just past the `}` that closes the brace opening at `open`.
fn brace_end(ch: &[char], open: usize) -> usize {
    let n = ch.len();
    let mut depth = 0usize;
    let mut j = open;
    while j < n {
        match ch[j] {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return j + 1;
                }
            }
            _ => {}
        }
        j += 1;
    }
    n
}

/// End (exclusive) of the item the attribute ending at `from` applies to.
///
/// Skips any further `#[…]` attributes, then takes the item as ending at the
/// first top-level `;` (`mod foo;`, `const X: u8 = 1;`) or at the close of the
/// first top-level `{…}` block (`mod foo { … }`, `fn f() { … }`).
fn cfg_test_item_end(ch: &[char], from: usize) -> usize {
    let n = ch.len();
    let mut j = from;
    // Further attributes stacked on the same item.
    loop {
        while j < n && ch[j].is_whitespace() {
            j += 1;
        }
        if j + 1 < n && ch[j] == '#' && ch[j + 1] == '[' {
            j = bracket_end(ch, j + 1);
            continue;
        }
        break;
    }
    let mut depth = 0usize;
    while j < n {
        match ch[j] {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            ';' if depth == 0 => return j + 1,
            '{' if depth == 0 => return brace_end(ch, j),
            _ => {}
        }
        j += 1;
    }
    n
}

/// Blank every `#[cfg(test)]` item in already-literal-stripped `src`.
///
/// This is what makes a mention inside a unit test stop counting as evidence
/// that production code reads a field or calls a function.
pub fn strip_cfg_test_items(src: &str) -> String {
    let ch: Vec<char> = src.chars().collect();
    let pat: Vec<char> = CFG_TEST.chars().collect();
    let n = ch.len();
    let mut out: Vec<char> = Vec::with_capacity(n);
    let mut i = 0usize;
    while i < n {
        if ch[i] == '#' && ch[i..].starts_with(&pat[..]) {
            let end = cfg_test_item_end(&ch, i + pat.len());
            blank_range(&ch, i, end, &mut out);
            i = end;
            continue;
        }
        out.push(ch[i]);
        i += 1;
    }
    out.into_iter().collect()
}

/// Module names declared `#[cfg(test)] mod NAME;` — i.e. whole FILES that exist
/// only for tests. Must be read BEFORE [`strip_cfg_test_items`] removes the
/// declarations.
pub fn test_module_names(stripped: &str) -> Vec<String> {
    let mut names = Vec::new();
    let ch: Vec<char> = stripped.chars().collect();
    let pat: Vec<char> = CFG_TEST.chars().collect();
    let n = ch.len();
    let mut i = 0usize;
    while i < n {
        if ch[i] == '#' && ch[i..].starts_with(&pat[..]) {
            let mut j = i + pat.len();
            while j < n && ch[j].is_whitespace() {
                j += 1;
            }
            let rest: String = ch[j..n.min(j + 4)].iter().collect();
            if rest.starts_with("mod ") {
                j += 4;
                while j < n && ch[j].is_whitespace() {
                    j += 1;
                }
                let mut name = String::new();
                while j < n && is_ident_char(ch[j]) {
                    name.push(ch[j]);
                    j += 1;
                }
                while j < n && ch[j].is_whitespace() {
                    j += 1;
                }
                // `mod name;` is a whole test-only file; `mod name {` is inline
                // and already removed by `strip_cfg_test_items`.
                if j < n && ch[j] == ';' && !name.is_empty() {
                    names.push(name);
                }
            }
            i += pat.len();
            continue;
        }
        i += 1;
    }
    names
}

/// Every `#[cfg(…)]` attribute body in `src` (literal-stripped), whitespace
/// removed — e.g. `cfg(test)`, `cfg(not(test))`, `cfg(any(test,windows))`.
pub fn cfg_attributes(src: &str) -> Vec<String> {
    let ch: Vec<char> = src.chars().collect();
    let n = ch.len();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 1 < n {
        if ch[i] == '#' && ch[i + 1] == '[' {
            let end = bracket_end(&ch, i + 1);
            let inner_end = end.saturating_sub(1).max(i + 2);
            let body: String = ch[i + 2..inner_end]
                .iter()
                .filter(|c| !c.is_whitespace())
                .collect();
            if body.starts_with("cfg(") {
                out.push(body);
            }
            i = end;
            continue;
        }
        i += 1;
    }
    out
}

// ---------------------------------------------------------------------------
// Corpus assembly
// ---------------------------------------------------------------------------

/// How a corpus request narrows the source tree.
#[derive(Debug, Clone, Default)]
pub struct CorpusOptions {
    /// Drop `scribe-core/src/config/**`. The config module DEFINES the sections,
    /// so it can never be evidence that anything CONSUMES them.
    pub skip_config_dir: bool,
    /// Extra file names to drop, e.g. the file that DEFINES the symbol being
    /// searched for (otherwise `pub fn replace_all` matches the `replace_all`
    /// probe and every dormant function looks called).
    pub skip_file_names: Vec<String>,
}

/// The two crate source roots the guards scan.
fn source_roots() -> [String; 2] {
    let manifest = env!("CARGO_MANIFEST_DIR");
    [
        format!("{manifest}/src"),
        format!("{manifest}/../scribe-core/src"),
    ]
}

fn walk(dir: &Path, skip_config_dir: bool, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    // Deterministic order so a failure message is reproducible.
    entries.sort();
    for p in entries {
        if p.is_dir() {
            if skip_config_dir && p.file_name().is_some_and(|nm| nm == "config") {
                continue;
            }
            walk(&p, skip_config_dir, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// Every `.rs` file under both crate roots, comment/literal-stripped, with its
/// path. No test-code stripping and no exclusions — the raw stripped tree.
pub fn stripped_files(skip_config_dir: bool) -> Vec<(PathBuf, String)> {
    let mut paths = Vec::new();
    for dir in source_roots() {
        walk(Path::new(&dir), skip_config_dir, &mut paths);
    }
    paths
        .into_iter()
        .filter_map(|p| {
            fs::read_to_string(&p)
                .ok()
                .map(|c| (p, strip_comments_and_literals(&c)))
        })
        .collect()
}

/// `true` for a file that exists only to hold tests: its module was declared
/// `#[cfg(test)] mod NAME;`, or it is the settings UI, or the caller asked for it
/// to be skipped.
pub fn is_excluded(path: &Path, test_mods: &BTreeSet<String>, skip_names: &[String]) -> bool {
    let file_name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    // The settings UI RENDERS the control; reading a field there is what makes
    // it a control, not what makes it wired.
    //
    // Keyed on the whole settings MODULE, not the single file name: the panel is
    // being decomposed from one 4.5k-line `settings.rs` into per-page siblings
    // under `settings/`, and those pages are the same UI doing the same thing.
    // A name-only key would let `settings/appearance.rs` re-enter the corpus the
    // moment it was created, where its `config.appearance.*` reads would count
    // as a runtime consumer — and a control that NOTHING outside settings reads
    // would start looking wired. That failure is silent and permanent: the guard
    // keeps passing while it has stopped guarding. Widening here is the safe
    // direction (it can only make MORE fields look unwired, which fails loudly);
    // narrowing is what kills the check.
    // Matched on the ANCESTOR chain, not just the immediate parent, so a page
    // later nested one level deeper cannot quietly re-enter the corpus. Taking
    // `parent()` first means a FILE called `settings` is not caught by this arm.
    let in_settings_module = file_name == "settings.rs"
        || path
            .parent()
            .is_some_and(|p| p.components().any(|c| c.as_os_str() == "settings"));
    if in_settings_module || skip_names.contains(&file_name) {
        return true;
    }
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    if test_mods.contains(&stem) {
        return true;
    }
    // `#[cfg(test)] mod foo;` can also resolve to `foo/mod.rs`, and any file
    // beneath such a directory is equally test-only.
    path.components().any(|c| {
        c.as_os_str()
            .to_str()
            .is_some_and(|s| test_mods.contains(s))
    })
}

/// Production (non-test, code-only) source of both crates, concatenated.
pub fn production_source(opts: &CorpusOptions) -> String {
    // Pass 1 — discover test-only modules. This must see the whole tree,
    // including the config module, because a `#[cfg(test)] mod x;` there still
    // names a test-only file.
    let all = stripped_files(false);
    let mut test_mods = BTreeSet::new();
    for (_, src) in &all {
        for name in test_module_names(src) {
            test_mods.insert(name);
        }
    }
    // Pass 2 — concatenate the surviving files with their `#[cfg(test)]` items
    // removed.
    let files = if opts.skip_config_dir {
        stripped_files(true)
    } else {
        all
    };
    let mut out = String::new();
    for (path, src) in files {
        if is_excluded(&path, &test_mods, &opts.skip_file_names) {
            continue;
        }
        out.push_str(&strip_cfg_test_items(&src));
        out.push('\n');
    }
    out
}

/// Convenience: production source of both crates with the config module kept.
///
/// `scribe-core/src/config/**` stays in scope for FIELD probes: the dotted
/// `section.field` form they look for never appears in a struct definition, so
/// finding one there still proves a real read.
pub fn runtime_source() -> String {
    production_source(&CorpusOptions::default())
}

/// Production source EXCLUDING the config module, for section-level probes.
///
/// A section probe (`.keybindings`) matches the field DECLARATION in
/// `config/mod.rs`, which would make an entirely unread section look consumed.
/// Dropping the config module is what makes that guard honest.
pub fn runtime_source_outside_config() -> String {
    production_source(&CorpusOptions {
        skip_config_dir: true,
        skip_file_names: Vec::new(),
    })
}

// ---------------------------------------------------------------------------
// Token search
// ---------------------------------------------------------------------------

/// `true` when `needle` occurs in `src` bounded by non-identifier characters on
/// both sides — so `editor.tab_width` is not satisfied by `editor.tab_width_px`,
/// and `ui_scale` is not satisfied by `effective_ui_scale`.
///
/// A boundary is only required on an END that could actually run into an
/// identifier. When the needle already STARTS with a non-identifier character —
/// the `.section` field-access form — it carries its own left boundary, and
/// demanding another one is not just redundant, it is wrong: `.appearance` in
/// `self.config.appearance.theme` is preceded by the `g` of `config`, so the
/// two-sided rule rejected a real read and reported the whole `[appearance]`
/// section as a dead config surface.
///
/// That made the section guard silently dependent on rustfmt: `.editor` matched
/// only where a long chain happened to be broken across lines, putting
/// whitespace before the dot. Sections were passing by luck of formatting, not
/// because they were read — the same verdict for the wrong reason.
pub fn contains_token(src: &str, needle: &str) -> bool {
    let needs_left = needle.chars().next().is_some_and(is_ident_char);
    let needs_right = needle.chars().next_back().is_some_and(is_ident_char);
    let mut from = 0usize;
    while let Some(rel) = src[from..].find(needle) {
        let start = from + rel;
        let end = start + needle.len();
        let before_ok = !needs_left
            || src[..start]
                .chars()
                .next_back()
                .is_none_or(|c| !is_ident_char(c));
        let after_ok = !needs_right || src[end..].chars().next().is_none_or(|c| !is_ident_char(c));
        if before_ok && after_ok {
            return true;
        }
        from = start + 1;
    }
    false
}
