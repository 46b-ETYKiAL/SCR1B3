//! PageUp / PageDown navigation, shared by both SCR1B3 editor paths.
//!
//! # Why this module exists
//!
//! egui implements **neither** PageUp nor PageDown. Its keyboard cursor
//! dispatch (`text_selection::cursor_range`) handles only the arrows, Home and
//! End; `Key::PageUp` / `Key::PageDown` appear in egui 0.34.3 solely as enum
//! variants. So both of SCR1B3's editors — the default `TextEdit` path and the
//! in-house rope path that auto-engages on large files — had dead page keys.
//!
//! # One algorithm, two buffer shapes
//!
//! Paging is "move `rows` lines in a direction, keeping the sticky goal
//! column". That algorithm is written **once**, in [`page_target`], against the
//! [`LineIndex`] abstraction. Two adapters supply the line arithmetic:
//!
//! * `Rope` — delegates to `scribe_core::editing::{line_col, char_at}`, which
//!   ride ropey's B-tree line index. Both are `O(log n)` in the buffer size, so
//!   a PageDown on a multi-GiB rope touches a handful of nodes, never the
//!   document. This is the property [`crate::rope_editor::word_nav`] exists to
//!   preserve for word navigation, held to here for vertical navigation.
//! * `str` — a direct scan, `O(n)`. This adapter serves ONLY the `TextEdit`
//!   path, which is capped by `rope_editor_auto_threshold_bytes` (16 MiB) and
//!   where egui itself already re-lays-out the whole galley every frame and
//!   reverses the whole buffer for a word move. Matching that complexity class
//!   is correct here; the sub-linear guarantee is a *rope-path* invariant.
//!
//! Because the target is computed by the same function for both paths, a page
//! move cannot drift between them as the editors evolve.

use egui::text::{CCursor, CCursorRange};
use ropey::Rope;
use scribe_core::editing::{self, EditState};

/// The line arithmetic [`page_target`] needs, abstracted over buffer shape.
///
/// Implementors must agree on ropey's line convention: a buffer is split on
/// `'\n'`, the count is `newlines + 1`, and a trailing newline therefore yields
/// a final empty line.
pub trait LineIndex {
    /// Total number of lines (ropey convention: `newlines + 1`).
    fn line_count(&self) -> usize;
    /// 0-based `(line, column)` of char index `ch`, column in chars from the
    /// line start. `ch` is clamped into the buffer.
    fn line_col_of(&self, ch: usize) -> (usize, usize);
    /// Char index at `(line, col)`, clamping `line` into the buffer and `col`
    /// to the line's content length (i.e. *before* any trailing newline).
    fn char_at_line_col(&self, line: usize, col: usize) -> usize;
}

impl LineIndex for Rope {
    fn line_count(&self) -> usize {
        self.len_lines()
    }
    fn line_col_of(&self, ch: usize) -> (usize, usize) {
        editing::line_col(self, ch)
    }
    fn char_at_line_col(&self, line: usize, col: usize) -> usize {
        editing::char_at(self, line, col)
    }
}

impl LineIndex for str {
    fn line_count(&self) -> usize {
        self.chars().filter(|c| *c == '\n').count() + 1
    }

    fn line_col_of(&self, ch: usize) -> (usize, usize) {
        let mut line = 0_usize;
        let mut col = 0_usize;
        for (i, c) in self.chars().enumerate() {
            if i >= ch {
                break;
            }
            if c == '\n' {
                line += 1;
                col = 0;
            } else {
                col += 1;
            }
        }
        (line, col)
    }

    fn char_at_line_col(&self, line: usize, col: usize) -> usize {
        // Clamp to the LAST line, exactly as `editing::char_at` does for a
        // rope. Walking off the end and returning the buffer end instead would
        // make the two adapters disagree for an out-of-range line.
        let line = line.min(self.line_count().saturating_sub(1));
        let mut cur_line = 0_usize;
        let mut line_start = 0_usize;
        let mut idx = 0_usize;
        // Locate the requested line's first char index.
        for c in self.chars() {
            if cur_line == line {
                break;
            }
            idx += 1;
            if c == '\n' {
                cur_line += 1;
                line_start = idx;
            }
        }
        // Content length of that line, excluding the trailing newline.
        let content = self
            .chars()
            .skip(line_start)
            .take_while(|c| *c != '\n')
            .count();
        line_start + col.min(content)
    }
}

/// Char index `rows` lines away from `cursor` in direction `dir` (`-1` up,
/// `+1` down), plus the goal column that should be carried forward.
///
/// The target line is **clamped** into the buffer, so PageUp at the top and
/// PageDown at the bottom land on the first / last line at the goal column
/// rather than doing nothing — the same clamping discipline
/// `editing::move_vertical` already uses for the arrow keys, so a page move and
/// a run of arrow moves agree.
pub fn page_target<T: LineIndex + ?Sized>(
    buf: &T,
    cursor: usize,
    dir: isize,
    rows: usize,
    goal_col: Option<usize>,
) -> (usize, usize) {
    let (line, col) = buf.line_col_of(cursor);
    let goal = goal_col.unwrap_or(col);
    let last_line = buf.line_count().saturating_sub(1);
    // `rows.max(1)` keeps a zero/unmeasured viewport from making the key inert;
    // the saturating product keeps a pathological row count from wrapping.
    let delta = dir.saturating_mul(rows.max(1) as isize);
    let target_line = (line as isize)
        .saturating_add(delta)
        .clamp(0, last_line as isize) as usize;
    (buf.char_at_line_col(target_line, goal), goal)
}

/// Page one caret of the rope path, extending the selection iff `select`.
///
/// Sub-linear: the whole cost is two ropey line-index lookups.
pub fn move_page(rope: &Rope, st: &mut EditState, dir: isize, rows: usize, select: bool) {
    let (next, goal) = page_target(rope, st.cursor, dir, rows, st.goal_col);
    st.cursor = next;
    if !select {
        st.anchor = next;
    }
    st.goal_col = Some(goal);
}

/// Page the `TextEdit` path's cursor range over `text`.
///
/// egui's `CCursorRange` carries `primary` (the caret) and `secondary` (the
/// anchor). A non-extending page collapses both onto the target — matching how
/// egui's own arrow handling treats a plain (unshifted) vertical move.
pub fn page_ccursor_range(
    text: &str,
    range: CCursorRange,
    dir: isize,
    rows: usize,
    select: bool,
) -> CCursorRange {
    let (next, _) = page_target(text, range.primary.index, dir, rows, None);
    let primary = CCursor::new(next);
    if select {
        CCursorRange::two(range.secondary, primary)
    } else {
        CCursorRange::one(primary)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn rope(s: &str) -> Rope {
        Rope::from_str(s)
    }

    /// 6 lines, ascending width, so a clamped column is observable.
    const DOC: &str = "aaaa\nbb\ncccccc\nd\neeeee\nffff";

    #[test]
    fn rope_and_str_adapters_agree_on_line_arithmetic() {
        // The two adapters back the SAME algorithm on the two paths; if they
        // disagreed, a page move would silently differ between the rope editor
        // and the TextEdit editor.
        let r = rope(DOC);
        for ch in 0..=DOC.chars().count() {
            assert_eq!(
                r.line_col_of(ch),
                DOC.line_col_of(ch),
                "line_col disagreement at char {ch}"
            );
        }
        assert_eq!(r.line_count(), DOC.line_count());
        for line in 0..8 {
            for col in [0_usize, 1, 3, 99] {
                assert_eq!(
                    r.char_at_line_col(line, col),
                    DOC.char_at_line_col(line, col),
                    "char_at disagreement at ({line}, {col})"
                );
            }
        }
    }

    #[test]
    fn str_adapter_matches_ropey_trailing_newline_convention() {
        assert_eq!("a\n".line_count(), rope("a\n").len_lines());
        assert_eq!("".line_count(), rope("").len_lines());
        assert_eq!("a\nb".line_count(), rope("a\nb").len_lines());
    }

    #[test]
    fn page_down_moves_by_exactly_the_row_count() {
        let r = rope(DOC);
        // From line 0 col 0, 2 rows down = line 2 col 0 = char index 8.
        let (target, goal) = page_target(&r, 0, 1, 2, None);
        assert_eq!(r.line_col_of(target), (2, 0));
        assert_eq!(goal, 0);
    }

    #[test]
    fn page_up_at_buffer_top_clamps_to_the_first_line() {
        let r = rope(DOC);
        // Caret on line 1 col 1; paging up 20 rows clamps to line 0, keeping
        // the goal column (arrow-key parity), and does NOT wrap or panic.
        let start = editing::char_at(&r, 1, 1);
        let (target, goal) = page_target(&r, start, -1, 20, None);
        assert_eq!(r.line_col_of(target), (0, 1));
        assert_eq!(goal, 1);
    }

    #[test]
    fn page_down_at_buffer_bottom_clamps_to_the_last_line() {
        let r = rope(DOC);
        let last = r.len_lines() - 1;
        let start = editing::char_at(&r, last, 0);
        let (target, _) = page_target(&r, start, 1, 50, None);
        assert_eq!(r.line_col_of(target), (last, 0));
        // Already on the last line at column 0 -> a further PageDown is a no-op,
        // never an out-of-range index.
        assert_eq!(target, start);
    }

    #[test]
    fn page_down_at_bottom_of_a_trailing_newline_buffer_lands_on_the_empty_line() {
        let r = rope("a\nb\n");
        let (target, _) = page_target(&r, 0, 1, 10, None);
        assert_eq!(target, r.len_chars());
        assert_eq!(r.line_col_of(target), (2, 0));
    }

    #[test]
    fn zero_rows_still_moves_one_line() {
        // An unmeasured viewport must not make the key inert.
        let r = rope(DOC);
        let (target, _) = page_target(&r, 0, 1, 0, None);
        assert_eq!(r.line_col_of(target), (1, 0));
    }

    #[test]
    fn goal_column_survives_paging_through_a_short_line() {
        let r = rope(DOC);
        let mut st = EditState::at(editing::char_at(&r, 2, 5)); // line 2, col 5
        move_page(&r, &mut st, 1, 1, false); // -> line 3 ("d"), col clamps to 1
        assert_eq!(r.line_col_of(st.cursor), (3, 1));
        move_page(&r, &mut st, 1, 1, false); // -> line 4 ("eeeee"), col 5 restored
        assert_eq!(r.line_col_of(st.cursor), (4, 5));
        assert_eq!(st.goal_col, Some(5));
    }

    #[test]
    fn unshifted_page_collapses_the_selection_shifted_page_extends_it() {
        let r = rope(DOC);
        let anchor = 0;
        let mut st = EditState {
            cursor: 0,
            anchor,
            goal_col: None,
        };
        move_page(&r, &mut st, 1, 2, true);
        assert_eq!(st.anchor, anchor, "shift keeps the anchor put");
        assert!(st.has_selection(), "shift+page selects");

        let mut st2 = EditState::at(0);
        move_page(&r, &mut st2, 1, 2, false);
        assert!(!st2.has_selection(), "plain page collapses");
        assert_eq!(st2.anchor, st2.cursor);
    }

    #[test]
    fn ccursor_range_page_extends_only_when_selecting() {
        let start = CCursorRange::one(CCursor::new(0));
        let extended = page_ccursor_range(DOC, start, 1, 2, true);
        assert_eq!(extended.secondary.index, 0);
        assert_eq!(extended.primary.index, DOC.char_at_line_col(2, 0));

        let plain = page_ccursor_range(DOC, start, 1, 2, false);
        assert_eq!(plain.primary.index, plain.secondary.index);
        assert_eq!(plain.primary.index, DOC.char_at_line_col(2, 0));
    }

    #[test]
    fn ccursor_range_page_up_at_top_clamps() {
        let text = "a\nb\nc";
        let at_c = CCursorRange::one(CCursor::new(4));
        let up = page_ccursor_range(text, at_c, -1, 99, false);
        assert_eq!(up.primary.index, 0);
    }

    /// Sub-linearity guard for the ROPE path (the whole reason that path
    /// exists), stated as a SCALE RATIO rather than an absolute wall-clock
    /// bound — so it means the same thing on a fast machine, a loaded machine,
    /// and a debug vs release build.
    ///
    /// The same number of page moves runs against a 1k-line rope and a rope
    /// 2000x larger. Ropey's `O(log n)` line index makes the big buffer only
    /// marginally slower; a whole-buffer scan would make it ~2000x slower. The
    /// 12x ceiling sits far below that and far above any plausible log-factor
    /// or cache-locality difference.
    #[test]
    fn rope_paging_cost_does_not_scale_with_buffer_size() {
        const OPS: usize = 20_000;
        const LINE: &str = "lorem ipsum dolor\n";
        let small = Rope::from_str(&LINE.repeat(1_000));
        let big = Rope::from_str(&LINE.repeat(2_000_000));
        assert!(big.len_chars() > 30_000_000, "buffer is genuinely large");

        let run = |rope: &Rope| {
            let mut st = EditState::at(rope.len_chars() / 2);
            let started = Instant::now();
            for i in 0..OPS {
                move_page(rope, &mut st, if i % 2 == 0 { 1 } else { -1 }, 40, false);
            }
            started.elapsed().as_secs_f64()
        };
        // Warm the code paths so the first-run cost is not attributed to size.
        run(&small);
        let small_s = run(&small).max(1e-6);
        let big_s = run(&big);

        assert!(
            big_s < small_s * 12.0,
            "paging cost scales with buffer size (looks O(n)): \
             1k lines took {small_s:.4}s, 2M lines took {big_s:.4}s \
             for {OPS} moves each"
        );
    }
}
