//! `[[wiki-link]]` extraction over note content.
//!
//! Pure parsing over a local string — no I/O, no network. Grammar (Obsidian /
//! Logseq compatible subset):
//!
//! ```text
//! [[Target]]
//! [[Target|Display]]
//! [[Target#Heading]]
//! [[Target#Heading|Display]]
//! [[#Heading]]            (intra-note link — Target is empty)
//! ![[Target]]             (embed / transclusion — `embed == true`)
//! ```
//!
//! The parser is deliberately non-nesting: the first `]]` after a `[[` closes
//! the link (Markdown wiki-links never nest). Byte offsets are reported so a UI
//! can map a link back to its span; `[[`, `]]`, `#`, `|` and `!` are all ASCII,
//! so every reported offset falls on a UTF-8 codepoint boundary.
//!
//! Extraction never resolves a target to a filesystem path — that is the job of
//! [`crate::notes::vault_path`], which rejects traversal. A `target` string here
//! is UNTRUSTED note content and must be sanitised before any path use.

/// One parsed `[[wiki-link]]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WikiLink {
    /// The link target: a note name or vault-relative path WITHOUT the
    /// `#heading` or `|display` parts. May be empty for an intra-note
    /// `[[#Heading]]` link.
    pub target: String,
    /// The `#heading` anchor, if present (without the leading `#`).
    pub heading: Option<String>,
    /// The `|display` alias, if present.
    pub display: Option<String>,
    /// True for an `![[embed]]` transclusion (leading `!`).
    pub embed: bool,
    /// Byte offset of the opening `[[` (or `!` for an embed).
    pub start: usize,
    /// Byte offset just past the closing `]]`.
    pub end: usize,
}

impl WikiLink {
    /// The text a UI should show for this link: the explicit `|display` alias
    /// when present, otherwise the target (or `#heading` for a self-link).
    #[must_use]
    pub fn label(&self) -> String {
        if let Some(d) = &self.display {
            return d.clone();
        }
        if self.target.is_empty() {
            if let Some(h) = &self.heading {
                return format!("#{h}");
            }
        }
        self.target.clone()
    }
}

/// Extract every `[[wiki-link]]` from `text`, in document order.
///
/// A link with an entirely empty body (`[[]]` or `[[|x]]` with no target and no
/// heading) is skipped — it names nothing. Whitespace around the target,
/// heading, and display is trimmed.
#[must_use]
pub fn extract_wikilinks(text: &str) -> Vec<WikiLink> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    let n = bytes.len();
    while i + 1 < n {
        if bytes[i] == b'[' && bytes[i + 1] == b'[' {
            // A `[[` — find the closing `]]`.
            let inner_start = i + 2;
            if let Some(rel) = find_close(&bytes[inner_start..]) {
                let inner_end = inner_start + rel;
                let inner = &text[inner_start..inner_end];
                let embed = i > 0 && bytes[i - 1] == b'!';
                let start = if embed { i - 1 } else { i };
                let end = inner_end + 2; // past the `]]`
                if let Some(link) = parse_inner(inner, embed, start, end) {
                    out.push(link);
                }
                i = end;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Find the byte offset of the first `]]` in `bytes`, or `None`. A lone `]`
/// does not close the link.
fn find_close(bytes: &[u8]) -> Option<usize> {
    let mut j = 0usize;
    while j + 1 < bytes.len() {
        if bytes[j] == b']' && bytes[j + 1] == b']' {
            return Some(j);
        }
        // A `[[` opening inside would-be inner text means the earlier `[[` was
        // not a link opener; but since we scan left-to-right and close greedily
        // at the first `]]`, a nested `[[` simply becomes part of the target and
        // is later rejected by sanitisation. Keep scanning for `]]`.
        j += 1;
    }
    None
}

/// Parse the inner text of a `[[...]]` into a [`WikiLink`], or `None` if it
/// names nothing.
fn parse_inner(inner: &str, embed: bool, start: usize, end: usize) -> Option<WikiLink> {
    // Split off the display alias on the FIRST `|`.
    let (left, display) = match inner.split_once('|') {
        Some((l, d)) => (l, Some(d.trim().to_string())),
        None => (inner, None),
    };
    // Split the remainder into target + optional `#heading` on the FIRST `#`.
    let (target_raw, heading) = match left.split_once('#') {
        Some((t, h)) => (t, Some(h.trim().to_string())),
        None => (left, None),
    };
    let target = target_raw.trim().to_string();
    // A link must name at least a target or a heading; `[[]]` / `[[|x]]` are
    // discarded. A blank display alias is normalised to None.
    if target.is_empty() && heading.as_ref().is_none_or(|h| h.is_empty()) {
        return None;
    }
    let display = display.filter(|d| !d.is_empty());
    let heading = heading.filter(|h| !h.is_empty());
    Some(WikiLink {
        target,
        heading,
        display,
        embed,
        start,
        end,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn targets(text: &str) -> Vec<String> {
        extract_wikilinks(text)
            .into_iter()
            .map(|l| l.target)
            .collect()
    }

    #[test]
    fn plain_link() {
        let links = extract_wikilinks("see [[Home]] now");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "Home");
        assert_eq!(links[0].heading, None);
        assert_eq!(links[0].display, None);
        assert!(!links[0].embed);
        // Offsets bracket the whole `[[Home]]`.
        assert_eq!(
            &"see [[Home]] now"[links[0].start..links[0].end],
            "[[Home]]"
        );
    }

    #[test]
    fn display_alias() {
        let links = extract_wikilinks("[[Target|Shown Text]]");
        assert_eq!(links[0].target, "Target");
        assert_eq!(links[0].display.as_deref(), Some("Shown Text"));
        assert_eq!(links[0].label(), "Shown Text");
    }

    #[test]
    fn heading_anchor() {
        let links = extract_wikilinks("[[Note#Section]]");
        assert_eq!(links[0].target, "Note");
        assert_eq!(links[0].heading.as_deref(), Some("Section"));
    }

    #[test]
    fn heading_and_display() {
        let links = extract_wikilinks("[[Note#Section|Alias]]");
        assert_eq!(links[0].target, "Note");
        assert_eq!(links[0].heading.as_deref(), Some("Section"));
        assert_eq!(links[0].display.as_deref(), Some("Alias"));
    }

    #[test]
    fn intra_note_heading_link() {
        let links = extract_wikilinks("[[#Overview]]");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "");
        assert_eq!(links[0].heading.as_deref(), Some("Overview"));
        assert_eq!(links[0].label(), "#Overview");
    }

    #[test]
    fn embed_transclusion() {
        let links = extract_wikilinks("![[image.png]]");
        assert_eq!(links.len(), 1);
        assert!(links[0].embed);
        assert_eq!(links[0].target, "image.png");
        assert_eq!(
            &"![[image.png]]"[links[0].start..links[0].end],
            "![[image.png]]"
        );
    }

    #[test]
    fn multiple_links_on_one_line() {
        assert_eq!(targets("[[A]] and [[B]] and [[C]]"), vec!["A", "B", "C"]);
    }

    #[test]
    fn subfolder_target_is_preserved_verbatim() {
        // Sanitisation happens at path-resolution time, not here; extraction
        // keeps the relative path so `projects/roadmap` resolves under the vault.
        let links = extract_wikilinks("[[projects/roadmap]]");
        assert_eq!(links[0].target, "projects/roadmap");
    }

    #[test]
    fn empty_link_is_skipped() {
        assert!(extract_wikilinks("[[]] and [[  ]] and [[|only display]]").is_empty());
    }

    #[test]
    fn unterminated_open_is_not_a_link() {
        assert!(extract_wikilinks("this [[ never closes").is_empty());
        assert!(extract_wikilinks("a [ single bracket ]").is_empty());
    }

    #[test]
    fn whitespace_is_trimmed() {
        let links = extract_wikilinks("[[  Spaced Note  #  Head  |  Disp  ]]");
        assert_eq!(links[0].target, "Spaced Note");
        assert_eq!(links[0].heading.as_deref(), Some("Head"));
        assert_eq!(links[0].display.as_deref(), Some("Disp"));
    }

    #[test]
    fn multibyte_content_before_link_keeps_offsets_valid() {
        let text = "café ☕ [[Note]]";
        let links = extract_wikilinks(text);
        assert_eq!(links.len(), 1);
        // The reported span slices cleanly (would panic on a non-boundary).
        assert_eq!(&text[links[0].start..links[0].end], "[[Note]]");
    }

    #[test]
    fn traversal_target_extracted_but_flagged_for_sanitiser() {
        // Extraction does not reject `..`; it is captured verbatim so the
        // path-safety layer can reject it. This guards that a malicious link
        // does not silently vanish (which would hide the attack).
        let links = extract_wikilinks("[[../../etc/passwd]]");
        assert_eq!(links[0].target, "../../etc/passwd");
    }

    #[test]
    fn label_prefers_display_then_target() {
        assert_eq!(extract_wikilinks("[[T|D]]")[0].label(), "D");
        assert_eq!(extract_wikilinks("[[T]]")[0].label(), "T");
    }

    #[test]
    fn adjacent_links_no_gap() {
        assert_eq!(targets("[[A]][[B]]"), vec!["A", "B"]);
    }
}
