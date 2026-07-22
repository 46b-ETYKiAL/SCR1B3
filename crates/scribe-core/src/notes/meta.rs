//! Per-note metadata derivation (title + tag set) from note content.
//!
//! Composes the existing pure parsers ([`super::frontmatter`], [`super::tags`])
//! into the two summary fields the note-list / search UI needs: a display TITLE
//! and the note's full TAG set. Pure — no I/O — so it is unit-testable against
//! fixed content.

use crate::notes::{frontmatter, tags};
use std::collections::BTreeSet;

/// The display title for a note: the frontmatter `title:` if present, otherwise
/// the first Markdown ATX heading (`# …`) in the body, otherwise `fallback_stem`
/// (the file stem). Never returns an empty string as long as `fallback_stem` is
/// non-empty.
#[must_use]
pub fn note_title(content: &str, fallback_stem: &str) -> String {
    let fm = frontmatter::parse(content);
    if let Some(t) = fm.title.as_deref() {
        let t = t.trim();
        if !t.is_empty() {
            return t.to_string();
        }
    }
    let body_start = fm.body_start.min(content.len());
    for line in content[body_start..].lines() {
        if let Some(title) = atx_heading_text(line) {
            return title.to_string();
        }
    }
    fallback_stem.trim().to_string()
}

/// If `line` is an ATX heading (`#`..`######` followed by a space), return the
/// trimmed heading text (non-empty), else `None`.
fn atx_heading_text(line: &str) -> Option<&str> {
    let l = line.trim_start();
    let hashes = l.len() - l.trim_start_matches('#').len();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let after = &l[hashes..];
    // A real ATX heading has a space after the run of `#` (excludes `#tag`).
    let text = after.strip_prefix(' ')?.trim();
    (!text.is_empty()).then_some(text)
}

/// The note's full tag set: frontmatter `tags:` plus every inline `#tag`, deduped
/// and sorted, WITHOUT the leading `#`. This is the set a `tag:` search operator
/// (and the tag surface) matches against.
#[must_use]
pub fn note_tags(content: &str) -> Vec<String> {
    let fm = frontmatter::parse(content);
    let mut set: BTreeSet<String> = fm.tags.into_iter().collect();
    for t in tags::extract_inline_tags(content) {
        set.insert(t);
    }
    set.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_title_wins() {
        let c = "---\ntitle: My Note\n---\n# Other Heading\nbody";
        assert_eq!(note_title(c, "file"), "My Note");
    }

    #[test]
    fn first_heading_when_no_frontmatter_title() {
        let c = "intro line\n## The Real Heading\nmore";
        assert_eq!(note_title(c, "file"), "The Real Heading");
    }

    #[test]
    fn falls_back_to_stem() {
        assert_eq!(note_title("just body, no heading", "my-file"), "my-file");
        // A `#tag` is not a heading (no space) → still falls back.
        assert_eq!(note_title("#tag only\nbody", "stem"), "stem");
    }

    #[test]
    fn heading_inside_frontmatter_is_not_the_title() {
        // The `#` inside the frontmatter block must not be read as a body heading;
        // parsing starts at body_start.
        let c = "---\ntags: [a]\n---\n# Body Heading\n";
        assert_eq!(note_title(c, "stem"), "Body Heading");
    }

    #[test]
    fn tags_merge_frontmatter_and_inline() {
        let c = "---\ntags: [project, idea]\n---\nbody with #inline and #project\n";
        assert_eq!(
            note_tags(c),
            vec![
                "idea".to_string(),
                "inline".to_string(),
                "project".to_string()
            ]
        );
    }

    #[test]
    fn tags_empty_when_none() {
        assert!(note_tags("plain body, no tags").is_empty());
    }
}
