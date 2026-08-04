//! OS default-app / file-association registration.
//!
//! Registers SCR1B3 as a handler for the user's chosen [`ClaimType`]s and, where
//! the OS allows it, sets it as the default. Every backend uses OS built-ins
//! (`reg.exe` on Windows, `xdg-mime` / `mimeapps.list` on Linux) or a documented
//! manual path (macOS) — **no FFI, no `unsafe`**, so `scribe-app` keeps its
//! crate-level `#![forbid(unsafe_code)]`.
//!
//! The hard per-OS reality:
//! - **Windows** — you CANNOT silently flip the default (UserChoice is hash-
//!   protected). We register the ProgID + `OpenWithProgids` + `Capabilities` +
//!   `RegisteredApplications` under `HKCU` (no admin) so SCR1B3 becomes a
//!   first-class choice, then deep-link the user to the Default Apps UI to
//!   confirm. Same constraint VS Code / Notepad++ live under.
//! - **Linux** — `xdg-mime default` sets it silently, per-user, no root.
//! - **macOS** — the bundle's `Info.plist` declares the document types; the user
//!   sets the default via Finder ▸ Get Info ▸ Open With ▸ Change All (no
//!   third-party CLI / no `unsafe` objc2 call from this `forbid(unsafe_code)` crate).
//!   Because there is no write to perform, `register` VERIFIES the running
//!   bundle's declaration and reports only the groups it really finds — it never
//!   asserts a registration it did not perform (see `macos::summarize_bundle`).

use scribe_core::config::ClaimType;

/// The outcome of a registration attempt, surfaced by the Settings UI.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RegisterReport {
    /// [`ClaimType::key`]s that registered successfully.
    pub registered: Vec<String>,
    /// `(key, error)` for groups that failed to register.
    pub failed: Vec<(String, String)>,
    /// The OS needs the user to finish in a system UI (Windows: confirm in the
    /// Default Apps window; macOS: pick SCR1B3 in Finder ▸ Get Info). The Settings
    /// UI surfaces the follow-up step when this is set.
    pub needs_user_action: bool,
    /// A short, user-facing status / next-step message.
    pub message: String,
}

// `linux` compiles on every OS under `test` so its pure `mimeapps.list` editor +
// tests run on all hosts; the runtime parts inside it are `cfg(target_os =
// "linux")`-gated.
#[cfg(any(test, target_os = "linux"))]
mod linux;
// `macos` compiles on every OS under `test` for the same reason `linux` does:
// its decision half (`summarize_bundle`) is pure, so the fake-success regression
// it fixes is pinned by tests that run on ALL hosts, not only a mac runner.
#[cfg(any(test, target_os = "macos"))]
mod macos;
#[cfg(windows)]
mod windows;

#[cfg(any(test, windows))]
pub(crate) mod windows_entries;

/// Register SCR1B3 as a handler for `types` and, where the OS permits, set it as
/// the default. Returns a [`RegisterReport`] for the Settings UI. Never panics —
/// a backend failure is reported, not raised.
pub fn register(types: &[ClaimType]) -> RegisterReport {
    #[cfg(windows)]
    {
        windows::register(types)
    }
    #[cfg(target_os = "linux")]
    {
        linux::register(types)
    }
    #[cfg(target_os = "macos")]
    {
        macos::register(types)
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        let _ = types;
        RegisterReport {
            message: "Setting the default app isn't supported on this platform.".into(),
            ..Default::default()
        }
    }
}

/// Pure decision for [`reregister_on_startup`]: refresh the associations only when
/// the user opted in AND there is at least one claimed type. Extracted so the
/// opt-in contract is unit-testable without a real registry — the reviewer's exact
/// complaint was that `register_file_types` was written but read by no production
/// code, so this pins that it IS read and honoured.
fn startup_reregister_types(opted_in: bool, claimed: &[ClaimType]) -> bool {
    opted_in && !claimed.is_empty()
}

/// Re-register the file associations at STARTUP, silently, when the user has
/// previously opted in (`config.integration.register_file_types`). This is the
/// call site the opt-in flag exists for: without it the flag was written by the
/// Settings toggle and never read, so a user who opted in got nothing on the next
/// launch — and, because Windows bakes the absolute exe path into the association
/// keys, an in-app update or a portable-zip move left every registered command
/// pointing at the OLD path. Re-running silently on each launch refreshes that
/// path. Never opens a Settings window (that would be hostile on every start) and
/// never blocks startup — a backend failure is logged, not raised.
///
/// Windows-only in effect: on Linux the associations live in the package-managed
/// `.desktop` file and on macOS in the app bundle's `Info.plist`, neither of which
/// a per-launch call should churn, so this is a no-op there.
pub fn reregister_on_startup(config: &scribe_core::config::IntegrationConfig) {
    let types = config.claimed_types();
    if !startup_reregister_types(config.register_file_types, &types) {
        return;
    }
    #[cfg(windows)]
    {
        let report = windows::register_silent(&types);
        if report.failed.is_empty() {
            tracing::debug!(
                target: "scribe::integration",
                count = report.registered.len(),
                "refreshed file associations at startup"
            );
        } else {
            // A failed silent re-register is not fatal — the user can re-run it
            // from Settings. Record it so a persistent failure is diagnosable
            // rather than silently swallowed.
            tracing::warn!(
                target: "scribe::integration",
                failures = report.failed.len(),
                "startup file-association refresh had failures"
            );
        }
    }
    #[cfg(not(windows))]
    {
        let _ = types;
    }
}

#[cfg(test)]
mod startup_reregister_tests {
    use super::startup_reregister_types;
    use scribe_core::config::ClaimType;

    /// The opt-in flag MUST gate the startup refresh. The reviewer found
    /// `register_file_types` was written by Settings but read by no production
    /// code; this pins that it is now consulted — opted-out never refreshes.
    #[test]
    fn opted_out_never_reregisters() {
        assert!(!startup_reregister_types(false, &[ClaimType::PlainText]));
        assert!(!startup_reregister_types(false, &ClaimType::ALL));
    }

    /// Opted in with at least one claimed type DOES refresh.
    #[test]
    fn opted_in_with_types_reregisters() {
        assert!(startup_reregister_types(true, &[ClaimType::PlainText]));
        assert!(startup_reregister_types(true, &ClaimType::ALL));
    }

    /// Opted in but with an empty claim set is a no-op — nothing to register.
    #[test]
    fn opted_in_but_no_types_is_a_noop() {
        assert!(!startup_reregister_types(true, &[]));
    }
}

#[cfg(test)]
mod packaging_consistency_tests {
    //! The OS handler-eligibility declarations (the Linux `.desktop` MimeType,
    //! the macOS `Info.plist` UTIs) MUST cover every type SCR1B3 claims — else a
    //! `register()` would set a default the OS doesn't recognise SCR1B3 as a
    //! handler for, and the choice would silently not stick. These tests bind the
    //! packaging files to [`ClaimType`] so they can't drift.

    use scribe_core::config::ClaimType;

    const DESKTOP: &str = include_str!("../../../../packaging/linux/scr1b3.desktop");
    const INFO_PLIST: &str = include_str!("../../../../packaging/macos/Info.plist");

    #[test]
    fn linux_desktop_declares_every_claimed_mime() {
        let mime_line = DESKTOP
            .lines()
            .find(|l| l.starts_with("MimeType="))
            .expect("MimeType= line in scr1b3.desktop");
        for ct in ClaimType::ALL {
            for m in ct.linux_mimes() {
                assert!(
                    mime_line.contains(m),
                    "scr1b3.desktop MimeType= is missing {m:?} (claimed by {ct:?}) — \
                     xdg-mime default for it wouldn't stick"
                );
            }
        }
    }

    #[test]
    fn macos_info_plist_declares_every_claimed_uti() {
        for ct in ClaimType::ALL {
            for uti in ct.macos_utis() {
                assert!(
                    INFO_PLIST.contains(uti),
                    "Info.plist LSItemContentTypes is missing {uti:?} (claimed by {ct:?})"
                );
            }
        }
    }

    /// The CI mutation gate EXCLUDES `integration/windows.rs`, and this is the
    /// premise that makes that exclusion honest rather than a blind spot.
    ///
    /// The gate runs on ubuntu. `windows.rs` is `#[cfg(windows)]`, so it is not
    /// compiled there: mutating it changes nothing that builds, no test can
    /// fail, and every mutant reports MISSED. Excluding it drops 100% false
    /// positives — but ONLY while it stays cfg-gated. If it is ever made
    /// cross-platform (or test-compiled like its `windows_entries` sibling), its
    /// mutants become real signal and the `--exclude` in .github/workflows/ci.yml
    /// would start hiding genuine gaps. Fail here so that cannot happen quietly.
    #[test]
    fn windows_module_is_cfg_gated_so_the_mutation_exclusion_stays_honest() {
        let src = include_str!("mod.rs");
        // ASSEMBLED, never written as a literal. This test reads its OWN file, so
        // a literal needle would sit in the source and `contains` would match the
        // needle itself — passing no matter what the real declaration said. The
        // first draft did exactly that, and only re-running the premise (flip the
        // gate, expect a failure) caught it. Building the string from parts keeps
        // the declaration up top the one and only match.
        let needle = ["#[cfg(", "windows", ")]\n", "mod windows;"].concat();
        assert_eq!(
            src.matches(needle.as_str()).count(),
            1,
            "`mod windows;` is no longer exactly `#[cfg(windows)]`-gated (or this \
             test now self-matches). If that module compiles off Windows its \
             mutants are real signal: DROP `--exclude \
             '**/src/integration/windows.rs'` from the mutation-in-diff job in \
             .github/workflows/ci.yml and delete this test, rather than leaving a \
             gate that silently skips a file it claims to cover."
        );
    }
}
