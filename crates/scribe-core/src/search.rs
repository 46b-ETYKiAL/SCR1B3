//! Find / replace engine: literal or regex, case-sensitive or not, whole-word.
//! Returns byte-offset match spans the UI highlights.
//!
//! # Zero-width (empty) match policy (C-03 / R5)
//!
//! Regex patterns such as `x*`, `a?`, `\b`, `^`, and `$` can match an *empty*
//! span (`start == end`) at one or more offsets. A naive `find_iter` /
//! `Regex::replace_all` reports an empty hit at every position, so a find
//! highlights a zero-width span between every character and `replace_all`
//! *injects* the replacement between every character (e.g.
//! `replace_all("abc", "x*", "-")` -> `"-a-b-c-"`). This is the standard
//! "empty match" footgun.
//!
//! Policy: **both `find_all` and `replace_all` skip zero-width matches
//! entirely** — only non-empty spans (`end > start`) are reported or
//! substituted. An empty match carries no selectable text, so for an editor
//! the least-surprising behavior is for find to highlight only real, navigable
//! spans and for replace to substitute only actual matched text, never
//! inject between characters. Non-empty matches (literals, `a+`, capture
//! groups, etc.) are completely unaffected.

use crate::error::{CoreError, Result};
use regex::RegexBuilder;

#[derive(Debug, Clone, Default)]
pub struct Query {
    pub pattern: String,
    pub regex: bool,
    pub case_sensitive: bool,
    pub whole_word: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Match {
    pub start: usize,
    pub end: usize,
}

fn build_regex(q: &Query) -> Result<regex::Regex> {
    let mut pat = if q.regex {
        q.pattern.clone()
    } else {
        regex::escape(&q.pattern)
    };
    if q.whole_word {
        pat = format!(r"\b(?:{pat})\b");
    }
    RegexBuilder::new(&pat)
        .case_insensitive(!q.case_sensitive)
        .build()
        .map_err(|e| CoreError::Regex(e.to_string()))
}

/// All non-overlapping, **non-empty** matches in `text`.
///
/// Zero-width matches (`start == end`, e.g. from `x*`, `\b`, `^`, `$`) are
/// skipped per the module-level empty-match policy: they carry no selectable
/// text, so an editor must not highlight them. `find_iter` already advances by
/// one codepoint past an empty match (so the scan terminates), so filtering the
/// empty spans out leaves only the real, navigable hits — and the surviving
/// byte offsets always fall on UTF-8 codepoint boundaries.
pub fn find_all(text: &str, q: &Query) -> Result<Vec<Match>> {
    if q.pattern.is_empty() {
        return Ok(Vec::new());
    }
    let re = build_regex(q)?;
    Ok(re
        .find_iter(text)
        .filter(|m| m.end() > m.start())
        .map(|m| Match {
            start: m.start(),
            end: m.end(),
        })
        .collect())
}

/// Replace all **non-empty** matches. For regex queries, `$1` capture refs in
/// `replacement` are honored (regex crate semantics); for LITERAL queries the
/// replacement is substituted verbatim (a `$1` in the replacement text stays
/// `$1` — see [`replace_n`]).
///
/// Zero-width matches are skipped per the module-level empty-match policy, so
/// the replacement is never injected between characters.
pub fn replace_all(text: &str, q: &Query, replacement: &str) -> Result<String> {
    replace_n(text, q, replacement, None)
}

/// Replace at most `max` **non-empty** matches (`None` = every match), left to
/// right. `Some(1)` is the "Replace next" semantics the find bar's single-step
/// replace button drives; `None` is [`replace_all`].
///
/// # Capture expansion is gated on `q.regex`
///
/// For a **regex** query, `$1` / `${name}` refs in `replacement` expand with the
/// regex crate's normal semantics — this is what makes capture-group replacement
/// (`(\w+)@(\w+)` -> `$2.$1`) work from the find bar.
///
/// For a **literal** query the user did not opt into regex syntax, so a `$` in
/// the replacement must land verbatim: replacing `a` with `$1` in a literal
/// search must produce the two characters `$1`, not an empty expansion of a
/// non-existent capture group. The `$` is therefore escaped (`$` -> `$$`, the
/// regex crate's literal-dollar form) before expansion. Without this gate a
/// literal replace silently ate `$`-bearing replacement text.
///
/// Zero-width matches are skipped per the module-level empty-match policy, so
/// the replacement is never injected between characters. The substitution is
/// driven manually (rather than via [`regex::Regex::replace_all`]) so each
/// match can be filtered on its span — and counted against `max` — before
/// deciding whether to substitute; `Captures::expand` provides the same
/// `$N` / `${name}` expansion semantics as the built-in replacer.
pub fn replace_n(text: &str, q: &Query, replacement: &str, max: Option<usize>) -> Result<String> {
    if q.pattern.is_empty() || max == Some(0) {
        return Ok(text.to_string());
    }
    let re = build_regex(q)?;
    // Literal queries never expand capture refs — escape `$` so `expand` emits
    // the replacement verbatim.
    let owned;
    let replacement: &str = if q.regex {
        replacement
    } else {
        owned = replacement.replace('$', "$$");
        &owned
    };

    let mut out = String::with_capacity(text.len());
    let mut last_end = 0usize;
    let mut done = 0usize;
    for caps in re.captures_iter(text) {
        if max.is_some_and(|m| done >= m) {
            break;
        }
        // The overall match is group 0; it always exists for a successful
        // capture, so the `unwrap`-free `get(0)` is guaranteed `Some`.
        let m = caps
            .get(0)
            .expect("regex capture iteration always yields group 0");
        // Skip zero-width matches: copying the unmatched gap between `last_end`
        // and a zero-width match's start (which equals `last_end` when matches
        // are contiguous) plus emitting no replacement leaves the text intact.
        if m.end() == m.start() {
            continue;
        }
        // Copy the text between the previous match and this one verbatim, then
        // expand the replacement (with capture refs) for this match.
        out.push_str(&text[last_end..m.start()]);
        caps.expand(replacement, &mut out);
        last_end = m.end();
        done += 1;
    }
    out.push_str(&text[last_end..]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(p: &str) -> Query {
        Query {
            pattern: p.into(),
            ..Default::default()
        }
    }

    #[test]
    fn literal_case_insensitive() {
        let m = find_all("Foo foo FOO", &q("foo")).unwrap();
        assert_eq!(m.len(), 3);
    }

    #[test]
    fn case_sensitive() {
        let query = Query {
            pattern: "foo".into(),
            case_sensitive: true,
            ..Default::default()
        };
        assert_eq!(find_all("Foo foo FOO", &query).unwrap().len(), 1);
    }

    #[test]
    fn regex_groups_replace() {
        let query = Query {
            pattern: r"(\w+)@(\w+)".into(),
            regex: true,
            ..Default::default()
        };
        let out = replace_all("a@b c@d", &query, "$2.$1").unwrap();
        assert_eq!(out, "b.a d.c");
    }

    #[test]
    fn whole_word() {
        let query = Query {
            pattern: "cat".into(),
            whole_word: true,
            ..Default::default()
        };
        assert_eq!(find_all("cat category cat", &query).unwrap().len(), 2);
    }

    #[test]
    fn bad_regex_is_error() {
        let query = Query {
            pattern: "(".into(),
            regex: true,
            ..Default::default()
        };
        assert!(find_all("x", &query).is_err());
    }

    // --- Zero-width / empty-match handling (C-03 / R5) -----------------------
    //
    // Policy: a zero-width match (start == end) carries no selectable text, so
    // both `find_all` and `replace_all` SKIP empty matches entirely. Only
    // non-empty spans (end > start) are reported / substituted. This is the
    // least-surprising behavior for an editor: find highlights real, navigable
    // spans; replace substitutes actual matched text and never injects between
    // characters. See the module-level doc and the empty-match filter in
    // `find_all` / `replace_all`.

    fn rq(p: &str) -> Query {
        Query {
            pattern: p.into(),
            regex: true,
            ..Default::default()
        }
    }

    #[test]
    fn zero_width_star_yields_no_matches() {
        // `x*` matches an empty span at every offset under the regex crate.
        // The correct editor behavior is: no selectable hits at all.
        let m = find_all("abc", &rq("x*")).unwrap();
        assert_eq!(
            m,
            Vec::new(),
            "x* must not report empty hits at every offset"
        );
    }

    #[test]
    fn zero_width_star_replace_is_identity() {
        // The footgun: `replace_all("abc", "x*", "-")` previously produced
        // "-a-b-c-". Empty matches must be skipped, leaving the text unchanged.
        let out = replace_all("abc", &rq("x*"), "-").unwrap();
        assert_eq!(out, "abc", "empty matches must not inject replacement");
    }

    #[test]
    fn word_boundary_yields_no_matches() {
        // `\b` is a pure zero-width assertion.
        let m = find_all("a b", &rq(r"\b")).unwrap();
        assert_eq!(m, Vec::new());
        let out = replace_all("a b", &rq(r"\b"), "|").unwrap();
        assert_eq!(out, "a b");
    }

    #[test]
    fn caret_anchor_yields_no_matches() {
        let m = find_all("line", &rq("^")).unwrap();
        assert_eq!(m, Vec::new());
        let out = replace_all("line", &rq("^"), ">").unwrap();
        assert_eq!(out, "line");
    }

    #[test]
    fn dollar_anchor_yields_no_matches() {
        let m = find_all("line", &rq("$")).unwrap();
        assert_eq!(m, Vec::new());
        let out = replace_all("line", &rq("$"), "<").unwrap();
        assert_eq!(out, "line");
    }

    #[test]
    fn multiline_anchors_yield_no_matches() {
        // With the multi-line flag, ^ / $ match at every line boundary, but all
        // are zero-width and must be skipped.
        let m = find_all("a\nb\nc", &rq("(?m)^")).unwrap();
        assert_eq!(m, Vec::new());
        let out = replace_all("a\nb\nc", &rq("(?m)$"), "X").unwrap();
        assert_eq!(out, "a\nb\nc");
    }

    #[test]
    fn star_interleaved_with_literal_keeps_only_real_hits() {
        // `a*` matches "aa", "" (between b and c, etc.), and "a". Only the
        // non-empty runs of 'a' survive.
        let m = find_all("baac", &rq("a*")).unwrap();
        assert_eq!(m, vec![Match { start: 1, end: 3 }]);
        // Replacement substitutes only the real run.
        let out = replace_all("baac", &rq("a*"), "X").unwrap();
        assert_eq!(out, "bXc");
    }

    #[test]
    fn optional_group_skips_empty_alternatives() {
        // `a?` matches "a" then "" repeatedly; only the real "a" counts.
        let m = find_all("xay", &rq("a?")).unwrap();
        assert_eq!(m, vec![Match { start: 1, end: 2 }]);
        let out = replace_all("xay", &rq("a?"), "Z").unwrap();
        assert_eq!(out, "xZy");
    }

    #[test]
    fn multibyte_text_zero_width_unchanged() {
        // Empty matches over multibyte text must not split a UTF-8 codepoint
        // nor inject between graphemes. "café" + "x*".
        let s = "café";
        let m = find_all(s, &rq("x*")).unwrap();
        assert_eq!(m, Vec::new());
        let out = replace_all(s, &rq("x*"), "-").unwrap();
        assert_eq!(out, s);
    }

    #[test]
    fn multibyte_real_match_preserved() {
        // A non-empty match over multibyte text returns correct byte offsets.
        let s = "café"; // 'é' is 2 bytes -> total length 5
        let m = find_all(s, &rq("é")).unwrap();
        assert_eq!(m, vec![Match { start: 3, end: 5 }]);
        let out = replace_all(s, &rq("é"), "e").unwrap();
        assert_eq!(out, "cafe");
    }

    #[test]
    fn empty_input_no_matches() {
        assert_eq!(find_all("", &rq("x*")).unwrap(), Vec::new());
        assert_eq!(replace_all("", &rq("x*"), "-").unwrap(), "");
        assert_eq!(find_all("", &q("foo")).unwrap(), Vec::new());
    }

    #[test]
    fn end_of_string_zero_width_skipped() {
        // `\b` fires at the end-of-string boundary after "word"; zero-width,
        // skipped.
        let out = replace_all("word", &rq(r"d\b"), "D").unwrap();
        // `d\b` is a *non-empty* match ("d") at end-of-string -> substituted.
        assert_eq!(out, "worD");
        // Pure end anchor stays zero-width -> skipped.
        let out2 = replace_all("word", &rq("$"), "!").unwrap();
        assert_eq!(out2, "word");
    }

    // --- Regression lock: ordinary (non-empty) search/replace is UNCHANGED ---

    #[test]
    fn regression_literal_search_unchanged() {
        assert_eq!(find_all("Foo foo FOO", &q("foo")).unwrap().len(), 3);
    }

    #[test]
    fn regression_a_plus_search_unchanged() {
        // `a+` is always non-empty; behavior must be identical to before.
        let m = find_all("baaab", &rq("a+")).unwrap();
        assert_eq!(m, vec![Match { start: 1, end: 4 }]);
        let out = replace_all("baaab", &rq("a+"), "X").unwrap();
        assert_eq!(out, "bXb");
    }

    #[test]
    fn regression_group_replace_unchanged() {
        let query = Query {
            pattern: r"(\w+)@(\w+)".into(),
            regex: true,
            ..Default::default()
        };
        let out = replace_all("a@b c@d", &query, "$2.$1").unwrap();
        assert_eq!(out, "b.a d.c");
    }

    // --- `replace_n` bound + literal-`$` gating -----------------------------

    #[test]
    fn replace_n_one_substitutes_only_the_first_match() {
        // "Replace next" semantics: exactly one substitution, the rest verbatim.
        let out = replace_n("alpha alpha alpha", &q("alpha"), "beta", Some(1)).unwrap();
        assert_eq!(out, "beta alpha alpha");
    }

    #[test]
    fn replace_n_none_is_replace_all() {
        assert_eq!(
            replace_n("alpha alpha", &q("alpha"), "beta", None).unwrap(),
            replace_all("alpha alpha", &q("alpha"), "beta").unwrap()
        );
    }

    #[test]
    fn replace_n_zero_is_identity() {
        assert_eq!(
            replace_n("alpha alpha", &q("alpha"), "beta", Some(0)).unwrap(),
            "alpha alpha"
        );
    }

    #[test]
    fn replace_n_cap_above_match_count_replaces_everything() {
        assert_eq!(replace_n("a a a", &q("a"), "b", Some(99)).unwrap(), "b b b");
    }

    #[test]
    fn replace_n_one_skips_zero_width_before_counting() {
        // `a*` matches empty at offset 0 of "ba"; the empty hit must not consume
        // the single-replacement budget — the real "a" run must still be hit.
        let out = replace_n("ba", &rq("a*"), "X", Some(1)).unwrap();
        assert_eq!(out, "bX");
    }

    #[test]
    fn replace_n_first_match_under_regex_expands_captures() {
        let query = Query {
            pattern: r"(\w+)@(\w+)".into(),
            regex: true,
            ..Default::default()
        };
        let out = replace_n("a@b c@d", &query, "$2.$1", Some(1)).unwrap();
        assert_eq!(out, "b.a c@d");
    }

    #[test]
    fn literal_query_does_not_expand_dollar_refs() {
        // A LITERAL search must splice the replacement verbatim: `$1` is two
        // characters, not an expansion of a non-existent capture group. Before
        // the `q.regex` gate this silently produced "X" (the group-1 expansion
        // of nothing) and ate the user's text.
        let out = replace_all("a", &q("a"), "$1").unwrap();
        assert_eq!(out, "$1", "a literal replacement must not expand `$1`");
        let out2 = replace_all("cost", &q("cost"), "$5.00").unwrap();
        assert_eq!(out2, "$5.00", "a literal `$5.00` must survive intact");
    }

    #[test]
    fn regex_query_still_expands_dollar_refs() {
        // The gate must not disable capture expansion for real regex queries.
        let query = Query {
            pattern: r"(\w+)@(\w+)".into(),
            regex: true,
            ..Default::default()
        };
        assert_eq!(
            replace_all("a@b", &query, "$2.$1").unwrap(),
            "b.a",
            "regex mode must still expand capture refs"
        );
    }

    #[test]
    fn regression_whole_word_unchanged() {
        let query = Query {
            pattern: "cat".into(),
            whole_word: true,
            ..Default::default()
        };
        assert_eq!(find_all("cat category cat", &query).unwrap().len(), 2);
    }
}
