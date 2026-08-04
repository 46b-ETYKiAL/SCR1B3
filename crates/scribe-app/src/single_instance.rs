//! Single-instance ownership + command-line hand-off.
//!
//! ## The problem this closes
//!
//! SCR1B3 registers itself for file associations (`integration::register_file_types`),
//! so Explorer launches `scr1b3.exe <path>` for every file the user opens. Without
//! an instance guard that means **selecting five files and pressing Enter starts
//! five separate processes** — five windows, five wgpu devices, five session
//! writers racing over the same `scr1b3.toml` and the same session manifest.
//!
//! With this module the first launch becomes the *primary*; every later launch is
//! a *secondary* that hands its paths to the primary and exits immediately.
//!
//! ## Why a filesystem hand-off and not a named mutex + `WM_COPYDATA`
//!
//! The conventional Windows shape is `CreateMutexW` + `FindWindow` +
//! `WM_COPYDATA` + `SetForegroundWindow`. Every one of those is a raw Win32 call,
//! and this crate is `#![forbid(unsafe_code)]` — a *forbid* (unlike `deny`) cannot
//! be re-opened by an inner `#![allow(unsafe_code)]`, so the C0PL4ND-style
//! "quarantine the FFI in a `mod imp`" pattern is structurally unavailable here.
//! Weakening the crate attribute to buy an IPC mechanism is the wrong trade.
//!
//! So the three jobs are done with safe primitives instead:
//!
//! | Job | Conventional Win32 | What we use |
//! |---|---|---|
//! | mutual exclusion | `CreateMutexW` | an exclusively-shared lock FILE ([`acquire`]) |
//! | argv transport | `WM_COPYDATA` | an atomically-renamed request file ([`forward`]) |
//! | raise the window | `SetForegroundWindow` | `egui::ViewportCommand::{Minimized(false), Focus}` |
//!
//! The lock is *not* a PID file. It is a real kernel-enforced share-mode lock:
//! the primary holds the handle open for its whole life with
//! `share_mode(FILE_SHARE_READ)`, so any other process that asks for write access
//! gets `ERROR_SHARING_VIOLATION`. The OS closes the handle when the process
//! dies — including a hard crash or a `TerminateProcess` — so a stale lock is
//! impossible by construction. That is the property a PID file cannot give.
//!
//! `ViewportCommand::Focus` is not a weaker `SetForegroundWindow`: winit's
//! `focus_window` performs the documented synthetic-ALT foreground-permission
//! dance before calling `SetForegroundWindow`, which is the same trick the
//! hand-written version needs. And eframe explicitly paints minimized/invisible
//! windows directly (see its `is_invisible_or_minimized` handling), so a queued
//! viewport command still lands while the window is minimized.
//!
//! ## Platform scope
//!
//! [`acquire`] takes the real lock on Windows and reports [`Startup::Primary`]
//! unconditionally elsewhere. This is deliberate and is *not* a stubbed-out
//! promise: the multi-launch storm above is created by Windows file associations,
//! `std` exposes no portable exclusive-open, and pulling in a `libc`/`unsafe`
//! dependency to `flock` on Unix would buy a behaviour nobody reported missing.
//! The hand-off transport ([`forward`] / [`drain`]) is fully portable and tested
//! on every host; only the lock is Windows-only.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Environment escape hatch: set to `1` to launch an additional, fully
/// independent instance (skips the guard entirely). Deliberately an env var
/// rather than a config key — a config key would be sticky, and the case for
/// "give me a second window right now" is per-launch.
pub const NEW_INSTANCE_ENV: &str = "SCR1B3_NEW_INSTANCE";

/// `ERROR_SHARING_VIOLATION` — another process holds the file open in a mode
/// that excludes our requested access. This is the "an instance is already
/// running" signal.
const ERROR_SHARING_VIOLATION: i32 = 32;
/// `ERROR_LOCK_VIOLATION` — the same class of "someone else owns it" refusal.
const ERROR_LOCK_VIOLATION: i32 = 33;
/// `FILE_SHARE_READ`. The primary lets others READ the lock file (so a
/// diagnostic `type instance.lock` works) but never WRITE it — which is exactly
/// what a second launch asks for, and therefore what gets refused.
#[cfg(windows)]
const FILE_SHARE_READ: u32 = 0x0000_0001;

/// One forwarded launch: the files a secondary process was asked to open, plus
/// its optional `PATH:LINE[:COLUMN]` jump target.
///
/// An EMPTY `paths` list is meaningful, not a no-op: it is a bare second launch
/// (the user double-clicked the icon or the taskbar tile), and the primary
/// answers it by raising its existing window.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    /// Paths to open, in the order the OS supplied them.
    #[serde(default)]
    pub paths: Vec<String>,
    /// `(line, column)` jump target from a `file:42:10` argument.
    #[serde(default)]
    pub jump: Option<(usize, Option<usize>)>,
}

/// The role this process took at startup.
#[derive(Debug)]
pub enum Startup {
    /// We own the instance. The [`InstanceLock`] must be held for the whole
    /// process lifetime — dropping it hands ownership to the next launch.
    Primary(InstanceLock),
    /// Another instance already owns the lock; this process should [`forward`]
    /// its arguments and exit.
    Secondary,
}

/// RAII holder for the instance lock file's OS handle. Ownership lasts exactly
/// as long as this value: the kernel releases the handle on drop *and* on
/// process death, so there is no stale-lock recovery path to get wrong.
#[derive(Debug)]
pub struct InstanceLock {
    /// Held open purely for its share-mode. Never read or written.
    _file: fs::File,
}

/// Verdict for a failed lock open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockVerdict {
    /// Another instance owns the lock — become a secondary.
    Busy,
    /// A real I/O failure (no permission, bad path, disk gone). The caller must
    /// NOT silently treat this as "an instance is running".
    Fatal,
}

/// Classify a lock-open failure by its raw OS error code.
///
/// Kept pure + separate from the `fs` call so the mapping is testable on every
/// host: `std` collapses both sharing and lock violations into
/// `ErrorKind::PermissionDenied`, which is also what a genuinely unreadable
/// directory yields — so matching on `ErrorKind` would make a permissions
/// misconfiguration silently masquerade as "already running" and swallow the
/// user's files.
#[must_use]
pub fn classify_lock_error(raw_os_error: Option<i32>) -> LockVerdict {
    match raw_os_error {
        Some(ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION) => LockVerdict::Busy,
        _ => LockVerdict::Fatal,
    }
}

/// Whether the caller asked for an independent extra instance.
///
/// Pure over the raw env value so the parsing rule ("exactly `1`", not merely
/// "set") is pinned by a test rather than by whatever the shell happened to do.
#[must_use]
pub fn wants_new_instance(env_value: Option<&str>) -> bool {
    matches!(env_value.map(str::trim), Some("1"))
}

/// Root directory for the instance lock + hand-off queue, derived from the
/// resolved config directory.
#[must_use]
pub fn instance_root(config_dir: &Path) -> PathBuf {
    config_dir.join("instance")
}

/// Path of the lock file inside `root`.
#[must_use]
fn lock_path(root: &Path) -> PathBuf {
    root.join("instance.lock")
}

/// Directory holding pending hand-off requests inside `root`.
#[must_use]
fn handoff_dir(root: &Path) -> PathBuf {
    root.join("handoff")
}

/// Try to become the single instance.
///
/// Creates `root` if needed, then takes an exclusive-write share-mode handle on
/// the lock file. Returns [`Startup::Secondary`] when another live process
/// already holds it, and an error for any *other* failure (which the caller
/// should treat as "run normally", never as "an instance exists").
///
/// # Errors
///
/// Returns the underlying [`io::Error`] when `root` cannot be created or the
/// lock file cannot be opened for a reason other than an existing owner.
pub fn acquire(root: &Path) -> io::Result<Startup> {
    fs::create_dir_all(root)?;
    fs::create_dir_all(handoff_dir(root))?;
    open_lock(&lock_path(root))
}

#[cfg(windows)]
fn open_lock(path: &Path) -> io::Result<Startup> {
    use std::os::windows::fs::OpenOptionsExt;

    match fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .share_mode(FILE_SHARE_READ)
        .open(path)
    {
        Ok(file) => Ok(Startup::Primary(InstanceLock { _file: file })),
        Err(err) => match classify_lock_error(err.raw_os_error()) {
            LockVerdict::Busy => Ok(Startup::Secondary),
            LockVerdict::Fatal => Err(err),
        },
    }
}

#[cfg(not(windows))]
fn open_lock(path: &Path) -> io::Result<Startup> {
    // See the module docs: no portable safe exclusive-open exists in `std`, and
    // the multi-launch storm is a Windows-file-association behaviour. We still
    // create the file so the hand-off root looks identical on every platform.
    let file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)?;
    Ok(Startup::Primary(InstanceLock { _file: file }))
}

/// Hand `request` to the running primary.
///
/// The request is written to a temporary name and then **renamed** into place,
/// so the primary can never observe a half-written file: `fs::rename` within one
/// directory is atomic on both NTFS and POSIX. The file name is prefixed with a
/// monotonically-increasing timestamp so [`drain`] returns concurrent launches
/// in submission order.
///
/// # Errors
///
/// Returns the underlying [`io::Error`] if the hand-off directory cannot be
/// created or written.
pub fn forward(root: &Path, request: &Request) -> io::Result<()> {
    let dir = handoff_dir(root);
    fs::create_dir_all(&dir)?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let stem = format!("{stamp:039}-{}", std::process::id());
    let tmp = dir.join(format!("{stem}.tmp"));
    let final_path = dir.join(format!("{stem}.json"));
    let body = serde_json::to_string(request)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    fs::write(&tmp, body)?;
    fs::rename(&tmp, &final_path)
}

/// Collect and consume every pending hand-off request in submission order.
///
/// A request file is removed whether or not it parsed: a corrupt entry (a
/// truncated write from a killed secondary, say) must not wedge the queue and
/// re-trigger a raise on every single frame forever. `.tmp` files are ignored —
/// they are in-flight writes that have not been renamed into place yet.
#[must_use]
pub fn drain(root: &Path) -> Vec<Request> {
    let dir = handoff_dir(root);
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    // Names are zero-padded-nanosecond prefixed, so lexicographic == chronological.
    files.sort();
    let mut out = Vec::new();
    for path in files {
        if let Ok(body) = fs::read_to_string(&path) {
            if let Ok(req) = serde_json::from_str::<Request>(&body) {
                out.push(req);
            }
        }
        let _ = fs::remove_file(&path);
    }
    out
}

/// Cheap "is anything waiting?" probe for the wake-up watcher, so the primary
/// only has to be repainted when there is genuinely something to pick up.
#[must_use]
pub fn pending(root: &Path) -> bool {
    fs::read_dir(handoff_dir(root)).is_ok_and(|mut it| {
        it.any(|e| e.is_ok_and(|e| e.path().extension().is_some_and(|x| x == "json")))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("instance");
        (dir, root)
    }

    #[test]
    fn classify_lock_error_separates_busy_from_fatal() {
        // The whole point of classifying on the RAW code: `std` maps a sharing
        // violation and a plain permission failure to the same ErrorKind, and
        // treating the latter as "already running" would drop the user's files
        // on the floor.
        assert_eq!(classify_lock_error(Some(32)), LockVerdict::Busy);
        assert_eq!(classify_lock_error(Some(33)), LockVerdict::Busy);
        assert_eq!(classify_lock_error(Some(5)), LockVerdict::Fatal); // ACCESS_DENIED
        assert_eq!(classify_lock_error(Some(3)), LockVerdict::Fatal); // PATH_NOT_FOUND
        assert_eq!(classify_lock_error(None), LockVerdict::Fatal);
    }

    #[test]
    fn wants_new_instance_requires_exactly_one() {
        assert!(wants_new_instance(Some("1")));
        assert!(wants_new_instance(Some(" 1 ")));
        assert!(!wants_new_instance(None));
        assert!(!wants_new_instance(Some("0")));
        assert!(!wants_new_instance(Some("")));
        // "set to anything" would make an accidental `SCR1B3_NEW_INSTANCE=false`
        // silently defeat the guard.
        assert!(!wants_new_instance(Some("false")));
        assert!(!wants_new_instance(Some("true")));
    }

    #[test]
    fn instance_root_is_under_the_config_dir() {
        let r = instance_root(Path::new("/cfg"));
        assert!(
            r.starts_with("/cfg"),
            "{r:?} must live under the config dir"
        );
        assert_ne!(r, PathBuf::from("/cfg"), "must not BE the config dir");
    }

    #[cfg(windows)]
    #[test]
    fn a_second_acquire_while_the_first_is_held_is_secondary() {
        // The behaviour the whole module exists for: while one process holds the
        // lock, the next launch must NOT come up as another primary.
        let (_tmp, root) = root();
        let first = acquire(&root).expect("first acquire");
        assert!(
            matches!(first, Startup::Primary(_)),
            "the first launch must own the instance"
        );
        let second = acquire(&root).expect("second acquire");
        assert!(
            matches!(second, Startup::Secondary),
            "a launch while the lock is held must be a secondary"
        );
    }

    #[cfg(windows)]
    #[test]
    fn releasing_the_lock_lets_the_next_launch_become_primary() {
        // Crash-safety proxy: the OS drops the handle when the owner dies, so a
        // released lock must be immediately re-acquirable with no stale-file
        // cleanup step. If this fails, a crash would lock the user out of their
        // own editor until they deleted a file by hand.
        let (_tmp, root) = root();
        let first = acquire(&root).expect("first acquire");
        assert!(matches!(first, Startup::Primary(_)));
        drop(first);
        let next = acquire(&root).expect("re-acquire");
        assert!(
            matches!(next, Startup::Primary(_)),
            "a released lock must be re-acquirable without manual cleanup"
        );
    }

    #[test]
    fn forward_then_drain_delivers_the_paths_and_the_jump() {
        let (_tmp, root) = root();
        fs::create_dir_all(&root).unwrap();
        let req = Request {
            paths: vec!["C:/a/one.rs".into(), "C:/a/two.md".into()],
            jump: Some((42, Some(10))),
        };
        forward(&root, &req).expect("forward");
        let got = drain(&root);
        assert_eq!(
            got,
            vec![req],
            "the primary must receive exactly what was sent"
        );
    }

    #[test]
    fn drain_consumes_so_the_same_launch_is_not_replayed() {
        // Without removal the primary would re-open the same files and re-raise
        // its window on every frame, forever.
        let (_tmp, root) = root();
        fs::create_dir_all(&root).unwrap();
        forward(
            &root,
            &Request {
                paths: vec!["one.txt".into()],
                jump: None,
            },
        )
        .unwrap();
        assert_eq!(drain(&root).len(), 1);
        assert!(
            drain(&root).is_empty(),
            "a drained request must not come back"
        );
        assert!(!pending(&root), "the queue must read empty after a drain");
    }

    #[test]
    fn requests_are_delivered_in_submission_order() {
        let (_tmp, root) = root();
        fs::create_dir_all(&root).unwrap();
        for name in ["first.txt", "second.txt", "third.txt"] {
            forward(
                &root,
                &Request {
                    paths: vec![name.to_string()],
                    jump: None,
                },
            )
            .unwrap();
            // Distinct nanosecond stamps; the sleep only guards against a
            // coarse clock on the test host.
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let got: Vec<String> = drain(&root).into_iter().map(|r| r.paths.join("")).collect();
        assert_eq!(got, vec!["first.txt", "second.txt", "third.txt"]);
    }

    #[test]
    fn a_corrupt_request_is_discarded_and_removed_not_left_to_wedge_the_queue() {
        let (_tmp, root) = root();
        let dir = handoff_dir(&root);
        fs::create_dir_all(&dir).unwrap();
        // A truncated write from a secondary that was killed mid-forward.
        fs::write(
            dir.join("00000000000000000000000000000000000001-9.json"),
            "{\"pa",
        )
        .unwrap();
        forward(
            &root,
            &Request {
                paths: vec!["good.txt".into()],
                jump: None,
            },
        )
        .unwrap();
        let got = drain(&root);
        assert_eq!(
            got.len(),
            1,
            "the corrupt entry must be skipped, the good one delivered"
        );
        assert_eq!(got[0].paths, vec!["good.txt".to_string()]);
        assert!(
            !pending(&root),
            "the corrupt entry must also be REMOVED — otherwise it re-raises the \
             window on every frame forever"
        );
    }

    #[test]
    fn drain_ignores_an_in_flight_tmp_write() {
        // `forward` writes `.tmp` then renames. A `.tmp` observed mid-write is a
        // partial file; reading it would deliver a truncated path list.
        let (_tmp, root) = root();
        let dir = handoff_dir(&root);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("000000001-1.tmp"), "{\"paths\":[\"half").unwrap();
        assert!(drain(&root).is_empty(), "a .tmp file must not be consumed");
        assert!(
            dir.join("000000001-1.tmp").exists(),
            "a .tmp file must not be deleted out from under the writer"
        );
    }

    #[test]
    fn a_bare_relaunch_forwards_an_empty_request_rather_than_nothing() {
        // An empty request is the "raise the window" signal. If it were dropped,
        // double-clicking the icon while SCR1B3 runs would appear to do nothing.
        let (_tmp, root) = root();
        fs::create_dir_all(&root).unwrap();
        forward(&root, &Request::default()).unwrap();
        let got = drain(&root);
        assert_eq!(got.len(), 1, "a bare relaunch must still reach the primary");
        assert!(got[0].paths.is_empty());
    }

    #[test]
    fn paths_survive_unicode_spaces_and_newlines() {
        // A Linux filename may legally contain a newline, and a Windows path may
        // contain spaces and non-ASCII. A line-oriented transport would corrupt
        // both; assert the transport is lossless.
        let (_tmp, root) = root();
        fs::create_dir_all(&root).unwrap();
        let nasty = vec![
            "C:/Users/.46b_/My Documents/ノート.md".to_string(),
            "/tmp/line\nbreak.txt".to_string(),
            "/tmp/quote\"and\\slash.txt".to_string(),
        ];
        forward(
            &root,
            &Request {
                paths: nasty.clone(),
                jump: None,
            },
        )
        .unwrap();
        assert_eq!(drain(&root)[0].paths, nasty);
    }

    #[test]
    fn drain_on_a_missing_root_is_empty_not_a_panic() {
        let (_tmp, root) = root();
        // Never created.
        assert!(drain(&root).is_empty());
        assert!(!pending(&root));
    }

    #[test]
    fn pending_is_true_exactly_while_a_request_waits() {
        let (_tmp, root) = root();
        fs::create_dir_all(&root).unwrap();
        assert!(!pending(&root), "an empty queue must not wake the primary");
        forward(&root, &Request::default()).unwrap();
        assert!(pending(&root), "a queued request must wake the primary");
        let _ = drain(&root);
        assert!(!pending(&root));
    }
}
