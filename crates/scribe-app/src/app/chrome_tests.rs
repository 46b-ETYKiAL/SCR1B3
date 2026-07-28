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

use super::ScribeApp;
use super::chrome::{
    MaximizeRectRetractor, TEST_APPLIED_CHROME_POLICY, TEST_PUBLISHED_MAX_RECT, glyph_is_hovered,
    system_menu_point,
};
use egui::{Rect, pos2, vec2};
use egui_kittest::kittest::Queryable as _;
use scribe_core::Config;

// ───────────────────────── the titlebar render path ─────────────────────────

/// An app with the CUSTOM (frameless) titlebar — the only mode that lays out the
/// painted caption buttons, and therefore the only mode that publishes the
/// maximize-button rect.
fn frameless_app() -> ScribeApp {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    cfg.appearance.frameless = true;
    ScribeApp::new_test(cfg)
}

fn harness(app: ScribeApp) -> egui_kittest::Harness<'static, ScribeApp> {
    egui_kittest::Harness::builder()
        .with_size(vec2(1100.0, 760.0))
        .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app)
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
    let mut h = harness(frameless_app());
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
    let mut h = harness(frameless_app());
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

/// The retraction latches: once retracted it is not re-stored on every
/// subsequent idle pass.
///
/// The end-of-pass plugin runs on EVERY pass, including the thousands where
/// nothing changed; re-publishing an empty rect each time would be a per-frame
/// cross-crate store for no reason.
#[test]
fn the_retraction_happens_once_and_then_latches() {
    let mut h = harness(frameless_app());
    h.run();
    h.state_mut().config.appearance.frameless = false;
    h.run();
    assert_eq!(
        published(),
        Some(scribe_win32_chrome::RectPx::new(0, 0, 0, 0)),
        "retracted after the titlebar went away"
    );

    // Clear the hook and idle: a latched retractor must not touch it again.
    TEST_PUBLISHED_MAX_RECT.with(|c| c.set(None));
    h.run();
    h.run();
    assert_eq!(
        published(),
        None,
        "the retraction is latched — idle passes must not keep re-storing it"
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
    let mut h = harness(frameless_app());
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

    let mut h = harness(app);
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
