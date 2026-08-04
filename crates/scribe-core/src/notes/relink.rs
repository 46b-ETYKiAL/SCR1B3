//! Rewrite `[[wiki-link]]` targets across note content — the pure half of
//! rename-with-link-refactor.
//!
//! Renaming a note without rewriting the links that point at it silently breaks
//! every backlink in the vault: the link still parses, still renders, and now
//! resolves to nothing. So the rename is not "move the file", it is "move the
//! file AND retarget its inbound links".
//!
//! This module owns only the string surgery. It takes a caller-supplied
//! predicate over the RAW link target, because deciding whether `[[Roadmap]]`,
//! `[[projects/Roadmap]]` and `[[Roadmap.md]]` name the same note is a
//! filesystem question that must go through
//! [`crate::notes::vault_path::resolve_in_vault`] — the single traversal-safety
//! chokepoint. Keeping that decision outside means this file does no I/O and
//! cannot itself widen the vault boundary.
//!
//! A matched link is REBUILT from its parsed parts rather than patched in
//! place, so the `#heading` anchor, the `|display` alias and the `!` embed
//! marker all survive a retarget. The rebuild normalises interior whitespace
//! (`[[ Old | Alias ]]` becomes `[[New|Alias]]`), which is the canonical form
//! the parser already reports.

use super::wikilink::{extract_wikilinks, WikiLink};

/// The result of a retarget pass over one note's content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retarget {
    /// The rewritten content.
    pub text: String,
    /// How many links were retargeted (always >= 1; a zero-match pass returns
    /// `None` instead, so a caller never rewrites a file it did not change).
    pub count: usize,
}

/// Render one link back to source with `new_target` substituted for its target,
/// preserving the embed marker, heading anchor and display alias.
#[must_use]
fn rebuild(link: &WikiLink, new_target: &str) -> String {
    let mut s = String::with_capacity(new_target.len() + 8);
    if link.embed {
        s.push('!');
    }
    s.push_str("[[");
    s.push_str(new_target);
    if let Some(h) = &link.heading {
        s.push('#');
        s.push_str(h);
    }
    if let Some(d) = &link.display {
        s.push('|');
        s.push_str(d);
    }
    s.push_str("]]");
    s
}

/// Retarget every `[[wiki-link]]` in `content` whose raw target satisfies
/// `matches`, pointing it at `new_target`.
///
/// Returns `None` when nothing matched — the caller must then leave the file
/// byte-for-byte alone rather than rewriting it with identical content (an
/// unnecessary write churns mtimes and would make every note in the vault look
/// externally modified after a rename).
///
/// `matches` receives the RAW, untrimmed-of-nothing target string exactly as it
/// appears between the brackets (already whitespace-trimmed by the parser); for
/// an intra-note `[[#Heading]]` link that is the empty string.
pub fn retarget_wikilinks<F>(content: &str, new_target: &str, mut matches: F) -> Option<Retarget>
where
    F: FnMut(&str) -> bool,
{
    let links = extract_wikilinks(content);
    let mut out = String::with_capacity(content.len());
    let mut cursor = 0usize;
    let mut count = 0usize;
    for link in &links {
        if !matches(&link.target) {
            continue;
        }
        // Spans are in document order and never overlap (the parser jumps past
        // each whole `[[...]]`), so a single forward walk is sufficient.
        out.push_str(&content[cursor..link.start]);
        out.push_str(&rebuild(link, new_target));
        cursor = link.end;
        count += 1;
    }
    if count == 0 {
        return None;
    }
    out.push_str(&content[cursor..]);
    Some(Retarget { text: out, count })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Match exactly one literal target — the simplest honest predicate.
    fn only(target: &'static str) -> impl FnMut(&str) -> bool {
        move |t: &str| t == target
    }

    #[test]
    fn plain_link_is_retargeted() {
        let r = retarget_wikilinks("see [[Old]] here", "New", only("Old")).unwrap();
        assert_eq!(r.text, "see [[New]] here");
        assert_eq!(r.count, 1);
    }

    #[test]
    fn display_alias_survives() {
        // A patch that replaced the whole `[[...]]` span with `[[New]]` would
        // silently drop the alias the user typed.
        let r = retarget_wikilinks("[[Old|The Old One]]", "New", only("Old")).unwrap();
        assert_eq!(r.text, "[[New|The Old One]]");
    }

    #[test]
    fn heading_anchor_survives() {
        let r = retarget_wikilinks("[[Old#Section 2]]", "New", only("Old")).unwrap();
        assert_eq!(r.text, "[[New#Section 2]]");
    }

    #[test]
    fn heading_and_alias_survive_together_in_order() {
        let r = retarget_wikilinks("[[Old#Sec|Alias]]", "New", only("Old")).unwrap();
        assert_eq!(r.text, "[[New#Sec|Alias]]");
    }

    #[test]
    fn embed_marker_survives() {
        // Dropping the `!` turns a transclusion into a plain link — a visible
        // content change, not a rename.
        let r = retarget_wikilinks("![[Old]]", "New", only("Old")).unwrap();
        assert_eq!(r.text, "![[New]]");
    }

    #[test]
    fn non_matching_note_returns_none_so_the_file_is_never_rewritten() {
        assert!(retarget_wikilinks("[[Other]] and [[Third]]", "New", only("Old")).is_none());
        assert!(retarget_wikilinks("no links at all", "New", only("Old")).is_none());
    }

    #[test]
    fn only_matching_links_change_and_the_rest_is_byte_identical() {
        let src = "a [[Keep]] b [[Old]] c [[Keep|x]] d";
        let r = retarget_wikilinks(src, "New", only("Old")).unwrap();
        assert_eq!(r.text, "a [[Keep]] b [[New]] c [[Keep|x]] d");
        assert_eq!(r.count, 1);
    }

    #[test]
    fn every_occurrence_is_retargeted_and_counted() {
        let r = retarget_wikilinks("[[Old]] [[Old|a]] ![[Old#h]]", "New", only("Old")).unwrap();
        assert_eq!(r.text, "[[New]] [[New|a]] ![[New#h]]");
        assert_eq!(r.count, 3);
    }

    #[test]
    fn adjacent_links_do_not_swallow_each_other() {
        let r = retarget_wikilinks("[[Old]][[Old]]", "New", only("Old")).unwrap();
        assert_eq!(r.text, "[[New]][[New]]");
        assert_eq!(r.count, 2);
    }

    #[test]
    fn link_at_the_very_start_and_end_of_the_document() {
        let r = retarget_wikilinks("[[Old]]", "New", only("Old")).unwrap();
        assert_eq!(r.text, "[[New]]");
        let r = retarget_wikilinks("tail [[Old]]", "New", only("Old")).unwrap();
        assert_eq!(r.text, "tail [[New]]");
    }

    #[test]
    fn multibyte_content_around_a_link_is_preserved() {
        // Spans are BYTE offsets; a char-index walk would slice mid-codepoint.
        let r = retarget_wikilinks("日本語 [[Old]] 日本語", "新しい", only("Old")).unwrap();
        assert_eq!(r.text, "日本語 [[新しい]] 日本語");
    }

    #[test]
    fn a_subfolder_target_is_written_out_verbatim() {
        let r = retarget_wikilinks("[[Old]]", "projects/New", only("Old")).unwrap();
        assert_eq!(r.text, "[[projects/New]]");
    }

    #[test]
    fn the_predicate_sees_the_raw_target_including_a_path_form() {
        // The vault decides equivalence, not this module: a predicate that
        // accepts several spellings retargets all of them.
        let seen = std::cell::RefCell::new(Vec::new());
        let r = retarget_wikilinks("[[Old]] [[folder/Old]] [[Old.md]] [[#Local]]", "New", |t| {
            seen.borrow_mut().push(t.to_string());
            t.ends_with("Old") || t == "Old.md"
        })
        .unwrap();
        assert_eq!(r.text, "[[New]] [[New]] [[New]] [[#Local]]");
        assert_eq!(r.count, 3);
        assert_eq!(
            *seen.borrow(),
            vec!["Old", "folder/Old", "Old.md", ""],
            "the intra-note link offers an empty target for the vault to reject"
        );
    }

    #[test]
    fn interior_whitespace_is_normalised_to_the_parsed_form() {
        let r = retarget_wikilinks("[[ Old | Alias ]]", "New", only("Old")).unwrap();
        assert_eq!(r.text, "[[New|Alias]]");
    }
}
