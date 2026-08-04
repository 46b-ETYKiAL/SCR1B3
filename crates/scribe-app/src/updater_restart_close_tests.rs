//! The update-restart close contract.
//!
//! An update restart is the one place in SCR1B3 where the app closes itself
//! without the user having asked to close: the one-click flow chains
//! `Downloaded(Ok)` / `InstallerReady(Ok)` straight into
//! [`Updater::apply_and_restart`] / [`Updater::run_installer`] from `poll`,
//! which `frame_tick` drains every frame. The user may be mid-sentence. So two
//! things have to hold, and neither was asserted before this file existed:
//!
//! 1. **Only a genuinely applied update may ask the window to go away.** Every
//!    refusal/failure path (wrong state, the TUF anti-rollback downgrade
//!    refusal, a failed in-place swap) must emit NO viewport command at all.
//!    The existing tests for those paths assert only `UpdateState`, so hoisting
//!    the close above the anti-rollback gate — or dropping it into an `Err` arm
//!    — would close the editor on an update that never happened, with the whole
//!    suite still green.
//!
//! 2. **The close it does ask for is a REQUEST the app adjudicates**, not a
//!    bare destroy. `ViewportCommand::Close` is round-tripped by the host into
//!    the next frame's `close_requested()` (egui-winit
//!    `process_viewport_commands` pushes a `ViewportEvent::Close` onto the
//!    viewport's `ViewportInfo`; eframe rebuilds `raw_input.viewports` from that
//!    info), which is the same signal an OS ✕ / Alt+F4 delivers — so the
//!    unsaved-changes guard in `frame_tick` rules on it exactly as it rules on
//!    those. [`an_update_restart_close_is_adjudicated_by_the_apps_close_guard`]
//!    drives that whole path through the REAL `ScribeApp::frame_tick` and
//!    asserts the app answers with `CancelClose` + `Visible(false)` — it takes
//!    ownership of the close and hides before it destroys — rather than the
//!    window simply disappearing.
//!
//! Every assertion here is on emitted `ViewportCommand`s, never on an internal
//! flag: it is `Close` / `Visible(false)`, not a bool, that takes an unsaved
//! buffer away. (Same reasoning as `app::close_guard_tests::frame_cmds` and
//! `deferred_actions_tests::viewport_cmds`.)

// `Context::run` is deprecated in egui 0.34 in favour of `run_ui`, but it is
// how this crate drives `frame_tick` headlessly everywhere (see the
// module-level allow in `app/mod.rs` and `app::close_guard_tests::frame_cmds`):
// `frame_tick` takes a `&Context`, not a `&mut Ui`. Same idiom, same allow.
#![allow(deprecated)]

use super::{UpdateMsg, UpdateState, Updater};
use crate::app::ScribeApp;
use scribe_core::Config;

/// Every `ViewportCommand` `f` emits during one egui pass.
fn viewport_cmds(f: impl FnMut(&egui::Context)) -> Vec<egui::ViewportCommand> {
    let ctx = egui::Context::default();
    let out = ctx.run(egui::RawInput::default(), f);
    out.viewport_output
        .get(&egui::ViewportId::ROOT)
        .map(|v| v.commands.clone())
        .unwrap_or_default()
}

/// What the HOST hands back to the app on the NEXT frame for each command a
/// frame emitted.
///
/// This mirrors `egui_winit::process_viewport_commands` (egui-winit 0.34.3):
/// `ViewportCommand::Close` — and only that command — becomes a
/// `ViewportEvent::Close` on the viewport's `ViewportInfo`, which eframe copies
/// into the next frame's `raw_input.viewports`, where `close_requested()` reads
/// it. Modelling it here is what makes the close-guard test below assert the
/// REAL chain (updater ⇒ host ⇒ guard) instead of a hand-fed close event: if
/// the updater ever emitted something the host does NOT turn into a close
/// request, this returns nothing and the app is never asked.
fn host_round_trip(cmds: &[egui::ViewportCommand]) -> Vec<egui::ViewportEvent> {
    cmds.iter()
        .filter(|c| matches!(c, egui::ViewportCommand::Close))
        .map(|_| egui::ViewportEvent::Close)
        .collect()
}

/// One frame through the REAL `frame_tick`, with `host_events` delivered as the
/// viewport's incoming events, returning the `ViewportCommand`s it emitted.
fn app_frame(
    app: &mut ScribeApp,
    ctx: &egui::Context,
    host_events: Vec<egui::ViewportEvent>,
) -> Vec<egui::ViewportCommand> {
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(1100.0, 720.0),
        )),
        viewport_id: egui::ViewportId::ROOT,
        viewports: std::iter::once((
            egui::ViewportId::ROOT,
            egui::ViewportInfo {
                events: host_events,
                ..Default::default()
            },
        ))
        .collect(),
        ..Default::default()
    };
    let out = ctx.run(input, |ctx| app.frame_tick(ctx));
    out.viewport_output
        .get(&egui::ViewportId::ROOT)
        .map(|v| v.commands.clone())
        .unwrap_or_default()
}

fn test_app() -> ScribeApp {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    cfg.appearance.frameless = false;
    ScribeApp::new_test(cfg)
}

/// Was the window told to go away (hide or destroy) by these commands?
fn destroys_or_hides(cmds: &[egui::ViewportCommand]) -> bool {
    cmds.iter().any(|c| {
        matches!(
            c,
            egui::ViewportCommand::Close | egui::ViewportCommand::Visible(false)
        )
    })
}

// ───────────────── the request, and what the app does with it ─────────────────

/// The updater's close must be a request the HOST hands back to the app, not a
/// command that only the window understands.
///
/// If `request_restart_close` ever emitted anything else — `Visible(false)`,
/// `Minimized(true)`, nothing at all — `host_round_trip` yields no close event
/// and the app is never consulted; that is the shape of a guard bypass, so it
/// is asserted directly rather than inferred.
#[test]
fn the_restart_close_is_a_request_the_host_hands_back_to_the_app() {
    let cmds = viewport_cmds(Updater::request_restart_close);
    assert_eq!(
        cmds,
        vec![egui::ViewportCommand::Close],
        "the restart close must be exactly one Close request, got: {cmds:?}"
    );
    assert_eq!(
        host_round_trip(&cmds),
        vec![egui::ViewportEvent::Close],
        "the host must turn the updater's request into the same close event an \
         OS ✕ delivers — otherwise frame_tick's guard never sees it"
    );
}

/// THE contract: an update restart goes through the app's close path, and that
/// path still hides before it destroys.
///
/// Driven end to end — the updater emits, the host round-trips, the REAL
/// `frame_tick` answers — and asserted on the emitted commands:
///   * `CancelClose` proves the app took ownership of the close (the guarded
///     branch ran) rather than the host destroying the window over its head;
///   * `Visible(false)` WITHOUT `Close` in the same frame proves phase 1 of the
///     two-phase close (the T19.1 DWM-ghost fix) is still in front of the
///     destroy — an update restart is not a shortcut past it;
///   * the destroy only lands on the FOLLOWING frame.
///
/// This app has nothing unsaved, so the guard lets the close proceed; the
/// dirty-buffer verdict on the very same `close_requested()` input is pinned by
/// `app::close_guard_tests::an_os_close_request_with_unsaved_changes_is_guarded_too`
/// (the guard cannot tell the two apart — it reads one flag).
#[test]
fn an_update_restart_close_is_adjudicated_by_the_apps_close_guard() {
    let mut app = test_app();
    let ctx = egui::Context::default();

    let settle = app_frame(&mut app, &ctx, Vec::new());
    assert!(
        !destroys_or_hides(&settle),
        "precondition: an idle frame must not be closing anything, got: {settle:?}"
    );

    // The updater asks to restart, and the host hands that request to the app.
    let asked = host_round_trip(&viewport_cmds(Updater::request_restart_close));
    assert!(
        !asked.is_empty(),
        "precondition: the host must deliver the updater's close request"
    );

    let answered = app_frame(&mut app, &ctx, asked);
    assert!(
        answered.contains(&egui::ViewportCommand::CancelClose),
        "the app must take ownership of the update restart (CancelClose) instead \
         of letting the window be destroyed over its head, got: {answered:?}"
    );
    assert!(
        answered.contains(&egui::ViewportCommand::Visible(false)),
        "an update restart must still go through phase 1 of the two-phase close \
         (hide before destroy — the T19.1 ghost fix), got: {answered:?}"
    );
    assert!(
        !answered.contains(&egui::ViewportCommand::Close),
        "the update restart must NOT destroy the window in the frame it is \
         requested — that is the ghost bug, got: {answered:?}"
    );

    let destroyed = app_frame(&mut app, &ctx, Vec::new());
    assert!(
        destroyed.contains(&egui::ViewportCommand::Close),
        "phase 2 destroys the now-hidden window, got: {destroyed:?}"
    );
}

// ─────────────── only a real apply may ask the window to go away ───────────────

/// A downgrade refused at apply time (TUF anti-rollback) must not close the
/// editor. Hoisting the close above that gate would end the session — and any
/// unsaved work with it, once the guard is answered — for an update that was
/// never installed.
#[test]
fn a_refused_installer_never_asks_the_window_to_go_away() {
    let mut u = Updater {
        state: UpdateState::ReadyToRunInstaller {
            installer: std::path::PathBuf::from("/nonexistent/scr1b3-setup.exe"),
            version: "0.0.1".to_string(), // older than the running build
        },
        ..Default::default()
    };
    let cmds = viewport_cmds(|ctx| u.run_installer(ctx));
    assert!(
        cmds.is_empty(),
        "a refused installer must emit no viewport command at all, got: {cmds:?}"
    );
    assert!(
        matches!(u.state, UpdateState::Failed(_)),
        "precondition: the downgrade must actually have been refused"
    );
}

/// Same gate on the in-place-swap route.
#[test]
fn a_refused_in_place_apply_never_asks_the_window_to_go_away() {
    let mut u = Updater {
        state: UpdateState::ReadyToApply {
            staged: std::path::PathBuf::from("/nonexistent/scr1b3"),
            version: "0.0.1".to_string(),
        },
        ..Default::default()
    };
    let cmds = viewport_cmds(|ctx| u.apply_and_restart(ctx));
    assert!(
        cmds.is_empty(),
        "a refused in-place apply must emit no viewport command at all, got: {cmds:?}"
    );
    assert!(
        matches!(u.state, UpdateState::Failed(_)),
        "precondition: the downgrade must actually have been refused"
    );
}

/// The `let-else` guard arms: a stray call from a state with nothing staged
/// must be inert at the window too, not just in `UpdateState`.
#[test]
fn an_apply_call_from_the_wrong_state_never_asks_the_window_to_go_away() {
    let mut installer = Updater {
        state: UpdateState::Idle,
        ..Default::default()
    };
    let cmds = viewport_cmds(|ctx| installer.run_installer(ctx));
    assert!(
        cmds.is_empty(),
        "run_installer from Idle must emit no viewport command, got: {cmds:?}"
    );

    let mut swap = Updater {
        state: UpdateState::Checking,
        ..Default::default()
    };
    let cmds = viewport_cmds(|ctx| swap.apply_and_restart(ctx));
    assert!(
        cmds.is_empty(),
        "apply_and_restart from Checking must emit no viewport command, got: {cmds:?}"
    );
}

/// The one-click flow's failure mode: a verified download chains straight into
/// the in-place swap, the swap fails (the staged binary is not there), and the
/// app must stay exactly where it is. A close here would be the worst case —
/// the editor ends the session for an update that is not installed.
#[test]
fn a_failed_in_place_swap_never_asks_the_window_to_go_away() {
    let mut u = Updater::default();
    let cmds = viewport_cmds(|ctx| {
        u.handle_update_msg(
            UpdateMsg::Downloaded(Ok((
                std::path::PathBuf::from("/nonexistent/scr1b3-staged-binary"),
                "9.9.9".to_string(), // newer, so the anti-rollback gate passes
            ))),
            ctx,
        );
    });
    assert!(
        cmds.is_empty(),
        "a failed swap must emit no viewport command at all, got: {cmds:?}"
    );
    match &u.state {
        UpdateState::Failed(e) => assert!(
            e.contains("install failed"),
            "precondition: the swap must actually have failed, got: {e}"
        ),
        other => panic!("precondition: expected Failed(install failed), got {other:?}"),
    }
}

/// The chokepoint is only a chokepoint while it is the ONLY way out.
///
/// A structural pin, in the idiom of the crate's other wiring guards: the
/// updater module must contain exactly one `send_viewport_cmd` call site (the
/// one inside `request_restart_close`) and no direct process exit. Re-adding a
/// bare `ctx.send_viewport_cmd(ViewportCommand::Close)` at either apply site —
/// the shape this file exists to prevent — fails here, because such a site
/// would sit below the refusal gates the tests above rely on.
#[test]
fn the_updater_has_exactly_one_place_that_can_take_the_window_away() {
    let src = include_str!("updater.rs");
    // Comments (including the chokepoint's own doc) name these symbols in prose;
    // count code only.
    let code: String = src
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        code.matches("send_viewport_cmd").count(),
        1,
        "the updater must have exactly ONE place that can take the window away \
         (Updater::request_restart_close) — every refusal path depends on sitting \
         above it"
    );
    assert!(
        !code.contains("process::exit"),
        "an update must never bypass the app's close path with a direct exit"
    );
}
