//! "Open a folder" had THREE writers and only one of them fed the MRU.
//!
//! `open_folder_root` (file_ops.rs) is the seam: it records the folder in
//! `config.editor.recent_folders` and persists it, which is the entire supply
//! for the "Open recent folder" modal. Commit 001859f introduced it and
//! converted only the palette path. The toolbar button
//! (`act.open_folder`) and the first-run welcome screen kept assigning
//! `self.file_tree_root = Some(folder)` raw, so a user who opens folders from
//! either of those surfaces — including, for a new user, the very first folder
//! they ever open — saw "Open recent folder" stay permanently empty and read
//! the feature as dead.
//!
//! These tests assert the MRU from EACH surface, driving the real dialog seam
//! (`dialogs::test_hooks`, which injects the answer the OS would have given so
//! the code around the picker still runs) rather than calling the helper
//! directly. Calling `open_folder_root` in a test proves only that the helper
//! works — which was never in doubt; the defect was the call that was missing.
#![allow(clippy::wildcard_imports)]

use super::deferred_actions::DeferredFlags;
use super::*;
use egui_kittest::kittest::Queryable as _;

fn app() -> ScribeApp {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    let app = ScribeApp::new_test(cfg);
    assert!(
        app.config.editor.recent_folders.is_empty(),
        "precondition — a fresh app has no recent folders"
    );
    app
}

fn temp_folder(tag: &str) -> tempfile::TempDir {
    let d = tempfile::Builder::new()
        .prefix(&format!("scr1b3-mru-{tag}-"))
        .tempdir()
        .expect("temp folder");
    d
}

/// The recent-folders MRU must name `folder`, and the tree root must point at
/// it — the MRU is worthless if the folder did not actually open.
fn assert_opened_and_recorded(app: &ScribeApp, folder: &std::path::Path, surface: &str) {
    assert_eq!(
        app.file_tree_root.as_deref(),
        Some(folder),
        "{surface}: the folder must actually open"
    );
    assert!(
        app.config
            .editor
            .recent_folders
            .iter()
            .any(|p| p.as_path() == folder),
        "{surface}: opened the folder WITHOUT recording it in the recent-folders \
         MRU — 'Open recent folder' stays empty and the feature reads as dead. \
         MRU was {:?}",
        app.config.editor.recent_folders
    );
}

/// Toolbar "open folder" button → `Pending::open_folder` → the deferred handler.
#[test]
fn the_toolbar_open_folder_records_the_recent_folders_mru() {
    let dir = temp_folder("toolbar");
    let mut app = app();
    let ctx = egui::Context::default();
    super::dialogs::test_hooks::set_next_pick_folder(dir.path().to_path_buf());

    app.apply_deferred_actions(
        &ctx,
        &mut Pending {
            open_folder: true,
            ..Default::default()
        },
        DeferredFlags {
            run_cmd: None,
            run_builtin: None,
            save_cfg: false,
            open_from_tree: None,
            close_tree: false,
            start_lsp: false,
            want_open_cfg: false,
            want_restore_cfg: false,
            want_dismiss_cfg: false,
        },
    );

    assert_opened_and_recorded(&app, dir.path(), "toolbar");
}

/// First-run welcome screen → the "Open folder…" button, clicked for real
/// through the AccessKit harness so the wire is proven, not simulated.
#[test]
fn the_welcome_screen_open_folder_records_the_recent_folders_mru() {
    let dir = temp_folder("welcome");
    let mut app = app();
    app.welcome_open = true;

    let mut h = egui_kittest::Harness::builder()
        .with_size(egui::Vec2::new(1100.0, 760.0))
        .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app);
    h.run();

    super::dialogs::test_hooks::set_next_pick_folder(dir.path().to_path_buf());
    let label = format!("{}  Open folder…", egui_phosphor::thin::FOLDER_OPEN);
    h.get_by_label(&label).click();
    h.run();

    assert!(
        !h.state().welcome_open,
        "precondition — the click must have been delivered (the modal closes)"
    );
    assert_opened_and_recorded(h.state(), dir.path(), "welcome screen");
}

/// The palette path, already correct when the defect was found. Pinned so the
/// surface that motivated the seam cannot silently regress off it.
#[test]
fn the_palette_open_folder_records_the_recent_folders_mru() {
    let dir = temp_folder("palette");
    let mut app = app();
    super::dialogs::test_hooks::set_next_pick_folder(dir.path().to_path_buf());

    app.execute_builtin(BuiltinCommand::OpenFolder);

    assert_opened_and_recorded(&app, dir.path(), "command palette");
}
