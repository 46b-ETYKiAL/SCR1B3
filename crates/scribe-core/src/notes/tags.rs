//! Inline `#tag` extraction (Obsidian-style), nesting-aware.
//!
//! A tag is `#` immediately followed by a run of tag characters
//! (`A-Z a-z 0-9 _ - /`), where the FIRST tag character is not a digit (so a
//! bare `#123` — an issue reference or a heading fragment — is not a tag) and
//! the `#` is at a word boundary (start of line, or preceded by whitespace or
//! an opening bracket/paren). Nested tags use `/`: `#project/frontend` yields
//! the full tag plus its ancestor prefixes for the tag tree.
//!
//! Fenced code blocks (```` ``` ````), indented code, and inline code spans
//! (`` `…` ``) are skipped so a `#comment` inside a shell snippet is not
//! mistaken for a tag. Pure — no I/O.

use super::scan_guard::assert_advanced;
use std::collections::BTreeSet;

/// True if `c` may appear in a tag body.
fn is_tag_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '/'
}

/// Extract every distinct inline `#tag` from `text`, sorted. A tag is returned
/// WITHOUT its leading `#`. Duplicate tags collapse. Nesting is preserved in the
/// returned string (`project/frontend`); use [`with_ancestors`] to expand the
/// tree prefixes.
#[must_use]
pub fn extract_inline_tags(text: &str) -> Vec<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    let mut in_fence = false;
    for raw_line in text.lines() {
        let trimmed = raw_line.trim_start();
        // Toggle fenced code blocks on ``` or ~~~ fences.
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        scan_line_tags(raw_line, &mut out);
    }
    out.into_iter().collect()
}

/// Scan one (non-fenced) line for tags, skipping inline `` `code` `` spans.
fn scan_line_tags(line: &str, out: &mut BTreeSet<String>) {
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0usize;
    let mut in_code = false;
    // Cursor at the top of the previous iteration. `usize::MAX` is a safe "no
    // previous iteration yet" sentinel: `i` is bounded by `chars.len()`, so it
    // can never legitimately reach it.
    let mut prev_i = usize::MAX;
    while i < chars.len() {
        if prev_i != usize::MAX {
            assert_advanced(prev_i, i, "scan_line_tags");
        }
        prev_i = i;
        let c = chars[i];
        if c == '`' {
            in_code = !in_code;
            i += 1;
            continue;
        }
        if in_code {
            i += 1;
            continue;
        }
        if c == '#' {
            // Boundary check: the `#` must start a line or follow whitespace or
            // an opening delimiter — never mid-word (e.g. a URL fragment
            // `page#frag` or a hex colour `#fff` after text).
            let prev_ok = i == 0
                || matches!(
                    chars[i - 1],
                    ' ' | '\t' | '(' | '[' | '{' | '<' | '"' | '\''
                );
            if prev_ok {
                // Collect the tag body.
                let mut j = i + 1;
                let mut prev_j = usize::MAX;
                while j < chars.len() && is_tag_char(chars[j]) {
                    if prev_j != usize::MAX {
                        assert_advanced(prev_j, j, "scan_line_tags tag body");
                    }
                    prev_j = j;
                    j += 1;
                }
                let body: String = chars[i + 1..j].iter().collect();
                if is_valid_tag_body(&body) {
                    out.insert(body);
                }
                i = j;
                continue;
            }
        }
        i += 1;
    }
}

/// A tag body is valid when non-empty, does not start or end with `/`, has no
/// empty nesting segment (`a//b`), and its first character is not a digit (so
/// `#123` is excluded). At least one segment must contain a letter so a pure
/// `#-` / `#_` is rejected.
fn is_valid_tag_body(body: &str) -> bool {
    if body.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return false;
    }
    // An empty body, a leading `/`, a trailing `/`, and an empty middle segment
    // (`a//b`) are all the SAME condition — "some `/`-separated segment is
    // empty": `""` splits to `[""]`, `"/b"` to `["", "b"]`, `"a/"` to
    // `["a", ""]`. A separate `is_empty() || starts_with('/') || ends_with('/')`
    // guard ahead of this one is therefore pure duplication: it can only ever
    // reject inputs this check already rejects, so no test could distinguish
    // whether it ran. `every_empty_segment_shape_is_rejected` pins all four
    // shapes against this single check.
    if body.split('/').any(str::is_empty) {
        return false;
    }
    body.chars().any(|c| c.is_ascii_alphabetic())
}

/// Expand a set of nested tags into the full tag-tree set: `project/frontend`
/// contributes both `project` and `project/frontend`. Used to build the tag
/// tree so a parent tag lists notes under any of its children.
#[must_use]
pub fn with_ancestors<'a, I>(tags: I) -> Vec<String>
where
    I: IntoIterator<Item = &'a String>,
{
    let mut out: BTreeSet<String> = BTreeSet::new();
    for tag in tags {
        let parts: Vec<&str> = tag.split('/').collect();
        let mut prefix = String::new();
        for (idx, part) in parts.iter().enumerate() {
            if idx > 0 {
                prefix.push('/');
            }
            prefix.push_str(part);
            out.insert(prefix.clone());
        }
    }
    out.into_iter().collect()
}

/// True if `candidate` is `tag` or a descendant of it (`tag/…`). Used to filter
/// notes by a selected tag-tree node.
#[must_use]
pub fn tag_matches(candidate: &str, selected: &str) -> bool {
    if candidate == selected {
        return true;
    }
    // A descendant is `selected` + `/` + more, so the byte at `selected.len()`
    // must be the separator. Reading it through `get` bounds the access itself,
    // which is why no `candidate.len() > selected.len()` guard is needed — and
    // why one would be indistinguishable duplication: equal lengths only reach
    // this point when the two strings DIFFER, and then `starts_with` is already
    // false. `tag_matches_boundary_cases` pins the shorter / equal / longer
    // candidate cases against this single bounds-safe read.
    match candidate.as_bytes().get(selected.len()) {
        Some(b'/') => candidate.starts_with(selected),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_tags() {
        assert_eq!(
            extract_inline_tags("a #todo and #idea here"),
            vec!["idea".to_string(), "todo".to_string()]
        );
    }

    #[test]
    fn tag_at_line_start() {
        assert_eq!(
            extract_inline_tags("#start of line"),
            vec!["start".to_string()]
        );
    }

    #[test]
    fn nested_tag_preserved() {
        assert_eq!(
            extract_inline_tags("#project/frontend/ui"),
            vec!["project/frontend/ui".to_string()]
        );
    }

    #[test]
    fn numeric_hash_is_not_a_tag() {
        // `#123` is an issue-ref / heading fragment, not a tag.
        assert!(extract_inline_tags("see #123 and #45").is_empty());
        // But `#1a` starts with a digit too → rejected.
        assert!(extract_inline_tags("#1a").is_empty());
        // A leading letter with digits later is fine.
        assert_eq!(
            extract_inline_tags("#v2release"),
            vec!["v2release".to_string()]
        );
    }

    #[test]
    fn atx_heading_is_not_a_tag() {
        // `# Heading` has a space after `#`, so no tag body — excluded.
        assert!(extract_inline_tags("# Heading\n## Sub").is_empty());
    }

    #[test]
    fn midword_hash_ignored() {
        // A URL fragment / hex colour after text is not a tag.
        assert!(extract_inline_tags("visit page#section or color#fff").is_empty());
    }

    #[test]
    fn hash_after_opening_bracket_is_a_tag() {
        assert_eq!(
            extract_inline_tags("(#inline) [#bracketed]"),
            vec!["bracketed".to_string(), "inline".to_string()]
        );
    }

    #[test]
    fn fenced_code_block_skipped() {
        let text = "before #real\n```\n#notatag in code\n```\nafter #also";
        assert_eq!(
            extract_inline_tags(text),
            vec!["also".to_string(), "real".to_string()]
        );
    }

    #[test]
    fn inline_code_span_skipped() {
        let text = "run `#!/bin/sh` but tag #real";
        assert_eq!(extract_inline_tags(text), vec!["real".to_string()]);
    }

    #[test]
    fn inline_code_span_skips_an_otherwise_valid_tag() {
        // The `#` here is preceded by a SPACE, so the mid-word boundary check
        // would happily accept it — the ONLY thing suppressing it is the
        // in-code-span state. `inline_code_span_skipped` above cannot see that:
        // its `#` follows a backtick, which the boundary check rejects on its
        // own, so the code-span tracking is never actually load-bearing there.
        let text = "run `echo #notatag` but tag #real";
        assert_eq!(extract_inline_tags(text), vec!["real".to_string()]);
    }

    #[test]
    fn code_span_state_toggles_back_off_after_the_closing_backtick() {
        // A tag AFTER a closed span is still collected, and a second span
        // re-enters the skip state. Pins the toggle (not just "sticky on").
        let text = "`echo #one` #two `echo #three` #four";
        assert_eq!(
            extract_inline_tags(text),
            vec!["four".to_string(), "two".to_string()]
        );
    }

    #[test]
    fn duplicate_tags_collapse() {
        assert_eq!(
            extract_inline_tags("#dup #dup #dup"),
            vec!["dup".to_string()]
        );
    }

    #[test]
    fn empty_and_malformed_rejected() {
        assert!(extract_inline_tags("# #/ #a/ #/b #a//b").is_empty());
        // A pure symbol tag has no letter → rejected.
        assert!(extract_inline_tags("#--- #___").is_empty());
    }

    #[test]
    fn with_ancestors_expands_tree() {
        let tags = vec!["project/frontend/ui".to_string(), "idea".to_string()];
        assert_eq!(
            with_ancestors(&tags),
            vec![
                "idea".to_string(),
                "project".to_string(),
                "project/frontend".to_string(),
                "project/frontend/ui".to_string(),
            ]
        );
    }

    #[test]
    fn tag_matches_self_and_descendants() {
        assert!(tag_matches("project", "project"));
        assert!(tag_matches("project/frontend", "project"));
        assert!(tag_matches("project/frontend/ui", "project"));
        assert!(!tag_matches("projectx", "project")); // not a `/` boundary
        assert!(!tag_matches("project", "project/frontend"));
    }

    #[test]
    fn every_empty_segment_shape_is_rejected() {
        // The four distinct malformed shapes that all reduce to "some
        // `/`-separated segment is empty", pinned individually so the single
        // segment check in `is_valid_tag_body` provably stands in for all of
        // them (and a regression in any one shape is attributable).
        assert!(extract_inline_tags("#").is_empty(), "empty body");
        assert!(extract_inline_tags("#/").is_empty(), "slash only");
        assert!(extract_inline_tags("#/b").is_empty(), "leading slash");
        assert!(extract_inline_tags("#a/").is_empty(), "trailing slash");
        assert!(
            extract_inline_tags("#a//b").is_empty(),
            "empty middle segment"
        );
        // A well-formed nested tag passes all of them.
        assert_eq!(extract_inline_tags("#a/b"), vec!["a/b".to_string()]);
    }

    #[test]
    fn tag_matches_boundary_cases() {
        // `/` is the ONLY descendant boundary, and the check must stay
        // bounds-safe for a candidate shorter than, equal to, or longer than
        // the selection.
        assert!(tag_matches("a/b", "a"), "direct child");
        assert!(
            !tag_matches("ab", "a"),
            "no `/` boundary is not a descendant"
        );
        assert!(
            !tag_matches("a", "a/b"),
            "candidate shorter than the selection"
        );
        assert!(tag_matches("", ""), "identical empty strings match");
        assert!(
            !tag_matches("a", ""),
            "an empty selection is not a `/` parent of `a`"
        );
        assert!(tag_matches("/a", ""), "`/a` does start at the `/` boundary");
        assert!(
            tag_matches("a/", "a"),
            "a trailing separator is a descendant form"
        );
    }

    #[test]
    fn hyphen_and_underscore_tags() {
        assert_eq!(
            extract_inline_tags("#to-do #in_progress"),
            vec!["in_progress".to_string(), "to-do".to_string()]
        );
    }
}
