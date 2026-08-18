//! INLINE (hybrid) markdown preview: markdown rendered *in place* in the editing
//! surface, instead of in a side pane.
//!
//! # What this is, and what it deliberately is not
//!
//! The preview PANE ([`super::parse`] → [`super::show`]) projects markdown into a
//! *different* widget tree: markers are consumed, text is rewritten, and the
//! result has no positional relationship to the source. That is correct for a
//! pane and impossible for an editing surface, where the caret, the selection,
//! find/replace and the LSP all address the buffer by byte offset.
//!
//! So this module renders markdown the only way an editing surface can: it
//! changes **how the source bytes are drawn**, never *which* bytes are drawn.
//! Every span it emits is a byte range of the string being laid out, and the
//! only thing it does with that range is pick an [`egui::TextFormat`]. Nothing is
//! inserted, removed, reordered or substituted. That is what makes the feature
//! safe to put under a live caret.
//!
//! # Why this is not a second markdown implementation
//!
//! It is the SAME parse. [`super::parse`] already runs
//! `Parser::new_ext(src, PARSER_OPTIONS).into_offset_iter()` and then *discards*
//! the offsets for every event except a task-list marker. [`spans`] runs the same
//! parser with the same [`super::PARSER_OPTIONS`] constant and keeps the offsets
//! instead of the text. Two projections of one parse, sharing one options
//! constant, in one module — so an extension enabled for the pane is enabled here
//! in the same edit, and neither can drift.
//!
//! # Why the construct set stops where it does
//!
//! A markdown token-COLOURING layer already exists in the highlighter
//! (`scribe_core::syntax::MdColorOpts`): dividers, `#tags`, `~~strikethrough~~`,
//! task boxes and table pipes are already coloured there, per-token switchable in
//! Settings. Re-colouring any of them here would be a parallel copy of a shipped
//! feature.
//!
//! What that layer structurally CANNOT do is the part that needs a real parse:
//! **hierarchy** (a heading's level, hence its size), **marker recession** (the
//! `#`/`**`/backticks drawn dimmer than the content they wrap), and **span
//! nesting** (which bytes are the emphasis and which are its delimiters). That is
//! exactly — and only — what this module adds. See [`InlineStyle`].
//!
//! # Weight
//!
//! `**strong**` is rendered as a colour lift, not as heavier strokes. Every
//! bundled face in `render_support::build_fonts` is a `-Regular`, egui selects
//! weight by font FAMILY, and epaint synthesises slant but not weight — so there
//! is no bold face to select. Shipping a bold face is a separate change (a new
//! binary asset plus the family-registration surface that guards it); until then
//! a colour lift is the honest rendering, and `HlSpan::bold` — which syntect
//! already computes and `highlight_job` already drops for the same reason — is
//! the standing evidence that nothing downstream can render weight either.

use super::PARSER_OPTIONS;
use egui::{Color32, TextFormat};
use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};
use std::ops::Range;

/// What a source byte range *is*, for the purpose of drawing it.
///
/// Split along one axis: a **marker** is punctuation the author typed to declare
/// structure (`#`, `**`, `` ` ``, `>`, `[`/`](…)`), and recedes; **content** is
/// the prose that structure applies to, and is emphasised. A hybrid preview is
/// exactly that contrast — which is why this enum names both halves of every
/// construct instead of only the interesting one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InlineStyle {
    /// Structural punctuation: dimmed so it recedes without disappearing.
    Marker,
    /// The text of an ATX heading. `level` is 1–6 and drives the size scale.
    Heading { level: u8 },
    /// `*emphasis*` content — rendered with real synthesised italics.
    Emphasis,
    /// `**strong**` content — rendered as a colour lift (see the module docs).
    Strong,
    /// Inline `` `code` `` content, and a fenced block's body.
    Code,
    /// Block-quoted body text.
    Quote,
    /// The visible text of a link (`[this](…)`).
    Link,
    /// A list item's `-`/`*`/`1.` bullet, kept legible rather than dimmed: it is
    /// the item's only visual affordance.
    ListMarker,
}

/// One styled byte range of the string being laid out.
///
/// `range` indexes the SAME string [`spans`] was given, which is the same string
/// the [`egui::text::LayoutJob`] was built from. Nothing here is ever expressed in
/// document coordinates, which is what makes this safe on a projected buffer (see
/// [`restyle_job`]).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct InlineSpan {
    pub range: Range<usize>,
    pub style: InlineStyle,
}

/// The colours the inline styles resolve to, read from the live theme once per
/// frame by the caller rather than looked up per span.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct InlinePalette {
    /// Dimmed structural punctuation.
    pub marker: Color32,
    /// Heading text.
    pub heading: Color32,
    /// `**strong**` content (the colour that stands in for weight).
    pub strong: Color32,
    /// Inline/fenced code foreground.
    pub code: Color32,
    /// Inline/fenced code backing.
    pub code_bg: Color32,
    /// Block-quote body.
    pub quote: Color32,
    /// Link text.
    pub link: Color32,
    /// List bullets and ordinals.
    pub list_marker: Color32,
}

/// Font-size multiplier for an ATX heading level.
///
/// `level` is 1..=6; anything else (a caller-constructed span, or a future
/// `HeadingLevel` variant) falls to `1.0` — body size, the no-op — never an
/// unbounded scale.
#[must_use]
pub fn heading_scale(level: u8) -> f32 {
    match level {
        1 => 1.60,
        2 => 1.42,
        3 => 1.26,
        4 => 1.14,
        5 => 1.06,
        _ => 1.0,
    }
}

fn heading_level_u8(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// Push `range` as `style`, dropping empty ranges (a zero-width span would split
/// a layout section for no visual effect).
fn push(out: &mut Vec<InlineSpan>, range: Range<usize>, style: InlineStyle) {
    if range.start < range.end {
        out.push(InlineSpan { range, style });
    }
}

/// Emit `inner` as `content`, and the two source stretches on either side of it
/// (the delimiters) as [`InlineStyle::Marker`].
///
/// This is the whole marker/content split: `outer` is what the parser reported for
/// the construct, `inner` is what it reported for the construct's text, and the
/// difference is, by definition, the punctuation the author typed. Reading the
/// delimiter width off the parse instead of assuming one is what makes `*a*`,
/// `_a_`, `**a**` and `__a__` all correct without four special cases.
fn wrap(
    out: &mut Vec<InlineSpan>,
    outer: &Range<usize>,
    inner: &Range<usize>,
    content: InlineStyle,
) {
    push(out, outer.start..inner.start, InlineStyle::Marker);
    push(out, inner.clone(), content);
    push(out, inner.end..outer.end, InlineStyle::Marker);
}

/// Byte length of the list marker at the start of `s` (`- `, `* `, `+ `, `12. `,
/// `3) `), including any leading indent and its trailing spaces/tabs. `0` when `s`
/// does not start with one.
///
/// Scanned from the source rather than taken from the parser because
/// `Tag::Item`'s range spans the whole item, marker and body together.
fn list_marker_len(s: &str) -> usize {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
        i += 1;
    }
    if i < b.len() && matches!(b[i], b'-' | b'*' | b'+') {
        i += 1;
    } else {
        let digits_start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == digits_start || i >= b.len() || !matches!(b[i], b'.' | b')') {
            return 0;
        }
        i += 1;
    }
    // A marker is only a marker when whitespace follows it.
    let after = i;
    while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
        i += 1;
    }
    if i == after {
        return 0;
    }
    i
}

/// A construct that has opened and not yet closed. `inner` is the hull of the
/// source the parser attributed to its CONTENT, accumulated from the inline events
/// that arrive while it is open.
struct Open {
    kind: OpenKind,
    outer: Range<usize>,
    inner: Option<Range<usize>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OpenKind {
    Heading(u8),
    Strong,
    Emphasis,
    Link,
}

impl OpenKind {
    /// The style the construct's CONTENT half draws as.
    fn content(self) -> InlineStyle {
        match self {
            OpenKind::Heading(level) => InlineStyle::Heading { level },
            OpenKind::Strong => InlineStyle::Strong,
            OpenKind::Emphasis => InlineStyle::Emphasis,
            OpenKind::Link => InlineStyle::Link,
        }
    }

    /// Whether a construct with no content at all (`###`, `[](x)`) should still
    /// draw its whole extent as marker. True for the block-ish constructs, whose
    /// punctuation is meaningful on its own; false for emphasis, where an empty
    /// span is not emphasis at all.
    fn marker_when_empty(self) -> bool {
        matches!(self, OpenKind::Heading(_) | OpenKind::Link)
    }
}

/// The last line of a fenced block that is a closing fence, as a byte offset into
/// `text`. `text.len()` when the fence is unterminated (the block runs to EOF), so
/// the body simply extends to the end rather than a stray last line being dimmed.
fn closing_fence_start(text: &str) -> usize {
    let mut at = 0usize;
    let mut found = text.len();
    let mut first = true;
    for line in text.split_inclusive('\n') {
        let t = line.trim_matches(|c: char| c.is_whitespace());
        // Skip the OPENING fence: a one-line block is all fence, not a close.
        if !first && !t.is_empty() && t.chars().all(|c| c == '`' || c == '~') {
            found = at;
        }
        first = false;
        at += line.len();
    }
    found
}

/// Project `src` onto the styled source ranges an editing surface can draw.
///
/// Pure: same input, same output, no egui and no theme. Ranges are byte offsets
/// into `src`, are non-empty, and are emitted so that an OUTER construct is always
/// pushed before the inner ones it contains — which is what lets [`restyle_job`]
/// compose them (heading size, then the strong colour inside it) rather than have
/// one silently erase the other.
///
/// Never panics: a malformed or truncated document yields a best-effort span list,
/// exactly as [`super::parse`] yields a best-effort block list.
#[must_use]
pub fn spans(src: &str) -> Vec<InlineSpan> {
    let mut out: Vec<InlineSpan> = Vec::new();
    let mut stack: Vec<Open> = Vec::new();

    // Close the innermost open construct, emitting its marker/content split.
    fn close(out: &mut Vec<InlineSpan>, stack: &mut Vec<Open>) {
        let Some(open) = stack.pop() else { return };
        match open.inner {
            Some(inner) => wrap(out, &open.outer, &inner, open.kind.content()),
            None if open.kind.marker_when_empty() => {
                push(out, open.outer, InlineStyle::Marker);
            }
            None => {}
        }
    }

    for (ev, range) in Parser::new_ext(src, PARSER_OPTIONS).into_offset_iter() {
        // Every inline event extends the content hull of each construct that is
        // currently open. The stack is nesting-deep (single digits), so this is
        // O(depth) per event, never O(constructs-so-far).
        let extends_hull = matches!(
            ev,
            Event::Text(_)
                | Event::Code(_)
                | Event::InlineMath(_)
                | Event::InlineHtml(_)
                | Event::FootnoteReference(_)
                | Event::SoftBreak
                | Event::HardBreak
        );
        if extends_hull {
            for open in &mut stack {
                match &mut open.inner {
                    Some(h) => {
                        h.start = h.start.min(range.start);
                        h.end = h.end.max(range.end);
                    }
                    slot @ None => *slot = Some(range.clone()),
                }
            }
        }

        match ev {
            Event::Start(Tag::Heading { level, .. }) => stack.push(Open {
                kind: OpenKind::Heading(heading_level_u8(level)),
                outer: range,
                inner: None,
            }),
            Event::Start(Tag::Strong) => stack.push(Open {
                kind: OpenKind::Strong,
                outer: range,
                inner: None,
            }),
            Event::Start(Tag::Emphasis) => stack.push(Open {
                kind: OpenKind::Emphasis,
                outer: range,
                inner: None,
            }),
            Event::Start(Tag::Link { .. }) => stack.push(Open {
                kind: OpenKind::Link,
                outer: range,
                inner: None,
            }),

            Event::End(TagEnd::Heading(_) | TagEnd::Strong | TagEnd::Emphasis | TagEnd::Link) => {
                close(&mut out, &mut stack)
            }

            // Inline code arrives as ONE event whose range covers the backticks and
            // whose text is the content, so the delimiter width is read off the
            // source directly.
            Event::Code(_) => {
                let ticks = src
                    .get(range.clone())
                    .map_or(0, |s| s.bytes().take_while(|b| *b == b'`').count());
                if ticks > 0 && range.start + 2 * ticks <= range.end {
                    let inner = (range.start + ticks)..(range.end - ticks);
                    wrap(&mut out, &range, &inner, InlineStyle::Code);
                } else {
                    push(&mut out, range, InlineStyle::Code);
                }
            }

            // A fenced block's body is code and its fence lines are marker; an
            // indented block has no fence, so it is all body.
            Event::Start(Tag::CodeBlock(_)) => {
                let text = src.get(range.clone()).unwrap_or("");
                if text.starts_with("```") || text.starts_with("~~~") {
                    let open_end = text.find('\n').map_or(text.len(), |i| i + 1);
                    let close_start = closing_fence_start(text).max(open_end);
                    let body = (range.start + open_end)..(range.start + close_start);
                    push(&mut out, range.start..body.start, InlineStyle::Marker);
                    push(&mut out, body.clone(), InlineStyle::Code);
                    push(&mut out, body.end..range.end, InlineStyle::Marker);
                } else {
                    push(&mut out, range, InlineStyle::Code);
                }
            }

            // The quote's whole extent is quote-toned; each `>` is then emitted as
            // a NARROWER marker span, which `restyle_job` applies after the wider
            // one, so the punctuation dims while the body keeps the quote tone.
            Event::Start(Tag::BlockQuote(_)) => {
                push(&mut out, range.clone(), InlineStyle::Quote);
                let text = src.get(range.clone()).unwrap_or("");
                let mut at = 0usize;
                for line in text.split_inclusive('\n') {
                    let lead = line.len() - line.trim_start_matches([' ', '\t']).len();
                    let rest = &line[lead..];
                    if rest.starts_with('>') {
                        // `>` plus one optional space, per CommonMark.
                        let w = usize::from(rest.as_bytes().get(1) == Some(&b' ')) + 1;
                        let start = range.start + at + lead;
                        push(&mut out, start..(start + w), InlineStyle::Marker);
                    }
                    at += line.len();
                }
            }

            Event::Start(Tag::Item) => {
                let len = list_marker_len(src.get(range.clone()).unwrap_or(""));
                push(
                    &mut out,
                    range.start..(range.start + len),
                    InlineStyle::ListMarker,
                );
            }

            _ => {}
        }
    }
    // An unterminated construct at EOF still styles what it covered.
    while !stack.is_empty() {
        close(&mut out, &mut stack);
    }
    out
}

/// Apply `spans` to an already-built [`egui::text::LayoutJob`], in place.
///
/// # The one invariant
///
/// `job.text` is **not touched**. Only `job.sections` is rewritten: existing
/// sections are split at span boundaries and the pieces get adjusted formats. The
/// laid-out string — and therefore every byte offset the caret, the selection,
/// find/replace and the diagnostics painter depend on — is bit-identical to what
/// the surface would have laid out with this feature off.
///
/// # Why this is safe on the fold view's projected buffer
///
/// The fold preview lays out a *projection* whose offsets do not match the
/// document, which is why a span-based OVERLAY (diagnostics, whose ranges come
/// from the language server in DOCUMENT coordinates) must never be pointed at it —
/// it would underline the wrong text. These spans are not an overlay: they are
/// derived, by [`spans`], from the very string being laid out. A projected buffer
/// therefore gets its own markdown styled, self-consistently, and the mismatch
/// that hazard is made of cannot arise. The direction of the data flow is the
/// whole difference.
///
/// Overlapping spans COMPOSE, widest first, so a `**strong**` inside an `# H1`
/// keeps the heading's size and takes the strong colour.
pub fn restyle_job(job: &mut egui::text::LayoutJob, spans: &[InlineSpan], pal: &InlinePalette) {
    if spans.is_empty() || job.sections.is_empty() {
        return;
    }
    // Cut points: the SPAN boundaries only.
    //
    // Section boundaries are deliberately NOT collected, and out-of-range cuts are
    // deliberately NOT filtered — both would be dead code. The rebuild loop below
    // walks one section at a time and takes only the cuts STRICTLY INSIDE it, so a
    // section's own start/end can never be selected (the inequalities exclude
    // them) and a cut past the end of the text can never fall inside any section.
    // A mutation pass proved both: neither an added section-boundary cut nor an
    // out-of-range one changes a single output section. Keeping them would be
    // defensive-looking code that no test could ever hold to account.
    let mut cuts: Vec<usize> = Vec::with_capacity(spans.len() * 2);
    for s in spans {
        cuts.push(s.range.start);
        cuts.push(s.range.end);
    }
    cuts.sort_unstable();
    cuts.dedup();

    let mut rebuilt: Vec<egui::text::LayoutSection> = Vec::with_capacity(cuts.len());
    let mut covering: Vec<&InlineSpan> = Vec::new();
    for section in &job.sections {
        let (lo, hi) = (section.byte_range.start, section.byte_range.end);
        let mut pos = lo;
        for &cut in cuts.iter().filter(|c| **c > lo && **c < hi) {
            rebuilt.push(piece(section, pos..cut, spans, pal, &mut covering));
            pos = cut;
        }
        if pos < hi {
            rebuilt.push(piece(section, pos..hi, spans, pal, &mut covering));
        }
    }
    job.sections = rebuilt;
}

/// One uniform piece of a section: the section's own format with every span that
/// CONTAINS the piece applied over it, widest first so inner constructs refine the
/// outer ones instead of erasing them.
fn piece<'a>(
    section: &egui::text::LayoutSection,
    range: Range<usize>,
    spans: &'a [InlineSpan],
    pal: &InlinePalette,
    scratch: &mut Vec<&'a InlineSpan>,
) -> egui::text::LayoutSection {
    let mut fmt = section.format.clone();
    scratch.clear();
    scratch.extend(
        spans
            .iter()
            .filter(|s| s.range.start <= range.start && range.end <= s.range.end),
    );
    // Stable, so equal-width spans keep their emission order.
    scratch.sort_by_key(|s| std::cmp::Reverse(s.range.end - s.range.start));
    for s in scratch.iter() {
        apply(&mut fmt, s.style, pal);
    }
    egui::text::LayoutSection {
        leading_space: section.leading_space,
        byte_range: range,
        format: fmt,
    }
}

/// Mutate `fmt` to render `style`. The single place a style becomes pixels.
fn apply(fmt: &mut TextFormat, style: InlineStyle, pal: &InlinePalette) {
    match style {
        InlineStyle::Marker => fmt.color = pal.marker,
        InlineStyle::Heading { level } => {
            fmt.color = pal.heading;
            fmt.font_id.size *= heading_scale(level);
            // `line_height` is an explicit per-row height set by `highlight_job` to
            // honour the user's line-height setting; a grown glyph in an unchanged
            // row would clip, so the row grows with it.
            if let Some(h) = fmt.line_height.as_mut() {
                *h *= heading_scale(level);
            }
        }
        InlineStyle::Emphasis => fmt.italics = true,
        InlineStyle::Strong => fmt.color = pal.strong,
        InlineStyle::Code => {
            fmt.color = pal.code;
            fmt.background = pal.code_bg;
        }
        InlineStyle::Quote => {
            fmt.color = pal.quote;
            fmt.italics = true;
        }
        InlineStyle::Link => {
            fmt.color = pal.link;
            fmt.underline = egui::Stroke::new(1.0, pal.link);
        }
        InlineStyle::ListMarker => fmt.color = pal.list_marker,
    }
}

/// Whether `ext` names a markdown file — the only files the inline preview styles.
/// Kept here, not at a call site, so every layout arm decides it the same way.
#[must_use]
pub fn is_markdown_ext(ext: Option<&str>) -> bool {
    matches!(
        ext.map(str::to_ascii_lowercase).as_deref(),
        Some("md" | "markdown" | "mdown" | "mkd" | "mdx")
    )
}

#[cfg(test)]
mod tests;
