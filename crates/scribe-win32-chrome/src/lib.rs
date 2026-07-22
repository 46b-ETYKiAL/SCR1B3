//! Isolated Win32 window-chrome fixups.
//!
//! This is the ONLY crate in the SCR1B3 tree permitted `unsafe` — mirroring
//! scribe-core's single mmap exception. It quarantines the audited Win32 FFI
//! that removes the "doubled caption buttons" the OS draws over our custom
//! titlebar on a frameless + transparent window.
//!
//! ## Root cause (deep-researched, primary-sourced)
//!
//! winit leaves `WS_SYSMENU | WS_MINIMIZEBOX | WS_MAXIMIZEBOX` set on an
//! UNDECORATED top-level window — it strips only `WS_CAPTION`/`WS_SIZEBOX`
//! (winit #2754). On Win11 DWM draws the native min/max/close caption buttons
//! GATED ON THOSE STYLE BITS. winit's transparency is `DwmEnableBlurBehindWindow`
//! (a DWM-COMPOSITED frame), so it does not CREATE the buttons — it UNMASKS the
//! DWM-drawn buttons the opaque panel was hiding ("doubled buttons with
//! transparency on"; opaque pixels hid them before).
//!
//! ## Why the earlier `WM_NCCALCSIZE` fix could never work
//!
//! Returning 0 from `WM_NCCALCSIZE` removes the STANDARD non-client frame, but
//! Microsoft documents that it "does not affect frames that are extended into
//! the client area" / DWM-composited content. The caption buttons are
//! DWM-composited, so `WM_NCCALCSIZE` is STRUCTURALLY incapable of removing
//! them. (`DWMWA_NCRENDERING_POLICY = DWMNCRP_DISABLED` is also wrong — it fights
//! winit's `DwmEnableBlurBehindWindow` transparency.)
//!
//! ## The fix (canonical: melak47/BorderlessWindow, MS DWM sample, Tao/Tauri,
//! Electron)
//!
//! Reconcile the window style with `SetWindowLongPtrW(GWL_STYLE, …)` +
//! `SetWindowPos(SWP_FRAMECHANGED)` so DWM stops compositing the native
//! min/max/close, and let the `WM_NCCALCSIZE`-returns-0 subclass leave no
//! non-client strip for the OS to paint into. winit's transparency is left
//! untouched. The reconcile runs every frame because winit re-derives styles
//! from its `WindowFlags` on some resize/restore paths; it is cheap because it
//! writes only when the current style actually differs from the desired one.
//!
//! ## Which bits are cleared — and why `WS_MAXIMIZEBOX` is NOT
//!
//! An earlier revision of this crate cleared
//! `WS_SYSMENU | WS_MINIMIZEBOX | WS_MAXIMIZEBOX | WS_CAPTION` unconditionally.
//! That was too broad and cost real functionality: **Windows gates Aero Snap on
//! `WS_MAXIMIZEBOX`**, so clearing it disabled drag-to-edge snapping,
//! drag-to-top-maximize, `Win`+`Left`/`Right`/`Up`, Snap Assist **and the
//! Windows 11 Snap Layouts flyout** — the last of which this crate elsewhere
//! goes to some length to enable. `WS_MINIMIZEBOX` likewise gates `Win`+`Down`
//! and the taskbar minimize/restore animation, and `WS_SYSMENU` gates
//! `Alt`+`Space`.
//!
//! So the default is now the narrow strip: only `WS_CAPTION` — the bit that
//! actually asks for a caption STRIP — is cleared, and the three
//! window-management bits are RETAINED (see [`imp::SNAP_RETAINED_STYLES`]).
//! [`set_snap_support_enabled`]`(false)` restores the old full strip for the case
//! documented there.
//!
//! The `WM_NCCALCSIZE` subclass also does its other job: clamping a borderless
//! MAXIMIZE to the monitor work area (and an auto-hide taskbar) so it doesn't
//! cover the taskbar. Resize and drag are egui-owned
//! (`ViewportCommand::BeginResize` — winit #4186 — and `StartDrag`), so the NC
//! area is otherwise unused.
//!
//! ## What is verified, and what is NOT
//!
//! Everything in this crate that is a **pure decision** — the hit-test
//! classification, the system-menu enable table, the style reconciliation, the
//! DPI mapping, the `LPARAM` unpacking, the `HT*`/`SC_*` constant values — is
//! unit-tested and runs on every host.
//!
//! What CANNOT be tested here, and is therefore **not claimed to work**, is
//! anything that requires a real window on real Windows 11: whether the Snap
//! Layouts flyout actually appears over the published rect, whether retaining
//! the snap bits re-admits doubled caption buttons on a transparent window,
//! whether Mica actually composites, and whether the rounded corners take. Those
//! need a rendered frame on the target OS; this crate's tests prove the inputs
//! to those behaviours are correct, not the behaviours themselves.
//!
//! ## Extensions: Snap Layouts, system menu, rounded corners, backdrop
//!
//! Everything above is unchanged. Layered on top:
//!
//! * **`WM_NCHITTEST`** — the subclass now answers it, returning `HTMAXBUTTON`
//!   over the app-published maximize-button rect ([`set_maximize_button_rect`]).
//!   That reply is the ONLY trigger for the Windows 11 **Snap Layouts** flyout.
//!   `DwmDefWindowProc` is consulted FIRST for the caption-button message set,
//!   per the MS custom-frame guidance, so DWM renders the flyout itself.
//! * **Drag/resize ownership** — deliberately left with egui
//!   ([`hit_test::HitTestMode::MaximizeButtonOnly`], the default): every point
//!   except the maximize button answers `HTCLIENT`, so
//!   `scribe-app/src/app/chrome.rs`'s `resize_dir_at` / `handle_frameless_resize`
//!   and `ViewportCommand::StartDrag` keep working untouched. Two systems both
//!   answering "is this a resize edge?" is a real bug source (the OS modal resize
//!   loop eats the button-up egui's state machine waits for), so exactly one owns
//!   it. [`hit_test::HitTestMode::FullNonClient`] is available for ports that
//!   want the opposite split — an app selecting it MUST disable its egui-space
//!   resize handler.
//! * **`HTMAXBUTTON` consequence** — Windows then routes clicks over that rect as
//!   `WM_NCLBUTTONDOWN`/`UP`, so egui never sees them. The subclass handles those
//!   itself and posts `WM_SYSCOMMAND(SC_MAXIMIZE|SC_RESTORE)`, and tracks hover
//!   for [`maximize_button_hovered`] so the app can still paint its hover state.
//! * **System menu** — [`show_system_menu`] pops the real `GetSystemMenu` popup
//!   with correct per-state greying ([`system_menu::menu_state`]).
//! * **Rounded corners** — `DWMWA_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND`,
//!   applied once from [`ensure_caption_stripped`]. Windows 10 rejects the
//!   attribute with an `HRESULT` we ignore, so it degrades to today's square
//!   window; it can never fail the app.
//! * **Backdrop (Mica/Acrylic)** — [`set_backdrop`], DEFAULT-OFF. See its doc for
//!   why it is opt-in rather than on.
//!
//! ## Call-site status (honest inventory)
//!
//! `scribe-app` currently calls [`ensure_caption_stripped`], [`set_main_hwnd`]
//! and [`allow_foreground_handoff`]. Inside this crate,
//! [`apply_rounded_corners`] and the backdrop push are called from
//! `imp::ensure_borderless`, so they run on the app's existing per-frame call.
//!
//! The Snap-Layouts and system-menu entry points —
//! [`set_maximize_button_rect`], [`clear_maximize_button_rect`],
//! [`maximize_button_hovered`], [`show_system_menu`], [`set_hit_test_mode`],
//! [`set_snap_support_enabled`], [`set_backdrop`] — are implemented and tested
//! but have **no caller in this repository yet**. They are the API the titlebar
//! in `scribe-app/src/app/chrome.rs` must call for the flyout and the system
//! menu to become reachable; until it does, `WM_NCHITTEST` never sees a
//! published rect and answers `HTCLIENT` everywhere, which is exactly today's
//! behaviour. This paragraph exists so the gap is visible rather than implied.

pub mod hit_test;
pub mod system_menu;

pub use hit_test::{HitTestMode, RectPx};

/// Which DWM system backdrop material the window requests.
///
/// Mirrors `DWM_SYSTEMBACKDROP_TYPE`. `None` is the default and is what ships:
/// see [`set_backdrop`] for the honest status of the others.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Backdrop {
    /// No DWM material — the window paints its own background (today's shipped
    /// behaviour).
    #[default]
    None,
    /// `DWMSBT_MAINWINDOW` — Mica.
    Mica,
    /// `DWMSBT_TRANSIENTWINDOW` — Acrylic.
    Acrylic,
    /// `DWMSBT_TABBEDWINDOW` — Mica Alt.
    MicaAlt,
}

/// Ensure THIS process's main top-level window draws no system caption buttons
/// over the custom titlebar, by reconciling the window style (clearing
/// `WS_CAPTION`, and — unless [`set_snap_support_enabled`] is off — RETAINING
/// the snap-gating `WS_SYSMENU|WS_MINIMIZEBOX|WS_MAXIMIZEBOX`) and installing a
/// one-time subclass that answers `WM_NCHITTEST` and clamps a borderless
/// maximize to the monitor work area. Also applies the rounded-corner and
/// backdrop DWM attributes. Windows-only; a no-op everywhere else.
///
/// Safe + cheap to call every frame: the HWND is cached, the subclass and the
/// corner attribute apply once, and the style reconcile writes only when the
/// current style differs from the desired one (so it self-heals if winit
/// re-asserts styles, at near-zero cost otherwise).
#[cfg(windows)]
pub fn ensure_caption_stripped() {
    imp::ensure_borderless();
}

/// No-op on non-Windows platforms (the bug is Windows-DWM-specific).
#[cfg(not(windows))]
pub fn ensure_caption_stripped() {}

/// Prime the crate with the REAL native window handle (from eframe's
/// `Frame`/`HasWindowHandle`). This is the authoritative HWND of the app's
/// window; passing it here means [`ensure_caption_stripped`] subclasses the
/// CORRECT window instead of falling back to an `EnumWindows` guess (which could
/// latch onto the wrong top-level window — the likely reason earlier caption
/// fixes had no effect). Idempotent; call every frame. Windows-only.
#[cfg(windows)]
pub fn set_main_hwnd(hwnd: isize) {
    imp::set_main_hwnd(hwnd);
}

/// No-op on non-Windows platforms.
#[cfg(not(windows))]
pub fn set_main_hwnd(_hwnd: isize) {}

/// Whether the OS is currently requesting REDUCED MOTION (WCAG 2.3.3). On Windows
/// this reads `SPI_GETCLIENTAREAANIMATION`: when the user has disabled animations
/// in Settings ▸ Accessibility ▸ Visual effects (or "Show animations in Windows"),
/// the flag is `FALSE`, which we report as reduced-motion `true`. The app gates
/// its animations through `MotionConfig::effective_enabled(this)`, so the OS
/// accessibility preference overrides the in-app toggle. Fast (a registry-cached
/// read); safe to call per frame.
#[cfg(windows)]
pub fn os_reduced_motion() -> bool {
    imp::os_reduced_motion()
}

/// Non-Windows: no OS signal is consulted, so motion follows the in-app toggle
/// only (reduced-motion `false` means "the OS is not forcing motion off").
#[cfg(not(windows))]
pub fn os_reduced_motion() -> bool {
    false
}

/// Hand THIS process's foreground right to any process about to be spawned, so a
/// just-launched child (the self-updater's relaunched binary, or the elevated
/// `setup.exe` started via PowerShell) is permitted to call
/// `SetForegroundWindow` on its own window and come to the FRONT instead of
/// flashing in the taskbar behind us.
///
/// ## Root cause (deep-researched, primary-sourced)
///
/// Windows enforces a foreground lock (`SPI_GETFOREGROUNDLOCKTIMEOUT`): a process
/// may set the foreground window only if it currently OWNS the foreground, or a
/// foreground-owning process called `AllowSetForegroundWindow` to delegate that
/// right to a target. When SCR1B3 spawns the installer/new binary it IS the
/// foreground process (the `ViewportCommand::Close` is queued, drained on the
/// next egui frame), but it never delegates the right — so the child's
/// `SetForegroundWindow` is silently demoted to a taskbar flash and the window
/// lands BEHIND. (MS docs: "SetForegroundWindow", "AllowSetForegroundWindow".)
///
/// Pass `ASFW_ANY` (`u32::MAX`), not a specific PID: on the elevated path the
/// real installer is a GRANDCHILD (PowerShell → UAC `consent.exe` → `setup.exe`),
/// so the PID SCR1B3 gets back from spawning `powershell` is not the installer's.
/// `ASFW_ANY` is the only grant that reaches it.
///
/// MUST be called BEFORE the spawn, while SCR1B3 still owns the foreground (the
/// API no-ops once the caller is no longer foreground). Windows-only; a no-op
/// everywhere else.
#[cfg(windows)]
pub fn allow_foreground_handoff() {
    imp::allow_foreground_handoff();
}

/// No-op on non-Windows platforms (the foreground-lock is Windows-specific).
#[cfg(not(windows))]
pub fn allow_foreground_handoff() {}

// ---------------------------------------------------------------------------
// Snap Layouts: publishing the maximize-button rect
// ---------------------------------------------------------------------------

/// Publish the app's maximize/restore caption-button rect so `WM_NCHITTEST` can
/// answer `HTMAXBUTTON` over it — the ONLY thing that makes Windows 11 show the
/// **Snap Layouts** flyout.
///
/// Coordinates are **physical pixels in client space** (origin = the window's
/// top-left client pixel). egui works in logical points, so scale first:
///
/// ```no_run
/// # let (rect, ppp) = (egui_stub::Rect, 1.0_f32);
/// # mod egui_stub { pub struct Rect; }
/// # fn demo(r: (f32, f32, f32, f32), ppp: f32) {
/// let p = scribe_win32_chrome::hit_test::logical_rect_to_physical(r.0, r.1, r.2, r.3, ppp);
/// scribe_win32_chrome::set_maximize_button_rect(p.left, p.top, p.right, p.bottom);
/// # }
/// ```
///
/// Cheap and idempotent — call it every frame from the titlebar layout so the
/// rect follows resizes, DPI changes and toolbar-size changes. An empty or
/// inverted rect is treated as "no button" (never hit), so a stale publish can
/// only cost the flyout, never mis-claim a region. Windows-only; a no-op
/// everywhere else.
#[cfg(windows)]
pub fn set_maximize_button_rect(left: i32, top: i32, right: i32, bottom: i32) {
    imp::set_maximize_button_rect(hit_test::RectPx::new(left, top, right, bottom));
}

/// No-op on non-Windows platforms.
#[cfg(not(windows))]
pub fn set_maximize_button_rect(_left: i32, _top: i32, _right: i32, _bottom: i32) {}

/// Retract the published maximize-button rect (e.g. while fullscreen, or when
/// the frameless titlebar is not shown). Windows-only; a no-op elsewhere.
#[cfg(windows)]
pub fn clear_maximize_button_rect() {
    imp::set_maximize_button_rect(hit_test::RectPx::default());
}

/// No-op on non-Windows platforms.
#[cfg(not(windows))]
pub fn clear_maximize_button_rect() {}

/// Whether the pointer is currently over the published maximize-button rect
/// **as Windows sees it**.
///
/// Once `WM_NCHITTEST` answers `HTMAXBUTTON`, Windows delivers
/// `WM_NCMOUSEMOVE`/`WM_NCLBUTTON*` for that region instead of client-area
/// events, so egui's own `Response::hovered()` goes permanently false there and
/// the button stops painting its hover fill. Read this and OR it into the
/// button's hover state to restore that. Always `false` off-Windows.
#[cfg(windows)]
#[must_use]
pub fn maximize_button_hovered() -> bool {
    imp::maximize_button_hovered()
}

/// Always `false` on non-Windows platforms.
#[cfg(not(windows))]
#[must_use]
pub fn maximize_button_hovered() -> bool {
    false
}

/// Select which regions this crate claims from `WM_NCHITTEST`. Defaults to
/// [`HitTestMode::MaximizeButtonOnly`], which is what SCR1B3 uses (egui keeps
/// drag + resize). Windows-only; a no-op elsewhere.
#[cfg(windows)]
pub fn set_hit_test_mode(mode: HitTestMode) {
    imp::set_hit_test_mode(mode);
}

/// No-op on non-Windows platforms.
#[cfg(not(windows))]
pub fn set_hit_test_mode(_mode: HitTestMode) {}

// ---------------------------------------------------------------------------
// Aero Snap / Win+Arrow: retaining the snap-gating style bits
// ---------------------------------------------------------------------------

/// Whether to retain `WS_SYSMENU | WS_MINIMIZEBOX | WS_MAXIMIZEBOX` so Windows
/// treats the window as snap-eligible. **DEFAULT-ON.**
///
/// Aero Snap drag-to-edge, drag-to-top-maximize, `Win`+arrow, Snap Assist and
/// the Windows 11 Snap Layouts flyout ALL gate on `WS_MAXIMIZEBOX`; `Win`+`Down`
/// and the taskbar minimize animation gate on `WS_MINIMIZEBOX`; `Alt`+`Space`
/// gates on `WS_SYSMENU`. Clearing those bits — which this crate used to do
/// unconditionally — disables every one of those gestures with no way to get
/// them back, so retaining them is the default.
///
/// Passing `false` restores the historical full strip. That escape hatch exists
/// because of a real, primary-sourced concern recorded in the crate root: on a
/// winit window made transparent via `DwmEnableBlurBehindWindow` the caption
/// buttons are DWM-COMPOSITED, and `WM_NCCALCSIZE` is documented as not
/// affecting frames extended into the client area — so on some Win11 builds and
/// transparency states, retaining the bits **may** re-admit the doubled native
/// buttons over the custom titlebar.
///
/// **That outcome has not been observed either way by this change** — it cannot
/// be determined from a unit test and needs a real window on real Win11. If
/// doubled buttons appear, call this with `false` to get byte-for-byte the old
/// behaviour (at the cost of snap). The `HTMAXBUTTON` reply from
/// [`set_maximize_button_rect`] is independent of this switch.
///
/// Takes effect immediately when the window is already known, and in BOTH
/// directions: unlike the old one-way strip, turning this back on restores bits
/// a previous frame had cleared.
#[cfg(windows)]
pub fn set_snap_support_enabled(enabled: bool) {
    imp::set_snap_support_enabled(enabled);
}

/// No-op on non-Windows platforms.
#[cfg(not(windows))]
pub fn set_snap_support_enabled(_enabled: bool) {}

// ---------------------------------------------------------------------------
// System menu (Alt+Space / titlebar right-click)
// ---------------------------------------------------------------------------

/// Pop the native window system menu at `(screen_x, screen_y)` and post the
/// chosen command as `WM_SYSCOMMAND`.
///
/// The app must call this itself: with the titlebar answering `HTCLIENT` (see the
/// ownership decision in [`hit_test`]) Windows never sees a non-client
/// right-click, so it never pops the menu on its own. Wire it to a right-click
/// (and, if desired, `Alt`+`Space`) in the custom titlebar, passing the pointer
/// position in **screen** coordinates.
///
/// Items are enabled/greyed for the current window state via
/// [`system_menu::menu_state`] — `GetSystemMenu` otherwise returns the default
/// state, which assumes a restored, resizable window and would offer "Maximize"
/// on an already-maximized window. Windows-only; a no-op elsewhere.
#[cfg(windows)]
pub fn show_system_menu(screen_x: i32, screen_y: i32) {
    imp::show_system_menu(screen_x, screen_y);
}

/// No-op on non-Windows platforms.
#[cfg(not(windows))]
pub fn show_system_menu(_screen_x: i32, _screen_y: i32) {}

// ---------------------------------------------------------------------------
// DWM: rounded corners + backdrop material
// ---------------------------------------------------------------------------

/// Request rounded window corners (`DWMWA_WINDOW_CORNER_PREFERENCE` =
/// `DWMWCP_ROUND`).
///
/// Called automatically (once) by [`ensure_caption_stripped`], so an app that
/// already calls that per frame needs no change. Exposed for ports that do not.
/// Windows 10 does not know the attribute and returns a failing `HRESULT`, which
/// is ignored — the window stays square exactly as it is today. No version probe
/// is needed or performed: the API's own rejection IS the clean degrade.
#[cfg(windows)]
pub fn apply_rounded_corners() {
    imp::apply_rounded_corners();
}

/// No-op on non-Windows platforms.
#[cfg(not(windows))]
pub fn apply_rounded_corners() {}

/// Request a DWM system backdrop material (`DWMWA_SYSTEMBACKDROP_TYPE`).
///
/// **Ships DEFAULT-OFF ([`Backdrop::None`]) and is NOT verified to composite.**
/// The DWM side is implemented and the attribute is set, but for the material to
/// be visible the app's own painted surface must be non-opaque all the way
/// through `egui-wgpu`'s composite-alpha path — SCR1B3 paints an opaque
/// `panel_fill` unless `effective_translucent()` is on, and `wgpu`'s selected
/// `CompositeAlphaMode` is negotiated per adapter. Whether Mica actually shows
/// through cannot be established from a unit test, and this agent has not seen a
/// rendered frame. Treat it as an opt-in to evaluate, not a shipped feature.
///
/// Applied immediately if the window is known, and re-applied on the next
/// [`ensure_caption_stripped`] otherwise. Windows-only; a no-op elsewhere.
#[cfg(windows)]
pub fn set_backdrop(backdrop: Backdrop) {
    imp::set_backdrop(backdrop);
}

/// No-op on non-Windows platforms.
#[cfg(not(windows))]
pub fn set_backdrop(_backdrop: Backdrop) {}

#[cfg(windows)]
mod imp {
    use std::sync::atomic::{AtomicBool, AtomicI32, AtomicIsize, AtomicU8, Ordering};

    use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
    use windows_sys::Win32::Graphics::Dwm::{DwmDefWindowProc, DwmSetWindowAttribute};
    use windows_sys::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromWindow, ScreenToClient, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        TrackMouseEvent, TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT,
    };
    use windows_sys::Win32::UI::Shell::{
        DefSubclassProc, SHAppBarMessage, SetWindowSubclass, ABM_GETSTATE, ABS_AUTOHIDE, APPBARDATA,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        AllowSetForegroundWindow, EnableMenuItem, EnumWindows, GetClientRect, GetSystemMenu,
        GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
        PostMessageW, SetWindowLongPtrW, SetWindowPos, TrackPopupMenu, GWL_STYLE, HTMAXBUTTON,
        MF_BYCOMMAND, MF_ENABLED, MF_GRAYED, NCCALCSIZE_PARAMS, SC_MAXIMIZE, SC_RESTORE,
        SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, TPM_RETURNCMD,
        TPM_RIGHTBUTTON, WM_NCCALCSIZE, WM_NCHITTEST, WM_NCLBUTTONDOWN, WM_NCLBUTTONUP,
        WM_NCMOUSELEAVE, WM_NCMOUSEMOVE, WM_NCRBUTTONDOWN, WM_NCRBUTTONUP, WM_SYSCOMMAND,
        WS_CAPTION, WS_MAXIMIZE, WS_MAXIMIZEBOX, WS_MINIMIZEBOX, WS_SYSMENU, WS_THICKFRAME,
    };

    use crate::hit_test::{self, HitTestGeometry, HitTestMode, HitZone, RectPx};
    use crate::system_menu::{menu_state, WindowState};
    use crate::Backdrop;

    /// Query `SPI_GETCLIENTAREAANIMATION`. The BOOL out-param is `TRUE` when
    /// animations are ON; reduced-motion is the negation. A failed call (returns
    /// 0) is treated as "not reduced" so a query error never suppresses motion.
    pub(super) fn os_reduced_motion() -> bool {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SystemParametersInfoW, SPI_GETCLIENTAREAANIMATION,
        };
        let mut animations_on: BOOL = 1;
        // SAFETY: SPI_GETCLIENTAREAANIMATION writes a single BOOL into the pointer
        // we pass; no ownership transfer, no aliasing. Read-only system query.
        let ok = unsafe {
            SystemParametersInfoW(
                SPI_GETCLIENTAREAANIMATION,
                0,
                (&mut animations_on as *mut BOOL).cast(),
                0,
            )
        };
        ok != 0 && animations_on == 0
    }

    /// The window-style bits that make DWM draw the native min/max/close caption
    /// buttons. winit leaves `WS_SYSMENU | WS_MINIMIZEBOX | WS_MAXIMIZEBOX` set on
    /// an UNDECORATED window — it only strips `WS_CAPTION`/`WS_SIZEBOX` (winit
    /// #2754). On a transparent (DWM blur-behind) window those buttons are DWM-
    /// composited and show THROUGH as a doubled set over our custom titlebar.
    /// Clearing these bits is the canonical fix (melak47/BorderlessWindow, the MS
    /// DWM custom-frame sample, Tao/Tauri, Electron); `WM_NCCALCSIZE` cannot
    /// remove them because they are composited by DWM, not part of the standard
    /// non-client frame (MS WM_NCCALCSIZE docs). `WS_CAPTION` is included for
    /// completeness — clearing an already-absent bit is a no-op.
    const CAPTION_BUTTON_STYLES: u32 = WS_SYSMENU | WS_MINIMIZEBOX | WS_MAXIMIZEBOX | WS_CAPTION;

    /// The window-management style bits Windows gates **snap** on, and which are
    /// therefore RETAINED by default (see [`SNAP_SUPPORT`]).
    ///
    /// This is the correctness fix for the original unconditional strip. Windows
    /// gates its window-management gestures on these bits:
    ///
    /// * `WS_MAXIMIZEBOX` — Aero Snap drag-to-edge, drag-to-top-maximize,
    ///   `Win`+`Left`/`Right`/`Up`, Snap Assist, **and the Win11 Snap Layouts
    ///   flyout**. Stripping it disables every one of them; there is no way to
    ///   re-enable snap while it is clear. This was the single most damaging bit
    ///   in the original strip set.
    /// * `WS_MINIMIZEBOX` — `Win`+`Down` minimize and the taskbar
    ///   minimize/restore animation.
    /// * `WS_SYSMENU` — `Alt`+`Space` and the taskbar right-click system menu
    ///   (and it is a prerequisite for the min/max boxes being meaningful at all).
    ///
    /// `WS_CAPTION` is deliberately NOT in this set: it is the bit that actually
    /// asks for a caption STRIP, and nothing about snap depends on it, so it is
    /// stripped in both modes.
    const SNAP_RETAINED_STYLES: u32 = WS_SYSMENU | WS_MINIMIZEBOX | WS_MAXIMIZEBOX;

    /// Which bits [`reconcile_styles`] clears, given the snap-support setting.
    ///
    /// With snap support ON (the default) this is just `WS_CAPTION`; with it OFF
    /// it is the full original caption-button set.
    const fn styles_to_strip(snap_support: bool) -> u32 {
        if snap_support {
            CAPTION_BUTTON_STYLES & !SNAP_RETAINED_STYLES
        } else {
            CAPTION_BUTTON_STYLES
        }
    }

    /// PURE style decision: the style `hwnd` SHOULD have, given its current
    /// `style` and the snap-support setting.
    ///
    /// Deliberately a full reconciliation rather than a one-way strip. The old
    /// code only ever CLEARED bits, which made the snap-support setting
    /// one-directional in practice: once a frame had stripped `WS_MAXIMIZEBOX`,
    /// nothing ever put it back, so enabling snap support at runtime (or enabling
    /// it after the first frame had already run) could never take effect. Because
    /// this returns the desired style rather than a masked one, the per-frame call
    /// both strips what must go and RESTORES what snap needs — so the setting is
    /// live in both directions and the per-frame pass converges instead of
    /// fighting itself.
    const fn desired_style(style: u32, snap_support: bool) -> u32 {
        let stripped = style & !styles_to_strip(snap_support);
        if snap_support {
            stripped | SNAP_RETAINED_STYLES
        } else {
            stripped
        }
    }

    /// Whether any caption-button style bit is currently set on `style`.
    /// Diagnostic-only (see [`nc_state_line`]).
    fn caption_button_styles_present(style: u32) -> bool {
        style & CAPTION_BUTTON_STYLES != 0
    }

    /// Cached main-window HWND (0 = not yet found). One window per process.
    /// Primed by [`set_main_hwnd`] with the real eframe handle when available;
    /// falls back to the `EnumWindows` guess only if never primed.
    static CACHED_HWND: AtomicIsize = AtomicIsize::new(0);
    /// Set once the NC subclass is successfully installed (install is one-shot).
    static SUBCLASSED: AtomicBool = AtomicBool::new(false);
    /// Set once the one-shot diagnostic file has been written.
    static DIAG_WRITTEN: AtomicBool = AtomicBool::new(false);
    /// Set once `DWMWA_WINDOW_CORNER_PREFERENCE` has been applied (one-shot).
    static CORNERS_APPLIED: AtomicBool = AtomicBool::new(false);

    /// Whether the snap-gating styles are retained. **Default ON** — see
    /// [`SNAP_RETAINED_STYLES`] and the public `set_snap_support_enabled` doc.
    static SNAP_SUPPORT: AtomicBool = AtomicBool::new(true);

    /// The published maximize-button rect, in physical client pixels.
    ///
    /// Four independent atomics rather than a lock: this is read from inside the
    /// window procedure, where taking a `Mutex` risks a re-entrant or
    /// cross-thread stall on a hot OS callback. The only cost is that a rect
    /// written concurrently with a hit test can be read half-updated for a single
    /// message — which at worst mis-answers one `WM_NCHITTEST` by a few pixels
    /// during a resize and self-corrects on the next frame's publish. A torn read
    /// can never claim a region outside the union of the old and new rects.
    static BTN_LEFT: AtomicI32 = AtomicI32::new(0);
    static BTN_TOP: AtomicI32 = AtomicI32::new(0);
    static BTN_RIGHT: AtomicI32 = AtomicI32::new(0);
    static BTN_BOTTOM: AtomicI32 = AtomicI32::new(0);

    /// Whether the pointer is over the maximize button *as Windows sees it*.
    static BTN_HOVERED: AtomicBool = AtomicBool::new(false);
    /// Whether a non-client left-press landed on the maximize button (so the
    /// matching button-UP is ours to act on, and a press-elsewhere-release-here
    /// is not).
    static BTN_PRESSED: AtomicBool = AtomicBool::new(false);

    /// `HitTestMode`, encoded for atomic storage (see [`hit_test_mode`]).
    static HIT_TEST_MODE: AtomicU8 = AtomicU8::new(MODE_MAX_BUTTON_ONLY);
    const MODE_MAX_BUTTON_ONLY: u8 = 0;
    const MODE_FULL_NON_CLIENT: u8 = 1;

    /// `Backdrop`, encoded for atomic storage (see [`backdrop_from_u8`]).
    static BACKDROP: AtomicU8 = AtomicU8::new(BACKDROP_NONE);
    const BACKDROP_NONE: u8 = 0;
    const BACKDROP_MICA: u8 = 1;
    const BACKDROP_ACRYLIC: u8 = 2;
    const BACKDROP_MICA_ALT: u8 = 3;

    /// `DWMWA_WINDOW_CORNER_PREFERENCE` (dwmapi.h). Not exported by windows-sys
    /// 0.59's `Dwm` module, so it is spelled out; a wrong value is rejected by DWM
    /// with a failing `HRESULT`, which is the same clean degrade as Windows 10.
    const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
    /// `DWMWA_SYSTEMBACKDROP_TYPE` (dwmapi.h). Same rationale.
    const DWMWA_SYSTEMBACKDROP_TYPE: u32 = 38;
    /// `DWMWCP_ROUND`.
    const DWMWCP_ROUND: u32 = 2;

    /// Map the stored [`HitTestMode`] discriminant back to the enum.
    fn hit_test_mode() -> HitTestMode {
        match HIT_TEST_MODE.load(Ordering::Relaxed) {
            MODE_FULL_NON_CLIENT => HitTestMode::FullNonClient,
            _ => HitTestMode::MaximizeButtonOnly,
        }
    }

    /// The `DWM_SYSTEMBACKDROP_TYPE` value for a stored [`Backdrop`] discriminant.
    /// `DWMSBT_NONE` is 1 (not 0 — 0 is `DWMSBT_AUTO`).
    const fn backdrop_from_u8(v: u8) -> u32 {
        match v {
            BACKDROP_MICA => 2,     // DWMSBT_MAINWINDOW
            BACKDROP_ACRYLIC => 3,  // DWMSBT_TRANSIENTWINDOW
            BACKDROP_MICA_ALT => 4, // DWMSBT_TABBEDWINDOW
            _ => 1,                 // DWMSBT_NONE
        }
    }

    /// The published maximize-button rect, or `None` when nothing usable is
    /// published (an empty/inverted rect is treated as absent, never as a claim).
    fn published_button_rect() -> Option<RectPx> {
        let r = RectPx::new(
            BTN_LEFT.load(Ordering::Relaxed),
            BTN_TOP.load(Ordering::Relaxed),
            BTN_RIGHT.load(Ordering::Relaxed),
            BTN_BOTTOM.load(Ordering::Relaxed),
        );
        (!r.is_empty()).then_some(r)
    }

    /// Publish the maximize-button rect. See the public wrapper.
    pub fn set_maximize_button_rect(rect: RectPx) {
        BTN_LEFT.store(rect.left, Ordering::Relaxed);
        BTN_TOP.store(rect.top, Ordering::Relaxed);
        BTN_RIGHT.store(rect.right, Ordering::Relaxed);
        BTN_BOTTOM.store(rect.bottom, Ordering::Relaxed);
        if rect.is_empty() {
            // A retracted rect can never stay "hovered" — the region is gone.
            BTN_HOVERED.store(false, Ordering::Relaxed);
            BTN_PRESSED.store(false, Ordering::Relaxed);
        }
    }

    /// See the public wrapper.
    pub fn maximize_button_hovered() -> bool {
        BTN_HOVERED.load(Ordering::Relaxed)
    }

    /// See the public wrapper.
    pub fn set_hit_test_mode(mode: HitTestMode) {
        let v = match mode {
            HitTestMode::MaximizeButtonOnly => MODE_MAX_BUTTON_ONLY,
            HitTestMode::FullNonClient => MODE_FULL_NON_CLIENT,
        };
        HIT_TEST_MODE.store(v, Ordering::Relaxed);
    }

    /// See the public wrapper. Applies on the next `ensure_borderless` pass (and
    /// immediately if the window is already known), in BOTH directions — see
    /// [`desired_style`].
    pub fn set_snap_support_enabled(enabled: bool) {
        SNAP_SUPPORT.store(enabled, Ordering::Relaxed);
        let hwnd = CACHED_HWND.load(Ordering::Relaxed);
        if hwnd != 0 {
            reconcile_styles(hwnd);
        }
    }

    /// A stable, arbitrary subclass id for our single subclass entry.
    const SUBCLASS_ID: usize = 0x5C_1B_3E;

    /// Prime the cached HWND with the authoritative handle (see the public
    /// wrapper). Stores only a non-zero value; idempotent.
    pub fn set_main_hwnd(hwnd: isize) {
        if hwnd != 0 {
            CACHED_HWND.store(hwnd, Ordering::Relaxed);
        }
    }

    /// `ASFW_ANY`: grant the foreground-set right to ANY process.
    ///
    /// ## Why `ASFW_ANY` and not a specific PID (S-03 least-privilege review)
    ///
    /// `AllowSetForegroundWindow` takes a single PID and MUST be called BEFORE
    /// the spawn, while THIS process still owns the foreground — once the caller
    /// loses the foreground the API no-ops (MS docs), and the grant must already
    /// be in place before the child's `SetForegroundWindow` fires. At every call
    /// site (`updater.rs` install + relaunch) the eventual foreground-setter's
    /// PID is therefore NOT yet available:
    ///
    /// * Elevated install: the real installer is a GRANDCHILD (PowerShell → UAC
    ///   `consent.exe` → `setup.exe`); the PID we get from spawning `powershell`
    ///   is not the installer's, so a PID-specific grant would target the wrong
    ///   process.
    /// * In-place relaunch: the child is spawned AFTER this call (it cannot be
    ///   spawned first — the grant has to precede the child's foreground-set, and
    ///   this process must still own the foreground when the grant is made), so
    ///   the child PID does not exist at the moment the grant is needed.
    ///
    /// `ASFW_ANY` is the only grant that satisfies the pre-spawn ordering. It is
    /// an ACCEPTED, time-bounded, narrowly-scoped grant — NOT an always-on one:
    /// it is issued ONLY from the two `updater.rs` spawn sites, each immediately
    /// before a `spawn()`, never from the main loop or any per-frame path. Its
    /// effect is inherently short-lived: Windows consumes the delegated right on
    /// the next `SetForegroundWindow` (or it lapses when this process loses the
    /// foreground a moment later as the relaunch/close proceeds). It does not
    /// persist a standing capability.
    const ASFW_ANY: u32 = 0xFFFF_FFFF;

    /// Delegate this (currently-foreground) process's right to set the foreground
    /// window to any soon-to-be-spawned process. See the public wrapper.
    pub fn allow_foreground_handoff() {
        // SAFETY: a single Win32 call with a constant `u32` argument (no pointers,
        // no handles). No-ops harmlessly if this process is not the foreground.
        unsafe {
            AllowSetForegroundWindow(ASFW_ANY);
        }
    }

    /// `EnumWindows` callback: record the first visible top-level window owned by
    /// this process into the `*mut isize` passed via `lparam`, then stop.
    unsafe extern "system" fn enum_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid == GetCurrentProcessId() && IsWindowVisible(hwnd) != 0 {
            *(lparam as *mut isize) = hwnd as isize;
            return 0; // FALSE → stop enumerating
        }
        1 // TRUE → keep going
    }

    fn find_main_window() -> isize {
        let mut found: isize = 0;
        // SAFETY: `enum_cb` only writes the `isize` behind `lparam` (a stack local
        // that outlives the synchronous EnumWindows call) and reads OS-owned HWNDs.
        unsafe {
            EnumWindows(Some(enum_cb), (&mut found as *mut isize) as LPARAM);
        }
        found
    }

    /// Whether the window is currently maximized (its `WS_MAXIMIZE` style is set).
    fn is_maximized(hwnd: HWND) -> bool {
        // SAFETY: `hwnd` is an OS window owned by this process; GWL_STYLE read.
        let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } as u32;
        style & WS_MAXIMIZE != 0
    }

    /// The work area (screen minus taskbar) of the monitor the window is on.
    fn monitor_work_area(hwnd: HWND) -> Option<RECT> {
        // SAFETY: canonical monitor-info query; `mi.cbSize` set before the call.
        unsafe {
            let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
            if mon.is_null() {
                return None;
            }
            let mut mi: MONITORINFO = std::mem::zeroed();
            mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            if GetMonitorInfoW(mon, &mut mi) != 0 {
                Some(mi.rcWork)
            } else {
                None
            }
        }
    }

    /// Whether any taskbar is in auto-hide mode. A borderless window that
    /// maximally covers an auto-hide taskbar's edge prevents it from popping up;
    /// the caller insets that edge by 1px to keep it reachable.
    fn taskbar_is_autohide() -> bool {
        // SAFETY: canonical app-bar state query; `cbSize` set before the call.
        unsafe {
            let mut abd: APPBARDATA = std::mem::zeroed();
            abd.cbSize = std::mem::size_of::<APPBARDATA>() as u32;
            let state = SHAppBarMessage(ABM_GETSTATE, &mut abd) as u32;
            state & ABS_AUTOHIDE != 0
        }
    }

    /// PURE geometry decision for the maximized `WM_NCCALCSIZE` client rect.
    ///
    /// A borderless maximize would otherwise cover the taskbar, so a maximized
    /// window clamps its client rect to the monitor work area. When an auto-hide
    /// taskbar is present we leave a 1px sliver on the bottom edge (the common
    /// edge) so it can still pop up.
    ///
    /// `proposed` is the OS-proposed client rect (the full window rect); it is
    /// returned unchanged here — the caller only invokes this on the maximized
    /// path, where the work-area clamp wins. The non-maximized path returns the
    /// proposed rect unchanged and never calls this. Keeping `proposed` in the
    /// signature documents the contract and keeps the function self-describing.
    fn maximized_client_rect(proposed: RECT, work: RECT, taskbar_autohide: bool) -> RECT {
        let _ = proposed;
        let mut work = work;
        if taskbar_autohide {
            // Leave a 1px sliver on the bottom so an auto-hide taskbar (the
            // common edge) can still pop up.
            work.bottom -= 1;
        }
        work
    }

    /// The non-client message set the DWM custom-frame guidance says to hand to
    /// `DwmDefWindowProc` FIRST, so DWM can run its own caption-button behaviour
    /// (hover/press visuals and, on Windows 11, the Snap Layouts flyout) over the
    /// region we answer `HTMAXBUTTON` for.
    const DWM_CAPTION_MESSAGES: [u32; 7] = [
        WM_NCHITTEST,
        WM_NCMOUSEMOVE,
        WM_NCMOUSELEAVE,
        WM_NCLBUTTONDOWN,
        WM_NCLBUTTONUP,
        WM_NCRBUTTONDOWN,
        WM_NCRBUTTONUP,
    ];

    /// Split a `WM_NCHITTEST`-style packed `LPARAM` into signed screen
    /// coordinates. The halves are SIGNED 16-bit: a window on a monitor left of
    /// or above the primary has negative screen coordinates, and a naive `as u16`
    /// would wrap them into ~65000 and miss every hit.
    const fn lparam_to_screen_point(lparam: LPARAM) -> (i32, i32) {
        let x = (lparam & 0xFFFF) as u16 as i16 as i32;
        let y = ((lparam >> 16) & 0xFFFF) as u16 as i16 as i32;
        (x, y)
    }

    /// Gather the live geometry a hit-test decision needs. Returns `None` when the
    /// client rect cannot be read (the pure classifier is never fed garbage).
    fn current_geometry(hwnd: HWND) -> Option<HitTestGeometry> {
        // SAFETY: `hwnd` is an OS window owned by this process; a client-rect read.
        let client = unsafe {
            let mut cr: RECT = std::mem::zeroed();
            if GetClientRect(hwnd, &mut cr) == 0 {
                return None;
            }
            RectPx::new(cr.left, cr.top, cr.right, cr.bottom)
        };
        Some(HitTestGeometry {
            client,
            max_button: published_button_rect(),
            // The caption strip and resize bands are only consulted in
            // `FullNonClient`; SCR1B3 ships `MaximizeButtonOnly`, where egui owns
            // both. The defaults match `chrome.rs`'s egui-space bands so a port
            // that flips the mode gets the same geometry it already draws.
            caption_height: 34,
            border: 8,
            corner: 12,
            maximized: is_maximized(hwnd),
        })
    }

    /// Convert a screen point to physical client coordinates.
    fn screen_to_client(hwnd: HWND, x: i32, y: i32) -> Option<(i32, i32)> {
        // SAFETY: `hwnd` is an OS window owned by this process; `pt` is a live
        // local the OS writes back into.
        unsafe {
            let mut pt = POINT { x, y };
            if ScreenToClient(hwnd, &mut pt) == 0 {
                return None;
            }
            Some((pt.x, pt.y))
        }
    }

    /// Classify a packed screen-coordinate `LPARAM` against the live window.
    fn classify_lparam(hwnd: HWND, lparam: LPARAM) -> Option<HitZone> {
        let (sx, sy) = lparam_to_screen_point(lparam);
        let (cx, cy) = screen_to_client(hwnd, sx, sy)?;
        let geo = current_geometry(hwnd)?;
        Some(hit_test::classify(hit_test_mode(), &geo, cx, cy))
    }

    /// Ask for a `WM_NCMOUSELEAVE` so a pointer that leaves the maximize button
    /// clears the hover state. Without this the button would latch "hovered" the
    /// moment the pointer left the window by any path that produces no further
    /// non-client move.
    fn track_nc_mouse_leave(hwnd: HWND) {
        // SAFETY: canonical `TrackMouseEvent` call; `tme.cbSize` set before it and
        // the struct is a live local the OS only reads.
        unsafe {
            let mut tme: TRACKMOUSEEVENT = std::mem::zeroed();
            tme.cbSize = std::mem::size_of::<TRACKMOUSEEVENT>() as u32;
            tme.dwFlags = TME_LEAVE | TME_NONCLIENT;
            tme.hwndTrack = hwnd;
            TrackMouseEvent(&mut tme);
        }
    }

    /// The `WM_NCCALCSIZE` + `WM_NCHITTEST` subclass: turn the whole window into
    /// client area so the OS reserves no non-client strip, keep a maximized
    /// window inside the monitor work area, and answer `HTMAXBUTTON` over the
    /// app-published maximize button so Windows 11 offers Snap Layouts.
    unsafe extern "system" fn nc_subclass_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _id: usize,
        _ref: usize,
    ) -> LRESULT {
        // --- DWM FIRST -------------------------------------------------------
        // The MS custom-frame guidance is explicit that `DwmDefWindowProc` gets
        // the caption-button messages BEFORE the app looks at them. That call is
        // what renders the Windows 11 Snap Layouts flyout and the DWM caption
        // hover/press visuals over our region; if we classified first and
        // returned, DWM would never see the message and the flyout would not
        // paint over custom caption buttons.
        if DWM_CAPTION_MESSAGES.contains(&msg) {
            let mut dwm_result: LRESULT = 0;
            if DwmDefWindowProc(hwnd, msg, wparam, lparam, &mut dwm_result) != 0 {
                return dwm_result;
            }
        }

        match msg {
            // --- Snap Layouts: claim the maximize button ----------------------
            WM_NCHITTEST => {
                if let Some(zone) = classify_lparam(hwnd, lparam) {
                    if zone != HitZone::Client {
                        return zone.ht_code();
                    }
                }
                // Fall through to the default proc for plain client area.
            }

            // --- hover tracking ----------------------------------------------
            // Once we answer HTMAXBUTTON, Windows routes this region's input as
            // non-client, so egui's own `Response::hovered()` goes permanently
            // false there. Track it here so the app can still paint hover.
            WM_NCMOUSEMOVE => {
                let over = wparam as u32 == HTMAXBUTTON;
                BTN_HOVERED.store(over, Ordering::Relaxed);
                if over {
                    track_nc_mouse_leave(hwnd);
                }
            }
            WM_NCMOUSELEAVE => {
                BTN_HOVERED.store(false, Ordering::Relaxed);
                BTN_PRESSED.store(false, Ordering::Relaxed);
            }

            // --- clicks on the claimed region --------------------------------
            // HTMAXBUTTON means egui never sees these, so the button would be
            // dead without this. Swallow the DOWN (so the OS does not start its
            // own caption interaction) and act on the UP, which is what makes a
            // press-then-drag-away correctly cancel.
            WM_NCLBUTTONDOWN if wparam as u32 == HTMAXBUTTON => {
                BTN_PRESSED.store(true, Ordering::Relaxed);
                return 0;
            }
            WM_NCLBUTTONUP if wparam as u32 == HTMAXBUTTON => {
                if BTN_PRESSED.swap(false, Ordering::Relaxed) {
                    let cmd = if is_maximized(hwnd) {
                        SC_RESTORE
                    } else {
                        SC_MAXIMIZE
                    };
                    PostMessageW(hwnd, WM_SYSCOMMAND, cmd as WPARAM, 0);
                }
                return 0;
            }

            _ => {}
        }

        if msg == WM_NCCALCSIZE && wparam != 0 {
            // wParam == TRUE: `lparam` is `*mut NCCALCSIZE_PARAMS`. Returning 0
            // with `rgrc[0]` (the proposed client rect) left as the full window
            // rect makes the entire window client area → no NC caption strip →
            // no system min/max/close, opaque or transparent.
            if is_maximized(hwnd) {
                // A borderless maximize would otherwise cover the taskbar. Clamp
                // the client rect to the monitor work area via the pure decision.
                if let Some(work) = monitor_work_area(hwnd) {
                    let params = lparam as *mut NCCALCSIZE_PARAMS;
                    let proposed = (*params).rgrc[0];
                    (*params).rgrc[0] =
                        maximized_client_rect(proposed, work, taskbar_is_autohide());
                }
            }
            return 0;
        }
        DefSubclassProc(hwnd, msg, wparam, lparam)
    }

    /// Install the NC subclass on `hwnd` and force a frame recalculation so the
    /// new (zero) non-client area takes effect immediately. Returns whether the
    /// subclass was installed.
    fn install_nc_subclass(hwnd: isize) -> bool {
        // SAFETY: `hwnd` is an OS window owned by this process; this is the
        // canonical comctl32 subclass install + a frame-changed re-layout.
        unsafe {
            let h = hwnd as HWND;
            let ok = SetWindowSubclass(h, Some(nc_subclass_proc), SUBCLASS_ID, 0) != 0;
            if ok {
                // "The new client area is not visible until the client region
                // needs to be resized" — trigger it once.
                SetWindowPos(
                    h,
                    std::ptr::null_mut(),
                    0,
                    0,
                    0,
                    0,
                    SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            ok
        }
    }

    /// Bring the window's style into agreement with [`desired_style`], then force
    /// a frame recalculation so DWM re-evaluates what it composites.
    ///
    /// Writes ONLY when the current style differs from the desired one, so it is
    /// cheap to call every frame AND self-heals if winit re-asserts styles on a
    /// resize/restore (winit re-derives styles from its `WindowFlags`). Leaves
    /// winit's `DwmEnableBlurBehindWindow` transparency untouched — unlike the old
    /// `DWMNCRP_DISABLED` attempt, which fought it.
    ///
    /// Replaces the previous one-way `strip_caption_styles`. That version
    /// re-stripped `WS_MAXIMIZEBOX`/`WS_SYSMENU` on EVERY frame, which meant any
    /// attempt to keep the window snap-eligible was undone within one frame — the
    /// per-frame pass actively fought the setting. Reconciling to a desired style
    /// makes the per-frame pass idempotent and convergent in both directions.
    fn reconcile_styles(hwnd: isize) {
        // SAFETY: `hwnd` is an OS window owned by this process; GWL_STYLE
        // read/write + a frame-changed re-layout — the canonical borderless
        // technique (melak47/BorderlessWindow, MS DWM custom-frame sample).
        unsafe {
            let h = hwnd as HWND;
            let style = GetWindowLongPtrW(h, GWL_STYLE) as u32;
            let want = desired_style(style, SNAP_SUPPORT.load(Ordering::Relaxed));
            if want == style {
                return; // already correct — nothing to do this frame.
            }
            SetWindowLongPtrW(h, GWL_STYLE, want as isize);
            SetWindowPos(
                h,
                std::ptr::null_mut(),
                0,
                0,
                0,
                0,
                SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }

    /// Set a `u32`-valued DWM window attribute, ignoring the `HRESULT`.
    ///
    /// Every attribute this crate sets is a Windows 11 nicety (rounded corners,
    /// backdrop material). Windows 10 does not know them and returns a failing
    /// `HRESULT`; that rejection IS the clean degrade, so no version probe is
    /// needed or performed and a failure can never affect the app.
    fn set_dwm_u32_attribute(hwnd: isize, attribute: u32, value: u32) {
        // SAFETY: `hwnd` is an OS window owned by this process. `value` is a live
        // `u32` local; the size passed matches its type exactly, which is the
        // documented contract for both attributes used here. DWM copies the value
        // before returning, so the local need not outlive the call.
        unsafe {
            let v = value;
            let _ = DwmSetWindowAttribute(
                hwnd as HWND,
                attribute,
                std::ptr::addr_of!(v).cast(),
                std::mem::size_of::<u32>() as u32,
            );
        }
    }

    /// See the public wrapper. One-shot per process.
    pub fn apply_rounded_corners() {
        let hwnd = CACHED_HWND.load(Ordering::Relaxed);
        if hwnd == 0 || CORNERS_APPLIED.swap(true, Ordering::Relaxed) {
            return;
        }
        set_dwm_u32_attribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND);
    }

    /// See the public wrapper. Stores the request and applies it if the window is
    /// already known; `ensure_borderless` re-applies it once the window appears.
    pub fn set_backdrop(backdrop: Backdrop) {
        let v = match backdrop {
            Backdrop::None => BACKDROP_NONE,
            Backdrop::Mica => BACKDROP_MICA,
            Backdrop::Acrylic => BACKDROP_ACRYLIC,
            Backdrop::MicaAlt => BACKDROP_MICA_ALT,
        };
        BACKDROP.store(v, Ordering::Relaxed);
        apply_backdrop();
    }

    /// Push the currently-requested backdrop to the window, if it is known.
    fn apply_backdrop() {
        let hwnd = CACHED_HWND.load(Ordering::Relaxed);
        if hwnd == 0 {
            return;
        }
        let value = backdrop_from_u8(BACKDROP.load(Ordering::Relaxed));
        set_dwm_u32_attribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, value);
    }

    pub fn ensure_borderless() {
        let mut hwnd = CACHED_HWND.load(Ordering::Relaxed);
        if hwnd == 0 {
            // Not primed with the real eframe handle yet — fall back to the guess.
            hwnd = find_main_window();
            CACHED_HWND.store(hwnd, Ordering::Relaxed);
        }
        // Install the subclass exactly once (early frames may run before the
        // window exists; we retry each frame until it does, then stop). The
        // subclass now exists ONLY for the maximized→work-area clamp (so a
        // borderless maximize doesn't cover the taskbar); it is NOT what removes
        // the caption buttons.
        if hwnd != 0 && !SUBCLASSED.load(Ordering::Relaxed) && install_nc_subclass(hwnd) {
            SUBCLASSED.store(true, Ordering::Relaxed);
        }
        // Reconcile the window style every frame (self-healing against winit
        // re-asserting styles on a resize/restore). With snap support ON — the
        // default — this strips `WS_CAPTION` and RETAINS the snap-gating
        // `WS_SYSMENU|WS_MINIMIZEBOX|WS_MAXIMIZEBOX`; with it OFF it falls back to
        // the original full strip. Writes only on an actual difference, so the
        // per-frame cost is one `GetWindowLongPtrW`.
        if hwnd != 0 {
            reconcile_styles(hwnd);
            // Win11 niceties; both are no-ops after the first successful call
            // (corners) or when nothing is requested (backdrop defaults to
            // `DWMSBT_NONE`). Windows 10 rejects both with a failing HRESULT,
            // which is the clean degrade.
            apply_rounded_corners();
            apply_backdrop();
        }
        // One-shot diagnostic: after the subclass is installed, record whether the
        // non-client strip is actually gone (client rect == window rect). Written
        // to %TEMP%\scr1b3-caption-diag.txt so a STILL-failing fix can be debugged
        // from evidence rather than another blind guess.
        if hwnd != 0
            && SUBCLASSED.load(Ordering::Relaxed)
            && !DIAG_WRITTEN.swap(true, Ordering::Relaxed)
        {
            write_diag(hwnd);
        }
    }

    /// Read the live window state the system menu's grey-out rules need.
    fn current_window_state(hwnd: HWND) -> WindowState {
        // SAFETY: `hwnd` is an OS window owned by this process; style + state reads.
        unsafe {
            let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
            WindowState {
                maximized: style & WS_MAXIMIZE != 0,
                minimized: IsIconic(hwnd) != 0,
                resizable: style & WS_THICKFRAME != 0,
                minimizable: style & WS_MINIMIZEBOX != 0,
                maximizable: style & WS_MAXIMIZEBOX != 0,
            }
        }
    }

    /// See the public wrapper. Pops the real `GetSystemMenu` popup with per-state
    /// greying from the pure [`menu_state`] table, and posts the chosen `SC_*`.
    pub fn show_system_menu(screen_x: i32, screen_y: i32) {
        let hwnd = CACHED_HWND.load(Ordering::Relaxed);
        if hwnd == 0 {
            return;
        }
        // SAFETY: `hwnd` is an OS window owned by this process. `GetSystemMenu`
        // with `brevert = FALSE` returns a borrowed handle owned by the window
        // (never freed here). `TrackPopupMenu` with `TPM_RETURNCMD` runs a modal
        // loop and RETURNS the command instead of posting it, so nothing is
        // dispatched behind our back; we post it ourselves afterwards.
        unsafe {
            let h = hwnd as HWND;
            let menu = GetSystemMenu(h, 0);
            if menu.is_null() {
                return;
            }

            // `GetSystemMenu` hands back the DEFAULT enable state, which assumes a
            // restored, resizable window — it would offer "Maximize" on an
            // already-maximized window. Apply the real table.
            let state = current_window_state(h);
            for (item, enabled) in menu_state(state) {
                let flags = MF_BYCOMMAND | if enabled { MF_ENABLED } else { MF_GRAYED };
                EnableMenuItem(menu, item.sc_command(), flags);
            }

            let chosen = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_RIGHTBUTTON,
                screen_x,
                screen_y,
                0,
                h,
                std::ptr::null(),
            );
            // 0 = dismissed without a selection.
            if chosen != 0 {
                PostMessageW(h, WM_SYSCOMMAND, chosen as WPARAM, 0);
            }
        }
    }

    /// Build a one-line snapshot of the window's NC state. `nc_strip_gone` is the
    /// load-bearing signal: when the `WM_NCCALCSIZE` fix works, the client rect
    /// equals the full window rect (no reserved caption strip).
    fn nc_state_line(hwnd: isize) -> String {
        // SAFETY: `hwnd` is an OS window owned by this process; rect + style reads.
        unsafe {
            let h = hwnd as HWND;
            let mut wr: RECT = std::mem::zeroed();
            let mut cr: RECT = std::mem::zeroed();
            let gw = GetWindowRect(h, &mut wr);
            let gc = GetClientRect(h, &mut cr);
            let win = (wr.right - wr.left, wr.bottom - wr.top);
            let cli = (cr.right - cr.left, cr.bottom - cr.top);
            let nc_gone = gw != 0 && gc != 0 && win == cli;
            let style = GetWindowLongPtrW(h, GWL_STYLE) as u32;
            // The load-bearing signal post-fix: with the caption-button styles
            // cleared, DWM draws no native min/max/close. `nc_strip_gone` is now
            // secondary (it never governed the DWM-composited buttons).
            let caption_btn_styles = if caption_button_styles_present(style) {
                "present"
            } else {
                "stripped"
            };
            format!(
                "scr1b3 caption diag: hwnd=0x{hwnd:x} subclassed={} style=0x{style:08x} \
                 caption_btn_styles={caption_btn_styles} win={}x{} client={}x{} \
                 nc_strip_gone={nc_gone}",
                SUBCLASSED.load(Ordering::Relaxed),
                win.0,
                win.1,
                cli.0,
                cli.1
            )
        }
    }

    /// Write the diagnostic line to `%TEMP%\scr1b3-caption-diag.txt` (best-effort).
    fn write_diag(hwnd: isize) {
        use std::io::Write;
        let path = std::env::temp_dir().join("scr1b3-caption-diag.txt");
        if let Ok(mut f) = std::fs::File::create(&path) {
            let _ = writeln!(f, "{}", nc_state_line(hwnd));
        }
    }

    #[cfg(all(windows, test))]
    mod tests {
        use super::*;

        /// Smoke: the reduced-motion query completes and returns a bool without
        /// panicking against the real OS (the value depends on the host's
        /// accessibility setting, so we assert it runs, not which way it lands).
        #[test]
        fn os_reduced_motion_query_runs() {
            let v = os_reduced_motion();
            assert!(v == v, "returns a definite bool");
        }

        fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
            RECT {
                left,
                top,
                right,
                bottom,
            }
        }

        #[test]
        fn maximized_clamps_to_work_area() {
            // Proposed = full monitor; work area reserves the bottom (taskbar).
            let proposed = rect(0, 0, 1920, 1080);
            let work = rect(0, 0, 1920, 1040);
            let out = maximized_client_rect(proposed, work, false);
            // No inset when the taskbar is not auto-hide: returns the work area.
            assert_eq!(out.left, work.left);
            assert_eq!(out.top, work.top);
            assert_eq!(out.right, work.right);
            assert_eq!(out.bottom, work.bottom);
        }

        #[test]
        fn autohide_taskbar_gets_one_px_inset() {
            let proposed = rect(0, 0, 1920, 1080);
            let work = rect(0, 0, 1920, 1080);
            let out = maximized_client_rect(proposed, work, true);
            // Bottom inset by exactly 1px; other edges identical to the work area.
            assert_eq!(out.bottom, work.bottom - 1);
            assert_eq!(out.left, work.left);
            assert_eq!(out.top, work.top);
            assert_eq!(out.right, work.right);
        }

        #[test]
        fn multi_monitor_offset_preserved() {
            // A second monitor placed left of / above the primary: negative origin.
            let proposed = rect(-1920, -120, 0, 960);
            let work = rect(-1920, -120, 0, 920);
            let out = maximized_client_rect(proposed, work, false);
            // Offset coordinates carried through exactly, no inset.
            assert_eq!(out.left, -1920);
            assert_eq!(out.top, -120);
            assert_eq!(out.right, 0);
            assert_eq!(out.bottom, 920);
        }

        #[test]
        fn autohide_inset_preserves_offset_edges() {
            // Auto-hide inset on an offset monitor still only touches `bottom`.
            let proposed = rect(-1920, -120, 0, 960);
            let work = rect(-1920, -120, 0, 920);
            let out = maximized_client_rect(proposed, work, true);
            assert_eq!(out.left, -1920);
            assert_eq!(out.top, -120);
            assert_eq!(out.right, 0);
            assert_eq!(out.bottom, 919);
        }

        /// A typical winit undecorated style: the caption-button bits set, plus an
        /// unrelated bit (`WS_MAXIMIZE` = the maximized STATE, not a style we own)
        /// that must survive every transformation.
        const WINIT_UNDECORATED: u32 =
            WS_CAPTION | WS_SYSMENU | WS_MAXIMIZEBOX | WS_MINIMIZEBOX | WS_MAXIMIZE;

        #[test]
        fn snap_mode_retains_the_snap_gating_styles() {
            let want = desired_style(WINIT_UNDECORATED, true);
            // THE regression this fix exists for: stripping WS_MAXIMIZEBOX is what
            // kills Aero Snap / Win+Arrow / drag-to-top-maximize / Snap Assist and
            // the Win11 Snap Layouts flyout. It must survive.
            assert_eq!(
                want & WS_MAXIMIZEBOX,
                WS_MAXIMIZEBOX,
                "WS_MAXIMIZEBOX gates Aero Snap and Snap Layouts — never strip it"
            );
            assert_eq!(want & WS_SYSMENU, WS_SYSMENU, "WS_SYSMENU must survive");
            assert_eq!(
                want & WS_MINIMIZEBOX,
                WS_MINIMIZEBOX,
                "WS_MINIMIZEBOX gates Win+Down and the taskbar minimize animation"
            );
            // WS_CAPTION is the actual caption-strip bit and is still stripped.
            assert_eq!(want & WS_CAPTION, 0, "WS_CAPTION must still be cleared");
            // The unrelated state bit is untouched.
            assert_eq!(want & WS_MAXIMIZE, WS_MAXIMIZE);
        }

        #[test]
        fn snap_disabled_falls_back_to_the_full_strip() {
            let want = desired_style(WINIT_UNDECORATED, false);
            assert!(
                !caption_button_styles_present(want),
                "with snap support off, every caption-button bit is cleared"
            );
            assert_eq!(want & WS_MAXIMIZE, WS_MAXIMIZE, "state bit preserved");
        }

        #[test]
        fn desired_style_is_idempotent_in_both_modes() {
            // The per-frame pass must converge, not oscillate: applying the
            // decision to its own output changes nothing.
            for snap in [true, false] {
                let once = desired_style(WINIT_UNDECORATED, snap);
                assert_eq!(
                    desired_style(once, snap),
                    once,
                    "desired_style must be idempotent (snap={snap})"
                );
            }
        }

        #[test]
        fn enabling_snap_restores_previously_stripped_styles() {
            // The bug the reconcile replaces: the old one-way strip could only
            // CLEAR bits, so once a frame had removed WS_MAXIMIZEBOX nothing ever
            // put it back and snap support could never actually turn on.
            let stripped = desired_style(WINIT_UNDECORATED, false);
            assert_eq!(stripped & WS_MAXIMIZEBOX, 0, "precondition: bit is gone");

            let restored = desired_style(stripped, true);
            assert_eq!(
                restored & SNAP_RETAINED_STYLES,
                SNAP_RETAINED_STYLES,
                "flipping snap support ON must RESTORE the snap-gating bits"
            );
        }

        #[test]
        fn styles_to_strip_never_includes_a_snap_gating_bit_in_snap_mode() {
            assert_eq!(
                styles_to_strip(true) & SNAP_RETAINED_STYLES,
                0,
                "snap mode must not strip any snap-gating bit"
            );
            assert_eq!(styles_to_strip(true), WS_CAPTION);
            assert_eq!(styles_to_strip(false), CAPTION_BUTTON_STYLES);
        }

        #[test]
        fn hit_zone_ht_codes_match_the_windows_sys_constants() {
            // `hit_test.rs` spells the HT* values out to stay platform-independent
            // and documents that a windows-only test asserts they equal the real
            // constants. This is that test — it is what makes the drift impossible.
            use windows_sys::Win32::UI::WindowsAndMessaging::{
                HTBOTTOM, HTBOTTOMLEFT, HTBOTTOMRIGHT, HTCAPTION, HTCLIENT, HTLEFT, HTRIGHT, HTTOP,
                HTTOPLEFT, HTTOPRIGHT,
            };
            let cases: [(HitZone, u32); 11] = [
                (HitZone::Client, HTCLIENT),
                (HitZone::Caption, HTCAPTION),
                (HitZone::MaxButton, HTMAXBUTTON),
                (HitZone::Left, HTLEFT),
                (HitZone::Right, HTRIGHT),
                (HitZone::Top, HTTOP),
                (HitZone::TopLeft, HTTOPLEFT),
                (HitZone::TopRight, HTTOPRIGHT),
                (HitZone::Bottom, HTBOTTOM),
                (HitZone::BottomLeft, HTBOTTOMLEFT),
                (HitZone::BottomRight, HTBOTTOMRIGHT),
            ];
            for (zone, want) in cases {
                assert_eq!(zone.ht_code(), want as isize, "{zone:?} HT code drift");
            }
        }

        #[test]
        fn sys_menu_commands_match_the_windows_sys_constants() {
            // The counterpart drift guard promised by `system_menu.rs`.
            use crate::system_menu::SysMenuItem;
            use windows_sys::Win32::UI::WindowsAndMessaging::{
                SC_CLOSE, SC_MINIMIZE, SC_MOVE, SC_SIZE,
            };
            let cases: [(SysMenuItem, u32); 6] = [
                (SysMenuItem::Size, SC_SIZE),
                (SysMenuItem::Move, SC_MOVE),
                (SysMenuItem::Minimize, SC_MINIMIZE),
                (SysMenuItem::Maximize, SC_MAXIMIZE),
                (SysMenuItem::Close, SC_CLOSE),
                (SysMenuItem::Restore, SC_RESTORE),
            ];
            for (item, want) in cases {
                assert_eq!(item.sc_command(), want, "{item:?} SC command drift");
            }
        }

        #[test]
        fn lparam_screen_point_handles_negative_coordinates() {
            // A monitor left of / above the primary produces NEGATIVE screen
            // coordinates. A naive `as u16` read would wrap -8 into 65528 and miss
            // every hit on that monitor.
            let pack = |x: i16, y: i16| -> LPARAM {
                ((x as u16 as u32) | ((y as u16 as u32) << 16)) as LPARAM
            };
            assert_eq!(lparam_to_screen_point(pack(930, 20)), (930, 20));
            assert_eq!(lparam_to_screen_point(pack(-8, -120)), (-8, -120));
            assert_eq!(lparam_to_screen_point(pack(-1920, 540)), (-1920, 540));
            assert_eq!(lparam_to_screen_point(pack(0, 0)), (0, 0));
        }

        #[test]
        fn backdrop_encoding_round_trips_to_the_dwm_values() {
            // DWMSBT_NONE is 1, NOT 0 (0 is DWMSBT_AUTO, which would let DWM pick
            // a material we never asked for).
            assert_eq!(backdrop_from_u8(BACKDROP_NONE), 1);
            assert_eq!(backdrop_from_u8(BACKDROP_MICA), 2);
            assert_eq!(backdrop_from_u8(BACKDROP_ACRYLIC), 3);
            assert_eq!(backdrop_from_u8(BACKDROP_MICA_ALT), 4);
            // An out-of-range discriminant degrades to "no material", never AUTO.
            assert_eq!(backdrop_from_u8(200), 1);
        }

        #[test]
        fn published_button_rect_rejects_empty_and_inverted_rects() {
            // Guards the "a stale publish can only cost the flyout, never
            // mis-claim a region" contract.
            set_maximize_button_rect(RectPx::new(908, 0, 954, 32));
            assert_eq!(
                published_button_rect(),
                Some(RectPx::new(908, 0, 954, 32)),
                "a real rect is published"
            );

            set_maximize_button_rect(RectPx::default());
            assert_eq!(published_button_rect(), None, "a zeroed rect is absent");

            set_maximize_button_rect(RectPx::new(500, 500, 100, 100));
            assert_eq!(published_button_rect(), None, "an inverted rect is absent");

            // Retracting the rect must also drop a latched hover state.
            BTN_HOVERED.store(true, Ordering::Relaxed);
            set_maximize_button_rect(RectPx::default());
            assert!(
                !maximize_button_hovered(),
                "retracting the rect clears hover"
            );
        }

        #[test]
        fn hit_test_mode_round_trips_through_the_atomic() {
            set_hit_test_mode(HitTestMode::FullNonClient);
            assert_eq!(hit_test_mode(), HitTestMode::FullNonClient);
            set_hit_test_mode(HitTestMode::MaximizeButtonOnly);
            assert_eq!(hit_test_mode(), HitTestMode::MaximizeButtonOnly);
            // The default (and any unknown byte) is the conservative mode, where
            // egui keeps drag + resize.
            HIT_TEST_MODE.store(200, Ordering::Relaxed);
            assert_eq!(hit_test_mode(), HitTestMode::MaximizeButtonOnly);
            HIT_TEST_MODE.store(MODE_MAX_BUTTON_ONLY, Ordering::Relaxed);
        }

        #[test]
        fn dwm_gets_the_caption_messages_before_we_classify() {
            // The Snap Layouts flyout is rendered by DWM, so DwmDefWindowProc must
            // see these messages FIRST. This asserts the routing table itself —
            // the ordering inside the window proc is a code-structure invariant
            // that cannot be exercised without a real HWND (see the crate docs).
            for msg in [
                WM_NCHITTEST,
                WM_NCMOUSEMOVE,
                WM_NCMOUSELEAVE,
                WM_NCLBUTTONDOWN,
                WM_NCLBUTTONUP,
            ] {
                assert!(
                    DWM_CAPTION_MESSAGES.contains(&msg),
                    "message {msg:#x} must be routed to DwmDefWindowProc first"
                );
            }
            // WM_NCCALCSIZE is deliberately NOT in the set: DWM does not handle it
            // and our work-area clamp must always run.
            assert!(!DWM_CAPTION_MESSAGES.contains(&WM_NCCALCSIZE));
        }
    }
}
