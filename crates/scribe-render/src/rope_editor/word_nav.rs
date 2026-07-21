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
//! Both window edges are cut at a whitespace char wherever one exists within
//! the budget, which is a definite word boundary in both scan directions — so
//! neither the forward scan nor egui's reversed-string scan can observe that
//! the rest of the document was withheld. Only a single "word" longer than the
//! budget can differ, and there the window cap is the deliberate bound (see
//! [`WINDOW_CHARS`]).

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

/// Extract the whitespace-delimited window around `cursor`.
///
/// Backwards: walk at most [`WINDOW_CHARS`] chars looking for a whitespace
/// char; cut the window so that whitespace is the window's FIRST char (keeping
/// it in-window preserves the "skip the whitespace run" behaviour egui's
/// scanner relies on). Forwards: symmetric — cut just after the first
/// whitespace char found.
pub fn window_around(rope: &Rope, cursor: usize) -> Window {
    let len = rope.len_chars();
    let cursor = cursor.min(len);

    // ---- backwards ----
    let mut start = cursor;
    {
        let mut it = rope.chars_at(cursor);
        let mut steps = 0;
        while start > 0 && steps < WINDOW_CHARS {
            let Some(ch) = it.prev() else { break };
            start -= 1;
            steps += 1;
            if ch.is_whitespace() {
                break;
            }
        }
    }

    // ---- forwards ----
    let mut end = cursor;
    {
        let mut it = rope.chars_at(cursor);
        let mut steps = 0;
        while end < len && steps < WINDOW_CHARS {
            let Some(ch) = it.next() else { break };
            end += 1;
            steps += 1;
            if ch.is_whitespace() {
                break;
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

/// Move one caret by `rows` lines in `dir`, preserving the sticky goal column.
///
/// This is the PageUp/PageDown primitive. It mirrors
/// `editing::move_vertical`'s goal-column discipline so paging down through a
/// short line and back up does not lose the original column.
pub fn move_page(rope: &Rope, st: &mut EditState, dir: isize, rows: usize, select: bool) {
    let (line, col) = editing::line_col(rope, st.cursor);
    let goal = st.goal_col.unwrap_or(col);
    let last_line = rope.len_lines().saturating_sub(1);
    let delta = dir.saturating_mul(rows.max(1) as isize);
    let target_line = (line as isize + delta).clamp(0, last_line as isize) as usize;
    let next = editing::char_at(rope, target_line, goal);
    st.cursor = next;
    if !select {
        st.anchor = next;
    }
    st.goal_col = Some(goal);
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
