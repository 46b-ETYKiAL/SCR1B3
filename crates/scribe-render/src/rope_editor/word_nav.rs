//! Sub-linear word-boundary navigation for the in-house rope editor path.
//!
//! # Why this module exists
//!
//! The rope path is the editor SCR1B3 auto-engages above
//! `rope_editor_auto_threshold_bytes` (16 MiB by default), so it is the path a
//! user gets on *large* files. egui's own word navigation
//! ([`ccursor_previous_word`]) reverses the **entire buffer** through a
//! grapheme iterator on every keypress — `O(n)` per Ctrl+Left. On a multi-MiB
//! (let alone multi-GiB) rope that is unusable.
//!
//! # How parity is kept without the `O(n)` cost
//!
//! Word segmentation under UAX#29 (plus egui's extra "`.` is a boundary" rule)
//! depends only on a *bounded neighbourhood* of the cursor. So instead of
//! re-implementing a third word-boundary convention — which would silently
//! drift from the `TextEdit` path — this module extracts a **whitespace-
//! delimited window** of at most [`WINDOW_CHARS`] chars either side of the
//! caret and hands that window to **egui's own** boundary functions. The
//! answer is therefore *identical by construction* to what the `TextEdit` path
//! would compute, at `O(window)` instead of `O(buffer)`.
//!
//! # Where the window may be cut
//!
//! Not at *any* whitespace. egui's scanner answers "the start of the next
//! non-word run, or the end of the word after it", so when the caret sits ON
//! whitespace it must see past the whitespace run AND the word that follows to
//! find the boundary beyond. Cutting at the first whitespace hit therefore
//! truncates the answer (a caret on the space in `let x = …` reported the space
//! itself instead of the boundary after `x`).
//!
//! The rule that is actually sufficient: walk outward and cut at the first
//! whitespace char reached **after at least one non-whitespace char**. That
//! yields a cut position `w` with `cursor < w` (forward) that is a genuine word
//! boundary, so:
//!
//! * the window's prefix segments exactly as the document does (both scans
//!   start from a boundary), and
//! * the scan can never want to return an index at or beyond `w` — at `w`
//!   itself egui's `cursor_ci < word_ci && !all_word_chars(word)` test already
//!   fires, so the withheld remainder is unreachable.
//!
//! The same rule mirrored backwards gives the window a leading whitespace char,
//! which is what makes egui's *reversed-string* scan well-formed: the reversed
//! window then both begins and ends on a segmentation break. The answer is
//! identical to the whole-buffer answer, which
//! [`tests::windowed_boundaries_match_egui_whole_buffer_boundaries`] asserts
//! exhaustively. Only a single "word" longer than the budget can differ, and
//! there the window cap is the deliberate bound (see [`WINDOW_CHARS`]).

use egui::text::CCursor;
use egui::text_selection::text_cursor_state::{ccursor_next_word, ccursor_previous_word};
use ropey::Rope;
use scribe_core::editing::{self, EditState};

/// Max chars of context taken on each side of the caret when resolving a word
/// boundary. This is the constant that makes word navigation `O(1)` in the
/// buffer size: a Ctrl+Left on a 4 GiB rope touches at most `2 * WINDOW_CHARS`
/// chars. A run of non-whitespace longer than this is truncated at the window
/// edge (the caret still moves, just to the budget edge) — the alternative is
/// re-introducing the unbounded scan this module exists to remove.
pub const WINDOW_CHARS: usize = 512;

/// A whitespace-delimited slice of the rope around the caret, plus the caret's
/// index *within* the slice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    /// The extracted text.
    pub text: String,
    /// Absolute char index in the rope of `text`'s first char.
    pub start: usize,
    /// Caret index relative to `start` (i.e. into `text`).
    pub rel: usize,
}

impl Window {
    /// Number of chars examined to answer one boundary query. Always bounded
    /// by `2 * WINDOW_CHARS`, independent of the rope's length — the property
    /// the sub-linearity test asserts.
    pub fn examined_chars(&self) -> usize {
        self.text.chars().count()
    }
}

/// Extract the window around `cursor` (see the module docs for the cut rule).
///
/// Backwards: walk at most [`WINDOW_CHARS`] chars and cut at the first
/// whitespace char reached after at least one non-whitespace char, keeping that
/// whitespace as the window's FIRST char. Forwards: mirrored, keeping the
/// terminating whitespace as the window's LAST char.
pub fn window_around(rope: &Rope, cursor: usize) -> Window {
    let len = rope.len_chars();
    let cursor = cursor.min(len);

    // ---- backwards ----
    let mut start = cursor;
    {
        let mut it = rope.chars_at(cursor);
        let mut steps = 0;
        let mut saw_word_char = false;
        while start > 0 && steps < WINDOW_CHARS {
            let Some(ch) = it.prev() else { break };
            start -= 1;
            steps += 1;
            if ch.is_whitespace() {
                if saw_word_char {
                    break;
                }
            } else {
                saw_word_char = true;
            }
        }
    }

    // ---- forwards ----
    let mut end = cursor;
    {
        let mut it = rope.chars_at(cursor);
        let mut steps = 0;
        let mut saw_word_char = false;
        while end < len && steps < WINDOW_CHARS {
            let Some(ch) = it.next() else { break };
            end += 1;
            steps += 1;
            if ch.is_whitespace() {
                if saw_word_char {
                    break;
                }
            } else {
                saw_word_char = true;
            }
        }
    }

    Window {
        text: rope.slice(start..end).chars().collect(),
        start,
        rel: cursor - start,
    }
}

/// Char index of the previous word boundary before `cursor` (egui parity).
pub fn prev_word_boundary(rope: &Rope, cursor: usize) -> usize {
    let w = window_around(rope, cursor);
    let idx = ccursor_previous_word(&w.text, CCursor::new(w.rel)).index;
    (w.start + idx).min(rope.len_chars())
}

/// Char index of the next word boundary after `cursor` (egui parity).
pub fn next_word_boundary(rope: &Rope, cursor: usize) -> usize {
    let w = window_around(rope, cursor);
    let idx = ccursor_next_word(&w.text, CCursor::new(w.rel)).index;
    (w.start + idx).min(rope.len_chars())
}

/// Move one caret by a word in `dir` (-1 left, +1 right), extending the
/// selection iff `select`.
///
/// Matches egui's `move_single_cursor`: unlike the plain arrow keys, a word
/// move does **not** collapse an existing selection onto its edge — it moves
/// from the caret and (when `select` is false) drags the anchor along.
pub fn move_word(rope: &Rope, st: &mut EditState, dir: isize, select: bool) {
    let target = if dir < 0 {
        prev_word_boundary(rope, st.cursor)
    } else {
        next_word_boundary(rope, st.cursor)
    };
    st.cursor = target;
    if !select {
        st.anchor = target;
    }
    st.goal_col = None;
}

/// Move one caret to the document start/end (`dir < 0` / `dir > 0`).
pub fn move_document(rope: &Rope, st: &mut EditState, dir: isize, select: bool) {
    let target = if dir < 0 { 0 } else { rope.len_chars() };
    st.cursor = target;
    if !select {
        st.anchor = target;
    }
    st.goal_col = None;
}

/// Delete one word before the caret (Ctrl+Backspace).
///
/// egui parity: when a selection is active the selection is deleted instead of
/// a word (`cursor_range.single()` is `None`, so `check_for_mutating_key_press`
/// falls through to `delete_selected`).
pub fn delete_word_prev(rope: &mut Rope, st: &mut EditState) {
    if editing::delete_selection(rope, st) {
        return;
    }
    let target = prev_word_boundary(rope, st.cursor);
    if target < st.cursor {
        st.anchor = target;
        editing::delete_selection(rope, st);
    }
}

/// Delete one word after the caret (Ctrl+Delete). Selection-first, as above.
pub fn delete_word_next(rope: &mut Rope, st: &mut EditState) {
    if editing::delete_selection(rope, st) {
        return;
    }
    let target = next_word_boundary(rope, st.cursor);
    if target > st.cursor {
        st.anchor = target;
        editing::delete_selection(rope, st);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// Corpus deliberately mixing every boundary class the two paths must agree
    /// on: ASCII words, a `.`-separated host (egui's extra boundary rule), a
    /// whitespace run, mixed punctuation, underscores, digits, and CJK (which
    /// UAX#29 segments without spaces).
    const CORPUS: &str = "let x = www.example.com;   foo_bar99  (a,b)\nCJK-below\ttail";

    /// CJK corpus kept separate so the parity sweep covers a no-space script.
    const CJK_CORPUS: &str = "日本語のテキスト abc 漢字とかな mixed";

    fn r(s: &str) -> Rope {
        Rope::from_str(s)
    }

    /// The windowed rope scan must return EXACTLY what egui's own whole-buffer
    /// scan returns, at every caret position. This is what makes "one word
    /// boundary implementation" true rather than aspirational: the rope path
    /// calls the same `ccursor_*_word` functions the `TextEdit` path does, and
    /// this asserts the windowing never changes the answer.
    #[test]
    fn windowed_boundaries_match_egui_whole_buffer_boundaries() {
        for text in [CORPUS, CJK_CORPUS] {
            let rope = r(text);
            let n = text.chars().count();
            for i in 0..=n {
                assert_eq!(
                    next_word_boundary(&rope, i),
                    ccursor_next_word(text, CCursor::new(i)).index,
                    "next-word disagreement at char {i} of {text:?}"
                );
                assert_eq!(
                    prev_word_boundary(&rope, i),
                    ccursor_previous_word(text, CCursor::new(i)).index,
                    "prev-word disagreement at char {i} of {text:?}"
                );
            }
        }
    }

    #[test]
    fn next_word_stops_at_a_dot_boundary() {
        // egui treats `.` as a boundary (Mac `www.example.com` behaviour); the
        // rope path inherits it rather than re-deciding it.
        let text = "www.example.com";
        let rope = r(text);
        let first = next_word_boundary(&rope, 0);
        assert_eq!(first, 3, "stops before the first dot");
        assert!(next_word_boundary(&rope, first) > first, "then advances");
    }

    #[test]
    fn next_word_from_inside_a_word_stops_at_the_following_whitespace_run() {
        // Caret inside `alpha` -> the boundary is the START of the run, so the
        // caret parks at the word's end (egui/browser behaviour).
        let text = "alpha     beta";
        let rope = r(text);
        assert_eq!(next_word_boundary(&rope, 0), 5);
        assert_eq!(next_word_boundary(&rope, 2), 5);
    }

    #[test]
    fn next_word_from_on_whitespace_skips_the_run_and_the_word_after_it() {
        // Caret ON the whitespace run -> egui skips the run (a pure-whitespace
        // "word" is never a stop) and lands at the end of the next word. This
        // is the case the window cut rule exists for: answering it needs sight
        // of the run AND the word beyond it.
        let text = "alpha     beta gamma";
        let rope = r(text);
        assert_eq!(next_word_boundary(&rope, 5), 14, "end of `beta`");
        assert_eq!(next_word_boundary(&rope, 7), 14, "mid-run, same answer");
    }

    #[test]
    fn word_move_handles_cjk_without_spaces() {
        let text = "日本語のテキスト";
        let rope = r(text);
        let stop = next_word_boundary(&rope, 0);
        assert!(stop > 0, "CJK advances");
        assert!(stop <= text.chars().count());
        assert_eq!(stop, ccursor_next_word(text, CCursor::new(0)).index);
        assert_eq!(prev_word_boundary(&rope, stop), 0, "and walks back out");
    }

    #[test]
    fn move_word_left_does_not_collapse_onto_the_selection_edge() {
        // egui's word move (unlike a plain arrow) moves from the caret even
        // with a live selection; asserting it pins that parity.
        let rope = r("alpha beta gamma");
        let mut st = EditState {
            cursor: 10,
            anchor: 6,
            goal_col: None,
        };
        move_word(&rope, &mut st, -1, false);
        assert_eq!(st.cursor, 6, "moved a word back from the CARET (10)");
        assert_eq!(st.anchor, st.cursor, "unshifted move drags the anchor");
    }

    #[test]
    fn shifted_word_move_extends_the_selection() {
        let rope = r("alpha beta gamma");
        let mut st = EditState::at(0);
        move_word(&rope, &mut st, 1, true);
        assert_eq!(st.anchor, 0, "anchor pinned");
        assert!(st.has_selection());
        assert_eq!(st.cursor, next_word_boundary(&rope, 0));
    }

    #[test]
    fn move_document_jumps_to_both_ends_and_can_select() {
        let rope = r("alpha\nbeta");
        let mut st = EditState::at(3);
        move_document(&rope, &mut st, 1, false);
        assert_eq!(st.cursor, rope.len_chars());
        assert!(!st.has_selection());

        let mut st = EditState::at(3);
        move_document(&rope, &mut st, -1, true);
        assert_eq!(st.cursor, 0);
        assert_eq!(st.anchor, 3, "shift keeps the anchor");
    }

    #[test]
    fn delete_word_prev_at_line_start_joins_the_previous_line() {
        // Ctrl+Backspace with the caret at column 0 must consume backwards
        // across the newline, not no-op.
        let mut rope = r("alpha beta\ngamma");
        let line_start = rope.line_to_char(1);
        let mut st = EditState::at(line_start);
        delete_word_prev(&mut rope, &mut st);
        let after = rope.to_string();
        assert!(
            !after.contains("beta\n"),
            "consumed backwards past the newline, got {after:?}"
        );
        assert!(after.ends_with("gamma"));
        assert!(st.cursor < line_start, "caret moved back");
        assert!(!st.has_selection(), "selection collapsed after the delete");
    }

    #[test]
    fn delete_word_prev_at_buffer_start_is_a_no_op() {
        let mut rope = r("alpha");
        let mut st = EditState::at(0);
        delete_word_prev(&mut rope, &mut st);
        assert_eq!(rope.to_string(), "alpha");
        assert_eq!(st.cursor, 0);
    }

    #[test]
    fn delete_word_next_at_buffer_end_is_a_no_op() {
        let mut rope = r("alpha");
        let end = rope.len_chars();
        let mut st = EditState::at(end);
        delete_word_next(&mut rope, &mut st);
        assert_eq!(rope.to_string(), "alpha");
        assert_eq!(st.cursor, end);
    }

    #[test]
    fn word_delete_removes_the_selection_when_one_is_active() {
        // egui parity: with a selection live, Ctrl+Backspace deletes the
        // SELECTION, not an extra word beyond it.
        for use_prev in [true, false] {
            let mut rope = r("alpha beta gamma");
            let mut st = EditState {
                cursor: 10,
                anchor: 6,
                goal_col: None,
            };
            if use_prev {
                delete_word_prev(&mut rope, &mut st);
            } else {
                delete_word_next(&mut rope, &mut st);
            }
            assert_eq!(rope.to_string(), "alpha  gamma");
            assert_eq!(st.cursor, 6);
        }
    }

    /// The window is the constant that makes word navigation `O(1)` in buffer
    /// size. This asserts the *step count* directly — no timing involved — so
    /// an accidental whole-buffer scan cannot slip through.
    #[test]
    fn window_examines_a_bounded_number_of_chars_on_a_huge_rope() {
        let big = Rope::from_str(&"lorem ipsum dolor sit amet\n".repeat(2_000_000));
        assert!(big.len_chars() > 50_000_000, "buffer is genuinely large");
        let probes = [
            0,
            1,
            big.len_chars() / 2,
            big.len_chars() - 1,
            big.len_chars(),
        ];
        for probe in probes {
            let w = window_around(&big, probe);
            assert!(
                w.examined_chars() <= 2 * WINDOW_CHARS,
                "examined {} chars at {probe} — window bound is {}",
                w.examined_chars(),
                2 * WINDOW_CHARS
            );
        }
    }

    /// Companion end-to-end guard, stated as a SCALE RATIO so it is meaningful
    /// on any machine and in any build profile: the same word-move workload on
    /// a 1k-line rope and one 2000x larger must cost about the same. egui's own
    /// whole-buffer scan (which reverses the entire string per Ctrl+Left) would
    /// be ~2000x slower on the big rope; the windowed scan is flat.
    #[test]
    fn word_navigation_cost_does_not_scale_with_buffer_size() {
        const OPS: usize = 20_000;
        const LINE: &str = "lorem ipsum dolor sit amet\n";
        let small = Rope::from_str(&LINE.repeat(1_000));
        let big = Rope::from_str(&LINE.repeat(2_000_000));

        let run = |rope: &Rope| {
            let mut st = EditState::at(rope.len_chars() / 2);
            let started = Instant::now();
            for i in 0..OPS {
                move_word(rope, &mut st, if i % 2 == 0 { 1 } else { -1 }, false);
            }
            started.elapsed().as_secs_f64()
        };
        run(&small); // warm-up
        let small_s = run(&small).max(1e-6);
        let big_s = run(&big);

        assert!(
            big_s < small_s * 12.0,
            "word navigation cost scales with buffer size (looks O(n)): \
             1k lines took {small_s:.4}s, 2M lines took {big_s:.4}s \
             for {OPS} moves each"
        );
    }
}
