//! Every LINE-anchored mutation pardon must still point at real code.
//!
//! `.cargo/mutants.toml` suppresses individual mutants, and many entries are
//! anchored as `'path/to/file\.rs:LINE:'`. A line number is not a stable key: any
//! edit above it slides the anchor onto a different statement. When that happens
//! the pardon stops matching the mutant it was written for — which fails SILENTLY
//! in the worst direction. The mutant reappears and the gate goes red for a
//! reason nobody wrote down, while the stale entry sits there looking like
//! coverage.
//!
//! That is not hypothetical. `'app/text_ops_methods\.rs:418:'` pardoned
//! `duplicate_cursor_line`'s `insert(ln + 1, copy)`; a later refactor pushed that
//! statement to 423 and the anchor landed on a comment. The pardon silently
//! covered nothing until a mutation run surfaced the "new" survivor. Two sibling
//! anchors in the same file (360, 799) had drifted to 364 and 805 the same way,
//! and a pair in `drag_scroll.rs` had come to rest on a blank line and a doc
//! comment.
//!
//! This test pins the invariant that would have caught all five at the moment
//! they rotated: a line-anchored pardon must land on a line that could plausibly
//! carry a mutant. Comments and blank lines never can.
//!
//! It deliberately does NOT try to verify that the anchored line is the *right*
//! statement — that needs a real `cargo mutants` run. Prefer anchoring on the
//! mutant DESCRIPTION instead (e.g.
//! `'replace \+ with \* in ScribeApp::duplicate_cursor_line'`), which does not
//! rotate at all; several entries have been converted and are exempt here
//! because they carry no line number.

use std::path::{Path, PathBuf};

/// `path/to/file.rs:LINE:` or `path/to/file.rs:LINE:COL:` anchors.
fn line_anchors(toml: &str) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    for raw in toml.lines() {
        // Only the quoted pattern, not the trailing `# comment`.
        let Some(start) = raw.find('\'') else {
            continue;
        };
        let rest = &raw[start + 1..];
        let Some(end) = rest.find('\'') else { continue };
        let pat = &rest[..end];

        let Some(rs) = pat.find(".rs:").or_else(|| pat.find("\\.rs:")) else {
            continue;
        };
        let file = pat[..rs].replace('\\', "");
        let tail = &pat[rs + if pat[rs..].starts_with("\\.") { 5 } else { 4 }..];
        let num: String = tail.chars().take_while(char::is_ascii_digit).collect();
        if num.is_empty() {
            continue;
        }
        out.push((format!("{file}.rs"), num.parse().expect("digits parse")));
    }
    out
}

/// Resolve a pardon's partial path (e.g. `app/text_ops_methods.rs`) to the file.
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

#[test]
fn every_line_anchored_pardon_points_at_code_not_a_comment() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/<crate> -> repo root")
        .to_path_buf();
    let toml_path = repo_root.join(".cargo/mutants.toml");
    let toml = std::fs::read_to_string(&toml_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", toml_path.display()));

    let anchors = line_anchors(&toml);
    // A parser that silently matched nothing would make this pass vacuously.
    assert!(
        anchors.len() >= 10,
        "expected the config to still carry line-anchored pardons; parsed only {} \
         — the parser has drifted from the file format",
        anchors.len()
    );

    let mut stale = Vec::new();
    let mut unresolved = Vec::new();
    for (file, line) in &anchors {
        let Some(path) = resolve(&repo_root, file) else {
            unresolved.push(file.clone());
            continue;
        };
        let src = std::fs::read_to_string(&path).expect("read pardoned source");
        let text = src.lines().nth(line - 1).unwrap_or("").trim().to_string();
        if text.is_empty() || text.starts_with("//") {
            stale.push(format!("{file}:{line} -> {:?}", text));
        }
    }

    assert!(
        unresolved.is_empty(),
        "these pardons name a file that no longer resolves to exactly one path — \
         the anchor cannot be checked, and probably no longer matches anything: {unresolved:?}"
    );
    assert!(
        stale.is_empty(),
        "these line-anchored mutation pardons have ROTATED onto a comment or a \
         blank line, so they now suppress nothing and the mutant they were written \
         for is unguarded:\n  {}\n\nRe-anchor each on the mutant DESCRIPTION (e.g. \
         'replace \\+ with \\* in ScribeApp::duplicate_cursor_line'), which does \
         not rotate.",
        stale.join("\n  ")
    );
}
