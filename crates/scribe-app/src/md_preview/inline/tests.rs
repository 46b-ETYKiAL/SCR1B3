//! Unit pins for the inline (hybrid) markdown preview's two pure halves:
//! [`super::spans`] (source → styled byte ranges) and [`super::restyle_job`]
//! (styled byte ranges → layout-section formats).
//!
//! Every assertion here was OBSERVED RED once, by cutting the product wire it
//! claims to measure and confirming the failure, before being committed. The
//! ledger is in the module header of `inline_preview_parity_tests`, alongside the
//! arm-level cuts, so the whole feature's falsification record reads as one list.
//!
//! Two disciplines these obey, both inherited from `surface_parity_tests`:
//!
//! 1. **No test supplies its own wire.** The input is a markdown SOURCE STRING —
//!    the thing a user types — and, for the job tests, a `LayoutJob` built the way
//!    `highlight_job` builds one (append text with a base format). Nothing here
//!    writes a span and then asserts a span was read.
//! 2. **Absence is pinned as loudly as presence.** The constructs deliberately
//!    left to the existing `MdColorOpts` colouring layer are pinned ABSENT, so
//!    adding a second implementation of one of them turns a test red instead of
//!    silently shipping a parallel copy.

use super::{
    heading_scale, is_markdown_ext, restyle_job, spans, InlinePalette, InlineSpan, InlineStyle,
};
use egui::text::{LayoutJob, LayoutSection};
use egui::{Color32, FontId, TextFormat};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A palette whose every role is a DIFFERENT colour, so an assertion that a
/// piece got the marker tone cannot pass because it happened to get the code
/// tone. A shared colour here would make several tests vacuous at once.
fn palette() -> InlinePalette {
    InlinePalette {
        marker: Color32::from_rgb(1, 0, 0),
        heading: Color32::from_rgb(2, 0, 0),
        strong: Color32::from_rgb(3, 0, 0),
        code: Color32::from_rgb(4, 0, 0),
        code_bg: Color32::from_rgb(5, 0, 0),
        quote: Color32::from_rgb(6, 0, 0),
        link: Color32::from_rgb(7, 0, 0),
        list_marker: Color32::from_rgb(8, 0, 0),
    }
}

const BASE_SIZE: f32 = 14.0;
const BASE_LINE_H: f32 = 21.0;
const BASE_COLOR: Color32 = Color32::from_rgb(200, 200, 200);

/// A one-section job over `text`, shaped like the ones `highlight_job` emits:
/// an explicit `line_height` (which the heading style must scale with the glyph)
/// and a base colour that is not any palette role.
fn job(text: &str) -> LayoutJob {
    let mut fmt = TextFormat::simple(FontId::monospace(BASE_SIZE), BASE_COLOR);
    fmt.line_height = Some(BASE_LINE_H);
    let mut j = LayoutJob::default();
    j.append(text, 0.0, fmt);
    j
}

/// A MULTI-section job over `text`, split at `at` — the shape a real syntect job
/// has. Restyling must split sections without disturbing the boundary that was
/// already there.
fn split_job(text: &str, at: usize) -> LayoutJob {
    let mut fmt = TextFormat::simple(FontId::monospace(BASE_SIZE), BASE_COLOR);
    fmt.line_height = Some(BASE_LINE_H);
    let mut j = LayoutJob::default();
    j.append(&text[..at], 0.0, fmt.clone());
    j.append(&text[at..], 0.0, fmt);
    j
}

/// The style applied to the span exactly covering `range`, or `None` when no span
/// has that exact extent.
fn style_of(spans: &[InlineSpan], range: std::ops::Range<usize>) -> Option<InlineStyle> {
    spans.iter().find(|s| s.range == range).map(|s| s.style)
}

/// Every `(source text, style)` pair the span list attributes, for readable
/// whole-document assertions.
fn styled<'a>(src: &'a str, spans: &[InlineSpan]) -> Vec<(&'a str, InlineStyle)> {
    spans
        .iter()
        .map(|s| (&src[s.range.clone()], s.style))
        .collect()
}

/// The section covering byte `at` after a restyle.
fn section_at(j: &LayoutJob, at: usize) -> &LayoutSection {
    j.sections
        .iter()
        .find(|s| s.byte_range.start <= at && at < s.byte_range.end)
        .unwrap_or_else(|| panic!("no section covers byte {at} of {:?}", j.text))
}

// ---------------------------------------------------------------------------
// spans() — the marker / content split
// ---------------------------------------------------------------------------

#[test]
fn a_heading_splits_into_a_dimmed_hash_run_and_scaled_text() {
    // The single most visible thing an inline preview does. The `# ` must be
    // MARKER (it recedes) and `Title` must be HEADING level 1 (it grows) — the
    // contrast between the two IS the feature. A single span over the whole line
    // would be a colouring pass, which the highlighter already has.
    let src = "# Title\n";
    let got = spans(src);
    assert_eq!(
        style_of(&got, 0..2),
        Some(InlineStyle::Marker),
        "the `# ` run must be marker; got {:?}",
        styled(src, &got)
    );
    assert_eq!(
        style_of(&got, 2..7),
        Some(InlineStyle::Heading { level: 1 }),
        "`Title` must be heading-1 content; got {:?}",
        styled(src, &got)
    );
}

#[test]
fn every_heading_level_reports_its_own_level() {
    // The level is the only thing that makes this a HIERARCHY rather than a
    // colour. A parser change that flattened all six to H1 would still colour
    // headings correctly and be invisible without this.
    for level in 1..=6u8 {
        let src = format!("{} H\n", "#".repeat(level as usize));
        let got = spans(&src);
        let text_at = level as usize + 1;
        assert_eq!(
            style_of(&got, text_at..(text_at + 1)),
            Some(InlineStyle::Heading { level }),
            "level {level} misreported; got {:?}",
            styled(&src, &got)
        );
    }
}

#[test]
fn the_heading_scale_is_monotonic_and_bottoms_out_at_body_size() {
    // Sizes must strictly shrink with depth (an H2 that outgrew its H1 would
    // invert the hierarchy the feature exists to show), H6 must be exactly body
    // size, and an out-of-range level must fall to the no-op rather than an
    // unbounded scale.
    for level in 1..6u8 {
        assert!(
            heading_scale(level) > heading_scale(level + 1),
            "h{level} must be larger than h{}",
            level + 1
        );
    }
    assert!(
        (heading_scale(6) - 1.0).abs() < f32::EPSILON,
        "h6 is body size"
    );
    assert!(
        (heading_scale(0) - 1.0).abs() < f32::EPSILON
            && (heading_scale(200) - 1.0).abs() < f32::EPSILON,
        "an out-of-range level falls to the no-op"
    );
}

#[test]
fn emphasis_delimiter_width_is_read_from_the_parse_not_assumed() {
    // `*a*` and `**a**` differ by delimiter width, and `_`/`__` are the same
    // constructs with different punctuation. Assuming a width would get exactly
    // half of these wrong — which is why the split is taken from the parser's own
    // content attribution instead.
    for (src, inner, style) in [
        ("*a*", 1..2, InlineStyle::Emphasis),
        ("_a_", 1..2, InlineStyle::Emphasis),
        ("**a**", 2..3, InlineStyle::Strong),
        ("__a__", 2..3, InlineStyle::Strong),
    ] {
        let got = spans(src);
        assert_eq!(
            style_of(&got, inner.clone()),
            Some(style),
            "{src}: content extent wrong; got {:?}",
            styled(src, &got)
        );
        assert_eq!(
            style_of(&got, 0..inner.start),
            Some(InlineStyle::Marker),
            "{src}: opening delimiter must be marker; got {:?}",
            styled(src, &got)
        );
        assert_eq!(
            style_of(&got, inner.end..src.len()),
            Some(InlineStyle::Marker),
            "{src}: closing delimiter must be marker; got {:?}",
            styled(src, &got)
        );
    }
}

#[test]
fn inline_code_backticks_recede_and_the_body_is_code() {
    // Double-backtick spans exist precisely so a literal backtick can appear
    // inside; a hard-coded width of one would style the wrong bytes.
    let src = "a `x` b ``y`` c";
    let got = spans(src);
    assert_eq!(style_of(&got, 2..3), Some(InlineStyle::Marker), "` opens");
    assert_eq!(style_of(&got, 3..4), Some(InlineStyle::Code), "x is code");
    assert_eq!(style_of(&got, 4..5), Some(InlineStyle::Marker), "` closes");
    assert_eq!(style_of(&got, 8..10), Some(InlineStyle::Marker), "`` opens");
    assert_eq!(style_of(&got, 10..11), Some(InlineStyle::Code), "y is code");
    assert_eq!(
        style_of(&got, 11..13),
        Some(InlineStyle::Marker),
        "`` closes"
    );
}

#[test]
fn a_fenced_block_dims_both_fences_and_codes_the_body() {
    let src = "```rust\nfn f() {}\n```\n";
    let got = spans(src);
    assert_eq!(
        style_of(&got, 0..8),
        Some(InlineStyle::Marker),
        "the opening fence line (info string included) recedes; got {:?}",
        styled(src, &got)
    );
    assert_eq!(
        style_of(&got, 8..18),
        Some(InlineStyle::Code),
        "the body is code; got {:?}",
        styled(src, &got)
    );
    assert_eq!(
        style_of(&got, 18..21),
        Some(InlineStyle::Marker),
        "the closing fence recedes; got {:?}",
        styled(src, &got)
    );
}

#[test]
fn an_unterminated_fence_still_codes_its_body_to_the_end() {
    // A fence the user is still typing has no closing line. Treating the last
    // line as a close would dim a line of real code, so the body must run to EOF.
    let src = "```\nstill typing\n";
    let got = spans(src);
    assert_eq!(
        style_of(&got, 0..4),
        Some(InlineStyle::Marker),
        "the open fence recedes; got {:?}",
        styled(src, &got)
    );
    assert_eq!(
        style_of(&got, 4..src.len()),
        Some(InlineStyle::Code),
        "the body runs to EOF; got {:?}",
        styled(src, &got)
    );
}

#[test]
fn a_quote_tones_its_body_and_dims_only_the_angle_markers() {
    // Two spans, deliberately overlapping: the wide QUOTE tone over the block and
    // a narrow MARKER over each `> `. `restyle_job` composes them widest-first, so
    // the body keeps the quote tone and the punctuation dims.
    let src = "> one\n> two\n";
    let got = spans(src);
    assert_eq!(
        style_of(&got, 0..src.len()),
        Some(InlineStyle::Quote),
        "the whole block is quote-toned; got {:?}",
        styled(src, &got)
    );
    assert_eq!(
        style_of(&got, 0..2),
        Some(InlineStyle::Marker),
        "first `> `"
    );
    assert_eq!(
        style_of(&got, 6..8),
        Some(InlineStyle::Marker),
        "second `> `"
    );
}

#[test]
fn a_list_marker_is_the_bullet_and_nothing_else() {
    // The bullet is the item's only affordance, so it is styled; the item BODY is
    // not, because `Tag::Item`'s range covers both and styling the whole thing
    // would tint every list line.
    for (src, marker_end) in [("- a\n", 2), ("* a\n", 2), ("+ a\n", 2), ("12. a\n", 4)] {
        let got = spans(src);
        assert_eq!(
            style_of(&got, 0..marker_end),
            Some(InlineStyle::ListMarker),
            "{src:?}: bullet extent wrong; got {:?}",
            styled(src, &got)
        );
        assert!(
            !got.iter()
                .any(|s| s.range.end > marker_end && s.style == InlineStyle::ListMarker),
            "{src:?}: the item body must not be list-marker toned; got {:?}",
            styled(src, &got)
        );
    }
}

#[test]
fn a_link_label_is_content_and_its_brackets_and_destination_recede() {
    let src = "see [the docs](https://example.com) ok";
    let got = spans(src);
    assert_eq!(
        style_of(&got, 5..13),
        Some(InlineStyle::Link),
        "`the docs` is the visible label; got {:?}",
        styled(src, &got)
    );
    assert_eq!(
        style_of(&got, 4..5),
        Some(InlineStyle::Marker),
        "`[` recedes"
    );
    assert_eq!(
        style_of(&got, 13..35),
        Some(InlineStyle::Marker),
        "`](url)` recedes; got {:?}",
        styled(src, &got)
    );
}

#[test]
fn strong_inside_a_heading_reports_both_constructs() {
    // The nesting case. The heading's content span must still cover the whole
    // title (so the SIZE applies across it) while the strong span sits inside it.
    // A stack that closed the wrong construct would lose one of the two.
    let src = "# a **b** c\n";
    let got = spans(src);
    assert_eq!(
        style_of(&got, 2..11),
        Some(InlineStyle::Heading { level: 1 }),
        "the heading content spans the whole title; got {:?}",
        styled(src, &got)
    );
    assert_eq!(
        style_of(&got, 6..7),
        Some(InlineStyle::Strong),
        "`b` is strong inside the heading; got {:?}",
        styled(src, &got)
    );
}

#[test]
fn constructs_owned_by_the_existing_colouring_layer_are_left_alone() {
    // EXPECTED-ABSENT PIN. Dividers, `#tags`, `~~strike~~`, task boxes and table
    // pipes are already coloured by `scribe_core::syntax::MdColorOpts`, per-token
    // switchable in Settings. Styling them here too would be a second
    // implementation of a shipped feature, and the user's per-token switch would
    // stop working. This pin turns red the moment one is added — which is the
    // point: it is the rehearsal of the edit that must NOT be made silently.
    let src = "---\n#tag\n~~gone~~\n- [x] done\n| a | b |\n";
    let got = spans(src);
    let content: Vec<_> = got
        .iter()
        .filter(|s| s.style != InlineStyle::ListMarker && s.style != InlineStyle::Marker)
        .map(|s| (&src[s.range.clone()], s.style))
        .collect();
    assert!(
        content.is_empty(),
        "these constructs belong to the MdColorOpts layer; inline styled {content:?}"
    );
}

#[test]
fn every_span_is_non_empty_and_inside_the_source() {
    // The safety envelope for `restyle_job`, which indexes `job.text` with these.
    // An out-of-bounds or inverted range is the shape of bug that turns a render
    // into a panic, and a torture document is where one would come from.
    let src = "# h\n\n**b** *i* `c`\n\n> q\n\n```\nx\n```\n\n- l\n\n[a](b)\n\n\
               ### *nested **deep** here*\n\n``\n\n#\n\n>\n\n- \n\n$x$\n\n\
               | a |\n|---|\n| 1 |\n";
    for s in spans(src) {
        assert!(
            s.range.start < s.range.end,
            "empty/inverted span {:?} ({:?})",
            s.range,
            s.style
        );
        assert!(
            s.range.end <= src.len()
                && src.is_char_boundary(s.range.start)
                && src.is_char_boundary(s.range.end),
            "span {:?} is out of bounds or off a char boundary",
            s.range
        );
    }
}

#[test]
fn multibyte_text_is_split_on_char_boundaries() {
    // Byte ranges over non-ASCII prose are where an off-by-one becomes a panic
    // rather than a cosmetic bug, since `restyle_job` hands them to epaint.
    let src = "# 見出し\n\n**強調**と`コード`\n";
    let got = spans(src);
    assert!(!got.is_empty(), "the document must produce spans");
    for s in &got {
        assert!(
            src.is_char_boundary(s.range.start) && src.is_char_boundary(s.range.end),
            "{:?} splits a multibyte char",
            s.range
        );
    }
    assert!(
        got.iter()
            .any(|s| s.style == InlineStyle::Heading { level: 1 }
                && src[s.range.clone()].starts_with('見')),
        "the heading content starts at the first kanji; got {:?}",
        styled(src, &got)
    );
}

#[test]
fn plain_prose_produces_no_spans_at_all() {
    // The no-op case: a note with no markdown must cost nothing and change
    // nothing, so `restyle_job` can early-return and the section list stays
    // byte-identical to the highlighter's.
    assert!(spans("just some ordinary prose\nwith two lines\n").is_empty());
}

#[test]
fn only_markdown_extensions_opt_in() {
    for ext in ["md", "MD", "markdown", "mdown", "mkd", "mdx"] {
        assert!(is_markdown_ext(Some(ext)), "{ext} is markdown");
    }
    for ext in ["rs", "txt", "toml", "mdb", "cmd", ""] {
        assert!(!is_markdown_ext(Some(ext)), "{ext} is not markdown");
    }
    assert!(
        !is_markdown_ext(None),
        "a pathless scratch buffer is not markdown"
    );
}

// ---------------------------------------------------------------------------
// restyle_job() — the one invariant, and composition
// ---------------------------------------------------------------------------

#[test]
fn restyling_never_changes_the_laid_out_text() {
    // THE load-bearing invariant of the whole feature. The caret, the selection,
    // find/replace and the diagnostics painter all address this buffer by byte
    // offset; if a restyle could insert, drop or rewrite a byte, every one of
    // them would point at the wrong place. Sections must also still TILE the text
    // exactly — a gap would drop glyphs, an overlap would double-draw them.
    let src = "# T\n\n**b** and `c` and [l](u)\n\n> q\n\n- i\n";
    let mut j = job(src);
    let before = j.text.clone();
    restyle_job(&mut j, &spans(src), &palette());

    assert_eq!(j.text, before, "the laid-out string must be untouched");
    let mut at = 0usize;
    for s in &j.sections {
        assert_eq!(s.byte_range.start, at, "section gap/overlap at {at}");
        assert!(
            s.byte_range.start < s.byte_range.end,
            "empty section at {at}"
        );
        at = s.byte_range.end;
    }
    assert_eq!(at, before.len(), "sections must cover the whole text");
}

#[test]
fn restyling_preserves_a_pre_existing_section_boundary() {
    // A real job arrives pre-split by the highlighter. Restyling adds cuts; it
    // must never MERGE across one that was already there, because the two sides
    // can carry different syntect colours and merging would repaint one of them.
    let src = "# Title\n";
    let mut j = split_job(src, 4);
    restyle_job(&mut j, &spans(src), &palette());
    assert!(
        j.sections.iter().any(|s| s.byte_range.end == 4)
            && j.sections.iter().any(|s| s.byte_range.start == 4),
        "the pre-existing boundary at 4 was lost: {:?}",
        j.sections
            .iter()
            .map(|s| s.byte_range.clone())
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_heading_grows_both_the_glyph_and_its_row() {
    // `highlight_job` sets an explicit per-row `line_height` from the user's
    // setting. Growing the glyph inside an unchanged row clips the ascenders, so
    // the row has to grow with it — a defect that is invisible in a span dump and
    // only shows up in the format.
    let src = "# Title\n";
    let mut j = job(src);
    restyle_job(&mut j, &spans(src), &palette());
    let head = section_at(&j, 3).format.clone();
    let scale = heading_scale(1);
    assert!(
        (head.font_id.size - BASE_SIZE * scale).abs() < 1e-3,
        "heading glyph not scaled: {} vs {}",
        head.font_id.size,
        BASE_SIZE * scale
    );
    assert!(
        head.line_height
            .is_some_and(|h| (h - BASE_LINE_H * scale).abs() < 1e-3),
        "heading row not scaled with the glyph: {:?}",
        head.line_height
    );
    assert_eq!(head.color, palette().heading, "heading tone");
    // ...and the marker beside it is dimmed at BODY size, so the contrast is real.
    let marker = section_at(&j, 0).format.clone();
    assert_eq!(marker.color, palette().marker, "the `#` must dim");
    assert!(
        (marker.font_id.size - BASE_SIZE).abs() < 1e-3,
        "the `#` must not grow with the text it introduces"
    );
}

#[test]
fn an_inner_construct_refines_the_outer_one_instead_of_erasing_it() {
    // `**b**` inside `# h` must keep the HEADING's size and take the STRONG
    // colour. A last-span-wins rule would drop the size (the strong span is
    // narrower and emitted later); a first-span-wins rule would drop the colour.
    // Only widest-first composition gets both, and only this test can tell.
    let src = "# a **b** c\n";
    let mut j = job(src);
    restyle_job(&mut j, &spans(src), &palette());
    let strong = section_at(&j, 6).format.clone();
    assert_eq!(
        strong.color,
        palette().strong,
        "inner construct sets the tone"
    );
    assert!(
        (strong.font_id.size - BASE_SIZE * heading_scale(1)).abs() < 1e-3,
        "the outer heading's size must survive the inner span: {}",
        strong.font_id.size
    );
}

#[test]
fn a_quote_marker_dims_without_losing_the_quote_tone_around_it() {
    // The other composition direction: a NARROW marker inside a WIDE quote. The
    // body keeps the quote tone and italics; the `> ` takes the marker tone.
    let src = "> quoted\n";
    let mut j = job(src);
    restyle_job(&mut j, &spans(src), &palette());
    let body = section_at(&j, 4).format.clone();
    assert_eq!(body.color, palette().quote, "quote body tone");
    assert!(body.italics, "quote body is slanted");
    assert_eq!(section_at(&j, 0).format.color, palette().marker, "`>` dims");
}

#[test]
fn code_gets_a_backing_plate_and_prose_does_not() {
    // The background is the only style that paints OUTSIDE the glyph, so a leak
    // onto neighbouring prose is the most visible failure mode this can have.
    let src = "a `x` b";
    let mut j = job(src);
    restyle_job(&mut j, &spans(src), &palette());
    let pal = palette();
    assert_eq!(
        section_at(&j, 3).format.background,
        pal.code_bg,
        "code plate"
    );
    assert_eq!(section_at(&j, 3).format.color, pal.code, "code tone");
    for at in [0, 6] {
        assert_eq!(
            section_at(&j, at).format.background,
            Color32::TRANSPARENT,
            "prose at byte {at} must carry no plate"
        );
    }
}

#[test]
fn emphasis_slants_the_text_and_the_delimiters_stay_upright_and_dim() {
    let src = "an *em* word";
    let mut j = job(src);
    restyle_job(&mut j, &spans(src), &palette());
    assert!(section_at(&j, 4).format.italics, "`em` is slanted");
    assert!(!section_at(&j, 3).format.italics, "the `*` is not slanted");
    assert_eq!(section_at(&j, 3).format.color, palette().marker, "`*` dims");
    assert!(
        !section_at(&j, 0).format.italics,
        "surrounding prose is upright"
    );
}

#[test]
fn a_link_label_is_underlined_in_the_link_tone() {
    let src = "[go](u)";
    let mut j = job(src);
    restyle_job(&mut j, &spans(src), &palette());
    let label = section_at(&j, 1).format.clone();
    assert_eq!(label.color, palette().link);
    assert_eq!(label.underline.color, palette().link);
    assert!(label.underline.width > 0.0, "the label must be underlined");
    assert_eq!(
        section_at(&j, 5).format.underline.width,
        0.0,
        "the destination must NOT be underlined"
    );
}

#[test]
fn an_empty_span_list_leaves_the_job_byte_identical() {
    // The plain-prose fast path. Any churn here would cost every non-markdown
    // buffer a section rebuild per layout for no visual change.
    let src = "ordinary prose";
    let mut j = job(src);
    let before: Vec<_> = j.sections.iter().map(|s| s.byte_range.clone()).collect();
    restyle_job(&mut j, &[], &palette());
    let after: Vec<_> = j.sections.iter().map(|s| s.byte_range.clone()).collect();
    assert_eq!(before, after, "an empty span list must be a no-op");
}

#[test]
fn a_span_reaching_past_the_text_cannot_corrupt_the_sections() {
    // `spans` is bounded by construction, but `restyle_job` is `pub(crate)` and
    // the fold arm hands it a projection — so an out-of-range span must degrade
    // to "no cut", never to a panic or a section past the end of the string.
    let src = "abc";
    let mut j = job(src);
    restyle_job(
        &mut j,
        &[InlineSpan {
            range: 1..999,
            style: InlineStyle::Code,
        }],
        &palette(),
    );
    assert_eq!(j.text, src, "text untouched");
    assert!(
        j.sections.iter().all(|s| s.byte_range.end <= src.len()),
        "no section may reach past the text: {:?}",
        j.sections
            .iter()
            .map(|s| s.byte_range.clone())
            .collect::<Vec<_>>()
    );
}
