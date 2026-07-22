//! Cross-note search query parsing + matching.
//!
//! A search string is parsed into a set of AND-ed clauses. Each clause is either
//! a field operator or a free-text term, and may be negated with a leading `-`:
//!
//! ```text
//! tag:project          — the note carries `#project` (or a descendant `#project/…`)
//! path:2026/           — the note's vault-relative path contains `2026/`
//! title:roadmap        — the note's title contains `roadmap`
//! "exact phrase"       — the body or title contains that exact phrase (spaces kept)
//! meeting              — a bare word: body or title contains it
//! -tag:archive         — the note does NOT carry `#archive` (negation)
//! ```
//!
//! Everything is case-insensitive. Field values may themselves be quoted
//! (`title:"my note"`). All clauses must match (AND); an empty query matches
//! every note. Pure — no I/O — so the operator grammar is exhaustively testable
//! against fixed inputs.
//!
//! This is a QUERY LAYER over the existing note parsers ([`super::tags`] for tag
//! semantics), not a second text-search engine: the free-text match is a plain
//! case-insensitive substring test, and callers reuse the vault walk / find
//! machinery to produce the [`NoteRef`]s fed in here.

use crate::notes::tags::tag_matches;

/// One field a query clause can target, plus free text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Term {
    /// `tag:x` — matches a note tag equal to `x` or nested under it (`x/…`).
    Tag(String),
    /// `path:x` — substring of the note's vault-relative path.
    Path(String),
    /// `title:x` — substring of the note's title.
    Title(String),
    /// A bare word or `"quoted phrase"` — substring of the note's title OR body.
    Text(String),
}

/// One AND-ed clause: a [`Term`], optionally negated (`-` prefix).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clause {
    /// True when the clause was written with a leading `-` (the note must NOT
    /// match the term).
    pub negated: bool,
    /// The field / text this clause tests.
    pub term: Term,
}

/// A parsed search query: a conjunction of [`Clause`]s.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NoteQuery {
    /// The AND-ed clauses. Empty ⇒ the query matches every note.
    pub clauses: Vec<Clause>,
}

/// The per-note fields a query is evaluated against. Borrowed so the caller's
/// note index owns the strings.
#[derive(Debug, Clone, Copy)]
pub struct NoteRef<'a> {
    /// Vault-relative path, using `/` separators (e.g. `projects/roadmap.md`).
    pub rel_path: &'a str,
    /// Display title (frontmatter `title:`, first heading, or file stem).
    pub title: &'a str,
    /// The note's tags (frontmatter + inline), WITHOUT the leading `#`.
    pub tags: &'a [String],
    /// The note body (used for free-text matching).
    pub body: &'a str,
}

/// Parse a raw search string into a [`NoteQuery`].
///
/// Tokenisation keeps a `"quoted phrase"` together (spaces preserved), including
/// after a field operator (`title:"my note"`). A lone `-` or an operator with an
/// empty value contributes no clause (it names nothing), so a half-typed query
/// never spuriously matches nothing.
#[must_use]
pub fn parse(input: &str) -> NoteQuery {
    let mut clauses = Vec::new();
    for word in tokenize(input) {
        if let Some(clause) = parse_word(&word) {
            clauses.push(clause);
        }
    }
    NoteQuery { clauses }
}

/// Split `input` into words on whitespace, keeping `"…"`-quoted runs (including
/// their internal spaces) as a single word. The surrounding quotes are retained
/// in the emitted word so [`parse_word`] can tell a quoted value from a bare one;
/// they are stripped there.
fn tokenize(input: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    let mut has_content = false;
    for c in input.chars() {
        match c {
            '"' => {
                in_quote = !in_quote;
                cur.push(c);
                has_content = true;
            }
            c if c.is_whitespace() && !in_quote => {
                if has_content {
                    words.push(std::mem::take(&mut cur));
                    has_content = false;
                }
            }
            c => {
                cur.push(c);
                has_content = true;
            }
        }
    }
    if has_content {
        words.push(cur);
    }
    words
}

/// Parse a single tokenised word into a [`Clause`], or `None` when it names
/// nothing (a lone `-`, an empty operator value, or an empty quote).
fn parse_word(word: &str) -> Option<Clause> {
    let (negated, rest) = match word.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, word),
    };
    if rest.is_empty() {
        return None;
    }
    // A recognised `field:value` operator (the value may itself be quoted).
    if let Some((key, value)) = split_operator(rest) {
        let value = unquote(value);
        if value.is_empty() {
            return None;
        }
        let term = match key {
            "tag" => Term::Tag(value),
            "path" => Term::Path(value),
            "title" => Term::Title(value),
            // An unrecognised `foo:bar` is treated as free text (keep the colon).
            _ => Term::Text(unquote(rest)),
        };
        return Some(Clause { negated, term });
    }
    let text = unquote(rest);
    if text.is_empty() {
        return None;
    }
    Some(Clause {
        negated,
        term: Term::Text(text),
    })
}

/// Split a `key:value` operator when `key` is a bare (unquoted) identifier
/// followed by `:`. Returns `None` when there is no colon or the key side is
/// quoted (`"a:b"` is a phrase, not an operator).
fn split_operator(s: &str) -> Option<(&str, &str)> {
    let colon = s.find(':')?;
    let key = &s[..colon];
    // The key must be a plain word — no quote, no space — or it is not an operator.
    if key.is_empty() || key.contains('"') || key.contains(char::is_whitespace) {
        return None;
    }
    Some((key, &s[colon + 1..]))
}

/// Strip one layer of surrounding double quotes and lowercase for
/// case-insensitive matching. `"Foo Bar"` → `foo bar`; `Foo` → `foo`.
fn unquote(s: &str) -> String {
    let trimmed = s
        .strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .unwrap_or(s);
    trimmed.to_lowercase()
}

impl NoteQuery {
    /// True when `note` satisfies every clause (AND). An empty query matches all.
    #[must_use]
    pub fn matches(&self, note: NoteRef<'_>) -> bool {
        self.clauses
            .iter()
            .all(|clause| clause.negated ^ term_matches(&clause.term, note))
    }

    /// True when the query has no clauses (matches every note) — the caller can
    /// short-circuit the whole-vault scan.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.clauses.is_empty()
    }
}

/// Does `note` satisfy `term` (ignoring negation, which the caller applies)?
fn term_matches(term: &Term, note: NoteRef<'_>) -> bool {
    match term {
        Term::Tag(t) => note
            .tags
            .iter()
            .any(|nt| tag_matches(&nt.to_lowercase(), t)),
        Term::Path(p) => note.rel_path.to_lowercase().contains(p.as_str()),
        Term::Title(t) => note.title.to_lowercase().contains(t.as_str()),
        Term::Text(s) => {
            note.title.to_lowercase().contains(s.as_str())
                || note.body.to_lowercase().contains(s.as_str())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note<'a>(rel: &'a str, title: &'a str, tags: &'a [String], body: &'a str) -> NoteRef<'a> {
        NoteRef {
            rel_path: rel,
            title,
            tags,
            body,
        }
    }

    #[test]
    fn empty_query_matches_everything() {
        let q = parse("   ");
        assert!(q.is_empty());
        assert!(q.matches(note("a.md", "A", &[], "body")));
    }

    #[test]
    fn bare_word_matches_body_or_title() {
        let q = parse("roadmap");
        assert!(q.matches(note("x.md", "Q3 Roadmap", &[], "content")));
        assert!(q.matches(note("x.md", "Other", &[], "the ROADMAP is here")));
        assert!(!q.matches(note("x.md", "Other", &[], "nothing relevant")));
    }

    #[test]
    fn quoted_phrase_keeps_internal_spaces() {
        let q = parse("\"exact phrase here\"");
        assert_eq!(q.clauses.len(), 1);
        assert_eq!(q.clauses[0].term, Term::Text("exact phrase here".into()));
        assert!(q.matches(note("x.md", "t", &[], "an exact phrase here yes")));
        assert!(!q.matches(note("x.md", "t", &[], "exact phrase")));
    }

    #[test]
    fn tag_operator_matches_tag_and_descendants() {
        let tags = vec!["project/frontend".to_string(), "idea".to_string()];
        assert!(parse("tag:project").matches(note("x.md", "t", &tags, "")));
        assert!(parse("tag:project/frontend").matches(note("x.md", "t", &tags, "")));
        assert!(!parse("tag:archive").matches(note("x.md", "t", &tags, "")));
        // Case-insensitive.
        assert!(parse("tag:PROJECT").matches(note("x.md", "t", &tags, "")));
    }

    #[test]
    fn path_and_title_operators() {
        let n = note("journal/2026/day.md", "Morning Pages", &[], "");
        assert!(parse("path:2026/").matches(n));
        assert!(!parse("path:2025/").matches(n));
        assert!(parse("title:morning").matches(n));
        assert!(!parse("title:evening").matches(n));
    }

    #[test]
    fn title_operator_can_be_quoted() {
        let q = parse("title:\"Morning Pages\"");
        assert_eq!(q.clauses[0].term, Term::Title("morning pages".into()));
        assert!(q.matches(note("x.md", "Morning Pages Draft", &[], "")));
    }

    #[test]
    fn negation_excludes() {
        let tags = vec!["archive".to_string()];
        // -tag:archive must EXCLUDE a note that has #archive.
        assert!(!parse("-tag:archive").matches(note("x.md", "t", &tags, "")));
        assert!(parse("-tag:archive").matches(note("x.md", "t", &[], "")));
        // Negated free text.
        assert!(!parse("-draft").matches(note("x.md", "t", &[], "this is a draft")));
        assert!(parse("-draft").matches(note("x.md", "t", &[], "final copy")));
    }

    #[test]
    fn clauses_are_anded() {
        let tags = vec!["project".to_string()];
        let q = parse("tag:project roadmap -tag:archive");
        assert!(q.matches(note("x.md", "The Roadmap", &tags, "")));
        // Missing the free-text term → fails the AND.
        assert!(!q.matches(note("x.md", "Unrelated", &tags, "nope")));
        // Has the excluded tag → fails.
        let both = vec!["project".to_string(), "archive".to_string()];
        assert!(!q.matches(note("x.md", "The Roadmap", &both, "")));
    }

    #[test]
    fn lone_dash_and_empty_operator_contribute_no_clause() {
        assert!(parse("-").is_empty());
        assert!(parse("tag:").is_empty());
        assert!(parse("title:\"\"").is_empty());
        // A real term next to a noise token still parses to exactly one clause.
        assert_eq!(parse("- roadmap").clauses.len(), 1);
    }

    #[test]
    fn unrecognised_operator_is_free_text() {
        // `foo:bar` is not a known field, so it is matched as literal text
        // (colon included) against the body — never silently dropped.
        let q = parse("foo:bar");
        assert_eq!(q.clauses[0].term, Term::Text("foo:bar".into()));
        assert!(q.matches(note("x.md", "t", &[], "see foo:bar there")));
    }

    #[test]
    fn quoted_key_is_a_phrase_not_an_operator() {
        // `"a:b"` — the colon is inside quotes, so it is a phrase, not an operator.
        let q = parse("\"a:b\"");
        assert_eq!(q.clauses[0].term, Term::Text("a:b".into()));
    }
}
