//! The per-frame render loop for `ScribeApp` — `frame_tick` (the eframe
//! `update` body), plus its two private helpers `apply_scroll_settings`
//! (Wave-2 scroll knobs + middle-click autoscroll) and
//! `invalidate_galley_caches`. Extracted from `mod.rs` (A-01 wave 3 —
//! behavior-preserving move of the whole `impl ScribeApp` block; only
//! `invalidate_galley_caches` is widened to `pub(super)` for the `e2e`
//! sibling test that calls it. `frame_tick` keeps its `pub(crate)`
//! visibility; `apply_scroll_settings` stays private (called only here).
#![allow(clippy::wildcard_imports)]

use super::*;

/// The OS reduced-motion preference, with a test-only override seam.
///
/// The real query is a `user32` call that reports whatever the HOST machine's
/// "Show animations in Windows" setting happens to be — so a test could neither
/// force it on nor force it off, and the entire OS-gating wire was therefore
/// unprotected: cutting it left the whole suite green (that is exactly how an
/// earlier deliberate break survived into a commit here). This seam lets a test
/// pin the OS answer and assert the gate really consumes it.
pub(super) fn os_reduced_motion_now() -> bool {
    #[cfg(test)]
    if let Some(forced) = motion_test_hook::get_override() {
        return forced;
    }
    scribe_win32_chrome::os_reduced_motion()
}

/// Test-only override for [`os_reduced_motion_now`]. Thread-local, so parallel
/// tests cannot clobber each other's setting.
#[cfg(test)]
pub(super) mod motion_test_hook {
    use std::cell::Cell;

    thread_local! {
        static FORCED: Cell<Option<bool>> = const { Cell::new(None) };
    }

    /// Force the OS answer for the current thread. `None` restores the real query.
    pub(in crate::app) fn set_override(v: Option<bool>) {
        FORCED.with(|c| c.set(v));
    }

    pub(super) fn get_override() -> Option<bool> {
        FORCED.with(Cell::get)
    }

    /// RAII guard so a panicking test cannot leak its override into the next
    /// test on the same thread.
    pub(in crate::app) struct ForcedOsReducedMotion;

    impl ForcedOsReducedMotion {
        pub(in crate::app) fn on() -> Self {
            set_override(Some(true));
            Self
        }
        pub(in crate::app) fn off() -> Self {
            set_override(Some(false));
            Self
        }
    }

    impl Drop for ForcedOsReducedMotion {
        fn drop(&mut self) {
            set_override(None);
        }
    }
}

/// ctx-data slot the editor right-click context menu stashes its chosen
/// [`crate::app::commands::BuiltinCommand`] into, to be drained + dispatched a
/// frame later where `self` is mutable (the menu closure runs while the
/// highlighter borrows `self`).
/// `pub(super)` so the GRID pane's own right-click menu (`grid_methods`) stashes
/// into the SAME slot the single-pane menu uses — one drain contract, not two.
pub(super) fn editor_ctx_cmd_id() -> egui::Id {
    egui::Id::new("scr1b3_editor_ctx_menu_cmd")
}

/// ctx-data slot the in-editor `[[wiki-link]]` click stashes its TARGET into, to
/// be drained + followed a frame later.
///
/// Same reason as [`editor_ctx_cmd_id`]: the click is detected inside the
/// `ScrollArea` closure while the highlight layouter holds `&self`, so
/// `open_or_create_wikilink` (which opens a tab and can rescan the vault) cannot
/// run there. Deferring by a frame also keeps the `active` tab index the rest of
/// this frame's editor code is indexing with from moving under it.
fn editor_wikilink_follow_id() -> egui::Id {
    egui::Id::new("scr1b3_editor_wikilink_follow")
}

/// ctx-data slot recording the frame on which a TEXT paste event was seen.
///
/// A clipboard carrying BOTH text and a bitmap must paste the TEXT; see
/// [`PASTE_IMAGE_TEXT_GRACE_FRAMES`].
fn last_text_paste_frame_id() -> egui::Id {
    egui::Id::new("scr1b3_last_text_paste_frame")
}

/// How many frames a text paste suppresses the image-paste branch for.
///
/// `egui_winit` emits `Event::Paste` on the paste key-DOWN and the `V` key
/// RELEASE one or more frames later, so the two halves of ONE gesture land in
/// different frames. This window joins them back together.
const PASTE_IMAGE_TEXT_GRACE_FRAMES: u64 = 20;

/// Which editor surface actually rendered the active buffer on the last frame.
///
/// SCR1B3 swaps the editor out from under the user in three situations — the
/// automatic rope-editor swap past `rope_editor_auto_threshold_bytes` (16 MiB by
/// default), the read-only browse past the hard size cap, and the folded
/// preview. Each of those disables a large set of `TextEdit`-only features, and
/// none of them used to produce a toast, a badge or a gutter marker: the user
/// was simply dropped into a degraded editor with no explanation for why their
/// features stopped working. The status bar renders this so the active mode is
/// always visible.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum EditorMode {
    /// egui `TextEdit` — the full-feature default. Deliberately badge-less: no
    /// badge IS the "nothing is degraded" signal.
    Standard,
    /// The in-house rope editor (opt-in, or auto past the byte threshold).
    Rope,
    /// The rope editor still on a memory-mapped buffer (read-only banner only).
    RopeMmap,
    /// Read-only huge-file browse past the hard size cap.
    ReadOnlyLarge,
    /// The folded read-only preview.
    Fold,
}

impl EditorMode {
    /// Map the widget's own report of which buffer variant it walked. This is
    /// the first non-test consumer of `RopeEditorResponse::buffer_mode`.
    /// `pub(super)` because the grid pane path renders the SAME widget and must
    /// report the same distinction (`grid_methods`).
    pub(super) fn from_buffer_mode(mode: &scribe_render::BufferModeSeen) -> Self {
        match mode {
            scribe_render::BufferModeSeen::Rope => Self::Rope,
            scribe_render::BufferModeSeen::Mmap => Self::RopeMmap,
        }
    }

    /// Short status-bar badge, or `None` for the full-feature default.
    pub(super) fn badge(self) -> Option<&'static str> {
        match self {
            Self::Standard => None,
            Self::Rope => Some("ROPE"),
            Self::RopeMmap => Some("MMAP"),
            Self::ReadOnlyLarge => Some("READ-ONLY"),
            Self::Fold => Some("FOLDED"),
        }
    }

    /// The explanation shown on hover — names the trade-off, so the badge is
    /// self-describing rather than an unexplained acronym.
    ///
    /// The rope wording used to name FOUR unavailable features (breadcrumbs,
    /// sticky scroll, spellcheck overlay, completion popup). That was a
    /// substantial under-report: the rope branch `return`s before the whole
    /// remainder of the `TextEdit` body, so roughly a dozen MORE conveniences
    /// die with it — every galley-driven overlay, every caret op that needs the
    /// `TextEditState`, multi-cursor, and the app-drawn gutter decorations. A
    /// badge whose explanation lists a quarter of the damage is barely better
    /// than no badge, so the list below is the real one.
    pub(super) fn hover(self) -> &'static str {
        match self {
            Self::Standard => "Standard editor — all editing features available",
            Self::Rope => {
                "Large-file editor: this buffer is past the rope-editor size threshold, so it \
                 renders through the in-house viewport-culled editor. Still working: typing, \
                 undo/redo, find, line numbers, whitespace markers, snippets, and inline LSP \
                 diagnostics (squiggle, gutter bar and hover message). Unavailable \
                 here: breadcrumbs, sticky scroll, spellcheck underlines, the completion \
                 popup, the find highlight-all wash, the right-click menu, multi-cursor \
                 (Ctrl+D / Ctrl+click / Alt+drag column select), the markdown caret chords and \
                 palette caret ops (bold, italic, inline code, strikethrough, task toggle, \
                 format table, change case, insert date/time, duplicate selection, jump to \
                 matching bracket), auto-indent on Enter, auto-pair, list Tab/Shift+Tab, smart \
                 paste (URL to link), clickable URLs, indent guides, column rulers, the \
                 trailing-whitespace tint, the current-line highlight, bracket-match boxes, \
                 selection-occurrence boxes, the Block/Underline caret style, the status-bar \
                 Ln/Col and selection counters, and the gutter's bookmark dots and change bars."
            }
            Self::RopeMmap => {
                "Large-file editor on a memory-mapped buffer — read-only until the file is \
                 loaded into a rope. Everything the ROPE badge lists as unavailable is \
                 unavailable here too, and editing is disabled on top of that. Inline LSP \
                 diagnostics do NOT paint here either: a memory-mapped buffer lays out no \
                 per-row galley, so the overlay has nothing to position against. The \
                 status-bar problem counter is still live."
            }
            Self::ReadOnlyLarge => {
                "Read-only browse: this file is past the hard size cap, so it opens read-only \
                 for O(viewport) navigation. Editing is disabled, and so is every \
                 TextEdit-only convenience the ROPE badge lists (overlays, caret ops, \
                 multi-cursor, the right-click menu, completion and spellcheck). Inline LSP \
                 diagnostics do NOT paint here: the browse path lays out no per-row galley, \
                 so the overlay has nothing to position against."
            }
            Self::Fold => "Folded preview — read-only projection. Exit folds to edit.",
        }
    }

    /// The one-shot toast raised when the editor SWAPS into this surface, or
    /// `None` for a surface the user asked for explicitly.
    ///
    /// The rope swap past `rope_editor_auto_threshold_bytes` is the only
    /// transition the user never requested and cannot predict: they type past
    /// 16 MiB and a dozen features stop responding. The badge alone is a passive,
    /// four-letter signal that has to be HOVERED to explain itself, so a user who
    /// does not already know it exists gets no explanation at all. `Fold` and
    /// `ReadOnlyLarge` are deliberately silent — folding is an explicit user
    /// action, and the read-only browse already prints its own
    /// "[ large file: read-only ]" segment next to the badge.
    pub(super) fn entry_notice(self) -> Option<&'static str> {
        match self {
            // The wording NAMES diagnostics, and that is the point of it. It used
            // to stop at "Editing, undo and find still work", which was a
            // half-truth in the most damaging direction available: inline
            // diagnostics did NOT work on this path, and a banner that lists
            // three surviving features and omits the one that just died reads as
            // "everything important still works". The rope path now paints
            // diagnostics, so saying so is both true and the answer to the
            // question the user actually has.
            Self::Rope => Some(
                "This file crossed the large-file threshold, so SCR1B3 switched to the \
                 viewport-culled editor to stay responsive. Editing, undo, find and \
                 inline diagnostics still work — hover the mode badge in the status bar \
                 for what is unavailable.",
            ),
            // Split off from `Rope` because the claim differs: a memory-mapped
            // buffer lays out no per-row galley, so the diagnostic overlay has
            // nothing to position against and paints nothing. Telling an MMAP
            // user "inline diagnostics still work" would re-create the exact
            // false claim above, one surface over.
            Self::RopeMmap => Some(
                "This file crossed the large-file threshold, so SCR1B3 switched to the \
                 viewport-culled editor to stay responsive. Editing, undo and find still \
                 work; inline diagnostics do NOT paint until the file is loaded into a \
                 rope — hover the mode badge in the status bar for what is unavailable.",
            ),
            Self::Standard | Self::ReadOnlyLarge | Self::Fold => None,
        }
    }
}

/// ctx-data slot the active [`EditorMode`] is published into. The status bar
/// renders BEFORE the central panel, so it reads the previous frame's value —
/// a one-frame lag that is invisible, and always correct once steady.
fn editor_mode_id() -> egui::Id {
    egui::Id::new("scr1b3_active_editor_mode")
}

/// ctx-data slot holding the one-shot notice queued by [`publish_editor_mode`]
/// on a real transition INTO a degraded surface, drained by
/// [`ScribeApp::drain_editor_mode_notice`] where `self` is mutable again.
fn editor_mode_notice_id() -> egui::Id {
    egui::Id::new("scr1b3_editor_mode_notice")
}

/// Publish the surface that just rendered.
///
/// Every editor path MUST call this: the status bar reads the published value
/// and would otherwise keep rendering the PREVIOUS surface's badge forever. The
/// doc comment here used to assert that every path already did — it did not.
/// `render_grid_central_panel` (the split / multi-note grid) published nothing,
/// so switching into grid view left whatever the last single-pane frame had
/// published frozen on screen: open a 20 MiB file, flip on the grid, and the
/// status bar still claimed ROPE while a full-feature `TextEdit` was rendering
/// — the exact "silent and misleading" state the badge exists to prevent. The
/// grid path now publishes too, and `grid_publishes_the_active_panes_mode` /
/// `grid_badge_clears_when_the_active_pane_is_a_small_buffer` pin it so the
/// claim in this comment stays true.
///
/// On a real transition into an unrequested degraded surface it also queues
/// [`EditorMode::entry_notice`], so the swap is announced once instead of only
/// being discoverable by hovering a four-letter badge.
pub(super) fn publish_editor_mode(ctx: &egui::Context, mode: EditorMode) {
    let prev: Option<EditorMode> = ctx.data(|d| d.get_temp(editor_mode_id()));
    if prev != Some(mode) {
        if let Some(notice) = mode.entry_notice() {
            ctx.data_mut(|d| d.insert_temp(editor_mode_notice_id(), notice.to_string()));
        }
    }
    ctx.data_mut(|d| d.insert_temp(editor_mode_id(), mode));
}

/// The surface that rendered last frame, defaulting to the full-feature editor
/// before the first central-panel frame has run.
pub(super) fn active_editor_mode(ctx: &egui::Context) -> EditorMode {
    ctx.data(|d| d.get_temp(editor_mode_id()))
        .unwrap_or(EditorMode::Standard)
}

/// What the user chose in the unsaved-changes close prompt.
///
/// The three answers a close guard must offer: keep the work, knowingly drop it,
/// or stay. `Cancel` is the safe default (Esc maps to it) and is the ONLY one
/// that leaves the window alive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CloseChoice {
    /// Save every unsaved buffer, then close — but only if every save landed.
    Save,
    /// Close and lose the unsaved changes, deliberately.
    Discard,
    /// Abort the close entirely.
    Cancel,
}

impl ScribeApp {
    /// Apply the Wave-2 scroll knobs and drive middle-click autoscroll. Called
    /// at the very top of [`Self::frame_tick`], before any `ScrollArea` shows.
    ///
    /// - **Wheel speed** is `line_scroll_speed` (pre-smoothing; egui's built-in
    ///   "reach 90% in 0.1s" wheel smoothing still applies, so no double-smooth).
    /// - **Jump animation** eases programmatic scrolls (goto-line / find-next).
    /// - **Autoscroll** injects into `smooth_scroll_delta` so the ScrollArea the
    ///   pointer is over consumes it — the same additive contract `ScrollArea`
    ///   uses for the wheel, without threading a handle through every pane.
    fn apply_scroll_settings(&self, ctx: &egui::Context) {
        let scroll = self.config.scroll;
        ctx.options_mut(|o| o.input_options.line_scroll_speed = scroll.clamped_speed());
        // Wave-6 smooth-scroll: when the editor's smooth_scroll is OFF, kill the
        // jump easing so the wheel moves in discrete notches (snappier).
        let smooth = scroll.animate_jumps && self.config.editor.smooth_scroll;
        ctx.all_styles_mut(|s| {
            s.scroll_animation = if smooth {
                egui::style::ScrollAnimation::new(1500.0, egui::Rangef::new(0.05, 0.20))
            } else {
                egui::style::ScrollAnimation::none()
            };
        });
        if !scroll.autoscroll {
            return;
        }
        let id = egui::Id::new("scr1b3_autoscroll");
        let mut st: AutoScrollState = ctx.data(|d| d.get_temp(id).unwrap_or_default());
        // The central editor region (everything left after the titlebar / toolbar
        // / tab / status panels). At the top of a frame `available_rect` still
        // holds last frame's central area, so this excludes a middle-click on a
        // tab / toolbar button (which must keep its own middle-click meaning, e.g.
        // close-tab) from starting an autoscroll drift + repaint loop.
        let editor_area = ctx.available_rect();
        let (mb_pressed, exit_pressed, pos, dt) = ctx.input(|i| {
            (
                i.pointer.button_pressed(egui::PointerButton::Middle),
                i.pointer.button_pressed(egui::PointerButton::Primary)
                    || i.pointer.button_pressed(egui::PointerButton::Secondary),
                i.pointer.latest_pos(),
                i.stable_dt,
            )
        });
        // Enter on a middle press (toggles off if already active); otherwise a
        // left/right press exits. `entered` gates the entering frame so the same
        // press can't both enter and immediately drift.
        let mut entered = false;
        if mb_pressed {
            if st.active {
                st.active = false;
            } else if let Some(p) = pos {
                // Only arm autoscroll for a middle-click inside the editor surface
                // — never on the tabs / toolbar / status chrome.
                if editor_area.contains(p) {
                    st.active = true;
                    st.anchor = p;
                    entered = true;
                }
            }
        } else if st.active && exit_pressed {
            st.active = false;
        }
        if st.active && !entered {
            if let Some(p) = pos {
                let from_anchor = p - st.anchor;
                let dead = scroll.clamped_dead_zone();
                let drifting = from_anchor.length() >= dead;
                if drifting {
                    // smooth_scroll_delta +y moves content down (view toward the
                    // top), so to scroll toward the END when the pointer is BELOW
                    // the anchor (from_anchor.y > 0) the injected delta is negated.
                    let delta = -from_anchor * scroll.clamped_sensitivity() * dt;
                    // ScrollArea consumes `smooth_scroll_delta` (zeroing it when it
                    // takes it), so injecting here scrolls the hovered area.
                    ctx.input_mut(|i| i.smooth_scroll_delta += delta);
                    // Keep integrating the drift even when the pointer is held
                    // stationary-but-offset (no input event would otherwise wake
                    // the reactive loop). Crucially, when the pointer is AT rest in
                    // the dead-zone we do NOT request a repaint — otherwise a plain
                    // middle-click (e.g. that also closed a tab) would spin forever.
                    ctx.request_repaint();
                }
                // Origin glyph on a foreground layer + a directional cursor so the
                // affordance reads like the Windows wheel-click autoscroll. Drawn
                // whenever active (cheap; persists between input events at rest).
                let col = ctx.style().visuals.text_color();
                let painter = ctx.layer_painter(egui::LayerId::new(
                    egui::Order::Foreground,
                    egui::Id::new("scr1b3_autoscroll_glyph"),
                ));
                painter.circle_stroke(st.anchor, 11.0, egui::Stroke::new(1.5, col));
                painter.circle_filled(st.anchor, 1.5, col);
                let icon = if !drifting {
                    egui::CursorIcon::Move
                } else if from_anchor.y.abs() >= from_anchor.x.abs() {
                    if from_anchor.y < 0.0 {
                        egui::CursorIcon::ResizeNorth
                    } else {
                        egui::CursorIcon::ResizeSouth
                    }
                } else if from_anchor.x < 0.0 {
                    egui::CursorIcon::ResizeWest
                } else {
                    egui::CursorIcon::ResizeEast
                };
                ctx.set_cursor_icon(icon);
            }
        }
        ctx.data_mut(|d| d.insert_temp(id, st));
    }

    /// Drop every cached, atlas-baked galley (the note-text highlight galley and
    /// the minimap galley) plus the highlight-job memo. MUST be called right
    /// after `ctx.set_fonts()` rebuilds the font atlas: an `Arc<Galley>` baked
    /// against the OLD atlas keeps stale glyph→texture UVs, so reusing it after
    /// the rebuild paints garbled "broken" text. The layouter cache key
    /// (`make_layouter`) keys on font SIZE but not the family face — and
    /// `FontId::monospace` is identical before/after a face swap — so the cache
    /// cannot self-invalidate on a family change; this explicit drop is the only
    /// signal. (Bug: changing the app UI font silently rebuilt the atlas and the
    /// note text rendered from the stale galley.)
    /// Move a queued editor-mode entry notice (see [`publish_editor_mode`]) onto
    /// the toast line. Called once per frame AFTER the central panel, which is
    /// the first point at which `self` is mutable again — the publish sites all
    /// sit inside closures that hold a borrow of `self.hl` or `self.tabs`.
    pub(super) fn drain_editor_mode_notice(&mut self, ctx: &egui::Context) {
        let queued = ctx.data_mut(|d| {
            let id = editor_mode_notice_id();
            let v = d.get_temp::<String>(id);
            d.remove::<String>(id);
            v
        });
        if let Some(msg) = queued {
            self.toast = Some(msg);
        }
    }

    pub(super) fn invalidate_galley_caches(&self) {
        *self.hl_cache.borrow_mut() = None;
        *self.hl_galley_cache.borrow_mut() = None;
        *self.minimap_cache.borrow_mut() = None;
        *self.minimap_draw_cache.borrow_mut() = None;
    }

    /// One per-frame tick of the editor UI. Separated from `eframe::App::ui` so
    /// `egui_kittest` E2E tests can drive it through `Context::run` without an
    /// `eframe::Frame`. Drives every top-level panel via the deprecated-but-
    /// functional `Panel::show(ctx, …)` path.
    /// Whether animations should actually run this frame — the user's `[motion]`
    /// toggle GATED by the OS reduced-motion preference (WCAG 2.3.3). All animated
    /// paint (caret trail, cursor blink, CRT scanlines) reads THIS rather than
    /// `config.motion.enabled` raw, so Windows' "show animations" accessibility
    /// setting overrides the in-app toggle. The OS query is a cheap cached
    /// user32 read; on non-Windows it is a `false` constant so motion follows the
    /// toggle only.
    /// `pub(super)` so SIBLING modules gate on it too. It was private, and
    /// `theme_visuals::apply_motion_style` — which drives egui's `animation_time`
    /// and the NATIVE caret blink, i.e. the widget-layer half of the whole Motion
    /// feature — therefore still read `config.motion.enabled` raw and ignored the
    /// OS preference entirely. Gating only the overlay painters is not
    /// "end-to-end"; every consumer must resolve through here.
    pub(super) fn motion_active(&self) -> bool {
        self.config
            .motion
            .effective_enabled(os_reduced_motion_now())
    }

    /// Does ANY open tab hold unsaved edits? The close guard's whole predicate.
    pub(super) fn has_unsaved_tabs(&self) -> bool {
        self.tabs.iter().any(EditorTab::is_dirty)
    }

    /// Feed the active buffer to the language server, debounced.
    ///
    /// Called once per frame. Before this existed the client sent `didOpen` and
    /// then nothing: the server's copy of the document was frozen at the moment
    /// the file was opened, so every diagnostic the editor displayed described
    /// text the user had already changed. `note_change` only records; the actual
    /// `textDocument/didChange` goes out from `flush_pending_change` once the
    /// buffer has been quiet for `lsp::sync::DEBOUNCE`.
    ///
    /// Guarded on URI identity: the client tracks ONE document, and a tab switch
    /// must not send the newly-active buffer's text against the previously
    /// opened file's URI. When the active tab is not the opened one we send
    /// nothing and leave the server on its last known-good state (the user can
    /// re-run "Start LSP" to move it) rather than corrupting it.
    pub(super) fn sync_lsp_document(&mut self) {
        let Some(client) = self.lsp.as_mut() else {
            return;
        };
        let Some(open_uri) = client.open_uri().map(str::to_owned) else {
            return;
        };
        let active = self.active.min(self.tabs.len().saturating_sub(1));
        let matching_text = self.tabs.get(active).and_then(|t| {
            let path = t.doc.path()?;
            (path_to_uri(path) == open_uri).then(|| t.text.clone())
        });
        if let Some(text) = matching_text {
            client.note_change(&text, std::time::Instant::now());
        }
        // Flush unconditionally: a pending change must still go out on the frame
        // AFTER the user tabbed away, and a flush with nothing pending is a
        // cheap `Option` check.
        if let Err(e) = client.flush_pending_change(std::time::Instant::now()) {
            // The writer thread is gone (server died). Diagnostics will stop
            // updating; drop the client so a later "Start LSP" can spawn a
            // fresh one instead of talking to a corpse.
            tracing::warn!(
                target: "scribe::lsp",
                error_kind = ?e.kind(),
                "language server stopped accepting changes; dropping the client"
            );
            self.lsp = None;
            self.lsp_lang = None;
        }
    }

    /// The active buffer's diagnostics resolved onto byte spans, ready to paint.
    /// Empty (and free) when there are no diagnostics, which is the common case.
    pub(super) fn diagnostic_spans_for_active(
        &self,
        active: usize,
    ) -> Vec<super::diagnostics_overlay::DiagSpan> {
        if self.diagnostics.is_empty() {
            return Vec::new();
        }
        let Some(tab) = self.tabs.get(active) else {
            return Vec::new();
        };
        super::diagnostics_overlay::diagnostic_spans(&tab.text, &self.diagnostics)
    }

    /// Paint the inline diagnostic overlay over the ROPE editor's painted rows.
    ///
    /// The `TextEdit` path walks `out.galley.rows`; this path has no galley of
    /// its own, so it walks [`RopeEditorResponse::rows`] — the rects and
    /// per-row galleys the widget reports for the rows it actually painted this
    /// frame. Everything else (severity colours, the gutter bar, the hover
    /// tooltip) is deliberately the SAME as the `TextEdit` overlay: a user who
    /// crosses the size threshold should not have to learn a second visual
    /// language for the same information.
    ///
    /// Silent when the widget reports no rows — the read-only browse path and a
    /// still-memory-mapped buffer lay no per-row galley out, so there is nothing
    /// to position against. That gap is stated in the mode badge's hover text
    /// rather than left for the user to discover.
    fn paint_rope_diagnostics(
        &self,
        ui: &egui::Ui,
        resp: &scribe_render::RopeEditorResponse,
        active: usize,
        viewport: egui::Rect,
        accent: Color32,
        muted: Color32,
    ) {
        if self.diagnostics.is_empty() || resp.rows.is_empty() {
            return;
        }
        let Some(tab) = self.tabs.get(active) else {
            return;
        };
        let spans = super::diagnostics_overlay::diagnostic_spans(&tab.text, &self.diagnostics);
        if spans.is_empty() {
            return;
        }
        let err_c = ui_color(&self.theme, "error", Rgba::new(0xe5, 0x3e, 0x3e, 255));
        let warn_c = ui_color(&self.theme, "warning", Rgba::new(0xf2, 0xb3, 0x3d, 255));
        let color_of = |sev: u8| match sev {
            super::diagnostics_overlay::SEVERITY_ERROR => err_c,
            super::diagnostics_overlay::SEVERITY_WARNING => warn_c,
            super::diagnostics_overlay::SEVERITY_INFO => accent,
            _ => muted,
        };
        // Clip to the editor viewport: a row scrolled half out of the top of the
        // scroll area is clipped by the widget, and an overlay that ignored that
        // would paint a squiggle across the toolbar.
        let painter = ui.painter().with_clip_rect(viewport);
        let visible = resp.visible_line_range.clone();

        // Squiggles, one segment per (span × source line) in view.
        let mut painted: Vec<(usize, egui::Rect)> = Vec::new();
        for seg in super::diagnostics_overlay::row_segments(&tab.text, &spans, visible.clone()) {
            let Some(geom) = resp.rows.get(&seg.line) else {
                continue;
            };
            let x0 = geom.col_x(seg.start_col);
            let x1 = geom.col_x(seg.end_col);
            if x1 <= x0 {
                continue;
            }
            paint_squiggle(&painter, x0, x1, geom.bottom, color_of(seg.severity));
            painted.push((
                seg.line,
                egui::Rect::from_min_max(egui::pos2(x0, geom.top), egui::pos2(x1, geom.bottom)),
            ));
        }

        // Gutter bar on each diagnosed line's START line, at the left edge of
        // the rope editor's own gutter — the same shape and lane as the
        // `TextEdit` path's bar in the external gutter panel.
        for (line, sev) in super::diagnostics_overlay::gutter_marks(&self.diagnostics) {
            let line = line as usize;
            if !visible.contains(&line) {
                continue;
            }
            let Some(geom) = resp.rows.get(&line) else {
                continue;
            };
            let h = geom.bottom - geom.top;
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(geom.row_left, h.mul_add(0.2, geom.top)),
                    egui::pos2(geom.row_left + 2.5, h.mul_add(-0.2, geom.bottom)),
                ),
                1.0,
                color_of(sev),
            );
        }

        // Hover: resolved through the hovered ROW's galley, so the message
        // belongs to the character under the pointer rather than to the line.
        let Some(p) = ui.ctx().pointer_hover_pos() else {
            return;
        };
        if !viewport.contains(p) {
            return;
        }
        let Some((line, _)) = painted.iter().find(|(_, r)| r.contains(p)) else {
            return;
        };
        let Some(geom) = resp.rows.get(line) else {
            return;
        };
        let byte =
            super::diagnostics_overlay::byte_of_line_col(&tab.text, *line, geom.col_at_x(p.x));
        if let Some(text) = super::diagnostics_overlay::hover_text(&spans, byte) {
            egui::show_tooltip_at_pointer(
                ui.ctx(),
                ui.layer_id(),
                egui::Id::new("scr1b3-diagnostic-tooltip"),
                |ui| {
                    ui.label(text);
                },
            );
        }
    }

    /// Titles of the tabs holding unsaved edits, for the close prompt. Listing
    /// them is what makes the prompt actionable — "some file is unsaved" leaves
    /// the user unable to judge whether Discard is safe.
    pub(super) fn unsaved_tab_names(&self) -> Vec<String> {
        self.tabs
            .iter()
            .filter(|t| t.is_dirty())
            .map(|t| t.doc.file_name().to_string())
            .collect()
    }

    /// Save every dirty tab through the REAL single-tab save path (so save-time
    /// hygiene, encoding, change-bar baselines and save hooks all behave exactly
    /// as a manual save), restoring the active tab afterwards.
    ///
    /// Deliberately reports nothing: the caller re-asks [`has_unsaved_tabs`]
    /// instead. An untitled buffer routes through Save-As, which the user can
    /// cancel, and a write can fail — in both cases the tab is STILL dirty and
    /// the close must not proceed. Trusting a "saved everything" return value
    /// here is precisely how a cancelled Save-As would silently become a discard.
    pub(super) fn save_all_dirty(&mut self) {
        let prev_active = self.active;
        for i in 0..self.tabs.len() {
            if !self.tabs[i].is_dirty() {
                continue;
            }
            self.active = i;
            self.save_active();
        }
        self.active = prev_active.min(self.tabs.len().saturating_sub(1));
    }

    /// Enter phase 1 of the two-phase close: hide the window now, so phase 2
    /// (next frame, via the `self.closing` branch) can destroy it without the
    /// DWM keeping the last composited frame on screen as a ghost (T19.1).
    pub(super) fn begin_hide_then_close(&mut self, ctx: &egui::Context) {
        self.closing = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        ctx.request_repaint();
    }

    /// Apply a resolved [`CloseChoice`]. Returns `true` when the close has been
    /// started (the window is now hidden and the frame should be abandoned).
    ///
    /// Split out of the rendering so the CONSEQUENCE of each button is
    /// unit-testable without driving pixels — and, in particular, so the
    /// "Save-As cancelled ⇒ still dirty ⇒ do NOT close" path can be pinned.
    pub(super) fn apply_close_choice(&mut self, ctx: &egui::Context, choice: CloseChoice) -> bool {
        match choice {
            CloseChoice::Save => {
                self.save_all_dirty();
                if self.has_unsaved_tabs() {
                    // A Save-As was cancelled or a write failed. The prompt stays
                    // up: proceeding here would turn "Save" into "Discard".
                    self.status =
                        "Still unsaved — the save didn't complete, so SCR1B3 stayed open.".into();
                    false
                } else {
                    self.close_confirm_open = false;
                    self.begin_hide_then_close(ctx);
                    true
                }
            }
            CloseChoice::Discard => {
                // Explicit, informed discard: bypass the guard rather than
                // re-entering it through `want_close` (which would re-raise this
                // same prompt forever).
                self.close_confirm_open = false;
                self.begin_hide_then_close(ctx);
                true
            }
            CloseChoice::Cancel => {
                // A real abort: no hide, no `closing` latch, nothing destroyed.
                self.close_confirm_open = false;
                false
            }
        }
    }

    /// The unsaved-changes close prompt. Returns `true` when the user's choice
    /// started the close (see [`apply_close_choice`](Self::apply_close_choice)).
    ///
    /// A modal — the same `egui::Modal` shape as the update / crash-consent /
    /// report-issue dialogs — because the question must be answered before
    /// anything else happens, and Esc-to-cancel matches those dialogs too.
    pub(super) fn render_close_confirm(&mut self, ctx: &egui::Context) -> bool {
        let names = self.unsaved_tab_names();
        let mut choice: Option<CloseChoice> = None;
        egui::Modal::new(egui::Id::new("scr1b3_close_confirm")).show(ctx, |ui| {
            ui.set_max_width(420.0);
            ui.heading("Unsaved changes");
            ui.add_space(8.0);
            ui.label(if names.len() == 1 {
                "1 file has unsaved changes:".to_string()
            } else {
                format!("{} files have unsaved changes:", names.len())
            });
            ui.add_space(4.0);
            for n in &names {
                // Indented with a spacer rather than padded text so each file's
                // ACCESSIBLE name is exactly the file name (a screen reader — and
                // the kittest a11y query that pins this list — reads the label).
                ui.horizontal(|ui| {
                    ui.add_space(12.0);
                    ui.label(n);
                });
            }
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui
                    .button("Save and close")
                    .on_hover_text("Save every unsaved file, then close SCR1B3.")
                    .clicked()
                {
                    choice = Some(CloseChoice::Save);
                }
                if ui
                    .button("Discard and close")
                    .on_hover_text("Close SCR1B3 and lose these unsaved changes.")
                    .clicked()
                {
                    choice = Some(CloseChoice::Discard);
                }
                if ui
                    .button("Cancel")
                    .on_hover_text("Stay open and keep editing.")
                    .clicked()
                {
                    choice = Some(CloseChoice::Cancel);
                }
            });
        });
        // Esc cancels, mirroring the other dialogs. Cancel is the safe answer, so
        // it is the one the dismissal gesture maps to.
        if choice.is_none() && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            choice = Some(CloseChoice::Cancel);
        }
        match choice {
            Some(c) => self.apply_close_choice(ctx, c),
            None => false,
        }
    }

    pub(crate) fn frame_tick(&mut self, ctx: &egui::Context) {
        // Resolve the effective motion gate ONCE per frame (the OS reduced-motion
        // preference AND the in-app toggle), before any borrow of `self` fields —
        // the animated paint sites below read this Copy `bool` rather than calling
        // the `&self` helper mid-borrow. Querying once also costs one OS read, not
        // one per animated element.
        let motion_on = self.motion_active();
        // Font-switch step 2 (see step 1 at the `ctx.set_fonts` call below):
        // `set_fonts` took effect at the START of this frame, so the NEW atlas is
        // now live. Drop the galley caches that were (re)baked against the OLD
        // atlas on the switch frame, BEFORE any panel renders this frame — the
        // editor then re-bakes against the new atlas and the note paints correctly
        // immediately (no blank/garbled frame, no need to type to refresh).
        if self.font_rebuild_pending {
            self.invalidate_galley_caches();
            self.font_rebuild_pending = false;
        }
        // Wave 2 scroll: apply the wheel-speed + jump-animation knobs and run the
        // middle-click autoscroll state machine BEFORE any ScrollArea shows this
        // frame (egui reads line_scroll_speed while building the wheel delta, and
        // the autoscroll injects into smooth_scroll_delta which the hovered
        // ScrollArea consumes when it renders later this tick).
        self.apply_scroll_settings(ctx);
        // M6 accessibility UI zoom: apply the whole-app zoom factor once per frame.
        // `effective_ui_scale` clamps to 0.5..=3.0 and maps any non-finite stored
        // value to 1.0, so this can never blank the window; at the default 1.0 it
        // is a no-op. Set every frame so a live settings change takes effect at
        // once (egui only repaints when the factor actually changes).
        ctx.set_zoom_factor(self.config.effective_ui_scale());
        // Drain a palette-requested clipboard/history action BEFORE any panel
        // renders, so the injected event reaches the central editor (shown
        // later this frame) and egui's TextEdit performs it natively.
        self.drain_pending_editor_action(ctx);
        // Follow a `[[wiki-link]]` clicked in the EDITOR on a previous frame
        // (stashed by the overlay pass, which runs while `self` is immutably
        // borrowed). Unconditional — a link in a read-only buffer is still
        // followable, and this is the same `open_or_create_wikilink` the notes
        // pane's link list calls, so the vault-traversal gate is identical.
        if let Some(target) = ctx.data_mut(|d| {
            let id = editor_wikilink_follow_id();
            let v = d.get_temp::<String>(id);
            d.remove::<String>(id);
            v
        }) {
            self.open_or_create_wikilink(&target);
        }
        // F-022 — poll the disk mtimes of every open file-backed tab. Cheap
        // when nothing changed (one stat per tab); silent reload when the
        // buffer is clean; status toast when local edits would be clobbered.
        // P-06: throttled to once every N frames (see `should_poll_disk`).
        self.poll_external_disk_changes(ctx.cumulative_pass_nr());
        // Phase 18 T18.2 — keep the grid in step with the editor.grid_enabled
        // config preference (toggled in Settings or via TOML edit + watcher).
        // This is cheap on the common path (config unchanged + ids already
        // assigned) and lets the grid show up the same frame the user flips
        // the checkbox.
        self.sync_grid_state();
        // Follow-OS-theme watcher: when `appearance.follow_os_theme` is on,
        // re-resolve + apply the theme whenever the OS flips light/dark. Cheap
        // — one input read; only re-applies on an actual change.
        {
            let os_theme = ctx.theme();
            if self.config.appearance.follow_os_theme && Some(os_theme) != self.last_os_theme {
                self.reapply_theme(ctx);
            }
        }
        // Once per launch: kick off an automatic update check if opted in.
        self.maybe_remind_update(ctx);
        // Republish the unsaved-work verdict BEFORE the drain, so the apply
        // sites inside `poll` see this frame's answer. `poll` auto-chains
        // `Downloaded(Ok)` / `InstallerReady(Ok)` straight into
        // `apply_and_restart` / `run_installer` with no user present, and both
        // of those do their IRREVERSIBLE work (exe swap + replacement spawn; the
        // elevated `-Wait` installer launch) BEFORE any close is adjudicated —
        // so by the time the unsaved-changes prompt appears, Cancel could no
        // longer mean what it says. The apply itself has to be gated, not just
        // the close. `has_unsaved_tabs` is the same predicate the close guard
        // rules on, so the two can never disagree.
        self.updater.unsaved_work = self.has_unsaved_tabs();
        // Drain the updater worker each frame. A `notify`-mode launch check that
        // found a release raises a prominent top banner (Update / Dismiss) instead
        // of the easily-missed passive toast — see the "update-notice" panel below.
        self.updater.poll(ctx);
        if let Some(v) = self.updater.toast_pending.take() {
            self.update_notice = Some(v);
        }
        // An apply that was HELD for unsaved work must say so — a silent hold is
        // indistinguishable from a broken "Restart now" button. The updater's
        // state is left retriable, so saving and clicking again just works.
        if let Some(msg) = self.updater.unsaved_hold_notice.take() {
            self.toast = Some(msg);
        }
        // `auto`-mode found-an-update yes/no modal.
        self.render_update_prompt(ctx);
        // W1TN3SS opt-in crash-consent modal (ask-each-time). Renders only when a
        // prior session spooled a crash report AND the user opted into
        // AskEachTime; presents an editable preview + equal-weight Send/Don't-send.
        self.render_crash_consent(ctx);
        // W1TN3SS user-initiated "Report an issue" modal. Renders only when the
        // user has opened it from the command palette; previews the exact body,
        // diagnostics OFF by default, and launches the GitHub deep-link / mailto
        // only on an explicit button click.
        self.render_report_issue(ctx);
        // Keep egui's animation time + caret style in sync with the motion
        // preferences every frame (cheap; also covers startup before any
        // theme reapply).
        self.apply_motion_style(ctx);
        // ---- Two-phase close (T19.1 ghost-window fix) ----
        // A transparent / layered window (frameless or translucent) must be
        // HIDDEN one frame before it is destroyed, or the Windows DWM keeps its
        // last composited frame on screen as a ghost after the process exits.
        // Phase 1: on any close request (custom ✕ or OS close) cancel the
        // immediate close, hide the window, repaint. Phase 2 (next frame): the
        // window is hidden, so issue the real Close.
        //
        // The UNSAVED-CHANGES GUARD sits in front of phase 1, not instead of it.
        // The two-phase sequence is unchanged for every close that proceeds; the
        // guard only decides WHETHER a close proceeds. Before it existed, closing
        // with dirty buffers hid and destroyed the window unconditionally and the
        // unsaved text was gone with no prompt (the hot-exit backup is opt-in and
        // throttled, so it is not a substitute for asking).
        if self.closing {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        let os_close = ctx.input(|i| i.viewport().close_requested());
        if os_close || self.want_close {
            self.want_close = false;
            if os_close {
                // Stop eframe acting on the OS close THIS frame; we drive it.
                // Sent on BOTH branches: whether we close or prompt, eframe must
                // not destroy the window out from under us this frame.
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            }
            if self.has_unsaved_tabs() {
                // Do NOT latch `closing` and do NOT hide: raise the prompt and
                // fall through so this frame renders it. Cancel genuinely aborts.
                self.close_confirm_open = true;
                ctx.request_repaint();
            } else {
                self.begin_hide_then_close(ctx);
                return;
            }
        }
        // The prompt renders every frame it is open (not only the frame the close
        // was requested), and returns true once the user's choice has started the
        // close — in which case the window is already hidden and the rest of this
        // frame is skipped exactly as the direct path skips it.
        if self.close_confirm_open && self.render_close_confirm(ctx) {
            return;
        }

        // Rebuild the egui visuals whenever a visuals-affecting setting changes
        // (tint colour / strength, opacity, translucency, background overrides,
        // theme) — not just once at startup — so dragging the tint slider updates
        // the main window live.
        let vsig = self.visuals_signature();
        if !self.visuals_applied || vsig != self.applied_visuals_sig {
            ctx.set_visuals(self.current_visuals());
            self.visuals_applied = true;
            self.applied_visuals_sig = vsig;
        }

        // #24/#40 — the "doubled caption buttons" fix. ROOT CAUSE: winit keeps
        // `WS_SYSMENU | WS_MINIMIZEBOX | WS_MAXIMIZEBOX` on undecorated TOP-LEVEL
        // windows (only the WS_CHILD branch strips caption bits — winit #2754), and
        // Windows 11 DWM paints the three native caption buttons from those residual
        // style bits over our custom titlebar. It is NOT the DWM backdrop (removing
        // window-vibrancy changed nothing) and NOT transparency; the old per-frame
        // `Decorations(false)` re-assert only toggled winit's decorations marker,
        // never the style bits, so it was a no-op. The real fix strips those bits
        // off the HWND — quarantined in the `scribe-win32-chrome` crate (the only
        // `unsafe` besides scribe-core's mmap). Called every frame because it is
        // idempotent + cheap (a single GetWindowLongPtrW read once stripped) and a
        // maximize re-applies winit's styles, which would otherwise re-add them.
        // `!cfg!(test)`: the headless kittest harness has no real OS window.
        if !cfg!(test) && self.config.appearance.frameless {
            scribe_win32_chrome::ensure_caption_stripped();
        }

        // #87/#103 — restart-free font switch: rebuild + re-apply the font set
        // whenever the chosen note OR UI family changes (cheap string compare).
        let font_key = font_state_key(&self.config.fonts);
        if font_key != self.applied_font_family {
            // Font-switch step 1: queue the new font set. `set_fonts` only takes
            // effect at the START of the NEXT frame — THIS frame still renders with
            // the old atlas. So: drop the stale caches now (cheap), then mark a
            // rebuild pending + request a repaint so step 2 (top of `frame_tick`)
            // drops the caches AGAIN next frame once the new atlas is live. Without
            // the next-frame drop, this frame re-bakes a galley against the still-
            // old atlas and the note renders blank/garbled until the next edit.
            ctx.set_fonts(build_fonts(
                &self.config.fonts.editor_family,
                &self.config.fonts.ui_family,
            ));
            self.applied_font_family = font_key;
            self.invalidate_galley_caches();
            self.font_rebuild_pending = true;
            ctx.request_repaint();
        }

        // #104 / #E P1 — apply the editor highlight theme when it changes (also
        // runs once on the first frame to honour the saved config). When
        // `syntax_from_theme` is ON the active CHROME theme's documented
        // `[syntax]` map (incl. the `markup.*` keys) drives editor colours;
        // otherwise the `note_theme` syntect preset does. The applied marker
        // reuses `applied_note_theme`: a NUL-prefixed `\0core:<name>` sentinel
        // (which can never equal a real note-theme name) keys off the chrome
        // theme so a theme switch — or toggling the flag — re-applies. Clearing
        // the highlight cache forces a re-colour next render.
        // Extra markdown token-colouring passes — the master switch gates all;
        // each per-token switch gates its pass.
        let md_opts = if self.config.editor.md_rich_coloring {
            scribe_core::syntax::MdColorOpts {
                dividers: self.config.editor.md_color_dividers,
                tags: self.config.editor.md_color_tags,
                strikethrough: self.config.editor.md_color_strikethrough,
                task_boxes: self.config.editor.md_color_task_boxes,
                table_pipes: self.config.editor.md_color_table_pipes,
            }
        } else {
            scribe_core::syntax::MdColorOpts::none()
        };
        let base_hl_theme = if self.config.editor.syntax_from_theme {
            format!("\u{0}core:{}", self.theme.name)
        } else {
            self.config.editor.note_theme.clone()
        };
        // Fold the markdown-colour flags into the applied marker so toggling one
        // in Settings re-applies + clears the highlight cache (a colour-only
        // change that does not bump `edit_gen`).
        let desired_hl_theme = format!(
            "{base_hl_theme}\u{0}md:{}{}{}{}{}",
            u8::from(md_opts.dividers),
            u8::from(md_opts.tags),
            u8::from(md_opts.strikethrough),
            u8::from(md_opts.task_boxes),
            u8::from(md_opts.table_pipes),
        );
        if desired_hl_theme != self.applied_note_theme {
            if self.config.editor.syntax_from_theme {
                self.hl.set_core_theme(&self.theme);
            } else {
                self.hl.set_theme(&self.config.editor.note_theme);
            }
            self.hl.set_md_colors(md_opts);
            *self.hl_cache.borrow_mut() = None;
            *self.hl_galley_cache.borrow_mut() = None;
            self.applied_note_theme = desired_hl_theme;
        }

        // Live-reload config when the file changes on disk (external edit).
        let mut reload_cfg = false;
        if let Some(rx) = &self.cfg_rx {
            while rx.try_recv().is_ok() {
                reload_cfg = true;
            }
        }
        if reload_cfg {
            self.reload_config_from_disk(ctx);
        }

        // 4-02 — drain any batches the off-thread project-find worker streamed
        // back this frame so the results pane fills in progressively. Cheap
        // (one `try_recv` loop) and a no-op when no search is in flight.
        self.drain_find_in_files();

        // Keep the language server's copy of the buffer in step with the
        // editor's, then drain whatever it published back.
        //
        // Sync FIRST: the server only ever knew the text as it stood at
        // `didOpen`, so without this every diagnostic on screen described a file
        // the user had already edited past. `note_change` is a cheap per-frame
        // record; nothing goes on the wire until the buffer has been quiet for
        // `lsp::sync::DEBOUNCE`, so a keystroke cannot spam the server.
        self.sync_lsp_document();

        // Drain LSP diagnostics published by the server thread.
        let mut new_diags: Option<Vec<Diagnostic>> = None;
        if let Some(client) = &self.lsp {
            while let Ok(d) = client.diagnostics.try_recv() {
                new_diags = Some(d);
            }
        }
        if let Some(d) = new_diags {
            self.diagnostics = d;
        }

        // Collect deferred actions from shortcuts.
        let mut act = Pending::default();
        // #R6 — find-bar F3 navigation direction, recorded here and applied
        // after the input closure so `find_navigate` can re-borrow `self`.
        let mut find_nav: Option<bool> = None;
        self.handle_keyboard_shortcuts(ctx, &mut act, &mut find_nav);
        // #R6 — apply the find-bar F3 navigation collected above (outside the
        // input borrow so `find_navigate` can re-borrow `self`).
        if let Some(forward) = find_nav {
            self.find_navigate(forward);
        }
        // #72 — identifier completion is an EDITOR-surface popup. While any
        // text-input / navigation modal owns the keyboard (find bar, command
        // palette, fuzzy finder, go-to-symbol / go-to-line, recent files,
        // settings, cheatsheet, welcome), completion must NOT open and must NOT
        // intercept ↑↓/Enter — otherwise a Ctrl+Space typed into (say) the find
        // field would spawn a popup that then steals the find bar's navigation
        // keys. Force any open popup closed and leave Ctrl+Space for the modal.
        let modal_owns_keys = self.modal_owns_keyboard();
        if modal_owns_keys {
            self.completion = None;
        }
        // Ctrl/Cmd+Space requests identifier completion at the cursor (only when
        // the editor — not a modal — owns the keyboard; short-circuits so the
        // key is left unconsumed for a focused modal field).
        let want_completion = !modal_owns_keys
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Space));
        // While the completion popup is open, intercept navigation keys BEFORE
        // the TextEdit sees them so arrows/enter drive the list, not the caret.
        let mut accept_completion = false;
        if self.completion.is_some() {
            ctx.input_mut(|i| {
                if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                    if let Some(c) = &mut self.completion {
                        c.selected = (c.selected + 1).min(c.items.len().saturating_sub(1));
                    }
                }
                if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                    if let Some(c) = &mut self.completion {
                        c.selected = c.selected.saturating_sub(1);
                    }
                }
                if i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
                    || i.consume_key(egui::Modifiers::NONE, egui::Key::Tab)
                {
                    accept_completion = true;
                }
                if i.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
                    self.completion = None;
                }
            });
        }
        // Deferred plugin-command invocation (set by palette/menu, applied after UI).
        let mut run_cmd: Option<String> = None;
        // Deferred config persistence (set by View-menu toggles).
        let mut save_cfg = false;
        // Deferred file-tree actions.
        let mut open_from_tree: Option<PathBuf> = None;
        let mut close_tree = false;
        // Deferred LSP start (set by the Language menu).
        let mut start_lsp = false;

        let accent = ui_color(&self.theme, "accent", Rgba::new(0, 255, 254, 255));
        // Secondary brand colour for the split-tone wordmark (`1 B 3` half). A
        // theme MAY set `accent_alt` explicitly; when it doesn't, we derive the
        // fallback from that theme's `keyword` syntax hue — a colour every theme
        // defines and which is chosen to CONTRAST the accent. This makes BOTH
        // halves of the wordmark recolour together on a theme change. (Previously
        // this fell back to a FIXED violet, so only the accent-coloured "S C R "
        // half followed the theme while "1 B 3" stayed violet on every theme —
        // the "only half recolours" report.) Chrome stays one-accent everywhere
        // ELSE; the split wordmark is the single deliberate two-tone mark.
        let accent_alt_default = self
            .theme
            .syntax_color("keyword", Rgba::new(0x9d, 0x7c, 0xff, 255));
        let accent_alt = ui_color(&self.theme, "accent_alt", accent_alt_default);
        let muted = ui_color(&self.theme, "line_number", Rgba::new(0x5a, 0x58, 0x69, 255));
        // Chrome panels (titlebar/toolbar/status/filetree/split/gutter/minimap) all
        // fill with this color. In a translucent window mode the fill MUST carry the
        // reduced alpha — otherwise opaque chrome covers the transparent/blurred
        // surface and "transparency doesn't work" (the T19.2 root cause). The master
        // `transparency_enabled` toggle gates this via `effective_translucent()`.
        let panel = panel_fill(
            &self.theme,
            &self.config.window,
            self.config.appearance.background_override.as_deref(),
        );
        let warn = ui_color(&self.theme, "warning", Rgba::new(0xfb, 0xbf, 0x24, 255));

        // F11 fullscreen (editor-only): derive the OS fullscreen state each frame
        // (no separate field — avoids a re-sync race when the user exits via the
        // OS). `chrome_hidden` hides the toolbar/tabs/status/minimap/gutter; the
        // custom titlebar additionally hides in fullscreen (the OS gives no frame),
        // whereas zen keeps it for window dragging.
        let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        let chrome_hidden = self.zen_mode || fullscreen;
        // Toolbar-in-titlebar mode (only meaningful with the custom titlebar).
        let toolbar_in_titlebar =
            self.config.appearance.toolbar_in_titlebar && self.config.appearance.frameless;

        // ---- Custom frameless titlebar ----
        // Height is CONSTANT regardless of `toolbar_in_titlebar` — it is sized to
        // fit the quick-access toolbar buttons in BOTH states (so toggling the
        // option never resizes the titlebar). It grows only if the user raises the
        // toolbar button-size setting (a separate, expected knob), and never drops
        // below the bare-chrome baseline (34). Previously it was 40 when the
        // toolbar lived here and 34 otherwise, so flipping the toggle jumped it.
        let titlebar_h = (self.config.toolbar.clamped_button_size() + 10.0).max(34.0);
        if self.config.appearance.frameless && !fullscreen {
            egui::TopBottomPanel::top("titlebar")
                .exact_height(titlebar_h)
                .frame(egui::Frame::default().fill(panel))
                .show(ctx, |ui| {
                    let resp = ui.interact(
                        ui.max_rect(),
                        egui::Id::new("titlebar-drag"),
                        egui::Sense::click_and_drag(),
                    );
                    if resp.drag_started() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                    }
                    if resp.double_clicked() {
                        let is_max = ctx.input(|i| i.viewport().maximized).unwrap_or(false);
                        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!is_max));
                    }
                    ui.horizontal_centered(|ui| {
                        // RESERVE the window caption buttons on the RIGHT first. The
                        // wordmark + in-titlebar toolbar then fill the space to their
                        // LEFT, clipped to that boundary — so on a narrow window the
                        // toolbar compresses/clips instead of the min/max/close buttons
                        // being painted over by it (the "caption buttons go over the
                        // toolbar when narrow" report). Previously the left content was
                        // laid out first and the caption buttons took only the leftover
                        // width, so a wide toolbar pushed them under itself / off-edge.
                        // Caption-button height tracks the toolbar button size so
                        // they stay consistent when the user picks a large size,
                        // while preserving the default 28px (`.max(28.0)`).
                        let cap_h = self.config.toolbar.clamped_button_size().max(28.0);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let is_max = ctx.input(|i| i.viewport().maximized).unwrap_or(false);
                            let close_hover = Color32::from_rgb(0xE8, 0x11, 0x23);
                            let soft_hover = Color32::from_rgba_unmultiplied(0xff, 0xff, 0xff, 26);
                            if caption_btn(ui, CaptionIcon::Close, muted, close_hover, cap_h)
                                .clicked()
                            {
                                // Funnel into the two-phase close (hide-before-destroy)
                                // so a transparent window leaves no DWM ghost (T19.1).
                                self.want_close = true;
                            }
                            let max_icon = if is_max {
                                CaptionIcon::Restore
                            } else {
                                CaptionIcon::Maximize
                            };
                            if caption_btn(ui, max_icon, muted, soft_hover, cap_h).clicked() {
                                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!is_max));
                            }
                            if caption_btn(ui, CaptionIcon::Minimize, muted, soft_hover, cap_h)
                                .clicked()
                            {
                                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                            }
                            // Settings "gear", relocated here from the quick-access
                            // toolbar. In this right_to_left layout, painting it AFTER
                            // Minimize places it visually to the LEFT of Minimize — the
                            // rightmost non-window-control button. Opens Settings, the
                            // same effect as the old toolbar gear (the command-palette
                            // "Open Settings" command + its shortcut are unchanged).
                            if caption_btn(ui, CaptionIcon::Settings, muted, soft_hover, cap_h)
                                .on_hover_text("Settings")
                                .clicked()
                            {
                                self.settings_open = true;
                            }
                            // LEFT content: wordmark + (optional) in-titlebar toolbar,
                            // laid out left-to-right in the width remaining to the left
                            // of the caption buttons. The clip rect is pinned to that
                            // region so an overflowing toolbar can never paint over the
                            // reserved caption buttons.
                            ui.with_layout(
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    ui.set_clip_rect(ui.max_rect().intersect(ui.clip_rect()));
                                    ui.add_space(10.0);
                                    // Chrome text follows the APP UI font (Proportional
                                    // family), NOT the note/editor font. Split-tone
                                    // wordmark: "S C R " accent, "1 B 3" secondary;
                                    // painted with zero item-spacing so they read as ONE
                                    // wordmark.
                                    let saved_spacing = ui.spacing().item_spacing.x;
                                    ui.spacing_mut().item_spacing.x = 0.0;
                                    // Guarantee the split-tone wordmark keeps a
                                    // legible luminance gap against the titlebar
                                    // fill on ANY theme — SCR1B3's own teal voice
                                    // is preserved (hue kept), only lifted/pushed
                                    // if a theme's accent would wash out.
                                    let wm_accent = ensure_readable_tone(accent, panel);
                                    let wm_accent_alt = ensure_readable_tone(accent_alt, panel);
                                    ui.label(RichText::new("S C R ").color(wm_accent).strong());
                                    ui.label(RichText::new("1 B 3").color(wm_accent_alt).strong());
                                    ui.spacing_mut().item_spacing.x = saved_spacing;
                                    // Decorative separator + JP subtitle (写本 —
                                    // shahon) drop out FIRST when the titlebar is
                                    // tight, so the core "SCR1B3" wordmark never has
                                    // to clip mid-glyph on a narrow window.
                                    if ui.available_width() > 120.0 {
                                        ui.add_space(6.0);
                                        ui.label(RichText::new("//").color(muted));
                                        ui.label(
                                            RichText::new(scribe_core::PRODUCT_SUBTITLE_JP)
                                                .color(muted)
                                                .small(),
                                        );
                                    }
                                    if toolbar_in_titlebar {
                                        ui.add_space(12.0);
                                        // Button PARITY with the standalone toolbar row:
                                        // same configured height + spacing so the buttons
                                        // are identical whether the toolbar lives here or
                                        // in its own row.
                                        let btn = self.config.toolbar.clamped_button_size();
                                        let gap = self.config.toolbar.clamped_button_spacing();
                                        ui.spacing_mut().interact_size.y = btn;
                                        ui.spacing_mut().item_spacing.x = gap;
                                        self.toolbar_contents(
                                            ui,
                                            &mut act,
                                            &mut save_cfg,
                                            &mut start_lsp,
                                        );
                                    }
                                },
                            );
                        });
                    });
                });
        }

        // ---- Quick-access toolbar (replaces the classic menu bar) ----
        // Hidden in zen / fullscreen; suppressed when moved into the titlebar.
        if !chrome_hidden && !toolbar_in_titlebar {
            egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
                // Phase 18 T18.5: apply the user-configurable button size + spacing
                // BEFORE the horizontal row so every quick-access item inherits the
                // sizing. All values are clamped at the config layer to defend
                // against a malformed user toml producing a 4000-px-tall toolbar.
                let btn = self.config.toolbar.clamped_button_size();
                let gap = self.config.toolbar.clamped_button_spacing();
                ui.spacing_mut().interact_size.y = btn;
                ui.spacing_mut().item_spacing.x = gap;
                ui.horizontal(|ui| {
                    self.toolbar_contents(ui, &mut act, &mut save_cfg, &mut start_lsp);
                });
            });
        }

        // ---- Tab strip in its OWN bar (T18.4) — separate from the toolbar ----
        //
        // #R5: in split/grid view the top tab strip is redundant — every pane
        // now carries its own chip header (note name + pin + close), so the
        // global strip is suppressed. New notes remain reachable via Ctrl+N,
        // the command palette, and the toolbar's customizable items.
        // The whole tab strip is hidden in zen mode and F11 fullscreen.
        // Set when the tab bar is at Bottom: its panel is rendered later, AFTER
        // the status bar, so the status bar keeps the very bottom screen edge and
        // the tab strip stacks directly above it (egui gives the first-shown bottom
        // panel the outermost slot).
        let mut bottom_tabs_deferred = false;
        if !chrome_hidden && !self.config.editor.grid_enabled {
            match self.config.editor.tab_bar_position {
                scribe_core::config::TabBarPosition::Top => {
                    // A dedicated tab bar directly below the quick-access toolbar
                    // (added after the "toolbar" top panel, so it stacks beneath it).
                    egui::TopBottomPanel::top("tabs-top")
                        .frame(egui::Frame::default().fill(panel))
                        .show(ctx, |ui| {
                            // PA-06: wrap the top strip in a HORIZONTAL ScrollArea
                            // (mirroring the side strips' vertical ScrollArea in
                            // `draw_side_tab_strip`) so that with many open tabs the
                            // overflowing tabs stay scroll-reachable instead of
                            // clipping off the right edge with no affordance.
                            egui::ScrollArea::horizontal()
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| self.draw_tab_strip(ui, accent, muted));
                                });
                        });
                }
                scribe_core::config::TabBarPosition::Bottom => {
                    // DEFERRED: a bottom tab bar must sit ABOVE the status bar, but
                    // egui gives the FIRST-shown bottom panel the screen edge. The
                    // status panel is shown later (below), so rendering the tab strip
                    // here would pin it under the status bar. Defer it and render it
                    // immediately AFTER the status panel so status keeps the very
                    // bottom edge and the tab strip stacks directly above it.
                    bottom_tabs_deferred = true;
                }
                scribe_core::config::TabBarPosition::Left => {
                    let rotated = self.config.editor.side_tabs_rotated;
                    let w = self.side_tab_bar_width(ctx, rotated);
                    let sp = egui::SidePanel::left("tabs-left")
                        .frame(egui::Frame::default().fill(panel).inner_margin(4.0));
                    // Rotated: a narrow vertical-text strip that just hugs its
                    // content (`exact_width`). Non-rotated (horizontal labels):
                    // RESIZABLE so the user can drag the divider to widen OR — now
                    // that titles truncate/ellipsise — SHRINK the bar below the
                    // longest title (min ~64px). `default_width` opens it fit-to-
                    // content; egui then remembers the user's dragged width (#16).
                    let sp = if rotated {
                        sp.exact_width(w)
                    } else {
                        sp.resizable(true)
                            .default_width(w)
                            .min_width(64.0)
                            .max_width(400.0)
                    };
                    sp.show(ctx, |ui| {
                        self.draw_side_tab_strip(ui, accent, muted, rotated);
                    });
                }
                scribe_core::config::TabBarPosition::Right => {
                    let rotated = self.config.editor.side_tabs_rotated;
                    let w = self.side_tab_bar_width(ctx, rotated);
                    let sp = egui::SidePanel::right("tabs-right")
                        .frame(egui::Frame::default().fill(panel).inner_margin(4.0));
                    let sp = if rotated {
                        sp.exact_width(w)
                    } else {
                        sp.resizable(true)
                            .default_width(w)
                            .min_width(64.0)
                            .max_width(400.0)
                    };
                    sp.show(ctx, |ui| {
                        self.draw_side_tab_strip(ui, accent, muted, rotated);
                    });
                }
            }
        }

        // ---- Config-error banner (F-038) ----
        //
        // Persistent top banner when the config TOML failed to parse on
        // launch. Surfaces the error message + actionable choices:
        // "Open config" (opens the TOML file as a new tab so the user can
        // hand-edit it), "Restore default" (overwrites the file with the
        // default Config and reloads), and "Dismiss" (clears the banner
        // for the session — the user took ownership of the warning).
        let mut want_open_cfg = false;
        let mut want_restore_cfg = false;
        let mut want_dismiss_cfg = false;
        if let Some(msg) = self.config_error_banner.clone() {
            egui::TopBottomPanel::top("config-error-banner")
                .frame(
                    egui::Frame::default()
                        .fill(warn.linear_multiply(0.20))
                        .inner_margin(egui::Margin::same(6)),
                )
                .show(ctx, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            RichText::new(egui_phosphor::thin::WARNING)
                                .color(warn)
                                .strong(),
                        );
                        ui.label(
                            RichText::new(format!("Config has errors: {msg}"))
                                .color(warn)
                                .monospace(),
                        );
                        if ui.button("Open config").clicked() {
                            want_open_cfg = true;
                        }
                        if ui.button("Restore default").clicked() {
                            want_restore_cfg = true;
                        }
                        if ui.button("Dismiss").clicked() {
                            want_dismiss_cfg = true;
                        }
                    });
                });
        }

        // ---- Update-available notice (notify mode) ----
        //
        // A PROMINENT top banner (accent-filled, bold) — not the passive toast —
        // so a found update is actually noticeable. Carries an "Update" button
        // that jumps straight to Settings → Updates to begin the update, plus a
        // "Dismiss" button. Shown only in `notify` mode (auto mode uses the modal).
        if let Some(v) = self.update_notice.clone() {
            let mut want_update = false;
            let mut want_dismiss = false;
            egui::TopBottomPanel::top("update-notice")
                .frame(
                    egui::Frame::default()
                        .fill(accent.linear_multiply(0.22))
                        .inner_margin(egui::Margin::symmetric(10, 7)),
                )
                .show(ctx, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            RichText::new(format!(
                                "SCR1B3 v{v} is available — you have v{}.",
                                crate::updater::current_version()
                            ))
                            .color(accent)
                            .strong(),
                        );
                        ui.add_space(8.0);
                        if ui.button(RichText::new("Update").strong()).clicked() {
                            want_update = true;
                        }
                        if ui.button("Dismiss").clicked() {
                            want_dismiss = true;
                        }
                    });
                });
            if want_update {
                // Jump to Settings → Updates so the user can start the update
                // (download → verify → restart) from the manual update controls.
                crate::settings::request_category(ctx, "Updates");
                self.settings_open = true;
                self.update_notice = None;
            }
            if want_dismiss {
                self.update_notice = None;
            }
        }

        // ---- External-change banner (F-022b) ----
        // A file open here was modified on disk WHILE it holds unsaved local
        // edits. Prompt the user to update to the saved version (or keep theirs)
        // instead of silently overwriting the newer file on save. A CLEAN tab is
        // reloaded silently by `poll_external_disk_changes` and never reaches here.
        if self.active < self.tabs.len() && self.tabs[self.active].external_change {
            let name = self.tabs[self.active].doc.file_name();
            let warn = egui::Color32::from_rgb(0xE0, 0x9A, 0x20);
            let mut want_reload = false;
            let mut want_keep = false;
            egui::TopBottomPanel::top("external-change-notice")
                .frame(
                    egui::Frame::default()
                        .fill(warn.linear_multiply(0.20))
                        .inner_margin(egui::Margin::symmetric(10, 7)),
                )
                .show(ctx, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            RichText::new(format!(
                                "{}  \"{name}\" was changed on disk, and you have unsaved edits here.",
                                egui_phosphor::thin::WARNING
                            ))
                            .color(warn)
                            .strong(),
                        );
                        ui.add_space(8.0);
                        if ui
                            .button(RichText::new("Reload from disk").strong())
                            .on_hover_text(
                                "Discard your unsaved edits and load the current saved version.",
                            )
                            .clicked()
                        {
                            want_reload = true;
                        }
                        if ui
                            .button("Keep my version")
                            .on_hover_text(
                                "Keep your edits — the next save will overwrite the disk version.",
                            )
                            .clicked()
                        {
                            want_keep = true;
                        }
                    });
                });
            let i = self.active;
            if want_reload {
                if let Some(path) = self.tabs[i].doc.path().map(|p| p.to_path_buf()) {
                    // ENC-1: encoding-preserving reload (see session_io.rs) — the
                    // user's explicit "reload from disk" must honour the file's
                    // detected encoding, not assume UTF-8.
                    if self.tabs[i].doc.reload_from_disk().is_ok() {
                        let fresh = self.tabs[i].doc.text();
                        self.tabs[i].set_text(fresh.clone());
                        self.tabs[i].disk_text = fresh;
                        if let Some(m) = file_mtime(&path) {
                            self.tabs[i].disk_mtime = Some(m);
                        }
                        // Change-bar: reloaded content is the new clean baseline.
                        self.tabs[i].reset_change_baselines();
                        // P2-C: the buffer was replaced wholesale — any
                        // multi-cursor carets now point at stale offsets.
                        self.mc_clear_carets();
                        self.status = format!("reloaded {} from disk", path.display());
                    }
                }
                self.tabs[i].external_change = false;
            }
            if want_keep {
                // Accept the current disk mtime as known so we stop re-prompting,
                // but keep the buffer + its unsaved edits (a later save overwrites
                // the disk file).
                if let Some(path) = self.tabs[i].doc.path().map(|p| p.to_path_buf()) {
                    if let Some(m) = file_mtime(&path) {
                        self.tabs[i].disk_mtime = Some(m);
                    }
                }
                self.tabs[i].external_change = false;
            }
        }

        // ---- Find / Replace bar ----
        //
        // F-008 from docs/audits/overlooked-surfaces-2026-05-29.md: the
        // pre-audit find bar had no replace field. Ctrl+F still opens
        // find-only; Ctrl+H opens the same bar with focus pre-set to the
        // replace field. "Replace next" replaces only the first match,
        // "Replace all" walks every match in the active buffer.
        if self.find_open {
            egui::TopBottomPanel::top("find").show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("find").color(accent).monospace());
                    let r = ui.text_edit_singleline(&mut self.find_query);
                    if self.focus_find {
                        r.request_focus();
                        self.focus_find = false;
                    }
                    // Editing the query restarts navigation at the first match.
                    if self.find_query != self.find_last_query {
                        self.find_match_idx = 0;
                        self.find_last_query = self.find_query.clone();
                    }
                    let count = self.find_matches_active().len();
                    self.find_match_idx = self.find_match_idx.min(count.saturating_sub(1));
                    // Enter in the find field jumps to the next match.
                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        self.find_navigate(true);
                    }
                    if ui
                        .add_enabled(
                            count > 0,
                            egui::Button::new(egui_phosphor::thin::ARROW_UP).small(),
                        )
                        .on_hover_text("Previous match (Shift+F3)")
                        .clicked()
                    {
                        self.find_navigate(false);
                    }
                    if ui
                        .add_enabled(
                            count > 0,
                            egui::Button::new(egui_phosphor::thin::ARROW_DOWN).small(),
                        )
                        .on_hover_text("Next match (F3 / Enter)")
                        .clicked()
                    {
                        self.find_navigate(true);
                    }
                    let counter = if count == 0 {
                        if self.find_query.is_empty() {
                            String::new()
                        } else {
                            "no matches".to_string()
                        }
                    } else {
                        format!("{}/{}", self.find_match_idx + 1, count)
                    };
                    ui.label(RichText::new(counter).color(muted).small());
                    if ui.button("close").clicked() {
                        self.find_open = false;
                    }
                });
                // Matching-mode row: regex / match-case / whole-word toggles plus
                // the inline invalid-regex error. THIS CALL IS THE WIRE the
                // toggles ride — `find_query_flags` reads the very fields these
                // checkboxes bind, so cutting this line makes the modes
                // unreachable from the UI (which `find_toggle_tests` detects).
                self.find_bar_options_ui(ui);
                // Second row: replace field + actions.
                ui.horizontal(|ui| {
                    ui.label(RichText::new("with").color(accent).monospace());
                    let rr = ui.text_edit_singleline(&mut self.replace_query);
                    if self.focus_replace {
                        rr.request_focus();
                        self.focus_replace = false;
                    }
                    if ui.button("Replace next").clicked() {
                        self.replace_in_active(false);
                    }
                    if ui.button("Replace all").clicked() {
                        self.replace_in_active(true);
                    }
                });
                // Third row: regex / match-case / whole-word toggles + the
                // inline invalid-regex error (see `find_replace.rs`).
                // CUT
            });
        }

        // ---- Wave-5: find in files (project-wide search results pane) ----
        if self.find_in_files_open {
            // PA-02: read Up/Down/Enter for RESULT navigation here (outside the
            // panel body), mirroring the command-palette / fuzzy-finder list-nav.
            // Enter opens the selected result, but ONLY when the query field is
            // not focused — an Enter in the query field triggers SEARCH (handled
            // below via `lost_focus()`), so the two Enter meanings never collide.
            let result_count = self.find_in_files_results.len();
            let (up, down, enter_pressed) = ctx.input(|i| {
                (
                    i.key_pressed(egui::Key::ArrowUp),
                    i.key_pressed(egui::Key::ArrowDown),
                    i.key_pressed(egui::Key::Enter),
                )
            });
            if result_count == 0 {
                self.find_in_files_selected = 0;
            } else {
                self.find_in_files_selected =
                    fuzzy_move_selection(self.find_in_files_selected, result_count, up, down);
            }
            let selected = self.find_in_files_selected;
            let mut open_selected_via_enter = false;
            egui::SidePanel::right("find_in_files")
                .resizable(true)
                .default_width(360.0)
                .frame(egui::Frame::default().fill(panel).inner_margin(6.0))
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("find in files").color(accent).monospace());
                        if ui.button("close").clicked() {
                            self.find_in_files_open = false;
                        }
                    });
                    let r = ui.text_edit_singleline(&mut self.find_in_files_query);
                    if self.focus_find_in_files {
                        r.request_focus();
                        self.focus_find_in_files = false;
                    }
                    let query_focused = r.has_focus();
                    // Enter while the query is NOT focused (e.g. after arrow-key
                    // navigation moved focus into the results) opens the selected
                    // result — the keyboard-activate leg the audit (PA-02) flagged.
                    if enter_pressed && !query_focused && result_count > 0 {
                        open_selected_via_enter = true;
                    }
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.find_in_files_regex, "regex");
                        ui.checkbox(&mut self.find_in_files_case_sensitive, "match case");
                        ui.checkbox(&mut self.find_in_files_whole_word, "whole word");
                        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if enter || ui.button("search").clicked() {
                            self.run_find_in_files(ctx);
                        }
                    });
                    if let Some(err) = &self.find_in_files_error {
                        ui.colored_label(Color32::from_rgb(0xe5, 0x3e, 0x3e), err);
                    }
                    // 4-02: streaming hint while the off-thread worker is walking.
                    if self.find_in_files_running {
                        ui.label(
                            RichText::new(format!(
                                "searching… {} so far",
                                self.find_in_files_results.len()
                            ))
                            .color(muted)
                            .small(),
                        );
                    }
                    ui.separator();
                    let mut open_target: Option<(PathBuf, usize)> = None;
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        for (idx, m) in self.find_in_files_results.iter().enumerate() {
                            let name = m.path.file_name().and_then(|n| n.to_str()).unwrap_or("?");
                            let label = format!("{}:{}  {}", name, m.line, m.line_text.trim());
                            // PA-02: a highlighted selectable row (mirroring the
                            // palette / fuzzy finder) replaces the bare click-Label,
                            // so the keyboard-selected result is visibly distinct
                            // and Up/Down/Enter drive it — not mouse-click only.
                            let row = ui.selectable_label(
                                idx == selected,
                                RichText::new(label).monospace().small(),
                            );
                            if row.clicked() {
                                open_target = Some((m.path.clone(), m.line));
                            }
                            if idx == selected && (up || down) {
                                row.scroll_to_me(Some(egui::Align::Center));
                            }
                        }
                    });
                    // Enter (query unfocused) opens the keyboard-selected result.
                    if open_selected_via_enter {
                        if let Some(m) = self.find_in_files_results.get(selected) {
                            open_target = Some((m.path.clone(), m.line));
                        }
                    }
                    if let Some((path, line)) = open_target {
                        self.open_find_in_files_result(path, line);
                    }
                });
        }

        // ---- Command palette (built-in + plugin commands) ----
        //
        // F-004 fix from docs/audits/overlooked-surfaces-2026-05-29.md:
        // the palette previously surfaced only plugin commands. On a fresh
        // install (zero plugins loaded), opening Ctrl+Shift+P showed
        // "no plugin commands yet" — the editor's primary self-discovery
        // surface was empty. Now every built-in editor action is listed
        // alphabetically alongside plugin commands and the fuzzy filter
        // searches both.
        let mut run_builtin: Option<BuiltinCommand> = None;
        if self.palette_open {
            // BUG-APP-01 fix: build the filtered command list ONCE up front so
            // keyboard nav (Up/Down/Enter) and the rendered rows agree on the
            // same set — mirroring the fuzzy-file-finder's "rank once up front"
            // pattern (frame_modals.rs `if self.fuzzy_open`). Each entry carries what to
            // run; the index into this Vec is the selectable highlight.
            enum PaletteAction {
                Builtin(BuiltinCommand),
                Plugin(String),
            }
            struct PaletteItem {
                display: String,
                action: PaletteAction,
                /// True for the first plugin command — render a separator above
                /// it, preserving the prior built-in/plugin visual split.
                separator_before: bool,
            }
            let q = self.palette_query.to_lowercase();
            let mut items: Vec<PaletteItem> = Vec::new();
            // Built-in commands first — universally available even with zero
            // plugins.
            for cmd in BUILTIN_COMMANDS {
                let label = cmd.label;
                // Same rule as the cheatsheet: show the chord the user actually
                // has, not the one this command shipped with. Searching matches
                // the DISPLAYED text, so typing "ctrl+e" finds a rebound command.
                let shortcut = self
                    .keymap
                    .display_for(cmd.bindings)
                    .unwrap_or_else(|| keymap::platform_chord_text(cmd.shortcut));
                if q.is_empty()
                    || label.to_lowercase().contains(&q)
                    || shortcut.to_lowercase().contains(&q)
                {
                    let display = if shortcut.is_empty() {
                        label.to_string()
                    } else {
                        format!("{label}  ·  {shortcut}")
                    };
                    items.push(PaletteItem {
                        display,
                        action: PaletteAction::Builtin(cmd.action),
                        separator_before: false,
                    });
                }
            }
            let mut first_plugin = true;
            for c in &self.plugin_cmds {
                if q.is_empty() || c.label.to_lowercase().contains(&q) || c.id.contains(&q) {
                    items.push(PaletteItem {
                        display: format!("{}  ·  {}", c.label, c.plugin_id),
                        action: PaletteAction::Plugin(c.id.clone()),
                        separator_before: first_plugin,
                    });
                    first_plugin = false;
                }
            }

            // Read Up/Down/Enter here (outside the window body). A singleline
            // TextEdit ignores these keys, so this does not fight the query
            // field's caret — same rationale as the fuzzy finder.
            let (up, down, enter) = ctx.input(|i| {
                (
                    i.key_pressed(egui::Key::ArrowUp),
                    i.key_pressed(egui::Key::ArrowDown),
                    i.key_pressed(egui::Key::Enter),
                )
            });
            if items.is_empty() {
                self.palette_selected = 0;
            } else {
                self.palette_selected =
                    fuzzy_move_selection(self.palette_selected, items.len(), up, down);
                if enter {
                    match &items[self.palette_selected].action {
                        PaletteAction::Builtin(a) => run_builtin = Some(*a),
                        PaletteAction::Plugin(id) => run_cmd = Some(id.clone()),
                    }
                }
            }
            let selected = self.palette_selected;

            let mut query_changed = false;
            egui::Window::new(
                RichText::new(format!("{}  command palette", egui_phosphor::thin::COMMAND))
                    .color(accent)
                    .monospace(),
            )
            .collapsible(false)
            .resizable(false)
            // A fixed width so the primary command-discovery surface opens at a
            // consistent size (matching the other modal pickers) instead of
            // sizing to its content. Aligns with go-to-symbol/recent/fuzzy.
            .default_width(600.0)
            .anchor(egui::Align2::CENTER_TOP, [0.0, 64.0])
            .show(ctx, |ui| {
                let r = ui.text_edit_singleline(&mut self.palette_query);
                if self.focus_palette {
                    r.request_focus();
                    self.focus_palette = false;
                }
                query_changed = r.changed();
                egui::ScrollArea::vertical()
                    .max_height(360.0)
                    .show(ui, |ui| {
                        for (idx, item) in items.iter().enumerate() {
                            if item.separator_before {
                                ui.separator();
                            }
                            let row = ui.selectable_label(idx == selected, item.display.clone());
                            if row.clicked() {
                                match &item.action {
                                    PaletteAction::Builtin(a) => run_builtin = Some(*a),
                                    PaletteAction::Plugin(id) => run_cmd = Some(id.clone()),
                                }
                            }
                            // Keep the keyboard-highlighted row in view.
                            if idx == selected && (up || down) {
                                row.scroll_to_me(Some(egui::Align::Center));
                            }
                        }
                        if items.is_empty() {
                            ui.label(RichText::new("no match").color(muted).small());
                        }
                    });
            });
            // A new query invalidates the old highlight position — reset to the
            // top so Enter runs the new top match (acceptance criterion 2).
            if query_changed {
                self.palette_selected = 0;
            }
        }

        // ---- Settings window (deep customization, live preview) ----
        if self.settings_open {
            // The Settings window is exempt from the app colour-tint; pass the
            // theme's UN-tinted text-field background so its inputs don't inherit
            // the tinted global `extreme_bg_color`.
            let untinted_field_bg = scribe_render::theme_to_visuals(&self.theme).extreme_bg_color;
            let changed = crate::settings::show(
                ctx,
                &mut self.config,
                &mut self.settings_open,
                &mut self.updater,
                untinted_field_bg,
            );
            // F-039 — the Plugins section's "Manage plugins…" button stashes a
            // request flag; pick it up and open the plugin-manager modal.
            if crate::settings::take_open_plugin_manager_request(ctx) {
                self.plugin_manager
                    .ensure_defaults(Config::config_dir().as_deref());
                self.plugin_manager.open = true;
            }
            if changed {
                self.reapply_theme(ctx);
                // Spellcheck language / custom-dict edits take effect live.
                self.reload_spell_engine();
                // F-035 — push the always-on-top flag to the viewport
                // immediately so the toggle is live (no restart required).
                let level = if self.config.window.always_on_top {
                    egui::WindowLevel::AlwaysOnTop
                } else {
                    egui::WindowLevel::Normal
                };
                ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(level));
                self.save_config();
            }
        } else {
            // Settings is closed — by its own ✕, by Esc, by a command, or by any
            // other route — so no chord capture can be in flight. Clearing here
            // (rather than only on the ✕ path) is what keeps a capture the user
            // walked away from from surviving into the next time Settings opens,
            // where it would stand the editor's shortcut layer down for keys the
            // user never meant to rebind.
            crate::app::settings_keys::clear_capture(ctx);
        }

        // ---- Keyboard cheatsheet (F1) ----
        //
        // F-014 from docs/audits/overlooked-surfaces-2026-05-29.md. Lists
        // every wired shortcut so the user doesn't have to guess. The table
        // is rendered as a markdown-like 2-column grid; the data lives in
        // KEYBOARD_SHORTCUTS so any future shortcut addition lands in one
        // place + the modal stays current.
        if self.cheatsheet_open {
            let mut still_open = true;
            egui::Window::new(
                RichText::new(format!(
                    "{}  keyboard shortcuts",
                    egui_phosphor::thin::KEYBOARD
                ))
                .color(accent)
                .monospace(),
            )
            .open(&mut still_open)
            .collapsible(false)
            .resizable(true)
            .default_width(420.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(420.0)
                    .show(ui, |ui| {
                        egui::Grid::new("cheatsheet-grid")
                            .num_columns(2)
                            .spacing([24.0, 6.0])
                            .striped(true)
                            .show(ui, |ui| {
                                for entry in KEYBOARD_SHORTCUTS {
                                    // Rebindable rows render the user's CURRENT
                                    // chord; a hard-wired row (`bindings: &[]`)
                                    // keeps its static text. Reading the live
                                    // keymap is what stops the cheatsheet from
                                    // teaching the default chord to someone who
                                    // rebound it.
                                    let chord =
                                        self.keymap.display_for(entry.bindings).unwrap_or_else(
                                            || keymap::platform_chord_text(entry.chord),
                                        );
                                    ui.label(RichText::new(chord).color(accent).monospace());
                                    ui.label(RichText::new(entry.action).color(muted).small());
                                    ui.end_row();
                                }
                            });
                    });
                ui.add_space(8.0);
                ui.label(
                    RichText::new("press F1 or Esc to close")
                        .color(muted)
                        .small()
                        .monospace(),
                );
            });
            if !still_open {
                self.cheatsheet_open = false;
            }
        }

        // ---- Plugin manager modal (F-039 + F-040) ----
        //
        // Surfaces the Phase-20 plugin foundation. The host builds the Loaded
        // rows from `discover()` + `config.plugins.disabled`, passes the
        // plugins dir, and applies whatever action the modal returns.
        if self.plugin_manager.open {
            let plugins_dir = Config::config_dir()
                .map(|d| d.join("plugins"))
                .unwrap_or_else(|| PathBuf::from("plugins"));
            let loaded = self.discovered_plugin_rows(&plugins_dir);
            let action = self
                .plugin_manager
                .show(ctx, accent, muted, &loaded, &plugins_dir);
            if let Some(id) = action.toggle_disabled {
                if let Some(pos) = self.config.plugins.disabled.iter().position(|d| *d == id) {
                    self.config.plugins.disabled.remove(pos);
                } else {
                    self.config.plugins.disabled.push(id);
                }
                self.save_config();
            }
            if action.open_plugins_dir {
                // Best-effort: create the dir so the reveal lands somewhere,
                // then open it in the OS file manager.
                let _ = std::fs::create_dir_all(&plugins_dir);
                open_in_file_manager(&plugins_dir);
            }
            if let Some(id) = action.approve {
                self.approve_plugin(&id);
            }
        }

        self.render_picker_modals(ctx, accent, muted);

        // Spellcheck status (computed before the status-bar closure borrows self).
        let spell_on = self.config.spellcheck.enabled;
        let spell_misspellings = self.spell_count();
        let diag_errors = self.diagnostics.iter().filter(|d| d.severity == 1).count();
        let diag_total = self.diagnostics.len();

        // ---- Status bar ----
        let mut cycle_eol_for_active = false;
        let mut open_settings_for = None;
        // Hidden in zen / distraction-free mode and in F11 fullscreen, and when
        // the user has turned the status bar off in Appearance settings.
        if !chrome_hidden && self.config.appearance.show_status_bar {
            egui::TopBottomPanel::bottom("status")
                .frame(egui::Frame::default().fill(panel))
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        // Edge padding so the leftmost status segment isn't flush against
                        // the window edge (mirrors the titlebar's 10px lead-in).
                        ui.add_space(8.0);
                        let active = self.active.min(self.tabs.len().saturating_sub(1));
                        // PA-04: line/word/char counts via the (edit_gen, doc_id)
                        // memo — recomputed only on edit, not every idle frame.
                        let (lines, words, chars) = self.doc_counts_active(active);
                        if let Some(t) = self.tabs.get(active) {
                            // F-025 — clickable EOL segment cycles LF → CRLF → CR.
                            if ui
                                .selectable_label(
                                    false,
                                    RichText::new(t.doc.eol().label().to_string())
                                        .color(muted)
                                        .small()
                                        .monospace(),
                                )
                                .on_hover_text("Click to cycle line-ending: LF → CRLF → CR")
                                .clicked()
                            {
                                cycle_eol_for_active = true;
                            }
                            // F-025 — encoding + language: click opens Settings
                            // so the user lands on the relevant editor section.
                            if ui
                                .selectable_label(
                                    false,
                                    RichText::new(t.doc.encoding().name.clone())
                                        .color(muted)
                                        .small()
                                        .monospace(),
                                )
                                .on_hover_text("Click to open Settings → Editor")
                                .clicked()
                            {
                                open_settings_for = Some("Editor");
                            }
                            let lang = t.doc.language_hint().unwrap_or_else(|| "text".into());
                            if ui
                                .selectable_label(
                                    false,
                                    RichText::new(lang).color(accent).small().monospace(),
                                )
                                .on_hover_text("Click to open Settings → Editor (language hint)")
                                .clicked()
                            {
                                open_settings_for = Some("Editor");
                            }
                            // F-024 — word + line counters in the status bar.
                            // Computed via `doc_counts_active` (PA-04 memo): the
                            // three O(n) passes run once per edit, not per frame.
                            // Word/char are 0 for is_read_only_large() (multi-GB
                            // rope-browser) buffers, as before.
                            ui.label(
                                RichText::new(format!("{lines} ln · {words} w · {chars} ch"))
                                    .color(muted)
                                    .small()
                                    .monospace(),
                            );
                            // F-005 / F-024 from docs/audits/overlooked-surfaces-2026-05-29.md:
                            // Render the caret position ("Ln 4, Col 17") + the selection
                            // length when non-empty. Every editor on Earth ships this
                            // indicator; SCR1B3 used to omit it.
                            if let Some((ln, col)) = self.last_cursor_line_col {
                                ui.label(
                                    RichText::new(format!("Ln {ln}, Col {col}"))
                                        .color(muted)
                                        .small()
                                        .monospace(),
                                );
                            }
                            if self.last_selection_chars > 0 {
                                let sel = self.last_selection_chars;
                                let noun = if sel == 1 { "char" } else { "chars" };
                                ui.label(
                                    RichText::new(format!("({sel} {noun} sel)"))
                                        .color(accent)
                                        .small()
                                        .monospace(),
                                );
                            }
                            if t.doc.is_read_only_large() {
                                ui.label(
                                    RichText::new("[ large file: read-only ]")
                                        .color(muted)
                                        .small()
                                        .monospace(),
                                );
                            }
                            // Editor-mode badge. When the buffer crosses the rope
                            // threshold the editor silently swaps to a degraded
                            // surface and ~25 TextEdit-only conveniences stop working;
                            // before this, the user was given no signal at all. The
                            // full-feature Standard mode is deliberately badge-less —
                            // no badge IS the "nothing is degraded" signal — and the
                            // hover names the exact trade-off so the acronym is
                            // self-describing. Reads last frame's published mode
                            // (status bar renders before the central panel); the
                            // one-frame lag is invisible and always correct at rest.
                            if let Some(badge) = active_editor_mode(ctx).badge() {
                                ui.label(
                                    RichText::new(format!("[ {badge} ]"))
                                        .color(warn)
                                        .small()
                                        .monospace(),
                                )
                                .on_hover_text(active_editor_mode(ctx).hover());
                            }
                            if spell_on {
                                let (txt, col) = if spell_misspellings == 0 {
                                    (format!("spell {}", egui_phosphor::thin::CHECK), accent)
                                } else {
                                    (format!("spell: {spell_misspellings}"), warn)
                                };
                                ui.label(RichText::new(txt).color(col).small().monospace());
                            }
                            if diag_total > 0 {
                                let col = if diag_errors > 0 { warn } else { muted };
                                ui.label(
                                    RichText::new(format!(
                                        "{} {diag_errors}e / {diag_total}",
                                        egui_phosphor::thin::PROHIBIT
                                    ))
                                    .color(col)
                                    .small()
                                    .monospace(),
                                );
                            }
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            // Inset from the right window edge so the last glyph
                            // isn't flush against it (right_to_left places this
                            // space at the right edge, before the text).
                            ui.add_space(8.0);
                            // The status text (e.g. "opened C:\…\file.rs") gets the
                            // width REMAINING to the right of the left-side indicators
                            // and TRUNCATES with an ellipsis instead of overflowing
                            // leftward and overlapping them on a narrow window. The
                            // full text stays available on hover. `.truncate()` clips
                            // to the laid-out width (the remaining space in this
                            // right_to_left child), so it never collides with the left
                            // segments; on a wide window it has room for the whole
                            // string and looks unchanged.
                            if !self.status.is_empty() {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(&self.status)
                                            .color(muted)
                                            .small()
                                            .monospace(),
                                    )
                                    .truncate(),
                                )
                                .on_hover_text(&self.status);
                            }
                        });
                    });
                });
        }
        // Bottom tab bar (deferred from the tab-position match): rendered HERE,
        // after the status panel, so the status bar keeps the very bottom edge
        // and the tab strip sits directly above it.
        if bottom_tabs_deferred {
            egui::TopBottomPanel::bottom("tabs-bottom")
                .frame(egui::Frame::default().fill(panel))
                .show(ctx, |ui| {
                    ui.horizontal(|ui| self.draw_tab_strip(ui, accent, muted));
                });
        }
        // F-025 — apply the click-to-edit status-bar actions captured above.
        if cycle_eol_for_active {
            let active = self.active.min(self.tabs.len().saturating_sub(1));
            if let Some(t) = self.tabs.get_mut(active) {
                let next = match t.doc.eol() {
                    scribe_core::eol::Eol::Lf => scribe_core::eol::Eol::Crlf,
                    scribe_core::eol::Eol::Crlf => scribe_core::eol::Eol::Cr,
                    scribe_core::eol::Eol::Cr => scribe_core::eol::Eol::Lf,
                };
                t.doc.set_eol(next);
                self.status = format!("line-ending: {}", next.label());
            }
        }
        if let Some(section) = open_settings_for {
            // Honour the deep-link: open Settings ON the advertised category
            // (the tooltips promise "Settings → Editor"), not the last-used one.
            crate::settings::request_category(ctx, section);
            self.settings_open = true;
        }

        // ---- Toast (errors / notices) ----
        if let Some(msg) = self.toast.clone() {
            egui::TopBottomPanel::bottom("toast").show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("!")
                            .color(ui_color(
                                &self.theme,
                                "warning",
                                Rgba::new(0xfb, 0xbf, 0x24, 255),
                            ))
                            .strong(),
                    );
                    ui.label(RichText::new(&msg).small());
                    if ui.small_button("dismiss").clicked() {
                        self.toast = None;
                    }
                });
            });
        }

        // ---- File-tree sidebar ----
        if let Some(root) = self.file_tree_root.clone() {
            egui::SidePanel::left("filetree")
                .default_width(220.0)
                .frame(egui::Frame::default().fill(panel).inner_margin(6.0))
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        // #74 — the tree supports ↑↓ Home End ⏎ navigation, but
                        // that was undiscoverable. Surface it: a hover tip on the
                        // header plus a always-visible muted key hint.
                        ui.label(RichText::new("EXPLORER").color(accent).small().monospace())
                            .on_hover_text(
                                "File explorer. Keyboard: ↑/↓ move · Home/End jump to first/last \
                                 · Enter open · (works when no dialog is open and the editor isn't \
                                 focused).",
                            );
                        ui.label(
                            RichText::new(format!(
                                "{}{} Home End {}",
                                egui_phosphor::thin::ARROW_UP,
                                egui_phosphor::thin::ARROW_DOWN,
                                egui_phosphor::thin::ARROW_ELBOW_DOWN_LEFT
                            ))
                            .color(muted)
                            .small()
                            .monospace(),
                        )
                        .on_hover_text("Navigate the file tree from the keyboard.");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("×").clicked() {
                                close_tree = true;
                            }
                        });
                    });
                    ui.separator();
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        if let Some(p) = self.file_tree_state.show(ui, &root) {
                            open_from_tree = Some(p);
                        }
                    });
                });
            // F-041: arrow-key / Enter / Home / End nav for the sidebar.
            // Only fires when no modal is open AND the editor isn't focused
            // (egui owns key events when a TextEdit holds focus, so we don't
            // need to gate explicitly on that — `consume_key` is a no-op
            // when the key was already routed to a widget).
            let modal_open = self.palette_open
                || self.find_open
                || self.fuzzy_open
                || self.goto_open
                || self.goto_symbol_open
                || self.recent_open
                || self.recent_folders_open
                || self.cheatsheet_open
                || self.settings_open
                || self.welcome_open;
            if !modal_open {
                if let Some(p) = self.file_tree_state.handle_input(ctx) {
                    open_from_tree = Some(p);
                }
            }
        }

        // ---- Notes (PKM) side pane ----
        // Self-guarded: renders its own left SidePanel only when `notes_pane_open`
        // is set (toggled by BuiltinCommand::ToggleNotesPane), lazily (re)building
        // the vault index. This is the single live call site that makes the whole
        // note-app surface reachable — the vault scan, the operator search, the
        // wiki-link open/create, and the backlinks pane.
        self.render_notes_pane(ctx, panel, accent, muted);

        let active = self.active.min(self.tabs.len().saturating_sub(1));
        self.active = active;
        let font = FontId::monospace(self.config.fonts.clamped_editor_size());
        let line_height = self.config.fonts.clamped_line_height();
        let word_wrap = self.config.editor.word_wrap;
        let show_line_numbers = self.config.editor.show_line_numbers;
        let gutter_row_h = font.size * line_height;
        let ext = self.tabs[active].doc.language_hint();
        let read_only = self.tabs[active].doc.is_read_only_large();
        // The editor should be ready to type whenever no field/menu is open.
        let overlay_open = self.find_open || self.palette_open || self.settings_open;

        // ---- Wave-5 P1: markdown live preview (right side panel) ----
        // Only for markdown buffers; renders the buffer via pulldown-cmark.
        if self.md_preview_open && !chrome_hidden {
            let is_md = self
                .tabs
                .get(active)
                .and_then(|t| t.doc.language_hint())
                .map(|l| l == "md" || l == "markdown")
                .unwrap_or(false);
            if is_md {
                let md = self.tabs[active].text.clone();
                // Bound on the size of note the live preview will render.
                //
                // This cap used to be justified by "the preview re-parses the
                // whole document and rebuilds the widget tree every frame". Half
                // of that is no longer true: the parse is cached on the source
                // text (`md_preview::cache`) and so are the three header metric
                // scans below (`note_metrics`). The cap survives on the OTHER
                // half — the widget rebuild, which is not cached and cannot
                // easily be: `md_preview::show` reconstructs every egui widget
                // for the whole document on every frame.
                //
                // Re-derived by measurement rather than inherited. Release
                // profile, synthetic markdown, this machine:
                //
                //   size     parse (cold)   metrics (cold / cached)   full frame
                //   256 KiB     10.5 ms        1.4 ms / 0.017 ms        26.8 ms
                //   512 KiB      9.8 ms        2.3 ms / 0.035 ms        50.8 ms
                //   1 MiB       23.8 ms        4.5 ms / 0.134 ms       117.3 ms
                //   2 MiB       42.6 ms        9.7 ms / 0.214 ms       292.1 ms
                //
                // "full frame" is a complete egui pass through `md_preview::show`
                // with the parse ALREADY cached. At 1 MiB that is ~117 ms — about
                // 8 fps, seven times over a 60 fps budget — and the caching this
                // change added removed only ~28 ms of it (parse + metrics). So
                // the cap stays at 1 MiB: raising it would hand the user a
                // preview that pins a core for a quarter-second per frame, and
                // the two things that got cheaper were never what made it
                // expensive.
                const PREVIEW_MAX_BYTES: usize = 1 << 20; // 1 MiB
                let preview_too_large = md.len() > PREVIEW_MAX_BYTES;
                // P0-1 / P1-3: task progress + reading-time + heading count, shown
                // in the preview header. Three full-document scans, cached on the
                // source text so they run once per EDIT rather than once per
                // frame. Still skipped entirely for an oversized note, whose
                // preview is not drawn at all.
                let metrics = if preview_too_large {
                    super::note_metrics::NoteMetrics::default()
                } else {
                    super::note_metrics::metrics_for(&md)
                };
                let (done, total) = (metrics.done, metrics.total);
                let (mins, headings) = (metrics.minutes, metrics.headings);
                // Source lines whose checkbox was clicked this frame (applied
                // after the panel closes so the borrow on `md` is released).
                let mut toggled: Vec<usize> = Vec::new();
                egui::SidePanel::right("md-preview")
                    .default_width(360.0)
                    .frame(egui::Frame::default().fill(panel).inner_margin(8.0))
                    .show(ctx, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Markdown preview").color(muted).small());
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.small_button("close").clicked() {
                                        self.md_preview_open = false;
                                    }
                                },
                            );
                        });
                        // Note metrics line: "~N min · H headings · ☑ done/total".
                        if !preview_too_large {
                            ui.horizontal_wrapped(|ui| {
                                let mut bits = format!("~{mins} min · {headings} headings");
                                if total > 0 {
                                    bits.push_str(&format!(" · ☑ {done}/{total}"));
                                }
                                ui.label(RichText::new(bits).color(muted).small().monospace());
                            });
                        }
                        ui.separator();
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                if preview_too_large {
                                    ui.label(
                                        RichText::new(format!(
                                            "Note is {:.1} MiB — live preview is disabled above \
                                             {} MiB to keep editing responsive. Editing, syntax \
                                             highlighting and all other features stay active.",
                                            md.len() as f64 / (1024.0 * 1024.0),
                                            PREVIEW_MAX_BYTES / (1024 * 1024),
                                        ))
                                        .color(muted)
                                        .small(),
                                    );
                                } else {
                                    toggled = crate::md_preview::show(ui, &md, accent, muted);
                                }
                            });
                    });
                // Apply any preview checkbox clicks to the SOURCE (edits the real
                // `[ ]`/`[x]` line, never a hidden state).
                if !toggled.is_empty() && active < self.tabs.len() {
                    let mut text = self.tabs[active].text.clone();
                    for line in toggled {
                        if let Some(next) =
                            scribe_core::md_ops::toggle_task_on_lines(&text, line, line)
                        {
                            text = next;
                        }
                    }
                    if text != self.tabs[active].text {
                        self.tabs[active].set_text_keep_undo(text);
                        self.tabs[active].doc.mark_dirty();
                    }
                }
            }
        }

        // ---- Wave-5 P1: diff vs disk (right side panel) ----
        if self.diff_view_open && !chrome_hidden {
            let cur = self.tabs.get(active).map(|t| t.text.clone());
            let disk = self
                .tabs
                .get(active)
                .and_then(|t| t.doc.path())
                .and_then(|p| std::fs::read_to_string(p).ok())
                .unwrap_or_default();
            let colors = crate::diff_view::DiffColors {
                insert: ui_color(&self.theme, "ok", Rgba::new(0x6e, 0xc7, 0x7a, 255)),
                delete: ui_color(&self.theme, "error", Rgba::new(0xd0, 0x6e, 0x6e, 255)),
                context: muted,
            };
            if let Some(cur) = cur {
                egui::SidePanel::right("diff-view")
                    .default_width(420.0)
                    .frame(egui::Frame::default().fill(panel).inner_margin(8.0))
                    .show(ctx, |ui| {
                        let rows = crate::diff_view::diff_lines(&disk, &cur);
                        let (ins, del) = crate::diff_view::summary(&rows);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Diff vs disk").color(muted).small());
                            ui.label(
                                RichText::new(format!("+{ins}"))
                                    .color(colors.insert)
                                    .small(),
                            );
                            ui.label(
                                RichText::new(format!("-{del}"))
                                    .color(colors.delete)
                                    .small(),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.small_button("close").clicked() {
                                        self.diff_view_open = false;
                                    }
                                },
                            );
                        });
                        ui.separator();
                        crate::diff_view::show_rows(ui, &rows, colors);
                    });
            }
        }

        // ---- Minimap (rightmost strip) ----
        // Skipped for read-only huge files: the minimap hashes + lays out the
        // whole buffer, which defeats the viewport-culled browse path below.
        if self.config.editor.show_minimap && !read_only && !chrome_hidden {
            self.show_minimap(ctx, panel, accent);
        }

        // Split is no longer a separate same-buffer side panel — it is unified
        // with the multi-note grid (`editor.grid_enabled`): the open tabs render
        // as panes (two = side-by-side split, more = grid) via
        // `render_grid_central_panel`. See the "split" toolbar button + the
        // grid central-panel branch above.

        // ---- Line-number gutter (sticky left strip; numbers are synced to the
        // editor galley rows captured last frame — one-frame lag, like minimap).
        // The external gutter is driven by the TextEdit's per-line galley Ys
        // (`line_gutter`). The read-only RopeEditor draws its OWN gutter, so
        // skip this one there (and avoid the O(n) `lines().count()` on a
        // 256 MiB+ buffer).
        if show_line_numbers && !self.fold_view && !read_only && !chrome_hidden {
            // Change-bar: refresh the per-line state cache before borrowing it.
            self.ensure_change_states(active);
            // PA-05: reuse the PA-04 (edit_gen, doc_id) memo for the gutter
            // digit-width line count — no extra per-frame O(n) `lines().count()`.
            let total = self.doc_counts_active(active).0;
            let digits = total.to_string().len().max(2);
            let gutter_w = digits as f32 * (font.size * 0.62) + 16.0;
            let rows = &self.line_gutter;
            let bookmarks = &self.tabs[active].bookmarks;
            let show_change_bar = self.config.editor.show_change_bar;
            let change_states = &self.tabs[active].change_states;
            let cb_unsaved = ui_color(
                &self.theme,
                "change_bar_unsaved",
                Rgba::new(0xf2, 0xb3, 0x3d, 255),
            );
            let cb_saved = ui_color(
                &self.theme,
                "change_bar_saved",
                Rgba::new(0x6f, 0xb8, 0x9a, 255),
            );
            // Diagnostic gutter marks: worst severity per source line, sorted.
            // The squiggle tells you a line is wrong once you are looking at it;
            // the gutter mark is what tells you WHICH line to look at while you
            // are somewhere else in the file.
            let diag_marks = super::diagnostics_overlay::gutter_marks(&self.diagnostics);
            let diag_err = ui_color(&self.theme, "error", Rgba::new(0xe5, 0x3e, 0x3e, 255));
            let diag_warn = ui_color(&self.theme, "warning", Rgba::new(0xf2, 0xb3, 0x3d, 255));
            egui::SidePanel::left("line-gutter")
                .exact_width(gutter_w)
                .resizable(false)
                .frame(egui::Frame::default().fill(panel))
                .show(ctx, |ui| {
                    let painter = ui.painter();
                    let clip = ui.clip_rect();
                    let rx = ui.max_rect().right() - 8.0;
                    let lx = ui.max_rect().left() + 4.0;
                    // Change bar sits flush against the gutter's right edge
                    // (between the numbers and the text), Notepad++-style.
                    let bar_r = ui.max_rect().right();
                    let nfont = FontId::monospace((font.size * 0.92).max(8.0));
                    for (i, &y) in rows.iter().enumerate() {
                        if y < clip.top() - gutter_row_h || y > clip.bottom() {
                            continue;
                        }
                        // Change-bar stripe: amber for edited-unsaved lines,
                        // green for edited-then-saved; untouched lines have none.
                        if show_change_bar {
                            let col = match change_states.get(i) {
                                Some(crate::change_bar::LineChange::Unsaved) => Some(cb_unsaved),
                                Some(crate::change_bar::LineChange::Saved) => Some(cb_saved),
                                _ => None,
                            };
                            if let Some(col) = col {
                                // 3.5px stripe flush to the gutter's right edge
                                // (Notepad++/VS Code use ~3px; a touch wider here
                                // so it reads clearly at the gutter boundary).
                                // Extend the stripe down to the NEXT logical line's
                                // Y so that a word-wrapped modified line (which
                                // occupies several visual rows) shows a full-height
                                // marker spanning every wrapped row, not just a
                                // single-row tick. `rows` holds one Y per logical
                                // line, so `rows[i+1]` is the bottom of line i's
                                // wrapped block; the last line falls back to one
                                // row height. When word-wrap is off this is exactly
                                // one row, so no visual change there.
                                let bottom = rows.get(i + 1).copied().unwrap_or(y + gutter_row_h);
                                painter.rect_filled(
                                    egui::Rect::from_min_max(
                                        egui::pos2(bar_r - 3.5, y),
                                        egui::pos2(bar_r, bottom),
                                    ),
                                    0.0,
                                    col,
                                );
                            }
                        }
                        // Bookmark marker: a small filled dot at the gutter's
                        // left edge for each bookmarked (0-based) line.
                        if bookmarks.contains(&i) {
                            painter.circle_filled(
                                egui::pos2(lx, y + gutter_row_h * 0.5),
                                3.0,
                                accent,
                            );
                        }
                        // Diagnostic marker: a short severity-coloured bar on the
                        // gutter's LEFT edge (the bookmark dot's own lane is a
                        // circle at `lx`; a bar is a distinguishable shape at the
                        // same glance, and the two can legitimately coexist on
                        // one line). Errors win over warnings on a shared line.
                        if let Ok(m) = diag_marks.binary_search_by_key(&(i as u32), |(l, _)| *l) {
                            let sev = diag_marks[m].1;
                            // INFO resolves to the theme accent, exactly as the
                            // squiggle's own `match` does. Without this arm an
                            // info diagnostic fell through to `muted` and drew a
                            // GREY bar under a GREEN underline — the same
                            // diagnostic wearing two colours, which reads as two
                            // unrelated marks.
                            let col = match sev {
                                super::diagnostics_overlay::SEVERITY_ERROR => diag_err,
                                super::diagnostics_overlay::SEVERITY_WARNING => diag_warn,
                                super::diagnostics_overlay::SEVERITY_INFO => accent,
                                _ => muted,
                            };
                            painter.rect_filled(
                                egui::Rect::from_min_max(
                                    egui::pos2(lx - 3.0, y + gutter_row_h * 0.2),
                                    egui::pos2(lx - 0.5, y + gutter_row_h * 0.8),
                                ),
                                1.0,
                                col,
                            );
                        }
                        painter.text(
                            egui::pos2(rx, y),
                            egui::Align2::RIGHT_TOP,
                            (i + 1).to_string(),
                            nfont.clone(),
                            muted,
                        );
                    }
                });
        }

        // ---- Central editor surface ----
        // Phase 18 T18.2 — when the multi-note grid is enabled, render
        // every open tab as a movable / resizable pane via egui_tiles.
        // The single-pane code path below stays the default for users
        // who don't opt in.
        if self.grid_tree.is_some() {
            self.render_grid_central_panel(ctx, font.clone());
        } else {
            egui::CentralPanel::default().show(ctx, |ui| {
                // Folded read-only preview is a distinct surface (no live editing).
                if self.fold_view {
                    // The fold preview builds its own `ScrollArea` (id_salt
                    // "fold-scroll") inside `show_fold_view`, so a queued
                    // find-navigate / go-to-line scroll had nothing to consume it
                    // here. Bridge it through the persisted state the same way the
                    // rope paths do.
                    //
                    // The drag assist and the minimap deliberately do NOT run for
                    // this surface: the preview's `TextEdit` is `interactive(false)`
                    // (no selection to extend), and its content height is the
                    // projected galley's, which is not observable from outside the
                    // widget — publishing a guessed height would make the minimap
                    // lie, which is worse than leaving it alone.
                    let fold_scroll = super::drag_scroll::embedded_scroll_id(ui, "fold-scroll");
                    self.drive_embedded_scroll(ctx, fold_scroll);
                    publish_editor_mode(ctx, EditorMode::Fold);
                    self.show_fold_view(ui, font.clone(), ext.as_deref());
                    return;
                }

                // Read-only huge-file browse (KEYSTONE): a file past the
                // 256 MiB threshold opens read-only. Rendering it through the
                // viewport-culled RopeEditor — instead of laying out the whole
                // multi-hundred-MiB string in a TextEdit every frame — is the
                // O(viewport) browse path. Read-only ⇒ no editing regression;
                // the widget draws its own line numbers + viewport-scoped
                // syntax highlighting (F-030).
                if read_only {
                    let rope = self.tabs[active].doc.rope().clone();
                    // Exact content height: `RopeEditor` lays out through
                    // `ScrollArea::show_rows(ui, line_h, total_lines, ..)`, so the
                    // content is precisely `len_lines * gutter_row_h`. Captured
                    // before the buffer is moved into the widget.
                    let content_h = rope.len_lines() as f32 * gutter_row_h;
                    let scroll_id = super::drag_scroll::embedded_scroll_id(
                        ui,
                        super::drag_scroll::DEFAULT_SCROLL_SALT,
                    );
                    let focus_id = super::drag_scroll::rope_editor_focus_id(ui);
                    let viewport = ui.max_rect();
                    // Consume a queued find-navigate / go-to-line scroll BEFORE the
                    // widget renders — the read-only browse path owns no ScrollArea
                    // builder of its own, so this bridge is what makes those jumps
                    // move the viewport instead of silently doing nothing.
                    self.drive_embedded_scroll(ctx, scroll_id);
                    let mut buf = scribe_core::buffer::Buffer::Rope(rope);
                    let fg = ui_color(&self.theme, "foreground", Rgba::new(0xc8, 0xd6, 0xdc, 255));
                    scribe_render::RopeEditor::new(&mut buf, font.clone(), gutter_row_h)
                        .with_text_color(fg)
                        .with_gutter_color(muted)
                        .with_line_numbers(show_line_numbers)
                        .with_syntax(&self.hl, ext.clone())
                        .show(ui);
                    publish_editor_mode(ctx, EditorMode::ReadOnlyLarge);
                    self.finish_embedded_scroll(ctx, scroll_id, focus_id, viewport, content_h);
                    return;
                }

                // KEYSTONE — experimental owned rope editor (opt-in). Renders
                // normal files through the in-house editor (own caret /
                // selection / undo) instead of egui's TextEdit. The rope is
                // bridged from `text` each frame and written back after, so the
                // rest of the app (save, status bar, find) keeps seeing a
                // String. Default OFF — the egui path below stays canonical.
                // Wave-3: ALSO auto-engaged for buffers past the configured byte
                // threshold (default 16 MiB) so a multi-MiB file gets O(viewport)
                // rendering instead of the per-frame O(n) egui TextEdit.
                if use_rope_editor(
                    self.config.editor.experimental_rope_editor,
                    self.tabs[active].text.len(),
                    self.config.editor.rope_editor_auto_threshold_bytes,
                ) {
                    let fg = ui_color(&self.theme, "foreground", Rgba::new(0xc8, 0xd6, 0xdc, 255));
                    // KEYSTONE perf: the rope persists across frames in the tab.
                    // Build it once (O(n)) from `text`; thereafter the widget
                    // mutates it in place and we sync back to `text` ONLY when
                    // an edit actually changed content. `ropey` clones are O(1)
                    // (Arc-shared), so persistence costs no extra memory churn.
                    // Capture the disjoint fields the editor needs BEFORE the
                    // `&mut self.tabs` borrow (Wave-5 P1 snippets — gated on the
                    // config toggle; `&self.snippets` coexists with the tab's
                    // mutable rope borrow as a disjoint-field borrow).
                    let render_whitespace = self.config.editor.render_whitespace;
                    let snippets_enabled = self.config.editor.snippets_enabled;
                    // Scroll-surface handles for the shared assist. Taken from the
                    // SAME `ui` the widget is handed, before the `&mut self.tabs`
                    // borrow below, so the ids match the ones `RopeEditor` derives
                    // internally.
                    let scroll_id = super::drag_scroll::embedded_scroll_id(
                        ui,
                        super::drag_scroll::DEFAULT_SCROLL_SALT,
                    );
                    let focus_id = super::drag_scroll::rope_editor_focus_id(ui);
                    let viewport = ui.max_rect();
                    self.drive_embedded_scroll(ctx, scroll_id);
                    let snippets = &self.snippets;
                    let hl = &self.hl;
                    let tab = &mut self.tabs[active];
                    // Lazily (re)build the persistent rope from `text`. Done as a
                    // separate `is_none` check rather than `get_or_insert_with`
                    // so the closure does not capture `tab` while `rope_buf` is
                    // mutably borrowed (disjoint-field borrow).
                    if tab.rope_buf.is_none() {
                        tab.rope_buf = Some(scribe_core::buffer::Buffer::from_text(&tab.text));
                    }
                    let buf = tab.rope_buf.as_mut().expect("rope_buf set above");
                    let state = tab
                        .rope_state
                        .get_or_insert_with(scribe_render::RopeEditorState::new);
                    let mut editor =
                        scribe_render::RopeEditor::new(buf, font.clone(), gutter_row_h)
                            .with_text_color(fg)
                            .with_gutter_color(muted)
                            .with_line_numbers(show_line_numbers)
                            .with_render_whitespace(render_whitespace)
                            .with_syntax(hl, ext.clone());
                    if snippets_enabled {
                        editor = editor.with_snippets(snippets);
                    }
                    let (resp, clipboard) = editor.show_editable(ui, state);
                    // Sync `text` from the rope ONLY on a real content edit — the
                    // O(n) `to_string()` now runs on keystrokes, not every frame.
                    if resp.content_changed {
                        if let Some(rope) = tab.rope_buf.as_ref().and_then(|b| b.as_rope()) {
                            tab.text = rope.to_string();
                            tab.doc.mark_dirty();
                        }
                        // Wave-3: rope write-back bypasses set_text + the egui
                        // Response, so bump the gen counter here for parity.
                        tab.edit_gen = tab.edit_gen.wrapping_add(1);
                    }
                    // Exact content height for the minimap + the drag assist:
                    // `RopeEditor` lays out via `show_rows(ui, line_h,
                    // total_lines, ..)`, so it is `len_lines * gutter_row_h`.
                    // Read while `tab` is still borrowed, used after it drops.
                    let content_h = tab
                        .rope_buf
                        .as_ref()
                        .and_then(scribe_core::buffer::Buffer::as_rope)
                        .map_or(1.0, |r| r.len_lines() as f32 * gutter_row_h);
                    // The FIRST non-test consumer of `RopeEditorResponse::buffer_mode`:
                    // publish which buffer variant actually rendered so the status
                    // bar can tell the user they are in the degraded editor
                    // instead of silently swapping it in under them.
                    publish_editor_mode(ctx, EditorMode::from_buffer_mode(&resp.buffer_mode));
                    // Inline LSP diagnostics on the ROPE path. `paint_squiggle`
                    // used to be reachable ONLY from the `TextEdit` body below,
                    // so this path — the one a buffer is AUTO-promoted into past
                    // `rope_editor_auto_threshold_bytes`, i.e. exactly the files
                    // where an LSP is worth having — showed no squiggle and no
                    // gutter mark at all. The two integers in the status bar were
                    // the whole of it. The widget now reports the geometry of the
                    // rows it painted, so the same spans can be drawn here.
                    self.paint_rope_diagnostics(ui, &resp, active, viewport, accent, muted);
                    // Join the shared autoscroll + minimap-metrics implementation, the
                    // same call the read-only-large path makes above. Without this the
                    // editable rope path recorded no `scroll_metrics` (freezing the
                    // minimap on the last TextEdit frame) and drag-select autoscroll
                    // never ran here — the parity gap the frame-loop work exists to
                    // close. `content_h` is the rope's real laid-out height.
                    self.finish_embedded_scroll(ctx, scroll_id, focus_id, viewport, content_h);
                    if let Some(text) = clipboard {
                        // On Cut the selection is already removed from the buffer,
                        // so a clipboard failure here means the text is only
                        // undo-recoverable — log it rather than swallow silently.
                        match arboard::Clipboard::new() {
                            Ok(mut cb) => {
                                if let Err(e) = cb.set_text(text) {
                                    tracing::warn!(
                                        "clipboard write after cut/copy failed; text is \
                                         still undo-recoverable: {e}"
                                    );
                                }
                            }
                            Err(e) => tracing::warn!(
                                "could not open the clipboard for cut/copy; text is still \
                                 undo-recoverable: {e}"
                            ),
                        }
                    }
                    return;
                }

                // F-033 / F-034 from docs/audits/overlooked-surfaces-2026-05-29.md:
                // brace-delimited definition scopes for the breadcrumb bar (above
                // the editor) and the sticky-scroll headers (pinned at the
                // viewport top). P-05: memoized by `(edit_gen, doc_id)` so the
                // O(n) scan runs only on an edit or a tab switch, not every
                // frame. Still skipped for very large buffers inside the memo.
                let scopes = self.symbol_scopes_for_active();
                // Breadcrumb bar (F-033): the enclosing-symbol path of the
                // cursor line, outermost first (`mod foo › impl Bar › fn baz`).
                if !scopes.is_empty() {
                    let cursor_line0 = self
                        .last_cursor_line_col
                        .map(|(l, _)| l.saturating_sub(1))
                        .unwrap_or(0);
                    let crumbs = crate::editor_features::breadcrumb_at(&scopes, cursor_line0);
                    if !crumbs.is_empty() {
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            for (i, s) in crumbs.iter().enumerate() {
                                if i > 0 {
                                    ui.label(RichText::new("›").color(muted).small());
                                }
                                ui.label(RichText::new(&s.label).color(accent).small().monospace());
                            }
                        });
                        ui.separator();
                    }
                }

                // Tab / Shift+Tab. On a list item (note files, smart-lists on)
                // Tab indents / Shift+Tab outdents the whole item (P0-3). Off a
                // list, Tab inserts the configured spaces (when insert_spaces is
                // on) — the pre-existing behaviour is unchanged for code files.
                // Per-tab editor Id: salt the constant base with the tab's stable
                // `doc_id` so egui keys the TextEdit's `TextEditState` (selection,
                // caret, undo history) PER NOTE. With a single constant Id the
                // selection made in one note leaked into every other note (phantom
                // highlight) and undo history bled across tabs — the reported bug.
                let editor_id =
                    egui::Id::new("scr1b3-central-editor").with(self.tabs[active].doc_id);
                // `set_text` replaced this buffer from an EXTERNAL source (a
                // disk reload, a session restore, a plugin). egui keeps this
                // widget's undo history in ITS OWN memory under `editor_id`,
                // where `set_text` cannot reach — so drop it here, before the
                // widget renders. The `feed_state` inside `TextEdit::show`
                // then seeds a fresh first undo point from the NEW content,
                // making the next Ctrl+Z a no-op instead of a wholesale
                // overwrite with a document the user no longer has. Same
                // honest outcome `invalidate_rope_state` gives the rope path.
                if std::mem::take(&mut self.tabs[active].textedit_undo_stale) {
                    if let Some(mut st) = egui::TextEdit::load_state(ctx, editor_id) {
                        st.clear_undoer();
                        st.store(ctx, editor_id);
                    }
                }
                let editor_focused = ctx.memory(|m| m.has_focus(editor_id));
                if !read_only && editor_focused && ctx.input(|i| i.key_pressed(egui::Key::Tab)) {
                    let shift = ctx.input(|i| i.modifiers.shift);
                    let on_list = self.active_selection_on_list(ctx, editor_id, active);
                    if shift {
                        if on_list {
                            ctx.input_mut(|i| {
                                i.consume_key(egui::Modifiers::SHIFT, egui::Key::Tab);
                            });
                            self.indent_list_lines_active(ctx, editor_id, active, -1);
                        }
                    } else if on_list {
                        ctx.input_mut(|i| {
                            i.consume_key(egui::Modifiers::NONE, egui::Key::Tab);
                        });
                        self.indent_list_lines_active(ctx, editor_id, active, 1);
                    } else if self.config.editor.insert_spaces
                        && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Tab))
                    {
                        self.indent_with_spaces(ctx, editor_id, active);
                    }
                }

                // #107 — auto-indent on Enter: the new line keeps the current
                // line's leading whitespace. Only consume Enter when there IS
                // indentation to carry (otherwise let egui insert the plain
                // newline). Skipped while the completion popup owns Enter.
                if !read_only
                    && self.completion.is_none()
                    && ctx.memory(|m| m.has_focus(editor_id))
                    && ctx.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.is_none())
                    && self.auto_indent_newline(ctx, editor_id, active)
                {
                    ctx.input_mut(|i| {
                        i.consume_key(egui::Modifiers::NONE, egui::Key::Enter);
                    });
                }

                // P2-2 — auto-pair (default-OFF). Runs before the editor sees
                // the typed char so it can consume/rewrite the insertion.
                if !read_only && editor_focused {
                    self.handle_auto_pair(ctx, editor_id, active);
                }

                // Note-usability caret CHORDS (Ctrl+Enter / Ctrl+B / Ctrl+I /
                // Ctrl+` / Ctrl+Shift+X). Route to the same pending_* the palette
                // uses; consume the key so egui's TextEdit does not also see it.
                if !read_only && editor_focused {
                    ctx.input_mut(|i| {
                        use egui::{Key, Modifiers};
                        let ctrl = Modifiers::COMMAND;
                        let ctrl_shift = Modifiers::COMMAND | Modifiers::SHIFT;
                        if i.consume_key(ctrl, Key::Enter) {
                            self.pending_toggle_task = true;
                        }
                        if i.consume_key(ctrl, Key::B) {
                            self.pending_wrap_marker = Some("**");
                        }
                        if i.consume_key(ctrl, Key::I) {
                            self.pending_wrap_marker = Some("*");
                        }
                        if i.consume_key(ctrl, Key::Backtick) {
                            self.pending_wrap_marker = Some("`");
                        }
                        if i.consume_key(ctrl_shift, Key::X) {
                            self.pending_wrap_marker = Some("~~");
                        }
                    });

                    // Ctrl/Cmd+V with an IMAGE on the clipboard saves it into the
                    // vault's attachments folder and inserts the markdown link —
                    // the same `paste_image_attachment` the palette runs, now on
                    // the key people actually press.
                    //
                    // The hook is the V key RELEASE, not `consume_key(COMMAND, V)`,
                    // and that is NOT a stylistic choice: `egui_winit`'s
                    // `on_keyboard_input` special-cases the paste chord on
                    // key-DOWN and `return`s immediately after pushing
                    // `Event::Paste` — so `Event::Key { key: V, pressed: true }`
                    // NEVER reaches us, and when the clipboard holds no TEXT it
                    // pushes nothing at all. On an image-only clipboard the key
                    // release is therefore the ONLY observable event of the whole
                    // gesture; a `consume_key` here would be permanently dead
                    // code. Shift is excluded so Ctrl+Shift+V keeps its binding.
                    let now = ctx.cumulative_pass_nr();
                    let (text_pasted, paste_released) = ctx.input(|i| {
                        (
                            i.events
                                .iter()
                                .any(|e| matches!(e, egui::Event::Paste(t) if !t.is_empty())),
                            i.events.iter().any(|e| {
                                matches!(
                                    e,
                                    egui::Event::Key {
                                        key: egui::Key::V,
                                        pressed: false,
                                        modifiers,
                                        ..
                                    } if modifiers.command && !modifiers.shift && !modifiers.alt
                                )
                            }),
                        )
                    });
                    if text_pasted {
                        ctx.data_mut(|d| d.insert_temp(last_text_paste_frame_id(), now));
                    }
                    if paste_released {
                        // A clipboard carrying BOTH text and a bitmap pastes the
                        // TEXT: that already happened on the key-down frame, so
                        // the release must not also drop an image in.
                        let text_won = ctx
                            .data_mut(|d| {
                                let id = last_text_paste_frame_id();
                                let v = d.get_temp::<u64>(id);
                                d.remove::<u64>(id);
                                v
                            })
                            .is_some_and(|f| {
                                now.saturating_sub(f) <= PASTE_IMAGE_TEXT_GRACE_FRAMES
                            });
                        if !text_won && self.config.notes.vault_dir.is_some() {
                            self.paste_image_attachment();
                        }
                    }

                    // P1-2 — smart paste: rewrite a clipboard-URL Paste event that
                    // lands over a non-empty selection into `[selection](url)`, so
                    // egui's native paste replaces the selection with a md link.
                    if self.config.editor.paste_url_as_link && self.note_file_active(active) {
                        if let Some(sel) = self.active_selection_text(ctx, editor_id, active) {
                            if !sel.is_empty() {
                                ctx.input_mut(|i| {
                                    for ev in i.events.iter_mut() {
                                        if let egui::Event::Paste(txt) = ev {
                                            if scribe_core::md_ops::looks_like_url(txt) {
                                                *txt = scribe_core::md_ops::make_markdown_link(
                                                    &sel, txt,
                                                );
                                            }
                                        }
                                    }
                                });
                            }
                        }
                    }
                }

                // Caret commands from the keyboard (`act.*`) or the command
                // palette (`self.pending_*`). They need the egui TextEditState,
                // so they run here — after the editor stored its state this
                // frame — and `store` takes effect next frame.
                if !read_only {
                    // The keyboard chord and the `pending_*` latch are the same
                    // request; fold the chord into the latch so ONE drain applies
                    // every caret op.
                    if act.jump_bracket {
                        self.pending_jump_bracket = true;
                    }
                    // A right-click context-menu command stashed in ctx-data on a
                    // previous frame (it couldn't run inside the editor closure
                    // while the highlighter borrowed `self`) — dispatch it now so
                    // its pending_* flag is applied by the drain just below.
                    if let Some(cmd) = ctx.data_mut(|d| {
                        let id = editor_ctx_cmd_id();
                        let v = d.get_temp::<crate::app::commands::BuiltinCommand>(id);
                        d.remove::<crate::app::commands::BuiltinCommand>(id);
                        v
                    }) {
                        self.execute_builtin(cmd);
                    }
                    // Note-usability caret ops (palette + chord) — P0-1 / P0-4 /
                    // P1-4 / P2-1. Shared with the grid pane path so the two
                    // surfaces can never drift into two different drain sets (the
                    // grid drained NONE of them, so every one of these latched
                    // forever in split view).
                    self.apply_pending_caret_ops(ctx, editor_id, active);
                }

                // #78 — misspellings for the active buffer, computed (memoized)
                // BEFORE the partial borrows below so the owned Vec can move into
                // the editor closure and drive the red underline painter.
                let misspellings = self.misspellings_for_active();
                // LSP diagnostics resolved onto byte spans, owned so they can
                // move into the editor closure alongside `misspellings` and
                // drive the squiggle + hover overlay. Empty (and free) when the
                // language server has published nothing.
                let diag_spans = self.diagnostic_spans_for_active(active);
                // Wave-5: compute all find matches once (needs &self) so the
                // highlight-all overlay can paint every match, not just the
                // navigated one. Empty when the find bar is closed.
                let find_hits: Vec<scribe_core::search::Match> = if self.find_open {
                    self.find_matches_active()
                } else {
                    Vec::new()
                };
                let find_cur = self.find_match_idx;
                // Scope the layouter (which borrows `self.hl`) so it drops before
                // the `&mut self` completion calls below.
                let mut new_gutter: Vec<f32> = Vec::new();
                // F-034: a clicked sticky header records its target line here;
                // it is applied to `pending_scroll` after the hl borrow drops.
                let mut sticky_jump: Option<usize> = None;
                // Captured out of the editor closure so the post-render
                // drag-scroll + caret-scroll-off assists (which need `&mut self`)
                // can read the editor's screen-space viewport once the `hl`
                // borrow has dropped. Assigned unconditionally inside the block.
                let editor_vp: egui::Rect;

                // ---- P2 multi-cursor gestures + edit interception ----
                // Replay edit keys at all carets (and handle Esc / Ctrl+D) BEFORE
                // the TextEdit consumes this frame's events. Gesture geometry that
                // needs the galley (Ctrl/Cmd+click add-caret, Alt+drag column
                // select) is resolved after the closure lays it out.
                // P1-A: bind the app-global multi-cursor state to the active tab.
                // The editor is keyed PER TAB (doc_id-salted id) and switching tabs
                // auto-focuses the new editor, so stale carets from the previous tab
                // must be dropped BEFORE any edit interception or secondary-caret
                // paint this frame — otherwise the next keystroke silently edits the
                // WRONG document at clamped offsets.
                self.mc_reconcile_owner(active);
                // Esc collapses multi-cursor to a single caret. Focus-independent:
                // being in multi-cursor mode is enough signal (egui can transiently
                // drop editor focus between an intercepted edit and the next key),
                // and it must run whether or not the editor currently holds focus —
                // but it does NOT steal Escape while an overlay is open (P2-E), so
                // an open find bar / palette / settings still receives it.
                self.mc_collapse_on_escape(ctx, overlay_open);
                let mc_focus = !read_only && !overlay_open && editor_focused;
                if mc_focus {
                    self.handle_multi_cursor_keys(ctx, editor_id, active);
                }
                let (mc_primary_pressed, mc_cmd, mc_alt, mc_primary_down, mc_ptr) =
                    ctx.input(|i| {
                        (
                            i.pointer.primary_pressed(),
                            i.modifiers.command,
                            i.modifiers.alt,
                            i.pointer.primary_down(),
                            i.pointer.interact_pos(),
                        )
                    });
                // A plain click (no Ctrl/Alt) collapses multi-cursor mode.
                if mc_focus
                    && mc_primary_pressed
                    && !mc_cmd
                    && !mc_alt
                    && self.multi_cursor.is_active()
                {
                    self.multi_cursor.clear();
                }
                // Ctrl/Cmd+click adds (or toggles off) a caret at the clicked
                // position, resolved via a galley hit-test in the closure. The
                // pre-click primary head is remembered so it can be restored — the
                // click becomes a NEW secondary and the existing primary stays.
                let mc_ctrl_click =
                    mc_focus && mc_primary_pressed && mc_cmd && !mc_alt && mc_ptr.is_some();
                let mc_prev_primary: Option<usize> = if mc_ctrl_click {
                    super::multi_cursor_glue::mc_load_primary_head(ctx, editor_id)
                } else {
                    None
                };
                // Alt+press starts a column-selection anchor; Alt+drag extends it.
                let mc_alt_press = mc_focus && mc_alt && mc_primary_pressed && mc_ptr.is_some();
                let mc_alt_drag = mc_focus
                    && mc_alt
                    && mc_primary_down
                    && !mc_primary_pressed
                    && self.column_anchor.is_some()
                    && mc_ptr.is_some();
                if mc_focus && !mc_primary_down {
                    self.column_anchor = None;
                }
                // Snapshot the secondaries for painting inside the closure.
                let mc_secondaries: Vec<crate::multi_cursor::Caret> =
                    self.multi_cursor.secondaries().to_vec();
                // Galley-resolved gesture outputs (need pos->char hit testing).
                let mut mc_alt_anchor_idx: Option<usize> = None;
                let mut mc_alt_head_idx: Option<usize> = None;
                let mut mc_ctrl_click_idx: Option<usize> = None;

                let anchor: Option<(egui::Pos2, usize)> = {
                    let hl = &self.hl;
                    let ext_ref = ext.as_deref();
                    let layout_fg =
                        ui_color(&self.theme, "foreground", Rgba::new(0xc8, 0xd6, 0xdc, 255));
                    // #D — the themeable URL colour: the chrome theme's `[syntax]`
                    // `url` token, falling back to the `accent` UI colour so every
                    // theme colours links coherently without extra config.
                    let detect_links = self.config.editor.detect_links;
                    let url_color = scribe_render::color32(self.theme.syntax_color(
                        "url",
                        self.theme.ui("accent", Rgba::new(0x4c, 0xc2, 0xff, 255)),
                    ));
                    let mut layouter = make_layouter(
                        hl,
                        &self.hl_cache,
                        &self.hl_galley_cache,
                        &self.hl_inc_cache,
                        ext_ref,
                        font.clone(),
                        line_height,
                        word_wrap,
                        layout_fg,
                        url_color,
                        detect_links,
                    );
                    // Per-tab scroll: salt the ScrollArea Id with the tab's stable
                    // `doc_id` so egui keeps each note's scroll offset independently.
                    // Without a salt every tab shared one offset, so switching notes
                    // jumped the viewport to the previous note's position.
                    let mut sa = (if word_wrap {
                        egui::ScrollArea::vertical()
                    } else {
                        egui::ScrollArea::both()
                    })
                    .id_salt(("scr1b3-editor-scroll", self.tabs[active].doc_id));
                    // ROOT CAUSE of "drag-select can't scroll the view" (P0-2).
                    // egui's own TextEdit cursor-follow calls
                    // `ui.scroll_to_rect(primary_cursor_rect, None)` on
                    // `response.changed() || selection_changed`
                    // (`text_edit/builder.rs`), which during a drag-select is
                    // true nearly every frame. With the ScrollArea's default
                    // `animated == true` that installs a persistent
                    // `state.offset_target`, and `Prepared::begin` LERPS
                    // `state.offset` toward it (`scroll_area.rs`) AFTER our
                    // `vertical_scroll_offset` write below has landed but BEFORE
                    // `add_contents` lays anything out. So every value
                    // `drag_scroll_assist` computed was overwritten before it
                    // could take effect, and because `scroll_metrics` is then
                    // recorded from the CLOBBERED offset the assist re-based off
                    // egui's value each frame and could never accumulate.
                    //
                    // With `animated(false)` the same cursor-follow instead
                    // writes `state.offset[d] = target_offset` directly and
                    // installs NO `offset_target`, so nothing survives into the
                    // next frame to clobber our write.
                    //
                    // Gated STRICTLY on an active drag-select: turning animation
                    // off unconditionally would also kill the user's
                    // `animate_jumps` easing for goto-line / find-navigation.
                    let drag_selecting = ctx.input(|i| i.pointer.primary_down())
                        && ctx.memory(|m| m.has_focus(editor_id));
                    if drag_selecting {
                        sa = sa.animated(false);
                    }
                    if let Some(off) = self.pending_scroll.take() {
                        sa = sa.vertical_scroll_offset(off);
                    }
                    // Wave-6 scrollbar style.
                    sa = match self.config.editor.scrollbar_style {
                        scribe_core::config::ScrollbarStyle::Hidden => sa.scroll_bar_visibility(
                            egui::scroll_area::ScrollBarVisibility::AlwaysHidden,
                        ),
                        scribe_core::config::ScrollbarStyle::Thin
                        | scribe_core::config::ScrollbarStyle::Auto => sa.scroll_bar_visibility(
                            egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded,
                        ),
                    };
                    let thin_scrollbar = self.config.editor.scrollbar_style
                        == scribe_core::config::ScrollbarStyle::Thin;
                    let mut a: Option<(egui::Pos2, usize)> = None;
                    // Viewport height captured before the TextEdit consumes the
                    // inner ui, for the P1-3 scroll-past-end trailing pad below.
                    let scroll_past_end = self.config.scroll.scroll_past_end;
                    let sa_out = sa.show(ui, |ui| {
                        let vp_h = ui.available_height();
                        if thin_scrollbar {
                            ui.style_mut().spacing.scroll.bar_width = 6.0;
                        }
                        let dw = if word_wrap {
                            ui.available_width()
                        } else {
                            f32::INFINITY
                        };
                        let editor = egui::TextEdit::multiline(&mut self.tabs[active].text)
                            .id(editor_id)
                            .code_editor()
                            .desired_width(dw)
                            .desired_rows(30)
                            .lock_focus(true)
                            .interactive(!read_only)
                            .layouter(&mut layouter);
                        let out = editor.show(ui);
                        // ---- P2 multi-cursor — galley-resolved gestures + paint ----
                        // Hit-test the pointer to a char index against the laid-out
                        // galley (deterministic geometry) for the Ctrl/Cmd+click and
                        // Alt+drag gestures captured before the closure.
                        if let Some(p) = mc_ptr {
                            let local = p - out.galley_pos;
                            if mc_alt_press {
                                mc_alt_anchor_idx = Some(out.galley.cursor_from_pos(local).index);
                            } else if mc_alt_drag {
                                mc_alt_head_idx = Some(out.galley.cursor_from_pos(local).index);
                            } else if mc_ctrl_click {
                                mc_ctrl_click_idx = Some(out.galley.cursor_from_pos(local).index);
                            }
                        }
                        // Paint each secondary caret (and its single-row selection
                        // band) so multi-cursor renders distinctly from egui's
                        // primary caret.
                        if !mc_secondaries.is_empty() {
                            let painter = ui.painter();
                            let gp = out.galley_pos;
                            for c in &mc_secondaries {
                                if !c.is_empty() {
                                    let r = c.range();
                                    let rs = out
                                        .galley
                                        .pos_from_cursor(egui::text::CCursor::new(r.start));
                                    let re =
                                        out.galley.pos_from_cursor(egui::text::CCursor::new(r.end));
                                    if (rs.min.y - re.min.y).abs() < 0.5 {
                                        let sel = egui::Rect::from_min_max(
                                            gp + egui::vec2(rs.min.x, rs.min.y),
                                            gp + egui::vec2(re.max.x, re.max.y),
                                        );
                                        painter.rect_filled(sel, 0.0, accent.linear_multiply(0.30));
                                    }
                                }
                                let cr =
                                    out.galley.pos_from_cursor(egui::text::CCursor::new(c.head));
                                painter.line_segment(
                                    [
                                        gp + egui::vec2(cr.min.x, cr.min.y),
                                        gp + egui::vec2(cr.min.x, cr.max.y),
                                    ],
                                    egui::Stroke::new(1.5, accent),
                                );
                            }
                        }
                        // Right-click context menu — makes the note-usability actions
                        // DISCOVERABLE without knowing the command palette / a chord:
                        // the standard clipboard actions plus the markdown formatting
                        // commands (bold/italic/code/strike, task toggle, table format,
                        // case, date-time). Mutating actions are hidden in a read-only
                        // buffer. Each routes through the same `execute_builtin` the
                        // palette uses, so behaviour is identical.
                        out.response.context_menu(|ui| {
                            use crate::app::commands::BuiltinCommand as B;
                            ui.set_min_width(200.0);
                            // The command is stashed in ctx-data (not run here) —
                            // `self` is borrowed immutably by the highlight layouter
                            // in this ScrollArea closure, so the pick is drained +
                            // dispatched below where `self` is mutable again.
                            let pick = |ui: &mut egui::Ui, label: &str, cmd: B| {
                                if ui.button(label).clicked() {
                                    ui.ctx()
                                        .data_mut(|d| d.insert_temp(editor_ctx_cmd_id(), cmd));
                                    ui.close_menu();
                                }
                            };
                            pick(ui, "Cut", B::Cut);
                            pick(ui, "Copy", B::Copy);
                            if !read_only {
                                pick(ui, "Paste", B::Paste);
                                ui.separator();
                                pick(ui, "Bold", B::ToggleBold);
                                pick(ui, "Italic", B::ToggleItalic);
                                pick(ui, "Inline code", B::ToggleInlineCode);
                                pick(ui, "Strikethrough", B::ToggleStrikethrough);
                                ui.separator();
                                pick(ui, "Toggle task  [ ] / [x]", B::ToggleTaskCheckbox);
                                pick(ui, "Format table", B::FormatTable);
                                pick(ui, "Title Case selection", B::TitlecaseSelection);
                                pick(ui, "Insert date / time", B::InsertDateTime);
                            }
                        });
                        // Wave-3: the egui in-place edit happened inside show();
                        // `.changed()` is true exactly on the edited frame, so this
                        // is the ONLY hook for the default editor's text mutation.
                        //
                        // It must therefore run the FULL invalidation, not just the
                        // gen bump. `edit_gen` refreshes the gen-keyed minimap and
                        // spell caches; it does NOT clear `rope_buf`. A bare bump
                        // left the pre-switch rope alive, and because the rope path
                        // rebuilds only when `rope_buf.is_none()`, re-enabling the
                        // rope editor wrote that stale rope back over `text` and
                        // silently destroyed the user's typing. Same writer duty as
                        // `set_text` and the two in-place splicers.
                        if out.response.changed() {
                            self.tabs[active].note_text_mutated();
                        }
                        // P1-3 scroll-past-end: pad blank space below the last
                        // line so it can rest at a comfortable height instead of
                        // being pinned to the viewport bottom (VS Code
                        // `scrollBeyondLastLine`). Grows the ScrollArea content so
                        // the extra offset is real scroll range, not a caret jump.
                        if scroll_past_end {
                            ui.add_space(vp_h * 0.6);
                        }
                        // #D — clickable-URL overlay pass. The persistent colour +
                        // underline is painted by the syntax layer (highlight_job);
                        // here we add the hover affordance (P1) and the click-open
                        // (P0). Detect http(s):// URLs as GLOBAL byte ranges, map the
                        // pointer to a char via the galley, and on Ctrl/Cmd-click
                        // over a URL open it in the OS browser — scheme-allow-listed
                        // to http/https (a URL in a file is untrusted data; open only
                        // on an explicit modifier-click, never on render). Bounded by
                        // a buffer-size cap like the other per-frame overlays.
                        //
                        // The SAME pointer->byte hit-test also resolves an
                        // in-editor `[[wiki-link]]`: `extract_wikilinks` already
                        // reports ABSOLUTE byte spans, so the link arm is a
                        // sibling of the URL arm rather than a second scan. The
                        // two are mutually exclusive at a given byte (a URL is
                        // never inside a link target), so URL wins the `find` and
                        // the link arm is the `else`.
                        //
                        // Nothing here is needed unless the pointer is actually
                        // over the editor, so the hover test comes FIRST: both
                        // scans are O(buffer) and would otherwise run every frame
                        // of every session, for a hit-test that cannot happen.
                        let link_hover = ui
                            .input(|i| i.pointer.hover_pos())
                            .filter(|p| out.response.rect.contains(*p));
                        if let Some(p) =
                            link_hover.filter(|_| self.tabs[active].text.len() <= 1_000_000)
                        {
                            let text_ref = &self.tabs[active].text;
                            let mut url_spans: Vec<(usize, usize, &str)> = Vec::new();
                            if self.config.editor.detect_links {
                                let mut base = 0usize;
                                for line in text_ref.split_inclusive('\n') {
                                    for r in scribe_core::url_scan::detect_urls(line) {
                                        url_spans.push((base + r.start, base + r.end, &line[r]));
                                    }
                                    base += line.len();
                                }
                            }
                            // A wiki-link is a NOTES affordance, not a URL one:
                            // it is live whenever a vault is configured (there is
                            // nowhere to resolve a target without one), which is
                            // exactly the precondition `open_or_create_wikilink`
                            // enforces. An empty target (`[[#Heading]]`, an
                            // intra-note anchor) names no note and is skipped.
                            let link_spans: Vec<scribe_core::notes::wikilink::WikiLink> =
                                if self.config.notes.vault_dir.is_some() {
                                    scribe_core::notes::wikilink::extract_wikilinks(text_ref)
                                        .into_iter()
                                        .filter(|l| !l.target.is_empty())
                                        .collect()
                                } else {
                                    Vec::new()
                                };
                            if !url_spans.is_empty() || !link_spans.is_empty() {
                                let rel = p - out.galley_pos;
                                let ci = out.galley.cursor_from_pos(rel).index;
                                let byte = char_to_byte(text_ref, ci);
                                if let Some(&(_, _, url)) =
                                    url_spans.iter().find(|(s, e, _)| byte >= *s && byte < *e)
                                {
                                    let cmd = ui.input(|i| i.modifiers.command);
                                    // P1 — pointer affordance when the follow
                                    // modifier is held.
                                    if cmd {
                                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                                    }
                                    // P1 — anti-phishing hover preview of the
                                    // destination (so the user sees where a
                                    // link goes before opening it).
                                    egui::show_tooltip_at_pointer(
                                        ui.ctx(),
                                        out.response.layer_id,
                                        egui::Id::new("scr1b3-url-tooltip"),
                                        |ui| {
                                            ui.label(if cmd {
                                                url.to_string()
                                            } else {
                                                format!("{url}  —  Ctrl+click to open")
                                            });
                                        },
                                    );
                                    // P0 — open only on explicit modifier-click,
                                    // and only for an http/https scheme.
                                    if cmd
                                        && ui.input(|i| i.pointer.primary_clicked())
                                        && scribe_core::url_scan::is_clickable_url(url)
                                    {
                                        ui.ctx().open_url(egui::OpenUrl::new_tab(url.to_string()));
                                    }
                                } else if let Some(link) =
                                    link_spans.iter().find(|l| byte >= l.start && byte < l.end)
                                {
                                    let cmd = ui.input(|i| i.modifiers.command);
                                    if cmd {
                                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                                    }
                                    // The hover names the TARGET, not the
                                    // label — an aliased link must not
                                    // hide where it goes (same contract as
                                    // the notes pane's link list).
                                    egui::show_tooltip_at_pointer(
                                        ui.ctx(),
                                        out.response.layer_id,
                                        egui::Id::new("scr1b3-wikilink-tooltip"),
                                        |ui| {
                                            ui.label(if cmd {
                                                format!("[[{}]]", link.target)
                                            } else {
                                                format!(
                                                    "[[{}]]  —  Ctrl+click to open",
                                                    link.target
                                                )
                                            });
                                        },
                                    );
                                    // Stash, don't follow: `self` is
                                    // borrowed here. Drained at the top of
                                    // the next frame.
                                    if cmd && ui.input(|i| i.pointer.primary_clicked()) {
                                        ui.ctx().data_mut(|d| {
                                            d.insert_temp(
                                                editor_wikilink_follow_id(),
                                                link.target.clone(),
                                            );
                                        });
                                    }
                                }
                            }
                        }
                        // #78 — paint a red squiggle under each misspelling. Map
                        // the byte span to galley cursor rects and draw a wavy
                        // underline along the word's baseline. Painted on the
                        // editor's own layer so it scrolls with the text.
                        if !misspellings.is_empty() {
                            let text_ref = &self.tabs[active].text;
                            let painter = ui.painter();
                            let red = Color32::from_rgb(0xe5, 0x3e, 0x3e);
                            for m in &misspellings {
                                let c0 = byte_to_char_index(text_ref, m.start);
                                let c1 = byte_to_char_index(text_ref, m.end);
                                let r0 = out.galley.pos_from_cursor(egui::text::CCursor::new(c0));
                                let r1 = out.galley.pos_from_cursor(egui::text::CCursor::new(c1));
                                // Same row only (words don't wrap); skip if the
                                // span spans rows (rare) to avoid a stray line.
                                if (r0.min.y - r1.min.y).abs() > 0.5 {
                                    continue;
                                }
                                let y = out.galley_pos.y + r0.max.y;
                                let x0 = out.galley_pos.x + r0.min.x;
                                let x1 = out.galley_pos.x + r1.min.x;
                                paint_squiggle(painter, x0, x1, y, red);
                            }
                        }
                        // ---- Inline LSP diagnostics ----
                        //
                        // The client has drained `publishDiagnostics` since it
                        // was written, but the ONLY thing rendered from them was
                        // a pair of integers in the status bar ("3e / 7") — no
                        // squiggle, no gutter mark, no message. Two integers do
                        // not tell you which line is wrong or why. Paint a
                        // severity-coloured squiggle under each diagnostic's
                        // range, and describe it on hover.
                        //
                        // Painted per galley ROW rather than per span, so a
                        // multi-line diagnostic (an unclosed delimiter, a type
                        // error spanning a match arm) underlines every line it
                        // covers instead of being dropped for spanning rows —
                        // and so it follows soft wrapping.
                        if !diag_spans.is_empty() {
                            let text_ref = &self.tabs[active].text;
                            let painter = ui.painter();
                            let err_c =
                                ui_color(&self.theme, "error", Rgba::new(0xe5, 0x3e, 0x3e, 255));
                            let warn_c =
                                ui_color(&self.theme, "warning", Rgba::new(0xf2, 0xb3, 0x3d, 255));
                            let info_c = accent;
                            let origin = out.galley_pos.to_vec2();
                            // Rects actually painted, so the hover test is
                            // "is the pointer over a squiggle", not "is it
                            // somewhere on a line that has one".
                            let mut painted: Vec<egui::Rect> = Vec::new();
                            for span in &diag_spans {
                                let color = match span.severity {
                                    crate::app::diagnostics_overlay::SEVERITY_ERROR => err_c,
                                    crate::app::diagnostics_overlay::SEVERITY_WARNING => warn_c,
                                    crate::app::diagnostics_overlay::SEVERITY_INFO => info_c,
                                    _ => muted,
                                };
                                let c0 = byte_to_char_index(text_ref, span.start);
                                let c1 = byte_to_char_index(text_ref, span.end);
                                let mut row_start = 0usize;
                                for prow in &out.galley.rows {
                                    let row_end = row_start + prow.char_count_including_newline();
                                    let s = c0.max(row_start);
                                    let e = c1.min(row_end);
                                    if s < e {
                                        let rx = origin.x + prow.pos.x;
                                        let x0 = rx + prow.row.x_offset(s - row_start);
                                        let x1 = rx + prow.row.x_offset(e - row_start);
                                        let top = origin.y + prow.pos.y;
                                        let bot = top + prow.row.size.y;
                                        paint_squiggle(painter, x0, x1, bot, color);
                                        painted.push(egui::Rect::from_min_max(
                                            egui::pos2(x0, top),
                                            egui::pos2(x1, bot),
                                        ));
                                    }
                                    row_start = row_end;
                                    if row_start >= c1 {
                                        break;
                                    }
                                }
                            }
                            // Hover: name the problem. Resolved through the
                            // galley so the message belongs to the character
                            // under the pointer, not merely to the same line.
                            if let Some(p) = ui.ctx().pointer_hover_pos() {
                                if painted.iter().any(|r| r.contains(p)) {
                                    let cursor = out.galley.cursor_from_pos(p - out.galley_pos);
                                    let byte = char_to_byte(text_ref, cursor.index);
                                    if let Some(text) = crate::app::diagnostics_overlay::hover_text(
                                        &diag_spans,
                                        byte,
                                    ) {
                                        egui::show_tooltip_at_pointer(
                                            ui.ctx(),
                                            out.response.layer_id,
                                            egui::Id::new("scr1b3-diagnostic-tooltip"),
                                            |ui| {
                                                ui.label(text);
                                            },
                                        );
                                    }
                                }
                            }
                        }
                        // Wave-5: incremental highlight-all — paint a translucent
                        // accent wash behind EVERY live find match (the current
                        // match stronger). Same galley-rect mapping as the
                        // squiggle painter; low alpha keeps the glyph legible.
                        if !find_hits.is_empty() {
                            let text_ref = &self.tabs[active].text;
                            let painter = ui.painter();
                            let hl_fill = accent.gamma_multiply(0.28);
                            let cur_fill = accent.gamma_multiply(0.5);
                            for (idx, m) in find_hits.iter().enumerate() {
                                let c0 = byte_to_char_index(text_ref, m.start);
                                let c1 = byte_to_char_index(text_ref, m.end);
                                let r0 = out.galley.pos_from_cursor(egui::text::CCursor::new(c0));
                                let r1 = out.galley.pos_from_cursor(egui::text::CCursor::new(c1));
                                if (r0.min.y - r1.min.y).abs() > 0.5 {
                                    continue;
                                }
                                let top = out.galley_pos.y + r0.min.y;
                                let bot = out.galley_pos.y + r0.max.y;
                                let x0 = out.galley_pos.x + r0.min.x;
                                let x1 = out.galley_pos.x + r1.min.x;
                                let fill = if idx == find_cur { cur_fill } else { hl_fill };
                                painter.rect_filled(
                                    egui::Rect::from_min_max(
                                        egui::pos2(x0, top),
                                        egui::pos2(x1, bot),
                                    ),
                                    2.0,
                                    fill,
                                );
                            }
                        }
                        // #28 — render-whitespace overlay for the DEFAULT egui
                        // TextEdit path. Previously the `·`/`→` markers only drew
                        // in the experimental rope editor, so the toggle did
                        // nothing in the default editor. Walk the laid-out galley
                        // glyphs (so the markers follow wrapping AND the chosen
                        // monospace face) and paint a faint `·` centred in each
                        // space cell, `→` in each tab cell. Pure overlay — the
                        // buffer text and the syntax spans are untouched.
                        if self.config.editor.render_whitespace {
                            let painter = ui.painter();
                            let ws_font =
                                FontId::monospace(self.config.fonts.clamped_editor_size());
                            let ws_color = muted.gamma_multiply(0.7);
                            let origin = out.galley_pos.to_vec2();
                            for row in &out.galley.rows {
                                let row_off = origin + row.pos.to_vec2();
                                let cy = row_off.y + row.size.y * 0.5;
                                for g in &row.glyphs {
                                    let marker = match g.chr {
                                        ' ' => "·",
                                        '\t' => "→",
                                        _ => continue,
                                    };
                                    let cx = row_off.x + g.pos.x + g.advance_width * 0.5;
                                    painter.text(
                                        egui::pos2(cx, cy),
                                        egui::Align2::CENTER_CENTER,
                                        marker,
                                        ws_font.clone(),
                                        ws_color,
                                    );
                                }
                            }
                        }
                        // Wave-6 indent guides: faint vertical lines at each
                        // tab_width column, drawn by walking the laid-out galley so
                        // they follow the chosen monospace face + wrapping.
                        if self.config.editor.indent_guides {
                            let painter = ui.painter();
                            let origin = out.galley_pos.to_vec2();
                            let cell_w = out
                                .galley
                                .rows
                                .iter()
                                .flat_map(|r| r.glyphs.iter())
                                .map(|g| g.advance_width)
                                .find(|w| *w > 0.0)
                                .unwrap_or(self.config.fonts.clamped_editor_size() * 0.6);
                            let step = cell_w * self.config.editor.tab_width as f32;
                            if step > 1.0 {
                                let guide = Color32::from_rgba_unmultiplied(
                                    muted.r(),
                                    muted.g(),
                                    muted.b(),
                                    40,
                                );
                                for row in &out.galley.rows {
                                    let row_off = origin + row.pos.to_vec2();
                                    let lead: f32 = row
                                        .glyphs
                                        .iter()
                                        .take_while(|g| g.chr == ' ' || g.chr == '\t')
                                        .map(|g| g.advance_width)
                                        .sum();
                                    let top = row_off.y;
                                    let bot = row_off.y + row.size.y;
                                    let mut x = row_off.x + step;
                                    while x <= row_off.x + lead + 0.5 {
                                        painter.line_segment(
                                            [egui::pos2(x, top), egui::pos2(x, bot)],
                                            egui::Stroke::new(1.0, guide),
                                        );
                                        x += step;
                                    }
                                }
                            }
                        }
                        // Trailing-whitespace tint: faintly mark the trailing
                        // space/tab run on each line (distinct from
                        // render_whitespace, which marks ALL whitespace).
                        if self.config.editor.highlight_trailing_whitespace {
                            let painter = ui.painter();
                            let tint = ui_color(
                                &self.theme,
                                "trailing_whitespace",
                                Rgba::new(0xd0, 0x6e, 0x6e, 28),
                            );
                            let origin = out.galley_pos.to_vec2();
                            for row in &out.galley.rows {
                                let row_off = origin + row.pos.to_vec2();
                                let mut run_start: Option<f32> = None;
                                let mut run_end = 0.0;
                                for g in &row.glyphs {
                                    if g.chr == ' ' || g.chr == '\t' {
                                        if run_start.is_none() {
                                            run_start = Some(row_off.x + g.pos.x);
                                        }
                                        run_end = row_off.x + g.pos.x + g.advance_width;
                                    } else {
                                        run_start = None;
                                    }
                                }
                                if let Some(sx) = run_start {
                                    painter.rect_filled(
                                        egui::Rect::from_min_max(
                                            egui::pos2(sx, row_off.y),
                                            egui::pos2(run_end, row_off.y + row.size.y),
                                        ),
                                        0.0,
                                        tint,
                                    );
                                }
                            }
                        }
                        // Column rulers: thin vertical guides at the configured
                        // 1-based columns (monospace; most meaningful without wrap).
                        if !self.config.editor.rulers.is_empty() {
                            let painter = ui.painter();
                            let cell_w = out
                                .galley
                                .rows
                                .iter()
                                .flat_map(|r| r.glyphs.iter())
                                .map(|g| g.advance_width)
                                .find(|w| *w > 0.0)
                                .unwrap_or(self.config.fonts.clamped_editor_size() * 0.6);
                            let ruler = ui_color(
                                &self.theme,
                                "ruler",
                                Rgba::new(muted.r(), muted.g(), muted.b(), 40),
                            );
                            let top = out.galley_pos.y;
                            let bot =
                                out.galley_pos.y + out.galley.size().y.max(ui.available_height());
                            for &col in &self.config.editor.rulers {
                                let x = out.galley_pos.x + cell_w * col as f32;
                                painter.line_segment(
                                    [egui::pos2(x, top), egui::pos2(x, bot)],
                                    egui::Stroke::new(1.0, ruler),
                                );
                            }
                        }
                        if let Some(range) = out.cursor_range {
                            // egui 0.34: CursorRange.primary is a CCursor directly
                            // (no nested .ccursor); Galley::pos_from_ccursor was
                            // renamed to pos_from_cursor (takes CCursor by value).
                            let cc = range.primary;
                            let rect = out.galley.pos_from_cursor(cc);
                            let pos = out.galley_pos + egui::vec2(rect.min.x, rect.max.y);
                            a = Some((pos, cc.index));
                            // F-005 / F-024 from docs/audits/overlooked-surfaces-2026-05-29.md:
                            // compute the human-visible (1-based) line + column and the
                            // selection-length-in-chars from the rope buffer + the
                            // egui CursorRange. This drives the status-bar "Ln N, Col N"
                            // and "(N chars selected)" indicators.
                            let text_ref = &self.tabs[active].text;
                            self.last_cursor_line_col =
                                Some(line_col_from_char_index(text_ref, cc.index));
                            self.last_selection_chars =
                                range.primary.index.abs_diff(range.secondary.index);
                            // Wave-6 motion: feed the caret-trail when the caret moves.
                            if motion_on && self.config.motion.caret_trail {
                                let t = ui.input(|i| i.time);
                                let caret_rect = egui::Rect::from_min_max(
                                    out.galley_pos + rect.min.to_vec2(),
                                    out.galley_pos + rect.max.to_vec2(),
                                )
                                .expand2(egui::vec2(1.0, 0.0));
                                let moved = self
                                    .caret_trail
                                    .back()
                                    .is_none_or(|(r, _)| r.min.distance(caret_rect.min) > 1.0);
                                if moved {
                                    self.caret_trail.push_back((caret_rect, t));
                                    while self.caret_trail.len() > 24 {
                                        self.caret_trail.pop_front();
                                    }
                                }
                            }
                            let collapsed = range.primary.index == range.secondary.index;
                            // Highlight every OTHER occurrence of the current
                            // selection (VS Code style). Single-line,
                            // non-whitespace selections only; bounded like
                            // bracket_match to stay cheap on huge files.
                            if self.config.editor.highlight_selection_occurrences
                                && !collapsed
                                && self.tabs[active].text.len() <= 500_000
                            {
                                let text_ref = &self.tabs[active].text;
                                let lo_ci = range.primary.index.min(range.secondary.index);
                                let hi_ci = range.primary.index.max(range.secondary.index);
                                let lo_b = char_to_byte(text_ref, lo_ci);
                                let hi_b = char_to_byte(text_ref, hi_ci);
                                let selected = &text_ref[lo_b..hi_b];
                                if !selected.trim().is_empty() && !selected.contains('\n') {
                                    let q = scribe_core::search::Query {
                                        pattern: selected.to_string(),
                                        case_sensitive: true,
                                        ..Default::default()
                                    };
                                    if let Ok(hits) = scribe_core::search::find_all(text_ref, &q) {
                                        let painter = ui.painter();
                                        let occ = ui_color(
                                            &self.theme,
                                            "selection_occurrence",
                                            Rgba::new(accent.r(), accent.g(), accent.b(), 130),
                                        );
                                        for m in &hits {
                                            if m.start == lo_b {
                                                continue; // skip the active selection itself
                                            }
                                            let c0 = byte_to_char_index(text_ref, m.start);
                                            let c1 = byte_to_char_index(text_ref, m.end);
                                            let r0 = out
                                                .galley
                                                .pos_from_cursor(egui::text::CCursor::new(c0));
                                            let r1 = out
                                                .galley
                                                .pos_from_cursor(egui::text::CCursor::new(c1));
                                            if (r0.min.y - r1.min.y).abs() > 0.5 {
                                                continue; // wrapped span; skip
                                            }
                                            let bx = egui::Rect::from_min_max(
                                                out.galley_pos + egui::vec2(r0.min.x, r0.min.y),
                                                out.galley_pos + egui::vec2(r1.min.x, r0.max.y),
                                            );
                                            painter.rect_stroke(
                                                bx,
                                                2.0,
                                                egui::Stroke::new(1.0, occ),
                                                egui::StrokeKind::Inside,
                                            );
                                        }
                                    }
                                }
                            }
                            // Wave-6 current-line highlight: a faint full-width band
                            // across the caret's galley row. Low alpha so it reads as
                            // a tint behind the (opaque) glyphs. Skipped on selection.
                            if self.config.editor.current_line_highlight && collapsed {
                                let painter = ui.painter();
                                let y0 = out.galley_pos.y + rect.min.y;
                                let y1 = out.galley_pos.y + rect.max.y;
                                let band = egui::Rect::from_min_max(
                                    egui::pos2(out.galley_pos.x, y0),
                                    egui::pos2(
                                        out.galley_pos.x
                                            + out.galley.size().x.max(ui.available_width()),
                                        y1,
                                    ),
                                );
                                let hl = Color32::from_rgba_unmultiplied(
                                    accent.r(),
                                    accent.g(),
                                    accent.b(),
                                    22,
                                );
                                painter.rect_filled(band, 0.0, hl);
                            }
                            // Wave-6 bracket-match: box the bracket next to the caret
                            // and its partner. The O(n) scan is bounded to a sane
                            // buffer size to stay cheap on huge files.
                            if self.config.editor.bracket_match
                                && collapsed
                                && self.tabs[active].text.len() <= 500_000
                            {
                                let text_ref = &self.tabs[active].text;
                                if let Some((open_ci, close_ci)) =
                                    matching_bracket_char_indices(text_ref, cc.index)
                                {
                                    let painter = ui.painter();
                                    let box_col = Color32::from_rgba_unmultiplied(
                                        accent.r(),
                                        accent.g(),
                                        accent.b(),
                                        60,
                                    );
                                    for ci in [open_ci, close_ci] {
                                        let r0 = out
                                            .galley
                                            .pos_from_cursor(egui::text::CCursor::new(ci));
                                        let r1 = out
                                            .galley
                                            .pos_from_cursor(egui::text::CCursor::new(ci + 1));
                                        if (r0.min.y - r1.min.y).abs() > 0.5 {
                                            continue; // span wrapped; skip
                                        }
                                        let bx = egui::Rect::from_min_max(
                                            out.galley_pos + egui::vec2(r0.min.x, r0.min.y),
                                            out.galley_pos + egui::vec2(r1.min.x, r0.max.y),
                                        );
                                        painter.rect_stroke(
                                            bx,
                                            1.0,
                                            egui::Stroke::new(1.0, box_col),
                                            egui::StrokeKind::Inside,
                                        );
                                    }
                                }
                            }
                            // Wave-6 caret style: draw a Block/Underline shape over
                            // egui's native caret (focus + no selection only). Honour
                            // blink when motion.cursor_blink is on.
                            if self.config.editor.caret_style
                                != scribe_core::config::CaretStyle::Bar
                                && collapsed
                                && out.response.has_focus()
                            {
                                let now = ui.ctx().input(|i| i.time);
                                let blink = motion_on && self.config.motion.cursor_blink;
                                let visible = if blink {
                                    (now / 1.06).rem_euclid(1.0) < 0.6
                                } else {
                                    true
                                };
                                if blink {
                                    ui.ctx().request_repaint_after(
                                        std::time::Duration::from_millis(120),
                                    );
                                }
                                if visible {
                                    let painter = ui.painter();
                                    let caret_col = ui_color(
                                        &self.theme,
                                        "caret",
                                        Rgba::new(accent.r(), accent.g(), accent.b(), 255),
                                    );
                                    let x = out.galley_pos.x + rect.min.x;
                                    let y0 = out.galley_pos.y + rect.min.y;
                                    let y1 = out.galley_pos.y + rect.max.y;
                                    let w = self.config.editor.clamped_caret_width();
                                    let cell_w = out
                                        .galley
                                        .rows
                                        .iter()
                                        .flat_map(|r| r.glyphs.iter())
                                        .map(|g| g.advance_width)
                                        .find(|w| *w > 0.0)
                                        .unwrap_or(self.config.fonts.clamped_editor_size() * 0.6);
                                    match self.config.editor.caret_style {
                                        scribe_core::config::CaretStyle::Block => {
                                            let blk = Color32::from_rgba_unmultiplied(
                                                caret_col.r(),
                                                caret_col.g(),
                                                caret_col.b(),
                                                110,
                                            );
                                            painter.rect_filled(
                                                egui::Rect::from_min_max(
                                                    egui::pos2(x, y0),
                                                    egui::pos2(x + cell_w, y1),
                                                ),
                                                0.0,
                                                blk,
                                            );
                                        }
                                        scribe_core::config::CaretStyle::Underline => {
                                            painter.rect_filled(
                                                egui::Rect::from_min_max(
                                                    egui::pos2(x, y1 - w.max(2.0)),
                                                    egui::pos2(x + cell_w, y1),
                                                ),
                                                0.0,
                                                caret_col,
                                            );
                                        }
                                        scribe_core::config::CaretStyle::Bar => {}
                                    }
                                }
                            }
                            // Wider Bar caret (width only): egui's caret is ~1px;
                            // overpaint a wider bar at the same x when width > 1.5.
                            if self.config.editor.caret_style
                                == scribe_core::config::CaretStyle::Bar
                                && self.config.editor.clamped_caret_width() > 1.5
                                && collapsed
                                && out.response.has_focus()
                            {
                                let painter = ui.painter();
                                let caret_col = ui_color(
                                    &self.theme,
                                    "caret",
                                    Rgba::new(accent.r(), accent.g(), accent.b(), 255),
                                );
                                let x = out.galley_pos.x + rect.min.x;
                                let y0 = out.galley_pos.y + rect.min.y;
                                let y1 = out.galley_pos.y + rect.max.y;
                                painter.rect_filled(
                                    egui::Rect::from_min_max(
                                        egui::pos2(x, y0),
                                        egui::pos2(
                                            x + self.config.editor.clamped_caret_width(),
                                            y1,
                                        ),
                                    ),
                                    0.0,
                                    caret_col,
                                );
                            }
                        }
                        // Capture each logical line's screen Y for the gutter (a row
                        // starts a logical line iff the previous row ended with \n).
                        if show_line_numbers {
                            let top = out.galley_pos.y;
                            let mut prev_newline = true;
                            for row in &out.galley.rows {
                                if prev_newline {
                                    // egui 0.34: PlacedRow.rect is now a method, not a field.
                                    new_gutter.push(top + row.rect().min.y);
                                }
                                prev_newline = row.ends_with_newline;
                            }
                        }
                        // Auto-focus the editor so typing works immediately on launch,
                        // new tab, or tab switch — no click required — unless a field,
                        // menu, or popup currently owns keyboard focus.
                        if !read_only
                            && !overlay_open
                            && ui.ctx().memory(|m| m.focused().is_none())
                            && !egui::Popup::is_any_open(ui.ctx())
                        {
                            out.response.request_focus();
                        }
                    });
                    // Record scroll metrics for the minimap's viewport indicator.
                    self.scroll_metrics = (
                        sa_out.state.offset.y,
                        sa_out.content_size.y.max(1.0),
                        sa_out.inner_rect.height().max(1.0),
                    );
                    // This is the full-feature TextEdit path — publish Standard so the
                    // status-bar mode badge CLEARS when the user returns to a small
                    // file. Without this the badge would keep showing ROPE/MMAP from
                    // whatever large file rendered last, which is exactly the "silent
                    // and misleading" state the badge exists to prevent.
                    publish_editor_mode(ctx, EditorMode::Standard);
                    // Hand the viewport rect to the post-render drag-scroll +
                    // caret-scroll-off assists (applied after the `hl` borrow).
                    editor_vp = sa_out.inner_rect;
                    // F-034 sticky scroll: pin the enclosing definition headers
                    // at the top of the viewport once their own header line has
                    // scrolled above it. Drawn with an opaque chrome fill so the
                    // pinned line occludes the scrolled body behind it. Clicking
                    // a pinned header jumps to that definition.
                    if !scopes.is_empty() {
                        let lh_px = (font.size * line_height).max(1.0);
                        let first_visible_line = (sa_out.state.offset.y / lh_px).floor() as usize;
                        let pinned =
                            crate::editor_features::sticky_chain_at(&scopes, first_visible_line, 5);
                        let vp = sa_out.inner_rect;
                        let bg = Color32::from_rgb(panel.r(), panel.g(), panel.b());
                        let painter = ui.painter_at(vp);
                        for (i, s) in pinned.iter().enumerate() {
                            let y = vp.top() + (i as f32) * lh_px;
                            let row = egui::Rect::from_min_max(
                                egui::pos2(vp.left(), y),
                                egui::pos2(vp.right(), y + lh_px),
                            );
                            painter.rect_filled(row, 0.0, bg);
                            let indent = 6.0 + (s.depth as f32) * 12.0;
                            painter.text(
                                egui::pos2(vp.left() + indent, y + lh_px * 0.5),
                                egui::Align2::LEFT_CENTER,
                                &s.label,
                                font.clone(),
                                accent,
                            );
                            if i + 1 == pinned.len() {
                                // Underline the bottom of the pinned stack so it
                                // reads as a header band, not part of the buffer.
                                painter.line_segment(
                                    [
                                        egui::pos2(vp.left(), row.bottom()),
                                        egui::pos2(vp.right(), row.bottom()),
                                    ],
                                    egui::Stroke::new(1.0, muted),
                                );
                            }
                            let resp = ui.interact(
                                row,
                                ui.id().with(("scr1b3-sticky", i)),
                                egui::Sense::click(),
                            );
                            if resp.clicked() {
                                sticky_jump = Some(s.start_line);
                            }
                        }
                    }
                    a
                };
                self.line_gutter = new_gutter;

                // ---- P2 multi-cursor — resolve galley-dependent gestures ----
                // Ctrl/Cmd+click: add (or toggle off) a secondary caret at the
                // clicked char and restore the pre-click primary so it stays
                // primary. Reconcile against `mc_prev_primary` (the pre-click
                // head) — egui may have already moved its live primary onto the
                // click point this frame, which would spuriously read as a hit.
                if let Some(ix) = mc_ctrl_click_idx {
                    let primary = mc_prev_primary
                        .map(crate::multi_cursor::Caret::at)
                        .unwrap_or_else(|| crate::multi_cursor::Caret::at(ix));
                    self.multi_cursor
                        .toggle_caret(crate::multi_cursor::Caret::at(ix), primary);
                    if let Some(prev) = mc_prev_primary {
                        super::multi_cursor_glue::mc_set_primary(ctx, editor_id, prev, prev);
                    }
                    ctx.request_repaint();
                }
                // Alt+press latches the column-selection anchor (and clears any
                // prior multi-cursor); Alt+drag builds the per-line block.
                if let Some(idx) = mc_alt_anchor_idx {
                    self.column_anchor = Some(idx);
                    self.multi_cursor.clear();
                }
                if let (Some(anchor_idx), Some(head_idx)) = (self.column_anchor, mc_alt_head_idx) {
                    let chars: Vec<char> = self.tabs[active].text.chars().collect();
                    let mut carets =
                        crate::multi_cursor::column_selection(&chars, anchor_idx, head_idx);
                    if carets.len() >= 2 {
                        // Primary = the caret nearest the drag head; the rest are
                        // secondaries painted + edited alongside it.
                        let pix = carets
                            .iter()
                            .enumerate()
                            .min_by_key(|(_, c)| {
                                (c.head as isize - head_idx as isize).unsigned_abs()
                            })
                            .map(|(i, _)| i)
                            .unwrap_or(0);
                        let primary = carets.remove(pix);
                        super::multi_cursor_glue::mc_set_primary(
                            ctx,
                            editor_id,
                            primary.anchor,
                            primary.head,
                        );
                        self.multi_cursor.set_secondaries(carets);
                        ctx.request_repaint();
                    } else if let Some(c) = carets.first() {
                        // A single-line Alt+drag spans one line — make it a normal
                        // ranged selection on that line instead of dropping the
                        // gesture.
                        super::multi_cursor_glue::mc_set_primary(ctx, editor_id, c.anchor, c.head);
                        self.multi_cursor.clear();
                        ctx.request_repaint();
                    }
                }
                // P1-A: attribute whatever multi-cursor state survived this frame's
                // gestures (Ctrl+D / Ctrl+click / Alt+drag) to the active tab, so
                // next frame's `mc_reconcile_owner` invalidates it on a tab switch.
                self.mc_record_owner(active);
                // F-034: apply a sticky-header click now that the hl borrow is
                // released. Scrolls so the clicked definition sits at the top.
                if let Some(line0) = sticky_jump {
                    let lh_px = (font.size * line_height).max(1.0);
                    self.pending_scroll = Some((line0 as f32) * lh_px);
                }
                // P0-1/P0-2 drag-select autoscroll + P1-4 caret scroll-off. Both
                // reuse `pending_scroll` (consumed next frame). The drag path only
                // acts while the primary button is held and the editor is focused;
                // the caret path only on a keyboard move with no button down, so
                // the two never write in the same frame (and neither disturbs the
                // sticky-header jump above, which fires on a click).
                self.drag_scroll_assist(ctx, editor_id, editor_vp);
                if let Some((caret_pos, _)) = anchor {
                    let lh_px = (font.size * line_height).max(1.0);
                    self.caret_scroll_off_assist(ctx, caret_pos.y, editor_vp, lh_px);
                }

                // Completion: open on Ctrl+Space, accept on Enter/Tab, render popup.
                let cursor_idx = anchor.map(|(_, i)| i);
                if want_completion {
                    self.open_completion(active, cursor_idx);
                }
                if accept_completion {
                    self.accept_completion(active, cursor_idx);
                }
                if let Some((pos, _)) = anchor {
                    let choice = self
                        .completion
                        .as_ref()
                        .and_then(|c| completion_popup(ui, pos, c));
                    if let Some(idx) = choice {
                        if let Some(c) = self.completion.as_mut() {
                            c.selected = idx;
                        }
                        self.accept_completion(active, cursor_idx);
                    }
                }
            });
        }

        // An unrequested swap into a degraded editor queues a one-shot notice
        // (see `publish_editor_mode`). Drain it HERE — the first point after the
        // central panel where `self` is mutable again — so the swap is announced
        // rather than only being discoverable by hovering the status-bar badge.
        self.drain_editor_mode_notice(ctx);

        // Window color-tint overlay (subtle wash; portable across modes/OSes).
        if self.config.window.tint_strength > 0.0 {
            paint_tint_overlay(
                ctx,
                &self.config.window.tint,
                self.config.window.tint_strength,
            );
        }
        // CRT scanlines post-effect (#14, ported from C0PL4ND). A calm animated
        // retro overlay; only when motion AND scanlines are both enabled. Drives
        // a modest ~30 fps repaint while on so the bands drift (no busy-spin), and
        // never paints in the headless test harness (no real window to overlay).
        if !cfg!(test) && motion_on && self.config.motion.crt_scanlines {
            let t = ctx.input(|i| i.time);
            paint_crt_scanlines(ctx, self.config.motion.scanline_darkness, t);
            ctx.request_repaint_after(std::time::Duration::from_millis(33));
        }
        // Wave-6 motion overlays (master-gated; never in the headless harness).
        // Each is a calm post-effect; while any is active we drive a ~30 fps
        // repaint so it animates. The resting (motion-off) frame is unchanged.
        if !cfg!(test) && motion_on {
            let t = ctx.input(|i| i.time);
            let accent = ui_color(&self.theme, "accent", Rgba::new(0x4c, 0xc2, 0xff, 255));
            let mut animating = false;
            if self.config.motion.wired_ambient {
                // Mesh colour follows the theme accent by default, or the user's
                // pinned override (Settings → Motion → Mesh colour) when set.
                let [mr, mg, mb] =
                    self.config
                        .motion
                        .resolved_mesh_color([accent.r(), accent.g(), accent.b()]);
                paint_wired_mesh(
                    ctx,
                    self.config.motion.clamped_mesh_density(),
                    self.config.motion.mesh_link_alpha(),
                    self.config.motion.mesh_dot_alpha(),
                    Color32::from_rgb(mr, mg, mb),
                    t,
                    self.config.motion.clamped_mesh_drift_speed(),
                );
                animating = true;
            }
            if self.config.motion.vhs_tracking {
                paint_vhs_tracking(ctx, t, self.config.motion.clamped_vhs_speed());
                animating = true;
            }
            if self.config.motion.flicker {
                paint_flicker(
                    ctx,
                    self.config.motion.clamped_flicker_strength(),
                    t,
                    self.config.motion.clamped_flicker_speed(),
                );
                animating = true;
            }
            if self.config.motion.caret_trail {
                let life = self.config.motion.caret_trail_life();
                while let Some(&(_, born)) = self.caret_trail.front() {
                    if t - born > life {
                        self.caret_trail.pop_front();
                    } else {
                        break;
                    }
                }
                paint_caret_trail(
                    ctx,
                    &self.caret_trail,
                    accent,
                    t,
                    self.config.motion.caret_trail_intensity,
                );
                if !self.caret_trail.is_empty() {
                    animating = true;
                }
            }
            if self.config.motion.boot_glitch {
                let started = *self.boot_glitch_started.get_or_insert(t);
                let elapsed = t - started;
                if elapsed <= 0.55 {
                    paint_boot_glitch(ctx, elapsed);
                    animating = true;
                }
            }
            if animating {
                ctx.request_repaint_after(std::time::Duration::from_millis(33));
            }
        }
        // Phase 18 T18.1: 8-zone resize overlay for the frameless window. egui
        // doesn't restore OS resize when window decorations are off (winit
        // #4186) so we paint invisible interact rectangles at the edges + four
        // corners that send `ViewportCommand::BeginResize(dir)` on drag and
        // hint the right cursor on hover.
        //
        // No persistent Foreground Areas (those swallowed tab/settings clicks
        // window-wide and could leave resize stuck after the first drag). This
        // is a pure per-frame check: hint the resize cursor at an edge and start
        // an OS resize on a press there — only when egui isn't already using the
        // pointer for a widget. Works repeatedly by construction.
        let _ = overlay_open;
        if self.config.appearance.frameless {
            let maximized = ctx.input(|i| i.viewport().maximized).unwrap_or(false);
            if !maximized && !fullscreen {
                handle_frameless_resize(ctx);
            }
        }

        // Apply deferred actions after all UI borrows are released.
        self.apply_deferred_actions(
            ctx,
            &mut act,
            deferred_actions::DeferredFlags {
                run_cmd,
                run_builtin,
                save_cfg,
                open_from_tree,
                close_tree,
                start_lsp,
                want_open_cfg,
                want_restore_cfg,
                want_dismiss_cfg,
            },
        );

        self.persist_session_and_autosave();
    }
}

/// Editor-surface link + paste wiring, asserted by OBSERVABLE OUTCOME.
///
/// Both features are wires between things that already worked separately (the
/// wiki-link resolver; the attachment-paste command), so the only failure worth
/// testing for is the wire itself being absent — which a "a pending flag was
/// set" assertion cannot see. Every test here asserts the end of the wire: the
/// note that got created and opened, or the markdown that landed in the buffer.
#[cfg(test)]
mod editor_link_paste_wiring_tests {
    use super::super::e2e::Driver;
    use super::super::note_capture::{test_hooks, ClipboardImage};
    use crate::app::ScribeApp;
    use scribe_core::config::Config;
    use std::path::{Path, PathBuf};

    const CMD: egui::Modifiers = egui::Modifiers::COMMAND;

    struct Vault {
        _root: tempfile::TempDir,
        path: PathBuf,
    }

    fn vault() -> Vault {
        let root = tempfile::tempdir().expect("temp root");
        let path = root.path().join("vault");
        std::fs::create_dir_all(&path).expect("vault dir");
        Vault { _root: root, path }
    }

    fn app_with_vault(v: &Path) -> ScribeApp {
        let mut cfg = Config::default();
        cfg.editor.first_run_completed = true;
        cfg.notes.vault_dir = Some(v.to_path_buf());
        ScribeApp::new_test(cfg)
    }

    fn solid_image(w: usize, h: usize) -> ClipboardImage {
        ClipboardImage {
            width: w,
            height: h,
            rgba: vec![0x40u8; w * h * 4],
        }
    }

    /// A modified pointer click (move + press + release) in ONE frame — the same
    /// shape `e2e`'s own Ctrl+click test uses.
    fn mod_click(d: &Driver, app: &mut ScribeApp, pos: egui::Pos2, modifiers: egui::Modifiers) {
        d.frame(
            app,
            modifiers,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers,
                },
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers,
                },
            ],
        );
    }

    /// A buffer whose every byte sits inside a link span, so the pointer
    /// hit-test cannot land in a gap between links. Taller than the window (any
    /// y lands on text) and each line is ~60 chars — wide enough that [`CLICK`]'s
    /// x is mid-row, narrow enough not to soft-wrap. Both bounds matter: a
    /// wrapped row's short tail clamps the hit-test onto the trailing NEWLINE,
    /// which is inside no link, and the click silently does nothing.
    fn wall_of(link: &str) -> String {
        let line = link.repeat(60_usize.div_ceil(link.len()));
        assert!(
            (60..90).contains(&line.chars().count()),
            "fixture geometry: {line:?}"
        );
        (0..200)
            .map(|_| line.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Well inside the wall of links, on both axes.
    const CLICK: egui::Pos2 = egui::Pos2::new(150.0, 380.0);

    // ---- A: in-editor [[wiki-link]] click ----

    /// Ctrl+clicking a `[[wiki-link]]` IN THE EDITOR opens the note — creating
    /// it when it does not exist yet, exactly as the notes pane's link list
    /// does. Asserted on the file that appears and the tab that ends up active,
    /// never on an intermediate latch.
    #[test]
    fn ctrl_clicking_a_wikilink_in_the_editor_opens_the_note() {
        let v = vault();
        std::fs::write(v.path.join("Home.md"), "seed\n").unwrap();
        let mut app = app_with_vault(&v.path);
        app.open_path(v.path.join("Home.md"));
        let active = app.active;
        app.tabs[active].text = wall_of("[[Target]]");

        let d = Driver::new();
        d.idle(&mut app);
        d.idle(&mut app); // editor lays out + takes focus
        assert!(
            !v.path.join("Target.md").exists(),
            "precondition: the link target does not exist yet"
        );

        mod_click(&d, &mut app, CLICK, CMD);
        d.idle(&mut app); // the stashed follow is drained at the top of the frame

        let target = v.path.join("Target.md");
        assert!(
            target.exists(),
            "the clicked [[Target]] must be created in the vault"
        );
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "# Target\n",
            "created through open_or_create_wikilink, seeded with its heading"
        );
        assert_eq!(
            app.tabs[app.active].doc.path(),
            Some(target.as_path()),
            "…and opened in the active tab"
        );
    }

    /// A plain (unmodified) click over a wiki-link must NOT follow it — the
    /// modifier is the whole consent gesture, same as the URL arm.
    #[test]
    fn a_plain_click_on_a_wikilink_does_not_open_anything() {
        let v = vault();
        std::fs::write(v.path.join("Home.md"), "seed\n").unwrap();
        let mut app = app_with_vault(&v.path);
        app.open_path(v.path.join("Home.md"));
        let active = app.active;
        app.tabs[active].text = wall_of("[[Target]]");

        let d = Driver::new();
        d.idle(&mut app);
        d.idle(&mut app);
        mod_click(&d, &mut app, CLICK, egui::Modifiers::NONE);
        d.idle(&mut app);

        assert!(
            !v.path.join("Target.md").exists(),
            "an unmodified click must not create or open the note"
        );
    }

    /// The traversal gate is the resolver's, not a second copy: a link that
    /// escapes the vault is refused with a toast and writes nothing outside it.
    #[test]
    fn a_traversal_wikilink_clicked_in_the_editor_is_refused() {
        let v = vault();
        let outside = v.path.parent().unwrap().to_path_buf();
        std::fs::write(v.path.join("Home.md"), "seed\n").unwrap();
        let mut app = app_with_vault(&v.path);
        app.open_path(v.path.join("Home.md"));
        let active = app.active;
        app.tabs[active].text = wall_of("[[../escaped]]");

        let d = Driver::new();
        d.idle(&mut app);
        d.idle(&mut app);
        mod_click(&d, &mut app, CLICK, CMD);
        d.idle(&mut app);

        assert!(
            !outside.join("escaped.md").exists(),
            "nothing may be written outside the vault"
        );
        assert!(
            app.toast
                .as_deref()
                .unwrap_or("")
                .contains("Can't open that link"),
            "the refusal is surfaced, not silent: {:?}",
            app.toast
        );
    }

    // ---- C: Ctrl+V pastes a clipboard image as an attachment ----

    /// Ctrl+V with an image on the clipboard runs the attachment paste and the
    /// markdown link lands IN THE BUFFER. The end of the wire, not the latch:
    /// `pending_insert_text.is_some()` would still pass with the drain deleted.
    #[test]
    fn ctrl_v_with_a_clipboard_image_inserts_the_attachment_markdown() {
        let v = vault();
        std::fs::write(v.path.join("Note.md"), "before\n").unwrap();
        let mut app = app_with_vault(&v.path);
        app.open_path(v.path.join("Note.md"));
        let active = app.active;

        let d = Driver::new();
        d.idle(&mut app);
        d.idle(&mut app); // the editor must own focus for the paste hook

        test_hooks::set_next_image(solid_image(2, 2));
        // egui_winit swallows the paste key-DOWN, so the RELEASE is the event
        // the app really sees; `Driver::key` sends press + release.
        d.key(&mut app, egui::Key::V, CMD);
        d.idle(&mut app); // deliver the queued insertion
        d.idle(&mut app); // …and let the editor settle it

        let text = app.tabs[active].text.clone();
        assert!(
            text.contains("![pasted image](attachments/pasted-"),
            "Ctrl+V must insert the attachment markdown, got {text:?}"
        );
        assert!(
            text.contains("before"),
            "existing content survives: {text:?}"
        );
        let name = text
            .split_once("](")
            .and_then(|(_, t)| t.split_once(')'))
            .map(|(p, _)| p.to_string())
            .expect("a link target");
        assert!(
            v.path.join(&name).exists(),
            "the link names a PNG that is really there: {name}"
        );
    }

    /// The RELEASE alone must fire it — which is the whole reason the hook is
    /// not `consume_key(COMMAND, V)`.
    ///
    /// `egui_winit::State::on_keyboard_input` special-cases the paste chord on
    /// key-DOWN and returns immediately, so `Event::Key { key: V, pressed: true }`
    /// is NEVER emitted for Ctrl+V, and on an image-only clipboard (no text to
    /// put in an `Event::Paste`) it emits nothing at all on the way down. This
    /// test replays exactly what the real integration delivers — the release and
    /// nothing else. Without it, `Driver::key`'s press+release pair lets a
    /// press-watching implementation pass while being dead code in the app.
    #[test]
    fn the_paste_key_release_alone_pastes_the_image() {
        let v = vault();
        std::fs::write(v.path.join("Note.md"), "before\n").unwrap();
        let mut app = app_with_vault(&v.path);
        app.open_path(v.path.join("Note.md"));
        let active = app.active;

        let d = Driver::new();
        d.idle(&mut app);
        d.idle(&mut app);

        test_hooks::set_next_image(solid_image(2, 2));
        d.frame(
            &mut app,
            CMD,
            vec![egui::Event::Key {
                key: egui::Key::V,
                physical_key: None,
                pressed: false,
                repeat: false,
                modifiers: CMD,
            }],
        );
        d.idle(&mut app);
        d.idle(&mut app);

        assert!(
            app.tabs[active]
                .text
                .contains("![pasted image](attachments/pasted-"),
            "the key RELEASE is the only event egui emits for an image-only \
             clipboard; got {:?}",
            app.tabs[active].text
        );
    }

    /// A clipboard carrying TEXT pastes the text: the image branch must not also
    /// fire on the key release and drop an unwanted attachment in.
    #[test]
    fn a_text_paste_is_not_hijacked_by_the_image_branch() {
        let v = vault();
        std::fs::write(v.path.join("Note.md"), "before\n").unwrap();
        let mut app = app_with_vault(&v.path);
        app.open_path(v.path.join("Note.md"));

        let d = Driver::new();
        d.idle(&mut app);
        d.idle(&mut app);

        test_hooks::set_next_image(solid_image(2, 2));
        // The real gesture shape: `Event::Paste` on the key-down frame, the `V`
        // release a frame later.
        d.frame(&mut app, CMD, vec![egui::Event::Paste("hello".into())]);
        d.key(&mut app, egui::Key::V, CMD);
        d.idle(&mut app);
        d.idle(&mut app);

        assert!(
            !v.path.join("attachments").exists(),
            "a text paste must not write an image attachment"
        );
        assert!(
            !app.tabs[app.active].text.contains("![pasted image]"),
            "…nor insert attachment markdown: {:?}",
            app.tabs[app.active].text
        );
    }

    /// Ctrl+SHIFT+V is a different binding (markdown preview) — the image branch
    /// must not claim its release.
    #[test]
    fn ctrl_shift_v_does_not_paste_an_image() {
        let v = vault();
        std::fs::write(v.path.join("Note.md"), "before\n").unwrap();
        let mut app = app_with_vault(&v.path);
        app.open_path(v.path.join("Note.md"));

        let d = Driver::new();
        d.idle(&mut app);
        d.idle(&mut app);

        test_hooks::set_next_image(solid_image(2, 2));
        d.key(&mut app, egui::Key::V, CMD | egui::Modifiers::SHIFT);
        d.idle(&mut app);
        d.idle(&mut app);

        assert!(
            !v.path.join("attachments").exists(),
            "Ctrl+Shift+V must not paste an image attachment"
        );
    }
}
