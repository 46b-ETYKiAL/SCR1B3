//! Find-navigation, scroll-to-offset, and bookmarks — extracted from `mod.rs` (A-01 wave 2).
#![allow(clippy::wildcard_imports)]

use super::*;

impl ScribeApp {
    /// F-015 — Scroll the active buffer so the given 1-based line is in the
    /// viewport. The minimap renderer already drives `pending_scroll` for
    /// click-jump; we reuse that pipe by computing the approximate Y of
    /// `line` from the current per-line gutter heights (one-frame lag is
    /// fine — same lag the minimap accepts).
    pub(super) fn goto_line(&mut self, line_1based: usize) {
        if self.active >= self.tabs.len() {
            return;
        }
        let line0 = line_1based.saturating_sub(1);
        // Prefer the captured per-line gutter Ys (most accurate; populated
        // each frame when line numbers render). Fall back to a simple
        // line-height * index estimate otherwise.
        if let Some(&y) = self.line_gutter.get(line0) {
            // line_gutter Ys are screen-Y; the editor scroll-pipe wants the
            // vertical offset INSIDE the scroll area. The minimap already
            // assumes scroll-area = full window vertically — keep that.
            self.pending_scroll = Some(y.max(0.0));
        } else {
            let lh =
                self.config.fonts.clamped_editor_size() * self.config.fonts.clamped_line_height();
            self.pending_scroll = Some((line0 as f32) * lh);
        }
        self.status = format!("go to line {line_1based}");
    }

    /// Apply an optional CLI `PATH:LINE[:COLUMN]` jump target (from `scr1b3
    /// file:42:10`) to the first opened tab.
    ///
    /// Scrolls the requested 1-based line into view on the first rendered frame
    /// by reusing the same [`goto_line`](Self::goto_line) scroll pipe the
    /// go-to-line command, bookmark navigation, and find-in-files "open result"
    /// all drive. The column is surfaced in the status hint; the app's jump
    /// convention across every existing surface is line-scroll (the caret is
    /// owned by the egui text widget and settles on the first frame), so line
    /// placement is exactly what "open at line N" means here. A `None` jump (no
    /// target, or a non-`Launch` action) is a no-op — the load-bearing negative
    /// that proves the wire fires only when a jump was parsed.
    pub(super) fn apply_cli_jump(&mut self, jump: Option<(usize, Option<usize>)>) {
        let Some((line, col)) = jump else {
            return;
        };
        // A 1-based line of 0 is not a real position (e.g. `file:0`); ignore it
        // rather than scrolling to a phantom line above the first.
        if line == 0 || self.tabs.is_empty() {
            return;
        }
        self.active = 0;
        self.goto_line(line);
        if let Some(c) = col {
            self.status = format!("go to line {line}:{c}");
        }
    }

    /// The find bar's live `scribe_core::search::Query` — the SINGLE place the
    /// bar's `regex` / `case_sensitive` / `whole_word` toggles become engine
    /// flags. Every find-bar-owned surface (`find_matches_active`, navigation,
    /// both Replace buttons) builds its query here, so the toggles can never
    /// drift apart between "what is highlighted" and "what gets replaced".
    ///
    /// Cutting any field here (e.g. reverting one to a hard-coded `false`) is
    /// caught by `find_toggle_tests` — those drive the flag through the real UI
    /// checkbox and assert the observable match/replace outcome moves.
    pub(super) fn find_query_flags(&self) -> scribe_core::search::Query {
        scribe_core::search::Query {
            pattern: self.find_query.clone(),
            regex: self.find_regex,
            case_sensitive: self.find_case_sensitive,
            whole_word: self.find_whole_word,
        }
    }

    /// The find bar's inline error text (`Some` when the current query is an
    /// invalid regex), as computed by the last [`Self::find_matches_active`].
    pub(super) fn find_error_text(&self) -> Option<String> {
        self.find_error.borrow().clone()
    }

    /// #R6 — all matches of the current find query in the active buffer (empty
    /// when there is no query / no buffer / the regex is invalid).
    ///
    /// An invalid regex yields an EMPTY match list and latches the compile error
    /// into [`Self::find_error`] for the bar's inline error line. It must never
    /// panic, and it must never silently fall back to a substring search — a
    /// half-typed `(foo` reporting literal hits for `(foo` would be a lie about
    /// what the user asked for.
    ///
    /// P-01 / 4-02 R2 — memoized in `find_cache`, keyed by
    /// `(query, mode flags, active tab edit_gen, doc_id)`. This function is
    /// called every frame the find bar is open (counter, highlight-all overlay,
    /// navigation); the cache makes the full-document rescan + regex recompile
    /// happen ONLY when the query, the MODE (regex / match-case / whole-word),
    /// the buffer (`edit_gen`), or the active tab (`doc_id`) actually changed.
    /// The mode is in the key because the same pattern denotes a different match
    /// set under a different mode — omitting it would make a toggle look dead. On an idle frame the cached matches are cloned out and
    /// `find_all` is never re-invoked. Mirrors the `spell_cache` /
    /// `ensure_change_states` generation-keyed idiom.
    pub(super) fn find_matches_active(&self) -> Vec<scribe_core::search::Match> {
        if self.find_query.is_empty() || self.active >= self.tabs.len() {
            *self.find_error.borrow_mut() = None;
            return Vec::new();
        }
        let tab = &self.tabs[self.active];
        let key = crate::find_cache::FindCacheKey::new(
            &self.find_query,
            (
                self.find_regex,
                self.find_case_sensitive,
                self.find_whole_word,
            ),
            tab.text.edit_gen(),
            tab.doc_id.raw(),
        );
        // Cache HIT: query, edit generation, and active document all unchanged
        // since the cached entry — reuse the matches, no rescan, no recompile.
        if let Some(entry) = self.find_cache.borrow().as_ref() {
            if !crate::find_cache::should_recompute(Some(&entry.key), &key) {
                return entry.matches.clone();
            }
        }
        // Cache MISS: recompute once and store under the new key.
        self.find_recompute_count
            .set(self.find_recompute_count.get().wrapping_add(1));
        let q = self.find_query_flags();
        // An invalid regex is a first-class, USER-VISIBLE outcome: report it on
        // the bar (empty match set + inline error). `unwrap_or_default` alone
        // would swallow the compile error and show a bare "no matches", which
        // reads as "your pattern is fine, the text has none".
        let matches = match scribe_core::search::find_all(&tab.text, &q) {
            Ok(m) => {
                *self.find_error.borrow_mut() = None;
                m
            }
            Err(e) => {
                *self.find_error.borrow_mut() = Some(format!("bad regex: {e}"));
                Vec::new()
            }
        };
        *self.find_cache.borrow_mut() = Some(crate::find_cache::FindCacheEntry {
            key,
            matches: matches.clone(),
        });
        matches
    }

    /// Scroll the editor so the byte offset `start` is in view, reusing the
    /// gutter-Y scroll pipe `goto_line` uses (without its status message).
    fn scroll_to_offset(&mut self, start: usize) {
        if self.active >= self.tabs.len() {
            return;
        }
        let line0 = {
            let text = &self.tabs[self.active].text;
            let clamped = start.min(text.len());
            text.as_bytes()[..clamped]
                .iter()
                .filter(|&&b| b == b'\n')
                .count()
        };
        if let Some(&y) = self.line_gutter.get(line0) {
            self.pending_scroll = Some(y.max(0.0));
        } else {
            let lh =
                self.config.fonts.clamped_editor_size() * self.config.fonts.clamped_line_height();
            self.pending_scroll = Some((line0 as f32) * lh);
        }
    }

    /// Move to the next (`forward`) or previous find match, wrapping around, and
    /// scroll it into view. No-op when there are no matches.
    pub(super) fn find_navigate(&mut self, forward: bool) {
        let matches = self.find_matches_active();
        if matches.is_empty() {
            return;
        }
        let n = matches.len();
        // Clamp first (the buffer or query may have changed since last frame).
        self.find_match_idx = self.find_match_idx.min(n - 1);
        self.find_match_idx = if forward {
            (self.find_match_idx + 1) % n
        } else {
            (self.find_match_idx + n - 1) % n
        };
        let start = matches[self.find_match_idx].start;
        self.scroll_to_offset(start);
        self.status = format!("match {} of {}", self.find_match_idx + 1, n);
    }

    /// 0-based cursor line of the active tab (from `last_cursor_line_col`,
    /// which is 1-based; defaults to line 0 when no caret has been seen yet).
    fn cursor_line0(&self) -> usize {
        self.last_cursor_line_col
            .map(|(l, _)| l.saturating_sub(1))
            .unwrap_or(0)
    }

    /// Toggle a bookmark on the active tab's cursor line.
    pub(super) fn toggle_bookmark(&mut self) {
        if self.active >= self.tabs.len() {
            return;
        }
        let line0 = self.cursor_line0();
        let bm = &mut self.tabs[self.active].bookmarks;
        if bm.remove(&line0) {
            self.status = format!("bookmark removed: line {}", line0 + 1);
        } else {
            bm.insert(line0);
            self.status = format!("bookmark added: line {}", line0 + 1);
        }
    }

    /// Jump to the next (`dir = 1`) or previous (`dir = -1`) bookmark on the
    /// active tab, wrapping around the buffer. No-op (with a status hint) when
    /// the tab has no bookmarks.
    pub(super) fn navigate_bookmark(&mut self, dir: i32) {
        if self.active >= self.tabs.len() {
            return;
        }
        let from = self.cursor_line0();
        let target = pick_bookmark(&self.tabs[self.active].bookmarks, from, dir);
        match target {
            Some(line0) => self.goto_line(line0 + 1),
            None => self.status = "no bookmarks in this buffer".to_string(),
        }
    }
}
