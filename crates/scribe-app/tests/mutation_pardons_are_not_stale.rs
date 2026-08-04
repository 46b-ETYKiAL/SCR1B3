//! A mutation pardon may not be anchored on a line number.
//!
//! `.cargo/mutants.toml` suppresses individual mutants with `exclude_re`, matched
//! against the mutant name exactly as `cargo mutants --list` prints it:
//! `path/to/file.rs:LINE:COL: <description>`. Anchoring on the LINE is the bug
//! this test exists to make impossible. A line number is not a stable key: any
//! edit above it slides the anchor onto a different statement, and the pardon
//! then suppresses the WRONG mutant — or nothing at all — while still looking
//! like coverage. It fails silently, in the worst direction.
//!
//! That is not hypothetical, and it is not rare. `app/text_ops_methods.rs:418`
//! pardoned `duplicate_cursor_line`'s `insert(ln + 1, copy)`; a refactor pushed
//! that statement to 423 and the anchor landed on a comment. Two siblings in the
//! same file (360, 799) had drifted the same way, a pair in `drag_scroll.rs` had
//! come to rest on a blank line and a doc comment, and `md_preview.rs:279` slid
//! onto `None => MdBlock::ListItem {` while `:289` slid onto a doc comment. The
//! migration pass that wrote this test then found EIGHT more anchors matching
//! zero mutants, and one — `app/drag_scroll.rs:189`, written for
//! `edge_autoscroll_step` — silently pardoning an unrelated `page_key_assist`
//! mutant instead.
//!
//! The earlier version of this test only flagged an anchor that had rotated onto
//! a COMMENT or a BLANK line. That caught `md_preview.rs:289` and missed
//! `md_preview.rs:279`, which had come to rest on real code — equally broken,
//! invisible to a comment check. Detecting rotation after the fact cannot work.
//! So this test no longer tries: it REJECTS the anchoring style outright.
//!
//! The supported anchors, in order of preference:
//!
//!   1. The mutant DESCRIPTION, e.g.
//!      `'replace \+ with \* in ScribeApp::duplicate_cursor_line'`. It cannot
//!      rotate.
//!   2. Where a bare description would also match a NEIGHBOUR the tests actually
//!      kill, the mutant's COLUMN with the line left as a wildcard, e.g.
//!      `'find_in_files\.rs:\d+:32: replace > with (==|<|>=) in walk_project'`.
//!      A column does not move when something above it is edited, so the
//!      rotation class stays closed while the pardon stays exactly as tight.
//!
//! Get the authoritative description from the tool, never from memory:
//!
//! ```text
//! cargo mutants --list --no-config --line-col true -f crates/<crate>/src/<file>.rs
//! ```
//!
//! A tiny frozen allowlist below carries the only three anchors for which
//! NEITHER form works, because the pardoned mutant is name- and column-identical
//! to a tested neighbour in the same function. Those three still get the
//! strongest check available: the line they name must still contain a recorded
//! source token, so a rotation onto real-but-wrong code is caught too.

use std::path::{Path, PathBuf};

/// The only literal-line anchors `.cargo/mutants.toml` may carry.
///
/// `(exact pattern, partial path, line, token that line must still contain)`.
/// Adding a fourth requires proving the same impossibility: that both the
/// mutant's description and its column are shared with a mutant that the test
/// suite kills, so any non-line anchor would over-suppress. Over-suppression is
/// worse than rotation — it deletes coverage silently.
const FROZEN_LINE_ANCHORS: &[(&str, &str, usize, &str)] = &[
    // `is_active() || !secondaries.is_empty()`: the `||` on the NEXT line is at
    // the same column with the same description, and IS tested.
    (
        r"app/multi_cursor_glue\.rs:77:13:",
        "app/multi_cursor_glue.rs",
        77,
        "!self.multi_cursor.secondaries()",
    ),
    (
        r"app/multi_cursor_glue\.rs:94:13:",
        "app/multi_cursor_glue.rs",
        94,
        "!self.multi_cursor.secondaries()",
    ),
    // parse_csv's CRLF lookahead: name- AND column-identical to the
    // escaped-quote lookahead earlier in the same function, which IS tested.
    (
        r"to_markdown\.rs:249:37:",
        "to_markdown.rs",
        249,
        r"Some(&'\n')",
    ),
];

/// Every string literal in the `exclude_re` array, comments stripped.
fn exclude_re_patterns(toml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut inside = false;
    for raw in toml.lines() {
        let line = raw.trim_start();
        if !inside {
            if line.starts_with("exclude_re") && raw.contains('[') {
                inside = true;
            }
            continue;
        }
        if line.starts_with(']') {
            break;
        }
        if line.starts_with('#') {
            continue;
        }
        if let Some(p) = first_string_literal(raw) {
            out.push(p);
        }
    }
    out
}

/// TOML literal (`'…'`, `'''…'''`) or basic (`"…"`) string, whichever opens first.
fn first_string_literal(raw: &str) -> Option<String> {
    for (i, c) in raw.char_indices() {
        match c {
            // A `#` before any opening quote starts a comment: nothing to read.
            '#' => return None,
            '\'' => {
                let (open, close) = if raw[i..].starts_with("'''") {
                    (3, "'''")
                } else {
                    (1, "'")
                };
                let rest = &raw[i + open..];
                return rest.find(close).map(|e| rest[..e].to_string());
            }
            '"' => {
                let rest = &raw[i + 1..];
                return rest.find('"').map(|e| rest[..e].to_string());
            }
            _ => {}
        }
    }
    None
}

/// True when the pattern pins a literal LINE — `.rs:` followed by a digit.
///
/// A column-pinned anchor (`.rs:\d+:32:`) has a backslash there, not a digit,
/// so it is deliberately NOT a line anchor: it cannot rotate.
fn is_line_anchored(pat: &str) -> bool {
    let mut idx = 0;
    while let Some(p) = pat[idx..].find(".rs:") {
        let at = idx + p + ".rs:".len();
        if pat[at..].chars().next().is_some_and(|c| c.is_ascii_digit()) {
            return true;
        }
        idx = at;
    }
    false
}

/// Resolve a pardon's partial path (e.g. `app/multi_cursor_glue.rs`) to the file.
fn resolve(repo_root: &Path, partial: &str) -> Option<PathBuf> {
    let want = partial.replace('\\', "/");
    let mut hits = Vec::new();
    let mut stack = vec![repo_root.join("crates")];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.to_string_lossy().replace('\\', "/").ends_with(&want) {
                hits.push(p);
            }
        }
    }
    // An ambiguous partial would make this test assert about the wrong file.
    if hits.len() == 1 {
        hits.pop()
    } else {
        None
    }
}

fn read_config() -> String {
    let toml_path = repo_root().join(".cargo/mutants.toml");
    std::fs::read_to_string(&toml_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", toml_path.display()))
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/<crate> -> repo root")
        .to_path_buf()
}

#[test]
fn no_mutation_pardon_is_anchored_on_a_line_number() {
    let toml = read_config();
    let patterns = exclude_re_patterns(&toml);

    // A parser that silently matched nothing would make this pass vacuously.
    assert!(
        patterns.len() >= 50,
        "parsed only {} exclude_re entries — the parser has drifted from the \
         file format and this test is no longer checking anything",
        patterns.len()
    );

    let offenders: Vec<&String> = patterns
        .iter()
        .filter(|p| is_line_anchored(p))
        .filter(|p| !FROZEN_LINE_ANCHORS.iter().any(|(f, ..)| &p.as_str() == f))
        .collect();

    assert!(
        offenders.is_empty(),
        "these mutation pardons are anchored on a LINE NUMBER, which is not a \
         stable key — the first edit above one slides it onto a different \
         statement and it then suppresses the wrong mutant, or nothing at all, \
         while still looking like coverage:\n  {}\n\n\
         Anchor on the mutant DESCRIPTION instead. Get it from the tool:\n    \
         cargo mutants --list --no-config --line-col true -f <path/to/file.rs>\n  \
         then regex-escape the text after the `line:col:` and use that, e.g.\n    \
         'replace \\+ with \\* in ScribeApp::duplicate_cursor_line'\n\n\
         If the bare description would also match a mutant you did NOT mean to \
         pardon, pin the COLUMN and wildcard the line instead — it cannot \
         rotate, and it keeps the pardon exactly as tight:\n    \
         'find_in_files\\.rs:\\d+:32: replace > with (==|<|>=) in walk_project'\n\n\
         Never widen a description to swallow a neighbour the tests kill: \
         over-suppression is a silent coverage loss, which is worse than the \
         rotation this rule prevents.",
        offenders
            .iter()
            .map(|p| format!("{p:?}"))
            .collect::<Vec<_>>()
            .join("\n  ")
    );
}

#[test]
fn the_frozen_line_anchors_still_point_at_the_code_they_pardon() {
    let toml = read_config();
    let patterns = exclude_re_patterns(&toml);
    let root = repo_root();

    let mut missing = Vec::new();
    let mut rotated = Vec::new();

    for (pat, partial, line, token) in FROZEN_LINE_ANCHORS {
        // A stale allowlist entry would quietly re-open the hole for that
        // pattern shape, so the allowlist may never outlive the config.
        if !patterns.iter().any(|p| p == pat) {
            missing.push(*pat);
            continue;
        }
        let Some(path) = resolve(&root, partial) else {
            rotated.push(format!("{partial}:{line} -> path no longer resolves"));
            continue;
        };
        let src = std::fs::read_to_string(&path).expect("read pardoned source");
        let text = src.lines().nth(line - 1).unwrap_or("").trim().to_string();
        if !text.contains(token) {
            rotated.push(format!(
                "{partial}:{line} -> {text:?} no longer contains {token:?}"
            ));
        }
    }

    assert!(
        missing.is_empty(),
        "these allowlisted line anchors are no longer in .cargo/mutants.toml — \
         delete them from FROZEN_LINE_ANCHORS so the allowlist cannot silently \
         grandfather a future re-add: {missing:?}"
    );
    assert!(
        rotated.is_empty(),
        "these frozen line anchors have ROTATED off the statement they pardon, \
         so they now suppress the wrong mutant (or nothing) while looking like \
         coverage:\n  {}\n\n\
         Re-derive the anchor from `cargo mutants --list --no-config \
         --line-col true -f <file>` and, if the mutant's description or column \
         has become unique, migrate it off the line number entirely.",
        rotated.join("\n  ")
    );
}
