//! PURE, platform-independent `WM_NCHITTEST` classification.
//!
//! Deliberately contains **no** `unsafe` and **no** Win32 imports so it compiles
//! (and its tests run) on every host, exactly like the `maximized_client_rect`
//! decision function in the crate root. The `unsafe` glue that reads the real
//! window state and calls `DwmDefWindowProc` lives in `lib.rs::imp`; all it does
//! is gather geometry, call [`classify`], and return [`HitZone::ht_code`].
//!
//! ## Why this exists
//!
//! Windows 11 shows the **Snap Layouts** flyout when the pointer rests over a
//! window region whose `WM_NCHITTEST` answer is `HTMAXBUTTON` (9) — that reply is
//! the *only* trigger. A frameless app that never answers `WM_NCHITTEST` (the
//! whole window is `HTCLIENT` by default) can therefore never show the flyout, no
//! matter how its maximize button is painted.
//!
//! ## Ownership of drag + resize (deliberate, documented)
//!
//! SCR1B3 already owns titlebar drag (`ViewportCommand::StartDrag`) and the 8/12px
//! edge-resize bands (`scribe-app/src/app/chrome.rs::resize_dir_at` +
//! `handle_frameless_resize`) in **egui space**, both unit-tested. Two systems
//! answering "is this pointer on a resize edge?" is a genuine bug source (the OS
//! modal resize loop swallows the button-up that egui's state machine is waiting
//! for), so this crate ships [`HitTestMode::MaximizeButtonOnly`] as the default:
//! it claims **only** the maximize-button rect and answers `HTCLIENT` everywhere
//! else, leaving drag and resize entirely with egui. Nothing in the app has to be
//! disabled.
//!
//! [`HitTestMode::FullNonClient`] implements the other half of the matrix (caption
//! strip + 8 resize zones) for ports that want Win32 to own drag/resize instead —
//! see the port note in the crate root. An app that selects it MUST disable its
//! own egui-space resize handler.

/// An inclusive-left / exclusive-right rectangle in **physical** pixels, in
/// client coordinates (origin = the window's top-left client pixel).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RectPx {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl RectPx {
    #[must_use]
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    /// Whether `(x, y)` falls inside the rect (left/top inclusive, right/bottom
    /// exclusive — the standard Win32 `RECT` convention).
    #[must_use]
    pub const fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }

    /// A rect with no area is never hit (guards a stale/zeroed published rect).
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.right <= self.left || self.bottom <= self.top
    }
}

/// Convert a **logical** (egui-space) rect to physical client pixels at the
/// window's current DPI scale factor. egui reports rects in points; Win32
/// hit-testing is in physical device pixels, so an app on a 150% display that
/// publishes raw logical coordinates would claim a rect a third of the size in
/// the wrong place. Rounded (not truncated) so a 1.25/1.5 scale lands on the same
/// pixel the renderer painted.
///
/// Returns an empty rect for a non-finite or non-positive scale rather than
/// producing garbage coordinates — an empty rect is never hit, so the worst case
/// is "the flyout does not trigger", never a mis-claimed region.
#[must_use]
pub fn logical_rect_to_physical(
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
    scale: f32,
) -> RectPx {
    if !scale.is_finite() || scale <= 0.0 {
        return RectPx::default();
    }
    let px = |v: f32| -> i32 {
        if v.is_finite() {
            (v * scale).round() as i32
        } else {
            0
        }
    };
    RectPx::new(px(left), px(top), px(right), px(bottom))
}

/// The window region a pointer position falls in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitZone {
    /// Ordinary client area — egui handles the event.
    Client,
    /// The app's maximize/restore button. Answering this is what makes Windows 11
    /// show the Snap Layouts flyout.
    MaxButton,
    /// Draggable titlebar strip (only produced in [`HitTestMode::FullNonClient`]).
    Caption,
    Left,
    Right,
    Top,
    Bottom,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl HitZone {
    /// The `WM_NCHITTEST` return code for this zone.
    ///
    /// The literals are the stable Win32 `HT*` values (winuser.h). They are
    /// spelled out rather than imported so this module stays platform-independent;
    /// a windows-only test in `lib.rs` asserts each one equals the corresponding
    /// `windows_sys` constant, so a drift can never go unnoticed.
    #[must_use]
    pub const fn ht_code(self) -> isize {
        match self {
            Self::Client => 1,       // HTCLIENT
            Self::Caption => 2,      // HTCAPTION
            Self::MaxButton => 9,    // HTMAXBUTTON
            Self::Left => 10,        // HTLEFT
            Self::Right => 11,       // HTRIGHT
            Self::Top => 12,         // HTTOP
            Self::TopLeft => 13,     // HTTOPLEFT
            Self::TopRight => 14,    // HTTOPRIGHT
            Self::Bottom => 15,      // HTBOTTOM
            Self::BottomLeft => 16,  // HTBOTTOMLEFT
            Self::BottomRight => 17, // HTBOTTOMRIGHT
        }
    }
}

/// Which regions this crate claims from `WM_NCHITTEST`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HitTestMode {
    /// **Default.** Claim only the maximize-button rect (for Snap Layouts);
    /// everything else is `HTCLIENT` so the app's egui-space drag/resize keeps
    /// working untouched.
    #[default]
    MaximizeButtonOnly,
    /// Claim the caption strip and all 8 resize zones as well. The app MUST then
    /// disable its own egui-space resize/drag handling.
    FullNonClient,
}

/// The geometry inputs a hit-test decision needs. All fields are **physical**
/// pixels in client coordinates.
#[derive(Debug, Clone, Copy)]
pub struct HitTestGeometry {
    /// The window's client rect (`GetClientRect`, so origin is always 0,0).
    pub client: RectPx,
    /// The app's maximize/restore button, published via
    /// `set_maximize_button_rect`. `None` (or empty) → the button is never hit.
    pub max_button: Option<RectPx>,
    /// Height of the draggable titlebar strip (`FullNonClient` only).
    pub caption_height: i32,
    /// Width of the straight edge resize bands.
    pub border: i32,
    /// Side length of the corner resize squares (>= `border`).
    pub corner: i32,
    /// Whether the window is currently maximized. A maximized window exposes no
    /// resize zones (Windows does not resize a maximized window by its edges).
    pub maximized: bool,
}

impl Default for HitTestGeometry {
    fn default() -> Self {
        Self {
            client: RectPx::default(),
            max_button: None,
            caption_height: 0,
            border: 8,
            corner: 12,
            maximized: false,
        }
    }
}

/// Classify a pointer position (physical client coordinates) into a [`HitZone`].
///
/// Precedence, outermost first:
///
/// 1. **Outside the client rect** → `Client` (let `DefWindowProc`/egui deal with
///    it; we never claim a point we cannot reason about).
/// 2. **Resize band** (skipped when maximized). In `MaximizeButtonOnly` this
///    yields `Client` so egui's own bands keep priority — which also means the
///    top `border` pixels of the maximize button stay grabbable as a top-edge
///    resize, matching the app's existing behaviour.
/// 3. **Maximize button** → `MaxButton`.
/// 4. **Caption strip** (`FullNonClient` only) → `Caption`.
/// 5. Otherwise → `Client`.
#[must_use]
pub fn classify(mode: HitTestMode, geo: &HitTestGeometry, x: i32, y: i32) -> HitZone {
    if geo.client.is_empty() || !geo.client.contains(x, y) {
        return HitZone::Client;
    }

    // --- 2. resize bands -----------------------------------------------------
    if !geo.maximized {
        if let Some(zone) = resize_zone(geo, x, y) {
            return match mode {
                // egui owns resize: hand the point back as client area.
                HitTestMode::MaximizeButtonOnly => HitZone::Client,
                HitTestMode::FullNonClient => zone,
            };
        }
    }

    // --- 3. maximize button --------------------------------------------------
    if let Some(btn) = geo.max_button {
        if !btn.is_empty() && btn.contains(x, y) {
            return HitZone::MaxButton;
        }
    }

    // --- 4. caption strip ----------------------------------------------------
    if mode == HitTestMode::FullNonClient
        && geo.caption_height > 0
        && y < geo.client.top + geo.caption_height
    {
        return HitZone::Caption;
    }

    HitZone::Client
}

/// Which of the 8 resize zones (if any) `(x, y)` is in. Corners win over edges.
/// Pure helper for [`classify`]; separated so the corner-priority rule is
/// testable on its own.
#[must_use]
fn resize_zone(geo: &HitTestGeometry, x: i32, y: i32) -> Option<HitZone> {
    let border = geo.border.max(0);
    let corner = geo.corner.max(border);
    if border == 0 {
        return None;
    }
    let (l, r, t, b) = (
        x - geo.client.left,
        geo.client.right - 1 - x,
        y - geo.client.top,
        geo.client.bottom - 1 - y,
    );
    let (w, e, n, s) = (l < border, r < border, t < border, b < border);
    let (cw, ce, cn, cs) = (l < corner, r < corner, t < corner, b < corner);
    let zone = if (n && cw) || (w && cn) {
        HitZone::TopLeft
    } else if (n && ce) || (e && cn) {
        HitZone::TopRight
    } else if (s && cw) || (w && cs) {
        HitZone::BottomLeft
    } else if (s && ce) || (e && cs) {
        HitZone::BottomRight
    } else if n {
        HitZone::Top
    } else if s {
        HitZone::Bottom
    } else if w {
        HitZone::Left
    } else if e {
        HitZone::Right
    } else {
        return None;
    };
    Some(zone)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1000x700 window with a 46x32 maximize button at x=908..954 (the
    /// second-from-right caption slot, matching `chrome.rs::caption_btn`'s 46px
    /// width), a 34px caption strip, 8px edges and 12px corners.
    fn geo() -> HitTestGeometry {
        HitTestGeometry {
            client: RectPx::new(0, 0, 1000, 700),
            max_button: Some(RectPx::new(908, 0, 954, 32)),
            caption_height: 34,
            border: 8,
            corner: 12,
            maximized: false,
        }
    }

    #[test]
    fn maximize_button_rect_yields_htmaxbutton() {
        let g = geo();
        // Below the 8px top resize band so the band does not take precedence.
        for (x, y) in [(930, 20), (908, 8), (953, 31), (940, 12)] {
            assert_eq!(
                classify(HitTestMode::MaximizeButtonOnly, &g, x, y),
                HitZone::MaxButton,
                "({x},{y}) must be the maximize button"
            );
            assert_eq!(
                classify(HitTestMode::FullNonClient, &g, x, y),
                HitZone::MaxButton,
                "({x},{y}) must be the maximize button in FullNonClient too"
            );
        }
        assert_eq!(HitZone::MaxButton.ht_code(), 9, "HTMAXBUTTON");
    }

    #[test]
    fn just_outside_the_button_is_not_the_button() {
        let g = geo();
        // right edge is exclusive; one past the bottom; one left of `left`.
        for (x, y) in [(954, 20), (930, 32), (907, 20)] {
            assert_ne!(
                classify(HitTestMode::MaximizeButtonOnly, &g, x, y),
                HitZone::MaxButton,
                "({x},{y}) is outside the button rect"
            );
        }
    }

    #[test]
    fn empty_or_absent_button_never_hits() {
        let mut g = geo();
        g.max_button = None;
        assert_eq!(
            classify(HitTestMode::MaximizeButtonOnly, &g, 930, 20),
            HitZone::Client
        );
        // A zeroed / inverted rect is treated as absent, not as a giant claim.
        g.max_button = Some(RectPx::new(0, 0, 0, 0));
        assert_eq!(
            classify(HitTestMode::MaximizeButtonOnly, &g, 0, 0),
            HitZone::Client
        );
        g.max_button = Some(RectPx::new(500, 500, 100, 100));
        assert_eq!(
            classify(HitTestMode::MaximizeButtonOnly, &g, 300, 300),
            HitZone::Client
        );
    }

    #[test]
    fn caption_strip_only_in_full_non_client_mode() {
        let g = geo();
        let (x, y) = (400, 20); // titlebar, away from buttons and edges
        assert_eq!(
            classify(HitTestMode::MaximizeButtonOnly, &g, x, y),
            HitZone::Client,
            "default mode leaves the titlebar to egui's StartDrag"
        );
        assert_eq!(
            classify(HitTestMode::FullNonClient, &g, x, y),
            HitZone::Caption
        );
        assert_eq!(HitZone::Caption.ht_code(), 2, "HTCAPTION");
    }

    #[test]
    fn all_eight_resize_zones_in_full_non_client_mode() {
        let g = geo();
        let cases: [(i32, i32, HitZone); 8] = [
            (0, 0, HitZone::TopLeft),
            (999, 0, HitZone::TopRight),
            (0, 699, HitZone::BottomLeft),
            (999, 699, HitZone::BottomRight),
            (500, 2, HitZone::Top),
            (500, 697, HitZone::Bottom),
            (2, 350, HitZone::Left),
            (997, 350, HitZone::Right),
        ];
        for (x, y, want) in cases {
            assert_eq!(
                classify(HitTestMode::FullNonClient, &g, x, y),
                want,
                "({x},{y}) → {want:?}"
            );
        }
    }

    #[test]
    fn resize_zones_are_client_in_default_mode() {
        // The whole point of the ownership decision: in the shipped mode every
        // resize zone answers HTCLIENT so egui's `resize_dir_at` keeps the drag.
        let g = geo();
        for (x, y) in [
            (0, 0),
            (999, 0),
            (0, 699),
            (999, 699),
            (500, 2),
            (500, 697),
            (2, 350),
            (997, 350),
        ] {
            assert_eq!(
                classify(HitTestMode::MaximizeButtonOnly, &g, x, y),
                HitZone::Client,
                "({x},{y}) must stay client-owned"
            );
        }
    }

    #[test]
    fn corners_beat_edges() {
        let g = geo();
        // 10px in from the left along the top: inside `corner` (12) but outside
        // `border` (8) horizontally → still the corner, not the top edge.
        assert_eq!(
            classify(HitTestMode::FullNonClient, &g, 10, 3),
            HitZone::TopLeft
        );
        // 20px in: past the corner square → straight top edge.
        assert_eq!(
            classify(HitTestMode::FullNonClient, &g, 20, 3),
            HitZone::Top
        );
    }

    #[test]
    fn maximized_window_has_no_resize_zones() {
        let mut g = geo();
        g.maximized = true;
        for (x, y) in [(0, 0), (999, 699), (500, 1), (1, 350)] {
            let z = classify(HitTestMode::FullNonClient, &g, x, y);
            assert!(
                matches!(z, HitZone::Caption | HitZone::Client),
                "({x},{y}) must not be a resize zone while maximized, got {z:?}"
            );
        }
        // …and the maximize (now "restore") button still hits, so the Snap
        // Layouts flyout is reachable from a maximized window.
        assert_eq!(
            classify(HitTestMode::MaximizeButtonOnly, &g, 930, 4),
            HitZone::MaxButton,
            "while maximized the top band is not a resize zone, so the button \
             owns its full height"
        );
    }

    #[test]
    fn top_band_of_the_button_stays_resizable_when_restored() {
        // Regression guard for the ownership decision: while NOT maximized the
        // top 8px over the button is a resize band, so it answers HTCLIENT and
        // egui's top-edge resize still works over the caption buttons.
        let g = geo();
        assert_eq!(
            classify(HitTestMode::MaximizeButtonOnly, &g, 930, 4),
            HitZone::Client
        );
        assert_eq!(
            classify(HitTestMode::FullNonClient, &g, 930, 4),
            HitZone::Top
        );
    }

    #[test]
    fn points_outside_the_client_rect_are_client() {
        let g = geo();
        for (x, y) in [(-5, 10), (10, -5), (1000, 10), (10, 700)] {
            assert_eq!(
                classify(HitTestMode::MaximizeButtonOnly, &g, x, y),
                HitZone::Client
            );
        }
        // A degenerate client rect never claims anything.
        let mut empty = geo();
        empty.client = RectPx::new(0, 0, 0, 0);
        assert_eq!(
            classify(HitTestMode::FullNonClient, &empty, 0, 0),
            HitZone::Client
        );
    }

    #[test]
    fn dpi_scaling_maps_logical_button_rect_to_physical() {
        // The app publishes the egui-space rect; at 150% the physical rect is
        // 1.5x and the hit test must land on the SCALED coordinates.
        let logical = (605.0_f32, 0.0, 636.0, 21.0);
        let phys = logical_rect_to_physical(logical.0, logical.1, logical.2, logical.3, 1.5);
        assert_eq!(phys, RectPx::new(908, 0, 954, 32)); // matches `geo()`

        let mut g = geo();
        g.max_button = Some(phys);
        assert_eq!(
            classify(HitTestMode::MaximizeButtonOnly, &g, 930, 20),
            HitZone::MaxButton,
            "a 150%-scaled rect must be hit at physical coordinates"
        );
        // The UNSCALED logical rect would be wrong — prove the scaling matters.
        let unscaled = logical_rect_to_physical(logical.0, logical.1, logical.2, logical.3, 1.0);
        assert!(!unscaled.contains(930, 20), "logical px must not match");
    }

    #[test]
    fn dpi_helper_handles_common_scales_and_rejects_garbage() {
        assert_eq!(
            logical_rect_to_physical(0.0, 0.0, 46.0, 32.0, 1.0),
            RectPx::new(0, 0, 46, 32)
        );
        assert_eq!(
            logical_rect_to_physical(0.0, 0.0, 46.0, 32.0, 1.25),
            RectPx::new(0, 0, 58, 40) // 57.5 rounds to 58
        );
        assert_eq!(
            logical_rect_to_physical(0.0, 0.0, 46.0, 32.0, 2.0),
            RectPx::new(0, 0, 92, 64)
        );
        for bad in [0.0_f32, -1.0, f32::NAN, f32::INFINITY] {
            assert!(
                logical_rect_to_physical(0.0, 0.0, 46.0, 32.0, bad).is_empty(),
                "scale {bad} must produce an unhittable rect"
            );
        }
    }

    #[test]
    fn ht_codes_are_the_documented_win32_values() {
        assert_eq!(HitZone::Client.ht_code(), 1);
        assert_eq!(HitZone::Caption.ht_code(), 2);
        assert_eq!(HitZone::MaxButton.ht_code(), 9);
        assert_eq!(HitZone::Left.ht_code(), 10);
        assert_eq!(HitZone::Right.ht_code(), 11);
        assert_eq!(HitZone::Top.ht_code(), 12);
        assert_eq!(HitZone::TopLeft.ht_code(), 13);
        assert_eq!(HitZone::TopRight.ht_code(), 14);
        assert_eq!(HitZone::Bottom.ht_code(), 15);
        assert_eq!(HitZone::BottomLeft.ht_code(), 16);
        assert_eq!(HitZone::BottomRight.ht_code(), 17);
    }

    #[test]
    fn zero_border_disables_resize_zones_entirely() {
        let mut g = geo();
        g.border = 0;
        g.corner = 0;
        assert_eq!(
            classify(HitTestMode::FullNonClient, &g, 0, 0),
            HitZone::Caption,
            "with no border the top-left corner is just caption"
        );
    }

    // ---------------------------------------------------------------------
    // Boundary + arithmetic pinning.
    //
    // The tests above prove the ZONES are right; the ones below prove the
    // ARITHMETIC that produces them is right. Every case here was written to
    // kill a specific surviving mutant reported by `cargo mutants`: each one
    // sat on a line the tests above already executed, but at a coordinate no
    // assertion could tell apart from its neighbour. They are deliberately
    // "one pixel" tests — that is exactly the class of bug an off-by-one in a
    // hit test produces, and the only class a coordinate in the middle of a
    // band can never catch.
    // ---------------------------------------------------------------------

    /// [`geo`] with a NON-ZERO client origin.
    ///
    /// `GetClientRect` always reports 0,0 in production, so every existing case
    /// runs with `client.left == client.top == 0` — and at that origin
    /// `x - client.left` and `x + client.left` are the same number. Moving the
    /// origin is the only thing that pins [`resize_zone`]'s coordinates as
    /// RELATIVE to the client rect, which is the entire reason those
    /// subtractions are there.
    fn offset_geo() -> HitTestGeometry {
        HitTestGeometry {
            client: RectPx::new(100, 50, 1100, 750),
            max_button: None,
            ..geo()
        }
    }

    #[test]
    fn a_rect_degenerate_in_only_one_axis_is_still_empty() {
        // `right <= left || bottom <= top`. Every rect the tests above feed to
        // `is_empty` is degenerate in BOTH axes (0,0,0,0 and the fully inverted
        // 500,500,100,100), and both disjuncts are then true — which cannot tell
        // `||` from `&&`. A rect with area in exactly one axis can.
        assert!(
            RectPx::new(0, 0, 0, 10).is_empty(),
            "zero WIDTH alone makes a rect unhittable"
        );
        assert!(
            RectPx::new(0, 0, 10, 0).is_empty(),
            "zero HEIGHT alone makes a rect unhittable"
        );
        assert!(
            RectPx::new(0, 0, -5, 10).is_empty(),
            "an inverted horizontal pair alone is empty"
        );
        assert!(
            RectPx::new(0, 0, 10, -5).is_empty(),
            "an inverted vertical pair alone is empty"
        );
        assert!(
            !RectPx::new(0, 0, 10, 10).is_empty(),
            "a rect with area in both axes is NOT empty"
        );
    }

    #[test]
    fn a_point_far_outside_a_healthy_client_rect_is_rejected_by_the_guard() {
        // `client.is_empty() || !client.contains(x, y)`. The existing
        // outside-the-rect points ((-5,10), (1000,10), …) all sit within a
        // border's reach of an edge, so even with the guard disabled they land
        // in a resize band that answers Client anyway — identical verdict, dead
        // assertion. A point far outside, in FullNonClient (where a band is NOT
        // rewritten to Client), separates the two: with `&&` the guard stops
        // firing and the point is classified as a resize corner.
        let g = geo();
        assert_eq!(
            classify(HitTestMode::FullNonClient, &g, 1500, 1500),
            HitZone::Client,
            "far past the bottom-right corner is not a resize corner"
        );
        assert_eq!(
            classify(HitTestMode::FullNonClient, &g, -500, -500),
            HitZone::Client,
            "far past the top-left corner is not a resize corner"
        );
    }

    #[test]
    fn the_caption_strip_ends_exactly_at_caption_height() {
        // `y < client.top + caption_height` — exclusive. `geo()`'s strip is
        // 34px tall at top 0, so row 33 is the last caption row and row 34 is
        // the first client row. `<=` would steal one row from the editor.
        let g = geo();
        assert_eq!(
            classify(HitTestMode::FullNonClient, &g, 400, 33),
            HitZone::Caption,
            "y = caption_height - 1 is the LAST caption row"
        );
        assert_eq!(
            classify(HitTestMode::FullNonClient, &g, 400, 34),
            HitZone::Client,
            "y = caption_height is the FIRST client row"
        );
    }

    #[test]
    fn resize_bands_are_measured_relative_to_the_client_origin() {
        // client = (100,50)..(1100,750). Each point below is 4px inside an edge
        // OF THAT RECT. Adding the origin instead of subtracting it puts every
        // one of them ~100-200px inside the window — nowhere near a band — so
        // the whole window would stop being resizable on an offset client rect.
        let g = offset_geo();
        assert_eq!(
            classify(HitTestMode::FullNonClient, &g, 104, 350),
            HitZone::Left,
            "4px inside the offset LEFT edge"
        );
        assert_eq!(
            classify(HitTestMode::FullNonClient, &g, 500, 54),
            HitZone::Top,
            "4px inside the offset TOP edge"
        );
        assert_eq!(
            classify(HitTestMode::FullNonClient, &g, 1095, 350),
            HitZone::Right,
            "4px inside the offset RIGHT edge"
        );
        assert_eq!(
            classify(HitTestMode::FullNonClient, &g, 500, 745),
            HitZone::Bottom,
            "4px inside the offset BOTTOM edge"
        );
    }

    #[test]
    fn the_right_and_bottom_bands_start_at_the_exact_pixel_the_width_implies() {
        // `client.right - 1 - x` is a distance from the LAST pixel (999), not
        // from the exclusive edge (1000). With border 8 the right band is
        // therefore x = 992..=999 and the bottom band y = 692..=699. The
        // existing cases (997, 697) sit two pixels deep, where every off-by-one
        // form of that expression still answers "in the band". 992/692 is the
        // first pixel of each band and 991/691 the last pixel before it, so an
        // off-by-one in either direction moves the answer.
        let g = geo();
        assert_eq!(
            classify(HitTestMode::FullNonClient, &g, 992, 350),
            HitZone::Right,
            "x = 992 is the FIRST pixel of the right band"
        );
        assert_eq!(
            classify(HitTestMode::FullNonClient, &g, 991, 350),
            HitZone::Client,
            "x = 991 is one pixel BEFORE the right band"
        );
        assert_eq!(
            classify(HitTestMode::FullNonClient, &g, 500, 692),
            HitZone::Bottom,
            "y = 692 is the FIRST pixel of the bottom band"
        );
        assert_eq!(
            classify(HitTestMode::FullNonClient, &g, 500, 691),
            HitZone::Client,
            "y = 691 is one pixel BEFORE the bottom band"
        );
    }

    #[test]
    fn the_left_band_ends_exactly_at_border() {
        // `l < border` — exclusive. With border 8 the left band is x = 0..=7.
        let g = geo();
        assert_eq!(
            classify(HitTestMode::FullNonClient, &g, 7, 350),
            HitZone::Left,
            "x = border - 1 is the LAST left-band pixel"
        );
        assert_eq!(
            classify(HitTestMode::FullNonClient, &g, 8, 350),
            HitZone::Client,
            "x = border is already client area"
        );
    }

    #[test]
    fn the_corner_squares_end_exactly_at_corner() {
        // `l < corner` (and its r/t/b siblings) — exclusive. With corner 12 the
        // corner square covers 0..=11 along each axis; at exactly 12 the point
        // is a straight EDGE, not a corner. `corners_beat_edges` brackets this
        // at 10 and 20, which leaves the actual boundary (11 vs 12) untested in
        // all four directions.
        let g = geo();
        assert_eq!(
            classify(HitTestMode::FullNonClient, &g, 11, 3),
            HitZone::TopLeft,
            "l = corner - 1 is still inside the top-left square"
        );
        assert_eq!(
            classify(HitTestMode::FullNonClient, &g, 12, 3),
            HitZone::Top,
            "l = corner is past the top-left square"
        );
        assert_eq!(
            classify(HitTestMode::FullNonClient, &g, 988, 3),
            HitZone::TopRight,
            "r = corner - 1 is still inside the top-right square"
        );
        assert_eq!(
            classify(HitTestMode::FullNonClient, &g, 987, 3),
            HitZone::Top,
            "r = corner is past the top-right square"
        );
        assert_eq!(
            classify(HitTestMode::FullNonClient, &g, 3, 11),
            HitZone::TopLeft,
            "t = corner - 1 is still inside the top-left square"
        );
        assert_eq!(
            classify(HitTestMode::FullNonClient, &g, 3, 12),
            HitZone::Left,
            "t = corner is past the top-left square"
        );
        assert_eq!(
            classify(HitTestMode::FullNonClient, &g, 3, 688),
            HitZone::BottomLeft,
            "b = corner - 1 is still inside the bottom-left square"
        );
        assert_eq!(
            classify(HitTestMode::FullNonClient, &g, 3, 687),
            HitZone::Left,
            "b = corner is past the bottom-left square"
        );
    }

    #[test]
    fn each_corner_is_reachable_from_either_of_its_two_arms() {
        // Each corner is `(edge_a && within_corner_b) || (edge_b &&
        // within_corner_a)` — an L, not a square, and the two arms are NOT
        // interchangeable. Every existing corner case satisfies BOTH arms (the
        // exact corner pixels 0,0 / 999,0 / …), which cannot tell `||` from
        // `&&`. Each point below satisfies exactly ONE arm: it is inside one
        // band and only inside the OTHER axis's corner square, so `&&` demotes
        // it from a corner to a plain edge.
        let g = geo();
        assert_eq!(
            classify(HitTestMode::FullNonClient, &g, 989, 3),
            HitZone::TopRight,
            "on the top band, within the corner square horizontally only"
        );
        assert_eq!(
            classify(HitTestMode::FullNonClient, &g, 3, 689),
            HitZone::BottomLeft,
            "on the left band, within the corner square vertically only"
        );
        assert_eq!(
            classify(HitTestMode::FullNonClient, &g, 989, 694),
            HitZone::BottomRight,
            "on the bottom band, within the corner square horizontally only"
        );
        assert_eq!(
            classify(HitTestMode::FullNonClient, &g, 10, 3),
            HitZone::TopLeft,
            "on the top band, within the corner square horizontally only"
        );
    }
}
