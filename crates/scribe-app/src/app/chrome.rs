//! Custom-titlebar chrome: caption buttons + frameless-window
//! resize handling. Free functions extracted from the `app`
//! module root; `use super::*` pulls in egui + app-local types.
//!
//! ## Win32 chrome wiring (the `scribe-win32-chrome` call sites)
//!
//! `scribe-win32-chrome` implements `WM_NCHITTEST`, the Snap-Layouts
//! `HTMAXBUTTON` reply, and the native system menu — but every one of those is
//! inert until the app *publishes* the maximize-button rect and *reads back* the
//! OS-side hover. Those call sites live here, in the one module that owns the
//! caption row:
//!
//! * [`caption_btn`] publishes the Maximize/Restore button's rect (converted
//!   from egui logical points to physical CLIENT pixels) every frame it lays the
//!   button out, and ORs `maximize_button_hovered()` into the button's hover
//!   visual. The second half is not cosmetic: once `WM_NCHITTEST` answers
//!   `HTMAXBUTTON` for that rect, Windows routes `WM_NCMOUSEMOVE` there instead
//!   of client-area events, so egui's own `Response::hovered()` is permanently
//!   `false` over the button and it would otherwise look dead.
//! * [`MaximizeRectRetractor`] — an egui plugin registered once per `Context` —
//!   retracts the rect at the end of any pass in which the titlebar did *not*
//!   lay the button out (fullscreen, `frameless` off, zen). Without it a stale
//!   rect would keep claiming `HTMAXBUTTON` over a region with no button, which
//!   would swallow egui pointer events there.
//! * [`caption_btn`] also pops the native system menu on a secondary click in
//!   the titlebar band. With the titlebar answering `HTCLIENT`, Windows never
//!   sees a non-client right-click, so it never pops the menu on its own.
//!
//! The three "policy" knobs (`set_hit_test_mode`, `set_snap_support_enabled`,
//! `set_backdrop`) are applied once from [`apply_chrome_policy`]; see that
//! function for why each value is what it is.
#![allow(clippy::wildcard_imports)]
use super::*;

/// The pointer-position band the titlebar occupied on the most recent frame, in
/// egui logical points, plus the union of the caption buttons laid out inside
/// it. Recorded by [`caption_btn`] so the titlebar right-click -> system-menu
/// check can exclude clicks that landed on a caption button (right-clicking a
/// caption button pops nothing on a native window either).
static TITLEBAR_BAND: std::sync::Mutex<Option<egui::Rect>> = std::sync::Mutex::new(None);
static CAPTION_BTN_UNION: std::sync::Mutex<Option<egui::Rect>> = std::sync::Mutex::new(None);

/// `cumulative_pass_nr` of the pass in which the maximize button last published
/// its rect. `u64::MAX` = never / retracted. Read by [`MaximizeRectRetractor`]
/// to decide whether the titlebar rendered this pass.
static LAST_MAX_RECT_PASS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(u64::MAX);

/// `cumulative_pass_nr` of the pass whose titlebar band + caption-button union
/// are currently recorded, so only the FIRST caption button of a pass resets
/// them. Distinct from [`LAST_MAX_RECT_PASS`] because the maximize button is not
/// the first button drawn.
static LAST_BAND_PASS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(u64::MAX);

#[cfg(test)]
thread_local! {
    /// Test hook: the physical-pixel rect the LAST [`publish_maximize_rect`]
    /// call handed to `scribe_win32_chrome::set_maximize_button_rect`. Recorded
    /// inside that single publish helper (never at the call site) so a test can
    /// assert the app's real titlebar render path drives the win32 layer — a
    /// test that called the win32 helper itself would pass forever even with the
    /// wire cut. `None` = never published this thread; an EMPTY rect = retracted.
    pub(crate) static TEST_PUBLISHED_MAX_RECT:
        std::cell::Cell<Option<scribe_win32_chrome::RectPx>> = const { std::cell::Cell::new(None) };
}

/// Hand the maximize/restore button's rect to `scribe-win32-chrome` so
/// `WM_NCHITTEST` can answer `HTMAXBUTTON` over it (the ONLY trigger for the
/// Windows 11 Snap Layouts flyout).
///
/// `rect` is in egui logical points; the crate wants **physical** pixels in
/// CLIENT coordinates. egui's screen space for the main viewport already has its
/// origin at the client top-left, so the only conversion needed is the DPI
/// scale — `logical_rect_to_physical` does it with rounding (not truncation) so
/// a 1.25/1.5 scale lands on the pixel the renderer actually painted.
fn publish_maximize_rect(rect: egui::Rect, pixels_per_point: f32) {
    let px = scribe_win32_chrome::hit_test::logical_rect_to_physical(
        rect.left(),
        rect.top(),
        rect.right(),
        rect.bottom(),
        pixels_per_point,
    );
    scribe_win32_chrome::set_maximize_button_rect(px.left, px.top, px.right, px.bottom);
    #[cfg(test)]
    TEST_PUBLISHED_MAX_RECT.with(|c| c.set(Some(px)));
}

/// Retract the published rect. See [`publish_maximize_rect`].
fn retract_maximize_rect() {
    scribe_win32_chrome::clear_maximize_button_rect();
    #[cfg(test)]
    TEST_PUBLISHED_MAX_RECT.with(|c| c.set(Some(scribe_win32_chrome::RectPx::new(0, 0, 0, 0))));
}

/// Apply the one-shot `scribe-win32-chrome` policy knobs. Called (idempotently)
/// from [`caption_btn`]; each value is deliberate:
///
/// * **`HitTestMode::MaximizeButtonOnly`** — SCR1B3 owns titlebar drag
///   (`ViewportCommand::StartDrag`) and the 8/12px edge-resize bands
///   ([`resize_dir_at`] / [`handle_frameless_resize`]) in egui space. Letting
///   Win32 *also* answer "is this a resize edge?" is a real bug source (the OS
///   modal resize loop swallows the button-up egui's state machine waits for),
///   so this crate claims ONLY the maximize-button rect. `FullNonClient` would
///   require disabling `handle_frameless_resize`, which is not wanted.
/// * **`set_snap_support_enabled(true)`** — retains
///   `WS_SYSMENU|WS_MINIMIZEBOX|WS_MAXIMIZEBOX`, which Aero Snap, `Win`+arrow,
///   Snap Assist and the Snap Layouts flyout all gate on. This is already the
///   crate default; it is set explicitly so the intent is stated at the call
///   site and the escape hatch is one edit away. Flip to `false` if retaining
///   those bits re-admits the DWM-drawn doubled caption buttons on a
///   transparent window (see the crate docs — that outcome needs a real window
///   on real Win11 and has not been observed either way here).
/// * **`Backdrop::None`** — SCR1B3 has **no** backdrop config key, and
///   `WindowConfig::effective_translucent`'s own docs record that applying a DWM
///   material (Mica/Acrylic/Tabbed) is what previously re-added the native
///   caption buttons over the custom titlebar. Mapping the legacy
///   `WindowMode::Mica` value onto `Backdrop::Mica` would therefore reintroduce
///   a known, already-fixed bug, so the shipped default is asserted explicitly
///   rather than derived.
fn apply_chrome_policy() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        scribe_win32_chrome::set_hit_test_mode(
            scribe_win32_chrome::HitTestMode::MaximizeButtonOnly,
        );
        scribe_win32_chrome::set_snap_support_enabled(true);
        scribe_win32_chrome::set_backdrop(scribe_win32_chrome::Backdrop::None);
    });
}

/// Retracts the published maximize-button rect on any pass where the titlebar
/// did not lay the maximize button out.
///
/// Registered once per [`egui::Context`] (`Context::add_plugin` dedupes by type
/// and ignores repeat registrations), so it runs at the end of EVERY pass —
/// including the passes where the titlebar is gone entirely and therefore no
/// `chrome.rs` code would otherwise run. That is exactly the case the retraction
/// exists for: fullscreen, zen, and `appearance.frameless == false`.
#[derive(Default)]
struct MaximizeRectRetractor;

impl egui::Plugin for MaximizeRectRetractor {
    fn debug_name(&self) -> &'static str {
        "scr1b3::MaximizeRectRetractor"
    }

    fn on_end_pass(&mut self, ui: &mut egui::Ui) {
        use std::sync::atomic::Ordering;
        let pass = ui.ctx().cumulative_pass_nr();
        if LAST_MAX_RECT_PASS.load(Ordering::Relaxed) == pass {
            return; // published this pass — the button is on screen.
        }
        // Not published this pass. Retract once, then latch (u64::MAX) so we do
        // not re-store the same empty rect on every subsequent idle pass.
        if LAST_MAX_RECT_PASS.swap(u64::MAX, Ordering::Relaxed) != u64::MAX {
            retract_maximize_rect();
            *TITLEBAR_BAND.lock().unwrap_or_else(|e| e.into_inner()) = None;
            *CAPTION_BTN_UNION.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
    }
}

/// Pop the native window system menu when the user secondary-clicks the
/// titlebar, excluding the caption buttons themselves.
///
/// With `HitTestMode::MaximizeButtonOnly` the titlebar answers `HTCLIENT`, so
/// Windows never sees a non-client right-click and never pops the menu itself —
/// the app must do it. Screen coordinates come from the viewport's `inner_rect`
/// (logical points, screen space) plus the pointer position; when the backend
/// does not report `inner_rect` we skip rather than pop the menu in the wrong
/// place.
fn handle_titlebar_system_menu(ctx: &egui::Context, band: egui::Rect) {
    // Never steal a right-click that egui is already using for its own popup
    // (the tab / editor context menus).
    if ctx.memory(|m| m.any_popup_open()) {
        return;
    }
    let Some(pos) = ctx.input(|i| {
        i.pointer
            .button_clicked(egui::PointerButton::Secondary)
            .then(|| i.pointer.interact_pos())
            .flatten()
    }) else {
        return;
    };
    if !band.contains(pos) {
        return;
    }
    // A right-click ON a caption button pops nothing on a native window either.
    if CAPTION_BTN_UNION
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_some_and(|r| r.contains(pos))
    {
        return;
    }
    let Some(inner) = ctx.input(|i| i.viewport().inner_rect) else {
        return;
    };
    let ppp = ctx.pixels_per_point();
    let screen = (inner.min.to_vec2() + pos.to_vec2()) * ppp;
    scribe_win32_chrome::show_system_menu(screen.x.round() as i32, screen.y.round() as i32);
}

pub(super) fn caption_btn(
    ui: &mut egui::Ui,
    icon: CaptionIcon,
    base: Color32,
    hover_fill: Color32,
    height: f32,
) -> egui::Response {
    // 46px wide is the standard Windows caption-button width; the height tracks
    // the titlebar so the buttons stay consistent with the in-titlebar toolbar
    // buttons when the user picks a large toolbar button size (the default 28px
    // is preserved — see the call site's `.max(28.0)`).
    let size = egui::vec2(46.0, height);
    // Record the titlebar band BEFORE the button consumes its slice, so the
    // right-click -> system-menu check sees the whole caption row (not just the
    // shrinking remainder). The first caption button of the frame wins; the
    // later ones only extend the caption-button union.
    let ctx = ui.ctx().clone();
    let pass = ctx.cumulative_pass_nr();
    {
        use std::sync::atomic::Ordering;
        // First caption button OF THIS PASS starts a fresh band + union; the
        // later ones only extend the union. Tracked on its own counter — the
        // maximize button is the SECOND button drawn, so keying this off
        // `LAST_MAX_RECT_PASS` would reset the union after Close was recorded.
        if LAST_BAND_PASS.swap(pass, Ordering::Relaxed) != pass {
            *TITLEBAR_BAND.lock().unwrap_or_else(|e| e.into_inner()) = Some(ui.max_rect());
            *CAPTION_BTN_UNION.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
    }
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    {
        let mut union = CAPTION_BTN_UNION.lock().unwrap_or_else(|e| e.into_inner());
        *union = Some(union.map_or(rect, |u| u.union(rect)));
    }
    // ---- win32 chrome wiring (see the module docs) ----
    // The maximize/restore button is the one region `WM_NCHITTEST` claims. Publish
    // its rect every frame it is laid out so the Snap Layouts flyout can trigger,
    // and register the retractor that clears it when the titlebar goes away.
    let is_max_button = matches!(icon, CaptionIcon::Maximize | CaptionIcon::Restore);
    if is_max_button {
        apply_chrome_policy();
        ctx.add_plugin(MaximizeRectRetractor);
        publish_maximize_rect(rect, ctx.pixels_per_point());
        LAST_MAX_RECT_PASS.store(pass, std::sync::atomic::Ordering::Relaxed);
        // Exactly ONE caption button runs this per frame — `show_system_menu`
        // spins a MODAL `TrackPopupMenu` loop, so running it once per button
        // would pop the menu four times over.
        if let Some(band) = *TITLEBAR_BAND.lock().unwrap_or_else(|e| e.into_inner()) {
            handle_titlebar_system_menu(&ctx, band);
        }
    }
    // Once Windows answers `HTMAXBUTTON` for that rect it delivers WM_NCMOUSEMOVE
    // there instead of client-area events, so egui's own `hovered()` is stuck
    // false over the button. OR in the OS-side hover or the button paints dead.
    let hovered =
        resp.hovered() || (is_max_button && scribe_win32_chrome::maximize_button_hovered());
    let painter = ui.painter();
    if hovered {
        painter.rect_filled(rect, 2.0, hover_fill);
    }
    let col = if hovered { Color32::WHITE } else { base };
    let c = rect.center();
    let s = 4.5_f32;
    let stroke = egui::Stroke::new(1.4, col);
    match icon {
        CaptionIcon::Minimize => {
            painter.line_segment([egui::pos2(c.x - s, c.y), egui::pos2(c.x + s, c.y)], stroke);
        }
        CaptionIcon::Maximize => {
            // egui 0.34: rect_stroke gained a 4th StrokeKind arg.
            painter.rect_stroke(
                egui::Rect::from_center_size(c, egui::vec2(2.0 * s, 2.0 * s)),
                1.0,
                stroke,
                egui::StrokeKind::Outside,
            );
        }
        CaptionIcon::Restore => {
            // Full front square (lower-left) + an L of the back square peeking
            // out upper-right — reads as "restore" with no overlap masking.
            let front = egui::Rect::from_center_size(
                egui::pos2(c.x - 1.5, c.y + 1.5),
                egui::vec2(2.0 * s, 2.0 * s),
            );
            painter.rect_stroke(front, 1.0, stroke, egui::StrokeKind::Outside);
            let top = front.top() - 3.0;
            let right = front.right() + 3.0;
            painter.line_segment(
                [egui::pos2(front.left() + 3.0, top), egui::pos2(right, top)],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(right, top),
                    egui::pos2(right, front.bottom() - 3.0),
                ],
                stroke,
            );
        }
        CaptionIcon::Close => {
            painter.line_segment(
                [egui::pos2(c.x - s, c.y - s), egui::pos2(c.x + s, c.y + s)],
                stroke,
            );
            painter.line_segment(
                [egui::pos2(c.x - s, c.y + s), egui::pos2(c.x + s, c.y - s)],
                stroke,
            );
        }
        CaptionIcon::Settings => {
            // A small "gear": a stroked hub ring + a center dot + 8 short radial
            // teeth. Painter-drawn so it never depends on font glyph coverage
            // (matching the other caption icons), and crisp at the caption size.
            let r = s; // hub-ring radius
            painter.circle_stroke(c, r, stroke);
            painter.circle_filled(c, 1.3, col);
            let tooth_outer = r + 2.0;
            for i in 0..8 {
                let ang = std::f32::consts::PI * (i as f32) / 4.0;
                let (sin, cos) = ang.sin_cos();
                let dir = egui::vec2(cos, sin);
                painter.line_segment([c + dir * r, c + dir * tooth_outer], stroke);
            }
        }
    }
    // These caption buttons are PAINTED (not text Buttons), so egui gives them no
    // accessible name — a screen reader (and the kittest e2e harness) cannot reach
    // them. Attach an explicit AccessKit Button name per icon (WCAG 4.1.2). The
    // Close name is deliberately distinct from the settings ✕ ("Close window") so a
    // by-label query is unambiguous; the icon already reads "Restore" while maximized,
    // so the Restore arm names it "Restore window".
    let name = match icon {
        // The app-window close (two-phase hide-before-destroy), distinct from the
        // settings dialog's ✕ which owns the bare "Close window" label.
        CaptionIcon::Close => "Close application window",
        CaptionIcon::Maximize => "Maximize window",
        CaptionIcon::Restore => "Restore window",
        CaptionIcon::Minimize => "Minimize window",
        CaptionIcon::Settings => "Open settings",
    };
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, name));
    resp
}

/// Width of the 4 edge resize zones, in logical px. Slim so they only intercept
/// pointer events right at the window border.
const RESIZE_EDGE_PX: f32 = 8.0;
/// Side length of the 4 corner resize zones, in logical px. Slightly larger than
/// the edges so diagonal grabs are forgiving.
const RESIZE_CORNER_PX: f32 = 12.0;

/// Which window-edge resize direction (if any) the pointer `p` is over, given
/// the window `rect` and the edge/corner band widths. Corners (within `corner`
/// of two sides) take priority over straight edges; the interior returns `None`.
/// Pure + unit-tested so the frameless-resize hit-testing can't silently regress.
pub(super) fn resize_dir_at(
    p: egui::Pos2,
    rect: egui::Rect,
    edge: f32,
    corner: f32,
) -> Option<egui::ResizeDirection> {
    use egui::ResizeDirection as D;
    let (l, r, t, b) = (
        p.x - rect.left(),
        rect.right() - p.x,
        p.y - rect.top(),
        rect.bottom() - p.y,
    );
    // Outside the window → not a resize zone.
    if l < 0.0 || r < 0.0 || t < 0.0 || b < 0.0 {
        return None;
    }
    let (w, e, n, s) = (l <= edge, r <= edge, t <= edge, b <= edge);
    let (nw, ne, nn, ns) = (l <= corner, r <= corner, t <= corner, b <= corner);
    if (n && nw) || (w && nn) {
        Some(D::NorthWest)
    } else if (n && ne) || (e && nn) {
        Some(D::NorthEast)
    } else if (s && nw) || (w && ns) {
        Some(D::SouthWest)
    } else if (s && ne) || (e && ns) {
        Some(D::SouthEast)
    } else if n {
        Some(D::North)
    } else if s {
        Some(D::South)
    } else if w {
        Some(D::West)
    } else if e {
        Some(D::East)
    } else {
        None
    }
}

/// Frameless window edge-resize, the no-Area way. Each frame: if the pointer is
/// over an edge band, hint the matching resize cursor; on a primary press there
/// — and only when egui isn't already using the pointer for a widget — start an
/// OS resize via `ViewportCommand::BeginResize`. No persistent `Order::Foreground`
/// Areas, so it never swallows clicks meant for tabs / the settings ✕ / panels,
/// and it works on every resize, not just the first.
pub(super) fn handle_frameless_resize(ctx: &egui::Context) {
    use egui::{CursorIcon as C, ResizeDirection as D, ViewportCommand};
    let Some(p) = ctx.pointer_latest_pos() else {
        return;
    };
    // Hit-test against the FULL window surface (screen_rect), not content_rect —
    // content_rect can exclude the top titlebar / bottom status panels, which
    // would push the resize bands inward off the real window edges so the user
    // can't grab them. screen_rect is the whole inner window area.
    let Some(dir) = resize_dir_at(p, ctx.screen_rect(), RESIZE_EDGE_PX, RESIZE_CORNER_PX) else {
        return;
    };
    ctx.set_cursor_icon(match dir {
        D::North => C::ResizeNorth,
        D::South => C::ResizeSouth,
        D::West => C::ResizeWest,
        D::East => C::ResizeEast,
        D::NorthWest => C::ResizeNorthWest,
        D::NorthEast => C::ResizeNorthEast,
        D::SouthWest => C::ResizeSouthWest,
        D::SouthEast => C::ResizeSouthEast,
    });
    // Start the OS resize on a FRESH press anywhere in the (thin) edge/corner
    // band. The previous `&& !ctx.wants_pointer_input()` guard is why resize
    // silently did nothing: the editor TextEdit + the status/side panels cover
    // every window edge, so `wants_pointer_input()` is true at the edges and the
    // BeginResize was always skipped (the cursor still changed — that part is
    // unconditional — which is exactly the "cursor changes but it doesn't
    // resize" report). The band is only 8px (12px at corners), so a press that
    // lands in it is an intentional resize; handing the drag to the OS is the
    // right call even if a widget also sits under the very edge. `primary_pressed`
    // is the rising edge, so no widget drag is in progress yet.
    if ctx.input(|i| i.pointer.primary_pressed()) {
        ctx.send_viewport_cmd(ViewportCommand::BeginResize(dir));
        // The OS now owns the drag. winit's modal resize loop swallows the
        // button-up, so egui can be left believing a drag is still in progress —
        // which makes `wants_pointer_input()` return true forever and blocks
        // EVERY subsequent resize (the "works once, then never" bug). Clearing
        // egui's drag bookkeeping here unsticks that state so resize re-arms.
        ctx.stop_dragging();
    }
    // Belt-and-suspenders: with no button held there can be no legitimate drag,
    // so proactively clear any phantom drag the OS resize loop may have orphaned.
    if !ctx.input(|i| i.pointer.any_down()) {
        ctx.stop_dragging();
    }
}
