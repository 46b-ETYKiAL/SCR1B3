//! Task-2 wire: a parsed `PATH:LINE[:COLUMN]` jump target must scroll the first
//! opened tab to the requested line on construction. Before this wire, `main.rs`
//! discarded `jump` with `..` and the editor always opened at line 1.
//!
//! `apply_cli_jump` is the owned application seam (called from `ScribeApp::new`);
//! these drive it directly on a headless `new_test` app. Under `new_test` the
//! `line_gutter` is empty, so `goto_line` takes the `line0 * (size*line_height)`
//! fallback — the exact same arithmetic `find_nav_tests` pins.
use super::*;

#[test]
fn apply_cli_jump_scrolls_first_tab_to_the_requested_line() {
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs[0].set_text("l1\nl2\nl3\nl4\nl5\nl6\n".into());
    let size = app.config.fonts.clamped_editor_size();
    let lh = app.config.fonts.clamped_line_height();
    // line 4 (1-based) -> line0 = 3 -> pending = 3 * (size*lh).
    let expected = 3.0_f32 * (size * lh);
    app.pending_scroll = None;
    app.apply_cli_jump(Some((4, Some(2))));
    assert_eq!(
        app.pending_scroll,
        Some(expected),
        "a CLI `file:LINE:COL` jump must scroll the first tab to the requested 1-based line"
    );
    assert!(
        app.status.contains("4:2"),
        "the status hint must surface the line:column jump target, got {:?}",
        app.status
    );
}

#[test]
fn apply_cli_jump_line_only_scrolls_without_a_column_suffix() {
    // A `file:42` jump (no column) still scrolls to the line; the status carries
    // the line-only "go to line N" message from `goto_line`.
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs[0].set_text("a\nb\nc\nd\ne\n".into());
    let size = app.config.fonts.clamped_editor_size();
    let lh = app.config.fonts.clamped_line_height();
    let expected = 2.0_f32 * (size * lh); // line 3 -> line0 = 2
    app.pending_scroll = None;
    app.apply_cli_jump(Some((3, None)));
    assert_eq!(app.pending_scroll, Some(expected));
    assert!(
        app.status.contains("go to line 3"),
        "status: {:?}",
        app.status
    );
}

#[test]
fn apply_cli_jump_none_is_a_noop() {
    // Reverting the wire (jump discarded, as it was before this fix) must leave
    // `pending_scroll` untouched — this is the negative that goes red if the
    // change is reverted at the discard site.
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs[0].set_text("a\nb\nc\n".into());
    app.pending_scroll = None;
    app.apply_cli_jump(None);
    assert_eq!(app.pending_scroll, None, "no jump target -> no scroll");
}

#[test]
fn apply_cli_jump_line_zero_is_ignored() {
    // `file:0` is not a real 1-based position; it must not scroll.
    let mut app = ScribeApp::new_test(Config::default());
    app.tabs[0].set_text("a\nb\n".into());
    app.pending_scroll = None;
    app.apply_cli_jump(Some((0, None)));
    assert_eq!(
        app.pending_scroll, None,
        "line 0 is not a valid jump target"
    );
}
