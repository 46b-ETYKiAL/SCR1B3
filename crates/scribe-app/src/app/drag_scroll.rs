//! Drag-select scroll conveniences for the central egui `TextEdit`.
//!
//! egui's `ScrollArea` deliberately ignores the mouse wheel while ANY widget is
//! being dragged (`scroll_area.rs`: the wheel-apply block is gated behind
//! `dragged_id().is_none()`), so during a `TextEdit` drag-selection the viewport
//! never scrolls and egui — which recomputes the selection end from the pointer
//! mapped into galley space every drag frame — can never extend the selection
//! past the visible region. That is the reported "hold the left button, roll the
//! wheel, keep selecting elsewhere — doesn't work" bug.
//!
//! The fix lives entirely in the app layer and exploits egui's own recompute:
//! SCR1B3 only has to MOVE the viewport (via the existing `pending_scroll`
//! plumbing the minimap + go-to-line already use); once the galley shifts under
//! the stationary pointer, egui's TextEdit drag handler extends the selection by
//! itself — no custom galley hit-testing. Two triggers drive the move:
//!   * **P0-1** — the wheel rolled mid-drag (`smooth_scroll_delta` is still
//!     intact because the gated ScrollArea never consumed it).
//!   * **P0-2** — the pointer held near the top/bottom viewport edge, with a
//!     quadratic distance-into-margin acceleration and a self-pumped repaint
//!     (egui is reactive; a stationary edge pointer emits no events).
//!
//! [`ScribeApp::caret_scroll_off_assist`] is the keyboard-navigation companion
//! (**P1-4**): it keeps the caret at least N lines from the viewport edge on an
//! arrow / home / end move (Vim `scrolloff`), never fighting the wheel or an
//! active drag. [`ScribeApp::page_key_assist`] is the third member of the set —
//! it *implements* PageUp/PageDown for the `TextEdit` path (egui ships neither)
//! and owns its own viewport pan, so page keys are deliberately outside the
//! scroll-off assist's trigger set.
//!
//! Note on egui 0.34: this stack exposes `smooth_scroll_delta` (points, smoothed
//! over frames) — there is NO `raw_scroll_delta`. Reusing the smoothed delta is
//! also what the middle-click autoscroll does, so the feel stays consistent.
//! Ctrl+wheel zoom does NOT: egui's `zoom_modifier` is COMMAND, so a wheel event
//! carrying Ctrl is folded into egui's own zoom accumulator and
//! `smooth_scroll_delta` is left at zero. That is why this module can read the
//! delta for a plain drag-scroll and `keyboard_input` must read `zoom_delta()`.
#![allow(clippy::wildcard_imports)]

use super::*;

/// Screen-space band (px) at each viewport edge inside which an active
/// drag-selection auto-pans. ~28px ≈ two editor lines at the default size.
const EDGE_MARGIN: f32 = 28.0;
/// Peak edge-autoscroll velocity (points/sec) at the very edge, before the
/// per-frame `dt` normalisation. Tuned to feel like VS Code's drag autoscroll.
const EDGE_MAX_SPEED: f32 = 1100.0;

/// Keys after which [`ScribeApp::caret_scroll_off_assist`] re-frames the caret.
///
/// PageUp/PageDown are deliberately ABSENT — see the comment on the read site
/// and [`tests::page_keys_are_not_caret_frame_nav_keys`], which pins that
/// absence so a future edit cannot quietly restore the stale-geometry bug.
const CARET_FRAME_NAV_KEYS: [egui::Key; 4] = [
    egui::Key::ArrowUp,
    egui::Key::ArrowDown,
    egui::Key::Home,
    egui::Key::End,
];

impl ScribeApp {
    /// Drive the editor viewport while a LEFT-drag selection is in progress so
    /// egui extends the selection past the visible region (P0-1 wheel + P0-2
    /// edge autoscroll). Call AFTER the editor's `ScrollArea` has shown and
    /// recorded [`Self::scroll_metrics`], passing the ScrollArea's screen-space
    /// `viewport` (`inner_rect`). Sets [`Self::pending_scroll`], which the editor
    /// consumes on the NEXT frame via `vertical_scroll_offset`.
    pub(super) fn drag_scroll_assist(
        &mut self,
        ctx: &egui::Context,
        editor_id: egui::Id,
        viewport: egui::Rect,
    ) {
        // PageUp/PageDown ride this per-frame hook because it is the one place
        // in the `TextEdit` branch that already receives `(ctx, editor_id,
        // viewport)` — the caret state, the widget to write it back to, and the
        // height that defines a "page". It runs BEFORE the drag-autoscroll
        // config gate below: paging is core navigation, not a scroll
        // convenience, so disabling `drag_autoscroll` must not disable it.
        self.page_key_assist(ctx, editor_id, viewport);
        if !self.config.scroll.drag_autoscroll {
            return;
        }
        let (off_y, content_h, view_h) = self.scroll_metrics;
        let max_off = (content_h - view_h).max(0.0);
        if max_off <= 0.0 {
            return; // content fits — nothing to scroll into
        }
        // A drag-selection is in progress when the primary button is held AND the
        // editor owns keyboard focus. `command` is held for Ctrl+wheel font zoom
        // (handled in keyboard_input) — never hijack that as a drag-scroll. `alt`
        // is held for an Alt+drag column (multi-cursor) selection (P3-F): its head
        // is an absolute char offset built via a galley hit-test, so an autoscroll
        // that remapped the pointer under a moving galley would fight the gesture
        // — bail while Alt is down and let the column build own the drag.
        let (primary_down, cmd, alt, wheel_y, ptr, dt) = ctx.input(|i| {
            (
                i.pointer.primary_down(),
                i.modifiers.command,
                i.modifiers.alt,
                i.smooth_scroll_delta.y,
                i.pointer.interact_pos(),
                i.stable_dt.clamp(1.0 / 240.0, 0.1),
            )
        });
        if !primary_down || cmd || alt || !ctx.memory(|m| m.has_focus(editor_id)) {
            return;
        }
        // A genuine drag-selection ALWAYS begins inside the editor viewport. A
        // press that started anywhere else — a toolbar/titlebar button, the
        // status bar, a side panel — is NOT an editor drag, even though the
        // editor still owns keyboard focus for the one frame before the click
        // surrenders it. Without this guard, clicking any top-bar button (the
        // pointer sits far above the viewport top) makes `edge_autoscroll_step`
        // read a full "past the top edge" depth and pan the note upward by a
        // whole autoscroll step — the reported "clicking anything in the top bar
        // scrolls the note up" bug. Gate on the press ORIGIN, not the live
        // pointer, so dragging a real selection PAST the top/bottom edge (pointer
        // beyond the viewport) still auto-pans as intended.
        let press_in_editor = ctx
            .input(|i| i.pointer.press_origin())
            .is_some_and(|origin| viewport.contains(origin));
        if !press_in_editor {
            return;
        }
        let mut delta = 0.0_f32;
        // P0-1: a positive `smooth_scroll_delta.y` means the content should move
        // DOWN (view toward the top), so the scroll OFFSET moves the opposite way.
        if wheel_y != 0.0 {
            delta -= wheel_y;
        }
        // P0-2: quadratic edge autoscroll when the drag pointer nears an edge.
        if let Some(p) = ptr {
            delta += edge_autoscroll_step(p.y, viewport, dt);
        }
        // P3-G: only pan (and self-pump a repaint) when the CLAMPED target
        // actually differs from the current offset — see [`scroll_step_target`].
        if let Some(target) = scroll_step_target(off_y, delta, max_off) {
            self.pending_scroll = Some(target);
            // Reactive repaint pump: a still edge-pointer emits no input events,
            // so without this the pan would stall after a single tick.
            ctx.request_repaint();
        }
    }

    /// PageUp / PageDown for the `TextEdit` path.
    ///
    /// egui implements neither key: its keyboard cursor dispatch handles only
    /// the arrows, Home and End, and `Key::PageUp` / `Key::PageDown` exist in
    /// egui 0.34.3 purely as enum variants. So this moves the caret itself,
    /// through the SAME [`scribe_render::rope_editor::page_nav`] target
    /// function the rope editor path uses, and pans the viewport by the same
    /// number of rows so the caret stays where it was on screen.
    ///
    /// Shift extends the selection; a read-only tab still pages (navigation is
    /// not a mutation).
    fn page_key_assist(&mut self, ctx: &egui::Context, editor_id: egui::Id, viewport: egui::Rect) {
        if !ctx.memory(|m| m.has_focus(editor_id)) {
            return;
        }
        let (up, down, shift) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::PageUp),
                i.key_pressed(egui::Key::PageDown),
                i.modifiers.shift,
            )
        });
        // Both held in one frame is ambiguous — ignore rather than guess.
        if up == down {
            return;
        }
        let dir = if down { 1_isize } else { -1 };

        let line_px = (self.config.fonts.clamped_editor_size()
            * self.config.fonts.clamped_line_height())
        .max(1.0);
        // One row of overlap so a page keeps a line of context, matching the
        // rope path's own step and every other editor.
        let rows = ((viewport.height() / line_px).floor() as usize).saturating_sub(1);

        let Some(mut state) = egui::TextEdit::load_state(ctx, editor_id) else {
            return;
        };
        let Some(range) = state.cursor.char_range() else {
            return;
        };
        let text = &self.tabs[self.active].text;
        let moved =
            scribe_render::rope_editor::page_nav::page_ccursor_range(text, range, dir, rows, shift);
        if moved == range {
            return; // already clamped at the document end — nothing to do
        }
        state.cursor.set_char_range(Some(moved));
        state.store(ctx, editor_id);

        // Pan by the same distance the caret travelled so the caret keeps its
        // screen row. `scroll_metrics` is this frame's measurement, which is
        // exactly what the user is looking at.
        let (off_y, content_h, view_h) = self.scroll_metrics;
        let max_off = (content_h - view_h).max(0.0);
        if max_off > 0.0 {
            let target = (off_y + dir as f32 * rows.max(1) as f32 * line_px).clamp(0.0, max_off);
            if (target - off_y).abs() > f32::EPSILON {
                self.pending_scroll = Some(target);
            }
        }
        ctx.request_repaint();
    }

    /// Push a queued [`Self::pending_scroll`] into a scroll surface the app does
    /// NOT build itself, BEFORE that surface renders.
    ///
    /// The `TextEdit` path owns its `ScrollArea` and can therefore consume
    /// `pending_scroll` through the `vertical_scroll_offset` builder. The other
    /// editor paths cannot: `scribe-render`'s `RopeEditor` (read-only browse and
    /// the owned rope editor) builds its `ScrollArea` internally and hands back
    /// only a `RopeEditorResponse`, and the fold preview's area is built inside
    /// `show_fold_view`. Without this bridge, find-navigate and go-to-line set
    /// `pending_scroll` and NOTHING ever consumed it on those paths — the match
    /// was selected off-screen and the viewport never moved.
    ///
    /// egui persists a `ScrollArea`'s state under
    /// `ui.make_persistent_id(id_salt)` (`ScrollArea::begin`), which is
    /// [`embedded_scroll_id`]. `State::offset` is a public field, so writing it
    /// before the widget runs is exactly equivalent to the builder call the
    /// `TextEdit` path uses.
    pub(super) fn drive_embedded_scroll(&mut self, ctx: &egui::Context, scroll_id: egui::Id) {
        let Some(off) = self.pending_scroll.take() else {
            return;
        };
        let mut state = egui::scroll_area::State::load(ctx, scroll_id).unwrap_or_default();
        state.offset.y = off.max(0.0);
        state.store(ctx, scroll_id);
    }

    /// Record [`Self::scroll_metrics`] for a widget-owned scroll surface and run
    /// the drag-select autoscroll assist against it.
    ///
    /// This is the ONE place every non-`TextEdit` editor path joins the same
    /// implementation the `TextEdit` path uses, so drag-autoscroll and the
    /// minimap's viewport indicator behave identically on all of them. Before
    /// this existed `scroll_metrics` was written ONLY in the `TextEdit` branch,
    /// so opening a file large enough to swap in the rope editor left the
    /// minimap frozen on the last `TextEdit` frame's numbers.
    ///
    /// `content_h` must be the surface's REAL content height — for a
    /// `RopeEditor` that is exactly `total_lines * line_height`, because the
    /// widget lays out through `ScrollArea::show_rows(ui, line_h, total_lines,
    /// ..)`. Never pass a guess: a wrong content height makes the minimap lie.
    pub(super) fn finish_embedded_scroll(
        &mut self,
        ctx: &egui::Context,
        scroll_id: egui::Id,
        focus_id: egui::Id,
        viewport: egui::Rect,
        content_h: f32,
    ) {
        let off_y = egui::scroll_area::State::load(ctx, scroll_id)
            .map(|s| s.offset.y)
            .unwrap_or(0.0);
        self.scroll_metrics = (off_y, content_h.max(1.0), viewport.height().max(1.0));
        self.drag_scroll_assist(ctx, focus_id, viewport);
    }

    /// Keep the caret at least `scroll.caret_scroll_off` lines from the viewport
    /// top/bottom on a keyboard caret move (P1-4). `caret_bottom_y` is the
    /// caret's screen-space galley baseline; `line_px` is one line's height. Runs
    /// ONLY on a navigation keypress with no button held, so it never fights the
    /// wheel or an active drag-select autoscroll.
    pub(super) fn caret_scroll_off_assist(
        &mut self,
        ctx: &egui::Context,
        caret_bottom_y: f32,
        viewport: egui::Rect,
        line_px: f32,
    ) {
        let off_lines = self.config.scroll.clamped_caret_scroll_off();
        if off_lines == 0 || line_px <= 0.0 {
            return;
        }
        // PageUp/PageDown are deliberately NOT in this set. `caret_bottom_y` is
        // the caret's galley position from THIS frame's layout, which still
        // reflects the caret's PREVIOUS offset — [`Self::page_key_assist`] has
        // only just written the new one, and egui re-lays-out on the next
        // frame. Framing a page move off that stale geometry nudged the
        // viewport by a caret-scroll-off delta computed from where the caret
        // used to be. `page_key_assist` moves the viewport by a whole page
        // itself, so paging owns its own scroll and needs no framing pass.
        let (nav, primary_down) = ctx.input(|i| {
            let pressed = CARET_FRAME_NAV_KEYS.iter().any(|k| i.key_pressed(*k));
            (pressed, i.pointer.primary_down())
        });
        if !nav || primary_down {
            return;
        }
        let (off_y, content_h, view_h) = self.scroll_metrics;
        let max_off = (content_h - view_h).max(0.0);
        if max_off <= 0.0 {
            return;
        }
        // Cap the margin so it can never exceed ~40% of the viewport (a tall
        // scroll-off on a short pane would otherwise oscillate).
        let margin = (off_lines as f32 * line_px).min(view_h * 0.4);
        let nudge = caret_edge_nudge(caret_bottom_y, viewport, margin, line_px);
        if nudge != 0.0 {
            self.pending_scroll = Some((off_y + nudge).clamp(0.0, max_off));
            ctx.request_repaint();
        }
    }
}

/// The `egui::Id` under which a `ScrollArea` built on `ui` persists its
/// [`egui::scroll_area::State`].
///
/// `ScrollArea::begin` computes `ui.make_persistent_id(id_salt)`, where an
/// un-salted area's salt is `Id::new("scroll_area")` and a salted one's is
/// `Id::new(salt)`. `make_persistent_id` is `ui.id().with(salt)`, so the id is
/// reproducible from the same `ui` the widget was handed — the only handle the
/// app has on a scroll surface owned by a child widget.
pub(super) fn embedded_scroll_id(ui: &egui::Ui, salt: &str) -> egui::Id {
    ui.id().with(egui::Id::new(salt))
}

/// Salt of an un-salted `ScrollArea` (what `scribe-render`'s `RopeEditor`
/// builds). Mirrors `ScrollArea::begin`'s `id_salt.unwrap_or_else(|| Id::new("scroll_area"))`.
pub(super) const DEFAULT_SCROLL_SALT: &str = "scroll_area";

/// The focus id `RopeEditor::show_editable` claims for keyboard input
/// (`ui.id().with("scr1b3-rope-editable")`). The drag assist must gate on the id
/// that actually holds focus, not the app's `TextEdit` id.
pub(super) fn rope_editor_focus_id(ui: &egui::Ui) -> egui::Id {
    ui.id().with("scr1b3-rope-editable")
}

/// The clamped scroll OFFSET to move to this frame, or `None` when the viewport
/// cannot actually move (P3-G). Returns `Some(target)` only when `off_y + delta`,
/// clamped to `[0, max_off]`, differs from `off_y`. At the document top/bottom the
/// edge-autoscroll step stays non-zero while the pointer is held in the margin, so
/// gating on `delta != 0.0` alone would re-request a repaint every frame — a
/// continuous ~1-core busy-spin with nothing left to scroll. Clamping FIRST, then
/// comparing, stops the pump exactly at the ends.
fn scroll_step_target(off_y: f32, delta: f32, max_off: f32) -> Option<f32> {
    if delta == 0.0 {
        return None;
    }
    let target = (off_y + delta).clamp(0.0, max_off);
    ((target - off_y).abs() > f32::EPSILON).then_some(target)
}

/// Per-frame vertical autoscroll velocity (points) when the drag pointer sits
/// within [`EDGE_MARGIN`] of the viewport top/bottom, else `0.0`. Positive pans
/// toward the document end (increasing scroll offset). Acceleration is quadratic
/// in the depth into the margin and normalised by `dt` for frame-rate stability.
fn edge_autoscroll_step(py: f32, viewport: egui::Rect, dt: f32) -> f32 {
    let over_bottom = py - (viewport.bottom() - EDGE_MARGIN);
    let over_top = (viewport.top() + EDGE_MARGIN) - py;
    let (dir, depth) = if over_bottom > 0.0 {
        (1.0_f32, over_bottom)
    } else if over_top > 0.0 {
        (-1.0_f32, over_top)
    } else {
        return 0.0;
    };
    let t = (depth / EDGE_MARGIN).clamp(0.0, 1.0);
    dir * t * t * EDGE_MAX_SPEED * dt
}

/// Scroll-offset delta (points) that pulls the caret back inside the keep-away
/// band: negative scrolls the view up (caret near the top), positive scrolls it
/// down (caret near the bottom), `0.0` when the caret is comfortably framed. The
/// top limit adds one `line_px` so the caret's own row is fully clear of the
/// margin, not straddling it.
fn caret_edge_nudge(caret_bottom_y: f32, viewport: egui::Rect, margin: f32, line_px: f32) -> f32 {
    let top_limit = viewport.top() + margin + line_px;
    let bot_limit = viewport.bottom() - margin;
    if caret_bottom_y < top_limit {
        caret_bottom_y - top_limit
    } else if caret_bottom_y > bot_limit {
        caret_bottom_y - bot_limit
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vp() -> egui::Rect {
        egui::Rect::from_min_max(egui::pos2(0.0, 100.0), egui::pos2(400.0, 500.0))
    }

    #[test]
    fn scroll_step_sub_epsilon_move_is_not_a_target() {
        // A move of exactly f32::EPSILON is below the "did the offset really
        // change?" denoise threshold -> None. The `> EPSILON` -> `>= EPSILON`
        // mutant treats an exactly-EPSILON delta as real motion. Kills 179:29.
        assert_eq!(scroll_step_target(0.0, f32::EPSILON, 500.0), None);
    }

    #[test]
    fn caret_nudge_top_limit_includes_the_caret_row_height() {
        // top keep-away limit = top + margin + line_px = 100+40+16 = 156. A caret
        // at y=130 is inside the band -> nudged up by exactly (130-156). The
        // `+ line_px` -> `- line_px` mutant drops the limit to 124. Kills 206:45.
        let n = caret_edge_nudge(130.0, vp(), 40.0, 16.0);
        assert_eq!(n, 130.0 - 156.0);
        assert!(n < 0.0);
    }

    #[test]
    fn caret_nudge_bottom_is_distance_below_the_limit() {
        // bottom nudge = caret_bottom_y - bot_limit exactly. vp bottom=500,
        // margin=40 => bot_limit=460; caret 495 nudges down by 35. The `-`->`+`
        // and `-`->`/` mutants both diverge from 35. Kills 211:24 (x2).
        assert_eq!(caret_edge_nudge(495.0, vp(), 40.0, 16.0), 35.0);
    }

    #[test]
    fn edge_step_zero_in_neutral_band() {
        // A pointer in the middle of the viewport does not autoscroll.
        assert_eq!(edge_autoscroll_step(300.0, vp(), 1.0 / 60.0), 0.0);
    }

    #[test]
    fn edge_step_pans_down_near_bottom_and_up_near_top() {
        let dt = 1.0 / 60.0;
        // Just inside the bottom margin -> positive (toward document end).
        let down = edge_autoscroll_step(495.0, vp(), dt);
        assert!(down > 0.0, "near bottom pans down, got {down}");
        // Just inside the top margin -> negative (toward document start).
        let up = edge_autoscroll_step(105.0, vp(), dt);
        assert!(up < 0.0, "near top pans up, got {up}");
    }

    #[test]
    fn edge_step_accelerates_with_depth_and_caps_beyond_edge() {
        let dt = 1.0 / 60.0;
        let shallow = edge_autoscroll_step(viewport_bottom_at(10.0), vp(), dt);
        let deep = edge_autoscroll_step(viewport_bottom_at(2.0), vp(), dt);
        assert!(deep > shallow, "deeper into the margin pans faster");
        // Past the very edge the velocity is clamped to the peak, not unbounded.
        let past = edge_autoscroll_step(vp().bottom() + 200.0, vp(), dt);
        let peak = EDGE_MAX_SPEED * dt;
        assert!(
            (past - peak).abs() < 1e-3,
            "clamped to peak at/over the edge"
        );
    }

    /// A y that is `inset` px above the viewport bottom (i.e. `inset` into the
    /// margin band when `inset < EDGE_MARGIN`).
    fn viewport_bottom_at(inset: f32) -> f32 {
        vp().bottom() - inset
    }

    #[test]
    fn scroll_step_no_target_when_clamped_at_document_ends() {
        // P3-G: at the very end (off_y == max_off) a toward-end delta yields NO
        // target — nothing to scroll, so no repaint spin.
        assert_eq!(scroll_step_target(500.0, 40.0, 500.0), None);
        // Symmetrically at the start (off_y == 0) a toward-start delta yields None.
        assert_eq!(scroll_step_target(0.0, -40.0, 500.0), None);
        // A zero delta never scrolls.
        assert_eq!(scroll_step_target(200.0, 0.0, 500.0), None);
    }

    #[test]
    fn scroll_step_returns_clamped_target_when_movement_remains() {
        // Mid-document: a real move returns the new offset.
        assert_eq!(scroll_step_target(200.0, 40.0, 500.0), Some(240.0));
        // A move overshooting the end clamps to max_off but STILL counts as motion.
        assert_eq!(scroll_step_target(480.0, 40.0, 500.0), Some(500.0));
        // A move overshooting the start clamps to 0 and still counts as motion.
        assert_eq!(scroll_step_target(20.0, -40.0, 500.0), Some(0.0));
    }

    #[test]
    fn caret_nudge_frames_caret_away_from_edges() {
        let margin = 40.0;
        let line = 16.0;
        // Caret near the bottom -> positive nudge (scroll down).
        let n = caret_edge_nudge(495.0, vp(), margin, line);
        assert!(n > 0.0, "caret near bottom nudges down, got {n}");
        // Caret near the top -> negative nudge (scroll up).
        let n = caret_edge_nudge(105.0, vp(), margin, line);
        assert!(n < 0.0, "caret near top nudges up, got {n}");
        // Caret comfortably centred -> no nudge.
        assert_eq!(caret_edge_nudge(300.0, vp(), margin, line), 0.0);
    }

    // ---------------------------------------------------------------------
    // Exact-boundary probes for the two edge-band predicates.
    //
    // `>` vs `>=` and `<` vs `<=` differ ONLY at the exact boundary value, and
    // on a TALL viewport (`vp()`, 400px) the boundary case degenerates to
    // "depth 0 -> 0.0" either way. A pane SHORTER than two margins makes the
    // two bands overlap, which is where the choice of predicate is really
    // load-bearing — and is a real layout (a 40px editor strip).
    // ---------------------------------------------------------------------

    /// A 40px-tall pane: shorter than `2 * EDGE_MARGIN`, so the top and bottom
    /// autoscroll bands overlap.
    fn short_vp() -> egui::Rect {
        egui::Rect::from_min_max(egui::pos2(0.0, 100.0), egui::pos2(400.0, 140.0))
    }

    #[test]
    fn edge_step_at_the_exact_bottom_boundary_still_pans_up_on_a_short_pane() {
        // y == bottom - EDGE_MARGIN => over_bottom == 0.0 EXACTLY. The real `>`
        // rejects the bottom band and falls through to the top band, which on a
        // 40px pane is still 16px deep -> a real UPWARD pan. The `>= 0.0`
        // mutant takes the bottom branch at depth 0 and returns 0.0, so the
        // drag would stop auto-panning inside a short pane.
        let dt = 1.0 / 60.0;
        let y = short_vp().bottom() - EDGE_MARGIN; // 112.0
        let got = edge_autoscroll_step(y, short_vp(), dt);
        let depth = (short_vp().top() + EDGE_MARGIN) - y; // 16.0
        let want = -(depth / EDGE_MARGIN) * (depth / EDGE_MARGIN) * EDGE_MAX_SPEED * dt;
        assert!(
            got < 0.0,
            "must still pan UP at the overlapped boundary, got {got}"
        );
        assert!((got - want).abs() < 1e-3, "got {got}, want {want}");
    }

    #[test]
    fn edge_step_neutral_band_returns_positive_zero_not_a_direction_branch_zero() {
        // At y == top + EDGE_MARGIN, `over_top` is exactly 0.0: the real `>`
        // rejects BOTH bands and returns the literal `0.0`. The `>= 0.0` mutant
        // ENTERS the top branch with depth 0 and computes
        // `-1.0 * 0.0 * 0.0 * SPEED * dt`, which is `-0.0`. `-0.0 == 0.0`, so
        // the branch actually taken is observable only through the sign bit.
        let got = edge_autoscroll_step(vp().top() + EDGE_MARGIN, vp(), 1.0 / 60.0);
        assert_eq!(got, 0.0);
        assert!(
            got.is_sign_positive(),
            "the neutral band returns the literal +0.0, never a signed zero \
             produced by entering a direction branch at zero depth"
        );
    }

    #[test]
    fn caret_nudge_at_the_exact_top_limit_falls_through_to_the_bottom_rule() {
        // On a short pane the keep-away bands overlap (top_limit > bot_limit).
        // At caret == top_limit the real `<` is FALSE, so the bottom rule
        // applies and pushes the view down by (caret - bot_limit). The `<=`
        // mutant takes the top branch and returns 0.0 — the caret would never
        // be framed out of the bottom margin on a short pane.
        let (margin, line) = (16.0, 16.0);
        let top_limit = short_vp().top() + margin + line; // 132.0
        let bot_limit = short_vp().bottom() - margin; // 124.0
        assert!(
            top_limit > bot_limit,
            "fixture must overlap the bands (top {top_limit}, bot {bot_limit})"
        );
        assert_eq!(
            caret_edge_nudge(top_limit, short_vp(), margin, line),
            top_limit - bot_limit
        );
    }

    // ---------------------------------------------------------------------
    // `page_key_assist` — PageUp / PageDown for the `TextEdit` path.
    // ---------------------------------------------------------------------

    /// A config whose editor line is EXACTLY 40px (20.0 x 2.0), so `line_px`,
    /// the derived row count, and the pan distance are all exact integers and
    /// the `*`->`+` (22px) / `*`->`/` (10px) mutants land on visibly different
    /// row counts. Both values are inside the config clamps ([6, 96], [0.8, 4]).
    fn page_config() -> scribe_core::config::Config {
        let mut cfg = scribe_core::config::Config::default();
        cfg.editor.first_run_completed = true;
        cfg.fonts.editor_size = 20.0;
        cfg.fonts.line_height = 2.0;
        cfg
    }

    /// 400px tall => `rows = floor(400 / 40) - 1 = 9` (one row of overlap).
    fn page_viewport() -> egui::Rect {
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(600.0, 400.0))
    }

    const PAGE_ROWS: f32 = 9.0;
    const PAGE_LINE_PX: f32 = 40.0;

    /// 30 single-word lines, so a 9-row page always has somewhere to go.
    fn page_text() -> String {
        (0..30)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Char index of the first char of `line` in [`page_text`].
    fn line_start(line: usize) -> usize {
        page_text()
            .split('\n')
            .take(line)
            .map(|l| l.chars().count() + 1)
            .sum()
    }

    fn page_app(ctx: &egui::Context, id: egui::Id, caret_line: usize) -> ScribeApp {
        let mut app = ScribeApp::new_test(page_config());
        app.tabs[app.active].text = page_text();
        set_caret(ctx, id, line_start(caret_line));
        app
    }

    /// Store a collapsed caret at char index `at` under `editor_id`.
    fn set_caret(ctx: &egui::Context, editor_id: egui::Id, at: usize) {
        let mut st = egui::TextEdit::load_state(ctx, editor_id).unwrap_or_default();
        st.cursor.set_char_range(Some(egui::text::CCursorRange::one(
            egui::text::CCursor::new(at),
        )));
        st.store(ctx, editor_id);
    }

    /// `(anchor, head)` of the stored caret range.
    fn caret_of(ctx: &egui::Context, editor_id: egui::Id) -> (usize, usize) {
        let st = egui::TextEdit::load_state(ctx, editor_id).expect("TextEditState stored");
        let r = st.cursor.char_range().expect("a char range");
        (r.secondary.index, r.primary.index)
    }

    /// Drive exactly ONE egui pass in which `editor_id` (optionally) holds
    /// focus, `keys` are pressed, and `page_key_assist` runs — the focus
    /// request and the key event must be live in the SAME pass the assist reads.
    fn run_page_keys(
        app: &mut ScribeApp,
        ctx: &egui::Context,
        editor_id: egui::Id,
        keys: &[egui::Key],
        shift: bool,
        focus: bool,
    ) {
        let modifiers = egui::Modifiers {
            shift,
            ..Default::default()
        };
        let input = egui::RawInput {
            modifiers,
            events: keys
                .iter()
                .map(|k| egui::Event::Key {
                    key: *k,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers,
                })
                .collect(),
            ..Default::default()
        };
        let vp = page_viewport();
        let mut passes = 0_u32;
        let _ = ctx.run(input, |ctx| {
            passes += 1;
            if focus {
                ctx.memory_mut(|m| m.request_focus(editor_id));
            }
            app.page_key_assist(ctx, editor_id, vp);
        });
        assert_eq!(passes, 1, "the harness must drive exactly one egui pass");
    }

    #[test]
    fn page_down_moves_the_caret_one_page_and_pans_by_the_same_rows() {
        let ctx = egui::Context::default();
        let id = egui::Id::new("page-editor");
        let mut app = page_app(&ctx, id, 0);
        app.scroll_metrics = (100.0, 4000.0, 400.0); // max_off = 3600
        run_page_keys(&mut app, &ctx, id, &[egui::Key::PageDown], false, true);

        let (anchor, head) = caret_of(&ctx, id);
        assert_eq!(head, line_start(9), "caret pages down exactly 9 rows");
        assert_eq!(anchor, head, "an unshifted page collapses the selection");
        // Pan by the SAME distance the caret travelled, so it keeps its row.
        assert_eq!(
            app.pending_scroll,
            Some(100.0 + PAGE_ROWS * PAGE_LINE_PX),
            "viewport pans by rows x line_px"
        );
    }

    #[test]
    fn shift_page_down_extends_the_selection_from_the_anchor() {
        let ctx = egui::Context::default();
        let id = egui::Id::new("page-editor");
        let mut app = page_app(&ctx, id, 3);
        app.scroll_metrics = (0.0, 4000.0, 400.0);
        run_page_keys(&mut app, &ctx, id, &[egui::Key::PageDown], true, true);

        let (anchor, head) = caret_of(&ctx, id);
        assert_eq!(anchor, line_start(3), "the anchor stays put under Shift");
        assert_eq!(head, line_start(12), "the head pages down 9 rows");
    }

    #[test]
    fn page_up_moves_the_caret_up_a_page_and_pans_back() {
        let ctx = egui::Context::default();
        let id = egui::Id::new("page-editor");
        let mut app = page_app(&ctx, id, 20);
        app.scroll_metrics = (1000.0, 4000.0, 400.0);
        run_page_keys(&mut app, &ctx, id, &[egui::Key::PageUp], false, true);

        assert_eq!(caret_of(&ctx, id).1, line_start(11), "up 9 rows, not down");
        assert_eq!(
            app.pending_scroll,
            Some(1000.0 - PAGE_ROWS * PAGE_LINE_PX),
            "PageUp pans toward the document START (dir == -1)"
        );
    }

    #[test]
    fn both_page_keys_in_one_frame_are_ignored() {
        let ctx = egui::Context::default();
        let id = egui::Id::new("page-editor");
        let mut app = page_app(&ctx, id, 10);
        app.scroll_metrics = (500.0, 4000.0, 400.0);
        run_page_keys(
            &mut app,
            &ctx,
            id,
            &[egui::Key::PageUp, egui::Key::PageDown],
            false,
            true,
        );
        assert_eq!(app.pending_scroll, None, "ambiguous frame pans nothing");
        assert_eq!(
            caret_of(&ctx, id).1,
            line_start(10),
            "ambiguous frame moves no caret"
        );
    }

    #[test]
    fn page_keys_do_nothing_without_editor_focus() {
        let ctx = egui::Context::default();
        let id = egui::Id::new("page-editor");
        let mut app = page_app(&ctx, id, 4);
        app.scroll_metrics = (200.0, 4000.0, 400.0);
        run_page_keys(&mut app, &ctx, id, &[egui::Key::PageDown], false, false);
        assert_eq!(app.pending_scroll, None, "unfocused editor never pages");
        assert_eq!(caret_of(&ctx, id).1, line_start(4), "caret untouched");
    }

    #[test]
    fn a_page_already_at_the_document_end_moves_the_caret_but_queues_no_pan() {
        // max_off == 500 and off_y == 500: the pan target CLAMPS back onto the
        // current offset, so there is nothing to scroll. The caret must still
        // page (navigation is not gated on the viewport having room).
        let ctx = egui::Context::default();
        let id = egui::Id::new("page-editor");
        let mut app = page_app(&ctx, id, 0);
        app.scroll_metrics = (500.0, 900.0, 400.0);
        run_page_keys(&mut app, &ctx, id, &[egui::Key::PageDown], false, true);
        assert_eq!(caret_of(&ctx, id).1, line_start(9), "the caret still pages");
        assert_eq!(
            app.pending_scroll, None,
            "clamped onto the current offset -> no pan queued"
        );
    }

    #[test]
    fn a_page_never_pans_when_the_content_fits_the_viewport() {
        // content_h == view_h -> max_off == 0. A stale non-zero `off_y` must NOT
        // be "corrected" to 0 by the page pan (`> 0.0` -> `>= 0.0` would queue
        // Some(0.0) and yank an already-correct viewport to the top).
        let ctx = egui::Context::default();
        let id = egui::Id::new("page-editor");
        let mut app = page_app(&ctx, id, 0);
        app.scroll_metrics = (50.0, 400.0, 400.0);
        run_page_keys(&mut app, &ctx, id, &[egui::Key::PageDown], false, true);
        assert_eq!(caret_of(&ctx, id).1, line_start(9), "the caret still pages");
        assert_eq!(app.pending_scroll, None, "content fits -> nothing to pan");
    }

    #[test]
    fn a_sub_epsilon_page_pan_is_below_the_denoise_threshold() {
        // max_off is EXACTLY f32::EPSILON (1.0 + EPSILON is representable and
        // (1.0 + EPSILON) - 1.0 == EPSILON), so the clamped target sits one ULP
        // from off_y == 0 — the `> f32::EPSILON` boundary. `>=` would queue a
        // one-ULP pan every page keypress.
        let ctx = egui::Context::default();
        let id = egui::Id::new("page-editor");
        let mut app = page_app(&ctx, id, 0);
        app.scroll_metrics = (0.0, 1.0 + f32::EPSILON, 1.0);
        assert_eq!(
            (1.0f32 + f32::EPSILON) - 1.0,
            f32::EPSILON,
            "fixture: max_off must be exactly one ULP"
        );
        run_page_keys(&mut app, &ctx, id, &[egui::Key::PageDown], false, true);
        assert_eq!(caret_of(&ctx, id).1, line_start(9), "the caret still pages");
        assert_eq!(app.pending_scroll, None, "a one-ULP move is not motion");
    }

    #[test]
    fn page_keys_are_not_caret_frame_nav_keys() {
        // Pins the deliberate ABSENCE of PageUp/PageDown from the scroll-off
        // assist's trigger set: `page_key_assist` owns its own pan, and framing
        // a page move off this frame's STALE galley geometry re-introduces the
        // "page then jitter" bug.
        assert!(!CARET_FRAME_NAV_KEYS.contains(&egui::Key::PageUp));
        assert!(!CARET_FRAME_NAV_KEYS.contains(&egui::Key::PageDown));
        assert!(CARET_FRAME_NAV_KEYS.contains(&egui::Key::ArrowUp));
        assert!(CARET_FRAME_NAV_KEYS.contains(&egui::Key::ArrowDown));
        assert!(CARET_FRAME_NAV_KEYS.contains(&egui::Key::Home));
        assert!(CARET_FRAME_NAV_KEYS.contains(&egui::Key::End));
    }

    // ---------------------------------------------------------------------
    // The embedded-scroll bridge (`drive_embedded_scroll` /
    // `finish_embedded_scroll` / the two id derivations).
    // ---------------------------------------------------------------------

    /// A headless `RawInput` with a real screen rect, so panels get a size.
    fn ui_input() -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(800.0, 600.0),
            )),
            ..Default::default()
        }
    }

    /// Run `f` inside a real `Ui` for exactly one egui pass.
    fn with_ui<R>(ctx: &egui::Context, mut f: impl FnMut(&mut egui::Ui) -> R) -> R {
        let mut out = None;
        let _ = ctx.run(ui_input(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                out = Some(f(ui));
            });
        });
        out.expect("the CentralPanel body ran")
    }

    #[test]
    fn a_queued_scroll_is_pushed_into_a_widget_owned_scroll_area() {
        // The end-to-end contract: `pending_scroll` + `embedded_scroll_id` must
        // land on the SAME state key an un-salted `ScrollArea` persists under,
        // so a find-navigate / go-to-line really moves a `RopeEditor` viewport.
        let ctx = egui::Context::default();
        let mut app = ScribeApp::new_test(page_config());

        let id = with_ui(&ctx, |ui| {
            let id = embedded_scroll_id(ui, DEFAULT_SCROLL_SALT);
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.allocate_space(egui::vec2(10.0, 5000.0));
            });
            id
        });

        app.pending_scroll = Some(321.0);
        app.drive_embedded_scroll(&ctx, id);
        assert!(
            app.pending_scroll.is_none(),
            "the queued offset is CONSUMED, not re-applied every frame"
        );
        let st = egui::scroll_area::State::load(&ctx, id).expect("scroll state written");
        assert_eq!(st.offset.y, 321.0);

        // …and the real widget adopts it on its next pass.
        let seen = with_ui(&ctx, |ui| {
            egui::ScrollArea::vertical()
                .show(ui, |ui| {
                    ui.allocate_space(egui::vec2(10.0, 5000.0));
                })
                .state
                .offset
                .y
        });
        assert!(
            (seen - 321.0).abs() < 1.0,
            "the ScrollArea really moved to the queued offset, got {seen}"
        );
    }

    #[test]
    fn drive_embedded_scroll_clamps_a_negative_queued_offset_to_zero() {
        let ctx = egui::Context::default();
        let mut app = ScribeApp::new_test(page_config());
        let id = egui::Id::new("emb-scroll-neg");
        app.pending_scroll = Some(-50.0);
        app.drive_embedded_scroll(&ctx, id);
        let st = egui::scroll_area::State::load(&ctx, id).expect("scroll state written");
        assert_eq!(st.offset.y, 0.0, "a negative offset is clamped, not stored");
    }

    #[test]
    fn drive_embedded_scroll_is_a_no_op_with_nothing_queued() {
        let ctx = egui::Context::default();
        let mut app = ScribeApp::new_test(page_config());
        let id = egui::Id::new("emb-scroll-empty");
        app.pending_scroll = None;
        app.drive_embedded_scroll(&ctx, id);
        assert!(
            egui::scroll_area::State::load(&ctx, id).is_none(),
            "no queued offset must not fabricate a scroll state"
        );
    }

    #[test]
    fn finish_embedded_scroll_records_the_metrics_the_minimap_reads() {
        let ctx = egui::Context::default();
        let mut app = ScribeApp::new_test(page_config());
        let scroll_id = egui::Id::new("emb-metrics");
        let focus_id = egui::Id::new("emb-focus");
        let mut st = egui::scroll_area::State::default();
        st.offset.y = 137.0;
        st.store(&ctx, scroll_id);

        let viewport = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(300.0, 450.0));
        app.scroll_metrics = (0.0, 1.0, 1.0);
        app.finish_embedded_scroll(&ctx, scroll_id, focus_id, viewport, 2400.0);
        assert_eq!(
            app.scroll_metrics,
            (137.0, 2400.0, 450.0),
            "(live offset, real content height, viewport height)"
        );
    }

    #[test]
    fn finish_embedded_scroll_floors_content_and_view_height_at_one() {
        // A zero content height would make the minimap divide by zero.
        let ctx = egui::Context::default();
        let mut app = ScribeApp::new_test(page_config());
        let scroll_id = egui::Id::new("emb-metrics-zero");
        let empty = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(0.0, 0.0));
        app.finish_embedded_scroll(&ctx, scroll_id, egui::Id::new("f"), empty, 0.0);
        assert_eq!(app.scroll_metrics, (0.0, 1.0, 1.0));
    }

    #[test]
    fn the_two_embedded_ids_are_derived_from_the_ui_not_constants() {
        let ctx = egui::Context::default();
        let (scroll_a, focus_a, other_salt, panel_id) = with_ui(&ctx, |ui| {
            (
                embedded_scroll_id(ui, DEFAULT_SCROLL_SALT),
                rope_editor_focus_id(ui),
                embedded_scroll_id(ui, "some-other-salt"),
                ui.id(),
            )
        });
        // Mirrors `ScrollArea::begin`'s `ui.make_persistent_id(id_salt)` and
        // `RopeEditor::show_editable`'s `ui.id().with("scr1b3-rope-editable")`.
        assert_eq!(scroll_a, panel_id.with(egui::Id::new(DEFAULT_SCROLL_SALT)));
        assert_eq!(focus_a, panel_id.with("scr1b3-rope-editable"));
        // Distinct salts / roles never collide…
        assert_ne!(scroll_a, other_salt);
        assert_ne!(scroll_a, focus_a);
        assert_ne!(focus_a, panel_id);

        // …and both are a function of the Ui: a DIFFERENT ui yields different
        // ids (a constant would collide across every scroll surface, so every
        // embedded editor would fight over one scroll state).
        let (scroll_b, focus_b) = {
            let mut out = None;
            let _ = ctx.run(ui_input(), |ctx| {
                egui::SidePanel::left("other-panel").show(ctx, |ui| {
                    out = Some((
                        embedded_scroll_id(ui, DEFAULT_SCROLL_SALT),
                        rope_editor_focus_id(ui),
                    ));
                });
            });
            out.expect("the SidePanel body ran")
        };
        assert_ne!(scroll_a, scroll_b, "scroll ids are per-Ui, not global");
        assert_ne!(focus_a, focus_b, "focus ids are per-Ui, not global");
    }
}
