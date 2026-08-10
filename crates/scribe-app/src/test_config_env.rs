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
    with_config_dir_locked(dir, body)
}

/// The redirect + restore itself, WITHOUT taking the lock.
///
/// The caller must already hold [`config_dir_env_guard`]. This exists so a test
/// can observe the state BEFORE, INSIDE and AFTER one redirect within a SINGLE
/// lock window. Composing those observations out of two windows (take the lock,
/// clear the var, release, call [`with_config_dir`], then read the var) leaves
/// gaps in which a peer legitimately owns the variable, so the reading is not
/// evidence about this function at all — it is a race whose outcome depends on
/// whether any peer happened to be contending.
fn with_config_dir_locked<T>(dir: &Path, body: impl FnOnce() -> T) -> T {
    let prev = std::env::var_os("SCR1B3_CONFIG_DIR");
    std::env::set_var("SCR1B3_CONFIG_DIR", dir);
    let out = body();
    match prev {
        Some(v) => std::env::set_var("SCR1B3_CONFIG_DIR", v),
        None => std::env::remove_var("SCR1B3_CONFIG_DIR"),
    }
    out
}

/// Rust source with comments and string literals removed.
///
/// A structural guard that greps the RAW text is defeated by a doc comment: a
/// file can revert to a private mutex and still "reference" the shared lock in
/// prose, which is precisely the "a comment asserting a guarantee the code
/// lacks" failure this module was created to stop. Scanning code-only means the
/// reference has to be a real call. String literals go too, so a file cannot
/// satisfy the guard by naming the helper inside a message.
///
/// Shared crate-wide (it is also the matcher behind the grid-pane
/// `.changed()`-arm guard in `app::text_ops_methods`) so there is exactly ONE
/// code-only matcher in the crate rather than a second, subtly different copy.
pub(crate) fn code_only(src: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Which thread took the lock for one window, so the interleaving can be
    /// asserted after the scope has joined.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Who {
        A,
        B,
    }

    /// How many times the acquisition order changed hands. Zero-to-one
    /// transitions mean one thread ran to completion before the other started —
    /// i.e. the windows never raced and the "exclusion" assertions below were
    /// never actually exercised.
    fn handovers(order: &[Who]) -> usize {
        order.windows(2).filter(|w| w[0] != w[1]).count()
    }

    /// Rounds each exclusion test runs. Every round is barrier-synchronised, so
    /// each one is a genuine two-thread race for the same lock.
    const ROUNDS: usize = 40;

    /// The whole point: two calls cannot interleave. A second `with_config_dir`
    /// entered from another thread must not observe — or clobber — the first
    /// one's redirect while it is still inside its body.
    ///
    /// A `Barrier` is load-bearing, not decoration. Without it the spawned
    /// thread routinely ran all of its windows before the main thread reached
    /// its first one, so the two never contended and the equality assertions
    /// held trivially — the test asserted exclusion it had not exercised. Both
    /// threads now rendezvous BEFORE every window (outside the lock, or the
    /// holder would wait on a thread that is waiting on the holder), so each
    /// round is a real race, and `handovers` proves after the fact that the
    /// race happened.
    ///
    /// Neither thread asserts inline: a panic inside one of them would leave
    /// the other blocked on the barrier forever, turning a failure into a hang.
    /// Both record what they saw; every assertion runs after the scope joins.
    #[test]
    fn with_config_dir_is_mutually_exclusive_across_threads() {
        let a = std::env::temp_dir().join("scr1b3-lock-a");
        let b = std::env::temp_dir().join("scr1b3-lock-b");
        let seen_by_a = std::sync::Arc::new(Mutex::new(Vec::<std::ffi::OsString>::new()));
        let seen_by_b = std::sync::Arc::new(Mutex::new(Vec::<std::ffi::OsString>::new()));
        let order = std::sync::Arc::new(Mutex::new(Vec::<Who>::new()));
        let gate = std::sync::Barrier::new(2);

        std::thread::scope(|s| {
            let (obs, ord, bb) = (
                std::sync::Arc::clone(&seen_by_b),
                std::sync::Arc::clone(&order),
                b.clone(),
            );
            let gate = &gate;
            let t = s.spawn(move || {
                for _ in 0..ROUNDS {
                    gate.wait();
                    with_config_dir(&bb, || {
                        // Inside the window, the var must be OURS, never the
                        // other thread's — that equality IS the exclusion.
                        ord.lock().unwrap().push(Who::B);
                        let seen = std::env::var_os("SCR1B3_CONFIG_DIR").unwrap_or_default();
                        obs.lock().unwrap().push(seen);
                    });
                }
            });
            for _ in 0..ROUNDS {
                gate.wait();
                with_config_dir(&a, || {
                    order.lock().unwrap().push(Who::A);
                    let seen = std::env::var_os("SCR1B3_CONFIG_DIR").unwrap_or_default();
                    seen_by_a.lock().unwrap().push(seen);
                });
            }
            t.join().expect("thread b");
        });

        let order = order.lock().unwrap();
        assert!(
            handovers(&order) >= 10,
            "the two threads never actually contended (only {} handover(s) in {} \
             windows) — one ran to completion before the other started, so the \
             exclusion assertions below were never exercised",
            handovers(&order),
            order.len()
        );
        let by_a = seen_by_a.lock().unwrap();
        let by_b = seen_by_b.lock().unwrap();
        assert_eq!(by_a.len(), ROUNDS, "thread a must complete every window");
        assert_eq!(by_b.len(), ROUNDS, "thread b must complete every window");
        assert!(
            by_a.iter().all(|v| v.as_os_str() == a.as_os_str()),
            "thread a observed a redirect it did not set: {by_a:?}"
        );
        assert!(
            by_b.iter().all(|v| v.as_os_str() == b.as_os_str()),
            "thread b observed a redirect it did not set: {by_b:?}"
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

    /// Where one scanned source stands against a shared-env-var lock guard.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Standing {
        /// Never names the variable at all.
        Absent,
        /// Names it but never flips it — production read paths.
        ReadsOnly,
        /// Mutates it AND takes the crate-wide lock in real code.
        Locked,
        /// Mutates it without taking the crate-wide lock. The violation.
        Unlocked,
    }

    /// Classify one source file against `var`. THE predicate both structural
    /// guards below are built from, so the decoy tests exercise the real
    /// matcher rather than a restatement of it.
    ///
    /// Three properties, each closing a way a raw-text `contains` passed while
    /// the race was open:
    ///
    ///  1. **Code-only reference.** The `test_config_env` reference must survive
    ///     [`code_only`], so a doc comment — or a string literal — mentioning it
    ///     cannot satisfy the guard. That is the exact "a comment asserting a
    ///     guarantee the code lacks" evasion this module exists to disbelieve.
    ///  2. **Not keyed to a literal call shape.** Detection is the env-var NAME
    ///     (which lives in a string literal at every call site, hence read from
    ///     the RAW text) plus any `set_var`/`remove_var` in real code — so a
    ///     rustfmt-wrapped call, a `const KEY`, or a remove-only mutation is
    ///     still seen. `set_var("SCR1B3_CONFIG_DIR"` as one literal string was
    ///     none of those.
    ///  3. **Read-only files are not mutators**, so migrating a mutator onto the
    ///     helper never makes the scan look like it lost reach.
    fn classify(raw: &str, var: &str) -> Standing {
        if !raw.contains(var) {
            return Standing::Absent;
        }
        let code = code_only(raw);
        if !(code.contains("set_var") || code.contains("remove_var")) {
            return Standing::ReadsOnly;
        }
        if code.contains("test_config_env") {
            Standing::Locked
        } else {
            Standing::Unlocked
        }
    }

    /// Walk the crate and classify every file against `var`.
    ///
    /// Returns `(mentions, mutators, offenders)`. This file itself is skipped —
    /// it IS the lock, so it necessarily mutates without "referencing" itself.
    fn scan(var: &str) -> (usize, usize, Vec<std::path::PathBuf>) {
        let me = Path::new(file!())
            .file_name()
            .expect("this file has a name")
            .to_owned();
        let (mut mentions, mut mutators, mut offenders) = (0usize, 0usize, Vec::new());
        for path in crate_sources() {
            if path.file_name() == Some(me.as_os_str()) {
                continue;
            }
            let raw = std::fs::read_to_string(&path).expect("readable source");
            match classify(&raw, var) {
                Standing::Absent => {}
                Standing::ReadsOnly => mentions += 1,
                Standing::Locked => {
                    mentions += 1;
                    mutators += 1;
                }
                Standing::Unlocked => {
                    mentions += 1;
                    mutators += 1;
                    offenders.push(path);
                }
            }
        }
        (mentions, mutators, offenders)
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
    /// `SCR1B3_CONFIG_DIR` must reference `test_config_env` IN CODE, i.e. must
    /// be holding this crate-wide lock. Two files legitimately still call
    /// `set_var` themselves — `action_log` (which owns a second var of its own)
    /// and `theme_editor` (RAII restore-on-drop) — and both take the shared
    /// guard, which is what the reference proves.
    ///
    /// The anti-vacuity floor is anchored on the SCAN's REACH, not on today's
    /// mutator count. `checked >= 4` had zero headroom: migrating a mutator onto
    /// the helper — the stated direction of travel — would drop the count and
    /// fail with a message blaming the walk, so the natural repair was to lower
    /// the number until it passed, at which point the floor guards nothing. "The
    /// scan can see this variable at all" stays true as mutators migrate.
    #[test]
    fn every_config_dir_env_mutator_in_this_crate_takes_the_shared_lock() {
        const VAR: &str = "SCR1B3_CONFIG_DIR";
        let (mentions, mutators, offenders) = scan(VAR);

        assert!(
            mentions >= 2,
            "only {mentions} file(s) mention {VAR} — the scan is not reaching \
             the files it is meant to police, so a violation would pass unseen"
        );
        assert!(
            offenders.is_empty(),
            "these files redirect the process-global {VAR} without taking the \
             crate-wide lock in `test_config_env` (checked {mutators} mutator(s)). \
             A module-private `static Mutex<()>` excludes only that module; every \
             module here compiles into ONE test binary that cargo runs in \
             parallel, so the redirects clobber each other. Route them through \
             `test_config_env::with_config_dir` / `config_dir_env_guard`: {offenders:#?}"
        );
    }

    /// The same structural recurrence guard, for the second shared variable.
    ///
    /// `S4F3_DISABLE_TELEMETRY` is process-global exactly like the config dir,
    /// and it is mutated from modules (`reporting`, `issue_intake`) that compile
    /// into ONE parallel test binary. A module-private mutex — which is what
    /// `reporting` had — excludes only that module, so a peer could flip the
    /// opt-out mid-window. As with the config dir, a passing suite is not
    /// evidence: the race is intermittent by construction.
    #[test]
    fn every_telemetry_env_mutator_in_this_crate_takes_the_shared_lock() {
        const VAR: &str = "S4F3_DISABLE_TELEMETRY";
        let (mentions, mutators, offenders) = scan(VAR);

        assert!(
            mentions >= 2,
            "only {mentions} file(s) mention {VAR} — the scan is not reaching the \
             files it is meant to police, so a violation would pass unseen"
        );
        assert!(
            offenders.is_empty(),
            "these files mutate the process-global {VAR} without taking the \
             crate-wide lock in `test_config_env` (checked {mutators} mutator(s)). \
             A module-private `static Mutex<()>` excludes only that module; every \
             module here compiles into ONE test binary that cargo runs in \
             parallel, so the opt-out flips clobber each other. Route them \
             through `test_config_env::with_telemetry_opt_out` / \
             `telemetry_env_guard`: {offenders:#?}"
        );
    }

    /// The POSITIVE control. Without it every decoy test below would also pass
    /// against a `classify` that returned `Unlocked` for everything — a guard
    /// that rejects the whole crate is not a guard, and a decoy corpus with no
    /// known-accepted case can confirm anything.
    #[test]
    fn a_real_shared_lock_call_is_accepted() {
        let honest = r#"
            fn f() {
                let _g = crate::test_config_env::config_dir_env_guard();
                std::env::set_var("SCR1B3_CONFIG_DIR", "/tmp/x");
            }
        "#;
        assert_eq!(
            classify(honest, "SCR1B3_CONFIG_DIR"),
            Standing::Locked,
            "a real call to the shared guard must satisfy the check, or the \
             decoy tests are passing against a classifier that rejects \
             everything"
        );
    }

    /// A production file that only READS the variable is not a mutator and must
    /// not be dragged into the offender list.
    #[test]
    fn a_read_only_file_is_not_a_mutator() {
        let reader = r#"fn f() -> bool { std::env::var_os("SCR1B3_CONFIG_DIR").is_some() }"#;
        assert_eq!(
            classify(reader, "SCR1B3_CONFIG_DIR"),
            Standing::ReadsOnly,
            "reading the variable is not flipping it"
        );
    }

    /// Neither guard may be satisfiable by PROSE. Reverting a module to a
    /// private mutex while leaving a doc line that mentions `test_config_env` is
    /// the exact evasion this asserts is impossible — and it is the evasion the
    /// raw-text config-dir guard actually admitted.
    #[test]
    fn a_doc_comment_does_not_satisfy_either_guard() {
        for var in ["SCR1B3_CONFIG_DIR", "S4F3_DISABLE_TELEMETRY"] {
            let evasive = format!(
                r#"
                //! Serialised under the crate-wide lock (see `test_config_env`).
                static PRIVATE: Mutex<()> = Mutex::new(());
                fn f() {{ std::env::set_var("{var}", "1"); }}
            "#
            );
            assert_eq!(
                classify(&evasive, var),
                Standing::Unlocked,
                "a doc comment must NOT survive as a shared-lock reference — if \
                 it does, the recurrence guard for {var} is defeated by exactly \
                 the kind of comment it exists to disbelieve"
            );
        }
    }

    /// A string literal must not satisfy either guard either.
    #[test]
    fn a_string_literal_does_not_satisfy_either_guard() {
        for var in ["SCR1B3_CONFIG_DIR", "S4F3_DISABLE_TELEMETRY"] {
            let evasive = format!(
                r#"fn f() {{ panic!("use test_config_env"); std::env::set_var(K, "{var}"); }}"#
            );
            assert_eq!(
                classify(&evasive, var),
                Standing::Unlocked,
                "a string literal must not count as a call to the shared helper \
                 for {var}"
            );
        }
    }

    /// The detection half must not be keyed to one literal call shape. A
    /// rustfmt-wrapped call and a remove-only mutation are both real
    /// mutations that `set_var("SCR1B3_CONFIG_DIR"` as a single literal missed.
    #[test]
    fn a_wrapped_or_remove_only_mutation_is_still_seen() {
        let wrapped = r#"
            static PRIVATE: Mutex<()> = Mutex::new(());
            fn f() {
                std::env::set_var(
                    "SCR1B3_CONFIG_DIR",
                    dir.path(),
                );
            }
        "#;
        assert_eq!(
            classify(wrapped, "SCR1B3_CONFIG_DIR"),
            Standing::Unlocked,
            "a call rustfmt split across lines is still a mutation"
        );
        let remove_only = r#"
            static PRIVATE: Mutex<()> = Mutex::new(());
            fn f() { std::env::remove_var("SCR1B3_CONFIG_DIR"); }
        "#;
        assert_eq!(
            classify(remove_only, "SCR1B3_CONFIG_DIR"),
            Standing::Unlocked,
            "clearing the variable races with a peer's redirect just as setting \
             it does"
        );
    }

    /// The telemetry lock must actually exclude — the behavioural half of the
    /// structural guard above, mirroring `with_config_dir_is_mutually_exclusive`.
    ///
    /// Barrier-synchronised per round, and asserted after the scope, for the
    /// same two reasons as that test: without the rendezvous the threads ran
    /// end-to-end instead of racing, and an inline assertion failure would hang
    /// its peer on the barrier instead of failing the test.
    #[test]
    fn with_telemetry_opt_out_is_mutually_exclusive_across_threads() {
        let seen_by_a = std::sync::Arc::new(Mutex::new(Vec::<bool>::new()));
        let seen_by_b = std::sync::Arc::new(Mutex::new(Vec::<bool>::new()));
        let order = std::sync::Arc::new(Mutex::new(Vec::<Who>::new()));
        let gate = std::sync::Barrier::new(2);

        std::thread::scope(|s| {
            let (obs, ord) = (
                std::sync::Arc::clone(&seen_by_b),
                std::sync::Arc::clone(&order),
            );
            let gate = &gate;
            let t = s.spawn(move || {
                for _ in 0..ROUNDS {
                    gate.wait();
                    with_telemetry_opt_out(true, || {
                        ord.lock().unwrap().push(Who::B);
                        let set = std::env::var_os("S4F3_DISABLE_TELEMETRY").is_some();
                        obs.lock().unwrap().push(set);
                    });
                }
            });
            for _ in 0..ROUNDS {
                gate.wait();
                with_telemetry_opt_out(false, || {
                    order.lock().unwrap().push(Who::A);
                    let set = std::env::var_os("S4F3_DISABLE_TELEMETRY").is_some();
                    seen_by_a.lock().unwrap().push(set);
                });
            }
            t.join().expect("thread b");
        });

        let order = order.lock().unwrap();
        assert!(
            handovers(&order) >= 10,
            "the two threads never actually contended (only {} handover(s) in {} \
             windows) — one ran to completion before the other started, so the \
             exclusion assertions below were never exercised",
            handovers(&order),
            order.len()
        );
        let by_a = seen_by_a.lock().unwrap();
        let by_b = seen_by_b.lock().unwrap();
        assert_eq!(by_a.len(), ROUNDS, "thread a must complete every window");
        assert_eq!(by_b.len(), ROUNDS, "thread b must complete every window");
        assert!(
            by_a.iter().all(|set| !set),
            "a peer set the opt-out inside our window — the lock is not exclusive"
        );
        assert!(
            by_b.iter().all(|set| *set),
            "a peer cleared the opt-out inside our window — the lock is not \
             exclusive"
        );
    }

    /// Every observation happens inside ONE lock window, via
    /// [`with_config_dir_locked`]. The earlier version released the lock between
    /// establishing "absent" and reading the result, so a peer holding its own
    /// redirect in that gap failed this test for a reason that had nothing to do
    /// with the restore path — and it only ever looked stable because the
    /// exclusion tests above were not actually racing.
    #[test]
    fn with_config_dir_restores_an_absent_var() {
        let dir = std::env::temp_dir().join("scr1b3-lock-restore");
        let observed = {
            let _g = config_dir_env_guard();
            let prev = std::env::var_os("SCR1B3_CONFIG_DIR");
            std::env::remove_var("SCR1B3_CONFIG_DIR");
            let seen = with_config_dir_locked(&dir, || std::env::var_os("SCR1B3_CONFIG_DIR"));
            let after = std::env::var_os("SCR1B3_CONFIG_DIR");
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
