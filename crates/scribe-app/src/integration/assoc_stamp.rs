//! Startup convergence for the Windows file-association refresh.
//!
//! `reregister_on_startup` re-applies EVERY association key on every launch. With
//! the full claim set that is 182 `reg.exe` writes plus the removal sweep — each
//! one a synchronous process spawn — and it ran on the MAIN thread before the
//! window appeared. The Settings path already knew this was intolerable (its own
//! comment says an inline run "would otherwise FREEZE the window for a few
//! seconds"), and moved to a worker thread; the startup path never did.
//!
//! It also never CONVERGED: `scribe_core::config::registration_fingerprint`
//! exists precisely so a launch can tell "nothing changed" from "the exe moved
//! or the claims changed", but nothing called it and nothing stored its result,
//! so every launch paid the full cost to rewrite byte-identical values.
//!
//! This module supplies the missing half — a persisted stamp of the last
//! SUCCESSFUL registration — and keeps the whole decision pure enough to test on
//! any OS: [`startup_refresh`] takes the registrar as a closure, so the tests
//! below assert the number of times a real registration would have been
//! attempted rather than asserting a message about it.
//!
//! ## Why a sidecar file and not an `IntegrationConfig` field
//!
//! The stamp is not a user preference — it is a cache of what SCR1B3 last wrote
//! to another system's database (the registry). `IntegrationConfig` is
//! user-owned, round-trips through the visible `scr1b3.toml`, and is handed to
//! `reregister_on_startup` by shared reference; persisting through it would mean
//! writing the user's config file on launch to record an implementation detail.
//! A sidecar keeps the config honest and the cache disposable: delete the file
//! and the next launch simply re-registers.

use super::windows_entries::{
    entries_digest, registry_entries, unregister_entries, APP_NAME, APP_ROOT, CLASS_ROOT,
};
use super::RegisterReport;
use scribe_core::config::{registration_fingerprint, ClaimType};
use std::path::{Path, PathBuf};

/// Sidecar file name inside the SCR1B3 config directory.
const STAMP_FILE: &str = "windows-associations.stamp";

/// Full path of the stamp inside `config_dir`.
pub(crate) fn stamp_path(config_dir: &Path) -> PathBuf {
    config_dir.join(STAMP_FILE)
}

/// The stamp recorded by the last successful registration, if any.
///
/// An unreadable / absent stamp is `None`, which means "re-register" — the
/// fail-safe direction. A corrupt stamp costs one extra registration, whereas
/// treating a read error as "up to date" would strand a user with associations
/// pointing at an exe path that no longer exists.
pub(crate) fn read_stamp(config_dir: &Path) -> Option<String> {
    std::fs::read_to_string(stamp_path(config_dir))
        .ok()
        .map(|s| s.trim().to_string())
}

/// Record `stamp` as the state of the last successful registration.
pub(crate) fn write_stamp(config_dir: &Path, stamp: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(config_dir)?;
    std::fs::write(stamp_path(config_dir), stamp)
}

/// The stamp describing the registry work `types` at `exe` would perform.
///
/// Two components, both load-bearing:
///
/// - `registration_fingerprint` — the exe path plus the claimed group keys. This
///   is the human-legible half and the reason the function exists: it names the
///   two things that legitimately change (an in-app update or portable-zip move
///   relocates the exe; the user re-ticks the Settings boxes).
/// - [`entries_digest`] — a digest of the actual emitted entries and removals.
///   Without it the stamp would be blind to a BUILD change: a release that adds
///   a registry surface produces the same exe path and the same claims, so an
///   upgrading user would match the old stamp and never receive the new keys.
pub(crate) fn current_stamp(exe: &str, types: &[ClaimType]) -> String {
    let entries = registry_entries(types, exe, CLASS_ROOT, APP_ROOT, APP_NAME);
    let deletes = unregister_entries(types, exe, CLASS_ROOT, APP_ROOT);
    format!(
        "{}|{}",
        registration_fingerprint(exe, types),
        entries_digest(&entries, &deletes)
    )
}

/// What a startup refresh actually did. Returned so the caller can log honestly
/// and the tests can assert the decision rather than a log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StartupOutcome {
    /// The stamp matched — no `reg.exe` was spawned at all.
    AlreadyCurrent,
    /// A registration ran and every write landed; the stamp was refreshed.
    Registered { count: usize },
    /// A registration ran and something failed; the stamp was deliberately NOT
    /// written, so the next launch retries.
    Failed { failures: usize },
}

/// Refresh the associations only when the work has actually changed.
///
/// `register` is injected so this — the whole decision, including the stamp
/// read/write — is testable on any host without a registry. `config_dir` is
/// `None` when the OS gives us no config directory: registration still runs (the
/// user opted in and we must not silently do nothing), it simply cannot
/// converge, which is the honest degradation.
pub(crate) fn startup_refresh<F>(
    config_dir: Option<&Path>,
    exe: &str,
    types: &[ClaimType],
    register: F,
) -> StartupOutcome
where
    F: FnOnce(&[ClaimType]) -> RegisterReport,
{
    let current = current_stamp(exe, types);
    if let Some(dir) = config_dir {
        if read_stamp(dir).as_deref() == Some(current.as_str()) {
            return StartupOutcome::AlreadyCurrent;
        }
    }

    let report = register(types);
    if report.failed.is_empty() {
        if let Some(dir) = config_dir {
            // A stamp we cannot persist is not fatal — it costs one repeated
            // registration next launch. Surfaced, never swallowed.
            if let Err(err) = write_stamp(dir, &current) {
                tracing::warn!(
                    target: "scribe::integration",
                    %err,
                    "could not record the file-association stamp; startup will re-register"
                );
            }
        }
        StartupOutcome::Registered {
            count: report.registered.len(),
        }
    } else {
        StartupOutcome::Failed {
            failures: report.failed.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    const EXE: &str = r"C:\Apps\SCR 1B3\scr1b3.exe";

    fn ok_report(types: &[ClaimType]) -> RegisterReport {
        RegisterReport {
            registered: types.iter().map(|t| t.key().to_string()).collect(),
            failed: Vec::new(),
            needs_user_action: true,
            message: "ok".into(),
        }
    }

    fn failing_report(_types: &[ClaimType]) -> RegisterReport {
        RegisterReport {
            registered: Vec::new(),
            failed: vec![("Software\\Classes\\.txt".into(), "Access is denied.".into())],
            needs_user_action: false,
            message: "failed".into(),
        }
    }

    /// The regression this whole module exists for: an UNCHANGED launch must
    /// spawn ZERO registry writes. Asserted as a call COUNT on the injected
    /// registrar — a message assertion would pass while the 182 spawns happened
    /// anyway, which is the vacuous-assertion trap this module already fell into
    /// once.
    #[test]
    fn a_second_unchanged_launch_does_no_registry_work_at_all() {
        let dir = tempfile::tempdir().expect("tempdir");
        let calls = Cell::new(0usize);
        let types = ClaimType::ALL;

        let first = startup_refresh(Some(dir.path()), EXE, &types, |t| {
            calls.set(calls.get() + 1);
            ok_report(t)
        });
        assert_eq!(first, StartupOutcome::Registered { count: 5 });
        assert_eq!(calls.get(), 1, "the first launch must register");

        let second = startup_refresh(Some(dir.path()), EXE, &types, |t| {
            calls.set(calls.get() + 1);
            ok_report(t)
        });
        assert_eq!(second, StartupOutcome::AlreadyCurrent);
        assert_eq!(
            calls.get(),
            1,
            "an unchanged second launch must not touch the registry"
        );
    }

    #[test]
    fn a_moved_executable_forces_a_rewrite() {
        // The exact case the fingerprint was written for: an in-app update or a
        // portable-zip move relocates the exe, leaving every registered
        // `shell\open\command` pointing at a path that no longer exists.
        let dir = tempfile::tempdir().expect("tempdir");
        let calls = Cell::new(0usize);
        let types = [ClaimType::PlainText];

        startup_refresh(Some(dir.path()), EXE, &types, |t| {
            calls.set(calls.get() + 1);
            ok_report(t)
        });
        let after_move = startup_refresh(Some(dir.path()), r"D:\Moved\scr1b3.exe", &types, |t| {
            calls.set(calls.get() + 1);
            ok_report(t)
        });
        assert!(matches!(after_move, StartupOutcome::Registered { .. }));
        assert_eq!(calls.get(), 2, "a moved exe must re-register");
    }

    #[test]
    fn a_changed_claim_set_forces_a_rewrite() {
        let dir = tempfile::tempdir().expect("tempdir");
        let calls = Cell::new(0usize);

        startup_refresh(Some(dir.path()), EXE, &[ClaimType::PlainText], |t| {
            calls.set(calls.get() + 1);
            ok_report(t)
        });
        startup_refresh(
            Some(dir.path()),
            EXE,
            &[ClaimType::PlainText, ClaimType::Json],
            |t| {
                calls.set(calls.get() + 1);
                ok_report(t)
            },
        );
        assert_eq!(calls.get(), 2, "a changed claim set must re-register");
    }

    /// A FAILED registration must not stamp itself as done — otherwise a single
    /// transient failure would freeze the user's associations permanently, which
    /// is strictly worse than the redundant work this module removes.
    #[test]
    fn a_failed_registration_is_never_stamped_and_is_retried() {
        let dir = tempfile::tempdir().expect("tempdir");
        let calls = Cell::new(0usize);
        let types = [ClaimType::PlainText];

        let first = startup_refresh(Some(dir.path()), EXE, &types, |t| {
            calls.set(calls.get() + 1);
            failing_report(t)
        });
        assert_eq!(first, StartupOutcome::Failed { failures: 1 });
        assert!(
            read_stamp(dir.path()).is_none(),
            "a failed run must leave no stamp"
        );

        let second = startup_refresh(Some(dir.path()), EXE, &types, |t| {
            calls.set(calls.get() + 1);
            ok_report(t)
        });
        assert!(matches!(second, StartupOutcome::Registered { .. }));
        assert_eq!(calls.get(), 2, "the next launch must retry");
        assert_eq!(
            read_stamp(dir.path()).as_deref(),
            Some(current_stamp(EXE, &types).as_str()),
            "the successful retry records the stamp"
        );
    }

    /// With no config directory there is nowhere to converge, so the refresh
    /// must still RUN rather than silently skipping the work the user opted in
    /// to. Pins the fail-safe direction.
    #[test]
    fn without_a_config_dir_it_still_registers_every_time() {
        let calls = Cell::new(0usize);
        let types = [ClaimType::PlainText];
        for _ in 0..2 {
            startup_refresh(None, EXE, &types, |t| {
                calls.set(calls.get() + 1);
                ok_report(t)
            });
        }
        assert_eq!(calls.get(), 2, "no stamp store ⇒ never skip");
    }

    /// A corrupt / foreign stamp must re-register, not be mistaken for current.
    #[test]
    fn an_unrecognised_stamp_re_registers() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_stamp(dir.path(), "not-a-real-stamp").expect("write");
        let calls = Cell::new(0usize);
        startup_refresh(Some(dir.path()), EXE, &[ClaimType::PlainText], |t| {
            calls.set(calls.get() + 1);
            ok_report(t)
        });
        assert_eq!(calls.get(), 1);
    }

    /// The stamp carries BOTH halves. The digest half is what makes an upgrade
    /// that adds registry surfaces re-register a user whose exe and claims are
    /// unchanged; without it the fingerprint alone would match forever.
    #[test]
    fn the_stamp_contains_the_fingerprint_and_a_distinct_digest() {
        let types = [ClaimType::PlainText];
        let stamp = current_stamp(EXE, &types);
        let fingerprint = registration_fingerprint(EXE, &types);
        assert!(
            stamp.starts_with(&fingerprint),
            "the stamp must carry the fingerprint verbatim: {stamp}"
        );
        let digest = stamp
            .strip_prefix(&fingerprint)
            .and_then(|r| r.strip_prefix('|'))
            .expect("a digest follows the fingerprint");
        assert_eq!(digest.len(), 16, "a 64-bit digest in hex: {digest:?}");
        assert!(
            digest.chars().all(|c| c.is_ascii_hexdigit()),
            "digest is hex: {digest:?}"
        );
        // The digest is derived from the real entry set, so it is not constant
        // across claim sets even though the fingerprint already differs.
        let other = current_stamp(EXE, &[ClaimType::Json]);
        let other_digest = other.rsplit('|').next().expect("digest");
        assert_ne!(digest, other_digest);
    }

    #[test]
    fn the_stamp_lives_beside_the_config_and_not_inside_it() {
        let dir = Path::new("/tmp/cfg");
        assert_eq!(
            stamp_path(dir),
            Path::new("/tmp/cfg").join("windows-associations.stamp")
        );
    }
}
