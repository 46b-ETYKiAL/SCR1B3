//! Sigil-triggered completion source for note surfaces (`#tag`, `[[note`).
//!
//! Given the text being typed and a caret BYTE offset, [`complete`] decides
//! whether the caret sits inside an unfinished `#tag` or `[[note` token and, if
//! so, reports the byte span that token occupies plus the ranked candidates for
//! it. [`apply`] splices a chosen replacement over that span and returns a NEW
//! string — this module never mutates the caller's buffer and never decides what
//! the replacement text should be, so the same source serves a search box (which
//! wants a `tag:` operator) and an editor (which wants `[[Note]]`).
//!
//! Pure — no I/O, no state.
//!
//! # Recognition rules
//!
//! - **`#tag`** — the `#` must sit at a word boundary (start of text, or after
//!   whitespace or an opening delimiter) and everything between it and the caret
//!   must be a tag character. This is the SAME boundary + character class
//!   [`super::tags`] extracts with, so a completion is never offered for text
//!   that could not parse as a tag (`page#frag`, `#123`'s host contexts).
//! - **`[[note`** — the caret is inside the most recent `[[` that has not been
//!   closed by a `]]` and does not span a newline. A partially typed link that
//!   already carries a `#heading` or `|alias` is NOT completed: this module has
//!   no heading index, and replacing the span would silently discard the anchor
//!   or alias the user already typed.
//! - **An unclosed `[[` OWNS the caret.** A `#` inside a wiki-link is a heading
//!   anchor, not a tag, so the link context is resolved FIRST and an
//!   uncompletable link yields nothing at all — it never falls through to the
//!   tag branch and offers to rewrite an anchor as a tag.

use super::tags::is_tag_char;

/// Which sigil opened the completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// An inline `#tag`.
    Tag,
    /// A `[[note` wiki-link target.
    Note,
}

/// An active completion: the span to replace and the candidates for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    /// Which sigil is being completed.
    pub trigger: Trigger,
    /// Byte offset of the sigil itself (`#`, or the first `[` of `[[`). The
    /// replacement span STARTS here, so an accepted candidate replaces the
    /// sigil too and the caller decides the final form.
    pub start: usize,
    /// Byte offset just past the typed text — the caret.
    pub end: usize,
    /// The text already typed after the sigil, lowercased. Empty right after the
    /// sigil itself, which offers every candidate.
    pub query: String,
    /// Ranked candidates: prefix matches first, then substring matches, each
    /// group sorted case-insensitively. Capped by the caller's `max`.
    pub candidates: Vec<String>,
}

/// Resolve the completion active at `caret` in `text`, or `None`.
///
/// `tags` and `notes` are the candidate pools for the two triggers (a vault's
/// tag paths and note titles). Returns `None` when no sigil context is open,
/// when nothing matches what has been typed, when `max` is 0, or when `caret` is
/// out of range / not on a `char` boundary (a stale caret is dropped rather than
/// panicking a slice).
#[must_use]
pub fn complete(
    text: &str,
    caret: usize,
    tags: &[String],
    notes: &[String],
    max: usize,
) -> Option<Suggestion> {
    if caret > text.len() || !text.is_char_boundary(caret) {
        return None;
    }
    let head = &text[..caret];
    // An unclosed `[[` OWNS the caret. Resolving it first — and returning `None`
    // rather than falling through when it cannot be completed — is what stops a
    // `#heading` anchor inside a link being offered tag completions.
    if let Some(start) = enclosing_link_start(head) {
        let inner = &head[start + 2..];
        if inner.contains('#') || inner.contains('|') {
            return None;
        }
        return build(
            Trigger::Note,
            start,
            caret,
            inner.to_lowercase(),
            notes,
            max,
        );
    }
    let start = open_tag_start(head)?;
    let query = head[start + 1..].to_lowercase();
    build(Trigger::Tag, start, caret, query, tags, max)
}

/// Assemble a [`Suggestion`], or `None` when nothing matches.
fn build(
    trigger: Trigger,
    start: usize,
    end: usize,
    query: String,
    pool: &[String],
    max: usize,
) -> Option<Suggestion> {
    let candidates = rank(pool, &query, max);
    (!candidates.is_empty()).then_some(Suggestion {
        trigger,
        start,
        end,
        query,
        candidates,
    })
}

/// Byte offset of the `[[` the caret sits inside, or `None`.
///
/// The most recent `[[` wins; it encloses the caret only when the text after it
/// holds no `]]` (the link is already closed, so the caret is past it) and no
/// newline (a wiki-link never spans lines). Whether that enclosing link can be
/// COMPLETED is a separate question the caller answers — enclosure alone is
/// enough to deny the tag branch.
fn enclosing_link_start(head: &str) -> Option<usize> {
    let start = head.rfind("[[")?;
    let inner = &head[start + 2..];
    (!inner.contains("]]") && !inner.contains('\n')).then_some(start)
}

/// Byte offset of the `#` whose tag the caret is inside, or `None`.
///
/// Walks back over tag characters to the sigil and then applies the extractor's
/// word-boundary rule, so `page#frag` (mid-word) never opens a completion.
fn open_tag_start(head: &str) -> Option<usize> {
    let mut idx = head.len();
    for (i, c) in head.char_indices().rev() {
        if !is_tag_char(c) {
            break;
        }
        idx = i;
    }
    // The character immediately before the typed body must be the sigil.
    let sigil = head[..idx].chars().next_back().filter(|c| *c == '#')?;
    debug_assert_eq!(sigil, '#');
    let start = idx - 1; // `#` is one byte
    let boundary = head[..start].chars().next_back().is_none_or(|c| {
        matches!(
            c,
            ' ' | '\t' | '\n' | '\r' | '(' | '[' | '{' | '<' | '"' | '\''
        )
    });
    boundary.then_some(start)
}

/// Rank `pool` against `query`: prefix matches before substring matches, each
/// group sorted case-insensitively, duplicates (case-folded) collapsed, capped
/// at `max`. An empty query offers the whole pool.
fn rank(pool: &[String], query: &str, max: usize) -> Vec<String> {
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut scored: Vec<(u8, String, String)> = Vec::new();
    for candidate in pool {
        let lower = candidate.to_lowercase();
        if !seen.insert(lower.clone()) {
            continue;
        }
        let tier = if query.is_empty() || lower.starts_with(query) {
            0
        } else if lower.contains(query) {
            1
        } else {
            continue;
        };
        scored.push((tier, lower, candidate.clone()));
    }
    scored.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    scored
        .into_iter()
        .take(max)
        .map(|(_, _, candidate)| candidate)
        .collect()
}

/// `text` with `suggestion`'s span replaced by `replacement`.
///
/// The span's ends are byte offsets this module produced (an ASCII sigil and a
/// validated caret), so the splice is always on `char` boundaries.
#[must_use]
pub fn apply(text: &str, suggestion: &Suggestion, replacement: &str) -> String {
    let mut out = String::with_capacity(text.len() + replacement.len());
    out.push_str(&text[..suggestion.start]);
    out.push_str(replacement);
    out.push_str(&text[suggestion.end..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    fn tags() -> Vec<String> {
        pool(&["project", "project/frontend", "errand", "idea"])
    }

    fn notes() -> Vec<String> {
        pool(&["Roadmap", "Groceries", "Road Trip"])
    }

    /// Complete at the END of `text` (the common case: the user is typing).
    fn at_end(text: &str) -> Option<Suggestion> {
        complete(text, text.len(), &tags(), &notes(), 8)
    }

    #[test]
    fn a_bare_hash_offers_every_tag() {
        let s = at_end("#").expect("a sigil alone opens a completion");
        assert_eq!(s.trigger, Trigger::Tag);
        assert_eq!(s.start, 0);
        assert_eq!(s.end, 1);
        assert!(s.query.is_empty());
        assert_eq!(
            s.candidates,
            vec![
                "errand".to_string(),
                "idea".to_string(),
                "project".to_string(),
                "project/frontend".to_string()
            ],
            "an empty query offers the whole pool, sorted"
        );
    }

    #[test]
    fn a_typed_prefix_narrows_the_tags_and_keeps_the_sigil_in_the_span() {
        let s = at_end("some text #pro").expect("an open tag completion");
        assert_eq!(s.trigger, Trigger::Tag);
        assert_eq!(s.query, "pro");
        assert_eq!(
            &"some text #pro"[s.start..s.end],
            "#pro",
            "the span covers the sigil AND the typed body, so accepting replaces both"
        );
        assert_eq!(
            s.candidates,
            vec!["project".to_string(), "project/frontend".to_string()]
        );
    }

    #[test]
    fn a_mid_word_hash_is_not_a_tag_context() {
        // Same rule the extractor applies: `page#frag` / `color#fff` are not
        // tags, so offering completions there would suggest an edit that could
        // never parse as one.
        assert!(at_end("visit page#pro").is_none());
        assert!(at_end("x#pro").is_none());
    }

    #[test]
    fn a_hash_after_an_opening_delimiter_or_a_newline_is_a_tag_context() {
        assert!(at_end("(#pro").is_some(), "after an opening paren");
        assert!(at_end("line one\n#pro").is_some(), "at the start of a line");
        assert!(at_end("  #pro").is_some(), "after whitespace");
    }

    #[test]
    fn a_query_matching_nothing_opens_no_completion() {
        assert!(
            at_end("#zzz").is_none(),
            "an empty candidate list is no suggestion, not an empty popup"
        );
    }

    #[test]
    fn a_substring_match_ranks_below_a_prefix_match() {
        let p = pool(&["frontend", "project/frontend"]);
        let s = complete("#front", 6, &p, &[], 8).expect("a completion");
        assert_eq!(
            s.candidates,
            vec!["frontend".to_string(), "project/frontend".to_string()],
            "the prefix match comes first even though it sorts later"
        );
    }

    #[test]
    fn the_candidate_cap_is_honoured_and_zero_means_no_completion() {
        let s = complete("#", 1, &tags(), &[], 2).expect("a completion");
        assert_eq!(s.candidates.len(), 2, "capped at max");
        assert!(
            complete("#", 1, &tags(), &[], 0).is_none(),
            "a zero cap yields no suggestion rather than an empty one"
        );
    }

    #[test]
    fn case_folded_duplicates_collapse_to_one_candidate() {
        let p = pool(&["Project", "project"]);
        let s = complete("#pro", 4, &p, &[], 8).expect("a completion");
        assert_eq!(s.candidates.len(), 1, "one node, not two spellings");
    }

    #[test]
    fn an_open_double_bracket_completes_note_titles() {
        let s = at_end("see [[Road").expect("an open link completion");
        assert_eq!(s.trigger, Trigger::Note);
        assert_eq!(s.query, "road");
        assert_eq!(
            &"see [[Road"[s.start..s.end],
            "[[Road",
            "the span covers the `[[` so accepting replaces the whole token"
        );
        assert_eq!(
            s.candidates,
            vec!["Road Trip".to_string(), "Roadmap".to_string()]
        );
    }

    #[test]
    fn a_closed_link_is_no_longer_an_open_context() {
        assert!(
            at_end("see [[Roadmap]]").is_none(),
            "the `]]` closed it — the caret is outside the link"
        );
        assert!(
            at_end("see [[Roadmap]] and more").is_none(),
            "still closed once text follows"
        );
    }

    #[test]
    fn a_link_never_spans_a_newline() {
        assert!(at_end("[[Road\nmap").is_none());
    }

    #[test]
    fn the_most_recent_open_bracket_owns_the_caret() {
        let s = at_end("[[Done]] then [[Road").expect("the second link is open");
        assert_eq!(
            &"[[Done]] then [[Road"[s.start..s.end],
            "[[Road",
            "the earlier, closed link must not capture the caret"
        );
    }

    #[test]
    fn a_hash_inside_an_open_link_is_a_heading_anchor_not_a_tag() {
        // The tag pool WOULD match `#pro` here; the link context must win and
        // then decline, rather than offering to rewrite a heading anchor as a
        // tag.
        assert!(
            at_end("[[Roadmap#pro").is_none(),
            "an anchor inside an open link opens no completion at all"
        );
    }

    #[test]
    fn a_tag_sigil_inside_an_open_link_offers_nothing_at_all() {
        // The `#` here sits after a SPACE, so the tag branch would happily claim
        // it on its own — the ONLY thing suppressing it is the enclosing
        // unclosed `[[`. Resolving the link context first is therefore
        // load-bearing, not a stylistic ordering: without it the user is offered
        // a tag rewrite in the middle of a half-typed wiki-link.
        assert!(at_end("[[Note #pro").is_none());
        // Once the link is CLOSED the same text is a genuine tag context again.
        assert!(at_end("[[Note]] #pro").is_some());
    }

    #[test]
    fn a_partially_typed_alias_is_not_completed_over() {
        // Replacing the span here would discard the alias the user already
        // typed.
        assert!(at_end("[[Roadmap|My Ro").is_none());
    }

    #[test]
    fn a_caret_before_the_end_completes_the_text_to_its_left_only() {
        let text = "#pro and trailing words";
        let s = complete(text, 4, &tags(), &notes(), 8).expect("a completion at the caret");
        assert_eq!(s.end, 4, "the span ends at the caret, not at the text end");
        assert_eq!(s.query, "pro");
    }

    #[test]
    fn an_out_of_range_or_mid_char_caret_is_dropped_rather_than_panicking() {
        let text = "#é";
        assert!(
            complete(text, 99, &tags(), &notes(), 8).is_none(),
            "past the end"
        );
        assert!(
            !text.is_char_boundary(2),
            "fixture: byte 2 is inside the 2-byte 'é'"
        );
        assert!(
            complete(text, 2, &tags(), &notes(), 8).is_none(),
            "a caret inside a multi-byte char is dropped, never sliced"
        );
    }

    #[test]
    fn apply_splices_the_replacement_over_the_whole_sigil_span() {
        let text = "find #pro now";
        let s = complete(text, 9, &tags(), &notes(), 8).expect("a completion");
        assert_eq!(
            apply(text, &s, "tag:project"),
            "find tag:project now",
            "the sigil and the typed body are both replaced; the tail survives"
        );
    }

    #[test]
    fn apply_keeps_multibyte_text_on_both_sides_intact() {
        let text = "café #pro ☕";
        let s = complete(text, "café #pro".len(), &tags(), &notes(), 8).expect("a completion");
        assert_eq!(apply(text, &s, "tag:project"), "café tag:project ☕");
    }

    #[test]
    fn plain_text_with_no_sigil_opens_nothing() {
        assert!(at_end("just some words").is_none());
        assert!(at_end("").is_none());
        assert!(
            at_end("[").is_none(),
            "a single bracket is not a link opener"
        );
    }
}
