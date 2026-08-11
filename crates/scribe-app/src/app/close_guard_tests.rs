//! The unsaved-changes close guard.
//!
//! Before this guard existed, `frame_tick`'s two-phase close was a ghost-window
//! fix ONLY: any close request hid the window and destroyed it on the next frame,
//! unconditionally. Closing with dirty buffers therefore threw the unsaved text
//! away with no prompt of any kind — there was not so much as a "discard
//! changes?" string anywhere in the crate. (The hot-exit session backup is opt-in
//! AND throttled, so it is not a substitute for asking.)
//!
//! These tests pin BOTH halves of the contract:
//!   * the guard really blocks the close (and Cancel really aborts it), and
//!   * the ghost-window fix it wraps is NOT regressed — every close that does
//!     proceed still hides one frame before it destroys.
//!
//! The button tests drive the REAL modal through the accessibility tree
//! (egui_kittest), not an injected choice, so "the buttons are wired" is proven
//! rather than assumed. `apply_close_choice` is additionally exercised directly
//! for the one path a click cannot reach headlessly: a Save-As the user cancels.
#![allow(clippy::wildcard_imports)]
use super::frame_tick::CloseChoice;
use super::*;
use egui_kittest::kittest::Queryable as _;
use std::path::PathBuf;

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "scr1b3-closeguard-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    // Never inherit a previous run's state: the name is unique among LIVE
    // processes, but PIDs recycle and these dirs are not cleaned up.
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Raw-context frames — enough for the state-level (no-click) assertions.
fn run_frames(app: &mut ScribeApp, n: usize) {
    let ctx = egui::Context::default();
    for _ in 0..n {
        frame_cmds(app, &ctx);
    }
}

/// One frame through the REAL `frame_tick`, returning the `ViewportCommand`s it
/// emitted.
///
/// The `closing` / `close_confirm_open` flags say what the app INTENDED; these
/// say what the WINDOW was actually told to do — and it is `Visible(false)` +
/// `Close`, not a bool, that takes the unsaved buffer away. Asserting only the
/// flag would leave both defects re-introducible: a body that skips the guard's
/// flags but still sends `Close` loses the work with the suite green, and a
/// `begin_hide_then_close` that stopped sending `Visible(false)` would bring the
/// T19.1 DWM ghost back with the suite green. The helper mirrors
/// `deferred_actions_tests::viewport_cmds`, which exists for the same reason.
///
/// Takes the context by reference so a caller can span several frames on ONE
/// context — a fresh `egui::Context` per frame would drop the memory the
/// two-phase close is sequenced through.
fn frame_cmds(app: &mut ScribeApp, ctx: &egui::Context) -> Vec<egui::ViewportCommand> {
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(1100.0, 720.0),
        )),
        ..Default::default()
    };
    let out = ctx.run(input, |ctx| app.frame_tick(ctx));
    out.viewport_output
        .get(&egui::ViewportId::ROOT)
        .map(|v| v.commands.clone())
        .unwrap_or_default()
}

/// Was the window told to go away (hide or destroy) this frame?
fn destroys_or_hides(cmds: &[egui::ViewportCommand]) -> bool {
    cmds.iter().any(|c| {
        matches!(
            c,
            egui::ViewportCommand::Close | egui::ViewportCommand::Visible(false)
        )
    })
}

fn guard_app() -> ScribeApp {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    cfg.appearance.frameless = false;
    ScribeApp::new_test(cfg)
}

/// An app holding one file-backed tab whose buffer diverges from disk.
fn app_with_dirty_file(tag: &str, name: &str, on_disk: &str, edited: &str) -> (ScribeApp, PathBuf) {
    let mut app = guard_app();
    let path = temp_dir(tag).join(name);
    std::fs::write(&path, on_disk).unwrap();
    app.open_path(path.clone());
    let a = app.active;
    app.tabs[a].set_text(edited.to_string());
    assert!(
        app.tabs[a].is_dirty(),
        "fixture precondition: the tab must actually be dirty"
    );
    (app, path)
}

fn harness(app: ScribeApp) -> egui_kittest::Harness<'static, ScribeApp> {
    egui_kittest::Harness::builder()
        .with_size(egui::Vec2::new(1100.0, 760.0))
        .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app)
}

/// Raise the prompt through the REAL close path and settle a frame.
fn armed(app: ScribeApp) -> egui_kittest::Harness<'static, ScribeApp> {
    let mut h = harness(app);
    h.run();
    h.state_mut().want_close = true;
    // `run` (not `step`): a single step queues the click/paint but does not
    // settle the interaction — the working modal tests in `report_issue_tests`
    // use `run` for exactly this reason.
    h.run();
    assert!(
        h.state().close_confirm_open,
        "precondition: the close request must have raised the prompt"
    );
    h
}

// ───────────────────────────── the guard itself ─────────────────────────────

/// THE defect this pins: a close request with unsaved work must NOT close.
///
/// The old body ran `closing = true` + `Visible(false)` on every close request,
/// so the window went away and the buffer went with it.
#[test]
fn a_close_request_with_unsaved_changes_does_not_close() {
    let (mut app, _path) = app_with_dirty_file("noclose", "notes.md", "on disk", "edited");
    app.want_close = true;
    let ctx = egui::Context::default();

    let cmds = frame_cmds(&mut app, &ctx);
    assert!(
        !destroys_or_hides(&cmds),
        "the window must be told neither to hide nor to close while work is \
         unsaved, got: {cmds:?}"
    );
    assert!(
        !app.closing,
        "a close request with unsaved changes must NOT start the close"
    );
    assert!(
        app.close_confirm_open,
        "it must raise the unsaved-changes confirmation instead"
    );

    // And it must keep NOT closing while the prompt is unanswered — a guard that
    // only delays by one frame is not a guard.
    for frame in 0..3 {
        let cmds = frame_cmds(&mut app, &ctx);
        assert!(
            !destroys_or_hides(&cmds),
            "frame {frame} after the prompt: still no hide/close, got: {cmds:?}"
        );
    }
    assert!(
        !app.closing,
        "the app must stay open while the prompt is unanswered"
    );
}

/// An OS-initiated close (the window ✕ / Alt+F4 path) is guarded too, not just
/// the in-app caption button. `close_requested()` is the OS signal; feeding it
/// through the raw context is the only way to reach that branch headlessly.
#[test]
fn an_os_close_request_with_unsaved_changes_is_guarded_too() {
    let (mut app, _path) = app_with_dirty_file("osclose", "notes.md", "on disk", "edited");
    let ctx = egui::Context::default();
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(1100.0, 720.0),
        )),
        viewport_id: egui::ViewportId::ROOT,
        viewports: std::iter::once((
            egui::ViewportId::ROOT,
            egui::ViewportInfo {
                // What `close_requested()` reads: the OS-delivered Close event.
                events: vec![egui::ViewportEvent::Close],
                ..Default::default()
            },
        ))
        .collect(),
        ..Default::default()
    };
    let _ = ctx.run(input, |ctx| app.frame_tick(ctx));

    assert!(!app.closing, "an OS close with unsaved work must not close");
    assert!(
        app.close_confirm_open,
        "an OS close with unsaved work must raise the prompt"
    );
}

/// The ghost-window fix this guard wraps is NOT regressed: with nothing unsaved,
/// a close request still HIDES on frame 1 and only DESTROYS on frame 2.
///
/// Asserted on the emitted `ViewportCommand`s, not on `closing`: the DWM ghost
/// (T19.1) comes back if the hide command stops being sent, and a `closing` flag
/// cannot see that.
#[test]
fn a_clean_close_still_hides_before_it_destroys() {
    let mut app = guard_app();
    assert!(
        !app.has_unsaved_tabs(),
        "precondition: a fresh app has nothing unsaved"
    );
    app.want_close = true;
    let ctx = egui::Context::default();

    let first = frame_cmds(&mut app, &ctx);
    assert!(
        first.contains(&egui::ViewportCommand::Visible(false)),
        "phase 1 must HIDE the window, got: {first:?}"
    );
    assert!(
        !first.contains(&egui::ViewportCommand::Close),
        "phase 1 must NOT destroy it in the same frame — that is the ghost bug, \
         got: {first:?}"
    );
    assert!(
        app.closing,
        "with nothing unsaved the first frame latches the hide-then-close phase"
    );
    assert!(!app.want_close, "want_close consumed");
    assert!(
        !app.close_confirm_open,
        "no prompt when there is nothing to lose"
    );

    let second = frame_cmds(&mut app, &ctx);
    assert!(
        second.contains(&egui::ViewportCommand::Close),
        "phase 2 destroys the now-hidden window, got: {second:?}"
    );
}

/// An empty untitled scratch buffer is not unsaved work — prompting on it would
/// make the guard fire on every single close and train the user to click through.
#[test]
fn an_empty_scratch_buffer_does_not_trigger_the_prompt() {
    let mut app = guard_app();
    app.want_close = true;
    run_frames(&mut app, 1);
    assert!(
        app.closing,
        "an empty scratch buffer must not block the close"
    );
}

/// A buffer with unsaved text but no path (an untitled note) IS unsaved work.
#[test]
fn an_untitled_buffer_with_text_is_unsaved_work() {
    let mut app = guard_app();
    let a = app.active;
    app.tabs[a].set_text("typed but never saved".into());
    assert!(app.has_unsaved_tabs());
    app.want_close = true;
    run_frames(&mut app, 1);
    assert!(
        !app.closing,
        "an untitled note with text must block the close"
    );
    assert!(app.close_confirm_open);
}

/// A file typed into and then typed BACK to exactly its on-disk content has
/// nothing to lose, so it must not prompt.
///
/// The guard's predicate is "differs from disk", not "was ever touched" — a
/// was-edited flag would pass every other test in this file while nagging on a
/// close the user has nothing at stake in, which is how a guard gets clicked
/// through without being read.
#[test]
fn a_file_edited_back_to_its_on_disk_content_does_not_prompt() {
    let (mut app, _path) = app_with_dirty_file("reverted", "notes.md", "on disk", "edited");
    let a = app.active;
    app.tabs[a].set_text("on disk".to_string());
    assert!(
        !app.has_unsaved_tabs(),
        "precondition: reverting the edit makes the tab clean again"
    );

    app.want_close = true;
    run_frames(&mut app, 1);
    assert!(
        !app.close_confirm_open,
        "a buffer identical to disk must not raise the prompt"
    );
    assert!(app.closing, "…and the close proceeds normally");
}

// ────────────────────────────── the prompt's UI ─────────────────────────────

/// The prompt offers all three answers and names the files at risk — a prompt
/// that says only "unsaved changes" leaves the user unable to judge Discard.
#[test]
fn the_prompt_offers_three_answers_and_names_the_unsaved_files() {
    let (app, _path) = app_with_dirty_file("names", "budget.md", "on disk", "edited");
    let h = armed(app);
    for label in ["Save and close", "Discard and close", "Cancel"] {
        assert!(
            h.query_by_label(label).is_some(),
            "the close prompt must offer `{label}`"
        );
    }
    assert!(
        h.query_by_label("budget.md").is_some(),
        "the prompt must name the file with unsaved changes"
    );
}

/// Cancel GENUINELY aborts: no hide, no destroy, now or later.
#[test]
fn cancel_genuinely_aborts_the_close() {
    let (app, _path) = app_with_dirty_file("cancel", "notes.md", "on disk", "edited");
    let mut h = armed(app);

    h.get_by_label("Cancel").click();
    // `step`, not `run`: `run` settles only while the UI stops repainting, so a
    // Cancel that (wrongly) started the close would blow the max-steps budget
    // instead of failing the assertion that names the defect.
    h.step();

    assert!(!h.state().close_confirm_open, "Cancel dismisses the prompt");
    assert!(!h.state().closing, "Cancel must not start the close");
    assert!(
        h.state().has_unsaved_tabs(),
        "Cancel keeps the unsaved work"
    );

    // Later frames must not "resume" the cancelled close.
    h.step();
    h.step();
    assert!(
        !h.state().closing,
        "a cancelled close must stay cancelled on later frames"
    );
}

/// Esc maps to Cancel — the safe answer — like the other dialogs.
#[test]
fn escape_cancels_the_prompt() {
    let (app, _path) = app_with_dirty_file("esc", "notes.md", "on disk", "edited");
    let mut h = armed(app);

    h.key_press(egui::Key::Escape);
    h.step();

    assert!(!h.state().close_confirm_open, "Esc dismisses the prompt");
    assert!(!h.state().closing, "Esc must not close the app");
    assert!(h.state().has_unsaved_tabs(), "Esc keeps the unsaved work");
}

/// Discard closes deliberately — and still hides FIRST, so the ghost-window fix
/// survives the new path.
#[test]
fn discard_closes_and_still_hides_before_destroying() {
    let (app, path) = app_with_dirty_file("discard", "notes.md", "on disk", "edited");
    let mut h = armed(app);

    h.get_by_label("Discard and close").click();
    // `step`, not `run`: the resulting close latch repaints every frame (the
    // two-phase hide-then-destroy), which would trip `run`'s max-steps guard —
    // the same reason `e2e_overlays` steps the caption-close test.
    h.step();

    assert!(h.state().closing, "Discard must start the close");
    assert!(
        !h.state().close_confirm_open,
        "Discard dismisses the prompt"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "on disk",
        "Discard means discard — it must NOT secretly save"
    );
}

/// Save writes every dirty buffer through the real save path, then closes.
#[test]
fn save_and_close_saves_every_dirty_tab_then_closes() {
    let dir = temp_dir("saveall");
    let first = dir.join("one.md");
    let second = dir.join("two.md");
    std::fs::write(&first, "old one").unwrap();
    std::fs::write(&second, "old two").unwrap();

    let mut app = guard_app();
    app.open_path(first.clone());
    app.open_path(second.clone());
    for tab in &mut app.tabs {
        if tab.doc.path().is_some() {
            let edited = format!("new {}", tab.doc.file_name());
            tab.set_text(edited);
        }
    }
    assert!(app.has_unsaved_tabs(), "precondition: both tabs dirty");

    let mut h = armed(app);
    h.get_by_label("Save and close").click();
    // `step`, not `run`: the resulting close latch repaints every frame (the
    // two-phase hide-then-destroy), which would trip `run`'s max-steps guard —
    // the same reason `e2e_overlays` steps the caption-close test.
    h.step();

    assert_eq!(
        std::fs::read_to_string(&first).unwrap(),
        "new one.md",
        "Save must write the FIRST dirty tab, not only the active one"
    );
    assert_eq!(
        std::fs::read_to_string(&second).unwrap(),
        "new two.md",
        "Save must write the second dirty tab too"
    );
    assert!(
        !h.state().has_unsaved_tabs(),
        "every buffer is clean after Save"
    );
    assert!(h.state().closing, "Save then closes");
}

/// THE way "Save" silently becomes "Discard": an untitled buffer routes through
/// Save-As, the user cancels the picker, nothing is written — and a close that
/// proceeded anyway would destroy exactly the work the user just tried to save.
///
/// Driven through `apply_close_choice` because the cancel is the dialog seam's
/// default (nothing injected ⇒ the picker reports Cancel), which a click cannot
/// express any more directly.
#[test]
fn a_cancelled_save_as_never_becomes_a_discard() {
    let mut app = guard_app();
    let a = app.active;
    app.tabs[a].set_text("typed but never saved".into());
    app.close_confirm_open = true;
    let ctx = egui::Context::default();

    // Nothing injected into `dialogs::test_hooks` ⇒ the Save-As picker cancels.
    let started = app.apply_close_choice(&ctx, CloseChoice::Save);

    assert!(!started, "a cancelled Save-As must NOT start the close");
    assert!(!app.closing, "…and must not latch the hide phase");
    assert!(
        app.has_unsaved_tabs(),
        "the buffer is still unsaved — nothing was written"
    );
    assert!(
        app.close_confirm_open,
        "the prompt stays up so the user can choose again"
    );
}

/// The ROOT CAUSE behind "Save and close" silently becoming a discard.
///
/// `save_active` synced the buffer into the document model (`doc.set_text`)
/// BEFORE noticing the buffer had no path to save to. `is_dirty()` is
/// `text != doc.text()`, so that sync marked the buffer CLEAN — and then the
/// Save-As picker could be cancelled, leaving unsaved text with no `*` marker,
/// nothing on disk, and a close guard that saw nothing left to protect. The doc
/// model must not be told about a save that has not happened.
#[test]
fn a_cancelled_save_as_leaves_the_buffer_marked_unsaved() {
    let mut app = guard_app();
    let a = app.active;
    app.tabs[a].set_text("typed but never saved".into());

    // Nothing injected into `dialogs::test_hooks` ⇒ the Save-As picker cancels.
    app.save_active();

    assert!(
        app.tabs[a].is_dirty(),
        "a cancelled Save-As must leave the buffer unsaved — clearing the dirty \
         flag here is what let unsaved work be closed away"
    );
    assert!(
        app.tabs[a].title().starts_with("* "),
        "…and the unsaved marker must stay in the tab title, got: {:?}",
        app.tabs[a].title()
    );
    assert!(
        app.has_unsaved_tabs(),
        "…so the close guard still sees work to protect"
    );
}

/// `save_all_dirty` leaves the user on the tab they were editing — a save-all
/// that silently jumps focus to the last file is its own small data hazard.
#[test]
fn save_all_dirty_restores_the_active_tab() {
    let dir = temp_dir("active");
    let first = dir.join("one.md");
    let second = dir.join("two.md");
    std::fs::write(&first, "old one").unwrap();
    std::fs::write(&second, "old two").unwrap();

    let mut app = guard_app();
    app.open_path(first);
    app.open_path(second);
    for (i, tab) in app.tabs.iter_mut().enumerate() {
        if tab.doc.path().is_some() {
            tab.set_text(format!("edited {i}"));
        }
    }
    app.active = 0;

    app.save_all_dirty();

    assert_eq!(app.active, 0, "the active tab is restored after save-all");
    assert!(!app.has_unsaved_tabs(), "every dirty tab was saved");
}
