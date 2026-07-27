//! Headless e2e (egui_kittest, no GPU) for the find bar's **regex /
//! match-case / whole-word** toggles and its invalid-regex error line.
//!
//! # Why these are wiring tests, not engine tests
//!
//! `scribe_core::search` already honoured `Query { regex, case_sensitive,
//! whole_word }`; the gap was that the find bar hard-coded
//! `..Default::default()` so the user could never reach two of the three modes,
//! while the README advertised "full regex". A test that BUILDS a `Query` with
//! `case_sensitive: true` and asserts the core respects it proves nothing about
//! the bar — it supplies the very flag under test.
//!
//! So every test here drives the flag through the REAL UI checkbox
//! (`get_by_label("match case").click()`) and asserts the observable app-level
//! outcome moves: the match count the bar renders, or the buffer text after a
//! Replace click. Cut any link in the chain — the checkbox binding, the
//! `find_bar_options_ui` call-site in `frame_tick`, `find_query_flags`, the
//! flag in the find-cache key, or `replace_in_active`'s use of the shared query
//! — and one of these fails.
#![allow(clippy::wildcard_imports)]
use super::*;
use egui_kittest::kittest::Queryable as _;

/// A find-bar harness app: first-run done + non-frameless so no welcome /
/// titlebar chrome competes for the queried labels.
fn find_app(text: &str, query: &str) -> ScribeApp {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    cfg.appearance.frameless = false;
    let mut app = ScribeApp::new_test(cfg);
    app.tabs[0].text = text.to_string();
    app.find_query = query.to_string();
    app.find_open = true;
    app
}

fn find_harness(app: ScribeApp) -> egui_kittest::Harness<'static, ScribeApp> {
    egui_kittest::Harness::builder()
        .with_size(egui::Vec2::new(1100.0, 760.0))
        .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app)
}

// ---------------------------------------------------------------------------
// Match-case
// ---------------------------------------------------------------------------

/// Clicking "match case" must narrow the live match set from case-insensitive
/// to exact-case. The count is read back through `find_matches_active()` — the
/// same call the bar's "{i}/{n}" counter renders from.
#[test]
fn match_case_checkbox_narrows_the_live_match_set() {
    let app = find_app("Foo foo FOO", "foo");
    let mut h = find_harness(app);
    h.run();
    assert_eq!(
        h.state().find_matches_active().len(),
        3,
        "default (case-insensitive) must match all three casings"
    );

    h.get_by_label("match case").click();
    h.run();
    assert!(
        h.state().find_case_sensitive,
        "the match-case checkbox must flip find_case_sensitive"
    );
    assert_eq!(
        h.state().find_matches_active().len(),
        1,
        "with match-case ON only the exact-case `foo` matches — if the flag \
         never reaches the Query (or the find cache hands back the stale \
         case-insensitive memo) this stays 3"
    );
}

/// The find CACHE must not mask a toggle. This is the regression guard for the
/// flags being part of `FindCacheKey`: the first `find_matches_active()` call
/// populates the memo under the old flags, so a key that omitted them would
/// return the stale list forever (the query text and buffer never change here).
#[test]
fn toggling_after_a_warm_cache_still_recomputes() {
    let app = find_app("Foo foo FOO", "foo");
    let mut h = find_harness(app);
    h.run();
    // Warm the memo hard: several idle frames + explicit calls, all under the
    // default flags.
    for _ in 0..3 {
        h.run();
        let _ = h.state().find_matches_active();
    }
    let warm = h.state().find_matches_active().len();
    assert_eq!(warm, 3, "cache warmed on the case-insensitive result");

    h.get_by_label("match case").click();
    h.run();
    assert_eq!(
        h.state().find_matches_active().len(),
        1,
        "a warm find cache must NOT survive a mode toggle"
    );
}

// ---------------------------------------------------------------------------
// Whole-word
// ---------------------------------------------------------------------------

/// Clicking "whole word" must drop substring hits (`alphabet`, `betaalpha`)
/// and keep only the standalone words.
#[test]
fn whole_word_checkbox_drops_substring_matches() {
    let app = find_app("alpha alphabet betaalpha alpha", "alpha");
    let mut h = find_harness(app);
    h.run();
    assert_eq!(
        h.state().find_matches_active().len(),
        4,
        "default (substring) matches inside alphabet/betaalpha too"
    );

    h.get_by_label("whole word").click();
    h.run();
    assert!(
        h.state().find_whole_word,
        "the whole-word checkbox must flip find_whole_word"
    );
    assert_eq!(
        h.state().find_matches_active().len(),
        2,
        "with whole-word ON only the two standalone `alpha` words match"
    );
}

// ---------------------------------------------------------------------------
// Regex
// ---------------------------------------------------------------------------

/// Clicking "regex" must switch the query from a literal to a pattern. The
/// probe string `a|b` is a literal that appears NOWHERE in the buffer, so any
/// match at all can only come from the alternation being compiled as a regex.
#[test]
fn regex_checkbox_switches_literal_to_pattern() {
    let app = find_app("aaa bbb ccc", "a|b");
    let mut h = find_harness(app);
    h.run();
    assert_eq!(
        h.state().find_matches_active().len(),
        0,
        "as a LITERAL, `a|b` occurs nowhere in the buffer"
    );

    h.get_by_label("regex").click();
    h.run();
    assert!(
        h.state().find_regex,
        "the regex checkbox must flip find_regex"
    );
    assert_eq!(
        h.state().find_matches_active().len(),
        6,
        "as a PATTERN, `a|b` matches each of the three a's and three b's"
    );
}

/// The README advertises "full regex and capture-group replacement". This is
/// that claim, driven end-to-end through the UI: turn the regex checkbox on,
/// click "Replace all", and assert the `$2.$1` capture refs expanded.
#[test]
fn regex_replace_all_expands_capture_groups_through_the_ui() {
    let mut app = find_app("a@b c@d", r"(\w+)@(\w+)");
    app.replace_query = "$2.$1".into();
    let mut h = find_harness(app);
    h.run();
    h.get_by_label("regex").click();
    h.run();
    h.get_by_label("Replace all").click();
    h.run();
    let a = h.state().active;
    assert_eq!(
        h.state().tabs[a].text,
        "b.a d.c",
        "with regex ON, Replace all must expand $1/$2 capture refs"
    );
}

/// The inverse guard: with regex OFF, a `$` in the replacement is LITERAL
/// text. Without the `q.regex` gate in `search::replace_n` this silently
/// produced an empty capture expansion and ate the user's `$5.00`.
#[test]
fn literal_replace_does_not_expand_dollar_refs_through_the_ui() {
    let mut app = find_app("total: PRICE", "PRICE");
    app.replace_query = "$5.00".into();
    let mut h = find_harness(app);
    h.run();
    h.get_by_label("Replace all").click();
    h.run();
    let a = h.state().active;
    assert_eq!(
        h.state().tabs[a].text,
        "total: $5.00",
        "with regex OFF the replacement is literal — `$5.00` must survive"
    );
}

/// "Replace next" must honour the toggles too, not just "Replace all": with
/// match-case ON it must skip the wrong-case first occurrence and rewrite the
/// first EXACT-case one.
#[test]
fn replace_next_honours_the_match_case_toggle() {
    let mut app = find_app("FOO foo foo", "foo");
    app.replace_query = "bar".into();
    let mut h = find_harness(app);
    h.run();
    h.get_by_label("match case").click();
    h.run();
    h.get_by_label("Replace next").click();
    h.run();
    let a = h.state().active;
    assert_eq!(
        h.state().tabs[a].text,
        "FOO bar foo",
        "match-case ON must leave the upper-case FOO alone and rewrite the \
         first exact-case match only"
    );
}

// ---------------------------------------------------------------------------
// Invalid regex
// ---------------------------------------------------------------------------

/// A half-typed pattern must surface an inline error — not panic, and not
/// silently degrade to a substring search. The buffer deliberately CONTAINS
/// the literal text `(foo`, so a silent literal fallback would report a match;
/// the assertion that the match set is EMPTY is what rules that out.
#[test]
fn invalid_regex_shows_an_inline_error_and_matches_nothing() {
    let app = find_app("literally (foo here", "(foo");
    let mut h = find_harness(app);
    h.run();
    // Literal mode: `(foo` IS present, so it matches. This is the control that
    // proves the buffer would happily yield a hit under a fallback.
    assert_eq!(
        h.state().find_matches_active().len(),
        1,
        "control: as a literal, `(foo` is present in the buffer"
    );

    h.get_by_label("regex").click();
    h.run();
    assert_eq!(
        h.state().find_matches_active().len(),
        0,
        "an invalid regex must match NOTHING — a non-empty set here means the \
         bar silently fell back to a substring search"
    );
    let err = h
        .state()
        .find_error_text()
        .expect("an invalid regex must latch an inline error, not a bare `no matches`");
    assert!(
        err.starts_with("bad regex"),
        "the inline error must name the cause (got {err:?})"
    );
    // And it is actually rendered on the bar.
    assert!(
        h.query_by_label(err.as_str()).is_some(),
        "the inline error must be RENDERED on the find bar, not just stored"
    );
}

/// Replacing under an invalid regex must leave the buffer untouched and report
/// the error — never a panic, and never a partial rewrite.
#[test]
fn invalid_regex_replace_all_leaves_the_buffer_untouched() {
    let mut app = find_app("literally (foo here", "(foo");
    app.replace_query = "X".into();
    let mut h = find_harness(app);
    h.run();
    h.get_by_label("regex").click();
    h.run();
    h.get_by_label("Replace all").click();
    h.run();
    let a = h.state().active;
    assert_eq!(
        h.state().tabs[a].text,
        "literally (foo here",
        "an invalid regex must not rewrite anything"
    );
    assert!(
        h.state()
            .find_error_text()
            .is_some_and(|e| e.starts_with("bad regex")),
        "the failed replace must report the compile error"
    );
}

/// Fixing the pattern must clear the error line — the error is live state, not
/// a sticky latch that outlives the mistake.
#[test]
fn correcting_the_pattern_clears_the_error() {
    let app = find_app("literally (foo here", "(foo");
    let mut h = find_harness(app);
    h.run();
    h.get_by_label("regex").click();
    h.run();
    assert!(h.state().find_error_text().is_some(), "error latched");

    h.state_mut().find_query = r"\(foo".to_string();
    h.run();
    assert_eq!(
        h.state().find_matches_active().len(),
        1,
        "the escaped pattern matches the literal `(foo`"
    );
    assert!(
        h.state().find_error_text().is_none(),
        "a valid pattern must clear the inline error"
    );
}

// ---------------------------------------------------------------------------
// Find-in-files panel toggles
// ---------------------------------------------------------------------------

/// The project-search panel hard-coded `case_sensitive: false, whole_word:
/// false` beside its live `regex` flag. These tests drive the new checkboxes
/// and assert the RESULT SET moves — i.e. the flags actually reach the `Query`
/// that `run_find_in_files` hands the worker, not merely a bound bool.
fn fif_toggle_app() -> ScribeApp {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    cfg.appearance.frameless = false;
    ScribeApp::new_test(cfg)
}

/// Seed a folder whose file contains BOTH casings and a substring-embedded
/// occurrence, so match-case and whole-word each have something to remove.
fn seed_case_folder() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("mixed.txt"),
        "needle\nNEEDLE\nneedlepoint\n",
    )
    .unwrap();
    dir
}

/// Drive frames until the off-thread worker signals `Done`, wall-clock bounded
/// (same rationale as the `find_in_files_tests` twin: an iteration cap starves
/// silently under CPU contention).
fn run_until_done(h: &mut egui_kittest::Harness<'static, ScribeApp>) {
    const DEADLINE: std::time::Duration = std::time::Duration::from_secs(120);
    let start = std::time::Instant::now();
    while start.elapsed() < DEADLINE {
        h.run_ok();
        if !h.state().find_in_files_running {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[test]
fn find_in_files_match_case_checkbox_narrows_the_result_set() {
    let dir = seed_case_folder();
    let mut app = fif_toggle_app();
    app.open_folder_root(dir.path().to_path_buf());
    app.find_in_files_open = true;
    app.find_in_files_query = "needle".to_string();
    let mut h = egui_kittest::Harness::builder()
        .with_size(egui::Vec2::new(1100.0, 760.0))
        .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app);
    h.run_ok();

    h.get_by_label("search").click();
    run_until_done(&mut h);
    assert_eq!(
        h.state().find_in_files_results.len(),
        3,
        "default (case-insensitive) hits needle, NEEDLE and needlepoint"
    );

    h.get_by_label("match case").click();
    h.run_ok();
    h.get_by_label("search").click();
    run_until_done(&mut h);
    assert_eq!(
        h.state().find_in_files_results.len(),
        2,
        "match-case ON must drop the NEEDLE line — if the checkbox never \
         reaches run_find_in_files's Query this stays 3"
    );
}

#[test]
fn find_in_files_whole_word_checkbox_narrows_the_result_set() {
    let dir = seed_case_folder();
    let mut app = fif_toggle_app();
    app.open_folder_root(dir.path().to_path_buf());
    app.find_in_files_open = true;
    app.find_in_files_query = "needle".to_string();
    let mut h = egui_kittest::Harness::builder()
        .with_size(egui::Vec2::new(1100.0, 760.0))
        .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app);
    h.run_ok();

    h.get_by_label("search").click();
    run_until_done(&mut h);
    assert_eq!(h.state().find_in_files_results.len(), 3, "baseline");

    h.get_by_label("whole word").click();
    h.run_ok();
    h.get_by_label("search").click();
    run_until_done(&mut h);
    assert_eq!(
        h.state().find_in_files_results.len(),
        2,
        "whole-word ON must drop the `needlepoint` line — if the checkbox \
         never reaches run_find_in_files's Query this stays 3"
    );
}
