//! Behavioural tests for the custom-titlebar chrome (`app/chrome.rs`).
//!
//! `resize_tests.rs` already pins the frameless-resize hit-testing. This file
//! covers the rest of the module: the win32 Snap-Layouts wiring (publish the
//! maximize-button rect, retract it when the titlebar goes away), the one-shot
//! chrome policy, the titlebar right-click → system-menu decision, and the glyph
//! controls.
//!
//! Two of those surfaces have no in-process reader — `scribe-win32-chrome`'s
//! setters are `#[cfg(windows)]` and no-ops elsewhere, and the mutation/CI
//! runner is Linux — so the module records what it HANDED the crate in
//! `TEST_PUBLISHED_MAX_RECT` / `TEST_APPLIED_CHROME_POLICY`, inside the single
//! publish helper rather than at the call site. Asserting those recorded VALUES
//! after driving the app's real titlebar render path is what makes these wiring
//! tests rather than "the helper I just called did what I called it with".

use super::chrome::{
    glyph_is_hovered, system_menu_point, MaximizeRectRetractor, TEST_APPLIED_CHROME_POLICY,
    TEST_PUBLISHED_MAX_RECT,
};
use super::ScribeApp;
use egui::{pos2, vec2, Rect};
use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use scribe_core::Config;

// ───────────────────────── the titlebar render path ─────────────────────────

/// Serialises every test in THIS file that runs a titlebar pass.
///
/// `chrome.rs`'s own pass latches are per-`egui::Context` now, so they no longer
/// interleave between tests. What remains process-global is the layer BELOW
/// them: `scribe-win32-chrome`'s published maximize-button rect and
/// `apply_chrome_policy`'s `Once` are real process-wide state on Windows. Two
/// titlebar passes racing each other still both drive that one crate, so the
/// tests that assert what was handed to it stay serialised.
///
/// This lock cannot — and no longer has to — cover `e2e.rs` / `e2e_overlays.rs`,
/// which render frameless titlebars of their own without taking it. That is
/// exactly why the pass latches had to stop being process-global rather than be
/// wrapped in a wider lock: a lock only one file honours is not a lock.
static CHROME_GLOBALS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Take [`CHROME_GLOBALS_LOCK`] and clear this thread's publish recorder, so a
/// test starts from a known point rather than from whatever a sibling left
/// behind.
///
/// Poison-tolerant: one failing test must not cascade into every other test in
/// the file reporting a poisoned-mutex panic instead of its own result.
fn chrome_globals_guard() -> std::sync::MutexGuard<'static, ()> {
    let g = CHROME_GLOBALS_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    TEST_PUBLISHED_MAX_RECT.with(|c| c.set(None));
    g
}

/// An app with the CUSTOM (frameless) titlebar — the only mode that lays out the
/// painted caption buttons, and therefore the only mode that publishes the
/// maximize-button rect.
fn frameless_app() -> ScribeApp {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    cfg.appearance.frameless = true;
    ScribeApp::new_test(cfg)
}

/// Build a titlebar harness, holding [`CHROME_GLOBALS_LOCK`] for as long as the
/// caller keeps the returned guard.
///
/// The guard is returned FROM here rather than taken separately by each test on
/// purpose: running a pass is what touches the process-global latches, so every
/// caller of this function needs it — and when it was a separate call, one test
/// (`a_glyph_control_returns_a_real_click_response`) simply did not make it.
/// That single omission was enough to turn a plain `cargo test` red with a
/// ROTATING victim, because its unguarded pass stored its own pass number and
/// the guarded test's end-of-pass retractor then overwrote that test's rect.
///
/// Returning the guard makes the omission impossible: you cannot obtain a
/// harness without also obtaining the lock.
fn harness(
    app: ScribeApp,
) -> (
    std::sync::MutexGuard<'static, ()>,
    Harness<'static, ScribeApp>,
) {
    let guard = chrome_globals_guard();
    let h = Harness::builder()
        .with_size(vec2(1100.0, 760.0))
        .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app);
    (guard, h)
}

fn published() -> Option<scribe_win32_chrome::RectPx> {
    TEST_PUBLISHED_MAX_RECT.with(std::cell::Cell::get)
}

/// Laying out the titlebar publishes the maximize/restore button's rect to
/// `scribe-win32-chrome` in PHYSICAL client pixels.
///
/// That publish is the ONLY trigger for the Windows 11 Snap Layouts flyout: with
/// the wire cut, `WM_NCHITTEST` never answers `HTMAXBUTTON` and the flyout
/// silently never appears — a defect invisible to every other test in the tree.
#[test]
fn the_titlebar_publishes_the_maximize_button_rect() {
    TEST_PUBLISHED_MAX_RECT.with(|c| c.set(None));
    let (_chrome, mut h) = harness(frameless_app());
    h.run();

    let px = published().expect("the titlebar render path must publish a rect");
    assert!(
        px.right > px.left && px.bottom > px.top,
        "a real, non-empty button rect is published (an empty one reads as \
         'no button' to the crate): {px:?}"
    );
    // The standard Windows caption-button width is 46 logical px; at the
    // harness's 1.0 px-per-point that is 46 physical px.
    assert_eq!(
        px.right - px.left,
        46,
        "the published width is the caption button's own width, not the whole \
         titlebar or some other widget's rect"
    );
    assert!(
        px.top >= 0 && px.left >= 0,
        "coordinates are CLIENT-space (origin at the client top-left): {px:?}"
    );
}

/// When the titlebar stops laying the button out, the published rect is
/// RETRACTED.
///
/// Without the retraction a stale rect keeps claiming `HTMAXBUTTON` over a
/// region with no button, and Windows routes pointer events there away from
/// egui — so clicks in that area go nowhere. Fullscreen / non-frameless / zen are
/// exactly the states where no `chrome.rs` code runs at all, which is why the
/// retraction lives in an end-of-pass plugin rather than in the button layout.
#[test]
fn the_published_rect_is_retracted_once_the_titlebar_stops_rendering() {
    TEST_PUBLISHED_MAX_RECT.with(|c| c.set(None));
    let (_chrome, mut h) = harness(frameless_app());
    h.run();
    let live = published().expect("published while the titlebar renders");
    assert!(
        live.right > live.left,
        "precondition: a live rect: {live:?}"
    );

    // Take the titlebar away, then run further passes.
    h.state_mut().config.appearance.frameless = false;
    h.run();
    h.run();

    assert_eq!(
        published(),
        Some(scribe_win32_chrome::RectPx::new(0, 0, 0, 0)),
        "with no titlebar the rect must be retracted to empty, not left claiming \
         a region that no longer has a button"
    );
}

/// A SECOND live `egui::Context` cannot answer for this one's retraction.
///
/// This is the race that made the two harness-driven tests above fail ~4 runs in
/// 10 under `--test-threads=32`, reproduced DETERMINISTICALLY on one thread.
///
/// The retraction decision reads "the pass in which the titlebar last published"
/// and compares it to "the pass we are ending now". While that latch was a
/// process-global `AtomicU64` those two numbers could come from DIFFERENT
/// contexts, because `cumulative_pass_nr` is per-`Context` and a test binary
/// runs many (`e2e.rs`, `e2e_overlays.rs`, and this file all render frameless
/// titlebars). Three ways it went wrong, all observed at HEAD:
///
///   * sibling pass == our pass  -> read as `PublishedThisPass`, so a retraction
///     that was DUE never happened (this test's shape);
///   * sibling pass != our pass  -> read as `RetractNow`, so the retractor
///     retracted a rect that WAS on screen, between our own publish and our own
///     end-of-pass (`the_titlebar_publishes_the_maximize_button_rect`'s shape);
///   * sibling had just retracted -> `u64::MAX` read as `AlreadyLatched`, same
///     outcome as the first.
///
/// Here the sibling context is stepped one pass AHEAD, so its pass number equals
/// the victim's next one — the collision — and the victim is then asked to
/// retract. With the latch scoped to its own context the sibling is invisible to
/// it. With the latch process-global this fails on every run, not 2 in 10.
#[test]
fn a_sibling_context_cannot_answer_this_contexts_retraction() {
    let (_chrome, mut victim) = harness(frameless_app());
    // A second, independent context+app on this same thread — the single-thread
    // stand-in for the concurrent `e2e.rs` harness that caused the real flake.
    let mut sibling = Harness::builder()
        .with_size(vec2(1100.0, 760.0))
        .build_state(
            |ctx, app: &mut ScribeApp| app.frame_tick(ctx),
            frameless_app(),
        );

    victim.step();
    let live = published().expect("precondition: the victim's titlebar published");
    assert!(
        live.right > live.left,
        "precondition: a live rect: {live:?}"
    );

    // Step the sibling's own frameless titlebar until it sits exactly ONE pass
    // ahead of the victim: its publish then stamps the pass number the victim is
    // about to end on.
    let mut guard = 0;
    while sibling.ctx.cumulative_pass_nr() <= victim.ctx.cumulative_pass_nr() {
        sibling.step();
        guard += 1;
        assert!(guard < 64, "the sibling context never advanced its passes");
    }
    assert_eq!(
        sibling.ctx.cumulative_pass_nr(),
        victim.ctx.cumulative_pass_nr() + 1,
        "the collision this pins needs the sibling exactly one pass ahead"
    );

    // Take the victim's titlebar away and end its next pass — the one whose
    // number the sibling just stamped.
    victim.state_mut().config.appearance.frameless = false;
    victim.step();

    assert_eq!(
        published(),
        Some(scribe_win32_chrome::RectPx::new(0, 0, 0, 0)),
        "a sibling context's pass number must not read as THIS context's \
         publish — the rect has to retract"
    );
}

/// The retraction latches: once retracted it is not re-stored on every
/// subsequent idle pass.
///
/// The end-of-pass plugin runs on EVERY pass, including the thousands where
/// nothing changed; re-publishing an empty rect each time would be a per-frame
/// cross-crate store for no reason.
///
/// Asserted over the PURE decision rather than by idling a live harness: an
/// idling harness can only ever reach ONE of the three branches, and only after
/// however many passes it takes to get there. Over `(last_pass, pass)` the
/// decision is total, so all THREE branches get pinned here.
///
/// (This form was originally forced by the latch being a process-global that
/// `e2e.rs` / `e2e_overlays.rs` could un-latch mid-assertion. The latch is now
/// per-`Context`, so that pressure is gone —
/// `a_sibling_context_cannot_answer_this_contexts_retraction` pins the absence
/// of the interference directly — but the three-branch coverage is worth more
/// than the one-branch harness form either way, so it stays.)
#[test]
fn the_retraction_happens_once_and_then_latches() {
    use super::chrome::{classify_end_pass, EndPassAction};

    // Published on this very pass → leave the live rect alone.
    assert_eq!(
        classify_end_pass(7, 7),
        EndPassAction::PublishedThisPass,
        "a pass that published must not be retracted out from under itself"
    );

    // First pass after the titlebar went away → retract, exactly once.
    assert_eq!(
        classify_end_pass(7, 8),
        EndPassAction::RetractNow,
        "the first non-publishing pass is what retracts the stale rect"
    );

    // Already latched → every later idle pass is a no-op. This is the branch the
    // live harness could never hold still long enough to observe.
    assert_eq!(
        classify_end_pass(u64::MAX, 9),
        EndPassAction::AlreadyLatched,
        "an idle pass after the retraction must not re-store the empty rect"
    );
    assert_eq!(
        classify_end_pass(u64::MAX, 10_000),
        EndPassAction::AlreadyLatched,
        "…and it must still be a no-op thousands of passes later"
    );
}

/// The RETRACTION ITSELF, through the live plugin.
///
/// The pure test above pins the decision; this pins that the decision is
/// actually wired to the app's end-of-pass. Both halves are needed: a pure test
/// alone would pass with the plugin unregistered.
#[test]
fn the_titlebar_going_away_retracts_through_the_live_plugin() {
    TEST_PUBLISHED_MAX_RECT.with(|c| c.set(None));
    let (_chrome, mut h) = harness(frameless_app());
    h.run();
    h.state_mut().config.appearance.frameless = false;
    // TWO passes: the retractor is an END-of-pass plugin, so the pass that first
    // sees `frameless == false` is already laid out — the retraction lands on the
    // pass after it.
    h.run();
    h.run();
    assert_eq!(
        published(),
        Some(scribe_win32_chrome::RectPx::new(0, 0, 0, 0)),
        "retracted after the titlebar went away"
    );
}

/// The titlebar render path applies the one-shot `scribe-win32-chrome` policy,
/// with the three deliberate values documented on `apply_chrome_policy`.
///
/// The crate exposes no reader for these (the setters are Windows-only), so the
/// values are asserted through the record the helper itself makes. Each one is a
/// real decision: `FullNonClient` would break the app's own egui-space resize,
/// dropping snap support would disable Aero Snap / Win+arrow / the flyout, and a
/// DWM backdrop is what previously re-admitted the doubled native caption
/// buttons over the custom titlebar.
#[test]
fn the_titlebar_applies_the_chrome_policy() {
    TEST_APPLIED_CHROME_POLICY.with(|c| c.set(None));
    let (_chrome, mut h) = harness(frameless_app());
    h.run();

    assert_eq!(
        TEST_APPLIED_CHROME_POLICY.with(std::cell::Cell::get),
        Some((
            scribe_win32_chrome::HitTestMode::MaximizeButtonOnly,
            true,
            scribe_win32_chrome::Backdrop::None,
        )),
        "the app claims ONLY the maximize-button rect, keeps the snap-gating \
         style bits, and requests no DWM backdrop"
    );
}

/// The end-of-pass plugin is registered under a stable, namespaced debug name —
/// egui identifies plugins by it in its own debug/inspection output, and a bare
/// or empty name is unattributable when a plugin misbehaves.
#[test]
fn the_retractor_plugin_has_a_namespaced_debug_name() {
    use egui::Plugin as _;
    let name = MaximizeRectRetractor.debug_name();
    assert_eq!(name, "scr1b3::MaximizeRectRetractor");
}

// ───────────────── titlebar right-click → native system menu ────────────────

/// The titlebar band (y 0..32) of a window whose client origin sits at logical
/// screen point (100, 50).
fn band() -> Rect {
    Rect::from_min_max(pos2(0.0, 0.0), pos2(1000.0, 32.0))
}
fn inner() -> Rect {
    Rect::from_min_max(pos2(100.0, 50.0), pos2(1100.0, 810.0))
}

/// A right-click in the titlebar maps to the SCREEN point Win32 wants: the
/// viewport's logical origin plus the client-space pointer position, scaled to
/// physical pixels.
///
/// The conversion is the whole feature — an arithmetic slip pops the menu at the
/// wrong place on the desktop, which nothing else in the tree would catch. The
/// non-uniform scale (1.5) and the non-zero origin are what make each operand
/// load-bearing: with a 1.0 scale or a (0,0) origin the wrong operators agree
/// with the right one.
#[test]
fn a_titlebar_right_click_maps_to_its_screen_pixel() {
    // (100 + 200, 50 + 16) * 1.5 = (450, 99)
    assert_eq!(
        system_menu_point(pos2(200.0, 16.0), band(), None, inner(), 1.5),
        Some((450, 99)),
        "screen = (viewport origin + client pointer) * pixels_per_point"
    );
    // A different scale must move the answer — the scale is not decorative.
    assert_eq!(
        system_menu_point(pos2(200.0, 16.0), band(), None, inner(), 1.0),
        Some((300, 66))
    );
    // Rounding, not truncation: (100 + 0.7) * 1.0 = 100.7 → 101.
    assert_eq!(
        system_menu_point(pos2(0.7, 16.4), band(), None, inner(), 1.0),
        Some((101, 66)),
        "the point is ROUNDED to the pixel the user clicked, not truncated"
    );
}

/// A click outside the recorded titlebar band is not a titlebar right-click and
/// must pop nothing — otherwise a right-click anywhere in the editor would open
/// the window system menu.
#[test]
fn a_click_below_the_titlebar_band_pops_nothing() {
    assert_eq!(
        system_menu_point(pos2(200.0, 33.0), band(), None, inner(), 1.0),
        None,
        "one pixel below the band is outside it"
    );
    assert_eq!(
        system_menu_point(pos2(200.0, 400.0), band(), None, inner(), 1.0),
        None,
        "a right-click in the editor body must not pop the window menu"
    );
    // The band's own bottom edge IS part of the titlebar.
    assert!(
        system_menu_point(pos2(200.0, 32.0), band(), None, inner(), 1.0).is_some(),
        "the band is inclusive of its edge — a click ON it is still the titlebar"
    );
}

/// A right-click ON a caption button pops nothing — the same as on a native
/// window. The exclusion must apply only INSIDE the button union, not to the
/// whole titlebar.
#[test]
fn a_right_click_on_a_caption_button_pops_nothing() {
    let union = Some(Rect::from_min_max(pos2(862.0, 0.0), pos2(1000.0, 32.0)));
    assert_eq!(
        system_menu_point(pos2(900.0, 16.0), band(), union, inner(), 1.0),
        None,
        "a right-click on the min/max/close strip pops nothing"
    );
    assert_eq!(
        system_menu_point(pos2(200.0, 16.0), band(), union, inner(), 1.0),
        Some((300, 66)),
        "the rest of the titlebar still pops the menu when a union is recorded"
    );
    assert_eq!(
        system_menu_point(pos2(900.0, 16.0), band(), None, inner(), 1.0),
        Some((1000, 66)),
        "with no caption buttons recorded, that same point is ordinary titlebar"
    );
}

// ─────────────────────────── the glyph controls ─────────────────────────────

/// A tab's pin / close ✕ lights up from EITHER hover signal.
///
/// The raw pointer-in-rect test is what keeps the veil lit while the adjacent
/// tab chip holds the pointer grab mid-drag — in that state the control's own
/// `Response::hovered()` is false, and requiring both signals is exactly the
/// regression that made these controls read dead under the cursor.
#[test]
fn a_glyph_control_lights_from_either_hover_signal() {
    assert!(glyph_is_hovered(true, true));
    assert!(
        glyph_is_hovered(true, false),
        "the sensed response alone lights it"
    );
    assert!(
        glyph_is_hovered(false, true),
        "a raw pointer-in-rect alone lights it — the drag-grab case"
    );
    assert!(
        !glyph_is_hovered(false, false),
        "with neither signal the control stays at rest"
    );
}

/// A glyph control allocates a hit target at least as large as the WCAG 2.5.8
/// minimum (24×24), and the click `Response` it returns is REAL — clicking a
/// tab's ✕ by its accessible glyph closes that tab.
///
/// The control is painted (not an `egui::Button`), so both the interactive rect
/// and the accessible name are explicit. If the returned response stopped being
/// the sensed one, the ✕ would still PAINT and simply do nothing on click.
#[test]
fn a_glyph_control_returns_a_real_click_response() {
    let dir = tempfile::tempdir().unwrap();
    let alpha = dir.path().join("alpha.txt");
    let beta = dir.path().join("beta.txt");
    std::fs::write(&alpha, "A\n").unwrap();
    std::fs::write(&beta, "B\n").unwrap();

    let mut app = frameless_app();
    app.open_path(alpha);
    app.open_path(beta); // beta is active
                         // Pin every OTHER tab so exactly one ✕ renders and the
                         // by-glyph query is unambiguous.
    let last = app.tabs.len() - 1;
    for (i, t) in app.tabs.iter_mut().enumerate() {
        t.pinned = i != last;
    }

    let (_chrome, mut h) = harness(app);
    h.run();
    let before = h.state().tabs.len();
    h.get_by_label(egui_phosphor::thin::X).click();
    h.run();

    assert_eq!(
        h.state().tabs.len(),
        before - 1,
        "clicking a tab's ✕ closes exactly that tab"
    );
    assert!(
        !h.state()
            .tabs
            .iter()
            .any(|t| t.doc.path().is_some_and(|p| p.ends_with("beta.txt"))),
        "the closed tab is the one whose ✕ was clicked"
    );
}
