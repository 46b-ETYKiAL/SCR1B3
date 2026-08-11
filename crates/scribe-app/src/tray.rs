//! Windows system-tray icon: single-click minimize ⇄ restore, right-click menu.
//!
//! ## Shape
//!
//! Modelled on the sibling C0PL4ND implementation (`egui_app/tray.rs`): the
//! **decision** logic — what a click means, what a menu id maps to — is pure,
//! host-independent and unit-tested; only the OS plumbing is platform-gated.
//!
//! ## Where it differs from C0PL4ND, and why
//!
//! C0PL4ND's binary is `#![deny(unsafe_code)]`, so it re-opens the lint inside a
//! `#[cfg(windows)] mod imp { #![allow(unsafe_code)] }` and calls `IsIconic` /
//! `ShowWindow` / `SetForegroundWindow` / `AttachThreadInput` directly. SCR1B3's
//! binary is `#![forbid(unsafe_code)]`, and a *forbid* cannot be re-opened by an
//! inner `allow` — that quarantine pattern is structurally unavailable here.
//!
//! So the window operations go through egui instead of Win32:
//!
//! | C0PL4ND (raw Win32) | here (safe egui) |
//! |---|---|
//! | `IsIconic` / `IsWindowVisible` | `ctx.input(\|i\| i.viewport().minimized / .focused)` |
//! | `ShowWindow(SW_MINIMIZE)` | `ViewportCommand::Minimized(true)` |
//! | `ShowWindow(SW_RESTORE)` | `ViewportCommand::Minimized(false)` |
//! | `SetForegroundWindow` + `AttachThreadInput` dance | `ViewportCommand::Focus` |
//! | `PostMessageW(WM_CLOSE)` | `ViewportCommand::Close` |
//!
//! This is not a weaker substitute. `ViewportCommand::Focus` lands on winit's
//! `focus_window`, which performs the documented synthetic-ALT
//! foreground-permission trick before `SetForegroundWindow` — the same reason
//! C0PL4ND needs `AttachThreadInput`. And `ViewportCommand::Close` funnels into
//! the app's existing two-phase close (unsaved-changes guard + session flush),
//! which is exactly what C0PL4ND's `WM_CLOSE` is chosen for.
//!
//! `tray-icon` itself exposes a fully safe API; the crate's own FFI is audited
//! upstream. Only the *window control* needed a substitute.
//!
//! ## Events fire while the window is minimized
//!
//! `tray-icon` delivers events to a process-global handler as the winit message
//! loop pumps them, not via per-frame polling — so a click still arrives when
//! egui has stopped repainting. The handler queues a viewport command and asks
//! for a repaint; eframe explicitly paints invisible/minimized windows directly
//! (its `is_invisible_or_minimized` path, on a 100 ms cadence) so the queued
//! command is processed rather than stranded. Without that eframe behaviour a
//! queued `Minimized(false)` could never run — a minimized window receives no
//! `RedrawRequested`.

// Off Windows the `#[cfg(windows)] mod imp` half is not compiled, so nothing in
// the non-test build calls this module's pure logic. That is a property of the
// platform, not a dormancy bug: on Windows the lint is fully active, so a
// genuinely unwired item is still caught where it must be wired.
#![cfg_attr(not(windows), allow(dead_code))]

// ---------------------------------------------------------------------------
// PURE decision logic (compiled + tested on every host)
// ---------------------------------------------------------------------------

/// Stable menu-item ids. Fixed id STRINGS (rather than keeping `MenuItem`
/// handles alive to compare against) let [`classify_menu`] stay a pure function
/// with no tray types in the tested logic.
const MENU_SHOW_ID: &str = "scr1b3.tray.show";
const MENU_HIDE_ID: &str = "scr1b3.tray.hide";
const MENU_QUIT_ID: &str = "scr1b3.tray.quit";

/// What a single tray LEFT-click should do.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ToggleAction {
    /// Bring the window back and give it the foreground.
    Restore,
    /// Send the window to the taskbar.
    Minimize,
}

/// The toggle decision from the window's live state.
///
/// Restores when the window is minimized **or** merely not focused; minimizes
/// only when it is the window the user is already looking at.
///
/// The not-focused case is the one that separates a useful tray icon from an
/// annoying one. C0PL4ND's equivalent asks `IsIconic || !IsWindowVisible`, which
/// is the right question for an app that can HIDE to the tray. SCR1B3 never
/// hides — it only minimizes — so `!visible` is dead there and the honest
/// analogue is "not in front of me". Clicking the tray icon from another
/// application therefore raises SCR1B3 (what the user wanted) instead of
/// minimizing an already-minimized window (a no-op that reads as a broken icon).
#[must_use]
pub fn toggle_action(minimized: bool, focused: bool) -> ToggleAction {
    if minimized || !focused {
        ToggleAction::Restore
    } else {
        ToggleAction::Minimize
    }
}

/// A context-menu action.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MenuAction {
    /// Show / restore the window to the foreground.
    ShowRestore,
    /// Minimize the window to the taskbar.
    Hide,
    /// Quit, through the app's normal close path (so unsaved work is guarded).
    Quit,
}

/// Map an incoming menu-event id to its [`MenuAction`], or `None` for an id we
/// did not create.
///
/// The `muda` menu-event channel is process-wide, so a foreign id can arrive;
/// mapping one of those onto an action would let another component's menu quit
/// the editor.
#[must_use]
pub fn classify_menu(clicked: &str, show: &str, hide: &str, quit: &str) -> Option<MenuAction> {
    if clicked == show {
        Some(MenuAction::ShowRestore)
    } else if clicked == hide {
        Some(MenuAction::Hide)
    } else if clicked == quit {
        Some(MenuAction::Quit)
    } else {
        None
    }
}

/// The viewport commands that realise a [`ToggleAction`], in the order they must
/// be issued.
///
/// Order is load-bearing and is the reason this is a function rather than an
/// inline match: winit's `focus_window` **returns early while the window is
/// minimized**, so a `Focus` issued before `Minimized(false)` is silently
/// dropped and the window comes back behind whatever the user was looking at.
#[must_use]
pub fn restore_commands() -> [egui::ViewportCommand; 2] {
    [
        egui::ViewportCommand::Minimized(false),
        egui::ViewportCommand::Focus,
    ]
}

/// The viewport command that realises [`ToggleAction::Minimize`].
#[must_use]
pub fn minimize_command() -> egui::ViewportCommand {
    egui::ViewportCommand::Minimized(true)
}

/// Apply a [`ToggleAction`] to `ctx`, then ask for the repaint that lets eframe
/// process the queued commands (a minimized window is otherwise not repainted).
pub fn apply_toggle(ctx: &egui::Context, action: ToggleAction) {
    match action {
        ToggleAction::Restore => {
            for cmd in restore_commands() {
                ctx.send_viewport_cmd(cmd);
            }
        }
        ToggleAction::Minimize => ctx.send_viewport_cmd(minimize_command()),
    }
    ctx.request_repaint();
}

/// Read the live window state and decide what a click means.
fn toggle_from_ctx(ctx: &egui::Context) -> ToggleAction {
    let (minimized, focused) = ctx.input(|i| {
        let vp = i.viewport();
        // `None` means the backend could not report the state. Treating an
        // unknown as "minimized" would make the icon useless; treating it as
        // "focused" keeps the visible-window case (minimize) working, which is
        // the state a click most often arrives in.
        (vp.minimized.unwrap_or(false), vp.focused.unwrap_or(true))
    });
    toggle_action(minimized, focused)
}

// ---------------------------------------------------------------------------
// Public entry point — driven from `ScribeApp::new` (the wiring seam)
// ---------------------------------------------------------------------------

/// Create the tray icon + context menu and register its handlers.
///
/// Best-effort: a build failure (headless shell, no shell notification area)
/// logs at WARN and leaves no tray — it never panics and never blocks startup,
/// mirroring how the window icon is loaded. A no-op off Windows.
///
/// Call from the eframe creation closure, on the event-loop thread.
pub fn init(ctx: &egui::Context) {
    #[cfg(windows)]
    imp::init(ctx);
    #[cfg(not(windows))]
    let _ = ctx;
}

// ---------------------------------------------------------------------------
// Windows plumbing — `tray-icon`'s safe API only; no `unsafe` anywhere.
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod imp {
    use std::cell::RefCell;

    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{
        Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    };

    use super::{
        apply_toggle, classify_menu, toggle_from_ctx, MenuAction, ToggleAction, MENU_HIDE_ID,
        MENU_QUIT_ID, MENU_SHOW_ID,
    };

    thread_local! {
        /// Keeps the `TrayIcon` alive for the process lifetime — dropping it
        /// removes the icon from the shell. `TrayIcon` is `!Send`, and `init`
        /// runs on the event-loop thread, so that thread is the only accessor.
        static TRAY: RefCell<Option<TrayIcon>> = const { RefCell::new(None) };
    }

    /// Decode the shipped app icon into the raw RGBA `tray-icon` wants. Reuses
    /// the SAME PNG and the SAME decoder as the window/taskbar icon, so the tray
    /// can never drift to a different mark and no second image decoder enters
    /// the dependency graph.
    fn icon() -> Option<Icon> {
        let data = eframe::icon_data::from_png_bytes(include_bytes!("../assets/scr1b3-256.png"))
            .map_err(|err| {
                tracing::warn!("tray icon decode failed: {err}; no tray");
            })
            .ok()?;
        Icon::from_rgba(data.rgba, data.width, data.height)
            .map_err(|err| {
                tracing::warn!("tray icon build failed: {err}; no tray");
            })
            .ok()
    }

    pub fn init(ctx: &egui::Context) {
        let Some(icon) = icon() else { return };

        let menu = Menu::new();
        let show = MenuItem::with_id(MENU_SHOW_ID, "Show SCR1B3", true, None);
        let hide = MenuItem::with_id(MENU_HIDE_ID, "Hide to taskbar", true, None);
        let sep = PredefinedMenuItem::separator();
        let quit = MenuItem::with_id(MENU_QUIT_ID, "Quit SCR1B3", true, None);
        for item in [
            &show as &dyn tray_icon::menu::IsMenuItem,
            &hide,
            &sep,
            &quit,
        ] {
            if let Err(err) = menu.append(item) {
                tracing::warn!("tray menu append failed: {err}");
            }
        }

        let tray = TrayIconBuilder::new()
            .with_tooltip(scribe_core::PRODUCT_NAME)
            .with_icon(icon)
            .with_menu(Box::new(menu))
            // Left click is the minimize/restore TOGGLE; the menu opens on RIGHT
            // click only (the crate's default is left — turn that off).
            .with_menu_on_left_click(false)
            .build();
        let tray = match tray {
            Ok(tray) => tray,
            Err(err) => {
                tracing::warn!("tray icon build failed: {err}; no tray");
                return;
            }
        };
        TRAY.with(|slot| *slot.borrow_mut() = Some(tray));

        let click_ctx = ctx.clone();
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                apply_toggle(&click_ctx, toggle_from_ctx(&click_ctx));
            }
        }));

        let menu_ctx = ctx.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            match classify_menu(
                event.id().as_ref(),
                MENU_SHOW_ID,
                MENU_HIDE_ID,
                MENU_QUIT_ID,
            ) {
                Some(MenuAction::ShowRestore) => apply_toggle(&menu_ctx, ToggleAction::Restore),
                Some(MenuAction::Hide) => apply_toggle(&menu_ctx, ToggleAction::Minimize),
                Some(MenuAction::Quit) => {
                    // Through the normal close path, so the unsaved-changes
                    // guard and the session flush both run.
                    menu_ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    menu_ctx.request_repaint();
                }
                None => {}
            }
        }));
    }
}

// ---------------------------------------------------------------------------
// Pure-logic tests (run on every host)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_restores_when_out_of_view_else_minimizes() {
        // The full truth table — the whole behaviour a click has to get right.
        // minimized, focused
        assert_eq!(toggle_action(true, false), ToggleAction::Restore);
        assert_eq!(toggle_action(true, true), ToggleAction::Restore);
        assert_eq!(toggle_action(false, false), ToggleAction::Restore);
        assert_eq!(toggle_action(false, true), ToggleAction::Minimize);
    }

    #[test]
    fn only_the_window_in_front_of_you_gets_minimized() {
        // Restated as the user-visible rule so a future edit that "simplifies"
        // the predicate to `minimized` alone fails here with the reason.
        let visible_and_in_front = toggle_action(false, true);
        assert_eq!(
            visible_and_in_front,
            ToggleAction::Minimize,
            "clicking the tray while looking at SCR1B3 must put it away"
        );
        let visible_but_behind = toggle_action(false, false);
        assert_eq!(
            visible_but_behind,
            ToggleAction::Restore,
            "clicking the tray from another app must RAISE SCR1B3, not minimize \
             a window the user cannot see"
        );
    }

    #[test]
    fn classify_menu_maps_each_id_and_ignores_unknown() {
        assert_eq!(
            classify_menu(MENU_SHOW_ID, MENU_SHOW_ID, MENU_HIDE_ID, MENU_QUIT_ID),
            Some(MenuAction::ShowRestore),
        );
        assert_eq!(
            classify_menu(MENU_HIDE_ID, MENU_SHOW_ID, MENU_HIDE_ID, MENU_QUIT_ID),
            Some(MenuAction::Hide),
        );
        assert_eq!(
            classify_menu(MENU_QUIT_ID, MENU_SHOW_ID, MENU_HIDE_ID, MENU_QUIT_ID),
            Some(MenuAction::Quit),
        );
        // The muda menu channel is process-wide: a foreign id must map to
        // nothing, or another component's menu could quit the editor.
        assert_eq!(
            classify_menu(
                "some.other.app.item",
                MENU_SHOW_ID,
                MENU_HIDE_ID,
                MENU_QUIT_ID
            ),
            None,
        );
        assert_eq!(
            classify_menu("", MENU_SHOW_ID, MENU_HIDE_ID, MENU_QUIT_ID),
            None,
        );
    }

    #[test]
    fn menu_ids_are_distinct_and_namespaced() {
        // A copy-paste collision would silently route (e.g.) Quit to Show.
        assert_ne!(MENU_SHOW_ID, MENU_HIDE_ID);
        assert_ne!(MENU_HIDE_ID, MENU_QUIT_ID);
        assert_ne!(MENU_SHOW_ID, MENU_QUIT_ID);
        // Namespacing is what makes the foreign-id rejection above meaningful
        // on a process-wide channel.
        for id in [MENU_SHOW_ID, MENU_HIDE_ID, MENU_QUIT_ID] {
            assert!(id.starts_with("scr1b3.tray."), "{id} must be namespaced");
        }
    }

    #[test]
    fn restore_unminimizes_before_it_focuses() {
        // winit's `focus_window` returns early while the window is minimized, so
        // a Focus issued first is DROPPED and the window returns behind whatever
        // the user was looking at. Order is the fix; pin it.
        let cmds = restore_commands();
        assert!(
            matches!(cmds[0], egui::ViewportCommand::Minimized(false)),
            "restore must un-minimize FIRST, got {:?}",
            cmds[0]
        );
        assert!(
            matches!(cmds[1], egui::ViewportCommand::Focus),
            "restore must then take the foreground, got {:?}",
            cmds[1]
        );
    }

    #[test]
    fn minimize_command_actually_minimizes() {
        assert!(matches!(
            minimize_command(),
            egui::ViewportCommand::Minimized(true)
        ));
    }

    #[test]
    fn apply_toggle_queues_the_commands_on_the_context() {
        // Drives the real egui Context: a queued viewport command is observable
        // in the frame output, so this asserts the wiring, not a flag.
        let ctx = egui::Context::default();
        let out = ctx.run_ui(Default::default(), |_| {
            apply_toggle(&ctx, ToggleAction::Restore);
        });
        let cmds = &out.viewport_output[&egui::ViewportId::ROOT].commands;
        assert!(
            cmds.iter()
                .any(|c| matches!(c, egui::ViewportCommand::Minimized(false))),
            "restore must emit Minimized(false), got {cmds:?}"
        );
        assert!(
            cmds.iter()
                .any(|c| matches!(c, egui::ViewportCommand::Focus)),
            "restore must emit Focus, got {cmds:?}"
        );

        let ctx = egui::Context::default();
        let out = ctx.run_ui(Default::default(), |_| {
            apply_toggle(&ctx, ToggleAction::Minimize);
        });
        let cmds = &out.viewport_output[&egui::ViewportId::ROOT].commands;
        assert!(
            cmds.iter()
                .any(|c| matches!(c, egui::ViewportCommand::Minimized(true))),
            "minimize must emit Minimized(true), got {cmds:?}"
        );
        assert!(
            !cmds
                .iter()
                .any(|c| matches!(c, egui::ViewportCommand::Focus)),
            "minimize must NOT also grab the foreground, got {cmds:?}"
        );
    }

    #[test]
    fn toggle_from_ctx_reads_the_live_viewport_state() {
        // The read that decides the click, exercised against a real Context so a
        // future field rename (minimized/focused) fails here instead of silently
        // defaulting the window to "visible and in front".
        let ctx = egui::Context::default();
        let mut input = egui::RawInput::default();
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .expect("root viewport")
            .minimized = Some(true);
        let _ = ctx.run_ui(input, |_| {});
        assert_eq!(
            toggle_from_ctx(&ctx),
            ToggleAction::Restore,
            "a minimized window must decide RESTORE"
        );

        let ctx = egui::Context::default();
        let mut input = egui::RawInput::default();
        {
            let vp = input
                .viewports
                .get_mut(&egui::ViewportId::ROOT)
                .expect("root viewport");
            vp.minimized = Some(false);
            vp.focused = Some(true);
        }
        let _ = ctx.run_ui(input, |_| {});
        assert_eq!(
            toggle_from_ctx(&ctx),
            ToggleAction::Minimize,
            "a focused, non-minimized window must decide MINIMIZE"
        );
    }

    /// The `tray::imp` pardons in `.cargo/mutants.toml` rest entirely on `mod imp`
    /// being `#[cfg(windows)]`. The mutation runner is ubuntu-latest, so nothing
    /// inside that module is compiled there and no Linux test can ever kill its
    /// mutants — which is what makes the pardon honest. If the gate is ever
    /// removed the module starts compiling on the runner, its mutants become real
    /// signal, and the pardon would begin hiding compiled, untested code. Fail
    /// here rather than there.
    #[test]
    fn tray_imp_is_cfg_gated_so_the_mutation_pardon_stays_honest() {
        let src = include_str!("tray.rs");
        // ASSEMBLED, never written as a literal. This test reads its OWN file, so
        // a literal needle would sit in the source and `matches` would count the
        // needle itself — passing no matter what the real declaration said. Same
        // discipline as `windows_module_is_cfg_gated_so_the_mutation_exclusion_stays_honest`
        // in integration/mod.rs.
        let needle = ["#[cfg(", "windows", ")]\n", "mod imp {"].concat();
        assert_eq!(
            src.matches(needle.as_str()).count(),
            1,
            "`mod imp` is no longer exactly `#[cfg(windows)]`-gated (or this test \
             now self-matches). If that module compiles off Windows its mutants \
             are real signal: DROP the `tray\\.rs.*\\bimp::icon\\b` / \
             `tray\\.rs.*\\bimp::init\\b` entries and the `replace tray::init with \
             \\(\\)` entry from .cargo/mutants.toml and delete this test, rather \
             than leaving a pardon that silently hides code it claims cannot compile."
        );
    }

    /// The `init` pardon must be spelled the way cargo-mutants NAMES the mutant.
    ///
    /// This entry read `replace tray::init with \(\)` and matched nothing:
    /// cargo-mutants names a FREE function bare, so the mutant it prints is
    /// `tray.rs:185:5: replace init with ()`. The pardon was inert and `185:5`
    /// came back as a survivor on every shard — the same "anchor matching zero
    /// mutants" failure the mutants.toml header documents, reached by a
    /// module-qualified name instead of by a rotated line. A stale POSITIONAL
    /// anchor is caught by `every_positional_anchor_still_points_at_the_code_it
    /// _pardons`; a stale DESCRIPTION anchor was not caught by anything, so this
    /// pins the one that bit us.
    #[test]
    fn the_tray_init_pardon_uses_the_bare_function_name_cargo_mutants_prints() {
        let cfg = include_str!("../../../.cargo/mutants.toml");
        // Only the real ENTRIES, never the prose: the comment above the entry
        // quotes the broken spelling to explain it, and a whole-file `contains`
        // would read that explanation as the defect it warns about.
        let entries: String = cfg
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n");
        // ASSEMBLED so the needles are not themselves literals a future
        // whole-file scan could trip over.
        let qualified = ["replace ", "tray::init", " with"].concat();
        let bare = ["replace ", "init", " with \\(\\)"].concat();
        assert!(
            !entries.contains(qualified.as_str()),
            "the module-qualified spelling is back in .cargo/mutants.toml. \
             cargo-mutants prints free functions bare, so that form matches NO \
             mutant and the pardon is silently inert."
        );
        assert!(
            entries.contains(bare.as_str()),
            "the bare-name `init` pardon is missing from .cargo/mutants.toml; \
             tray.rs:185 would be reported as a survivor again."
        );
    }
}
