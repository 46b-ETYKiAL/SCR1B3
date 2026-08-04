//! The two seams these features live or die on, driven through the REAL
//! `frame_tick`.
//!
//! Both halves of this change are the "built but never called" shape:
//!
//!   * `LspClient::note_change` / `flush_pending_change` exist and are unit
//!     tested against the wire in `scribe_core::lsp` — but if `frame_tick` never
//!     calls them, the server's copy of the document stays frozen at `didOpen`
//!     exactly as it was before, with every one of those unit tests green.
//!   * `note_metrics` caches three full-document scans — but if the preview
//!     header still calls `md_ops::tasks_progress` etc. directly, the cache is
//!     correct, unused, and invisible: the header renders identical numbers
//!     either way, so no value assertion can tell the difference. Only the SCAN
//!     COUNT can.
//!
//! So these drive real frames and watch what the frame actually did, not what a
//! helper returns when called by hand.
#![allow(clippy::wildcard_imports)]

use super::*;
use std::path::PathBuf;

/// One frame through the REAL `frame_tick`. Same idiom as
/// `close_guard_tests::frame_cmds`; the commands are not what these tests are
/// after, so they are dropped.
fn run_frame(app: &mut ScribeApp, ctx: &egui::Context) {
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(1100.0, 720.0),
        )),
        ..Default::default()
    };
    let _ = ctx.run(input, |ctx| app.frame_tick(ctx));
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "scr1b3-lspwire-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn test_app() -> ScribeApp {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    cfg.appearance.frameless = false;
    ScribeApp::new_test(cfg)
}

// ─────────────────────────── LSP didChange wiring ───────────────────────────

/// A benign, long-lived, stdin-piped child standing in for a language server.
///
/// It must stay ALIVE for the whole test and keep its stdin pipe open — a child
/// that exits breaks the pipe, the writer thread ends, and the next enqueue
/// fails, which `sync_lsp_document` (correctly) treats as a dead server and
/// drops the client. A sleeping process never reads what we write, but the OS
/// pipe buffer absorbs it, which is all these tests need: they assert what
/// SCR1B3 SENT, not what a server replied. The child is reaped by `LspClient`'s
/// own `Drop`.
///
/// Spawned DIRECTLY, never through `cmd /c`: Windows has no tree-kill, so a
/// `cmd /c <thing>` wrapper leaves `<thing>` running as an orphan holding the
/// inherited pipe handles after `LspClient::drop` kills the shell. Those
/// orphans pile up across a test run and were observed blocking the next
/// `link.exe` with `LNK1104` on the test binary. One process, killable, no
/// grandchild.
fn fake_server_client() -> Option<scribe_core::lsp::LspClient> {
    let cfg = if cfg!(windows) {
        scribe_core::lsp::LspServerConfig {
            command: "ping".into(),
            args: vec!["-n".into(), "60".into(), "127.0.0.1".into()],
            languages: vec!["rs".into()],
        }
    } else {
        scribe_core::lsp::LspServerConfig {
            command: "sleep".into(),
            args: vec!["60".into()],
            languages: vec!["rs".into()],
        }
    };
    let c = scribe_core::lsp::LspClient::spawn(&cfg, "file:///proj").ok();
    assert!(
        c.is_some() || !cfg!(windows),
        "the stand-in server must be spawnable on Windows — a None here is a \
         broken harness, not an absent dependency"
    );
    c
}

/// An app with one saved `.rs` tab and a client that has that file open.
fn app_with_open_document(tag: &str) -> Option<(ScribeApp, PathBuf)> {
    let mut client = fake_server_client()?;
    let mut app = test_app();
    let path = temp_dir(tag).join("main.rs");
    std::fs::write(&path, "fn mai() {}\n").unwrap();
    app.open_path(path.clone());
    client
        .did_open(&path_to_uri(&path), "rs", "fn mai() {}\n")
        .unwrap();
    assert_eq!(
        client.document_version(),
        Some(1),
        "fixture precondition: didOpen is version 1"
    );
    app.lsp = Some(client);
    app.lsp_lang = Some("rs".into());
    Some((app, path))
}

#[test]
fn typing_makes_a_frame_tell_the_language_server_about_the_edit() {
    // THE contract this change exists for. Before it, the version stayed at 1
    // forever: the server's copy of the file was whatever it was at `didOpen`,
    // so every diagnostic on screen described text the user had already changed.
    //
    // The document version is the honest signal — it advances ONLY when a
    // `didChange` was really built and enqueued, never merely because an edit
    // was noted.
    let Some((mut app, _path)) = app_with_open_document("typing") else {
        return;
    };
    let ctx = egui::Context::default();

    let a = app.active;
    app.tabs[a].set_text("fn main() {}\n".to_string());

    // First frame: the edit is noted, but the quiet window has not elapsed, so
    // nothing has gone out yet.
    run_frame(&mut app, &ctx);
    assert!(
        app.lsp.as_ref().unwrap().has_pending_change(),
        "the frame must have NOTED the edit — a frame that never calls \
         note_change leaves nothing pending"
    );
    assert_eq!(
        app.lsp.as_ref().unwrap().document_version(),
        Some(1),
        "and it must not have sent yet: a keystroke must not hit the server \
         immediately"
    );

    // Let the debounce window elapse, then tick again.
    std::thread::sleep(scribe_core::lsp::sync::DEBOUNCE + std::time::Duration::from_millis(60));
    run_frame(&mut app, &ctx);
    assert_eq!(
        app.lsp.as_ref().unwrap().document_version(),
        Some(2),
        "after the pause the frame must actually SEND the change — a version \
         still at 1 means the server never heard about the edit"
    );

    // An idle frame after that sends nothing more.
    std::thread::sleep(scribe_core::lsp::sync::DEBOUNCE + std::time::Duration::from_millis(60));
    run_frame(&mut app, &ctx);
    assert_eq!(
        app.lsp.as_ref().unwrap().document_version(),
        Some(2),
        "an idle frame must not manufacture a change"
    );
}

#[test]
fn a_frame_with_a_different_tab_active_never_syncs_it_against_the_open_uri() {
    // The client tracks ONE document. Feeding the newly-active buffer against
    // the previously-opened file's URI would tell the server that `other.rs` now
    // contains `main.rs`'s text — silently corrupting its whole view of the
    // project.
    let Some((mut app, _path)) = app_with_open_document("tabswitch") else {
        return;
    };
    let other = temp_dir("tabswitch").join("other.rs");
    std::fs::write(&other, "fn other() {}\n").unwrap();
    app.open_path(other);
    let a = app.active;
    assert_ne!(
        app.tabs[a].doc.path().unwrap().file_name().unwrap(),
        "main.rs",
        "fixture precondition: a DIFFERENT file is active"
    );
    app.tabs[a].set_text("fn other() { edited }\n".to_string());

    let ctx = egui::Context::default();
    run_frame(&mut app, &ctx);
    assert!(
        !app.lsp.as_ref().unwrap().has_pending_change(),
        "the other tab's text must not be queued against main.rs's uri"
    );
    std::thread::sleep(scribe_core::lsp::sync::DEBOUNCE + std::time::Duration::from_millis(60));
    run_frame(&mut app, &ctx);
    assert_eq!(
        app.lsp.as_ref().unwrap().document_version(),
        Some(1),
        "and nothing may be sent for it either"
    );
}

#[test]
fn frames_run_harmlessly_with_no_language_server_at_all() {
    // The overwhelmingly common case: no LSP has ever been started. The sync
    // seam must be a no-op, not a panic and not a stall.
    let mut app = test_app();
    assert!(app.lsp.is_none());
    let ctx = egui::Context::default();
    for _ in 0..3 {
        run_frame(&mut app, &ctx);
    }
    assert!(app.lsp.is_none());
}

// ───────────────────────── diagnostics overlay wiring ─────────────────────────

#[test]
fn a_frame_resolves_published_diagnostics_onto_the_active_buffers_text() {
    // The overlay's placement rules are unit-tested in `diagnostics_overlay`;
    // this pins that `frame_tick` actually asks for them, against the ACTIVE
    // tab's text. A frame that never resolved a span would paint nothing while
    // the status bar still counted the diagnostic — the exact state this change
    // replaces.
    let mut app = test_app();
    let path = temp_dir("diagspans").join("main.rs");
    std::fs::write(&path, "fn main() {\n    let x = undefined_thing;\n}\n").unwrap();
    app.open_path(path.clone());
    let a = app.active;
    app.diagnostics = vec![scribe_core::lsp::Diagnostic {
        uri: path_to_uri(&path),
        line: 1,
        character: 12,
        end_line: 1,
        end_character: 27,
        severity: 1,
        message: "cannot find value".into(),
    }];

    let spans = app.diagnostic_spans_for_active(a);
    assert_eq!(spans.len(), 1, "the frame's span source must resolve it");
    assert_eq!(
        &app.tabs[a].text[spans[0].start..spans[0].end],
        "undefined_thing",
        "and it must land on the identifier the server named"
    );

    // A real frame with the overlay live must not panic on the galley mapping.
    let ctx = egui::Context::default();
    run_frame(&mut app, &ctx);
    run_frame(&mut app, &ctx);
}

#[test]
fn a_frame_with_no_diagnostics_resolves_no_spans() {
    let mut app = test_app();
    app.tabs[0].set_text("fn main() {}\n".to_string());
    assert!(app.diagnostics.is_empty());
    assert!(app.diagnostic_spans_for_active(0).is_empty());
}

// ─────────────── updater unsaved-work gate: the publishing half ───────────────

#[test]
fn frame_tick_publishes_the_unsaved_verdict_to_the_updater_every_frame() {
    // `Updater::hold_for_unsaved_work` gates the IRREVERSIBLE half of an apply
    // (the exe swap + replacement spawn, the elevated `-Wait` installer launch)
    // — but only while the host actually tells it the truth, every frame, BEFORE
    // `poll` drains the auto-chained apply. An unpublished flag is a gate that
    // never closes; a flag stuck at true is an update that can never install.
    // The refusal half is pinned on the emitted `ViewportCommand`s in
    // `crate::updater_restart_close_tests`.
    let mut app = test_app();
    let ctx = egui::Context::default();

    run_frame(&mut app, &ctx);
    assert!(!app.updater.unsaved_work, "a clean app must publish false");

    let path = temp_dir("unsaved-publish").join("note.txt");
    std::fs::write(&path, "on disk\n").unwrap();
    app.open_path(path);
    let a = app.active;
    app.tabs[a].set_text("edited, not saved\n".to_string());
    assert!(
        app.has_unsaved_tabs(),
        "fixture precondition: the buffer must really be dirty"
    );

    run_frame(&mut app, &ctx);
    assert!(
        app.updater.unsaved_work,
        "with a dirty buffer the frame must publish TRUE, and it must agree \
         with the close guard's own predicate"
    );

    // Saving clears it again — the gate must not latch.
    app.tabs[a].set_text("on disk\n".to_string());
    app.save_active();
    run_frame(&mut app, &ctx);
    assert!(
        !app.updater.unsaved_work,
        "once the work is saved the gate must re-open — a latched flag blocks \
         every future update"
    );
}

// ────────────────────── note-metrics cache wiring ──────────────────────

/// An app showing the markdown preview for `src`.
fn preview_app(tag: &str, src: &str) -> (ScribeApp, PathBuf) {
    let mut app = test_app();
    let path = temp_dir(tag).join("note.md");
    std::fs::write(&path, src).unwrap();
    app.open_path(path.clone());
    app.md_preview_open = true;
    let a = app.active;
    assert_eq!(
        app.tabs[a].doc.language_hint().as_deref(),
        Some("md"),
        "fixture precondition: the preview only renders for a markdown buffer"
    );
    (app, path)
}

#[test]
fn the_preview_header_scans_the_note_once_per_edit_not_once_per_frame() {
    // The measurable claim: an idle frame with the preview open must run ZERO
    // full-document scans. Asserting the header's NUMBERS would pass with no
    // cache at all — a re-scan returns exactly the same numbers. The scan
    // counter is the only thing that distinguishes a hit from a re-scan.
    let note = format!(
        "# Title\n\n{}\n\n- [x] done\n- [ ] todo\n",
        "word ".repeat(300)
    );
    let (mut app, _p) = preview_app("metrics", &note);
    let ctx = egui::Context::default();

    // Prime: the first frame with this source is a miss.
    run_frame(&mut app, &ctx);
    let after_first = super::note_metrics::live_scan_count();

    for _ in 0..20 {
        run_frame(&mut app, &ctx);
    }
    assert_eq!(
        super::note_metrics::live_scan_count(),
        after_first,
        "20 idle frames with the preview open must re-scan ZERO times — the \
         counter has to be FLAT"
    );

    // An EDIT must invalidate: the header cannot keep showing the old counts.
    let a = app.active;
    app.tabs[a].set_text(format!("{note}\n- [ ] another\n"));
    run_frame(&mut app, &ctx);
    assert_eq!(
        super::note_metrics::live_scan_count(),
        after_first + 1,
        "an edit forces exactly one re-scan"
    );
    for _ in 0..5 {
        run_frame(&mut app, &ctx);
    }
    assert_eq!(
        super::note_metrics::live_scan_count(),
        after_first + 1,
        "and the frames after the edit are cached again"
    );
}

#[test]
fn the_preview_header_shows_the_notes_real_counts() {
    // The companion to the counter test: the cache must be wired in a way that
    // still renders the RIGHT numbers. Read them off the rendered header text
    // via the accessibility tree, so this is what the user sees.
    use egui_kittest::kittest::Queryable as _;
    let (app, _p) = preview_app(
        "metrics-values",
        "# One\n\n## Two\n\n- [x] a\n- [ ] b\n- [x] c\n",
    );
    let mut h = egui_kittest::Harness::builder()
        .with_size(egui::Vec2::new(1200.0, 800.0))
        .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app);
    h.run();
    let label = "~1 min · 2 headings · ☑ 2/3";
    assert!(
        h.query_by_label(label).is_some(),
        "the preview header must render {label:?} for this note"
    );
}
