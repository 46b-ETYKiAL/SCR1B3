//! The egui_tiles grid central-panel methods for `ScribeApp`:
//! `render_grid_central_panel` (renders the tiles tree as the editor
//! surface, delegating each pane header to `grid_render`) and
//! `sync_grid_state` (keeps the tile tree in step with the tab model).
//! Extracted from `mod.rs` (A-01 wave 3 — behavior-preserving move; both
//! widened to `pub(super)` for the `frame_tick` sibling call-sites).
//!
//! # Pane/editor parity
//!
//! The pane body used to be a BARE `TextEdit::multiline` — no widget id, no
//! right-click menu, no scroll metrics, no drag/page scroll assists, no
//! multi-cursor, no rope path, and no editor-mode publish. Everything the
//! single-pane path in `frame_tick` wires around its editor simply did not
//! exist once the user turned the grid on, so "split view" silently downgraded
//! the editor. Each of those is wired here against the ACTIVE pane, which is
//! now bound to whichever pane owns keyboard focus (see the focus→active sync
//! at the top of [`ScribeApp::render_grid_central_panel`]) so every
//! `active`-keyed surface in the app addresses the pane the user is typing in.
#![allow(clippy::wildcard_imports)]

use super::frame_tick::{publish_editor_mode, EditorMode};
use super::*;
use std::cell::{Cell, RefCell};

/// The stable `egui::Id` of a grid pane's editor widget, derived ONLY from the
/// pane's document id.
///
/// Every capability the single-pane path gets hangs off a KNOWN, stable widget
/// id: `TextEdit::load_state` (the caret the page-key, multi-cursor and caret-op
/// seams read and write), `Memory::has_focus` (which pane the user is typing
/// in), and egui's own per-widget selection + undo history. The pane editor
/// carried no `.id()` at all, so egui derived one from the `Ui` path — which
/// changes whenever egui_tiles rearranges the tree, discarding the pane's
/// selection and undo history on a drag and leaving the app with no handle on
/// the widget. Salting on `doc_id` (exactly as the single-pane editor does)
/// makes the id survive every layout change and stay distinct per note.
pub(super) fn pane_editor_id(doc_id: crate::grid::DocId) -> egui::Id {
    egui::Id::new("scr1b3-grid-pane-editor").with(doc_id)
}

/// What the active pane's caret looked like on its own laid-out galley.
///
/// Named rather than a bare tuple because three unrelated units travel together:
/// a screen-space Y (for the scroll-off assist), a 1-based `(line, column)` (for
/// the status bar), and a character count (the selection length).
#[derive(Clone, Copy)]
struct PaneCaret {
    /// Screen-space bottom edge of the caret rect.
    screen_bottom_y: f32,
    /// 1-based `(line, column)` for the status-bar readout.
    line_col: (usize, usize),
    /// Selection length in characters (0 for a collapsed caret).
    selection_chars: usize,
}

/// Per-pane state the pane closure observes and the post-render half consumes.
///
/// The closure runs while `self.tabs` is mutably borrowed, so it cannot call the
/// `&mut self` assists directly. It records what it saw into these `Cell`s and
/// the caller applies them once `tree.ui()` has returned — the same
/// take-then-put-back idiom the pre-existing `closes` buffer uses.
#[derive(Default)]
struct ActivePaneObserved {
    /// `(offset_y, content_h, view_h)` of the active pane's scroll surface.
    metrics: Cell<Option<(f32, f32, f32)>>,
    /// The active pane's screen-space viewport (the scroll surface's rect).
    viewport: Cell<Option<egui::Rect>>,
    /// The id that actually holds keyboard focus for the active pane: the
    /// `TextEdit`'s own id, or `RopeEditor`'s internal focus id on a rope pane.
    focus_id: Cell<Option<egui::Id>>,
    /// The `ScrollArea` state key of a WIDGET-owned scroll surface (rope panes
    /// only), so a queued `pending_scroll` can be pushed into it afterwards.
    embedded_scroll_id: Cell<Option<egui::Id>>,
    /// Which surface the active pane actually rendered on.
    mode: Cell<Option<EditorMode>>,
    /// The active pane's caret, as read back from its laid-out galley.
    caret: Cell<Option<PaneCaret>>,
    /// Galley-resolved multi-cursor gestures (`Ctrl`+click / `Alt`+drag).
    mc_ctrl_click_idx: Cell<Option<usize>>,
    mc_alt_anchor_idx: Cell<Option<usize>>,
    mc_alt_head_idx: Cell<Option<usize>>,
    /// A pane whose editor was right-clicked this frame. The context menu's
    /// commands act on the ACTIVE tab, so the right-clicked pane must become
    /// active before the pick is drained — otherwise "Bold" typed into pane 2
    /// would embolden pane 1.
    menu_pane: Cell<Option<crate::grid::DocId>>,
    /// Per-logical-line screen Ys of the active pane's galley, for the gutter.
    gutter: RefCell<Vec<f32>>,
    /// Text the rope editor cut/copied, applied to the OS clipboard afterwards.
    clipboard: RefCell<Option<String>>,
}

impl ScribeApp {
    /// Phase 18 T18.2 — render the egui_tiles grid as the central
    /// editor surface. Each leaf pane wraps the SAME editor stack the
    /// single-pane path uses over the matching tab's text. The
    /// `Option::take`-then-put-back idiom hands `&mut self` to the callbacks
    /// while keeping the tree owned across frames.
    #[allow(clippy::too_many_lines)]
    pub(super) fn render_grid_central_panel(&mut self, ctx: &egui::Context, font: egui::FontId) {
        // Snapshot the titles up front so the behavior callback doesn't
        // need to re-borrow `self.tabs` (which is also borrowed mutably
        // by the body callback).
        let titles: Vec<(crate::grid::DocId, String)> =
            self.tabs.iter().map(|t| (t.doc_id, t.title())).collect();
        let Some(mut tree) = self.grid_tree.take() else {
            return;
        };
        if self.tabs.is_empty() {
            self.grid_tree = Some(tree);
            return;
        }

        // ---- Focus -> active tab ----
        // With a stable per-pane editor id the focused pane is identifiable
        // BEFORE anything renders, so `self.active` can follow it. Nothing used
        // to move `self.active` in grid view at all: the header chip's "active"
        // accent fill was frozen on whichever tab happened to be active when the
        // grid opened, the status bar's counters/encoding/language described that
        // tab rather than the one being typed in, and every `active`-keyed
        // operation below (caret ops, multi-cursor ownership, paging) would have
        // targeted the wrong document.
        if let Some(i) = self
            .tabs
            .iter()
            .position(|t| ctx.memory(|m| m.has_focus(pane_editor_id(t.doc_id))))
        {
            self.active = i;
        }
        let active = self.active.min(self.tabs.len() - 1);
        self.active = active;
        let active_doc = self.tabs[active].doc_id;
        let editor_id = pane_editor_id(active_doc);
        let editor_focused = ctx.memory(|m| m.has_focus(editor_id));
        // Same predicate the single-pane path uses to decide whether the editor
        // (rather than a modal) owns the keyboard this frame.
        let overlay_open = self.find_open || self.palette_open || self.settings_open;
        let active_read_only = self.tabs[active].doc.is_read_only_large();

        // ---- P2 multi-cursor — pre-render half ----
        // Identical ordering to the single-pane path: reconcile the carets'
        // owning doc, let Escape collapse them, then intercept the edit keys
        // BEFORE any `TextEdit` consumes this frame's events.
        self.mc_reconcile_owner(active);
        self.mc_collapse_on_escape(ctx, overlay_open);
        let mc_focus = !active_read_only && !overlay_open && editor_focused;
        if mc_focus {
            self.handle_multi_cursor_keys(ctx, editor_id, active);
        }
        let (mc_primary_pressed, mc_cmd, mc_alt, mc_primary_down, mc_ptr) = ctx.input(|i| {
            (
                i.pointer.primary_pressed(),
                i.modifiers.command,
                i.modifiers.alt,
                i.pointer.primary_down(),
                i.pointer.interact_pos(),
            )
        });
        if mc_focus && mc_primary_pressed && !mc_cmd && !mc_alt && self.multi_cursor.is_active() {
            self.multi_cursor.clear();
        }
        let mc_ctrl_click = mc_focus && mc_primary_pressed && mc_cmd && !mc_alt && mc_ptr.is_some();
        let mc_prev_primary: Option<usize> = if mc_ctrl_click {
            super::multi_cursor_glue::mc_load_primary_head(ctx, editor_id)
        } else {
            None
        };
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
        let mc_secondaries: Vec<crate::multi_cursor::Caret> =
            self.multi_cursor.secondaries().to_vec();

        // ---- Editor chords + the image-paste hook (single-pane parity) ----
        //
        // Both blocks live in the SINGLE-PANE arm of `frame_tick`'s
        // `if self.grid_tree.is_some() { … } else { … }` fork, so with split
        // view on the markdown chords did nothing (Ctrl+B / Ctrl+I / Ctrl+` /
        // Ctrl+Shift+X / Ctrl+Enter all fell through to egui) and Ctrl+V with an
        // image on the clipboard pasted nothing. The `pending_*` latches the
        // chords raise are already drained by `apply_pending_caret_ops` below,
        // so only the interception itself was missing.
        //
        // This runs BEFORE `CentralPanel::show`, which is the same position the
        // single-pane block occupies relative to its editor: the keys must be
        // consumed before any `TextEdit` sees this frame's events.
        if !active_read_only && editor_focused {
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
            // vault's attachments folder and inserts the markdown link. See
            // `grid_render::image_paste_gesture` for why the hook is the key
            // RELEASE rather than `consume_key(COMMAND, V)`.
            if grid_render::image_paste_gesture(ctx) && self.config.notes.vault_dir.is_some() {
                self.paste_image_attachment();
            }
        }

        // ---- Inline LSP diagnostics (single-pane parity) ----
        //
        // Resolved against the ACTIVE tab's text, and deliberately AFTER the
        // focus->active sync above: `self.active` follows the focused pane, so
        // the squiggle has to follow it too. Computing this before the sync
        // would underline the previously-active pane. Owned so it can move into
        // the pane closure, which mutably borrows `self.tabs`. Empty (and free)
        // when the language server has published nothing — the common case.
        let diag_spans = self.diagnostic_spans_for_active(active);
        let diag_colors = grid_render::DiagColors {
            error: ui_color(&self.theme, "error", Rgba::new(0xe5, 0x3e, 0x3e, 255)),
            warning: ui_color(&self.theme, "warning", Rgba::new(0xf2, 0xb3, 0x3d, 255)),
            info: ui_color(&self.theme, "accent", Rgba::new(0, 255, 254, 255)),
            hint: ui_color(&self.theme, "line_number", Rgba::new(0x5a, 0x58, 0x69, 255)),
        };
        // Whether a `[[wiki-link]]` has anywhere to resolve to — the same
        // precondition `open_or_create_wikilink` enforces.
        let vault_configured = self.config.notes.vault_dir.is_some();

        let line_height = self.config.fonts.clamped_line_height();
        let word_wrap = self.config.editor.word_wrap;
        // #28 — render-whitespace toggle + editor font size captured as locals so
        // the per-pane body closure (which can't re-borrow `self.config`) can
        // paint the `·`/`→` whitespace overlay on each pane's galley too.
        let render_whitespace = self.config.editor.render_whitespace;
        let editor_font_size = self.config.fonts.clamped_editor_size();
        let gutter_row_h = font.size * line_height;
        let show_line_numbers = self.config.editor.show_line_numbers;
        let experimental_rope = self.config.editor.experimental_rope_editor;
        let rope_threshold = self.config.editor.rope_editor_auto_threshold_bytes;
        let snippets_enabled = self.config.editor.snippets_enabled;
        let scroll_past_end = self.config.scroll.scroll_past_end;
        // Disjoint-field borrows captured as locals BEFORE the central-panel
        // closure (which mutably borrows `self.tabs`). The highlighter + its
        // cache are different fields than `tabs`, so the immutable borrows here
        // and the closure's `&mut self.tabs` coexist under disjoint closure
        // capture.
        let hl = &self.hl;
        let hl_cache = &self.hl_cache;
        let hl_galley_cache = &self.hl_galley_cache;
        let hl_inc_cache = &self.hl_inc_cache;
        let snippets = &self.snippets;
        // Wave-3: theme foreground for the highlighter tail colour (per-pane).
        let layout_fg = ui_color(&self.theme, "foreground", Rgba::new(0xc8, 0xd6, 0xdc, 255));
        // #D — themeable URL colour + link-detection toggle for the per-pane
        // layouter (same source as the single-pane editor).
        let detect_links = self.config.editor.detect_links;
        let url_color = scribe_render::color32(self.theme.syntax_color(
            "url",
            self.theme.ui("accent", Rgba::new(0x4c, 0xc2, 0xff, 255)),
        ));
        // #R5: theme colours + focused-pane id for the chip-styled pane headers
        // (the per-pane note bar now mirrors the top tab strip's chip look).
        let accent = ui_color(&self.theme, "accent", Rgba::new(0, 255, 254, 255));
        let muted = ui_color(&self.theme, "line_number", Rgba::new(0x5a, 0x58, 0x69, 255));
        // Per-frame shared close buffer. The pane `✕` button writes here and
        // `AppGridBehavior::retain_pane` reads it back during the SAME
        // `tree.ui()` call, so egui_tiles prunes exactly the closed pane and
        // preserves the rest of the user's arrangement (no full rebuild).
        let closes: RefCell<Vec<crate::grid::DocId>> = RefCell::new(Vec::new());
        let seen = ActivePaneObserved::default();
        // A queued find-navigate / go-to-line scroll. NOTHING consumed this in
        // grid view before, so those jumps selected the match off-screen and the
        // viewport never moved. Taken here and handed to the ACTIVE pane; if that
        // pane owns a widget-built scroll surface (a rope pane) it goes back on
        // `self` afterwards for `drive_embedded_scroll` instead.
        let mut pending_scroll = self.pending_scroll.take();
        // Mirrors the single-pane `animated(false)` drag gate — see the long
        // ROOT-CAUSE note at the single-pane `ScrollArea` for why an animated
        // area clobbers the drag-autoscroll's own offset write.
        let drag_selecting = mc_primary_down && editor_focused;

        egui::CentralPanel::default().show(ctx, |ui| {
            let tabs = &mut self.tabs;
            let render_closes = &closes;
            let seen = &seen;
            let mut render_body = |ui: &mut egui::Ui, doc_id: crate::grid::DocId| -> bool {
                let Some(idx) = tabs.iter().position(|t| t.doc_id == doc_id) else {
                    ui.weak("(document closed)");
                    return false;
                };
                // Per-pane header chip (wide one-row / narrow centered column, pin +
                // close + drag handle) extracted verbatim into grid_render::render_pane_header.
                let is_active = active_doc == doc_id;
                let drag_started = grid_render::render_pane_header(
                    ui,
                    &mut tabs[idx],
                    doc_id,
                    is_active,
                    accent,
                    muted,
                    render_closes,
                );
                // Per-pane syntax highlighting via the same memoizing layouter
                // the single-pane + split paths use, keyed on THIS pane's own
                // language hint — so each pane highlights for its own file type
                // instead of the old plain-text downgrade. The shared single-
                // slot `hl_cache` recomputes as focus moves between panes, which
                // is fine at the 6-pane ceiling.
                let ext = tabs[idx].doc.language_hint();
                let pane_id = pane_editor_id(doc_id);

                // ---- Read-only huge-file browse (single-pane KEYSTONE parity) ----
                // A read-only-large document keeps its bytes ONLY in the
                // document rope — `tab.text` is empty for it. The old bare
                // `TextEdit::multiline(&mut tabs[idx].text)` therefore rendered a
                // multi-hundred-MiB file as a BLANK pane that also accepted
                // typing, so a keystroke would have written into a buffer that is
                // supposed to be read-only. Render the same viewport-culled
                // `RopeEditor` the single-pane path uses.
                if tabs[idx].doc.is_read_only_large() {
                    let rope = tabs[idx].doc.rope().clone();
                    // Exact content height: `RopeEditor` lays out through
                    // `ScrollArea::show_rows(ui, line_h, total_lines, ..)`.
                    let content_h = rope.len_lines() as f32 * gutter_row_h;
                    let scroll_id = super::drag_scroll::embedded_scroll_id(
                        ui,
                        super::drag_scroll::DEFAULT_SCROLL_SALT,
                    );
                    let viewport = ui.max_rect();
                    let mut buf = scribe_core::buffer::Buffer::Rope(rope);
                    scribe_render::RopeEditor::new(&mut buf, font.clone(), gutter_row_h)
                        .with_text_color(layout_fg)
                        .with_gutter_color(muted)
                        .with_line_numbers(show_line_numbers)
                        .with_syntax(hl, ext.clone())
                        .show(ui);
                    if is_active {
                        let off_y = egui::scroll_area::State::load(ui.ctx(), scroll_id)
                            .map_or(0.0, |s| s.offset.y);
                        seen.metrics.set(Some((
                            off_y,
                            content_h.max(1.0),
                            viewport.height().max(1.0),
                        )));
                        seen.viewport.set(Some(viewport));
                        seen.focus_id
                            .set(Some(super::drag_scroll::rope_editor_focus_id(ui)));
                        seen.embedded_scroll_id.set(Some(scroll_id));
                        seen.mode.set(Some(EditorMode::ReadOnlyLarge));
                    }
                    return drag_started;
                }

                // ---- Owned rope editor (opt-in, or auto past the threshold) ----
                // Same swap the single-pane path performs. Without it a pane
                // holding a 20 MiB note laid the WHOLE buffer out in a `TextEdit`
                // every frame — the O(n)-per-frame cost the rope editor exists to
                // avoid — while the status bar (fed by the single-pane path)
                // claimed the file was on the rope editor anyway.
                if use_rope_editor(experimental_rope, tabs[idx].text.len(), rope_threshold) {
                    let scroll_id = super::drag_scroll::embedded_scroll_id(
                        ui,
                        super::drag_scroll::DEFAULT_SCROLL_SALT,
                    );
                    let focus_id = super::drag_scroll::rope_editor_focus_id(ui);
                    let viewport = ui.max_rect();
                    let tab = &mut tabs[idx];
                    // Lazily (re)build the persistent rope from `text`, as a
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
                            .with_text_color(layout_fg)
                            .with_gutter_color(muted)
                            .with_line_numbers(show_line_numbers)
                            .with_render_whitespace(render_whitespace)
                            .with_syntax(hl, ext.clone());
                    if snippets_enabled {
                        editor = editor.with_snippets(snippets);
                    }
                    let (resp, clipboard) = editor.show_editable(ui, state);
                    // Sync `text` from the rope ONLY on a real content edit — the
                    // O(n) `to_string()` runs on keystrokes, not every frame.
                    if resp.content_changed {
                        if let Some(rope) = tab.rope_buf.as_ref().and_then(|b| b.as_rope()) {
                            tab.text = rope.to_string();
                            tab.doc.mark_dirty();
                        }
                        tab.edit_gen = tab.edit_gen.wrapping_add(1);
                    }
                    let content_h = tab
                        .rope_buf
                        .as_ref()
                        .and_then(scribe_core::buffer::Buffer::as_rope)
                        .map_or(1.0, |r| r.len_lines() as f32 * gutter_row_h);
                    if let Some(text) = clipboard {
                        *seen.clipboard.borrow_mut() = Some(text);
                    }
                    if is_active {
                        let off_y = egui::scroll_area::State::load(ui.ctx(), scroll_id)
                            .map_or(0.0, |s| s.offset.y);
                        seen.metrics.set(Some((
                            off_y,
                            content_h.max(1.0),
                            viewport.height().max(1.0),
                        )));
                        seen.viewport.set(Some(viewport));
                        seen.focus_id.set(Some(focus_id));
                        seen.embedded_scroll_id.set(Some(scroll_id));
                        seen.mode
                            .set(Some(EditorMode::from_buffer_mode(&resp.buffer_mode)));
                    }
                    return drag_started;
                }

                // ---- Default egui `TextEdit` pane ----
                let mut layouter = make_layouter(
                    hl,
                    hl_cache,
                    hl_galley_cache,
                    hl_inc_cache,
                    ext.as_deref(),
                    font.clone(),
                    line_height,
                    word_wrap,
                    layout_fg,
                    url_color,
                    detect_links,
                );
                let mut sa = egui::ScrollArea::both().id_salt(("scr1b3-grid-pane", doc_id.raw()));
                if is_active {
                    if drag_selecting {
                        sa = sa.animated(false);
                    }
                    if let Some(off) = pending_scroll.take() {
                        sa = sa.vertical_scroll_offset(off);
                    }
                }
                let sa_out = sa.show(ui, |ui| {
                    let vp_h = ui.available_height();
                    let editor = egui::TextEdit::multiline(&mut tabs[idx].text)
                        .id(pane_id)
                        .code_editor()
                        .desired_width(f32::INFINITY)
                        .desired_rows(20)
                        .lock_focus(true)
                        .layouter(&mut layouter);
                    let out = editor.show(ui);
                    // Wave-3: per-pane edit-gen bump (grid panes share the
                    // single-slot caches; a focus/edit change is a key change).
                    if out.response.changed() {
                        tabs[idx].edit_gen = tabs[idx].edit_gen.wrapping_add(1);
                    }
                    // A right-click makes this pane the one the menu's commands
                    // act on — recorded BEFORE the menu is built so the pick is
                    // drained against the right document.
                    if out.response.secondary_clicked() {
                        seen.menu_pane.set(Some(doc_id));
                    }
                    // Right-click context menu — the same builtin actions the
                    // single-pane editor offers, routed through the SAME ctx-data
                    // slot and therefore the same `execute_builtin` dispatch.
                    let pane_read_only = tabs[idx].doc.is_read_only_large();
                    out.response.context_menu(|ui| {
                        use crate::app::commands::BuiltinCommand as B;
                        ui.set_min_width(200.0);
                        let pick = |ui: &mut egui::Ui, label: &str, cmd: B| {
                            if ui.button(label).clicked() {
                                ui.ctx().data_mut(|d| {
                                    d.insert_temp(super::frame_tick::editor_ctx_cmd_id(), cmd);
                                });
                                ui.close_menu();
                            }
                        };
                        pick(ui, "Cut", B::Cut);
                        pick(ui, "Copy", B::Copy);
                        if !pane_read_only {
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
                    if is_active {
                        // ---- P2 multi-cursor — galley-resolved gestures + paint ----
                        if let Some(p) = mc_ptr {
                            let local = p - out.galley_pos;
                            if mc_alt_press {
                                seen.mc_alt_anchor_idx
                                    .set(Some(out.galley.cursor_from_pos(local).index));
                            } else if mc_alt_drag {
                                seen.mc_alt_head_idx
                                    .set(Some(out.galley.cursor_from_pos(local).index));
                            } else if mc_ctrl_click {
                                seen.mc_ctrl_click_idx
                                    .set(Some(out.galley.cursor_from_pos(local).index));
                            }
                        }
                        if !mc_secondaries.is_empty() {
                            paint_secondary_carets(ui, &out, &mc_secondaries, accent);
                        }
                        if let Some(range) = out.cursor_range {
                            let cc = range.primary;
                            let rect = out.galley.pos_from_cursor(cc);
                            seen.caret.set(Some(PaneCaret {
                                screen_bottom_y: out.galley_pos.y + rect.max.y,
                                line_col: line_col_from_char_index(&tabs[idx].text, cc.index),
                                selection_chars: range
                                    .primary
                                    .index
                                    .abs_diff(range.secondary.index),
                            }));
                        }
                        // Capture each logical line's screen Y for the external
                        // gutter panel (a row starts a logical line iff the
                        // previous row ended with `\n`). Without this the gutter
                        // kept painting whatever the last single-pane frame left
                        // in `line_gutter`, so its numbers, bookmark dots and
                        // change bars sat at stale positions the whole time the
                        // grid was open.
                        if show_line_numbers {
                            let mut rows = seen.gutter.borrow_mut();
                            rows.clear();
                            let top = out.galley_pos.y;
                            let mut prev_newline = true;
                            for row in &out.galley.rows {
                                if prev_newline {
                                    rows.push(top + row.rect().min.y);
                                }
                                prev_newline = row.ends_with_newline;
                            }
                        }
                    }
                    // ---- URL + [[wiki-link]] overlay (single-pane parity) ----
                    // Deliberately NOT `is_active`-gated: the pointer can only
                    // be inside one pane's editor rect (the helper's own first
                    // test), and requiring a focus click first would make
                    // following a link in a side pane a two-gesture operation
                    // the single-pane editor never asked for.
                    grid_render::pane_link_overlay(
                        ui,
                        &out,
                        &tabs[idx].text,
                        detect_links,
                        vault_configured,
                    );
                    // ---- Inline LSP diagnostics (single-pane parity) ----
                    // `is_active`-gated because `diag_spans` were resolved onto
                    // the ACTIVE tab's byte offsets; painting them over another
                    // pane's galley would underline unrelated text.
                    if is_active {
                        grid_render::paint_pane_diagnostics(
                            ui,
                            &out,
                            &tabs[idx].text,
                            &diag_spans,
                            diag_colors,
                        );
                    }
                    // P1-3 scroll-past-end: pad blank space below the last line
                    // so it can rest at a comfortable height (VS Code
                    // `scrollBeyondLastLine`), matching the single-pane path.
                    if scroll_past_end {
                        ui.add_space(vp_h * 0.6);
                    }
                    // #28 — same render-whitespace overlay as the single-pane
                    // editor, so the markers appear in split/grid view too.
                    if render_whitespace {
                        paint_whitespace_markers(ui, &out, editor_font_size, muted);
                    }
                });
                if is_active {
                    seen.metrics.set(Some((
                        sa_out.state.offset.y,
                        sa_out.content_size.y.max(1.0),
                        sa_out.inner_rect.height().max(1.0),
                    )));
                    seen.viewport.set(Some(sa_out.inner_rect));
                    seen.focus_id.set(Some(pane_id));
                    seen.mode.set(Some(EditorMode::Standard));
                }
                drag_started
            };
            let mut behavior = crate::grid::AppGridBehavior {
                titles: &titles,
                render_body: &mut render_body,
                close_requests: &closes,
                // Thin theme-accent divider between panes (muted). Recomputed
                // from `accent` each frame so it follows a live theme change.
                divider: crate::grid::divider_color(accent),
            };
            tree.ui(&mut behavior, ui);
        });

        // The active pane may not have rendered a `TextEdit` (a rope pane owns
        // its own scroll area) or may not be gridded at all, so hand back
        // anything the pane did not consume.
        self.pending_scroll = pending_scroll;
        self.apply_active_pane_observations(ctx, &seen, mc_prev_primary, gutter_row_h);

        // Phase 18 T18.2 / #R6 — 6-pane cap, now actually ENFORCED:
        // `build_default_grid` caps the tree at MAX_PANES panes, so the grid
        // never shows more than six. When more tabs than that are open, the
        // extras stay open as tabs and we tell the user why they aren't gridded.
        let shown = crate::grid::count_panes(&tree);
        if self.tabs.len() > shown {
            self.toast = Some(format!(
                "Grid shows the first {} notes; {} more stay open as tabs. Close a pane to \
                 show another.",
                shown,
                self.tabs.len() - shown
            ));
        }
        // Drop the tabs the user closed via the pane chrome. `retain_pane`
        // already pruned the matching pane(s) during the frame, so here we only
        // remove the backing tabs — the surviving panes keep their positions.
        let to_close = closes.into_inner();
        if !to_close.is_empty() {
            for doc_id in to_close {
                self.tabs.retain(|t| t.doc_id != doc_id);
            }
            if self.tabs.is_empty() {
                self.tabs.push(EditorTab::scratch());
            }
            self.active = self.active.min(self.tabs.len() - 1);
        }
        // Reconcile additions: a tab opened while the grid is live has no pane
        // yet. Rebuild ONLY when the (capped) doc set actually differs from the
        // pane set, so steady-state editing and drag-rearranging never reset the
        // layout. The want-set is capped to MAX_PANES to match the capped tree —
        // otherwise a 7th tab would force a rebuild every frame.
        let docs: Vec<crate::grid::DocId> = self
            .tabs
            .iter()
            .map(|t| t.doc_id)
            .take(crate::grid::MAX_PANES)
            .collect();
        let want: std::collections::BTreeSet<crate::grid::DocId> = docs.iter().copied().collect();
        if want != crate::grid::pane_doc_ids(&tree) {
            tree = crate::grid::build_default_grid(&docs);
        }
        self.grid_tree = Some(tree);
    }

    /// Apply everything the active pane observed during `tree.ui()`.
    ///
    /// Split out so the borrow-constrained closure stays readable: each item
    /// here needs `&mut self`, which is unavailable while `self.tabs` is loaned
    /// to the pane renderer.
    fn apply_active_pane_observations(
        &mut self,
        ctx: &egui::Context,
        seen: &ActivePaneObserved,
        mc_prev_primary: Option<usize>,
        line_px: f32,
    ) {
        if self.tabs.is_empty() {
            return;
        }
        // A right-click retargets the active tab BEFORE the menu pick is
        // drained, so "Bold" chosen in pane 2 emboldens pane 2.
        if let Some(doc_id) = seen.menu_pane.get() {
            if let Some(i) = self.tabs.iter().position(|t| t.doc_id == doc_id) {
                self.active = i;
            }
        }
        let active = self.active.min(self.tabs.len() - 1);
        self.active = active;
        let editor_id = seen
            .focus_id
            .get()
            .unwrap_or_else(|| pane_editor_id(self.tabs[active].doc_id));

        // Status-bar caret readout + the external gutter's row positions. Both
        // were frozen on the last single-pane frame's values in grid view.
        if let Some(caret) = seen.caret.get() {
            self.last_cursor_line_col = Some(caret.line_col);
            self.last_selection_chars = caret.selection_chars;
        }
        {
            let rows = seen.gutter.borrow();
            if !rows.is_empty() {
                self.line_gutter = rows.clone();
            }
        }

        // ---- P2 multi-cursor — resolve the galley-dependent gestures ----
        if let Some(ix) = seen.mc_ctrl_click_idx.get() {
            // Reconcile against the PRE-click primary head: egui may already have
            // moved its live primary onto the click point this frame, which would
            // spuriously read as a hit on an existing caret.
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
        if let Some(idx) = seen.mc_alt_anchor_idx.get() {
            self.column_anchor = Some(idx);
            self.multi_cursor.clear();
        }
        if let (Some(anchor_idx), Some(head_idx)) = (self.column_anchor, seen.mc_alt_head_idx.get())
        {
            let chars: Vec<char> = self.tabs[active].text.chars().collect();
            let mut carets = crate::multi_cursor::column_selection(&chars, anchor_idx, head_idx);
            if carets.len() >= 2 {
                let pix = carets
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, c)| (c.head as isize - head_idx as isize).unsigned_abs())
                    .map_or(0, |(i, _)| i);
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
                super::multi_cursor_glue::mc_set_primary(ctx, editor_id, c.anchor, c.head);
                self.multi_cursor.clear();
                ctx.request_repaint();
            }
        }
        self.mc_record_owner(active);

        // ---- Scroll metrics + the scroll assists ----
        // `scroll_metrics` drives the minimap's viewport indicator and is the
        // measurement BOTH assists read, so it must land before either runs.
        // Grid view never wrote it, which left the minimap frozen on the last
        // single-pane frame's numbers and made PageUp/PageDown inert.
        if let Some(metrics) = seen.metrics.get() {
            self.scroll_metrics = metrics;
        }
        // A rope pane builds its own `ScrollArea`, so a queued jump is pushed
        // into that widget's persisted state instead. It lands on the pane's
        // NEXT frame (the widget has already run this one) — the same
        // one-frame lag the minimap and the mode badge already carry.
        if let Some(scroll_id) = seen.embedded_scroll_id.get() {
            self.drive_embedded_scroll(ctx, scroll_id);
        }
        if let Some(viewport) = seen.viewport.get() {
            // Also runs `page_key_assist` (PageUp/PageDown), which the grid had
            // no implementation of at all.
            self.drag_scroll_assist(ctx, editor_id, viewport);
            if let Some(caret) = seen.caret.get() {
                self.caret_scroll_off_assist(
                    ctx,
                    caret.screen_bottom_y,
                    viewport,
                    line_px.max(1.0),
                );
            }
        }

        // The badge must describe the surface the user is EDITING. When the
        // active tab is past the 6-pane cap no pane rendered it, so fall back to
        // the surface it WOULD render on rather than claiming the full-feature
        // default.
        let mode = seen
            .mode
            .get()
            .unwrap_or_else(|| self.editor_mode_for_tab(active));
        publish_editor_mode(ctx, mode);

        // A right-click pick stashed on a previous frame; dispatch it now that
        // `self` is mutable, then apply whatever `pending_*` it raised against
        // the active pane's editor id.
        if let Some(cmd) = ctx.data_mut(|d| {
            let id = super::frame_tick::editor_ctx_cmd_id();
            let v = d.get_temp::<crate::app::commands::BuiltinCommand>(id);
            d.remove::<crate::app::commands::BuiltinCommand>(id);
            v
        }) {
            self.execute_builtin(cmd);
        }
        self.apply_pending_caret_ops(ctx, editor_id, active);

        if let Some(text) = seen.clipboard.borrow_mut().take() {
            // On Cut the selection is already removed from the buffer, so a
            // clipboard failure means the text is only undo-recoverable — log it
            // rather than swallow it silently.
            match arboard::Clipboard::new() {
                Ok(mut cb) => {
                    if let Err(e) = cb.set_text(text) {
                        tracing::warn!(
                            "clipboard write after cut/copy failed; text is still \
                             undo-recoverable: {e}"
                        );
                    }
                }
                Err(e) => tracing::warn!(
                    "could not open the clipboard for cut/copy; text is still \
                     undo-recoverable: {e}"
                ),
            }
        }
    }

    /// Drain the caret-op requests raised by the command palette, a keyboard
    /// chord or the right-click menu, applying each to `active` through
    /// `editor_id`'s `TextEditState`.
    ///
    /// These flags were set but NEVER consumed while the grid was on: choosing
    /// "Bold" from the palette in split view latched `pending_wrap_marker`
    /// forever and did nothing, and the latch then fired the moment the user
    /// turned the grid back off.
    pub(super) fn apply_pending_caret_ops(
        &mut self,
        ctx: &egui::Context,
        editor_id: egui::Id,
        active: usize,
    ) {
        if active >= self.tabs.len() || self.tabs[active].doc.is_read_only_large() {
            return;
        }
        if std::mem::take(&mut self.pending_jump_bracket) {
            self.jump_matching_bracket(ctx, editor_id, active);
        }
        if std::mem::take(&mut self.pending_insert_datetime) {
            self.insert_datetime_at_caret(ctx, editor_id, active);
        }
        if std::mem::take(&mut self.pending_dup_selection) {
            self.duplicate_selection(ctx, editor_id, active);
        }
        if std::mem::take(&mut self.pending_toggle_task) {
            self.toggle_task_checkbox_active(ctx, editor_id, active);
        }
        if let Some(marker) = self.pending_wrap_marker.take() {
            self.wrap_selection_active(ctx, editor_id, active, marker);
        }
        if let Some(op) = self.pending_case.take() {
            self.case_selection_active(ctx, editor_id, active, op);
        }
        if std::mem::take(&mut self.pending_format_table) {
            self.format_table_active(ctx, editor_id, active);
        }
    }

    /// Which surface tab `idx` WOULD render on, for the one case the grid can
    /// observe no pane: an active tab past the `MAX_PANES` cap.
    fn editor_mode_for_tab(&self, idx: usize) -> EditorMode {
        let Some(tab) = self.tabs.get(idx) else {
            return EditorMode::Standard;
        };
        if tab.doc.is_read_only_large() {
            EditorMode::ReadOnlyLarge
        } else if use_rope_editor(
            self.config.editor.experimental_rope_editor,
            tab.text.len(),
            self.config.editor.rope_editor_auto_threshold_bytes,
        ) {
            // The editable rope path always builds a `Buffer::Rope` from `text`;
            // only a document that opened memory-mapped reports `Mmap`, and such
            // a document is `read_only_large` and handled above.
            EditorMode::Rope
        } else {
            EditorMode::Standard
        }
    }

    /// Phase 18 T18.2 — assign stable doc_ids to any tab missing one
    /// (e.g. restored from a pre-grid session). Then ensure the
    /// `grid_tree` matches the user's `editor.grid_enabled` preference.
    /// Called at the top of `update` so the grid catches up to any
    /// config-reload that flipped the flag.
    pub(super) fn sync_grid_state(&mut self) {
        // Pass 1: fill missing doc_ids so the grid has a stable id to
        // reference. DocId(0) is the legacy / unallocated sentinel.
        for tab in self.tabs.iter_mut() {
            if tab.doc_id.0 == 0 {
                // The allocator reserves 0 and starts at 1, so a single next()
                // always yields a real (non-sentinel) id.
                tab.doc_id = self.next_doc_id.next();
            }
            self.next_doc_id.observe(tab.doc_id);
        }
        // Pass 2: align tree state with the config flag.
        match (self.config.editor.grid_enabled, self.grid_tree.is_some()) {
            (true, false) => {
                let docs: Vec<crate::grid::DocId> = self
                    .tabs
                    .iter()
                    .map(|t| t.doc_id)
                    .take(crate::grid::MAX_PANES)
                    .collect();
                // #R6 — restore the persisted layout if it still references
                // exactly the reopened doc set (DocIds are assigned in tab order,
                // so a stable session reproduces them); otherwise fall back to a
                // fresh default grid. A corrupt/stale layout never blocks startup.
                let want: std::collections::BTreeSet<crate::grid::DocId> =
                    docs.iter().copied().collect();
                let restored = self
                    .config
                    .editor
                    .grid_layout
                    .as_deref()
                    .and_then(crate::grid::from_json)
                    .filter(|t| crate::grid::pane_doc_ids(t) == want);
                self.grid_tree =
                    Some(restored.unwrap_or_else(|| crate::grid::build_default_grid(&docs)));
            }
            (false, true) => {
                self.grid_tree = None;
                self.grid_close_queue.clear();
            }
            _ => {}
        }
    }
}

/// Paint each secondary caret (and its single-row selection band) so
/// multi-cursor renders distinctly from egui's own primary caret. Lifted from
/// the single-pane path so both surfaces draw an identical caret set.
fn paint_secondary_carets(
    ui: &egui::Ui,
    out: &egui::text_edit::TextEditOutput,
    secondaries: &[crate::multi_cursor::Caret],
    accent: Color32,
) {
    let painter = ui.painter();
    let gp = out.galley_pos;
    for c in secondaries {
        if !c.is_empty() {
            let r = c.range();
            let rs = out
                .galley
                .pos_from_cursor(egui::text::CCursor::new(r.start));
            let re = out.galley.pos_from_cursor(egui::text::CCursor::new(r.end));
            if (rs.min.y - re.min.y).abs() < 0.5 {
                let sel = egui::Rect::from_min_max(
                    gp + egui::vec2(rs.min.x, rs.min.y),
                    gp + egui::vec2(re.max.x, re.max.y),
                );
                painter.rect_filled(sel, 0.0, accent.linear_multiply(0.30));
            }
        }
        let cr = out.galley.pos_from_cursor(egui::text::CCursor::new(c.head));
        painter.line_segment(
            [
                gp + egui::vec2(cr.min.x, cr.min.y),
                gp + egui::vec2(cr.min.x, cr.max.y),
            ],
            egui::Stroke::new(1.5, accent),
        );
    }
}

/// #28 — paint the `·` / `→` render-whitespace markers over a laid-out galley.
fn paint_whitespace_markers(
    ui: &egui::Ui,
    out: &egui::text_edit::TextEditOutput,
    font_size: f32,
    muted: Color32,
) {
    let painter = ui.painter();
    let ws_font = egui::FontId::monospace(font_size);
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
