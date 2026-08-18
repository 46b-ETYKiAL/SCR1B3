//! Editor text operations (indent, auto-indent, bracket-jump, datetime, duplicate, comment, line ops) — extracted from `mod.rs` (A-01 wave 2).
#![allow(clippy::wildcard_imports)]

use super::*;

/// P3-3 — built-in "new note from template" seeds (ride the plain-buffer path,
/// no new subsystem). Bodies are checklist-first so the task features are
/// discoverable immediately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NoteTemplate {
    Checklist,
    Meeting,
    Daily,
}

impl NoteTemplate {
    fn label(self) -> &'static str {
        match self {
            NoteTemplate::Checklist => "checklist",
            NoteTemplate::Meeting => "meeting",
            NoteTemplate::Daily => "daily",
        }
    }

    fn body(self) -> &'static str {
        match self {
            NoteTemplate::Checklist => "# Checklist\n\n- [ ] \n- [ ] \n- [ ] \n",
            NoteTemplate::Meeting => {
                "# Meeting notes\n\n**Date:** \n**Attendees:** \n\n\
                 ## Agenda\n\n- \n\n## Decisions\n\n- \n\n## Action items\n\n- [ ] \n"
            }
            NoteTemplate::Daily => {
                "# Daily note\n\n## Focus\n\n- [ ] \n\n## Notes\n\n- \n\n## Done\n\n- [x] \n"
            }
        }
    }
}

impl ScribeApp {
    /// Replace the active editor's selection (or insert at the caret) with
    /// `tab_width` spaces, then advance the caret — the Tab-key handler when
    /// `insert_spaces` is enabled. Operates directly on the TextEdit state for
    /// `id` so the caret tracks the edit.
    pub(super) fn indent_with_spaces(&mut self, ctx: &egui::Context, id: egui::Id, active: usize) {
        let Some(mut state) = egui::TextEdit::load_state(ctx, id) else {
            return;
        };
        let Some(range) = state.cursor.char_range() else {
            return;
        };
        let lo = range.primary.index.min(range.secondary.index);
        let hi = range.primary.index.max(range.secondary.index);
        let (new_text, new_idx) = apply_indent(
            &self.tabs[active].text,
            lo,
            hi,
            self.config.editor.tab_width,
        );
        self.tabs[active].set_text(new_text);
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::one(
                egui::text::CCursor::new(new_idx),
            )));
        state.store(ctx, id);
    }

    /// Auto-indent on Enter (#107): insert a newline that keeps the current
    /// line's leading whitespace, so indentation carries to the next line. Only
    /// acts on a single caret (no selection); returns false so the caller lets
    /// egui handle Enter normally otherwise.
    pub(super) fn auto_indent_newline(
        &mut self,
        ctx: &egui::Context,
        id: egui::Id,
        active: usize,
    ) -> bool {
        let Some(mut state) = egui::TextEdit::load_state(ctx, id) else {
            return false;
        };
        let Some(range) = state.cursor.char_range() else {
            return false;
        };
        // Only a collapsed caret — a selection+Enter should replace, which we
        // leave to egui.
        if range.primary.index != range.secondary.index {
            return false;
        }
        let cursor = range.primary.index;

        // P0-2 — smart list continuation: for note files with smart-lists on,
        // continue the list marker (or terminate on an empty item) when Enter is
        // pressed at the end of a list line.
        if self.config.editor.smart_lists && self.note_file_active(active) {
            if let Some((new_text, new_idx)) = self.smart_list_newline(active, cursor) {
                self.tabs[active].set_text(new_text);
                state
                    .cursor
                    .set_char_range(Some(egui::text::CCursorRange::one(
                        egui::text::CCursor::new(new_idx),
                    )));
                state.store(ctx, id);
                return true;
            }
        }

        let (new_text, new_idx) = newline_with_indent(&self.tabs[active].text, cursor);
        // No indent to carry → let egui insert the plain newline (cheaper, and
        // keeps egui's own undo grouping for the common case).
        if new_idx == cursor + 1 {
            return false;
        }
        self.tabs[active].set_text(new_text);
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::one(
                egui::text::CCursor::new(new_idx),
            )));
        state.store(ctx, id);
        true
    }

    /// P0-2 helper — compute the `(new_text, new_caret)` for a smart list
    /// continuation, or `None` when the current line is not a list item, or the
    /// caret is not at the end of the line's content (fall back to plain Enter).
    fn smart_list_newline(&self, active: usize, cursor: usize) -> Option<(String, usize)> {
        let text = &self.tabs.get(active)?.text;
        let bcur = char_to_byte(text, cursor);
        let line_start_b = text[..bcur].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let line_end_b = text[bcur..]
            .find('\n')
            .map(|i| bcur + i)
            .unwrap_or(text.len());
        // Only continue when the caret is at the end of the line's content — a
        // mid-line Enter should split normally.
        if bcur != line_end_b {
            return None;
        }
        let line = &text[line_start_b..line_end_b];
        let cont = scribe_core::md_ops::continue_list_marker(line);
        if cont.clear_current_line {
            // Empty item → drop the dangling marker (exit the list). The line
            // becomes blank and the caret lands at its start.
            let mut new_text = String::with_capacity(text.len());
            new_text.push_str(&text[..line_start_b]);
            new_text.push_str(&text[line_end_b..]);
            let new_caret = text[..line_start_b].chars().count();
            return Some((new_text, new_caret));
        }
        let marker = cont.marker_to_insert?;
        let insert = format!("\n{marker}");
        let mut new_text = text.to_string();
        new_text.insert_str(bcur, &insert);
        Some((new_text, cursor + insert.chars().count()))
    }

    /// Move the caret to the bracket paired with the one at/next to the caret
    /// (Ctrl+M). No-op when the caret is not on a bracket pair. Bounded to the
    /// same buffer size as the bracket-match highlight.
    pub(super) fn jump_matching_bracket(
        &mut self,
        ctx: &egui::Context,
        id: egui::Id,
        active: usize,
    ) {
        if self.tabs[active].text.len() > 500_000 {
            return;
        }
        let Some(mut state) = egui::TextEdit::load_state(ctx, id) else {
            return;
        };
        let Some(range) = state.cursor.char_range() else {
            return;
        };
        let caret = range.primary.index;
        let Some((open_ci, close_ci)) =
            matching_bracket_char_indices(&self.tabs[active].text, caret)
        else {
            return;
        };
        // The caret sits on (or just past) one bracket of the pair; jump to the
        // other end. Pick whichever end the caret is NOT adjacent to.
        let target = if caret.abs_diff(open_ci) <= caret.abs_diff(close_ci) {
            close_ci
        } else {
            open_ci
        };
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::one(
                egui::text::CCursor::new(target),
            )));
        state.store(ctx, id);
    }

    /// Insert a UTC ISO-8601 timestamp at the caret, replacing any selection.
    pub(super) fn insert_datetime_at_caret(
        &mut self,
        ctx: &egui::Context,
        id: egui::Id,
        active: usize,
    ) {
        let ts = crate::datetime::now_iso8601_utc();
        let Some(mut state) = egui::TextEdit::load_state(ctx, id) else {
            return;
        };
        let Some(range) = state.cursor.char_range() else {
            return;
        };
        let lo = range.primary.index.min(range.secondary.index);
        let hi = range.primary.index.max(range.secondary.index);
        let text = &self.tabs[active].text;
        let lo_b = char_to_byte(text, lo);
        let hi_b = char_to_byte(text, hi);
        let mut new_text = String::with_capacity(text.len() + ts.len());
        new_text.push_str(&text[..lo_b]);
        new_text.push_str(&ts);
        new_text.push_str(&text[hi_b..]);
        self.tabs[active].set_text(new_text);
        let new_caret = lo + ts.chars().count();
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::one(
                egui::text::CCursor::new(new_caret),
            )));
        state.store(ctx, id);
        self.status = format!("inserted {ts}");
    }

    /// Duplicate the current selection (or the caret's line when there is no
    /// selection), inserting the copy immediately after and moving the caret
    /// onto the copy.
    pub(super) fn duplicate_selection(&mut self, ctx: &egui::Context, id: egui::Id, active: usize) {
        let Some(mut state) = egui::TextEdit::load_state(ctx, id) else {
            return;
        };
        let Some(range) = state.cursor.char_range() else {
            return;
        };
        let (prim, sec) = (range.primary.index, range.secondary.index);
        let text = &self.tabs[active].text;
        let (insert_at_b, copy, new_caret) = if prim != sec {
            // Selection: insert a copy of [lo,hi) right after hi; caret ends
            // after the inserted copy.
            let lo = prim.min(sec);
            let hi = prim.max(sec);
            let lo_b = char_to_byte(text, lo);
            let hi_b = char_to_byte(text, hi);
            (hi_b, text[lo_b..hi_b].to_string(), hi + (hi - lo))
        } else {
            // Collapsed caret: duplicate the whole line below, keeping the
            // caret's column on the new copy.
            let caret_b = char_to_byte(text, prim);
            let start_b = text[..caret_b].rfind('\n').map_or(0, |i| i + 1);
            let end_b = text[caret_b..]
                .find('\n')
                .map_or(text.len(), |i| caret_b + i);
            let line = text[start_b..end_b].to_string();
            // New caret = same column, one line down. Column in chars:
            let col = text[start_b..caret_b].chars().count();
            let dup_line_start_chars = prim + (line.chars().count() - col) + 1;
            (end_b, format!("\n{line}"), dup_line_start_chars + col)
        };
        let mut new_text = String::with_capacity(text.len() + copy.len());
        new_text.push_str(&text[..insert_at_b]);
        new_text.push_str(&copy);
        new_text.push_str(&text[insert_at_b..]);
        self.tabs[active].set_text(new_text);
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::one(
                egui::text::CCursor::new(new_caret),
            )));
        state.store(ctx, id);
    }
    /// F-016 — Toggle the line-comment prefix on every line touched by the
    /// active selection (or the cursor line if no selection). The prefix is
    /// picked from `comment_prefix_for_extension` based on the active doc's
    /// language hint; unknown languages fall back to no-op + status toast.
    ///
    /// Behaviour: if EVERY non-blank touched line already starts with the
    /// prefix, strip one prefix occurrence per line; otherwise prepend the
    /// prefix to every non-blank line.
    pub(super) fn toggle_comment_active(&mut self) {
        if self.active >= self.tabs.len() {
            return;
        }
        let lang = self.tabs[self.active].doc.language_hint();
        let prefix = lang
            .as_deref()
            .and_then(comment_prefix_for_extension)
            .unwrap_or("");
        if prefix.is_empty() {
            self.toast = Some("Commenting isn't available for this file type.".to_string());
            return;
        }
        let text = &self.tabs[self.active].text;
        // Cheap full-buffer rewrite: split, decide direction by ALL-vs-ANY,
        // toggle, rejoin. The user's "selection" surface is the whole
        // buffer until we wire egui's selection range through to the rope
        // helpers (Phase 15 KEYSTONE follow-up F-009).
        let lines: Vec<&str> = text.lines().collect();
        let non_blank = lines.iter().any(|l| !l.trim().is_empty());
        if !non_blank {
            return;
        }
        let all_commented = lines
            .iter()
            .filter(|l| !l.trim().is_empty())
            .all(|l| l.trim_start().starts_with(prefix));
        let pfx_with_space = format!("{prefix} ");
        let new_lines: Vec<String> = lines
            .iter()
            .map(|l| {
                if l.trim().is_empty() {
                    (*l).to_string()
                } else if all_commented {
                    // Strip the prefix (and one trailing space if present).
                    let trimmed = l.trim_start();
                    let leading_ws_len = l.len() - trimmed.len();
                    let after_pfx = trimmed
                        .strip_prefix(&pfx_with_space)
                        .or_else(|| trimmed.strip_prefix(prefix))
                        .unwrap_or(trimmed);
                    format!("{}{}", &l[..leading_ws_len], after_pfx)
                } else {
                    let trimmed = l.trim_start();
                    let leading_ws_len = l.len() - trimmed.len();
                    format!("{}{pfx_with_space}{trimmed}", &l[..leading_ws_len])
                }
            })
            .collect();
        // Preserve a trailing newline if the original buffer had one.
        let trailing_nl = text.ends_with('\n');
        let mut new_text = new_lines.join("\n");
        if trailing_nl {
            new_text.push('\n');
        }
        // MUST go through `set_text`: writing `text` in place leaves the
        // persistent `rope_buf` alive, and on the rope path the next content
        // edit writes that stale rope back over `text`, destroying this edit.
        // `set_text` also bumps `edit_gen` (no manual bump here).
        let i = self.active;
        self.tabs[i].set_text(new_text);
    }

    /// F-017 — Swap the cursor line with the neighbour `dir` rows away (-1 =
    /// up, +1 = down). No-op at the buffer's first/last line. The cursor
    /// "line" is read from `last_cursor_line_col`; if absent, defaults to
    /// line 0 (start of buffer) so the action is still observable on a
    /// fresh buffer.
    pub(super) fn move_cursor_line(&mut self, dir: i32) {
        if self.active >= self.tabs.len() {
            return;
        }
        let ln = self
            .last_cursor_line_col
            .map(|(l, _)| l.saturating_sub(1))
            .unwrap_or(0);
        let text = &self.tabs[self.active].text;
        let trailing_nl = text.ends_with('\n');
        let mut lines: Vec<String> = text.split('\n').map(str::to_string).collect();
        // split('\n') with a trailing newline produces a trailing "" — drop it.
        if trailing_nl && lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        if lines.is_empty() {
            return;
        }
        // Guard `ln` too, not just `target`: the cursor line (from
        // `last_cursor_line_col`, a 1-based line count that can point AT the
        // post-final-newline empty line) can equal `lines.len()` after the
        // trailing-"" pop above, while `target = ln - 1` is still in range — so
        // `lines.swap(ln, target)` would index `ln` out of bounds and abort
        // (`panic = "abort"`). The sibling line-ops (`duplicate_cursor_line`,
        // `join_cursor_line_with_next`) already guard `ln` this way.
        if ln >= lines.len() {
            return;
        }
        let target = (ln as i32) + dir;
        if target < 0 || (target as usize) >= lines.len() {
            return;
        }
        lines.swap(ln, target as usize);
        // Track the cursor to the moved line.
        let new_ln = target as usize + 1;
        let new_col = self.last_cursor_line_col.map(|(_, c)| c).unwrap_or(1);
        self.last_cursor_line_col = Some((new_ln, new_col));
        let mut new_text = lines.join("\n");
        if trailing_nl {
            new_text.push('\n');
        }
        // MUST go through `set_text` — see `toggle_comment_active`.
        let i = self.active;
        self.tabs[i].set_text(new_text);
    }

    /// F-017 — Duplicate the cursor line in-place: the new copy lands on the
    /// row immediately below.
    pub(super) fn duplicate_cursor_line(&mut self) {
        if self.active >= self.tabs.len() {
            return;
        }
        let ln = self
            .last_cursor_line_col
            .map(|(l, _)| l.saturating_sub(1))
            .unwrap_or(0);
        let text = &self.tabs[self.active].text;
        let trailing_nl = text.ends_with('\n');
        let mut lines: Vec<String> = text.split('\n').map(str::to_string).collect();
        if trailing_nl && lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        if ln >= lines.len() {
            return;
        }
        let copy = lines[ln].clone();
        // `ln + 1` (below) reads as the intent. Note for anyone chasing the
        // permanently-surviving `+`→`*` mutant here: it is EQUIVALENT, not a
        // test gap. `copy == lines[ln]`, so inserting it before or after itself
        // both yield `[…, X, X, …]` — identical output for every input. No test
        // can kill it because there is nothing to detect.
        lines.insert(ln + 1, copy);
        let mut new_text = lines.join("\n");
        if trailing_nl {
            new_text.push('\n');
        }
        // MUST go through `set_text` — see `toggle_comment_active`.
        let i = self.active;
        self.tabs[i].set_text(new_text);
    }

    // -------------------------------------------------------------------
    // Note-usability caret operations (P0–P2). Each loads the live
    // `TextEditState`, calls a pure `scribe_core::md_ops` transform, writes the
    // result back, and restores a sensible caret — mirroring the existing
    // caret-command methods above.
    // -------------------------------------------------------------------

    /// True when the active document is a note-shaped file (markdown / plain
    /// text / untitled scratch) — the surface where smart-lists, list-aware
    /// indent, and smart link-paste apply. Code files are excluded so their
    /// indentation/behaviour is unchanged.
    pub(super) fn note_file_active(&self, active: usize) -> bool {
        match self.tabs.get(active).and_then(|t| t.doc.language_hint()) {
            None => true,
            Some(l) => matches!(l.as_str(), "md" | "markdown" | "txt" | "text"),
        }
    }

    /// The active editor's selection substring, or `None` when there is no
    /// selection (collapsed caret) or no live state.
    pub(super) fn active_selection_text(
        &self,
        ctx: &egui::Context,
        id: egui::Id,
        active: usize,
    ) -> Option<String> {
        let state = egui::TextEdit::load_state(ctx, id)?;
        let range = state.cursor.char_range()?;
        let (lo, hi) = (
            range.primary.index.min(range.secondary.index),
            range.primary.index.max(range.secondary.index),
        );
        if lo == hi {
            return None;
        }
        let text = &self.tabs.get(active)?.text;
        let lo_b = char_to_byte(text, lo);
        let hi_b = char_to_byte(text, hi);
        Some(text[lo_b..hi_b].to_string())
    }

    /// The 0-based `(lo_line, hi_line)` line span touched by the current
    /// caret/selection, plus the primary caret char index. `None` on no state.
    fn active_line_span(
        &self,
        ctx: &egui::Context,
        id: egui::Id,
        active: usize,
    ) -> Option<(usize, usize, usize)> {
        let state = egui::TextEdit::load_state(ctx, id)?;
        let range = state.cursor.char_range()?;
        let (lo, hi) = (
            range.primary.index.min(range.secondary.index),
            range.primary.index.max(range.secondary.index),
        );
        let text = &self.tabs.get(active)?.text;
        let line_of = |ci: usize| -> usize {
            let b = char_to_byte(text, ci);
            text[..b].bytes().filter(|&c| c == b'\n').count()
        };
        let lo_line = line_of(lo);
        let mut hi_line = line_of(hi);
        // A selection ending exactly at a line start should not pull in the next
        // line (its char just before `hi` is the newline).
        if hi > lo {
            let hb = char_to_byte(text, hi);
            if text[..hb].ends_with('\n') {
                hi_line = hi_line.saturating_sub(1);
            }
        }
        Some((lo_line, hi_line, range.primary.index))
    }

    /// True when any line the caret/selection touches is a list item (bullet /
    /// ordered / task) AND the active file is note-shaped with smart-lists on.
    pub(super) fn active_selection_on_list(
        &self,
        ctx: &egui::Context,
        id: egui::Id,
        active: usize,
    ) -> bool {
        if !self.config.editor.smart_lists || !self.note_file_active(active) {
            return false;
        }
        let Some((lo, hi, _)) = self.active_line_span(ctx, id, active) else {
            return false;
        };
        let Some(text) = self.tabs.get(active).map(|t| &t.text) else {
            return false;
        };
        let lines: Vec<&str> = text.split('\n').collect();
        (lo..=hi).any(|idx| {
            lines
                .get(idx)
                .is_some_and(|l| scribe_core::md_ops::parse_list_marker(l).is_some())
        })
    }

    /// P0-3 — list-aware indent (`dir > 0`) / outdent (`dir < 0`) of the
    /// touched list lines, with ordered renumber. Returns true when it changed
    /// the buffer (so a Tab handler knows not to fall back to space-indent).
    pub(super) fn indent_list_lines_active(
        &mut self,
        ctx: &egui::Context,
        id: egui::Id,
        active: usize,
        dir: i32,
    ) -> bool {
        let Some((lo, hi, caret)) = self.active_line_span(ctx, id, active) else {
            return false;
        };
        let width = self.config.editor.tab_width;
        let Some(new_text) =
            scribe_core::md_ops::indent_list_lines(&self.tabs[active].text, lo, hi, width, dir)
        else {
            return false;
        };
        let new_len = new_text.chars().count();
        self.tabs[active].set_text(new_text);
        self.store_caret(ctx, id, caret.min(new_len));
        true
    }

    /// P0-1 — toggle / insert the GFM task checkbox on the caret / selection
    /// lines. Surfaces a toast when no list item was touched.
    pub(super) fn toggle_task_checkbox_active(
        &mut self,
        ctx: &egui::Context,
        id: egui::Id,
        active: usize,
    ) {
        let Some((lo, hi, caret)) = self.active_line_span(ctx, id, active) else {
            return;
        };
        match scribe_core::md_ops::toggle_task_on_lines(&self.tabs[active].text, lo, hi) {
            Some(new_text) => {
                let new_len = new_text.chars().count();
                self.tabs[active].set_text(new_text);
                self.tabs[active].doc.mark_dirty();
                self.store_caret(ctx, id, caret.min(new_len));
            }
            None => {
                self.toast = Some("No list item on this line to make a checkbox.".to_string());
            }
        }
    }

    /// P0-4 — wrap-toggle the selection with `marker` (`**`/`*`/`` ` ``/`~~`).
    pub(super) fn wrap_selection_active(
        &mut self,
        ctx: &egui::Context,
        id: egui::Id,
        active: usize,
        marker: &str,
    ) {
        let Some(mut state) = egui::TextEdit::load_state(ctx, id) else {
            return;
        };
        let Some(range) = state.cursor.char_range() else {
            return;
        };
        let (lo, hi) = (
            range.primary.index.min(range.secondary.index),
            range.primary.index.max(range.secondary.index),
        );
        let (new_text, new_lo, new_hi) =
            scribe_core::md_ops::toggle_wrap(&self.tabs[active].text, lo, hi, marker);
        self.tabs[active].set_text(new_text);
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::two(
                egui::text::CCursor::new(new_lo),
                egui::text::CCursor::new(new_hi),
            )));
        state.store(ctx, id);
    }

    /// P1-4 — case-convert the selection: 0 = lower, 1 = upper, 2 = title.
    pub(super) fn case_selection_active(
        &mut self,
        ctx: &egui::Context,
        id: egui::Id,
        active: usize,
        op: u8,
    ) {
        let Some(mut state) = egui::TextEdit::load_state(ctx, id) else {
            return;
        };
        let Some(range) = state.cursor.char_range() else {
            return;
        };
        let (lo, hi) = (
            range.primary.index.min(range.secondary.index),
            range.primary.index.max(range.secondary.index),
        );
        if lo == hi {
            self.toast = Some("Select some text first to change its case.".to_string());
            return;
        }
        let text = &self.tabs[active].text;
        let lo_b = char_to_byte(text, lo);
        let hi_b = char_to_byte(text, hi);
        let sel = &text[lo_b..hi_b];
        let converted = match op {
            1 => scribe_core::text_ops::to_case(sel, true),
            2 => scribe_core::md_ops::to_title_case(sel),
            _ => scribe_core::text_ops::to_case(sel, false),
        };
        let mut new_text = String::with_capacity(text.len());
        new_text.push_str(&text[..lo_b]);
        new_text.push_str(&converted);
        new_text.push_str(&text[hi_b..]);
        let new_hi = lo + converted.chars().count();
        self.tabs[active].set_text(new_text);
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::two(
                egui::text::CCursor::new(lo),
                egui::text::CCursor::new(new_hi),
            )));
        state.store(ctx, id);
    }

    /// P2-1 — format the markdown pipe table under the caret (align columns).
    pub(super) fn format_table_active(&mut self, ctx: &egui::Context, id: egui::Id, active: usize) {
        let Some((caret_line, _, caret)) = self.active_line_span(ctx, id, active) else {
            return;
        };
        let text = self.tabs[active].text.clone();
        let Some((lo, hi)) = scribe_core::md_ops::table_block_bounds(&text, caret_line) else {
            self.toast = Some("Put the caret inside a markdown table first.".to_string());
            return;
        };
        let lines: Vec<&str> = text.split('\n').collect();
        let block = lines[lo..=hi].join("\n");
        let formatted = scribe_core::md_ops::format_markdown_table(&block);
        if formatted == block {
            return;
        }
        let mut new_lines: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
        new_lines.splice(lo..=hi, formatted.split('\n').map(str::to_string));
        let new_text = new_lines.join("\n");
        let new_len = new_text.chars().count();
        self.tabs[active].set_text(new_text);
        self.tabs[active].doc.mark_dirty();
        self.store_caret(ctx, id, caret.min(new_len));
        self.status = "formatted table".to_string();
    }

    /// P3-3 — open a fresh note tab seeded from a built-in template.
    pub(super) fn new_note_from_template(&mut self, kind: NoteTemplate) {
        self.new_tab();
        let active = self.active;
        self.tabs[active].set_text(kind.body().to_string());
        self.status = format!("new note: {}", kind.label());
    }

    /// P2-2 — auto-pair (default-OFF `editor.auto_pair`). When an opening or
    /// closing bracket/quote/backtick is typed, apply the pure
    /// [`scribe_core::md_ops::auto_pair_action`] decision: wrap a selection,
    /// insert the pair (caret between), or type over an existing closing char.
    /// Consumes the originating `Event::Text` so egui does not also insert it.
    pub(super) fn handle_auto_pair(&mut self, ctx: &egui::Context, id: egui::Id, active: usize) {
        if !self.config.editor.auto_pair {
            return;
        }
        // A single-char Text event that is a pair-relevant char.
        let typed: Option<char> = ctx.input(|i| {
            i.events.iter().find_map(|e| {
                let egui::Event::Text(s) = e else { return None };
                let mut it = s.chars();
                let c = it.next()?;
                if it.next().is_some() {
                    return None;
                }
                if scribe_core::md_ops::auto_pair_close(c).is_some() || matches!(c, ')' | ']' | '}')
                {
                    Some(c)
                } else {
                    None
                }
            })
        });
        let Some(c) = typed else {
            return;
        };
        let Some(mut state) = egui::TextEdit::load_state(ctx, id) else {
            return;
        };
        let Some(range) = state.cursor.char_range() else {
            return;
        };
        let (lo, hi) = (
            range.primary.index.min(range.secondary.index),
            range.primary.index.max(range.secondary.index),
        );
        let text = &self.tabs[active].text;
        let char_after = text.chars().nth(hi);
        let action = scribe_core::md_ops::auto_pair_action(c, lo != hi, char_after);
        if matches!(action, scribe_core::md_ops::AutoPairAction::Passthrough) {
            return;
        }
        // Consume the char so egui's TextEdit does not ALSO insert it.
        ctx.input_mut(|i| {
            i.events.retain(|e| {
                !matches!(e, egui::Event::Text(s)
                    if s.chars().count() == 1 && s.starts_with(c))
            });
        });
        let lo_b = char_to_byte(text, lo);
        let hi_b = char_to_byte(text, hi);
        use scribe_core::md_ops::AutoPairAction as A;
        let (new_text, sel_lo, sel_hi) = match action {
            A::Wrap { open, close } => {
                let mut s = String::with_capacity(text.len() + 2);
                s.push_str(&text[..lo_b]);
                s.push(open);
                s.push_str(&text[lo_b..hi_b]);
                s.push(close);
                s.push_str(&text[hi_b..]);
                (s, lo + 1, hi + 1)
            }
            A::InsertPair { open, close } => {
                let mut s = String::with_capacity(text.len() + 2);
                s.push_str(&text[..lo_b]);
                s.push(open);
                s.push(close);
                s.push_str(&text[lo_b..]);
                (s, lo + 1, lo + 1)
            }
            A::TypeOver => {
                // No text change; step the caret over the existing closing char.
                (text.clone(), lo + 1, lo + 1)
            }
            A::Passthrough => return,
        };
        self.tabs[active].set_text(new_text);
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::two(
                egui::text::CCursor::new(sel_lo),
                egui::text::CCursor::new(sel_hi),
            )));
        state.store(ctx, id);
    }

    /// Store a single collapsed caret at `char_idx` in the editor state for `id`.
    fn store_caret(&self, ctx: &egui::Context, id: egui::Id, char_idx: usize) {
        if let Some(mut state) = egui::TextEdit::load_state(ctx, id) {
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::one(
                    egui::text::CCursor::new(char_idx),
                )));
            state.store(ctx, id);
        }
    }

    /// F-017 — Join the cursor line with the next: trims the trailing
    /// whitespace of the cursor line + the leading whitespace of the next,
    /// joins them with a single space (the standard editor convention).
    pub(super) fn join_cursor_line_with_next(&mut self) {
        if self.active >= self.tabs.len() {
            return;
        }
        let ln = self
            .last_cursor_line_col
            .map(|(l, _)| l.saturating_sub(1))
            .unwrap_or(0);
        let text = &self.tabs[self.active].text;
        let trailing_nl = text.ends_with('\n');
        let mut lines: Vec<String> = text.split('\n').map(str::to_string).collect();
        if trailing_nl && lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        if ln + 1 >= lines.len() {
            return;
        }
        let next = lines.remove(ln + 1);
        let cur = lines[ln].trim_end().to_string();
        let nxt = next.trim_start();
        lines[ln] = if cur.is_empty() || nxt.is_empty() {
            format!("{cur}{nxt}")
        } else {
            format!("{cur} {nxt}")
        };
        let mut new_text = lines.join("\n");
        if trailing_nl {
            new_text.push('\n');
        }
        // MUST go through `set_text` — see `toggle_comment_active`.
        let i = self.active;
        self.tabs[i].set_text(new_text);
    }
}

/// Regression tests for the two silent data-loss defects on the rope path.
///
/// Defect 1 — the in-place line/comment commands wrote `tabs[i].text` directly,
/// leaving the persistent `rope_buf` alive. On the rope path `frame_tick`
/// rebuilds the rope only when `rope_buf.is_none()`, so the stale rope survived
/// the command and the next content edit wrote it straight back over `text`,
/// destroying the user's edit with no error.
///
/// Defect 2 — `set_text` cleared `rope_buf` but never `rope_state`, whose undo
/// `History` holds snapshots of the PREVIOUS content. The first Undo after an
/// external edit therefore restored a buffer the user never had.
#[cfg(test)]
mod rope_writeback_tests {
    use super::*;
    use scribe_core::config::Config;

    /// The rope path is selected by `use_rope_editor(experimental, text_len,
    /// auto_threshold)` — a pure size comparison already pinned by
    /// `wave3_perf_tests::use_rope_editor_decision_matrix`. The 16 MiB default
    /// is not load-bearing for these defects; it only decides *when* the rope
    /// path engages. These tests therefore lower the threshold and use a buffer
    /// above it, taking the byte-identical `frame_tick` branch a 16 MiB file
    /// takes — without a multi-second debug-build frame over a 16 MiB rope
    /// (exactly why the sibling 8 MiB scale test is `#[ignore]`d and so never
    /// actually protects anything).
    const ROPE_THRESHOLD_BYTES: usize = 64;

    fn rope_path_config() -> Config {
        let mut cfg = Config::default();
        cfg.editor.first_run_completed = true;
        // Auto-promotion by size — NOT the `experimental_rope_editor` opt-in,
        // so this is the same path a large file takes for a default user.
        cfg.editor.experimental_rope_editor = false;
        cfg.editor.rope_editor_auto_threshold_bytes = ROPE_THRESHOLD_BYTES;
        cfg
    }

    /// Run `n` full UI frames against a fresh headless egui context.
    fn run_frames(app: &mut ScribeApp, n: usize) {
        let ctx = egui::Context::default();
        for _ in 0..n {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(1100.0, 720.0),
                )),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| app.frame_tick(ctx));
        }
    }

    fn text_event(s: &str) -> egui::Event {
        egui::Event::Text(s.to_string())
    }

    fn ctrl_z() -> egui::Event {
        egui::Event::Key {
            key: egui::Key::Z,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers {
                command: true,
                ctrl: true,
                ..Default::default()
            },
        }
    }

    /// A `.rs` file comfortably over `ROPE_THRESHOLD_BYTES` so the rope path
    /// engages, with a language hint so `toggle_comment_active` has a prefix.
    fn open_rope_backed_rs_file(app: &mut ScribeApp) -> (tempfile::TempDir, usize) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rope_backed.rs");
        let body: String = (0..12).map(|i| format!("fn f{i}() {{}}\n")).collect();
        assert!(
            body.len() > ROPE_THRESHOLD_BYTES,
            "fixture must exceed the rope threshold or the test proves nothing \
             (got {} bytes, need > {ROPE_THRESHOLD_BYTES})",
            body.len()
        );
        std::fs::write(&path, &body).unwrap();
        app.open_path(path);
        let idx = app.active;
        (dir, idx)
    }

    /// The invariant every in-place command must hold: after the command and
    /// the frames that follow it, the persistent rope and `text` agree.
    ///
    /// This is the exact precondition of the write-back at `frame_tick`'s
    /// `resp.content_changed` arm (`tab.text = rope.to_string()`). When rope
    /// and `text` agree that write-back is a no-op and the user's edit is
    /// safe; when they diverge it silently overwrites `text` with the stale
    /// rope. Asserting the agreement therefore proves the edit survives the
    /// next keystroke without re-implementing the write-back in the test.
    fn assert_rope_and_text_agree(app: &ScribeApp, idx: usize, label: &str) {
        let rope = app.tabs[idx]
            .rope_buf
            .as_ref()
            .and_then(scribe_core::buffer::Buffer::as_rope)
            .map(std::string::ToString::to_string)
            .unwrap_or_else(|| {
                panic!("{label}: the rope path must have (re)built a persistent rope")
            });
        assert_eq!(
            rope, app.tabs[idx].text,
            "{label}: the persistent rope still holds PRE-command content — the next \
             keystroke's write-back would overwrite `text` with it and destroy the edit"
        );
    }

    /// Drive one in-place command on a rope-backed buffer and prove the edit
    /// both lands in `text` AND survives the following frames.
    fn assert_command_survives_rope_writeback(label: &str, command: impl FnOnce(&mut ScribeApp)) {
        let mut app = ScribeApp::new_test(rope_path_config());
        let (_dir, idx) = open_rope_backed_rs_file(&mut app);

        // Frames build + persist the rope (the state a user has after scrolling
        // around a large file for a moment).
        run_frames(&mut app, 3);
        assert!(
            app.tabs[idx].rope_buf.is_some(),
            "{label}: precondition — the rope path must be active for this fixture"
        );
        let before = app.tabs[idx].text.clone();

        app.last_cursor_line_col = Some((1, 1));
        command(&mut app);

        let after_command = app.tabs[idx].text.clone();
        assert_ne!(
            before, after_command,
            "{label}: precondition — the command must actually change the buffer"
        );

        // The next frames: the rope is rebuilt from the new `text`.
        run_frames(&mut app, 2);
        assert_rope_and_text_agree(&app, idx, label);
        assert_eq!(
            app.tabs[idx].text, after_command,
            "{label}: the user's edit must still be in the buffer after the \
             following frames"
        );
    }

    // ---- Defect 1: one test per command that mutated `text` in place ----

    #[test]
    fn toggle_comment_survives_the_rope_writeback() {
        assert_command_survives_rope_writeback("toggle-comment", |app| {
            app.toggle_comment_active();
        });
    }

    #[test]
    fn duplicate_line_survives_the_rope_writeback() {
        assert_command_survives_rope_writeback("duplicate-line", |app| {
            app.duplicate_cursor_line();
        });
    }

    #[test]
    fn move_line_survives_the_rope_writeback() {
        assert_command_survives_rope_writeback("move-line", |app| {
            app.move_cursor_line(1);
        });
    }

    #[test]
    fn join_line_survives_the_rope_writeback() {
        assert_command_survives_rope_writeback("join-line", |app| {
            app.join_cursor_line_with_next();
        });
    }

    // ---- Defect 2: undo after an external edit on the rope path ----

    /// Types through the REAL `scribe_render::apply_event` path (so the undo
    /// history records a genuine snapshot), performs a command-palette-class
    /// edit, then presses Ctrl+Z. Undo must never resurrect the pre-edit
    /// content over the newer buffer.
    #[test]
    fn undo_after_external_edit_does_not_resurrect_pre_edit_content() {
        let mut app = ScribeApp::new_test(rope_path_config());
        let (_dir, idx) = open_rope_backed_rs_file(&mut app);
        run_frames(&mut app, 3);

        let original = app.tabs[idx].text.clone();

        // 1. The user types — the rope editor records an undo snapshot of
        //    `original` and the frame syncs the rope back into `text`.
        {
            let tab = &mut app.tabs[idx];
            let state = tab
                .rope_state
                .as_mut()
                .expect("the rope path creates its editing state on first frame");
            let rope = tab
                .rope_buf
                .as_mut()
                .and_then(scribe_core::buffer::Buffer::as_rope_mut)
                .expect("the rope path builds a persistent rope");
            scribe_render::apply_event(rope, state, &text_event("Z"));
            tab.text = rope.to_string();
        }
        let typed = app.tabs[idx].text.clone();
        assert_ne!(typed, original, "precondition — typing changed the buffer");

        // 2. A command-palette / find-replace class edit replaces the buffer.
        //    The replacement must ALSO stay above the rope threshold, or
        //    `use_rope_editor` hands the tab back to the egui TextEdit path and
        //    the test stops exercising the rope undo at all.
        let replaced: String = (0..12).map(|i| format!("fn g{i}() {{}}\n")).collect();
        assert!(
            replaced.len() > ROPE_THRESHOLD_BYTES,
            "the replacement must keep the tab on the rope path"
        );
        app.tabs[idx].set_text(replaced.clone());
        run_frames(&mut app, 2);
        assert_eq!(
            app.tabs[idx].text, replaced,
            "precondition — the external edit landed"
        );

        // 3. Undo.
        {
            let tab = &mut app.tabs[idx];
            let state = tab.rope_state.as_mut().expect("rope state present");
            let rope = tab
                .rope_buf
                .as_mut()
                .and_then(scribe_core::buffer::Buffer::as_rope_mut)
                .expect("rope rebuilt after the external edit");
            scribe_render::apply_event(rope, state, &ctrl_z());
            tab.text = rope.to_string();
        }

        let restored = app.tabs[idx].text.clone();
        assert_ne!(
            restored, typed,
            "undo restored content from BEFORE the external edit — the user's \
             current buffer was silently destroyed"
        );
        assert_ne!(
            restored, original,
            "undo restored the pre-typing content — a buffer two edits stale"
        );
        assert_eq!(
            restored, replaced,
            "the history described content that no longer exists, so undo must \
             be a no-op and leave the current buffer intact"
        );

        // The caret must stay addressable in the NEW buffer.
        let cursor = app.tabs[idx]
            .rope_state
            .as_ref()
            .map_or(0, |s| s.edit.cursor);
        assert!(
            cursor <= app.tabs[idx].text.chars().count(),
            "caret {cursor} is past the end of the {}-char buffer",
            app.tabs[idx].text.chars().count()
        );
    }

    /// A caret already inside the buffer is kept (clamped) rather than thrown
    /// back to the origin, so an in-place command does not scroll the user to
    /// the top of a large file.
    #[test]
    fn set_text_clamps_rather_than_discards_the_caret() {
        let mut tab = EditorTab::scratch();
        tab.text = "0123456789".to_string();
        let mut st = scribe_render::RopeEditorState::new();
        st.edit = scribe_core::editing::EditState::at(9);
        tab.rope_state = Some(st);

        // Shorter replacement → the old caret is out of range and must clamp.
        tab.set_text("abc".to_string());
        assert_eq!(
            tab.rope_state.as_ref().map(|s| s.edit.cursor),
            Some(3),
            "an out-of-range caret must clamp to the new end, never point past it"
        );

        // Longer replacement → the caret is in range and is preserved.
        let mut st = scribe_render::RopeEditorState::new();
        st.edit = scribe_core::editing::EditState::at(2);
        tab.rope_state = Some(st);
        tab.set_text("abcdefghij".to_string());
        assert_eq!(
            tab.rope_state.as_ref().map(|s| s.edit.cursor),
            Some(2),
            "an in-range caret is preserved so the view does not jump to the top"
        );
    }

    /// A tab the rope editor has not claimed keeps `rope_state == None` — the
    /// next frame builds it fresh, and creating one here would be pointless
    /// work on every `set_text` for every egui-TextEdit-path tab.
    #[test]
    fn set_text_leaves_an_unclaimed_tab_without_rope_state() {
        let mut tab = EditorTab::scratch();
        tab.text = "before".to_string();
        assert!(tab.rope_state.is_none());
        tab.set_text("after".to_string());
        assert!(
            tab.rope_state.is_none(),
            "set_text must not fabricate editing state for a tab the rope \
             editor never claimed"
        );
    }

    // ---- The cache-invalidation audit ----

    /// Every `EditorTab` field that is DERIVED from `text` must be invalidated
    /// by `set_text`.
    ///
    /// The exhaustive destructuring below is the load-bearing part: adding a
    /// field to `EditorTab` makes this test fail TO COMPILE, forcing whoever
    /// adds it to decide whether `set_text` must invalidate it. Never replace
    /// it with `..` — the missing-field compile error IS the assertion.
    #[test]
    fn set_text_invalidates_every_text_derived_cache() {
        let mut app = ScribeApp::new_test(rope_path_config());
        let (_dir, idx) = open_rope_backed_rs_file(&mut app);
        // Warm every text-derived cache: the rope, the rope editing state (via
        // a real typed edit, so the undo history is non-empty), and the
        // gen-keyed change-bar cache.
        run_frames(&mut app, 3);
        {
            let tab = &mut app.tabs[idx];
            let state = tab.rope_state.as_mut().expect("rope state present");
            let rope = tab
                .rope_buf
                .as_mut()
                .and_then(scribe_core::buffer::Buffer::as_rope_mut)
                .expect("rope present");
            scribe_render::apply_event(rope, state, &text_event("Q"));
            tab.text = rope.to_string();
        }
        app.ensure_change_states(idx);
        assert!(
            app.tabs[idx].change_gen.is_some(),
            "precondition — the change-bar cache is warm"
        );
        assert!(
            app.tabs[idx]
                .rope_state
                .as_ref()
                .is_some_and(|s| s.history.retained_bytes() > 0),
            "precondition — the undo history holds a snapshot of the old content"
        );
        let gen_before = app.tabs[idx].edit_gen;

        app.tabs[idx].set_text("fn replaced() {}\n".to_string());

        let EditorTab {
            doc: _,
            text,
            doc_id: _,
            pinned: _,
            disk_mtime: _,
            disk_text: _,
            rope_state,
            rope_buf,
            bookmarks: _,
            edit_gen,
            external_change: _,
            session_baseline: _,
            saved_baseline: _,
            change_states: _,
            change_gen,
        } = &app.tabs[idx];

        assert_eq!(text, "fn replaced() {}\n", "the new content is in place");
        assert!(
            rope_buf.is_none(),
            "`rope_buf` caches the OLD content — it must be invalidated or the \
             next keystroke writes it back over `text`"
        );
        assert!(
            rope_state
                .as_ref()
                .is_some_and(|s| s.history.retained_bytes() == 0),
            "`rope_state.history` holds snapshots of the OLD content — it must \
             be dropped or undo restores a buffer the user never had"
        );
        assert_ne!(
            *edit_gen, gen_before,
            "`edit_gen` keys the minimap / spellcheck / change-bar caches — it \
             must move or every one of them serves stale derived data"
        );
        assert_ne!(
            *change_gen,
            Some(*edit_gen),
            "the change-bar cache must not claim to be computed for the new \
             generation"
        );
    }
}
