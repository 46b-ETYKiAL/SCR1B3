//! Keyboard-shortcut handling for `frame_tick`, extracted from `mod.rs` (A-01 wave 3).
//!
//! Every chord here resolves from the user's `[keybindings]` config via
//! [`Keymap`](super::keymap::Keymap) — the config section is authoritative, not
//! decorative. The shipped defaults reproduce the chords this handler used to
//! hard-wire, so a user who never touches `[keybindings]` sees the same editor.
//!
//! Two deliberate differences from the old hard-wired form:
//! - **Modifiers match exactly.** The hard-wired tests were `cmd && key_pressed(K)`,
//!   which let Ctrl+Shift+S also fire plain Save and forced hand-written
//!   `!i.modifiers.shift` guards wherever a `mod+shift+…` action would otherwise
//!   collide. Exact matching makes those guards unnecessary and makes each
//!   binding mean one chord.
//! - **Only mapped actions are rebindable.** F1 (cheatsheet), Esc (close overlay),
//!   F3 (find-next) and Ctrl+scroll zoom stay hard-wired: they are not actions in
//!   the `[keybindings]` schema.
//!
//! This module also owns the whole **file-drop pipe**, both halves of it: the
//! drop itself (`RawInput.dropped_files` -> `Pending::files_to_open`) and the
//! drag-hover feedback that tells the user the window will accept the drop
//! (`RawInput.hovered_files` -> the drop-target overlay). The two live together
//! because they are one gesture; splitting them is how the hover half went
//! missing while the drop half worked.
#![allow(clippy::wildcard_imports)]

use super::keymap::{action, Keymap};
use super::*;

/// Zoom-gesture deadzone, as a `zoom_delta()` multiplier.
///
/// Keeps the feel of the +/-0.5-point scroll deadzone this replaced: at egui's
/// default `scroll_zoom_speed` (1/200) a 0.5-point wheel step is
/// `exp(0.5/200) ~= 1.0025`. Below it, trackpad jitter must not resize the font.
const ZOOM_DEADZONE: f32 = 1.0025;

/// The drop-target copy shown while `n` files are dragged over the window.
///
/// Pure, so what the overlay SAYS is assertable without rendering: the count and
/// the singular/plural form are the two things a user reads off it, and both are
/// pinned by `drop_target_hint_counts_and_pluralises`.
pub(super) fn drop_target_hint(n: usize) -> String {
    if n == 1 {
        "Drop to open 1 file".to_string()
    } else {
        format!("Drop to open {n} files")
    }
}

impl ScribeApp {
    /// F-011 companion — paint the drop-target overlay for `hovered` dragged
    /// files.
    ///
    /// Drawn into a FOREGROUND layer over the whole screen rect rather than into
    /// some panel's `Ui`, because a drop lands anywhere on the window: the
    /// feedback has to cover the same area the drop does. `hovered == 0` (no
    /// drag in flight) paints nothing at all — the overlay must never be visible
    /// during normal editing.
    fn paint_drop_target(&self, ctx: &egui::Context, hovered: usize) {
        if hovered == 0 {
            return;
        }
        let screen = ctx.screen_rect();
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("scr1b3-drop-target"),
        ));
        let style = ctx.style();
        let accent = style.visuals.selection.bg_fill;
        let text_color = style.visuals.strong_text_color();
        painter.rect_filled(screen, 0.0, egui::Color32::from_black_alpha(150));
        painter.rect_stroke(
            screen.shrink(10.0),
            8.0,
            egui::Stroke::new(2.0, accent),
            egui::StrokeKind::Inside,
        );
        painter.text(
            screen.center(),
            egui::Align2::CENTER_CENTER,
            drop_target_hint(hovered),
            egui::FontId::proportional(20.0),
            text_color,
        );
    }

    /// Collect this frame's keyboard shortcuts into `act` (a `Pending` action
    /// set) and record the find-bar F3 navigation direction in `find_nav`.
    ///
    /// Chords come from the user's keymap; the surrounding app-state logic (what
    /// an action does, which overlay it focuses) is unchanged.
    pub(super) fn handle_keyboard_shortcuts(
        &mut self,
        ctx: &egui::Context,
        act: &mut Pending,
        find_nav: &mut Option<bool>,
    ) {
        // The Settings → Keyboard page is waiting for a chord: stand down for
        // this frame. Otherwise pressing Ctrl+S to REBIND save would also save
        // the file, and the Escape that cancels a capture would also close every
        // overlay — the rebind UI would fire the very actions it is rebinding.
        //
        // Gated on the Settings window being OPEN as well as the capture flag, so
        // a capture the user walked away from can never leave the editor's whole
        // shortcut layer suppressed (the flag is also cleared when the window
        // closes; this is the belt to that suspenders).
        if self.settings_open && crate::app::settings_keys::capture_active(ctx) {
            return;
        }

        // Config live-reloads, so re-resolve when (and only when) the user's
        // bindings actually changed. `Keymap` owns its data, which keeps
        // `self` free to be mutated inside the `ctx.input` closure below.
        if self.keymap_src != self.config.keybindings {
            self.keymap_src = self.config.keybindings.clone();
            self.keymap = Keymap::resolve(&self.keymap_src);
        }
        let km = &self.keymap;

        // Applied AFTER the input closure, for the same reason `find_nav` is:
        // `save_as_active` re-borrows `self` (and opens a blocking OS dialog),
        // which must not happen while egui's input lock is held. It does not go
        // through `Pending` because nothing downstream needs to observe or
        // reorder it — the flag never outlives this call.
        let mut want_save_as = false;
        // Files being DRAGGED over the window this frame (0 == no drag). Read
        // inside the closure, painted after it.
        let mut hovered_files = 0usize;

        ctx.input(|i| {
            act.new = km.pressed(i, action::NEW_FILE);
            act.open = km.pressed(i, action::OPEN_FILE);
            act.save = km.pressed(i, action::SAVE);
            // Save As… — a distinct action, not a shifted Save. Exact modifier
            // matching is what keeps `mod+s` and `mod+shift+s` apart, so this
            // needs no `!shift` guard on the Save above.
            if km.pressed(i, action::SAVE_AS) {
                want_save_as = true;
            }
            if km.pressed(i, action::FIND) {
                if !self.find_open {
                    self.focus_find = true;
                }
                self.find_open = true;
            }
            // Wave-5: project-wide find (find in files).
            if km.pressed(i, action::FIND_IN_FILES) {
                if !self.find_in_files_open {
                    self.focus_find_in_files = true;
                }
                self.find_in_files_open = true;
            }
            // Wave-5 P1: zen / distraction-free mode. Entering zen also closes the
            // find bars so nothing but the editor remains.
            if km.pressed(i, action::TOGGLE_ZEN) {
                self.zen_mode = !self.zen_mode;
                if self.zen_mode {
                    self.find_open = false;
                    self.find_in_files_open = false;
                }
            }
            // Wave-5 P1: toggle the markdown live-preview panel.
            if km.pressed(i, action::TOGGLE_MD_PREVIEW) {
                self.md_preview_open = !self.md_preview_open;
            }
            // The command palette (plugin + builtin cmds).
            if km.pressed(i, action::COMMAND_PALETTE) {
                if !self.palette_open {
                    self.focus_palette = true;
                }
                self.palette_open = true;
                self.palette_query.clear();
                // BUG-APP-01: a fresh open starts the keyboard highlight at the
                // top so Enter runs the first match.
                self.palette_selected = 0;
            }
            // F-006 fix from docs/audits/overlooked-surfaces-2026-05-29.md —
            // wave 1 keyboard shortcuts: close the active tab, toggle the
            // multi-note grid (F-003 entry-point fix), cycle tabs.
            if km.pressed(i, action::CLOSE_TAB) {
                act.close_active_tab = true;
            }
            if km.pressed(i, action::TOGGLE_GRID) {
                act.toggle_grid = true;
            }
            // Wave-2 keyboard fill-in (docs/audits/overlooked-surfaces-2026-05-29.md).
            if km.pressed(i, action::REPLACE) {
                act.open_replace = true;
            }
            if km.pressed(i, action::TOGGLE_COMMENT) {
                act.toggle_comment = true;
            }
            // Jump to the matching bracket.
            if km.pressed(i, action::JUMP_BRACKET) {
                act.jump_bracket = true;
            }
            if km.pressed(i, action::TOGGLE_FULLSCREEN) {
                act.toggle_fullscreen = true;
            }
            // F-018 — Ctrl+K Ctrl+T (cycle theme) approximated as a single-key
            // chord since egui has no native multi-key chord layer. F-031 —
            // toggle the minimap. Both persist via save_config.
            if km.pressed(i, action::CYCLE_THEME) {
                act.cycle_theme = true;
            }
            if km.pressed(i, action::TOGGLE_MINIMAP) {
                act.toggle_minimap = true;
            }
            // F-032 — fold every region in the active buffer / expand every
            // region. Switches the editor into fold-view mode so the user sees
            // the change immediately (otherwise the fold set is updated but the
            // normal central panel doesn't honor it).
            if km.pressed(i, action::FOLD_ALL) {
                act.fold_all = true;
            }
            if km.pressed(i, action::EXPAND_ALL) {
                act.expand_all = true;
            }
            // Font zoom: bound chords in / out / reset, plus hard-wired
            // Ctrl+scroll. Universal editor convenience.
            if km.pressed(i, action::INCREASE_FONT) {
                act.font_zoom = Some(1);
            }
            if km.pressed(i, action::DECREASE_FONT) {
                act.font_zoom = Some(-1);
            }
            if km.pressed(i, action::RESET_FONT) {
                act.font_zoom = Some(0);
            }
            // Ctrl+scroll never reached this handler. egui's `zoom_modifier`
            // defaults to COMMAND, so when a wheel event carries Ctrl (which
            // egui-winit always attaches) egui folds it into `zoom_factor_delta`
            // and leaves `smooth_scroll_delta` at ZERO. Reading the scroll delta
            // under `if cmd` was therefore dead: `dy` was always 0.0.
            //
            // `zoom_delta()` is the signal egui actually publishes for "the user
            // wants to zoom" — and it reports trackpad pinch too, so that now
            // zooms the font as well. It is a multiplier: 1.0 means no gesture.
            let zoom = i.zoom_delta();
            if zoom > ZOOM_DEADZONE {
                act.font_zoom = Some(1);
            } else if zoom < 1.0 / ZOOM_DEADZONE {
                act.font_zoom = Some(-1);
            }
            // Reopen the most recently closed tab (the default is Ctrl+Shift+R —
            // Ctrl+Shift+T is already the theme-cycle chord in this editor).
            if km.pressed(i, action::REOPEN_TAB) {
                act.reopen_tab = true;
            }
            // F-017 — move the cursor line up/down, duplicate it, join the next.
            if km.pressed(i, action::MOVE_LINE_UP) {
                act.move_line_up = true;
            }
            if km.pressed(i, action::MOVE_LINE_DOWN) {
                act.move_line_down = true;
            }
            if km.pressed(i, action::DUPLICATE_LINE) {
                act.duplicate_line = true;
            }
            if km.pressed(i, action::JOIN_LINES) {
                act.join_lines = true;
            }
            // F-011 — drag-drop file open. egui collects DroppedFile entries
            // into RawInput.dropped_files; consume them here so the deferred
            // application opens each as a new tab.
            for file in i.raw.dropped_files.iter() {
                if let Some(p) = file.path.clone() {
                    act.files_to_open.push(p);
                }
            }
            // …and the other half of the same gesture: while a drag is still IN
            // FLIGHT egui reports it in `hovered_files`. Nothing read that, so
            // dragging a file over SCR1B3 gave no sign the window would accept
            // it — the drop worked but looked like it would not.
            hovered_files = i.raw.hovered_files.len();
            // Ctrl+1..9 — activate a tab by its 1-based index. `break` because
            // one press activates at most one tab: if a user binds two indices
            // to one combo (`validate` reports that as a Conflict), the earlier
            // index wins rather than both running.
            for (idx, tab_action) in action::GOTO_TAB.iter().enumerate() {
                if km.pressed(i, tab_action) {
                    if idx < self.tabs.len() {
                        self.active = idx;
                    } else {
                        // Deliberately NOT clamped to the last tab: clamping
                        // would silently switch to a tab the user did not ask
                        // for. Say nothing happened, and say why.
                        self.status = format!("no tab {}", idx + 1);
                    }
                    break;
                }
            }
            // Tab cycling is suppressed while the completion popup is open — it
            // consumes Tab to accept a candidate.
            if km.pressed(i, action::NEXT_TAB) && self.completion.is_none() {
                act.cycle_tab_next = true;
            }
            if km.pressed(i, action::PREV_TAB) && self.completion.is_none() {
                act.cycle_tab_prev = true;
            }
            // F-014: F1 toggles the keyboard cheatsheet — universal "help"
            // convention, deliberately not rebindable. The Esc handler below
            // closes it like any overlay.
            if i.key_pressed(egui::Key::F1) {
                self.cheatsheet_open = !self.cheatsheet_open;
            }
            // F-015 — the go-to-line modal.
            if km.pressed(i, action::GOTO_LINE) {
                self.goto_open = true;
                self.focus_goto = true;
                self.goto_query.clear();
            }
            // The go-to-symbol modal (jump to a definition in the active buffer).
            if km.pressed(i, action::GOTO_SYMBOL) {
                if !self.goto_symbol_open {
                    self.focus_goto_symbol = true;
                }
                self.goto_symbol_open = true;
                self.goto_symbol_query.clear();
                self.goto_symbol_selected = 0;
            }
            // F-012 — the recent-files modal.
            if km.pressed(i, action::RECENT_FILES) {
                self.recent_open = true;
                self.recent_selected = 0;
            }
            // Line bookmarks: toggle on the cursor line, jump to the next, jump
            // to the previous. Exact modifier matching keeps the three F2 chords
            // (Ctrl+F2 / F2 / Shift+F2 by default) from shadowing each other.
            if km.pressed(i, action::TOGGLE_BOOKMARK) {
                act.toggle_bookmark = true;
            }
            if km.pressed(i, action::NEXT_BOOKMARK) {
                act.next_bookmark = true;
            }
            if km.pressed(i, action::PREV_BOOKMARK) {
                act.prev_bookmark = true;
            }
            // #R6 — F3 / Shift+F3 cycle find matches while the find bar is open.
            // Not rebindable: it is find-bar navigation, not a global action.
            if self.find_open && i.key_pressed(egui::Key::F3) {
                *find_nav = Some(!i.modifiers.shift);
            }
            // F-010 — the fuzzy file finder (rebuilds the file index on first
            // open so cold-start cost lands here, not on launch).
            if km.pressed(i, action::FUZZY_FINDER) {
                act.open_fuzzy = true;
            }
            if i.key_pressed(egui::Key::Escape) {
                // Esc exits zen mode / F11 fullscreen first so the chrome comes
                // back before any overlay close — one press to leave the
                // distraction-free / fullscreen surface.
                if self.zen_mode {
                    self.zen_mode = false;
                } else if i.viewport().fullscreen.unwrap_or(false) {
                    // Exit OS fullscreen via the existing deferred handler
                    // (it sends Fullscreen(false) since we are currently in it).
                    act.toggle_fullscreen = true;
                } else {
                    self.find_open = false;
                    self.palette_open = false;
                    self.cheatsheet_open = false;
                    self.goto_open = false;
                    self.goto_symbol_open = false;
                    self.recent_open = false;
                    self.recent_folders_open = false;
                    self.welcome_open = false;
                    self.fuzzy_open = false;
                    // PA-02: route the project-find results pane through the same
                    // centralized Esc-close as the other overlays.
                    self.find_in_files_open = false;
                }
            }
        });
        // Outside the input borrow: both of these re-enter `self` / `ctx`.
        self.paint_drop_target(ctx, hovered_files);
        if want_save_as {
            self.save_as_active();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive ONE `handle_keyboard_shortcuts` frame and hand back every string
    /// the frame PAINTED.
    ///
    /// Reading the painted text (rather than a bool on `self`) is what makes the
    /// drop-target assertions observable: the overlay's whole job is to be
    /// visible, and a flag saying "we would have painted" proves nothing about
    /// whether anything reached the screen.
    fn frame(
        app: &mut ScribeApp,
        mods: egui::Modifiers,
        events: Vec<egui::Event>,
        hovered: usize,
    ) -> Vec<String> {
        let ctx = egui::Context::default();
        let raw = egui::RawInput {
            modifiers: mods,
            events,
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(640.0, 480.0),
            )),
            hovered_files: vec![egui::HoveredFile::default(); hovered],
            ..Default::default()
        };
        let mut act = Pending::default();
        let mut nav = None;
        let out = ctx.run(raw, |ctx| {
            app.handle_keyboard_shortcuts(ctx, &mut act, &mut nav);
        });
        out.shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Text(t) => Some(t.galley.text().to_string()),
                _ => None,
            })
            .collect()
    }

    fn press(key: egui::Key, mods: egui::Modifiers) -> Vec<egui::Event> {
        vec![egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: mods,
        }]
    }

    /// An app with `n` tabs, active on the first.
    fn app_with_tabs(n: usize) -> ScribeApp {
        let mut app = ScribeApp::new_test(Config::default());
        while app.tabs.len() < n {
            app.new_tab();
        }
        app.active = 0;
        app
    }

    const CMD: egui::Modifiers = egui::Modifiers::COMMAND;

    // ---- Ctrl+1..9: switch to a tab by number ----

    #[test]
    fn each_number_chord_activates_the_tab_with_that_number() {
        // The observable outcome is WHICH tab is showing, so assert `active`
        // after each chord — and cover every one of the nine, because a table
        // wired only for the first index looks correct from a single-case test.
        let mut app = app_with_tabs(9);
        let keys = [
            egui::Key::Num1,
            egui::Key::Num2,
            egui::Key::Num3,
            egui::Key::Num4,
            egui::Key::Num5,
            egui::Key::Num6,
            egui::Key::Num7,
            egui::Key::Num8,
            egui::Key::Num9,
        ];
        for (idx, key) in keys.iter().enumerate() {
            // Park somewhere else first, so "already there" can never be
            // mistaken for "the chord worked".
            app.active = if idx == 0 { 8 } else { 0 };
            frame(&mut app, CMD, press(*key, CMD), 0);
            assert_eq!(
                app.active,
                idx,
                "Ctrl+{} must activate tab {}",
                idx + 1,
                idx + 1
            );
        }
    }

    #[test]
    fn a_number_past_the_last_tab_leaves_the_active_tab_alone() {
        // Clamping to the last tab would be a silent lie: the user asked for
        // tab 7 and would land on tab 3 without being told. The contract is
        // no-op plus an explicit status line.
        let mut app = app_with_tabs(3);
        app.active = 1;
        app.status = "untouched".into();
        frame(&mut app, CMD, press(egui::Key::Num7, CMD), 0);
        assert_eq!(app.active, 1, "an out-of-range number must not move tabs");
        assert_eq!(
            app.status, "no tab 7",
            "…and must say so, naming the tab that is not there"
        );
    }

    #[test]
    fn a_bare_number_keypress_does_not_switch_tabs() {
        // Typing "3" into a note is a character. Losing the command modifier
        // would turn every digit into a tab jump.
        let mut app = app_with_tabs(5);
        app.active = 0;
        frame(
            &mut app,
            egui::Modifiers::NONE,
            press(egui::Key::Num3, egui::Modifiers::NONE),
            0,
        );
        assert_eq!(app.active, 0, "a bare digit is text, not navigation");
    }

    // ---- Ctrl+Shift+S: Save As ----

    #[test]
    fn ctrl_shift_s_saves_the_buffer_under_the_newly_picked_path() {
        // End to end through the REAL save_as_active: only the OS dialog's
        // answer is injected. The evidence is a file on disk at the PICKED path
        // — a path the buffer was never associated with, so a Ctrl+Shift+S that
        // fell through to a plain in-place Save cannot produce it.
        let dir = std::env::temp_dir().join(format!(
            "scr1b3-keyboard-input-tests/save-as-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let picked = dir.join("picked.md");

        let mut app = ScribeApp::new_test(Config::default());
        let active = app.active;
        app.tabs[active].set_text("hello from ctrl+shift+s".into());
        crate::app::dialogs::test_hooks::set_next_save_path(picked.clone());

        frame(
            &mut app,
            CMD | egui::Modifiers::SHIFT,
            press(egui::Key::S, CMD | egui::Modifiers::SHIFT),
            0,
        );

        assert_eq!(
            std::fs::read_to_string(&picked).unwrap(),
            "hello from ctrl+shift+s",
            "Ctrl+Shift+S must run Save As and write to the picked path"
        );
    }

    #[test]
    fn plain_ctrl_s_does_not_open_the_save_as_dialog() {
        // The negative half: if Ctrl+S also reached Save-As it would consume the
        // injected answer and write the file. Nothing may appear at the picked
        // path — and the injected answer must still be sitting there unused,
        // which the follow-up Ctrl+Shift+S proves by consuming it.
        let dir = std::env::temp_dir().join(format!(
            "scr1b3-keyboard-input-tests/plain-save-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let picked = dir.join("untouched.md");

        let mut app = ScribeApp::new_test(Config::default());
        let active = app.active;
        app.tabs[active].set_text("body".into());
        crate::app::dialogs::test_hooks::set_next_save_path(picked.clone());

        frame(&mut app, CMD, press(egui::Key::S, CMD), 0);
        assert!(
            !picked.exists(),
            "plain Ctrl+S must not run Save As — it is a different action"
        );

        frame(
            &mut app,
            CMD | egui::Modifiers::SHIFT,
            press(egui::Key::S, CMD | egui::Modifiers::SHIFT),
            0,
        );
        assert!(
            picked.exists(),
            "precondition: the injected dialog answer was still unconsumed, so \
             the Ctrl+S above really did skip Save As"
        );
    }

    // ---- drag-hover drop target ----

    #[test]
    fn drop_target_hint_counts_and_pluralises() {
        // Assert the WHOLE produced string, not a substring: a hint that dropped
        // the count, or echoed a bare number, would still contain "file".
        assert_eq!(drop_target_hint(1), "Drop to open 1 file");
        assert_eq!(drop_target_hint(2), "Drop to open 2 files");
        assert_eq!(drop_target_hint(17), "Drop to open 17 files");
    }

    #[test]
    fn dragging_files_over_the_window_paints_the_drop_target() {
        let mut app = ScribeApp::new_test(Config::default());
        let painted = frame(&mut app, egui::Modifiers::NONE, Vec::new(), 2);
        assert!(
            painted.iter().any(|t| t == "Drop to open 2 files"),
            "a drag in flight must paint the drop-target hint; painted: {painted:?}"
        );
    }

    #[test]
    fn nothing_is_painted_when_no_drag_is_in_flight() {
        // The load-bearing negative: an overlay that painted unconditionally
        // would cover the editor during normal typing, and the positive test
        // above would still pass.
        let mut app = ScribeApp::new_test(Config::default());
        let painted = frame(&mut app, egui::Modifiers::NONE, Vec::new(), 0);
        assert!(
            !painted.iter().any(|t| t.starts_with("Drop to open")),
            "no drag => no overlay; painted: {painted:?}"
        );
    }
}
