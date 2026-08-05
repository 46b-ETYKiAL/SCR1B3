//! THE process-global `SCR1B3_CONFIG_DIR` test lock, for the whole crate.
//!
//! WHY THIS EXISTS. Several test modules need to redirect `SCR1B3_CONFIG_DIR`,
//! because the functions they drive (`restore_tabs_from_manifest`,
//! `load_plugins`, `load_snippets`, `save_session`, `Config::config_file_path`,
//! the theme seed/save round-trips) read the GLOBAL config dir rather than an
//! instance one. Each module had grown its OWN `static Mutex<()>` plus a
//! byte-identical `set_var -> body -> restore` helper, and each doc-commented a
//! serialisation guarantee it did not have:
//!
//! * `app/session_io_tests.rs`      `CONFIG_DIR_ENV_LOCK`
//! * `app/mod_logic_tests.rs`       `CONFIG_DIR_ENV_LOCK`
//! * `app/build_plugins_tests.rs`   `CONFIG_DIR_ENV_LOCK`
//! * `app/deferred_actions_tests.rs` `CFG_ENV_LOCK`   (renamed — a grep for the
//!                                                     common name misses it)
//! * `app/render_support.rs`        `LK`   (function-local)
//! * `app/session_persist.rs`       `CFG_DIR_LOCK`
//! * `theme_editor.rs`              `CONFIG_DIR_LOCK`
//! * `action_log.rs`                `ENV_LOCK`  (also owns `SCR1B3_NO_ACTION_LOG`)
//!
//! Eight DISTINCT `Mutex` values give mutual exclusion only WITHIN each module.
//! Every one of these modules is declared in the same crate, so they compile
//! into ONE test binary and cargo runs them in parallel against each other —
//! and the env var they all mutate is per-PROCESS. Two modules therefore
//! overlapped freely: one could set the redirect while another was mid-`body`,
//! or restore it out from under the other, producing intermittent failures in
//! config-reload, session-restore and plugin-trust tests.
//!
//! A GREEN RUN DOES NOT DISPROVE THIS. The race is intermittent by
//! construction; one passing run demonstrates one interleaving, not the absence
//! of a bad one. The prior per-module doc comments are what made it survive —
//! each asserted the exclusion the reader was looking for.
//!
//! The correct version already existed in-tree at
//! `scribe-core/src/config/mod.rs` (commit 536fe64, whose message records the
//! real diagnosis: "the prior doc comment's `--test-threads=1` assumption was
//! false"). That copy is genuinely safe because it lives in a DIFFERENT crate,
//! hence a different test process — it is not a duplicate of this lock and must
//! not be merged with it.
//!
//! ONE lock, one helper, every call site routed through it.

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

/// The single lock serialising every `SCR1B3_CONFIG_DIR` mutation in this
/// crate's test binary.
static CONFIG_DIR_ENV_LOCK: Mutex<()> = Mutex::new(());

/// Take the crate-wide config-dir env lock.
///
/// For call sites that cannot use [`with_config_dir`] because they mutate a
/// second env var too (`action_log`'s `SCR1B3_NO_ACTION_LOG`) or restore via an
/// RAII guard (`theme_editor`). Holding THIS guard is what makes them exclusive
/// with the `with_config_dir` callers — a private mutex would not be.
///
/// A poisoned lock is recovered rather than propagated: a panicking test leaves
/// no shared data behind (the guard's only job is mutual exclusion), so
/// cascading the poison would just turn one failure into many.
pub(crate) fn config_dir_env_guard() -> MutexGuard<'static, ()> {
    CONFIG_DIR_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Run `body` with `SCR1B3_CONFIG_DIR` pointed at `dir`, restoring the previous
/// value afterwards, holding the crate-wide lock for the whole window.
pub(crate) fn with_config_dir<T>(dir: &Path, body: impl FnOnce() -> T) -> T {
    let _guard = config_dir_env_guard();
    let prev = std::env::var_os("SCR1B3_CONFIG_DIR");
    std::env::set_var("SCR1B3_CONFIG_DIR", dir);
    let out = body();
    match prev {
        Some(v) => std::env::set_var("SCR1B3_CONFIG_DIR", v),
        None => std::env::remove_var("SCR1B3_CONFIG_DIR"),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point: two calls cannot interleave. A second `with_config_dir`
    /// entered from another thread must not observe — or clobber — the first
    /// one's redirect while it is still inside its body.
    #[test]
    fn with_config_dir_is_mutually_exclusive_across_threads() {
        let a = std::env::temp_dir().join("scr1b3-lock-a");
        let b = std::env::temp_dir().join("scr1b3-lock-b");
        let observed_by_b = std::sync::Arc::new(Mutex::new(Vec::<std::ffi::OsString>::new()));

        std::thread::scope(|s| {
            let obs = std::sync::Arc::clone(&observed_by_b);
            let bb = b.clone();
            let t = s.spawn(move || {
                for _ in 0..40 {
                    with_config_dir(&bb, || {
                        // Inside the window, the var must be OURS, never the
                        // other thread's — that equality IS the exclusion.
                        let seen = std::env::var_os("SCR1B3_CONFIG_DIR").unwrap_or_default();
                        obs.lock().unwrap().push(seen);
                    });
                }
            });
            for _ in 0..40 {
                with_config_dir(&a, || {
                    assert_eq!(
                        std::env::var_os("SCR1B3_CONFIG_DIR").as_deref(),
                        Some(a.as_os_str()),
                        "another thread's redirect leaked into this window — the \
                         lock is not exclusive"
                    );
                });
            }
            t.join().expect("thread b");
        });

        let seen = observed_by_b.lock().unwrap();
        assert_eq!(seen.len(), 40, "thread b must have completed every window");
        assert!(
            seen.iter().all(|v| v.as_os_str() == b.as_os_str()),
            "thread b observed a redirect it did not set: {seen:?}"
        );
    }

    /// Every `.rs` file in this crate, recursively.
    fn crate_sources() -> Vec<std::path::PathBuf> {
        fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
            for e in std::fs::read_dir(dir).expect("readable src dir").flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    out.push(p);
                }
            }
        }
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut out = Vec::new();
        walk(&src, &mut out);
        assert!(
            out.len() > 20,
            "the source walk found only {} files — it is scanning the wrong tree, \
             and an empty scan would pass this guard vacuously",
            out.len()
        );
        out
    }

    /// THE recurrence guard, and the only one that can catch the actual defect.
    ///
    /// The runtime exclusion test above proves THIS lock excludes. It cannot
    /// prove there is only one — a new module that quietly grows its own
    /// `static Mutex<()>` and redirects the env under it would leave that test
    /// green while reopening exactly the race this module was created to close.
    /// A four-way (in fact eight-way) env race is intermittent by construction,
    /// so a passing suite is not evidence either.
    ///
    /// So the invariant is structural, not behavioural: any file that mutates
    /// `SCR1B3_CONFIG_DIR` must reference `test_config_env`, i.e. must be
    /// holding this crate-wide lock. Two files legitimately still call `set_var`
    /// themselves — `action_log` (which owns a second var of its own) and
    /// `theme_editor` (RAII restore-on-drop) — and both take the shared guard,
    /// which is what the reference proves.
    #[test]
    fn every_config_dir_env_mutator_in_this_crate_takes_the_shared_lock() {
        const MUTATOR: &str = "set_var(\"SCR1B3_CONFIG_DIR\"";
        let me = Path::new(file!())
            .file_name()
            .expect("this file has a name")
            .to_owned();

        let mut checked = 0usize;
        let mut offenders = Vec::new();
        for path in crate_sources() {
            if path.file_name() == Some(me.as_os_str()) {
                continue;
            }
            let body = std::fs::read_to_string(&path).expect("readable source");
            if !body.contains(MUTATOR) {
                continue;
            }
            checked += 1;
            if !body.contains("test_config_env") {
                offenders.push(path);
            }
        }

        assert!(
            checked >= 4,
            "only {checked} config-dir mutators found — the scan is not reaching \
             the files it is meant to police, so a violation would pass unseen"
        );
        assert!(
            offenders.is_empty(),
            "these files redirect the process-global SCR1B3_CONFIG_DIR without \
             taking the crate-wide lock in `test_config_env`. A module-private \
             `static Mutex<()>` excludes only that module; every module here \
             compiles into ONE test binary that cargo runs in parallel, so the \
             redirects clobber each other. Route them through \
             `test_config_env::with_config_dir` / `config_dir_env_guard`: {offenders:#?}"
        );
    }

    #[test]
    fn with_config_dir_restores_an_absent_var() {
        let dir = std::env::temp_dir().join("scr1b3-lock-restore");
        // Establish "absent" inside the lock so this cannot fight a peer.
        let observed = {
            let _g = config_dir_env_guard();
            let prev = std::env::var_os("SCR1B3_CONFIG_DIR");
            std::env::remove_var("SCR1B3_CONFIG_DIR");
            drop(_g);
            let seen = with_config_dir(&dir, || std::env::var_os("SCR1B3_CONFIG_DIR"));
            let after = std::env::var_os("SCR1B3_CONFIG_DIR");
            let _g = config_dir_env_guard();
            match prev {
                Some(v) => std::env::set_var("SCR1B3_CONFIG_DIR", v),
                None => std::env::remove_var("SCR1B3_CONFIG_DIR"),
            }
            (seen, after)
        };
        assert_eq!(
            observed.0.as_deref(),
            Some(dir.as_os_str()),
            "the redirect must be visible inside the body"
        );
        assert!(
            observed.1.is_none(),
            "an absent var must be restored to absent, not to an empty string"
        );
    }
}
