//! Minimal YAML frontmatter extraction for notes.
//!
//! A note may open with a `---` fenced YAML block:
//!
//! ```text
//! ---
//! title: My Note
//! tags: [project, idea]
//! ---
//! body…
//! ```
//!
//! This parses ONLY the two fields the note layer needs — `title` and `tags` —
//! plus the byte offset where the body begins (so callers can index it out).
//! It is intentionally NOT a general YAML parser (no new dependency): it
//! recognises `key: scalar`, inline flow sequences `tags: [a, b]`, and block
//! sequences (`- item` on following lines). Anything else in the block is
//! ignored, never an error. Pure — no I/O.

/// Parsed frontmatter fields the note layer consumes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frontmatter {
    /// The `title:` value, if present (unquoted, trimmed).
    pub title: Option<String>,
    /// Tags declared under a `tags:` key (flow `[a, b]` or block `- a`).
    pub tags: Vec<String>,
    /// Byte offset in the original text where the note BODY begins (just past
    /// the closing `---` line). Zero when there is no frontmatter block.
    pub body_start: usize,
}

/// Parse a leading `---` YAML frontmatter block. When `text` does not begin with
/// a frontmatter fence, returns an empty `Frontmatter` with `body_start == 0`.
#[must_use]
pub fn parse(text: &str) -> Frontmatter {
    let mut fm = Frontmatter::default();
    // The fence must be the very first line: `---` possibly with a trailing CR.
    let mut lines = text.split_inclusive('\n');
    let Some(first) = lines.next() else {
        return fm;
    };
    if first.trim_end() != "---" {
        return fm;
    }
    let mut offset = first.len();
    let mut pending_tags_block = false;
    let mut closed = false;

    for line in lines {
        let content = line.trim_end_matches(['\n', '\r']);
        offset += line.len();
        if content.trim_end() == "---" || content.trim_end() == "..." {
            closed = true;
            break;
        }
        let trimmed = content.trim();
        // Continuation of a `tags:` block sequence.
        if pending_tags_block {
            if let Some(item) = trimmed.strip_prefix("- ") {
                push_tag(&mut fm.tags, item);
                continue;
            } else if trimmed == "-" {
                continue;
            } else if content.starts_with(' ') || content.starts_with('\t') {
                // An indented non-`-` line under tags: — ignore, stay in block.
                continue;
            } else {
                pending_tags_block = false;
                // fall through to parse this line as a new key
            }
        }
        let Some((key, value)) = split_key_value(trimmed) else {
            continue;
        };
        match key {
            "title" => {
                let v = unquote(value.trim());
                if !v.is_empty() {
                    fm.title = Some(v);
                }
            }
            "tags" | "tag" => {
                let v = value.trim();
                if v.is_empty() {
                    pending_tags_block = true; // block sequence follows
                } else if let Some(inner) = v.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                    for item in inner.split(',') {
                        push_tag(&mut fm.tags, item);
                    }
                } else {
                    // A single scalar tag, or a space/comma list.
                    for item in v.split([',', ' ']) {
                        push_tag(&mut fm.tags, item);
                    }
                }
            }
            _ => {}
        }
    }
    // Only report a body_start when the block actually closed; an unterminated
    // `---` is treated as no frontmatter (body is the whole text).
    fm.body_start = if closed { offset } else { 0 };
    if !closed {
        // An unterminated fence is not frontmatter — discard parsed fields.
        return Frontmatter::default();
    }
    fm
}

/// Split `key: value` on the first `:`; returns `None` for a line with no colon.
fn split_key_value(line: &str) -> Option<(&str, &str)> {
    let (k, v) = line.split_once(':')?;
    Some((k.trim(), v))
}

/// Strip matching surrounding single or double quotes.
fn unquote(s: &str) -> String {
    let bytes = s.as_bytes();
    if bytes.len() >= 2
        && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''))
    {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

/// Normalise and push a tag item (trim, strip quotes and a leading `#`), when
/// non-empty and not already present.
fn push_tag(tags: &mut Vec<String>, item: &str) {
    let t = unquote(item.trim());
    let t = t.trim_start_matches('#').trim().to_string();
    if !t.is_empty() && !tags.contains(&t) {
        tags.push(t);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_frontmatter() {
        let fm = parse("# Just a heading\nbody");
        assert_eq!(fm, Frontmatter::default());
        assert_eq!(fm.body_start, 0);
    }

    #[test]
    fn title_and_flow_tags() {
        let text = "---\ntitle: My Note\ntags: [project, idea]\n---\nbody here";
        let fm = parse(text);
        assert_eq!(fm.title.as_deref(), Some("My Note"));
        assert_eq!(fm.tags, vec!["project".to_string(), "idea".to_string()]);
        assert_eq!(&text[fm.body_start..], "body here");
    }

    #[test]
    fn block_sequence_tags() {
        let text = "---\ntitle: T\ntags:\n  - a\n  - b\n  - nested/child\n---\nx";
        let fm = parse(text);
        assert_eq!(
            fm.tags,
            vec!["a".to_string(), "b".to_string(), "nested/child".to_string()]
        );
    }

    #[test]
    fn quoted_title() {
        let fm = parse("---\ntitle: \"Quoted: Title\"\n---\n");
        assert_eq!(fm.title.as_deref(), Some("Quoted: Title"));
    }

    #[test]
    fn single_scalar_tag() {
        let fm = parse("---\ntags: solo\n---\n");
        assert_eq!(fm.tags, vec!["solo".to_string()]);
    }

    #[test]
    fn tags_strip_leading_hash_and_quotes() {
        let fm = parse("---\ntags: ['#a', \"#b\"]\n---\n");
        assert_eq!(fm.tags, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn unterminated_fence_is_not_frontmatter() {
        // No closing `---` — the whole thing is body, no fields parsed.
        let fm = parse("---\ntitle: X\nstill going");
        assert_eq!(fm, Frontmatter::default());
        assert_eq!(fm.body_start, 0);
    }

    #[test]
    fn crlf_fence_handled() {
        let text = "---\r\ntitle: CRLF\r\n---\r\nbody";
        let fm = parse(text);
        assert_eq!(fm.title.as_deref(), Some("CRLF"));
        assert_eq!(&text[fm.body_start..], "body");
    }

    #[test]
    fn body_start_indexes_cleanly_after_block() {
        let text = "---\ntitle: T\n---\nfirst body line\n";
        let fm = parse(text);
        assert!(text[fm.body_start..].starts_with("first body line"));
    }

    #[test]
    fn dots_close_marker_accepted() {
        let fm = parse("---\ntitle: Dotted\n...\nbody");
        assert_eq!(fm.title.as_deref(), Some("Dotted"));
    }

    #[test]
    fn empty_tags_flow_is_empty() {
        let fm = parse("---\ntags: []\n---\n");
        assert!(fm.tags.is_empty());
    }

    #[test]
    fn non_frontmatter_first_line_ignored() {
        let fm = parse("not a fence\n---\ntitle: X\n---\n");
        assert_eq!(fm, Frontmatter::default());
    }
}
