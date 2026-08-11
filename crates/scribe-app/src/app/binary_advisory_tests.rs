//! Coverage for the binary-file ADVISORY TOAST — the consumer of
//! `Document::looks_binary()`, not the getter.
//!
//! `scribe-core`'s `open_sets_looks_binary_from_the_bytes` stops at the getter,
//! which is exactly where the code was already dormant before this feature
//! landed: the flag was computed and nothing read it. An adversarial review cut
//! both consumers (`open_path` in `mod.rs`, `open_dialog` in `file_ops.rs`) down
//! to `let _ = binary;` plus an unconditional "opened …" status, and the whole
//! 1328-test suite stayed green — the wire was live but unguarded.
//!
//! These tests assert on the OBSERVABLE user-facing artifact (the toast text) at
//! BOTH chokepoints, in both directions:
//!
//! * a file containing a NUL opens WITH the advisory toast, and
//! * a plain-text file opens with NO toast at all (the negative is what kills an
//!   "always warn" mutant, and the positive is what kills the discard mutant).
#![allow(clippy::wildcard_imports)]
use super::*;
use std::path::PathBuf;

/// The user-facing advisory substring both chokepoints must emit. Kept in one
/// place so a copy-edit of the message updates the pin, never silently unpins it.
const ADVISORY: &str = "looks like a binary file";

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "scr1b3-binadvisory-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    // PIDs recycle and these dirs are not swept — never inherit a prior run.
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn test_app() -> ScribeApp {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    ScribeApp::new_test(cfg)
}

// ---- open_path (mod.rs) ----

#[test]
fn open_path_toasts_the_binary_advisory_for_a_nul_bearing_file() {
    let dir = temp_dir("openpath-bin");
    let bin = dir.join("payload.bin");
    std::fs::write(&bin, b"\x7fELF\x00\x00binary\x00payload").unwrap();

    let mut app = test_app();
    assert!(
        app.toast.is_none(),
        "precondition: no toast before the open"
    );
    app.open_path(bin.clone());

    let toast = app
        .toast
        .as_deref()
        .expect("opening a binary file must raise the advisory toast");
    assert!(
        toast.contains(ADVISORY),
        "the toast must warn the text may be garbled, got {toast:?}"
    );
    assert!(
        toast.contains(&bin.display().to_string()),
        "the toast must name the offending file, got {toast:?}"
    );
    // The buffer still opens — the advisory is advisory, not a refusal.
    assert!(
        app.tabs.iter().any(|t| t.doc.path() == Some(bin.as_path())),
        "the binary file must still open into a tab"
    );
}

#[test]
fn open_path_leaves_no_toast_for_a_plain_text_file() {
    let dir = temp_dir("openpath-txt");
    let txt = dir.join("notes.txt");
    std::fs::write(&txt, b"just text\nsecond line\n").unwrap();

    let mut app = test_app();
    app.open_path(txt.clone());

    assert!(
        app.toast.is_none(),
        "a plain-text open must raise NO advisory toast, got {:?}",
        app.toast
    );
    // `open_path` continues into `save_config()`, which overwrites `status` —
    // so the tab (not the status line) is what proves the open really happened.
    assert!(
        app.tabs.iter().any(|t| t.doc.path() == Some(txt.as_path())),
        "the text file must open into a tab"
    );
}

// ---- open_dialog (file_ops.rs) ----

#[test]
fn open_dialog_toasts_the_binary_advisory_for_a_nul_bearing_file() {
    let dir = temp_dir("dialog-bin");
    let bin = dir.join("picked.bin");
    std::fs::write(&bin, b"MZ\x00\x00\x90\x00binary picked").unwrap();

    let mut app = test_app();
    super::dialogs::test_hooks::set_next_pick_file(bin.clone());
    app.open_dialog();

    let toast = app
        .toast
        .as_deref()
        .expect("picking a binary file must raise the advisory toast");
    assert!(
        toast.contains(ADVISORY),
        "the toast must warn the text may be garbled, got {toast:?}"
    );
    assert!(
        app.tabs.iter().any(|t| t.doc.path() == Some(bin.as_path())),
        "the picked binary file must still open into a tab"
    );
}

#[test]
fn open_dialog_leaves_no_toast_for_a_plain_text_file() {
    let dir = temp_dir("dialog-txt");
    let txt = dir.join("picked.md");
    std::fs::write(&txt, b"# heading\n\nbody\n").unwrap();

    let mut app = test_app();
    super::dialogs::test_hooks::set_next_pick_file(txt.clone());
    app.open_dialog();

    assert!(
        app.toast.is_none(),
        "a plain-text pick must raise NO advisory toast, got {:?}",
        app.toast
    );
    assert!(
        app.status.contains("opened"),
        "a plain-text pick takes the ordinary status path, got {:?}",
        app.status
    );
}
