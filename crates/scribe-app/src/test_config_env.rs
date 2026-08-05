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

/// The single lock serialising every `S4F3_DISABLE_TELEMETRY` mutation in this
/// crate's test binary.
///
/// Same defect, second variable. `reporting.rs` serialised its telemetry tests
/// on a module-private `ENDPOINT_LOCK` while `issue_intake.rs` mutated the SAME
/// process-global var under no lock at all — so the two modules, which compile
/// into ONE test binary that cargo runs in parallel, could clobber each other's
/// opt-out state. A test asserting the telemetry-ENABLED path is exactly what
/// makes that race observable: a peer setting `=1` mid-window turns a real
/// forwarding assertion into a spurious failure.
static TELEMETRY_ENV_LOCK: Mutex<()> = Mutex::new(());

/// Take the crate-wide telemetry-opt-out env lock.
///
/// For call sites that mutate a second env var in the same window (the report
/// endpoint) and so cannot use [`with_telemetry_opt_out`]. Poison is recovered,
/// not propagated, for the same reason as [`config_dir_env_guard`].
pub(crate) fn telemetry_env_guard() -> MutexGuard<'static, ()> {
    TELEMETRY_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Run `body` with `S4F3_DISABLE_TELEMETRY` either set to `1` (`disabled`) or
/// removed, restoring the previous value afterwards and holding the crate-wide
/// lock for the whole window.
pub(crate) fn with_telemetry_opt_out<T>(disabled: bool, body: impl FnOnce() -> T) -> T {
    let _guard = telemetry_env_guard();
    let prev = std::env::var_os("S4F3_DISABLE_TELEMETRY");
    if disabled {
        std::env::set_var("S4F3_DISABLE_TELEMETRY", "1");
    } else {
        std::env::remove_var("S4F3_DISABLE_TELEMETRY");
    }
    let out = body();
    match prev {
        Some(v) => std::env::set_var("S4F3_DISABLE_TELEMETRY", v),
        None => std::env::remove_var("S4F3_DISABLE_TELEMETRY"),
    }
    out
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

    /// Rust source with comments and string literals removed.
    ///
    /// A structural guard that greps the RAW text is defeated by a doc comment:
    /// a file can revert to a private mutex and still "reference" the shared
    /// lock in prose, which is precisely the "a comment asserting a guarantee
    /// the code lacks" failure this module was created to stop. Scanning
    /// code-only means the reference has to be a real call. String literals go
    /// too, so a file cannot satisfy the guard by naming the helper inside a
    /// message.
    fn code_only(src: &str) -> String {
        let mut out = String::with_capacity(src.len());
        let b: Vec<char> = src.chars().collect();
        let (mut i, n) = (0usize, b.len());
        while i < n {
            match b[i] {
                // line comment
                '/' if i + 1 < n && b[i + 1] == '/' => {
                    while i < n && b[i] != '\n' {
                        i += 1;
                    }
                }
                // block comment (non-nesting is sufficient for this crate)
                '/' if i + 1 < n && b[i + 1] == '*' => {
                    i += 2;
                    while i + 1 < n && !(b[i] == '*' && b[i + 1] == '/') {
                        i += 1;
                    }
                    i = (i + 2).min(n);
                }
                // string literal
                '"' => {
                    i += 1;
                    while i < n {
                        if b[i] == '\\' {
                            i += 2;
                            continue;
                        }
                        if b[i] == '"' {
                            i += 1;
                            break;
                        }
                        i += 1;
                    }
                    out.push(' ');
                }
                c => {
                    out.push(c);
                    i += 1;
                }
            }
        }
        out
    }

    /// The same structural recurrence guard, for the second shared variable.
    ///
    /// `S4F3_DISABLE_TELEMETRY` is process-global exactly like the config dir,
    /// and it is mutated from modules (`reporting`, `issue_intake`) that compile
    /// into ONE parallel test binary. A module-private mutex — which is what
    /// `reporting` had — excludes only that module, so a peer could flip the
    /// opt-out mid-window. As with the config dir, a passing suite is not
    /// evidence: the race is intermittent by construction.
    ///
    /// Three deliberate hardenings over the config-dir guard above, each closing
    /// a way that guard could pass while the race was open:
    ///
    ///  1. **Code-only.** The reference must survive [`code_only`], so a doc
    ///     comment mentioning `test_config_env` cannot satisfy it.
    ///  2. **Not keyed to a literal call shape.** It matches the env-var NAME
    ///     plus any `set_var`/`remove_var` in the same file, so a rustfmt-wrapped
    ///     call, a `const KEY`, or a remove-only mutation is still seen.
    ///  3. **Anti-vacuity anchored on the SCAN's reach, not on the offender
    ///     count.** Asserting `checked >= <today's count>` has zero headroom:
    ///     migrating a mutator to the helper — the stated direction of travel —
    ///     would drop the count and fail with a message blaming the walk. The
    ///     floor here is instead "the scan can see this variable at all", which
    ///     stays true as mutators migrate.
    #[test]
    fn every_telemetry_env_mutator_in_this_crate_takes_the_shared_lock() {
        const VAR: &str = "S4F3_DISABLE_TELEMETRY";
        let me = Path::new(file!())
            .file_name()
            .expect("this file has a name")
            .to_owned();

        let mut mentions = 0usize;
        let mut checked = 0usize;
        let mut offenders = Vec::new();
        for path in crate_sources() {
            if path.file_name() == Some(me.as_os_str()) {
                continue;
            }
            let raw = std::fs::read_to_string(&path).expect("readable source");
            // The var name lives in a string literal at every call site, so
            // detection reads the RAW text; the shared-lock REFERENCE must be
            // real code, so that half reads the stripped text.
            if !raw.contains(VAR) {
                continue;
            }
            mentions += 1;
            let code = code_only(&raw);
            let mutates = code.contains("set_var") || code.contains("remove_var");
            if !mutates {
                continue; // reads the gate only (production), never flips it
            }
            checked += 1;
            if !code.contains("test_config_env") {
                offenders.push(path);
            }
        }

        assert!(
            mentions >= 2,
            "only {mentions} file(s) mention {VAR} — the scan is not reaching the \
             files it is meant to police, so a violation would pass unseen"
        );
        assert!(
            offenders.is_empty(),
            "these files mutate the process-global {VAR} without taking the \
             crate-wide lock in `test_config_env` (checked {checked} mutator(s)). \
             A module-private `static Mutex<()>` excludes only that module; every \
             module here compiles into ONE test binary that cargo runs in \
             parallel, so the opt-out flips clobber each other. Route them \
             through `test_config_env::with_telemetry_opt_out` / \
             `telemetry_env_guard`: {offenders:#?}"
        );
    }

    /// The guard above must not be satisfiable by PROSE. Reverting a module to a
    /// private mutex while leaving a doc line that mentions `test_config_env` is
    /// the exact evasion this asserts is impossible.
    #[test]
    fn the_telemetry_guard_is_not_satisfied_by_a_doc_comment() {
        let evasive = r#"
            //! Serialised under the crate-wide lock (see `test_config_env`).
            static PRIVATE: Mutex<()> = Mutex::new(());
            fn f() { std::env::set_var("S4F3_DISABLE_TELEMETRY", "1"); }
        "#;
        let code = code_only(evasive);
        assert!(
            code.contains("set_var"),
            "the mutation must still be visible after stripping"
        );
        assert!(
            !code.contains("test_config_env"),
            "a doc comment must NOT survive as a shared-lock reference — if it \
             does, the recurrence guard is defeated by exactly the kind of \
             comment it exists to disbelieve"
        );
    }

    /// A string literal must not satisfy the guard either.
    #[test]
    fn the_telemetry_guard_is_not_satisfied_by_a_string_literal() {
        let evasive = r#"fn f() { panic!("use test_config_env"); std::env::set_var(K, "1"); }"#;
        let code = code_only(evasive);
        assert!(code.contains("set_var"));
        assert!(
            !code.contains("test_config_env"),
            "a string literal must not count as a call to the shared helper"
        );
    }

    /// The telemetry lock must actually exclude — the behavioural half of the
    /// structural guard above, mirroring `with_config_dir_is_mutually_exclusive`.
    #[test]
    fn with_telemetry_opt_out_is_mutually_exclusive_across_threads() {
        std::thread::scope(|s| {
            let t = s.spawn(|| {
                for _ in 0..40 {
                    with_telemetry_opt_out(true, || {
                        assert!(
                            std::env::var_os("S4F3_DISABLE_TELEMETRY").is_some(),
                            "a peer cleared the opt-out inside our window — the \
                             lock is not exclusive"
                        );
                    });
                }
            });
            for _ in 0..40 {
                with_telemetry_opt_out(false, || {
                    assert!(
                        std::env::var_os("S4F3_DISABLE_TELEMETRY").is_none(),
                        "a peer set the opt-out inside our window — the lock is \
                         not exclusive"
                    );
                });
            }
            t.join().expect("thread b");
        });
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
