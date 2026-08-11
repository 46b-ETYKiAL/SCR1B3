//! The Windows-only implementation half of this crate.
//!
//! Split out of `lib.rs` verbatim so the ENTIRE file is governed by the single
//! `#[cfg(windows)] mod imp;` declaration in the crate root. That one-line
//! premise is what makes the mutation-gate exclusion in
//! `.github/workflows/ci.yml` honest: on the ubuntu mutation runner none of
//! this file is compiled, so every mutant planted in it is a 100% false
//! positive that no test on that host could ever kill. The premise is pinned by
//! `tests::the_windows_only_module_is_cfg_gated_so_the_mutation_exclusion_stays_honest`
//! in `lib.rs`, so it cannot rot into a real blind spot.
//!
//! Its own coverage is the `#[cfg(all(windows, test))] mod tests` at the bottom
//! of this file, which runs on every Windows build and in the Windows leg of
//! the `build & test` CI matrix.

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
    DefSubclassProc, SHAppBarMessage, SHChangeNotify, SetWindowSubclass, ABM_GETSTATE,
    ABS_AUTOHIDE, APPBARDATA, SHCNE_ASSOCCHANGED, SHCNF_IDLIST,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, EnableMenuItem, EnumWindows, GetClientRect, GetSystemMenu,
    GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
    PostMessageW, SetWindowLongPtrW, SetWindowPos, TrackPopupMenu, GWL_STYLE, HTMAXBUTTON,
    MF_BYCOMMAND, MF_ENABLED, MF_GRAYED, NCCALCSIZE_PARAMS, SC_MAXIMIZE, SC_RESTORE,
    SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, WM_NCCALCSIZE, WM_NCHITTEST, WM_NCLBUTTONDOWN, WM_NCLBUTTONUP,
    WM_NCMOUSELEAVE, WM_NCMOUSEMOVE, WM_NCRBUTTONDOWN, WM_NCRBUTTONUP, WM_SYSCOMMAND, WS_CAPTION,
    WS_MAXIMIZE, WS_MAXIMIZEBOX, WS_MINIMIZEBOX, WS_SYSMENU, WS_THICKFRAME,
};

use crate::hit_test::{self, HitTestGeometry, HitTestMode, HitZone, RectPx};
use crate::system_menu::{menu_state, WindowState};
use crate::Backdrop;

/// Query `SPI_GETCLIENTAREAANIMATION`. The BOOL out-param is `TRUE` when
/// animations are ON; reduced-motion is the negation. A failed call (returns
/// 0) is treated as "not reduced" so a query error never suppresses motion.
pub fn os_reduced_motion() -> bool {
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

/// Broadcast `SHCNE_ASSOCCHANGED` so the shell re-reads file associations.
///
/// ## Why the item pointers are null, and why `SHCNF_IDLIST`
///
/// `SHCNE_ASSOCCHANGED` is one of the events Microsoft documents as taking NO
/// item: "`dwItem1` and `dwItem2` are not used and must be `NULL`" — the event
/// says *"associations changed"*, not *"this one file changed"*, so there is no
/// item to name. `SHCNF_IDLIST` (`0`) is the flag the docs pair with it: the
/// flags word tells `SHChangeNotify` how to INTERPRET `dwItem1`/`dwItem2`, and
/// `SHCNF_IDLIST` means "they are PIDLs". Two null PIDLs is the well-formed way
/// to say "no item", and it is the pairing the documented `SHCNE_ASSOCCHANGED`
/// usage (and every shell-integration implementation that follows it) uses. The
/// `PATH`/`PRINTER` flag variants would promise string or printer arguments we
/// are not passing, so they are wrong here even though the pointers are null
/// either way.
///
/// ## It is fire-and-forget — there is no success to report
///
/// `SHChangeNotify` returns `()`. It sets no last-error, gives no handle, and
/// hands the event to the shell's own notification queue, which delivers it to
/// listeners asynchronously. **Whether Explorer actually refreshed is not
/// observable from this process**, so this function has no return value and no
/// caller can branch on whether it "worked" — inventing a boolean here would be
/// a fabricated success signal, not a measurement.
pub fn notify_assoc_changed() {
    // SAFETY: `SHChangeNotify` with `SHCNE_ASSOCCHANGED` takes no item
    // arguments — MS documents `dwItem1`/`dwItem2` as unused and required to be
    // NULL for this event — so nothing is borrowed, owned, aliased, or required
    // to outlive the call, and there is no buffer for the callee to write into.
    // The other two arguments are compile-time constants. The cast is
    // width-safe: `SHCNE_ASSOCCHANGED` is `0x0800_0000` (134_217_728), well
    // inside `i32::MAX`, and only exists because windows-sys types the constant
    // as `u32` while the parameter is `i32`.
    unsafe {
        SHChangeNotify(
            SHCNE_ASSOCCHANGED as i32,
            SHCNF_IDLIST,
            std::ptr::null(),
            std::ptr::null(),
        );
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
                (*params).rgrc[0] = maximized_client_rect(proposed, work, taskbar_is_autohide());
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

    /// Smoke: the association-change broadcast links against the real
    /// `shell32.dll` export and completes without panicking or aborting.
    ///
    /// This is the CEILING of what is testable here, and it is deliberately not
    /// dressed up as more. `SHChangeNotify` returns nothing and sets no
    /// last-error, so there is no success to assert: this proves the symbol
    /// resolves and the null-PIDL + `SHCNF_IDLIST` argument shape is accepted,
    /// NOT that Explorer refreshed. The DECISION of when to call it is tested
    /// where it lives, in `scribe-app`'s `windows_entries::shell_notify_tests`.
    ///
    /// It does fire a real event at the host shell. That is harmless and
    /// self-healing — an association re-read changes no state of ours — and it
    /// is the only way to exercise the shipped call rather than a stand-in.
    #[test]
    fn notify_assoc_changed_reaches_the_real_shell_export() {
        notify_assoc_changed();
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
