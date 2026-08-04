//! Single-instance hand-off wiring: what the PRIMARY process does when a later
//! launch forwards its command line.
//!
//! The transport itself (lock, atomic write, drain) is covered in
//! `crate::single_instance`. These tests cover the app-side half — that a
//! forwarded request actually opens the files and raises the window — by driving
//! a real `egui::Context` and reading the viewport commands it emitted, rather
//! than asserting on an internal flag.

use super::*;
use crate::single_instance::Request;
use scribe_core::Config;

/// A `ScribeApp` wired to a temp hand-off root, plus the temp dir keeping it
/// alive.
fn app_with_handoff() -> (tempfile::TempDir, ScribeApp) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = ScribeApp::new_test(Config::default());
    app.handoff_root = Some(dir.path().join("instance"));
    (dir, app)
}

/// Run one egui frame that polls the queue, and return the viewport commands the
/// frame emitted.
fn poll_once(app: &mut ScribeApp) -> Vec<egui::ViewportCommand> {
    let ctx = egui::Context::default();
    let out = ctx.run_ui(Default::default(), |_| {
        app.poll_handoff_queue(&ctx);
    });
    out.viewport_output[&egui::ViewportId::ROOT]
        .commands
        .clone()
}

fn raises_the_window(cmds: &[egui::ViewportCommand]) -> bool {
    let un_minimize = cmds
        .iter()
        .position(|c| matches!(c, egui::ViewportCommand::Minimized(false)));
    let focus = cmds
        .iter()
        .position(|c| matches!(c, egui::ViewportCommand::Focus));
    match (un_minimize, focus) {
        // Order matters: winit's `focus_window` no-ops while minimized.
        (Some(m), Some(f)) => m < f,
        _ => false,
    }
}

#[test]
fn a_forwarded_file_is_opened_and_the_window_is_raised() {
    // The whole point of the feature: Explorer launched a second `scr1b3.exe`,
    // it handed us its path and exited, and the user must end up looking at
    // their file in THIS window.
    let (dir, mut app) = app_with_handoff();
    let file = dir.path().join("forwarded.txt");
    std::fs::write(&file, "hello from the second launch\n").unwrap();
    let root = app.handoff_root.clone().unwrap();
    crate::single_instance::forward(
        &root,
        &Request {
            paths: vec![file.display().to_string()],
            jump: None,
        },
    )
    .unwrap();

    let before = app.tabs.len();
    let cmds = poll_once(&mut app);

    assert_eq!(app.tabs.len(), before + 1, "the forwarded file must open");
    assert_eq!(
        app.tabs[app.active].doc.path(),
        Some(file.as_path()),
        "the forwarded file must be the ACTIVE tab, not a background one"
    );
    assert!(
        raises_the_window(&cmds),
        "the primary must un-minimize then focus itself, got {cmds:?}"
    );
}

#[test]
fn a_bare_relaunch_raises_the_window_without_opening_anything() {
    // Double-clicking the icon / taskbar tile while SCR1B3 runs forwards an
    // EMPTY request. Answering it with nothing visible reads as a dead app.
    let (_dir, mut app) = app_with_handoff();
    let root = app.handoff_root.clone().unwrap();
    crate::single_instance::forward(&root, &Request::default()).unwrap();

    let before = app.tabs.len();
    let cmds = poll_once(&mut app);

    assert_eq!(app.tabs.len(), before, "a bare relaunch must open no tabs");
    assert!(
        raises_the_window(&cmds),
        "a bare relaunch must still bring the window forward, got {cmds:?}"
    );
}

#[test]
fn an_idle_frame_with_an_empty_queue_raises_nothing() {
    // If an empty drain still emitted a raise, the window would steal the
    // foreground several times a second, forever.
    let (_dir, mut app) = app_with_handoff();
    let cmds = poll_once(&mut app);
    assert!(
        !raises_the_window(&cmds),
        "an empty queue must not touch the window, got {cmds:?}"
    );
}

#[test]
fn a_drained_request_is_not_replayed_on_the_next_frame() {
    // The consume-on-drain contract, observed from the app side: a re-delivered
    // request would re-raise the window on every poll and duplicate the tab.
    let (dir, mut app) = app_with_handoff();
    let file = dir.path().join("once.txt");
    std::fs::write(&file, "x\n").unwrap();
    let root = app.handoff_root.clone().unwrap();
    crate::single_instance::forward(
        &root,
        &Request {
            paths: vec![file.display().to_string()],
            jump: None,
        },
    )
    .unwrap();

    let after_first = {
        let cmds = poll_once(&mut app);
        assert!(raises_the_window(&cmds));
        app.tabs.len()
    };
    // Reset the throttle so the second poll genuinely runs.
    app.last_handoff_poll_frame = u64::MAX;
    let cmds = poll_once(&mut app);
    assert_eq!(
        app.tabs.len(),
        after_first,
        "the same file must not re-open"
    );
    assert!(
        !raises_the_window(&cmds),
        "a consumed request must not re-raise the window, got {cmds:?}"
    );
}

#[test]
fn a_process_without_the_instance_lock_never_drains_the_queue() {
    // A fall-through launch (unwritable config dir, failed hand-off) leaves
    // `handoff_root` unset. If it drained anyway, two windows would race for the
    // same forwarded file and one user's file would open in the wrong one.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("instance");
    let file = dir.path().join("contested.txt");
    std::fs::write(&file, "x\n").unwrap();
    crate::single_instance::forward(
        &root,
        &Request {
            paths: vec![file.display().to_string()],
            jump: None,
        },
    )
    .unwrap();

    let mut app = ScribeApp::new_test(Config::default());
    assert!(app.handoff_root.is_none(), "new_test must not own a queue");
    let before = app.tabs.len();
    let cmds = poll_once(&mut app);

    assert_eq!(app.tabs.len(), before);
    assert!(!raises_the_window(&cmds));
    assert!(
        crate::single_instance::pending(&root),
        "the request must be left for the process that actually holds the lock"
    );
}

#[test]
fn an_already_open_file_is_activated_rather_than_opened_twice() {
    // Re-opening a file from Explorer is a routine gesture. A duplicate tab is
    // both confusing and a data-loss hazard (two buffers, one file, last save
    // wins).
    let (dir, mut app) = app_with_handoff();
    let file = dir.path().join("already.txt");
    std::fs::write(&file, "content\n").unwrap();
    app.tabs.push(EditorTab::from_path(file.clone()).unwrap());
    let existing = app.tabs.len() - 1;
    app.active = 0;

    app.apply_handoff_request(&Request {
        paths: vec![file.display().to_string()],
        jump: None,
    });

    assert_eq!(
        app.tabs.len(),
        existing + 1,
        "an already-open file must not open a second tab"
    );
    assert_eq!(app.active, existing, "the existing tab must be activated");
}

#[test]
fn the_first_forwarded_path_becomes_active_matching_a_cold_launch() {
    // A cold launch makes argv[0] the active tab and the rest background tabs.
    // A hand-off is the same gesture and must not differ.
    let (dir, mut app) = app_with_handoff();
    let mut paths = Vec::new();
    for name in ["one.txt", "two.txt", "three.txt"] {
        let p = dir.path().join(name);
        std::fs::write(&p, "x\n").unwrap();
        paths.push(p.display().to_string());
    }
    let first = dir.path().join("one.txt");

    app.apply_handoff_request(&Request { paths, jump: None });

    assert_eq!(
        app.tabs[app.active].doc.path(),
        Some(first.as_path()),
        "the FIRST forwarded path must be the active tab"
    );
    assert!(
        app.tabs.iter().filter(|t| t.doc.path().is_some()).count() >= 3,
        "the remaining forwarded paths must still open as background tabs"
    );
}

#[test]
fn an_unopenable_forwarded_path_is_surfaced_not_silently_dropped() {
    // A path that vanished between the two launches. Silently doing nothing
    // looks identical to "the app ignored me".
    let (dir, mut app) = app_with_handoff();
    let missing = dir.path().join("does-not-exist.txt");
    let before = app.tabs.len();

    app.apply_handoff_request(&Request {
        paths: vec![missing.display().to_string()],
        jump: None,
    });

    assert_eq!(app.tabs.len(), before, "no tab for a path that cannot open");
    let toast = app.toast.clone().unwrap_or_default();
    assert!(
        toast.contains("does-not-exist"),
        "the failure must name the file, got {toast:?}"
    );
}

#[test]
fn a_forwarded_jump_target_scrolls_the_opened_file() {
    // `scr1b3 file:42:10` forwarded from a shell must land on line 42 in the
    // running window, exactly as it would on a cold launch.
    let (dir, mut app) = app_with_handoff();
    let file = dir.path().join("jump.txt");
    std::fs::write(&file, "l1\nl2\nl3\nl4\nl5\nl6\n").unwrap();
    let size = app.config.fonts.clamped_editor_size();
    let lh = app.config.fonts.clamped_line_height();
    let expected = 3.0_f32 * (size * lh); // 1-based line 4 -> 3 lines down
    app.pending_scroll = None;

    app.apply_handoff_request(&Request {
        paths: vec![file.display().to_string()],
        jump: Some((4, Some(2))),
    });

    assert_eq!(
        app.pending_scroll,
        Some(expected),
        "a forwarded jump must scroll to the requested line"
    );
    assert!(
        app.status.contains("4:2"),
        "the status must surface the jump target, got {:?}",
        app.status
    );
}

#[test]
fn a_forwarded_jump_targets_the_forwarded_tab_not_tab_zero() {
    // `apply_cli_jump` forces `active = 0`, which is right for a cold launch
    // (the CLI files ARE tabs 0..n) and wrong here: after a session restore the
    // forwarded file lands at the END, and jumping tab 0 would scroll an
    // unrelated document while the user stares at the one they asked for.
    let (dir, mut app) = app_with_handoff();
    let restored = dir.path().join("restored.txt");
    std::fs::write(&restored, "a\nb\nc\nd\ne\nf\n").unwrap();
    app.tabs[0] = EditorTab::from_path(restored).unwrap();
    let file = dir.path().join("jump.txt");
    std::fs::write(&file, "l1\nl2\nl3\nl4\nl5\nl6\n").unwrap();

    app.apply_handoff_request(&Request {
        paths: vec![file.display().to_string()],
        jump: Some((4, None)),
    });

    assert_ne!(app.active, 0, "the forwarded tab must be the active one");
    assert_eq!(
        app.tabs[app.active].doc.path(),
        Some(file.as_path()),
        "the jump must apply to the forwarded file"
    );
}
