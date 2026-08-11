//! The egui `TextEdit` path's undo history is the ONE text-derived cache that
//! is not a field of `EditorTab` — it lives in egui's own memory as
//! `TextEditState.undoer`, keyed by `doc_id`. `set_text` has no
//! `&egui::Context`, so `note_text_mutated` cannot reach it, and the exhaustive
//! destructure in `set_text_invalidates_every_text_derived_cache` is
//! structurally blind to it. That blind spot let a silent data-loss defect
//! survive the sibling fix that closed the identical hole on the rope path
//! (`invalidate_rope_state`).
//!
//! These tests drive the REAL `frame_tick` loop with real keystrokes, so they
//! assert what a user actually gets — not that a flag was set. The three of
//! them are a matched set and only mean something together:
//!
//! 1. [`ctrl_z_after_a_silent_external_reload_cannot_resurrect_the_pre_reload_document`]
//!    — the defect. Two keystrokes must not destroy a `git pull`.
//! 2. [`ordinary_typing_is_still_undoable`] — the counter-assertion. A fix that
//!    clears the undoer too eagerly (e.g. hooked onto the `TextEdit`'s own
//!    `.changed()` writer, or onto `note_text_mutated`) would break plain undo
//!    while making test 1 pass. This is what stops an over-broad fix.
//! 3. [`a_user_issued_buffer_command_is_still_undoable`] — the reason
//!    `set_text_keep_undo` exists at all. Hooking EVERY `set_text` would fix
//!    the data loss and silently cost the user "Ctrl+Z reverts a palette
//!    command" on ~20 call sites. This test is the evidence that cost was not
//!    paid.
#![allow(clippy::wildcard_imports)]
use super::e2e::Driver;
use super::*;
use std::path::{Path, PathBuf};

/// A private scratch dir, never inheriting a previous run's state (PIDs
/// recycle, and these dirs are not cleaned up).
fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "scr1b3-undoinval-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn app_with_file(name: &str, text: &str) -> (ScribeApp, PathBuf) {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    let mut app = ScribeApp::new_test(cfg);
    let path = temp_dir("file").join(name);
    std::fs::write(&path, text).unwrap();
    app.open_path(path.clone());
    (app, path)
}

fn app_with_text(text: &str) -> ScribeApp {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    let mut app = ScribeApp::new_test(cfg);
    app.tabs[0].set_text(text.to_string());
    app
}

/// Write `text` with an mtime the poll is guaranteed to read as newer — a
/// same-second write can land on an unchanged mtime (filesystem timestamp
/// granularity), which would make this flaky rather than wrong.
fn write_with_newer_mtime(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap();
    let future = std::time::SystemTime::now() + std::time::Duration::from_secs(10);
    let f = std::fs::File::options().write(true).open(path).unwrap();
    f.set_modified(future).unwrap();
}

/// A primary click (press + release) as a raw event pair.
fn click_events(pos: egui::Pos2) -> Vec<egui::Event> {
    vec![
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        },
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        },
    ]
}

/// The per-tab central-editor `Id` the render loop keys `TextEditState` on.
fn central_editor_id(app: &ScribeApp) -> egui::Id {
    egui::Id::new("scr1b3-central-editor").with(app.tabs[app.active].doc_id)
}

/// Does egui's undoer for this widget hold a state it would restore on Ctrl+Z?
///
/// Read through egui's OWN `has_undo` against the live caret + buffer, i.e. the
/// exact predicate `TextEdit`'s Ctrl+Z branch consults — not a proxy for it.
fn undoer_has_undo(ctx: &egui::Context, id: egui::Id, text: &str) -> bool {
    let Some(state) = egui::TextEdit::load_state(ctx, id) else {
        return false;
    };
    let cursor = state.cursor.char_range().unwrap_or_default();
    state.undoer().has_undo(&(cursor, text.to_owned()))
}

/// Focus the central editor by clicking into it, and prove the click landed.
fn focus_editor(d: &Driver, app: &mut ScribeApp) {
    d.frame(
        app,
        egui::Modifiers::NONE,
        click_events(egui::pos2(300.0, 300.0)),
    );
    let id = central_editor_id(app);
    assert!(
        d.ctx().memory(|m| m.has_focus(id)),
        "precondition — the click must focus the central editor, or no \
         keystroke below reaches the widget and every assertion is vacuous"
    );
}

// ---------------------------------------------------------------------------
// 1. THE DEFECT
// ---------------------------------------------------------------------------

/// A file open on the `TextEdit` path; the user types and saves, seeding egui's
/// undoer with the pre-edit document. An external tool then rewrites the file
/// (`git pull`, `cargo fmt`, another editor). The tab is clean, so
/// `poll_external_disk_changes` reloads it SILENTLY — no banner, and the change
/// bar is rebaselined, so nothing on screen marks the buffer as new.
///
/// One Ctrl+Z then handed egui a snapshot of the PREVIOUS document and
/// `TextEdit` did an unconditional whole-buffer `replace_with`; the next Ctrl+S
/// wrote it back over the pulled file. Two keystrokes, no warning, no visual
/// cue, and the pull was gone.
///
/// Every precondition is asserted BEFORE the payload so this cannot pass
/// vacuously: the `TextEdit` path (not the rope path) is rendering, the click
/// focused it, the keystroke reached the buffer, egui's undoer genuinely holds
/// an undo point, and the silent reload actually happened.
#[test]
fn ctrl_z_after_a_silent_external_reload_cannot_resurrect_the_pre_reload_document() {
    const V1: &str = "v1 from disk\n";
    const V2: &str = "v2 from disk — the pulled content\n";

    let (mut app, path) = app_with_file("pulled.md", V1);
    let d = Driver::new();
    d.idle(&mut app);

    // -- precondition: the egui TextEdit path owns this tab, not the rope path.
    // The rope path's invalidation is already complete; a rope-rendered tab
    // would make this test assert nothing about the hole being closed here.
    assert!(
        !app.active_editor_is_rope(),
        "precondition — this tab must render through the egui `TextEdit`"
    );
    assert!(
        app.tabs[app.active].text.rope_state().is_none(),
        "precondition — the rope path must not have claimed this tab"
    );

    focus_editor(&d, &mut app);

    // -- the seeding keystroke, then a save (which is what leaves the tab CLEAN
    // and therefore eligible for the silent reload below).
    d.frame(
        &mut app,
        egui::Modifiers::NONE,
        vec![egui::Event::Text("X".to_string())],
    );
    let typed = app.tabs[app.active].text.to_string();
    assert_ne!(
        typed, V1,
        "precondition — the keystroke must actually reach the buffer"
    );
    assert!(
        typed.contains('X'),
        "precondition — the typed character must be the one that landed, got {typed:?}"
    );
    app.save_active();
    d.idle(&mut app);
    assert!(
        !app.tabs[app.active].is_dirty(),
        "precondition — the tab must be clean, or the poll takes the WARN path \
         and no silent reload happens"
    );

    // -- precondition: egui's undoer really is holding a snapshot to restore.
    // Without this, a later green could mean "undo had nothing to give back"
    // rather than "the stale history was dropped".
    let id = central_editor_id(&app);
    assert!(
        undoer_has_undo(d.ctx(), id, &app.tabs[app.active].text),
        "precondition — egui's undoer must hold an undo point, or Ctrl+Z is a \
         no-op for reasons unrelated to this fix"
    );

    // -- the external rewrite + the SILENT reload, through the real poll inside
    // a real frame (so the ordering "poll, then clear, then render" is what is
    // under test, not a hand-sequenced approximation).
    write_with_newer_mtime(&path, V2);
    app.last_disk_poll_frame = u64::MAX; // "never polled" sentinel
    d.idle(&mut app);

    assert_eq!(
        app.tabs[app.active].text, V2,
        "precondition — the silent reload must have happened"
    );
    assert!(
        !app.tabs[app.active].external_change,
        "precondition — the reload is SILENT (no banner); that invisibility is \
         what makes the data loss so severe"
    );

    // -- the payload: one Ctrl+Z.
    d.key(&mut app, egui::Key::Z, egui::Modifiers::COMMAND);

    let after = app.tabs[app.active].text.to_string();
    assert_ne!(
        after, V1,
        "SILENT DATA LOSS — Ctrl+Z resurrected the pre-reload document over the \
         pulled content. The next Ctrl+S writes it to disk and the pull is gone."
    );
    assert_ne!(
        after, typed,
        "SILENT DATA LOSS — Ctrl+Z resurrected the pre-reload buffer over the \
         pulled content."
    );
    assert_eq!(
        after, V2,
        "after an external replacement the first Ctrl+Z must be a no-op — the \
         same honest outcome `invalidate_rope_state` gives the rope path"
    );
}

// ---------------------------------------------------------------------------
// 2. THE COUNTER-ASSERTION
// ---------------------------------------------------------------------------

/// Undo must still work for ordinary typing.
///
/// This is the fence around the fix above. Invalidating the undoer on the
/// `TextEdit`'s own `.changed()` writer, or inside `note_text_mutated` (which
/// the completion accept and the multi-cursor replay also call), would make
/// test 1 pass while quietly destroying undo for every keystroke. A fix is only
/// correct if BOTH tests are green.
#[test]
fn ordinary_typing_is_still_undoable() {
    const BEFORE: &str = "hello\n";

    let mut app = app_with_text(BEFORE);
    let d = Driver::new();
    d.idle(&mut app);
    assert!(
        !app.active_editor_is_rope(),
        "precondition — the egui `TextEdit` path owns this tab"
    );

    focus_editor(&d, &mut app);

    d.frame(
        &mut app,
        egui::Modifiers::NONE,
        vec![egui::Event::Text("Z".to_string())],
    );
    let typed = app.tabs[app.active].text.to_string();
    assert_ne!(
        typed, BEFORE,
        "precondition — the keystroke must actually reach the buffer"
    );
    assert!(
        !app.tabs[app.active].text.textedit_undo_stale(),
        "the user's own keystroke is NOT an external replacement — flagging it \
         would clear the very history undo needs"
    );

    d.key(&mut app, egui::Key::Z, egui::Modifiers::COMMAND);

    assert_eq!(
        app.tabs[app.active].text, BEFORE,
        "Ctrl+Z must still revert ordinary typing — an over-broad invalidation \
         that clears the undoer on every buffer write breaks undo entirely"
    );
}

// ---------------------------------------------------------------------------
// 3. WHY `set_text_keep_undo` EXISTS
// ---------------------------------------------------------------------------

/// A user-issued in-buffer command must stay revertible.
///
/// `apply_buffer_transform` is the shared implementation behind the
/// whole-buffer palette commands (sort lines, trim, indent conversion, case
/// transforms). Its result is a pure function of the CURRENT buffer, so the
/// snapshot egui's undoer holds is a state the user genuinely had a moment ago
/// — restoring it destroys nothing they have not seen. That is what undo is
/// for, and it is exactly what routing every `set_text` through the
/// invalidation would have cost, silently, on ~20 call sites.
#[test]
fn a_user_issued_buffer_command_is_still_undoable() {
    const BEFORE: &str = "beta\nalpha\n";

    let mut app = app_with_text(BEFORE);
    let d = Driver::new();
    d.idle(&mut app);
    assert!(
        !app.active_editor_is_rope(),
        "precondition — the egui `TextEdit` path owns this tab"
    );

    focus_editor(&d, &mut app);

    let id = central_editor_id(&app);
    assert!(
        undoer_has_undo(d.ctx(), id, &format!("{BEFORE}x")),
        "precondition — egui's undoer must already hold the pre-command \
         buffer, or this test cannot tell 'undo was preserved' from 'undo was \
         never seeded'"
    );

    app.apply_buffer_transform("sorted", |t| {
        let mut lines: Vec<&str> = t.lines().collect();
        lines.sort_unstable();
        let mut out = lines.join("\n");
        out.push('\n');
        out
    });
    assert_eq!(
        app.tabs[app.active].text, "alpha\nbeta\n",
        "precondition — the command must actually transform the buffer"
    );
    assert!(
        !app.tabs[app.active].text.textedit_undo_stale(),
        "a user-issued in-buffer command routes through `set_text_keep_undo`, \
         which must NOT flag the undo history stale"
    );

    d.idle(&mut app);
    d.key(&mut app, egui::Key::Z, egui::Modifiers::COMMAND);

    assert_eq!(
        app.tabs[app.active].text, BEFORE,
        "Ctrl+Z must still revert a user-issued palette command — this is the \
         behaviour `set_text_keep_undo` exists to preserve, and the reason the \
         fix is a classified seam rather than a blanket invalidation"
    );
}
