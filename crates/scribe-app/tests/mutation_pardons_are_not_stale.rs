//! A mutation pardon may not pin a POSITION that has stopped pointing at the
//! code it pardons — on either axis.
//!
//! `.cargo/mutants.toml` suppresses individual mutants with `exclude_re`, matched
//! against the mutant name exactly as `cargo mutants --list` prints it:
//! `path/to/file.rs:LINE:COL: <description>`. Pinning a coordinate in that name
//! is the bug this test exists to make impossible. A coordinate is not a stable
//! key: any edit above (or to the left of) it slides the anchor onto a different
//! statement, and the pardon then suppresses the WRONG mutant — or nothing at
//! all — while still looking like coverage. It fails silently, in the worst
//! direction.
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
//! ## Why this file was rewritten
//!
//! The first version flagged an anchor that had rotated onto a COMMENT or a
//! BLANK line. That caught `md_preview.rs:289` and missed `md_preview.rs:279`,
//! which had come to rest on real code — equally broken, invisible to a comment
//! check.
//!
//! The second version rejected the anchoring STYLE outright instead. It closed
//! the class against exactly ONE SPELLING and ONE AXIS, and an adversarial
//! review established that its headline claim was false:
//!
//!   * **Spelling.** `is_line_anchored` scanned for the literal byte sequence
//!     `".rs:"` followed by an ASCII digit, and nothing else. Five
//!     regex-equivalent spellings of the same line anchor walked straight
//!     through it — escaped colons (`\.rs\:249\:37\:`), a character class
//!     (`[.]rs:249:37:`), no filename at all (`:249:37: replace`), a
//!     parenthesised number (`\.rs:(249):37:`), and `\d` substitution
//!     (`\.rs:\d49:37:`). An author escaping colons out of habit silently
//!     reintroduced the exact defect, and the guard stayed green.
//!
//!   * **Axis.** Only VERTICAL rotation was closed. The migration converted 30
//!     line anchors into 10 COLUMN-pinned anchors (`\.rs:\d+:COL:`), and the
//!     frozen allowlist was a 4-tuple carrying NO column — so the column
//!     anchors had no check of any kind. Re-indenting by four columns, or
//!     merely renaming a local, left the guard GREEN while the pardon matched
//!     nothing — or, worse, landed on a DIFFERENT operator at the same column
//!     and silently suppressed the wrong mutant. That is the original
//!     `drag_scroll.rs:189` -> `page_key_assist` failure reproduced
//!     horizontally.
//!
//! So this version does three things instead:
//!
//!   1. It classifies position-pinning **semantically**, after normalising the
//!      pattern's regex spelling, so all five evasions above are caught.
//!   2. It requires every positional pardon — line-pinned OR column-pinned — to
//!      be declared in [`POSITIONAL_ANCHORS`] with the source text that must
//!      still sit at each pinned coordinate, and it verifies that text is still
//!      there. Both axes, every entry.
//!   3. It refuses to run vacuously: the config parser is checked line-by-line
//!      against the file, so a parser that has drifted cannot report "no
//!      offenders" simply by seeing nothing.
//!
//! ## The supported anchors, in order of preference
//!
//!   1. The mutant DESCRIPTION, e.g.
//!      `'replace \+ with \* in ScribeApp::duplicate_cursor_line'`. It cannot
//!      rotate on either axis, and it needs no registry entry.
//!   2. Where a bare description would also match a NEIGHBOUR the tests actually
//!      kill, the mutant's COLUMN with the line left as a wildcard, e.g.
//!      `'find_in_files\.rs:\d+:32: replace > with (==|<|>=) in walk_project'`.
//!      A column does not move when something ABOVE it is edited — but it does
//!      move when the line is re-indented or an identifier to its left is
//!      renamed, so it is NOT rotation-proof and MUST be registered below.
//!
//! Get the authoritative description and coordinates from the tool, never from
//! memory:
//!
//! ```text
//! cargo mutants --list --no-config --line-col true -f crates/<crate>/src/<file>.rs
//! ```

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// The positional-pardon registry
// ---------------------------------------------------------------------------

/// A pardon that pins a coordinate in the mutant name.
///
/// Every entry is verified on BOTH axes: a line-pinned anchor must still name
/// the recorded line, and EVERY anchor must still find its recorded source text
/// starting at the exact column it pins. That second half is what makes a
/// horizontal rotation — a re-indent, a rename to the left of the operator —
/// fail loudly instead of silently pardoning the wrong mutant.
struct PositionalAnchor {
    /// The `exclude_re` entry, byte-for-byte as it appears in the config.
    pattern: &'static str,
    /// Partial path that must resolve to exactly ONE file under `crates/`.
    file: &'static str,
    /// `Some(n)` for a frozen literal-line anchor; `None` when the pattern
    /// leaves the line as `\d+` and pins only the column.
    line: Option<usize>,
    /// Every column the pattern pins, with the source text that must start at
    /// that exact 1-based character column.
    ///
    /// `cargo mutants` reports the column of the mutated OPERATOR token (a
    /// `||`, a `!`, a `-`), not the start of the enclosing expression — these
    /// tokens were read back out of the tree rather than guessed.
    columns: &'static [(usize, &'static str)],
    /// The function the pardon's description names. It must still exist, or the
    /// pardon has silently stopped matching anything.
    function: &'static str,
}

/// Every position-pinning entry in `.cargo/mutants.toml`, and what each must
/// still point at.
///
/// Adding an entry here is a deliberate act: prefer a DESCRIPTION anchor, which
/// needs no registry entry because it cannot rotate. A line-pinned entry
/// additionally requires proving that both the mutant's description AND its
/// column are shared with a mutant the test suite kills, so no non-line anchor
/// would be tight enough. Over-suppression is worse than rotation — it deletes
/// coverage silently.
const POSITIONAL_ANCHORS: &[PositionalAnchor] = &[
    // --- Column-pinned (line left as `\d+`) --------------------------------
    // `line_span`'s last-line boundary. Column-pinned rather than
    // description-pinned because 9 of the 11 mutants on this condition ARE
    // killed by the suite; a bare `in line_span` would suppress them all. The
    // equivalence proof for these two lives in `.cargo/mutants.toml`.
    PositionalAnchor {
        pattern: r"rope_editor/mod\.rs:\d+:23: replace \+ with \* in line_span",
        file: "rope_editor/mod.rs",
        line: None,
        columns: &[(23, "+ 1")],
        function: "line_span",
    },
    PositionalAnchor {
        pattern: r"rope_editor/mod\.rs:\d+:27: replace < with <= in line_span",
        file: "rope_editor/mod.rs",
        line: None,
        columns: &[(27, "< rope.len_lines()")],
        function: "line_span",
    },
    // The three clipboard predicates' Windows-only comparison. Column-pinned
    // rather than description-pinned because each function ALSO opens with a
    // bare `keycode == Key::Cut/Copy/Paste` whose mutant carries a
    // byte-identical description and IS killed; a bare `in is_cut_command`
    // would suppress that real kill. The vacuity proof lives in
    // `.cargo/mutants.toml`.
    PositionalAnchor {
        pattern: r"keymap\.rs:\d+:71: replace == with != in is_cut_command",
        file: "app/keymap.rs",
        line: None,
        columns: &[(71, "== egui::Key::Delete)")],
        function: "is_cut_command",
    },
    PositionalAnchor {
        pattern: r"keymap\.rs:\d+:70: replace == with != in is_copy_command",
        file: "app/keymap.rs",
        line: None,
        columns: &[(70, "== egui::Key::Insert)")],
        function: "is_copy_command",
    },
    PositionalAnchor {
        pattern: r"keymap\.rs:\d+:71: replace == with != in is_paste_command",
        file: "app/keymap.rs",
        line: None,
        columns: &[(71, "== egui::Key::Insert)")],
        function: "is_paste_command",
    },
    PositionalAnchor {
        pattern: r"datetime\.rs:\d+:40: replace - with (\+|/) in format_iso8601_utc",
        file: "datetime.rs",
        line: None,
        columns: &[(40, "- 146_096")],
        function: "format_iso8601_utc",
    },
    PositionalAnchor {
        pattern: r"datetime\.rs:\d+:52: replace / with (%|\*) in format_iso8601_utc",
        file: "datetime.rs",
        line: None,
        columns: &[(52, "/ 146_097")],
        function: "format_iso8601_utc",
    },
    PositionalAnchor {
        pattern: r"filetree\.rs:\d+:54: replace == with != in dir_children",
        file: "filetree.rs",
        line: None,
        columns: &[(54, "== path.as_path()")],
        function: "dir_children",
    },
    PositionalAnchor {
        pattern: r"find_in_files\.rs:\d+:28: replace \+= with (-=|\*=) in walk_project",
        file: "find_in_files.rs",
        line: None,
        columns: &[(28, "+= 1;")],
        function: "walk_project",
    },
    PositionalAnchor {
        pattern: r"find_in_files\.rs:\d+:32: replace > with (==|<|>=) in walk_project",
        file: "find_in_files.rs",
        line: None,
        columns: &[(32, "> remaining {")],
        function: "walk_project",
    },
    PositionalAnchor {
        pattern: r"md_preview\.rs:\d+:(21|33|45): replace \|\| with && in is_safe_link_scheme",
        file: "md_preview.rs",
        line: None,
        columns: &[
            (21, "|| c == '?'"),
            (33, "|| c == '#'"),
            (45, "|| c.is_ascii_whitespace()"),
        ],
        function: "is_safe_link_scheme",
    },
    PositionalAnchor {
        pattern: r"multi_cursor\.rs:\d+:69: delete ! in reconcile_carets",
        file: "multi_cursor.rs",
        line: None,
        columns: &[(69, "!*is_primary))")],
        function: "reconcile_carets",
    },
    PositionalAnchor {
        pattern: r"app/session_persist\.rs:\d+:64: replace && with \|\| in ScribeApp::persist_session_and_autosave",
        file: "app/session_persist.rs",
        line: None,
        columns: &[(64, "&& !t.text.is_empty())")],
        function: "persist_session_and_autosave",
    },
    PositionalAnchor {
        pattern: r"app/session_persist\.rs:\d+:67: delete ! in ScribeApp::persist_session_and_autosave",
        file: "app/session_persist.rs",
        line: None,
        columns: &[(67, "!t.text.is_empty())")],
        function: "persist_session_and_autosave",
    },
    PositionalAnchor {
        pattern: r"app/session_persist\.rs:\d+:68: replace && with \|\| in ScribeApp::persist_session_and_autosave",
        file: "app/session_persist.rs",
        line: None,
        columns: &[(68, "&& self.tabs[i].is_dirty())")],
        function: "persist_session_and_autosave",
    },
    // --- The ONLY three literal-line anchors that survive -------------------
    // Each pardons a mutant whose NAME is byte-identical to a NEIGHBOUR in the
    // same function at the SAME column, so neither the description nor the
    // column can tell them apart, and widening to cover the neighbour would
    // suppress a mutant the tests actually kill.
    PositionalAnchor {
        pattern: r"app/multi_cursor_glue\.rs:77:13:",
        file: "app/multi_cursor_glue.rs",
        line: Some(77),
        columns: &[(13, "|| !self.multi_cursor.secondaries().is_empty()")],
        function: "mc_reconcile_owner",
    },
    PositionalAnchor {
        pattern: r"app/multi_cursor_glue\.rs:94:13:",
        file: "app/multi_cursor_glue.rs",
        line: Some(94),
        columns: &[(13, "|| !self.multi_cursor.secondaries().is_empty()")],
        function: "mc_record_owner",
    },
    PositionalAnchor {
        pattern: r"to_markdown\.rs:249:37:",
        file: "to_markdown.rs",
        line: Some(249),
        columns: &[(37, r"== Some(&'\n') {")],
        function: "parse_csv",
    },
];

// ---------------------------------------------------------------------------
// Config parsing
// ---------------------------------------------------------------------------

/// The parsed `exclude_re` array, plus enough bookkeeping to prove the parse
/// was not vacuous.
struct ParsedConfig {
    /// Every string literal in the array, in file order.
    patterns: Vec<String>,
    /// Lines inside the array that carry a quote — i.e. that SHOULD have
    /// yielded at least one pattern.
    entry_lines: usize,
    /// Quote-bearing lines the parser failed to extract anything from. Any
    /// entry here means the parser has drifted and is silently dropping rules.
    unparsed: Vec<String>,
}

/// Every string literal in the `exclude_re` array, comments stripped.
///
/// Reads ALL literals on a line, not just the first: an earlier version stopped
/// after one, so a second entry sharing a line would have been permanently
/// invisible to every check in this file.
fn parse_exclude_re(toml: &str) -> ParsedConfig {
    parse_array(toml, "exclude_re")
}

/// Every string literal in the `exclude_globs` array, comments stripped.
///
/// Same parser, different key. `exclude_globs` had NO guard of any kind until
/// this was added: a glob that selects zero files suppresses nothing while
/// still reading as coverage — the `exclude_re` rotation failure with a
/// different spelling.
fn parse_exclude_globs(toml: &str) -> ParsedConfig {
    parse_array(toml, "exclude_globs")
}

/// The shared array reader. `key` is the bare TOML key whose `[ … ]` array of
/// string literals is wanted.
fn parse_array(toml: &str, key: &str) -> ParsedConfig {
    let mut patterns = Vec::new();
    let mut entry_lines = 0usize;
    let mut unparsed = Vec::new();
    let mut inside = false;

    for raw in toml.lines() {
        let line = raw.trim_start();
        if !inside {
            if line.starts_with(key) && raw.contains('[') {
                inside = true;
            }
            continue;
        }
        if line.starts_with(']') {
            break;
        }
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        if !raw.contains('\'') && !raw.contains('"') {
            continue;
        }
        entry_lines += 1;
        let found = string_literals(raw);
        if found.is_empty() {
            unparsed.push(raw.to_string());
        }
        patterns.extend(found);
    }

    ParsedConfig {
        patterns,
        entry_lines,
        unparsed,
    }
}

/// Every TOML literal (`'…'`, `'''…'''`) or basic (`"…"`) string on one line,
/// stopping at the first `#` that opens a comment outside a string.
fn string_literals(raw: &str) -> Vec<String> {
    let chars: Vec<char> = raw.chars().collect();
    let mut out = Vec::new();
    let mut i = 0usize;

    while i < chars.len() {
        match chars[i] {
            // A `#` outside a string starts a comment. Everything after it is
            // prose — and the prose in this config DOES contain apostrophes, so
            // failing to stop here would invent phantom patterns.
            '#' => break,
            '\'' => {
                let triple = chars[i..].starts_with(&['\'', '\'', '\'']);
                let (open, close): (usize, &[char]) = if triple {
                    (3, &['\'', '\'', '\''])
                } else {
                    (1, &['\''])
                };
                let body = &chars[i + open..];
                match find_subslice(body, close) {
                    Some(end) => {
                        out.push(body[..end].iter().collect::<String>());
                        i += open + end + close.len();
                    }
                    None => break,
                }
            }
            '"' => {
                let body = &chars[i + 1..];
                match find_subslice(body, &['"']) {
                    Some(end) => {
                        out.push(body[..end].iter().collect::<String>());
                        i += 1 + end + 1;
                    }
                    None => break,
                }
            }
            _ => i += 1,
        }
    }
    out
}

fn find_subslice(hay: &[char], needle: &[char]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&i| &hay[i..i + needle.len()] == needle)
}

// ---------------------------------------------------------------------------
// Position classification (spelling-independent)
// ---------------------------------------------------------------------------

/// Which coordinates of the mutant name a pattern constrains.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
struct PositionClass {
    line: bool,
    column: bool,
}

impl PositionClass {
    const NONE: Self = Self {
        line: false,
        column: false,
    };
    fn is_positional(self) -> bool {
        self.line || self.column
    }
}

/// Rewrite a pattern so equivalent regex SPELLINGS of the same coordinate
/// collapse to one form.
///
/// This is the half the previous guard was missing entirely: it compared raw
/// bytes, so `\.rs\:249\:` and `[.]rs:249:` and `\.rs:(249):` — all identical in
/// meaning to the regex engine `cargo mutants` actually uses — read as "not a
/// line anchor" and sailed through.
fn normalise_spelling(pat: &str) -> String {
    let src: Vec<char> = pat.chars().collect();
    let mut out = String::new();
    let mut i = 0usize;

    while i < src.len() {
        let c = src[i];

        // `\d` is a digit WILDCARD and must survive as one; `\.` `\:` `\/` `\-`
        // `\_` are cosmetic escapes of characters that are not metacharacters
        // anyway, so they collapse to the bare character.
        if c == '\\' && i + 1 < src.len() {
            let n = src[i + 1];
            match n {
                'd' => out.push_str("\\d"),
                '.' | ':' | '/' | '-' | '_' => out.push(n),
                other => {
                    out.push('\\');
                    out.push(other);
                }
            }
            i += 2;
            continue;
        }

        // `[0-9]` / `[[:digit:]]` are `\d`; any single-character class is that
        // character.
        if c == '[' {
            if let Some(end) = find_subslice(&src[i..], &[']']) {
                let body: String = src[i + 1..i + end].iter().collect();
                if body.contains("0-9") || body.contains("digit") {
                    out.push_str("\\d");
                    i += end + 1;
                    continue;
                }
                if body.chars().count() == 1 {
                    out.push(body.chars().next().expect("one char"));
                    i += end + 1;
                    continue;
                }
            }
        }

        // A non-capturing group is a group.
        if src[i..].starts_with(&['(', '?', ':']) {
            out.push('(');
            i += 3;
            continue;
        }

        out.push(c);
        i += 1;
    }
    out
}

/// True when a colon-delimited field could be a LINE or COLUMN coordinate.
fn is_coordinate_shaped(field: &str) -> bool {
    !field.is_empty()
        && field
            .chars()
            .all(|c| c.is_ascii_digit() || "\\d+*()|{},?.-[]".contains(c))
}

/// True when a coordinate field pins an actual VALUE rather than wildcarding.
///
/// `\d+`, `[0-9]+`, `.*`, `\d{3}` constrain nothing; `64`, `(21|33|45)`,
/// `\d49`, `(249)` all do.
fn constrains_value(field: &str) -> bool {
    // Strip `{m,n}` repetition counts first so `\d{3}` is not read as pinning
    // the value 3.
    let mut stripped = String::new();
    let mut depth = 0usize;
    for c in field.chars() {
        match c {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            _ if depth == 0 => stripped.push(c),
            _ => {}
        }
    }
    stripped
        .replace("\\d", "")
        .chars()
        .any(|c| c.is_ascii_digit())
}

/// Classify which coordinates a pardon pattern pins, independent of spelling.
///
/// A `cargo mutants` name is `path:LINE:COL: description`, so the coordinates
/// are the last run of two or more consecutive coordinate-shaped fields between
/// colons. Deliberately generous: over-classifying a pattern as positional only
/// forces it to be registered, whereas under-classifying is the silent failure
/// this whole file exists to prevent.
fn classify_position(pat: &str) -> PositionClass {
    let norm = normalise_spelling(pat);
    let segments: Vec<&str> = norm.split(':').collect();
    if segments.len() < 3 {
        return PositionClass::NONE;
    }
    // Only segments with a colon on BOTH sides are fields.
    let fields = &segments[1..segments.len() - 1];

    let mut best: Option<Vec<&str>> = None;
    let mut run: Vec<&str> = Vec::new();
    for f in fields {
        if is_coordinate_shaped(f) {
            run.push(f);
        } else {
            if run.len() > 1 {
                best = Some(run.clone());
            }
            run.clear();
        }
    }
    if run.len() > 1 {
        best = Some(run);
    }

    match best {
        Some(r) => {
            let line = r[r.len() - 2];
            let col = r[r.len() - 1];
            PositionClass {
                line: constrains_value(line),
                column: constrains_value(col),
            }
        }
        None => PositionClass::NONE,
    }
}

// ---------------------------------------------------------------------------
// Anchor verification
// ---------------------------------------------------------------------------

/// Everything wrong with one registry entry, given the current source of the
/// file it points into.
///
/// Pure over `src` on purpose: the fail-proof tests below feed it deliberately
/// rotated source and assert it goes red, so this guard is never trusted on the
/// strength of "it passes today".
fn anchor_problems(src: &str, a: &PositionalAnchor) -> Vec<String> {
    let lines: Vec<&str> = src.lines().collect();
    let mut problems = Vec::new();

    if !src.contains(&format!("fn {}", a.function)) {
        problems.push(format!(
            "{}: `fn {}` no longer exists, so the pardon's description matches nothing",
            a.file, a.function
        ));
    }

    for (col, token) in a.columns {
        let starts_at = |text: &str| -> bool {
            text.chars()
                .skip(col - 1)
                .collect::<String>()
                .starts_with(token)
        };

        match a.line {
            Some(n) => {
                let text = lines.get(n - 1).copied().unwrap_or("");
                if !starts_at(text) {
                    problems.push(format!(
                        "{}:{}:{} no longer has {:?} at that column (found {:?})",
                        a.file,
                        n,
                        col,
                        token,
                        text.chars().skip(col - 1).take(40).collect::<String>()
                    ));
                }
            }
            None => {
                let hits: Vec<usize> = lines
                    .iter()
                    .enumerate()
                    .filter(|(_, t)| starts_at(t))
                    .map(|(i, _)| i + 1)
                    .collect();
                if hits.is_empty() {
                    problems.push(format!(
                        "{}:*:{} — no line has {:?} at that column any more; the \
                         column anchor has rotated horizontally and now pardons \
                         a different mutant, or none",
                        a.file, col, token
                    ));
                }
            }
        }
    }
    problems
}

/// Resolve a pardon's partial path (e.g. `app/multi_cursor_glue.rs`) to a file.
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

// ---------------------------------------------------------------------------
// The guards
// ---------------------------------------------------------------------------

/// The parser must be provably reading the whole array.
///
/// The previous floor was `>= 50` against 101 real entries — losing HALF the
/// config would still have passed. This checks the parse line-by-line against
/// the file instead, so silence has to be earned.
#[test]
fn the_config_parser_reads_every_exclude_re_entry() {
    let cfg = parse_exclude_re(&read_config());

    assert!(
        cfg.unparsed.is_empty(),
        "the exclude_re parser found no string literal on these entry lines, so \
         those pardons are invisible to every check in this file:\n  {}",
        cfg.unparsed.join("\n  ")
    );
    assert!(
        cfg.entry_lines >= 95,
        "only {} exclude_re entry lines found — the array shrank or the parser \
         lost the block; either way this file has stopped checking anything",
        cfg.entry_lines
    );
    assert!(
        cfg.patterns.len() >= cfg.entry_lines,
        "parsed {} patterns from {} entry lines — at least one line's pattern \
         was dropped",
        cfg.patterns.len(),
        cfg.entry_lines
    );
}

// ---------------------------------------------------------------------------
// exclude_globs: a glob that selects nothing is stale by definition
// ---------------------------------------------------------------------------

/// Does `pattern` select at least one file that exists in the tree?
///
/// A literal path (every current entry) is answered by `exists()`. A wildcard
/// pattern is answered by walking the tree, because "I cannot evaluate this"
/// must never resolve to "it is fine".
fn glob_selects_any(root: &Path, pattern: &str) -> bool {
    let pat = pattern.replace('\\', "/");
    if !pat.contains('*') && !pat.contains('?') {
        return root.join(&pat).exists();
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if p.is_dir() {
                // `target/` is build output and `.git/` is history — neither is
                // source cargo-mutants would ever mutate, and walking them turns
                // this guard into a minute-long scan.
                if name != "target" && name != ".git" {
                    stack.push(p);
                }
                continue;
            }
            let rel = p
                .strip_prefix(root)
                .unwrap_or(&p)
                .to_string_lossy()
                .replace('\\', "/");
            if glob_match(&pat, &rel) || (!pat.contains('/') && glob_match(&pat, &name)) {
                return true;
            }
        }
    }
    false
}

/// `*` matches within one path segment, `**` crosses segments, `?` is one
/// non-separator character. Deliberately small: the alternative is a guard that
/// cannot answer, and a guard that cannot answer is one that always passes.
fn glob_match(pattern: &str, text: &str) -> bool {
    fn go(p: &[char], t: &[char]) -> bool {
        if p.is_empty() {
            return t.is_empty();
        }
        match p[0] {
            '*' if p.len() > 1 && p[1] == '*' => {
                let rest = &p[2..];
                // `**/` must also match ZERO directories, so `a/**/b.rs` still
                // selects `a/b.rs`.
                if rest.first() == Some(&'/') && go(&rest[1..], t) {
                    return true;
                }
                (0..=t.len()).any(|i| go(rest, &t[i..]))
            }
            '*' => {
                for i in 0..=t.len() {
                    if go(&p[1..], &t[i..]) {
                        return true;
                    }
                    if t.get(i) == Some(&'/') {
                        break;
                    }
                }
                false
            }
            '?' => !t.is_empty() && t[0] != '/' && go(&p[1..], &t[1..]),
            c => !t.is_empty() && t[0] == c && go(&p[1..], &t[1..]),
        }
    }
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    go(&p, &t)
}

/// The globs that currently select nothing, named individually.
fn dead_globs(root: &Path, patterns: &[String]) -> Vec<String> {
    patterns
        .iter()
        .filter(|g| !glob_selects_any(root, g))
        .cloned()
        .collect()
}

/// A whole-FILE exclusion that matches no file is a pardon with nothing behind
/// it.
///
/// `exclude_re` has carried a staleness guard since the rotation incidents;
/// `exclude_globs` had none. The failure mode is the same and quieter: rename or
/// move `app/grid_render.rs` and its glob keeps sitting in the config, reading
/// as "this file is deliberately not mutated" while the file it names is gone
/// and the file it MOVED to is now silently mutation-gated (or, worse, the
/// reader assumes it is still excluded). Zero selection is stale by definition —
/// fail here, naming the dead pattern, rather than leaving it to be discovered
/// by a survivor nobody expected.
#[test]
fn every_exclude_glob_still_selects_a_file_that_exists() {
    let cfg = parse_exclude_globs(&read_config());

    assert!(
        cfg.unparsed.is_empty(),
        "the exclude_globs parser found no string literal on these entry lines, \
         so those exclusions are invisible to this check:\n  {}",
        cfg.unparsed.join("\n  ")
    );
    // The array has 10 entries today. A floor of 8 refuses to run vacuously: a
    // parser that lost the block would otherwise report "no dead globs" simply
    // by seeing nothing — the exact silence this file exists to forbid.
    assert!(
        cfg.patterns.len() >= 8,
        "only {} exclude_globs entries parsed — the array shrank or the parser \
         lost the block; either way this check has stopped checking anything",
        cfg.patterns.len()
    );

    let dead = dead_globs(&repo_root(), &cfg.patterns);
    assert!(
        dead.is_empty(),
        "these exclude_globs entries select ZERO files, so they exclude nothing \
         while still reading as a deliberate exclusion. Delete each one, or \
         repoint it at the path the file actually moved to:\n  {}",
        dead.join("\n  ")
    );
}

/// Proof the check above is not vacuous: a glob aimed at a path that does not
/// exist MUST be reported, and its own name MUST appear in the failure.
#[test]
fn fail_proof_a_glob_pointing_at_a_missing_file_is_caught() {
    let root = repo_root();
    let live = "crates/scribe-app/src/app/grid_render.rs".to_string();
    let dead = "crates/scribe-app/src/app/this_file_does_not_exist.rs".to_string();

    assert!(
        glob_selects_any(&root, &live),
        "the control pattern must select a REAL file, otherwise this proof \
         cannot tell a working checker from a broken one"
    );
    assert_eq!(
        dead_globs(&root, &[live.clone(), dead.clone()]),
        vec![dead],
        "only the missing path may be reported dead"
    );
}

/// Proof the wildcard branch is real. A `**` pattern that matches must pass and
/// a `**` pattern that cannot match must be reported — otherwise a future
/// wildcard entry would sail through the literal-path fast path untested.
#[test]
fn fail_proof_a_wildcard_glob_is_evaluated_not_waved_through() {
    let root = repo_root();
    assert!(
        glob_selects_any(&root, "crates/**/grid_render.rs"),
        "`**` must cross directory segments"
    );
    assert!(
        !glob_selects_any(&root, "crates/**/no_such_source_file_xyzzy.rs"),
        "a wildcard that matches nothing must be reported dead, not assumed live"
    );
    // The segment rules the matcher claims to implement.
    assert!(glob_match("a/*.rs", "a/b.rs"));
    assert!(!glob_match("a/*.rs", "a/b/c.rs"), "`*` must not cross `/`");
    assert!(glob_match("a/**/c.rs", "a/b/c.rs"));
    assert!(glob_match("a/**/c.rs", "a/c.rs"), "`**/` matches zero dirs");
    assert!(glob_match("a/?.rs", "a/b.rs"));
    assert!(!glob_match("a/?.rs", "a/bc.rs"));
}

/// No pardon may pin a coordinate without being registered — either axis, any
/// spelling.
#[test]
fn no_mutation_pardon_is_anchored_on_a_line_number() {
    let cfg = parse_exclude_re(&read_config());
    let registered: BTreeSet<&str> = POSITIONAL_ANCHORS.iter().map(|a| a.pattern).collect();

    let offenders: Vec<String> = cfg
        .patterns
        .iter()
        .filter(|p| classify_position(p).is_positional())
        .filter(|p| !registered.contains(p.as_str()))
        .map(|p| {
            let c = classify_position(p);
            format!(
                "{p:?}  (line pinned: {}, column pinned: {})",
                c.line, c.column
            )
        })
        .collect();

    assert!(
        offenders.is_empty(),
        "these mutation pardons pin a COORDINATE in the mutant name without a \
         POSITIONAL_ANCHORS entry. A coordinate is not a stable key — an edit \
         above one slides it vertically, and a re-indent or a rename to its \
         left slides it horizontally. Either way it then suppresses the wrong \
         mutant, or nothing at all, while still looking like coverage:\n  {}\n\n\
         Anchor on the mutant DESCRIPTION instead. Get it from the tool:\n    \
         cargo mutants --list --no-config --line-col true -f <path/to/file.rs>\n  \
         then regex-escape the text after the `line:col:` and use that, e.g.\n    \
         'replace \\+ with \\* in ScribeApp::duplicate_cursor_line'\n\n\
         If the bare description would also match a mutant you did NOT mean to \
         pardon, pin the COLUMN and wildcard the line — but a column still \
         rotates horizontally, so register it in POSITIONAL_ANCHORS with the \
         source text that must sit at it.\n\n\
         Never widen a description to swallow a neighbour the tests kill: \
         over-suppression is a silent coverage loss, which is worse than the \
         rotation this rule prevents.",
        offenders.join("\n  ")
    );
}

/// Every registered anchor must still point at the code it pardons — on BOTH
/// axes.
#[test]
fn every_positional_anchor_still_points_at_the_code_it_pardons() {
    let cfg = parse_exclude_re(&read_config());
    let root = repo_root();

    let mut missing = Vec::new();
    let mut problems = Vec::new();

    for a in POSITIONAL_ANCHORS {
        // A stale registry entry would quietly grandfather that pattern shape
        // back in, so the registry may never outlive the config.
        if !cfg.patterns.iter().any(|p| p == a.pattern) {
            missing.push(a.pattern);
            continue;
        }
        // A registered pattern that is no longer classified as positional means
        // the classifier has regressed and would stop policing this shape.
        assert!(
            classify_position(a.pattern).is_positional(),
            "{:?} is registered as positional but classify_position no longer \
             agrees — the classifier has regressed and this shape is now \
             unpoliced",
            a.pattern
        );
        let Some(path) = resolve(&root, a.file) else {
            problems.push(format!("{} -> path no longer resolves uniquely", a.file));
            continue;
        };
        let src = std::fs::read_to_string(&path).expect("read pardoned source");
        problems.extend(anchor_problems(&src, a));
    }

    assert!(
        missing.is_empty(),
        "these registered anchors are no longer in .cargo/mutants.toml — delete \
         them from POSITIONAL_ANCHORS so the registry cannot silently \
         grandfather a future re-add: {missing:?}"
    );
    assert!(
        problems.is_empty(),
        "these positional anchors have ROTATED off the code they pardon, so \
         they now suppress the wrong mutant (or nothing) while looking like \
         coverage:\n  {}\n\n\
         Re-derive the anchor from `cargo mutants --list --no-config \
         --line-col true -f <file>` and, if the mutant's description has become \
         unique, migrate it off the coordinate entirely.",
        problems.join("\n  ")
    );
}

/// The `<impl eframe::App for ScribeApp>` pardon is a blanket over every method
/// of that impl — including ones that do not exist yet.
///
/// It is kept (those three methods are live-frame/eframe-host only, with no
/// headless observable), but it may not silently acquire coverage of a FUTURE
/// method that does have testable logic. Adding a method to the impl turns this
/// red and forces the decision to be made explicitly.
#[test]
fn the_eframe_app_blanket_pardon_covers_only_the_documented_methods() {
    let cfg = parse_exclude_re(&read_config());
    let blanket = "<impl eframe::App for ScribeApp>";
    if !cfg.patterns.iter().any(|p| p == blanket) {
        return; // the blanket was removed or narrowed; nothing to police.
    }

    let path = resolve(&repo_root(), "app/mod.rs").expect("app/mod.rs resolves");
    let src = std::fs::read_to_string(&path).expect("read app/mod.rs");

    let start = src
        .find("impl eframe::App for ScribeApp")
        .expect("the impl block the blanket pardon names still exists");
    let mut depth = 0usize;
    let mut end = src.len();
    for (off, c) in src[start..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = start + off;
                    break;
                }
            }
            _ => {}
        }
    }

    let found: BTreeSet<String> = src[start..end]
        .lines()
        .filter_map(|l| l.trim().strip_prefix("fn "))
        .filter_map(|r| r.split(['(', '<']).next())
        .map(|s| s.trim().to_string())
        .collect();
    let documented: BTreeSet<String> = ["clear_color", "save", "ui"]
        .iter()
        .map(|s| s.to_string())
        .collect();

    assert_eq!(
        found, documented,
        "the `{blanket}` pardon blankets EVERY method of that impl, present and \
         future. Its method set has changed, so it is now suppressing mutants \
         nobody justified. Either narrow the pardon to the methods that are \
         genuinely live-frame-only, or add the new method here with an ADR-0007 \
         justification."
    );
}

// ---------------------------------------------------------------------------
// Fail-proofs
//
// A guard that cannot fail is worse than no guard. Everything below feeds the
// checks above a deliberately broken input and asserts they go RED — including
// a CONTROL that reproduces the previous implementation, so the five spelling
// evasions are demonstrated to have been real rather than asserted to be.
// ---------------------------------------------------------------------------

/// The previous detector, verbatim, as a control.
fn legacy_is_line_anchored(pat: &str) -> bool {
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

/// The five regex-equivalent spellings of `app/foo.rs:249:37:` that the old
/// detector let through.
const EVADING_SPELLINGS: &[(&str, &str)] = &[
    ("escaped colons", r"app/foo\.rs\:249\:37\:"),
    ("character class", r"[.]rs:249:37:"),
    ("no filename", r":249:37: replace > with >= in parse_csv"),
    ("parenthesised number", r"app/foo\.rs:(249):37:"),
    ("\\d substitution", r"app/foo\.rs:\d49:37:"),
];

#[test]
fn fail_proof_the_old_detector_really_did_miss_all_five_spellings() {
    for (name, pat) in EVADING_SPELLINGS {
        assert!(
            !legacy_is_line_anchored(pat),
            "control failed: the OLD detector already caught {name} ({pat:?}), so \
             this fail-proof is not demonstrating anything"
        );
    }
    // …and the one spelling it DID catch, so the control is not vacuously
    // negative on every input.
    assert!(
        legacy_is_line_anchored(r"app/multi_cursor_glue\.rs:77:13:"),
        "control is inert: the old detector caught nothing at all"
    );
}

#[test]
fn fail_proof_every_evading_spelling_is_now_classified_as_line_pinned() {
    for (name, pat) in EVADING_SPELLINGS {
        let c = classify_position(pat);
        assert!(
            c.line,
            "{name} ({pat:?}) is still not detected as a LINE anchor — the \
             spelling hole is open"
        );
    }
}

#[test]
fn fail_proof_an_unregistered_evading_spelling_would_fail_the_guard() {
    // The guard's filter, applied to a config that contains one evading
    // spelling: it must be reported as an offender.
    let registered: BTreeSet<&str> = POSITIONAL_ANCHORS.iter().map(|a| a.pattern).collect();
    for (name, pat) in EVADING_SPELLINGS {
        let flagged = classify_position(pat).is_positional() && !registered.contains(pat);
        assert!(
            flagged,
            "{name} ({pat:?}) would slip past \
             no_mutation_pardon_is_anchored_on_a_line_number"
        );
    }
}

#[test]
fn fail_proof_a_column_only_anchor_is_still_classified_positional() {
    // The axis hole: `\d+` on the line was treated as "not an anchor" and got
    // NO check at all, on either axis.
    let c =
        classify_position(r"find_in_files\.rs:\d+:32: replace > with (==|<|>=) in walk_project");
    assert!(
        !c.line,
        "the line is wildcarded, so it must not read as line-pinned"
    );
    assert!(
        c.column,
        "a column-pinned anchor must be classified positional — it rotates \
         horizontally on a re-indent or a rename"
    );
}

#[test]
fn fail_proof_a_description_anchor_is_not_flagged() {
    // The guard must not be a blanket that flags everything: description
    // anchors are the SUPPORTED form and must stay clean, or authors will be
    // pushed back toward coordinates.
    for pat in [
        r"replace \+ with \* in ScribeApp::duplicate_cursor_line",
        r"replace && with \|\| in ScribeApp::execute_builtin",
        r"hit_test\.rs.*replace > with >= in classify",
        r"<impl eframe::App for ScribeApp>",
        r"\bpaint_crt_scanlines\b",
        r"reporting\.rs.*replace log_outcome with \(\)",
    ] {
        assert_eq!(
            classify_position(pat),
            PositionClass::NONE,
            "{pat:?} is not positional but was flagged — this would push authors \
             off description anchors, the one form that cannot rotate"
        );
    }
}

/// Vertical rotation: an edit ABOVE a frozen line anchor slides it.
#[test]
fn fail_proof_vertical_rotation_is_caught() {
    let a = POSITIONAL_ANCHORS
        .iter()
        .find(|a| a.line == Some(249))
        .expect("the to_markdown frozen anchor is registered");
    let path = resolve(&repo_root(), a.file).expect("resolves");
    let src = std::fs::read_to_string(&path).expect("read");

    assert!(
        anchor_problems(&src, a).is_empty(),
        "precondition: the anchor is currently intact"
    );

    // Insert one line at the top — every line below slides down by one.
    let rotated = format!("// an innocuous new import\n{src}");
    assert!(
        !anchor_problems(&rotated, a).is_empty(),
        "a one-line insertion above the anchor did NOT turn the guard red — \
         vertical rotation is undetected"
    );
}

/// Horizontal rotation, axis one: a re-indent moves every column on the line.
#[test]
fn fail_proof_horizontal_rotation_by_reindent_is_caught() {
    for a in POSITIONAL_ANCHORS {
        let path = resolve(&repo_root(), a.file).expect("resolves");
        let src = std::fs::read_to_string(&path).expect("read");
        assert!(
            anchor_problems(&src, a).is_empty(),
            "precondition failed for {:?}",
            a.pattern
        );

        // Re-indent every line by four columns, exactly as wrapping a block in
        // an `if` would. Nothing about the CODE changed; only the columns did.
        let reindented: String = src.lines().map(|l| format!("    {l}\n")).collect();

        assert!(
            !anchor_problems(&reindented, a).is_empty(),
            "re-indenting by four columns did NOT turn the guard red for {:?} — \
             this anchor's column is unverified, which is the exact hole that \
             let drag_scroll.rs:189 pardon page_key_assist, reproduced \
             horizontally",
            a.pattern
        );
    }
}

/// Horizontal rotation, axis two: renaming a local to the LEFT of the operator
/// shifts the operator's column without touching the line's meaning.
#[test]
fn fail_proof_horizontal_rotation_by_rename_is_caught() {
    let a = POSITIONAL_ANCHORS
        .iter()
        .find(|a| a.file == "to_markdown.rs")
        .expect("the parse_csv anchor is registered");
    let path = resolve(&repo_root(), a.file).expect("resolves");
    let src = std::fs::read_to_string(&path).expect("read");
    assert!(
        anchor_problems(&src, a).is_empty(),
        "precondition: the anchor is currently intact"
    );

    // `chars` -> `it`: three characters shorter, so everything to its right on
    // that line — including the pardoned operator — shifts left by three.
    let renamed = src.replace("chars", "it");
    assert_ne!(
        renamed, src,
        "precondition: the rename actually changed the file"
    );
    assert!(
        !anchor_problems(&renamed, a).is_empty(),
        "renaming a local to the left of the pardoned operator did NOT turn the \
         guard red — the column anchor is unverified and now silently pardons a \
         different mutant, or none"
    );
}

/// A column anchor that lands on a DIFFERENT expression at the same column must
/// still be caught: the check is the recorded source TEXT, not merely "some
/// character is present here".
#[test]
fn fail_proof_a_different_expression_at_the_same_column_is_caught() {
    let a = POSITIONAL_ANCHORS
        .iter()
        .find(|a| a.file == "app/session_persist.rs" && a.columns[0].0 == 64)
        .expect("the session_persist column anchor is registered");
    let path = resolve(&repo_root(), a.file).expect("resolves");
    let src = std::fs::read_to_string(&path).expect("read");
    assert!(anchor_problems(&src, a).is_empty(), "precondition");

    // Same column, same operator SHAPE, different expression — the class of
    // silent mis-suppression this guard exists to catch.
    let swapped = src.replace("&& !t.text.is_empty())", "&& !t.path.is_none()!)");
    assert_ne!(swapped, src, "precondition: the swap changed the file");
    assert!(
        !anchor_problems(&swapped, a).is_empty(),
        "the anchor still passed after its column came to rest on a different \
         expression — it is checking presence, not identity"
    );
}

/// A registry entry whose function has been renamed away must be caught: the
/// pardon's description would then match nothing at all.
#[test]
fn fail_proof_a_renamed_pardoned_function_is_caught() {
    let a = POSITIONAL_ANCHORS
        .iter()
        .find(|a| a.function == "walk_project")
        .expect("the walk_project anchor is registered");
    let path = resolve(&repo_root(), a.file).expect("resolves");
    let src = std::fs::read_to_string(&path).expect("read");
    assert!(anchor_problems(&src, a).is_empty(), "precondition");

    let renamed = src.replace("fn walk_project", "fn walk_the_project");
    assert_ne!(renamed, src, "precondition: the rename changed the file");
    assert!(
        !anchor_problems(&renamed, a).is_empty(),
        "renaming the pardoned function did NOT turn the guard red — the pardon \
         now matches no mutant at all and nothing says so"
    );
}

/// The parser must see a second pattern sharing a line.
#[test]
fn fail_proof_a_second_pattern_on_one_line_is_not_invisible() {
    let one_line = "exclude_re = [\n  'first\\.rs:249:37:', 'second\\.rs:9:1:',\n]\n";
    let cfg = parse_exclude_re(one_line);
    assert_eq!(
        cfg.patterns.len(),
        2,
        "only saw {:?} — a pardon sharing a line with another is invisible to \
         every check in this file",
        cfg.patterns
    );
    // …and both must still be classified, not just extracted.
    assert!(cfg.patterns.iter().all(|p| classify_position(p).line));
}

/// Comment prose in this config contains apostrophes; the parser must not
/// invent patterns out of them.
#[test]
fn fail_proof_comment_prose_does_not_become_a_phantom_pattern() {
    let with_prose =
        "exclude_re = [\n  'real\\.rs:1:2:',  # the sibling Some('?') arm, which IS tested\n]\n";
    let cfg = parse_exclude_re(with_prose);
    assert_eq!(
        cfg.patterns,
        vec!["real\\.rs:1:2:".to_string()],
        "the parser read a phantom pattern out of comment prose"
    );
}

/// The vacuity floor must actually bite.
#[test]
fn fail_proof_a_gutted_config_fails_the_floor() {
    let gutted = "exclude_re = [\n  'replace foo with \\(\\)',\n]\n";
    let cfg = parse_exclude_re(gutted);
    assert!(
        cfg.entry_lines < 95,
        "precondition: the gutted config is small"
    );
    // The real config must clear the floor the gutted one fails, or the floor
    // is either inert or impossible.
    let real = parse_exclude_re(&read_config());
    assert!(
        real.entry_lines >= 95,
        "the real config no longer clears its own floor ({} entries)",
        real.entry_lines
    );
}
