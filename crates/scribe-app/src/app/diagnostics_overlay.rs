//! Turning LSP diagnostics into something the user can actually see.
//!
//! The client has always drained `publishDiagnostics` and the status bar has
//! always shown two integers ("3e / 7"). Two integers do not tell you WHICH
//! line is wrong, and hovering nothing tells you why. This module is the pure
//! half of the fix: it maps each diagnostic's UTF-16 `(line, character)` range
//! onto byte offsets in the buffer, decides the gutter mark for each line, and
//! answers "which diagnostic is under this offset?" for the hover tooltip. The
//! painting itself lives in `frame_tick`, which owns the galley.
//!
//! Everything here is a pure function of `(text, diagnostics)`, so the
//! placement rules — which are the part that can silently be WRONG, by
//! underlining the wrong span or the wrong line — are asserted directly.

use scribe_core::lsp::{Diagnostic, LineIndex};

/// LSP severity numbers. Lower is worse, which is why [`worst_severity`]
/// takes a `min`.
pub(crate) const SEVERITY_ERROR: u8 = 1;
pub(crate) const SEVERITY_WARNING: u8 = 2;
pub(crate) const SEVERITY_INFO: u8 = 3;

/// One diagnostic resolved onto byte offsets in the current buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiagSpan {
    /// Byte offset of the first character to underline.
    pub start: usize,
    /// Byte offset one past the last character to underline. Always `> start`
    /// for a span that is inside the buffer (see [`diagnostic_spans`]).
    pub end: usize,
    pub severity: u8,
    pub message: String,
}

impl DiagSpan {
    pub(crate) fn contains(&self, byte: usize) -> bool {
        byte >= self.start && byte < self.end
    }
}

/// Resolve every diagnostic onto a byte span in `text`.
///
/// Three rules that are the difference between a useful underline and a
/// misleading one:
///
///   * **Zero-width ranges are widened to one character.** Servers routinely
///     publish a zero-width range for "expected `;` here" / "unclosed
///     delimiter". A zero-width squiggle paints nothing at all, so the user
///     sees the status-bar count go up with nothing marked. One character is
///     the minimum that is visible and still points at the right place.
///   * **A reversed or stale range is dropped, not clamped into nonsense.** A
///     diagnostic whose end resolves before its start belongs to a document
///     version we have already edited past; underlining a guessed span would be
///     worse than underlining nothing.
///   * **A span past the end of the buffer is dropped.** Same reason.
///
/// One [`LineIndex`] is built for the whole batch, so this is `O(text)` once
/// rather than `O(text)` per diagnostic — it runs on every frame that has
/// diagnostics.
pub(crate) fn diagnostic_spans(text: &str, diags: &[Diagnostic]) -> Vec<DiagSpan> {
    if diags.is_empty() || text.is_empty() {
        return Vec::new();
    }
    let index = LineIndex::new(text);
    let mut out = Vec::with_capacity(diags.len());
    for d in diags {
        let start = index.offset_of(text, d.line, d.character);
        let mut end = index.offset_of(text, d.end_line, d.end_character);
        if start >= text.len() {
            continue; // entirely past the end of the buffer
        }
        if end < start {
            continue; // reversed: a stale range from an older document version
        }
        if end == start {
            // Widen to the next char boundary so the squiggle has width.
            end = text[start..]
                .chars()
                .next()
                .map_or(start, |c| start + c.len_utf8());
        }
        if end <= start {
            continue;
        }
        out.push(DiagSpan {
            start,
            end,
            severity: d.severity,
            message: d.message.clone(),
        });
    }
    out
}

/// Worst severity per **source line**, for the gutter marks.
///
/// Keyed on the diagnostic's START line: a multi-line error is marked at the
/// line the user has to look at. Returned sorted by line so the caller can
/// binary-search it while walking gutter rows.
pub(crate) fn gutter_marks(diags: &[Diagnostic]) -> Vec<(u32, u8)> {
    let mut marks: Vec<(u32, u8)> = Vec::new();
    for d in diags {
        match marks.binary_search_by_key(&d.line, |(l, _)| *l) {
            Ok(i) => marks[i].1 = marks[i].1.min(d.severity),
            Err(i) => marks.insert(i, (d.line, d.severity)),
        }
    }
    marks
}

/// One diagnostic's ink on ONE source line, in CHARACTER columns.
///
/// The `TextEdit` path paints per GALLEY row and gets its columns from the
/// galley it was handed. The rope editor lays every row out itself and hands
/// the host [`scribe_render::RopeRowGeom`] per SOURCE line instead, so the
/// overlay for that path needs the same spans re-expressed as `(line, column
/// range)` — which is what [`row_segments`] produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RowSegment {
    /// 0-based source line.
    pub line: usize,
    /// First character column to underline.
    pub start_col: usize,
    /// One past the last character column to underline.
    pub end_col: usize,
    pub severity: u8,
}

/// Byte range of source line `line`, EXCLUDING its line break.
///
/// `(text.len(), text.len())` for a line past the end of the buffer, so an
/// overlap test against it is empty rather than panicking.
fn line_byte_range(index: &LineIndex, text: &str, line: usize) -> (usize, usize) {
    let Ok(line) = u32::try_from(line) else {
        return (text.len(), text.len());
    };
    if line as usize >= index.line_count() {
        return (text.len(), text.len());
    }
    // `offset_of` stops at the line's break (or its end), so column 0 is the
    // line start and a column past every character is the line end.
    (
        index.offset_of(text, line, 0),
        index.offset_of(text, line, u32::MAX),
    )
}

/// Project byte spans onto per-line character-column segments, for the source
/// lines in `visible` only.
///
/// Scoped to the visible range on purpose: the rope path exists because the
/// buffer is large (16 MiB+ by default), and the whole point of that path is
/// that per-frame work is O(viewport). A projection over the whole document
/// would hand the rope editor back the O(n) cost it was introduced to remove.
///
/// A span covering several lines yields one segment per line — the same
/// per-row treatment the `TextEdit` painter applies, so a multi-line
/// diagnostic underlines every line it covers instead of being dropped.
/// Columns are CHARACTER indices into the line (what the row galley is keyed
/// on), never bytes and never UTF-16 units.
pub(crate) fn row_segments(
    text: &str,
    spans: &[DiagSpan],
    visible: std::ops::Range<usize>,
) -> Vec<RowSegment> {
    if spans.is_empty() || text.is_empty() || visible.is_empty() {
        return Vec::new();
    }
    let index = LineIndex::new(text);
    let mut out = Vec::new();
    for line in visible {
        if line >= index.line_count() {
            break; // past the end of the buffer, and so is every later line
        }
        let (ls, le) = line_byte_range(&index, text, line);
        for span in spans {
            let s = span.start.max(ls);
            let e = span.end.min(le);
            if s >= e {
                continue;
            }
            out.push(RowSegment {
                line,
                start_col: text[ls..s].chars().count(),
                end_col: text[ls..e].chars().count(),
                severity: span.severity,
            });
        }
    }
    out
}

/// Byte offset of character column `col` on source line `line`, clamped to the
/// line's end. The inverse of the column arithmetic in [`row_segments`], used
/// to resolve a pointer on a rope row back to a buffer offset for the hover.
pub(crate) fn byte_of_line_col(text: &str, line: usize, col: usize) -> usize {
    let index = LineIndex::new(text);
    let (ls, le) = line_byte_range(&index, text, line);
    text[ls..le]
        .char_indices()
        .nth(col)
        .map_or(le, |(i, _)| ls + i)
}

/// The hover text for a set of overlapping diagnostics at one offset.
///
/// The NARROWEST span leads: an inner "unknown field" inside an outer "this
/// expression has type …" is the more specific thing to say about the character
/// under the pointer. Ties break toward the worse severity. Every diagnostic
/// covering the byte is listed, one per line — dropping the outer one would
/// hide context the user needs.
pub(crate) fn hover_text(spans: &[DiagSpan], byte: usize) -> Option<String> {
    let mut hits: Vec<&DiagSpan> = spans.iter().filter(|s| s.contains(byte)).collect();
    if hits.is_empty() {
        return None;
    }
    hits.sort_by_key(|s| (s.end - s.start, s.severity));
    Some(
        hits.iter()
            .map(|s| format!("{}{}", severity_prefix(s.severity), s.message))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// The word that names a severity in the hover tooltip. Text, not colour: the
/// tooltip has to read correctly for a colour-blind user and in a screenshot.
pub(crate) fn severity_prefix(severity: u8) -> &'static str {
    match severity {
        SEVERITY_ERROR => "error: ",
        SEVERITY_WARNING => "warning: ",
        SEVERITY_INFO => "info: ",
        _ => "hint: ",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diag(line: u32, ch: u32, end_line: u32, end_ch: u32, sev: u8, msg: &str) -> Diagnostic {
        Diagnostic {
            uri: "file:///x.rs".into(),
            line,
            character: ch,
            end_line,
            end_character: end_ch,
            severity: sev,
            message: msg.into(),
        }
    }

    const SRC: &str = "fn main() {\n    let x = undefined_thing;\n}\n";

    #[test]
    fn a_diagnostic_underlines_exactly_the_identifier_the_server_named() {
        // The whole point of the feature. Assert on the TEXT the span covers,
        // not on the offsets — an off-by-one that still produced plausible
        // numbers would underline `undefined_thin` or `ndefined_thing`.
        let spans = diagnostic_spans(SRC, &[diag(1, 12, 1, 27, 1, "cannot find value")]);
        assert_eq!(spans.len(), 1);
        assert_eq!(
            &SRC[spans[0].start..spans[0].end],
            "undefined_thing",
            "the squiggle must cover exactly the offending identifier"
        );
        assert_eq!(spans[0].severity, 1);
        assert_eq!(spans[0].message, "cannot find value");
    }

    #[test]
    fn a_zero_width_range_is_widened_so_the_squiggle_is_visible() {
        // "expected `;`" arrives as a zero-width range. A zero-width underline
        // paints NOTHING — the count in the status bar goes up and the editor
        // shows the user nothing.
        let spans = diagnostic_spans(SRC, &[diag(1, 27, 1, 27, 1, "expected `;`")]);
        assert_eq!(spans.len(), 1);
        assert!(
            spans[0].end > spans[0].start,
            "a zero-width range must be widened, got {:?}",
            spans[0]
        );
        assert_eq!(
            &SRC[spans[0].start..spans[0].end],
            ";",
            "and it is widened over the NEXT character, at the right place"
        );
    }

    #[test]
    fn a_multi_line_range_covers_from_its_start_to_its_end() {
        let spans = diagnostic_spans(SRC, &[diag(0, 10, 2, 1, 1, "unclosed block")]);
        assert_eq!(
            &SRC[spans[0].start..spans[0].end],
            "{\n    let x = undefined_thing;\n}",
            "a multi-line diagnostic underlines the whole block"
        );
    }

    #[test]
    fn positions_are_utf16_units_so_wide_characters_do_not_shift_the_underline() {
        // `é` is 2 bytes / 1 UTF-16 unit; `😀` is 4 bytes / 2 units. A byte- or
        // char-counting mapping underlines the wrong text here while still
        // producing perfectly plausible-looking offsets.
        let src = "let s = \"é😀\"; let bad = 1;\n";
        // `bad` starts at UTF-16 column 19: `let s = "` (9) + é(1) + 😀(2) +
        // `"; let ` (7) = 19.
        let spans = diagnostic_spans(src, &[diag(0, 19, 0, 22, 2, "unused")]);
        assert_eq!(
            &src[spans[0].start..spans[0].end],
            "bad",
            "UTF-16 columns must be honoured, not bytes and not chars"
        );
    }

    #[test]
    fn a_stale_range_past_the_end_of_the_buffer_is_dropped_not_guessed() {
        // The server publishes against a version we have already edited past.
        // Underlining a guessed span is worse than underlining nothing.
        let spans = diagnostic_spans("short\n", &[diag(99, 0, 99, 5, 1, "stale")]);
        assert!(
            spans.is_empty(),
            "a diagnostic entirely past the buffer must be dropped, got {spans:?}"
        );
    }

    #[test]
    fn a_reversed_range_is_dropped() {
        let spans = diagnostic_spans(SRC, &[diag(1, 20, 1, 4, 1, "reversed")]);
        assert!(spans.is_empty(), "got {spans:?}");
    }

    #[test]
    fn no_diagnostics_and_an_empty_buffer_both_yield_nothing() {
        assert!(diagnostic_spans(SRC, &[]).is_empty());
        assert!(diagnostic_spans("", &[diag(0, 0, 0, 1, 1, "x")]).is_empty());
    }

    #[test]
    fn every_diagnostic_gets_a_span_not_just_the_first() {
        let spans = diagnostic_spans(
            SRC,
            &[
                diag(0, 3, 0, 7, 2, "unused fn"),
                diag(1, 8, 1, 9, 1, "unused var"),
                diag(1, 12, 1, 27, 1, "cannot find value"),
            ],
        );
        let covered: Vec<&str> = spans.iter().map(|s| &SRC[s.start..s.end]).collect();
        assert_eq!(covered, vec!["main", "x", "undefined_thing"]);
    }

    // ---- gutter ----

    #[test]
    fn the_gutter_marks_the_worst_severity_on_each_line() {
        // Two diagnostics on one line: the gutter shows the ERROR, not whichever
        // arrived last. A `max` (or a last-write-wins) would show the warning.
        let marks = gutter_marks(&[
            diag(4, 0, 4, 1, 2, "warn"),
            diag(4, 5, 4, 6, 1, "err"),
            diag(9, 0, 9, 1, 3, "info"),
        ]);
        assert_eq!(marks, vec![(4, 1), (9, 3)]);
    }

    #[test]
    fn the_gutter_marks_come_back_sorted_by_line_whatever_order_they_arrived_in() {
        let marks = gutter_marks(&[
            diag(30, 0, 30, 1, 1, "c"),
            diag(2, 0, 2, 1, 1, "a"),
            diag(11, 0, 11, 1, 1, "b"),
        ]);
        assert_eq!(
            marks.iter().map(|(l, _)| *l).collect::<Vec<_>>(),
            vec![2, 11, 30]
        );
    }

    #[test]
    fn the_gutter_marks_the_start_line_of_a_multi_line_diagnostic() {
        // The line the user has to look at is where the error BEGINS.
        let marks = gutter_marks(&[diag(3, 0, 40, 0, 1, "unclosed")]);
        assert_eq!(marks, vec![(3, 1)]);
    }

    #[test]
    fn no_diagnostics_means_no_gutter_marks() {
        assert!(gutter_marks(&[]).is_empty());
    }

    // ---- hover ----

    #[test]
    fn hovering_the_underlined_text_names_the_problem_and_hovering_elsewhere_does_not() {
        let spans = diagnostic_spans(SRC, &[diag(1, 12, 1, 27, 1, "cannot find value")]);
        let ident = SRC.find("undefined_thing").unwrap();
        assert_eq!(
            hover_text(&spans, ident).as_deref(),
            Some("error: cannot find value"),
            "hovering the squiggle must say what is wrong"
        );
        assert_eq!(
            hover_text(&spans, ident + 14).as_deref(),
            Some("error: cannot find value"),
            "the last character of the span still hovers"
        );
        assert!(
            hover_text(&spans, ident + 15).is_none(),
            "one past the end is OUTSIDE the span — the tooltip must not follow \
             the pointer off the underline"
        );
        assert!(hover_text(&spans, 0).is_none(), "and not at the file start");
    }

    #[test]
    fn the_hover_prefix_names_the_severity_in_words_not_only_in_colour() {
        // A colour-only signal is unreadable for a colour-blind user and in a
        // screenshot.
        assert_eq!(severity_prefix(SEVERITY_ERROR), "error: ");
        assert_eq!(severity_prefix(SEVERITY_WARNING), "warning: ");
        assert_eq!(severity_prefix(SEVERITY_INFO), "info: ");
        assert_eq!(severity_prefix(4), "hint: ");
        assert_eq!(
            severity_prefix(99),
            "hint: ",
            "an unknown severity is a hint"
        );
    }

    #[test]
    fn overlapping_diagnostics_lead_with_the_narrowest_and_list_them_all() {
        let spans = diagnostic_spans(
            SRC,
            &[
                diag(1, 4, 1, 27, 2, "outer statement problem"),
                diag(1, 12, 1, 27, 1, "inner identifier problem"),
            ],
        );
        let ident = SRC.find("undefined_thing").unwrap();
        assert_eq!(
            hover_text(&spans, ident).as_deref(),
            Some("error: inner identifier problem\nwarning: outer statement problem"),
            "the most specific diagnostic leads, and neither is dropped"
        );
        // Outside the inner span, only the outer one applies.
        let stmt = SRC.find("let x").unwrap();
        assert_eq!(
            hover_text(&spans, stmt).as_deref(),
            Some("warning: outer statement problem")
        );
    }

    // ---- the ROPE path's projection: byte spans -> per-line char columns ----
    //
    // The `TextEdit` overlay walks a galley and gets its columns from it. The
    // rope editor lays every row out itself and publishes per-SOURCE-LINE
    // geometry, so the overlay for that path needs the spans re-expressed as
    // `(line, column range)`. That arithmetic is where a rope-path squiggle
    // silently lands under the wrong characters, and it is invisible to the
    // `TextEdit` tests — hence its own set.

    /// Render `row_segments`' output as `line:"underlined text"`, so an
    /// off-by-one is a wrong STRING rather than a plausible-looking integer.
    fn underlined(text: &str, segs: &[RowSegment]) -> Vec<String> {
        segs.iter()
            .map(|s| {
                let line_start = byte_of_line_col(text, s.line, 0);
                let a = byte_of_line_col(text, s.line, s.start_col);
                let b = byte_of_line_col(text, s.line, s.end_col);
                debug_assert!(a >= line_start);
                format!("{}:{:?}", s.line, &text[a..b])
            })
            .collect()
    }

    #[test]
    fn row_segments_underlines_exactly_the_identifier_on_its_own_line() {
        let spans = diagnostic_spans(SRC, &[diag(1, 12, 1, 27, 1, "undefined")]);
        let segs = row_segments(SRC, &spans, 0..4);
        assert_eq!(underlined(SRC, &segs), vec![r#"1:"undefined_thing""#]);
        assert_eq!(segs[0].severity, SEVERITY_ERROR);
    }

    #[test]
    fn a_multi_line_span_yields_one_segment_per_line_it_covers() {
        // The behaviour a naive "underline start..end on the start line"
        // implementation gets wrong: the middle line must be underlined WHOLE,
        // the first from the diagnostic column to the line end, the last from
        // the line start to the diagnostic column.
        const SRC3: &str = "alpha bravo\ncharlie delta\necho foxtrot\n";
        let spans = diagnostic_spans(SRC3, &[diag(0, 6, 2, 4, 1, "spans three lines")]);
        let segs = row_segments(SRC3, &spans, 0..3);
        assert_eq!(
            underlined(SRC3, &segs),
            vec![r#"0:"bravo""#, r#"1:"charlie delta""#, r#"2:"echo""#],
            "each covered line gets its own segment, clipped to that line"
        );
        // …and the line break itself is never underlined: the last column of a
        // segment must not run past the line's own text.
        for s in &segs {
            let end = byte_of_line_col(SRC3, s.line, s.end_col);
            assert!(
                !SRC3[..end].ends_with('\n'),
                "segment on line {} underlines its line break",
                s.line
            );
        }
    }

    #[test]
    fn row_segments_is_scoped_to_the_visible_range() {
        // The whole reason the rope path exists is that per-frame work must be
        // O(viewport). A projection over the whole document would hand back the
        // O(n) cost the rope editor was introduced to remove — so a line
        // outside `visible` must produce NO segment even though its span
        // resolves perfectly well.
        const SRC3: &str = "alpha bravo\ncharlie delta\necho foxtrot\n";
        let spans = diagnostic_spans(SRC3, &[diag(2, 0, 2, 4, 1, "on the last line")]);
        assert!(
            row_segments(SRC3, &spans, 0..2).is_empty(),
            "line 2 is outside the visible range 0..2"
        );
        assert_eq!(row_segments(SRC3, &spans, 2..3).len(), 1);
        // An empty viewport is not a reason to walk the buffer.
        assert!(row_segments(SRC3, &spans, 0..0).is_empty());
    }

    #[test]
    fn columns_are_characters_not_bytes() {
        // A multibyte line is where byte arithmetic masquerading as columns
        // shows up: the row galley is keyed on CHARACTER index, so a span after
        // a multi-byte glyph must report the character column, not the byte
        // offset. `é` is 2 bytes, `→` is 3, `日` is 3.
        const SRC_U: &str = "é→日 tail\nplain\n";
        let start = SRC_U.find("tail").expect("fixture");
        let spans = diagnostic_spans(
            SRC_U,
            &[Diagnostic {
                uri: "file:///u.rs".into(),
                line: 0,
                character: 4, // UTF-16 units: é(1) →(1) 日(1) space(1)
                end_line: 0,
                end_character: 8,
                severity: SEVERITY_WARNING,
                message: "tail".into(),
            }],
        );
        assert_eq!(spans[0].start, start, "precondition: the span is on `tail`");
        let segs = row_segments(SRC_U, &spans, 0..2);
        assert_eq!(
            segs,
            vec![RowSegment {
                line: 0,
                start_col: 4,
                end_col: 8,
                severity: SEVERITY_WARNING,
            }],
            "columns must be CHARACTER indices (4..8), not byte offsets (9..13)"
        );
        assert_eq!(underlined(SRC_U, &segs), vec![r#"0:"tail""#]);
    }

    #[test]
    fn byte_of_line_col_is_the_inverse_of_the_column_arithmetic() {
        // The hover resolves a pointer x to a column and then back to a byte;
        // if the two disagree the tooltip names the wrong diagnostic.
        const SRC_U: &str = "é→日 tail\nplain\n";
        for (line, col, expect) in [(0usize, 0usize, "é"), (0, 3, " "), (1, 2, "a")] {
            let b = byte_of_line_col(SRC_U, line, col);
            assert!(
                SRC_U[b..].starts_with(expect),
                "line {line} col {col} resolved to byte {b}, which starts {:?} not {expect:?}",
                &SRC_U[b..(b + 4).min(SRC_U.len())]
            );
        }
        // A column past the end of the line clamps to the line's end — never
        // into the NEXT line, or a hover past the last character would report a
        // diagnostic belonging to the row below.
        let eol = byte_of_line_col(SRC_U, 0, 999);
        assert_eq!(&SRC_U[eol..eol + 1], "\n", "clamped to the line break");
        // …and a line past the end of the buffer clamps to the buffer end
        // rather than panicking.
        assert_eq!(byte_of_line_col(SRC_U, 99, 0), SRC_U.len());
    }

    /// The three empty-input short-circuits, each on its own.
    ///
    /// MUTATION NOTE — `cargo mutants -F row_segments` leaves exactly two
    /// survivors, both `replace || with && ` in this function's guard, and both
    /// are EQUIVALENT rather than a gap in the assertions below. With `&&` the
    /// guard stops short-circuiting and control falls into the main loop, which
    /// produces the same empty `Vec` for every one of the three cases: no spans
    /// means the inner loop never pushes, empty text means `line_count()` is 0
    /// so the outer loop breaks immediately, and an empty visible range means it
    /// never iterates. The only difference is the `LineIndex::new(text)` the
    /// guard avoids — an O(text) construction, which on the 16 MiB buffers this
    /// path exists for is the whole reason the short-circuit is written this
    /// way, but which has no output an assertion can read and no non-flaky
    /// timing test. Recorded here rather than pardoned or silenced, so the next
    /// reader knows these two were analysed and not missed.
    #[test]
    fn row_segments_handles_the_empty_cases_without_walking_the_buffer() {
        assert!(row_segments("", &[], 0..10).is_empty());
        assert!(row_segments(SRC, &[], 0..10).is_empty());
        let spans = diagnostic_spans(SRC, &[diag(1, 12, 1, 27, 1, "x")]);
        assert!(
            row_segments("", &spans, 0..10).is_empty(),
            "an empty buffer has no lines to project onto"
        );
        // A visible range that runs off the end of the buffer stops at the last
        // line instead of indexing past it.
        assert_eq!(row_segments(SRC, &spans, 0..9_999).len(), 1);
    }
}
