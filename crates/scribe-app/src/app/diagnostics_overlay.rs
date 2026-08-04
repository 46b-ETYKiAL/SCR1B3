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
        let covered: Vec<&str> = spans
            .iter()
            .map(|s| &SRC[s.start..s.end])
            .collect();
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
        assert_eq!(marks.iter().map(|(l, _)| *l).collect::<Vec<_>>(), vec![2, 11, 30]);
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
        assert_eq!(severity_prefix(99), "hint: ", "an unknown severity is a hint");
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
}
