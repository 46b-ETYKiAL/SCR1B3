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
// The Windows body is RE-EXPORTED from `imp` rather than wrapped here.
// A `#[cfg(windows)] pub fn` wrapper is not compiled on the ubuntu mutation
// runner, so every mutant of it is a vacuous MISS — but it shares its NAME
// with the `#[cfg(not(windows))]` stub below, whose mutants are REAL and are
// caught by `off_windows_the_query_stubs_answer_false`. A name-keyed
// exclusion could not separate the two and would have silenced the real one.
// Re-exporting moves the Windows definition into `imp.rs`, which is already
// excluded as a whole file, and leaves the compiled stub under full coverage.
#[cfg(windows)]
pub use imp::os_reduced_motion;

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
// The Windows body is RE-EXPORTED from `imp` rather than wrapped here.
// A `#[cfg(windows)] pub fn` wrapper is not compiled on the ubuntu mutation
// runner, so every mutant of it is a vacuous MISS — but it shares its NAME
// with the `#[cfg(not(windows))]` stub below, whose mutants are REAL and are
// caught by `off_windows_the_query_stubs_answer_false`. A name-keyed
// exclusion could not separate the two and would have silenced the real one.
// Re-exporting moves the Windows definition into `imp.rs`, which is already
// excluded as a whole file, and leaves the compiled stub under full coverage.
#[cfg(windows)]
pub use imp::maximize_button_hovered;

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

/// The Windows-only implementation, in its own file so a single `#[cfg(windows)]`
/// governs all of it — see `imp.rs` for why that matters to the mutation gate.
#[cfg(windows)]
mod imp;

/// Cross-platform tests for the crate root.
///
/// `imp.rs` has its own `#[cfg(all(windows, test))] mod tests`; this module is
/// the half that runs on EVERY host — which is the half the ubuntu mutation
/// runner can actually use.
#[cfg(test)]
mod tests {
    /// The Windows-only public wrappers whose mutants the ubuntu gate excludes.
    /// On that runner the function is not compiled, so mutating it changes
    /// nothing that builds and every mutant is a 100% false MISS. This list
    /// MIRRORS ci.yml — the tests below fail if the premise behind any entry
    /// stops holding, so the exclusion cannot rot into a real blind spot.
    ///
    /// Keyed by function NAME, deliberately not by line number. A line-number
    /// key rotates: insert one comment anywhere above and every entry points at
    /// a different function. Most such shifts would land on the opposite `#[cfg]`
    /// and fail loudly, but a shift that happens to land on a same-gated
    /// neighbour would keep passing while silently checking the wrong function —
    /// a guard that reads green having verified nothing.
    const MUTATION_EXCLUDED_WRAPPERS: [&str; 10] = [
        "ensure_caption_stripped",
        "set_main_hwnd",
        "allow_foreground_handoff",
        "set_maximize_button_rect",
        "clear_maximize_button_rect",
        "set_hit_test_mode",
        "set_snap_support_enabled",
        "show_system_menu",
        "apply_rounded_corners",
        "set_backdrop",
    ];

    /// The `#[cfg(not(windows))]` stubs. These ARE compiled on the ubuntu
    /// runner, so their mutants are real signal and must never be excluded —
    /// `off_windows_the_query_stubs_answer_false` is what kills them.
    ///
    /// Their Windows halves are RE-EXPORTS (`#[cfg(windows)] pub use imp::NAME;`),
    /// NOT wrappers, so they are deliberately absent from
    /// [`MUTATION_EXCLUDED_WRAPPERS`]. That is the whole point: a name-keyed
    /// exclusion cannot tell a vacuous `#[cfg(windows)]` wrapper from the REAL
    /// stub that shares its name, so excluding these two by name would have
    /// silenced mutants that are currently CAUGHT. Re-exporting puts the
    /// Windows definition inside the already-excluded `imp.rs` instead.
    const STUBS_THAT_MUST_STAY_GATED: [&str; 2] = ["os_reduced_motion", "maximize_button_hovered"];

    /// Every `#[cfg(..)]` attribute governing a top-level `fn <name>` in `src`.
    ///
    /// Returns one entry per definition, so a cfg-PAIR yields both halves and a
    /// caller can assert the pair is exactly `{windows, not(windows)}`.
    fn governing_cfgs_for(src: &str, name: &str) -> Vec<String> {
        let lines: Vec<&str> = src.split('\n').collect();
        let sig = format!("pub fn {name}(");
        let mut found = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            if !line.starts_with(&sig) {
                continue;
            }
            // Walk back over the attribute/doc-comment block to the `#[cfg(..)]`.
            let mut j = i;
            while j > 0 {
                j -= 1;
                let l = lines[j];
                if l.starts_with("#[cfg(") {
                    found.push(l.to_string());
                    break;
                }
                if !(l.starts_with("#[") || l.starts_with("//")) {
                    break;
                }
            }
        }
        assert!(
            !found.is_empty(),
            "no `#[cfg(..)]`-governed `pub fn {name}` found — the list in this \
             module names a function that no longer exists (or was renamed); \
             update it and .github/workflows/ci.yml together"
        );
        found
    }

    /// The premise behind the whole-file mutation exclusion of `imp.rs`.
    ///
    /// The gate runs on ubuntu. `imp.rs` is reached ONLY through this one
    /// `#[cfg(windows)] mod imp;`, so on that host none of its ~1,100 lines are
    /// compiled: mutating any of them changes nothing that builds, no test can
    /// fail, and every mutant reports MISSED. Excluding the file drops 100%
    /// false positives — but ONLY while that gate holds. Make the module
    /// cross-platform and its mutants become real signal, at which point the
    /// `--exclude` in `.github/workflows/ci.yml` would start hiding genuine
    /// gaps. Fail here so that cannot happen quietly.
    #[test]
    fn the_windows_only_module_is_cfg_gated_so_the_mutation_exclusion_stays_honest() {
        let src = include_str!("lib.rs");
        // ASSEMBLED, never written as a literal. This test reads its OWN file,
        // so a literal needle would sit in the source and `matches` would count
        // the needle itself — passing no matter what the real declaration said.
        let needle = ["#[cfg(", "windows", ")]\n", "mod imp;"].concat();
        assert_eq!(
            src.matches(needle.as_str()).count(),
            1,
            "`mod imp;` is no longer exactly `#[cfg(windows)]`-gated (or this \
             test now self-matches). If that module compiles off Windows its \
             mutants are real signal: DROP the `--exclude \
             '**/scribe-win32-chrome/src/imp.rs'` from the mutation job in \
             .github/workflows/ci.yml rather than leaving a blind spot."
        );
    }

    #[test]
    fn every_mutation_excluded_wrapper_has_a_windows_only_body() {
        let src = include_str!("lib.rs");
        for name in MUTATION_EXCLUDED_WRAPPERS {
            let cfgs = governing_cfgs_for(src, name);
            assert!(
                cfgs.iter().any(|c| c == "#[cfg(windows)]"),
                "ci.yml excludes `{name}` from the ubuntu mutation gate on the \
                 premise that its body is Windows-only code that runner never \
                 compiles — and it is not (any more): {cfgs:?}. Fix the list HERE \
                 and in .github/workflows/ci.yml together, or drop the exclusion: \
                 a cross-platform body's mutants are real signal."
            );
        }
    }

    #[test]
    fn the_non_windows_stubs_are_never_mutation_excluded() {
        let src = include_str!("lib.rs");
        for name in STUBS_THAT_MUST_STAY_GATED {
            let cfgs = governing_cfgs_for(src, name);
            // The stub half must still exist and still be off-Windows-gated.
            assert!(
                cfgs.iter().any(|c| c == "#[cfg(not(windows))]"),
                "`{name}` no longer has a `#[cfg(not(windows))]` stub: {cfgs:?}. \
                 The exclusion's honesty depends on the pair staying split — the \
                 stub is the half the ubuntu runner COMPILES, and \
                 `off_windows_the_query_stubs_answer_false` is what kills its \
                 mutants."
            );
            // The stub must NOT also have a `#[cfg(windows)] pub fn` twin here.
            // Such a twin is not compiled on the ubuntu runner, so all of its
            // mutants are vacuous MISSES — and because it shares this name, no
            // name-keyed exclusion could suppress them without ALSO suppressing
            // the stub's mutants, which are real and currently caught. The
            // Windows half therefore lives in the already-excluded `imp.rs` and
            // is surfaced by re-export.
            assert!(
                !cfgs.iter().any(|c| c == "#[cfg(windows)]"),
                "`{name}` grew a `#[cfg(windows)] pub fn` twin in lib.rs: {cfgs:?}. \
                 That twin's mutants are vacuous on the ubuntu gate and cannot be \
                 excluded by name without silencing the stub's REAL ones. Put the \
                 Windows body in `imp.rs` and re-export it instead."
            );

            // …and the re-export must actually be there, or the Windows build
            // has no definition at all.
            let reexport = format!("#[cfg(windows)]\npub use imp::{name};");
            assert!(
                src.contains(&reexport),
                "`{name}` has no `#[cfg(windows)] pub use imp::{name};` re-export — \
                 the Windows target would have no definition for it."
            );
        }
    }

    #[test]
    fn every_cfg_pair_in_this_file_is_accounted_for_by_the_exclusion_list() {
        // The rot guard with teeth: a NEW `#[cfg(windows)]` wrapper would add
        // vacuous MISSES that hold the ubuntu gate permanently red, and a new
        // `#[cfg(not(windows))]` stub would add real mutants that need killing.
        // Either way the author must revisit ci.yml — so make adding one fail
        // here until they do.
        let src = include_str!("lib.rs");
        let windows_gated = src.lines().filter(|l| *l == "#[cfg(windows)]").count();
        let stub_gated = src.lines().filter(|l| *l == "#[cfg(not(windows))]").count();
        assert_eq!(
            stub_gated, 12,
            "the number of off-Windows stubs changed; each new one carries a \
             REAL mutant that needs a killing assertion in this module"
        );
        // Every `#[cfg(windows)]` in this file is accounted for by exactly one of
        // three roles, so a NEW one cannot appear without failing here:
        //   * one per excluded wrapper (vacuous on ubuntu — excluded by name),
        //   * one on `mod imp;` (the whole file is excluded),
        //   * one per re-exported stub pair (`pub use imp::NAME;`) — the Windows
        //     definition lives inside the already-excluded `imp.rs`, which is how
        //     those two avoid needing a name-keyed exclusion that would also
        //     silence their compiled stubs.
        let reexported = STUBS_THAT_MUST_STAY_GATED.len();
        assert_eq!(
            windows_gated,
            MUTATION_EXCLUDED_WRAPPERS.len() + 1 + reexported,
            "expected one `#[cfg(windows)]` per excluded wrapper ({}), plus the one \
             on `mod imp;`, plus one per re-exported stub pair ({reexported}); \
             update BOTH this list and ci.yml",
            MUTATION_EXCLUDED_WRAPPERS.len()
        );
    }

    /// Kills the only two mutants in this file that the ubuntu mutation runner
    /// can actually reach: `os_reduced_motion -> true` (lib.rs:201) and
    /// `maximize_button_hovered -> true` (lib.rs:301). Both stubs return
    /// `false`, and both `false`s are load-bearing: a `true` from the first
    /// silently disables every animation in the app off Windows, and a `true`
    /// from the second pins the maximize button in its hover fill forever.
    #[cfg(not(windows))]
    #[test]
    fn off_windows_the_query_stubs_answer_false() {
        assert!(
            !super::os_reduced_motion(),
            "off Windows no OS accessibility signal is consulted, so \
             reduced-motion must be `false` and the in-app motion toggle decides \
             on its own"
        );
        assert!(
            !super::maximize_button_hovered(),
            "off Windows `WM_NCHITTEST` never runs, so the maximize button is \
             never reported as OS-hovered"
        );
    }
}
