//! Pure generation of the per-user (HKCU) registry entries that register SCR1B3
//! as a file handler. The exe path + roots are injected, so the full key/value
//! set is unit-testable on ANY OS (the executor in `windows.rs` is the only
//! Windows-only part). Compiled under `#[cfg(any(test, windows))]`.

use scribe_core::config::ClaimType;

/// The PRODUCTION roots, defined once.
///
/// `windows.rs` (the executor) and `assoc_stamp.rs` (the startup convergence
/// check) both have to name them, and the stamp is a digest of the entries built
/// from them — so two hard-coded copies drifting apart would silently make the
/// stamp describe a registration nobody performs. One definition removes that
/// failure mode by construction.
pub(crate) const CLASS_ROOT: &str = "Software\\Classes";
/// See [`CLASS_ROOT`].
pub(crate) const APP_ROOT: &str = "Software\\SCR1B3";
/// See [`CLASS_ROOT`]. The value name under `Software\RegisteredApplications`.
pub(crate) const APP_NAME: &str = "SCR1B3";

/// The `RegisteredApplications` value name used by the throwaway-root tests.
///
/// Deliberately NOT `"SCR1B3"`: that key is a fixed Windows location shared with
/// a real installation, so the value name is the only thing keeping a test out
/// of the user's actual registration. Defined once, here, so the entry builder
/// and the `#[ignore]`d integration test that cleans up after it cannot drift
/// apart — a test that registered one name and deleted another would either
/// leak a value or delete someone else's.
#[cfg(test)]
pub(crate) const TEST_APP_NAME: &str = "SCR1B3-Test";

/// The file-name half of `exe`, used as the `Applications\<name>` key segment.
///
/// Splits on `\`, `/` **and** `]`. The first two are the obvious path
/// separators. The third is the WiX dialect: the MSI writes these same keys with
/// the install directory expressed as a property token, so its "path" is
/// `[APPLICATIONFOLDER]scr1b3.exe` — the `]` terminates the directory half and
/// the file name follows it. Without that rule the installer would create
/// `Applications\[APPLICATIONFOLDER]scr1b3.exe`, a key Windows never consults,
/// and `wix_installer_registers_what_the_app_registers` would be comparing the
/// app's real key against an installer key that does not exist.
pub(crate) fn exe_file_name(exe: &str) -> &str {
    match exe.rfind(['\\', '/', ']']) {
        Some(i) => &exe[i + 1..],
        None => exe,
    }
}

/// The `Software\Classes\Applications\<exe>` key for `exe` under `class_root`.
fn applications_key(class_root: &str, exe: &str) -> String {
    format!("{class_root}\\Applications\\{}", exe_file_name(exe))
}

/// The `Software\Classes\*\shell\SCR1B3` key ("Edit with SCR1B3" on EVERY file)
/// under `class_root`.
fn star_verb_key(class_root: &str) -> String {
    format!("{class_root}\\*\\shell\\SCR1B3")
}

/// The `PerceivedType` every claimed group is: all five are text formats, and
/// the value is what makes Explorer/Search treat the extension as text.
const PERCEIVED_TYPE: &str = "text";

/// The "Edit with SCR1B3" context-menu verb label.
const STAR_VERB_LABEL: &str = "Edit with SCR1B3";

/// One registry write: a key path under HKCU, a value name (`""` = the key's
/// default value), and the string data.
///
/// `claim` attributes the write to the [`ClaimType`] it registers, or `None`
/// for the SHARED app-level writes (`Capabilities`,
/// `RegisteredApplications`) that every claim depends on. Without this
/// attribution a partial failure could not be reported honestly — the caller
/// could only say "everything worked" or "everything failed", which is exactly
/// how a failed registration came to be reported as a success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RegEntry {
    pub key: String,
    pub name: String,
    pub data: String,
    pub claim: Option<&'static str>,
}

/// Build the HKCU registry entries registering `types` with `exe` as the open
/// handler. `class_root` is normally `Software\Classes` and `app_root`
/// `Software\SCR1B3`; a test passes throwaway roots so it never touches real
/// associations. The writes are ADDITIVE — each extension's `OpenWithProgids`
/// gets our ProgID added (it never overwrites the user's existing default), and
/// `Capabilities` + `RegisteredApplications` make SCR1B3 a first-class entry in
/// the Default Apps UI.
///
/// `app_name` is the VALUE name written under `Software\RegisteredApplications`;
/// production passes `"SCR1B3"`. It is a parameter because that key is a
/// Windows-defined location — unlike the other two roots it CANNOT be relocated
/// into a throwaway subtree, so scoping the value name is the only way a test
/// stays out of the real registration. It was hard-coded, which meant the
/// `#[ignore]`d Windows integration test overwrote the real user registration
/// and then DELETED it on cleanup, removing SCR1B3 from Settings ▸ Default Apps
/// on any machine where it was actually installed.
pub(crate) fn registry_entries(
    types: &[ClaimType],
    exe: &str,
    class_root: &str,
    app_root: &str,
    app_name: &str,
) -> Vec<RegEntry> {
    let mut out = Vec::new();
    let apps = applications_key(class_root, exe);

    // Capabilities + RegisteredApplications (one set, always — makes SCR1B3
    // appear in Settings ▸ Default Apps).
    out.push(RegEntry {
        key: format!("{app_root}\\Capabilities"),
        name: "ApplicationName".into(),
        data: "SCR1B3".into(),
        claim: None,
    });
    out.push(RegEntry {
        key: format!("{app_root}\\Capabilities"),
        name: "ApplicationDescription".into(),
        data: "Fast, telemetry-free code & text editor.".into(),
        claim: None,
    });
    out.push(RegEntry {
        key: "Software\\RegisteredApplications".into(),
        name: app_name.into(),
        data: format!("{app_root}\\Capabilities"),
        claim: None,
    });

    // The app-level surfaces that make SCR1B3 appear in Explorer's "Open with"
    // submenu and in the right-click menu. They only make sense once something
    // is actually claimed: with an empty claim set SCR1B3 advertises no
    // `SupportedTypes`, so an `Applications\scr1b3.exe` open-command would be an
    // app declaring it opens files it has registered none of. Gating here is
    // also what keeps `empty_types_still_registers_the_app_capabilities`
    // meaningful — the app entry stays present, the file-opening claims do not.
    if !types.is_empty() {
        // `Applications\<exe>` + FriendlyAppName: the name Explorer shows in the
        // "Open with" list. Without it Windows falls back to the raw exe name.
        out.push(RegEntry {
            key: apps.clone(),
            name: "FriendlyAppName".into(),
            data: "SCR1B3".into(),
            claim: None,
        });
        out.push(RegEntry {
            key: format!("{apps}\\shell\\open\\command"),
            name: String::new(),
            data: format!("\"{exe}\" \"%1\""),
            claim: None,
        });
        // "Edit with SCR1B3" on EVERY file type (`*`), the way an editor is
        // expected to behave for the extensions it does not claim outright.
        let verb = star_verb_key(class_root);
        out.push(RegEntry {
            key: verb.clone(),
            name: String::new(),
            data: STAR_VERB_LABEL.into(),
            claim: None,
        });
        out.push(RegEntry {
            key: verb.clone(),
            name: "Icon".into(),
            data: format!("{exe},0"),
            claim: None,
        });
        out.push(RegEntry {
            key: format!("{verb}\\command"),
            name: String::new(),
            data: format!("\"{exe}\" \"%1\""),
            claim: None,
        });
    }

    for t in types {
        let progid = t.windows_progid();
        let claim = Some(t.key());
        // ProgID: the open command (`"<exe>" "%1"`), an icon, and a friendly name.
        out.push(RegEntry {
            key: format!("{class_root}\\{progid}\\shell\\open\\command"),
            name: String::new(),
            data: format!("\"{exe}\" \"%1\""),
            claim,
        });
        out.push(RegEntry {
            key: format!("{class_root}\\{progid}\\DefaultIcon"),
            name: String::new(),
            data: format!("{exe},0"),
            claim,
        });
        out.push(RegEntry {
            key: format!("{class_root}\\{progid}"),
            name: String::new(),
            data: "SCR1B3 document".into(),
            claim,
        });
        for ext in t.windows_extensions() {
            // Additive: register our ProgID as an "open with" option for the ext.
            out.push(RegEntry {
                key: format!("{class_root}\\.{ext}\\OpenWithProgids"),
                name: progid.to_string(),
                data: String::new(),
                claim,
            });
            // And declare the association under Capabilities for the Default
            // Apps UI.
            out.push(RegEntry {
                key: format!("{app_root}\\Capabilities\\FileAssociations"),
                name: format!(".{ext}"),
                data: progid.to_string(),
                claim,
            });
            // `Applications\<exe>\SupportedTypes`: what puts SCR1B3 in the
            // "Open with ▸ Choose another app" list for this extension. Without
            // it Windows only offers apps it has seen open the type before.
            out.push(RegEntry {
                key: format!("{apps}\\SupportedTypes"),
                name: format!(".{ext}"),
                data: String::new(),
                claim,
            });
            // `PerceivedType = text` on the extension key. Written under HKCU,
            // which is a per-user OVERLAY of HKCR — so `unregister_entries`
            // removing it restores whatever the machine hive says rather than
            // erasing a system default. That reversibility is the reason it is
            // safe to write a value on a key SCR1B3 does not own.
            out.push(RegEntry {
                key: format!("{class_root}\\.{ext}"),
                name: "PerceivedType".into(),
                data: PERCEIVED_TYPE.into(),
                claim,
            });
        }
    }
    out
}

/// One registry REMOVAL: either a whole key (and its subtree) or a single value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RegDelete {
    /// The key to remove, or the key holding the value to remove.
    pub key: String,
    /// `None` removes the whole KEY and its subtree; `Some(name)` removes just
    /// that value and leaves the key (and any other app's values) alone.
    pub name: Option<String>,
}

/// Build the removals that bring the registry into line with `desired` — the
/// half that was missing entirely, so registration was write-only and every
/// association SCR1B3 ever wrote survived forever.
///
/// Two distinct classes of stale state are cleaned:
///
/// 1. **Un-claimed groups.** Every [`ClaimType`] NOT in `desired` has its ProgID
///    key, its per-extension `OpenWithProgids` value, its
///    `Capabilities\FileAssociations` value, its `SupportedTypes` value and its
///    `PerceivedType` overlay removed. This is what makes un-ticking a group in
///    Settings actually remove something.
///
/// 2. **Extensions that MIGRATED between groups.** Seven extensions moved from
///    `SourceCode` to `ConfigData`; a user who registered before the move still
///    has `SCR1B3.source` sitting in those extensions' `OpenWithProgids`, and
///    because both groups are still claimed class 1 never touches it. So for
///    every claimed extension we also strip every OTHER SCR1B3 ProgID from its
///    `OpenWithProgids`. `every_extension_belongs_to_exactly_one_group` is what
///    makes that safe: a ProgID other than the extension's current owner is, by
///    construction, one we wrote under a previous layout and never one we are
///    about to re-write.
///
/// Only ever names keys/values SCR1B3 itself writes — a `SCR1B3.*` ProgID, our
/// own app root, our own `Applications\<exe>` key, or the reversible per-user
/// `PerceivedType` overlay. It never removes another application's entry.
pub(crate) fn unregister_entries(
    desired: &[ClaimType],
    exe: &str,
    class_root: &str,
    app_root: &str,
) -> Vec<RegDelete> {
    let mut out = Vec::new();
    let apps = applications_key(class_root, exe);

    for stale in ClaimType::ALL.into_iter().filter(|c| !desired.contains(c)) {
        let progid = stale.windows_progid();
        // The whole ProgID subtree (open command, icon, friendly name).
        out.push(RegDelete {
            key: format!("{class_root}\\{progid}"),
            name: None,
        });
        for ext in stale.windows_extensions() {
            out.push(RegDelete {
                key: format!("{class_root}\\.{ext}\\OpenWithProgids"),
                name: Some(progid.to_string()),
            });
            out.push(RegDelete {
                key: format!("{app_root}\\Capabilities\\FileAssociations"),
                name: Some(format!(".{ext}")),
            });
            out.push(RegDelete {
                key: format!("{apps}\\SupportedTypes"),
                name: Some(format!(".{ext}")),
            });
            out.push(RegDelete {
                key: format!("{class_root}\\.{ext}"),
                name: Some("PerceivedType".into()),
            });
        }
    }

    // The app-level surfaces are the exact mirror of the `!types.is_empty()`
    // gate in `registry_entries`: they are written only while SOMETHING is
    // claimed, so they must be removed once NOTHING is. Without this, a user who
    // unticks every group keeps "Edit with SCR1B3" on the context menu of every
    // file on the machine forever — the same never-removed leak as the ProgIDs,
    // just on the surface most visible to them.
    if desired.is_empty() {
        out.push(RegDelete {
            key: apps.clone(),
            name: None,
        });
        out.push(RegDelete {
            key: star_verb_key(class_root),
            name: None,
        });
    }

    // Class 2: strip migrated-away ProgIDs from the extensions we DO claim.
    for d in desired {
        let owner = d.windows_progid();
        for ext in d.windows_extensions() {
            for other in ClaimType::ALL {
                let stale_progid = other.windows_progid();
                if stale_progid == owner {
                    continue;
                }
                out.push(RegDelete {
                    key: format!("{class_root}\\.{ext}\\OpenWithProgids"),
                    name: Some(stale_progid.to_string()),
                });
            }
        }
    }

    out
}

/// `reg.exe delete` exits non-zero when the key or value is already absent. For
/// an unregister that is the SUCCESS case — there was nothing to remove — so
/// classifying it as a failure would make every clean run report hundreds of
/// bogus errors and would stop the startup refresh ever converging.
///
/// Deliberately does NOT treat an EMPTY stderr as benign: a non-zero exit that
/// says nothing is an unknown failure, and swallowing it is exactly the silent
/// pass this module already had to be fixed for once.
pub(crate) fn delete_error_is_benign(stderr: &str) -> bool {
    let s = stderr.to_ascii_lowercase();
    s.contains("unable to find") || s.contains("cannot find")
}

/// Append an honest note when stale entries could not be removed.
///
/// Kept OUT of [`summarize`] on purpose: a cleanup failure does not mean a claim
/// failed to register, so folding it into `registered` would under-report a
/// registration that actually landed. But leaving it entirely unsaid would be
/// the same class of quiet dishonesty as the hardcoded success string, so the
/// count is surfaced in the message.
pub(crate) fn append_cleanup_note(message: String, failed_deletes: usize) -> String {
    if failed_deletes == 0 {
        return message;
    }
    let entries = if failed_deletes == 1 {
        "entry"
    } else {
        "entries"
    };
    format!("{message} ({failed_deletes} stale registry {entries} could not be removed.)")
}

/// FNV-1a (64-bit). A tiny, fully specified, dependency-free digest.
///
/// `DefaultHasher` would have done the job but is explicitly NOT guaranteed
/// stable across Rust releases, and this value is PERSISTED — a toolchain bump
/// would silently invalidate every user's stamp and re-run a full registration.
/// FNV-1a is fixed by its specification, so the stamp means the same thing
/// forever.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// A digest of the EXACT registry work a registration would perform.
///
/// The startup short-circuit compares this against the stamp of the last
/// successful run. Deriving it from the emitted entries + removals — rather than
/// from a hand-maintained schema version — means it is drift-proof by
/// construction: any change to what we write or remove changes the digest, so a
/// user upgrading into a build that adds a surface (as this change does) is
/// re-registered automatically instead of being pinned to the old, matching
/// fingerprint forever.
pub(crate) fn entries_digest(entries: &[RegEntry], deletes: &[RegDelete]) -> String {
    let mut buf = String::new();
    for e in entries {
        buf.push_str(&e.key);
        buf.push('\u{1}');
        buf.push_str(&e.name);
        buf.push('\u{1}');
        buf.push_str(&e.data);
        buf.push('\u{2}');
    }
    buf.push('\u{3}');
    for d in deletes {
        buf.push_str(&d.key);
        buf.push('\u{1}');
        buf.push_str(d.name.as_deref().unwrap_or("*KEY*"));
        buf.push('\u{2}');
    }
    format!("{:016x}", fnv1a64(buf.as_bytes()))
}

/// One registry write that failed, carrying the claim it belonged to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FailedWrite {
    /// The claim this write registered, or `None` for a shared app-level write.
    pub claim: Option<&'static str>,
    /// The registry key that could not be written.
    pub key: String,
    /// The trimmed `reg.exe` stderr.
    pub err: String,
}

/// The honest outcome of a registration attempt: which claims actually landed,
/// and a message that REFLECTS the failures rather than asserting success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RegistrationOutcome {
    pub registered: Vec<String>,
    pub message: String,
}

/// PURE: derive the registered-claim list and the user-facing message from what
/// actually succeeded.
///
/// This exists because the message used to be a hardcoded success string
/// returned regardless of `failed` — so a registration in which EVERY
/// `reg.exe` write failed still told the user "SCR1B3 is registered." Being
/// pure, it is unit-testable on any OS, which is what lets the fake-success
/// regression be pinned by a test.
///
/// A failed SHARED write (`Capabilities` / `RegisteredApplications`) fails the
/// whole registration: without those keys SCR1B3 does not appear in the Default
/// Apps list at all, so no per-type claim can be honestly called registered.
pub(crate) fn summarize(requested: &[ClaimType], failures: &[FailedWrite]) -> RegistrationOutcome {
    let shared_failed = failures.iter().any(|f| f.claim.is_none());
    let failed_claims: Vec<&str> = requested
        .iter()
        .map(|t| t.key())
        .filter(|k| failures.iter().any(|f| f.claim == Some(*k)))
        .collect();

    let registered: Vec<String> = if shared_failed {
        Vec::new()
    } else {
        requested
            .iter()
            .map(|t| t.key())
            .filter(|k| !failed_claims.contains(k))
            .map(str::to_string)
            .collect()
    };

    let first_err = failures
        .first()
        .map(|f| format!(" First error: {} ({}).", f.err, f.key))
        .unwrap_or_default();
    let n = failures.len();
    let writes = if n == 1 { "write" } else { "writes" };

    let message = if failures.is_empty() {
        format!(
            "SCR1B3 is registered for {} file-type group{}. Windows does not let \
             an app make itself the default — open Default Apps and pick SCR1B3 \
             for each type.",
            requested.len(),
            if requested.len() == 1 { "" } else { "s" }
        )
    } else if shared_failed {
        format!(
            "Registration FAILED — SCR1B3 could not be added to the Windows \
             Default Apps list ({n} registry {writes} failed).{first_err}"
        )
    } else {
        format!(
            "Registered {} of {} file-type groups. FAILED: {} ({n} registry \
             {writes} failed).{first_err}",
            registered.len(),
            requested.len(),
            failed_claims.join(", ")
        )
    };

    RegistrationOutcome {
        registered,
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A path WITH a space, so the open-command quoting is genuinely exercised.
    const EXE: &str = r"C:\Apps\SCR 1B3\scr1b3.exe";

    fn failure(claim: Option<&'static str>) -> FailedWrite {
        FailedWrite {
            claim,
            key: "Software\\Classes\\.txt\\OpenWithProgids".into(),
            err: "Access is denied.".into(),
        }
    }

    #[test]
    fn a_total_failure_is_never_reported_as_success() {
        // THE regression this pins: `register_under` returned a hardcoded
        // "SCR1B3 is registered." message regardless of `failed`, so a
        // registration in which every write failed was shown to the user as a
        // success. The message must reflect the failure, and nothing may be
        // claimed as registered.
        let requested = [ClaimType::PlainText, ClaimType::Markdown];
        let out = summarize(&requested, &[failure(None), failure(Some("plain_text"))]);
        assert!(
            out.registered.is_empty(),
            "nothing registered when the shared app keys failed"
        );
        let m = out.message.to_lowercase();
        assert!(
            m.contains("fail"),
            "message must state the failure, got: {}",
            out.message
        );
        assert!(
            !m.contains("scr1b3 is registered"),
            "message must NOT claim success, got: {}",
            out.message
        );
        assert!(
            out.message.contains("Access is denied."),
            "message must surface the underlying error, got: {}",
            out.message
        );
    }

    #[test]
    fn a_partial_failure_reports_what_landed_and_what_did_not() {
        let requested = [ClaimType::PlainText, ClaimType::Markdown, ClaimType::Json];
        let out = summarize(&requested, &[failure(Some("markdown"))]);
        assert_eq!(
            out.registered,
            vec!["plain_text".to_string(), "json".to_string()],
            "only the claims whose writes all succeeded are registered"
        );
        assert!(
            out.message.contains("Registered 2 of 3"),
            "honest count, got: {}",
            out.message
        );
        assert!(
            out.message.contains("markdown"),
            "names the failed group, got: {}",
            out.message
        );
    }

    /// The failure count and its noun must agree.
    ///
    /// `if n == 1 { "write" } else { "writes" }` is the only thing keeping the
    /// message from reading "1 registry writes failed" / "2 registry write
    /// failed". Every existing message assertion matches a prefix that stops
    /// before the noun, so flipping that `==` to `!=` inverts the grammar of
    /// EVERY failure message a user ever sees with the whole suite green. Both
    /// sides of the equality are pinned here — one alone leaves the flip alive
    /// in the other direction.
    #[test]
    fn the_failure_count_and_its_noun_agree() {
        let requested = [ClaimType::PlainText, ClaimType::Markdown, ClaimType::Json];

        let one = summarize(&requested, &[failure(Some("markdown"))]);
        assert!(
            one.message.contains("(1 registry write failed)"),
            "a single failed write is singular, got: {}",
            one.message
        );

        let two = summarize(
            &requested,
            &[failure(Some("markdown")), failure(Some("json"))],
        );
        assert!(
            two.message.contains("(2 registry writes failed)"),
            "two failed writes are plural, got: {}",
            two.message
        );

        // The shared-key branch builds its own sentence from the same pair, so
        // it needs the same guarantee.
        let shared = summarize(&requested, &[failure(None)]);
        assert!(
            shared.message.contains("(1 registry write failed)"),
            "the total-failure message is singular for one write, got: {}",
            shared.message
        );
    }

    #[test]
    fn a_clean_run_reports_success_and_the_windows_limitation() {
        let requested = [ClaimType::PlainText];
        let out = summarize(&requested, &[]);
        assert_eq!(out.registered, vec!["plain_text".to_string()]);
        assert!(out.message.contains("registered"));
        assert!(
            out.message
                .contains("does not let an app make itself the default"),
            "the success message stays honest about the UserChoice limit, got: {}",
            out.message
        );
        assert!(
            !out.message.to_lowercase().contains("failed"),
            "a clean run must not mention failure, got: {}",
            out.message
        );
    }

    fn entries(types: &[ClaimType]) -> Vec<RegEntry> {
        registry_entries(
            types,
            EXE,
            "Software\\Classes",
            "Software\\SCR1B3",
            "SCR1B3",
        )
    }

    #[test]
    fn progid_open_command_quotes_exe_and_file_arg() {
        let es = entries(&[ClaimType::PlainText]);
        let cmd = es
            .iter()
            .find(|e| e.key.ends_with("SCR1B3.txt\\shell\\open\\command"))
            .expect("open command entry");
        assert_eq!(cmd.name, "", "open command is the key's default value");
        assert_eq!(
            cmd.data,
            format!("\"{EXE}\" \"%1\""),
            "exe and %1 both quoted"
        );
    }

    #[test]
    fn plain_text_registration_covers_dot_txt() {
        // The user-reported symptom: ".txt is not available as a default app".
        // A registration of the PlainText group must emit BOTH halves Windows
        // needs for `.txt` to appear in Settings ▸ Default Apps — the additive
        // `OpenWithProgids` entry AND the `Capabilities\FileAssociations`
        // declaration — plus a ProgID open-command pointing at our exe.
        let es = entries(&[ClaimType::PlainText]);
        assert!(
            es.iter()
                .any(|e| e.key == "Software\\Classes\\.txt\\OpenWithProgids"
                    && e.name == "SCR1B3.txt"),
            "no OpenWithProgids entry for .txt"
        );
        assert!(
            es.iter().any(
                |e| e.key == "Software\\SCR1B3\\Capabilities\\FileAssociations"
                    && e.name == ".txt"
                    && e.data == "SCR1B3.txt"
            ),
            "no Capabilities\\FileAssociations entry for .txt — Default Apps \
             would not list SCR1B3 for .txt"
        );
        assert!(
            es.iter().any(
                |e| e.key == "Software\\Classes\\SCR1B3.txt\\shell\\open\\command"
                    && e.data.contains(EXE)
            ),
            "no open command for the .txt ProgID"
        );
    }

    #[test]
    fn every_entry_is_attributed_to_its_claim_or_to_the_shared_app_set() {
        // Failure reporting depends on this attribution: an unattributed
        // per-claim write could not be blamed on the claim it broke.
        let es = entries(&[ClaimType::PlainText, ClaimType::Markdown]);
        // The EXHAUSTIVE set of app-level keys allowed to be unattributed. Listed
        // literally rather than by prefix so a newly-added shared write cannot
        // slip in unnoticed — the point of the assertion is that every write is
        // accounted for, and a `starts_with` whitelist would account for writes
        // nobody reviewed.
        let shared_keys = [
            "Software\\SCR1B3\\Capabilities".to_string(),
            "Software\\RegisteredApplications".to_string(),
            "Software\\Classes\\Applications\\scr1b3.exe".to_string(),
            "Software\\Classes\\Applications\\scr1b3.exe\\shell\\open\\command".to_string(),
            "Software\\Classes\\*\\shell\\SCR1B3".to_string(),
            "Software\\Classes\\*\\shell\\SCR1B3\\command".to_string(),
        ];
        for e in &es {
            match e.claim {
                None => assert!(
                    shared_keys.contains(&e.key),
                    "unattributed write that is not a shared app entry: {}",
                    e.key
                ),
                Some(k) => assert!(
                    k == "plain_text" || k == "markdown",
                    "entry attributed to a claim that was not requested: {k}"
                ),
            }
        }
        assert!(
            es.iter().any(|e| e.claim == Some("plain_text")),
            "the plain_text claim produced no attributed entry"
        );
    }

    #[test]
    fn each_extension_gets_an_additive_openwithprogids_entry() {
        let es = entries(&[ClaimType::Markdown]);
        for ext in ClaimType::Markdown.windows_extensions() {
            let e = es
                .iter()
                .find(|e| e.key == format!("Software\\Classes\\.{ext}\\OpenWithProgids"))
                .unwrap_or_else(|| panic!("OpenWithProgids entry for .{ext}"));
            assert_eq!(e.name, "SCR1B3.md", "value NAME is the ProgID");
            assert_eq!(
                e.data, "",
                "OpenWithProgids data is empty (additive, never steals)"
            );
        }
    }

    #[test]
    fn registered_applications_points_at_capabilities() {
        let es = entries(&[ClaimType::Json]);
        let ra = es
            .iter()
            .find(|e| e.key == "Software\\RegisteredApplications" && e.name == "SCR1B3")
            .expect("RegisteredApplications entry");
        assert_eq!(ra.data, "Software\\SCR1B3\\Capabilities");
    }

    #[test]
    fn capabilities_file_associations_cover_every_extension() {
        let es = entries(&[ClaimType::PlainText]);
        for ext in ClaimType::PlainText.windows_extensions() {
            assert!(
                es.iter().any(
                    |e| e.key == "Software\\SCR1B3\\Capabilities\\FileAssociations"
                        && e.name == format!(".{ext}")
                        && e.data == "SCR1B3.txt"
                ),
                "FileAssociations entry for .{ext}"
            );
        }
    }

    #[test]
    fn a_test_class_root_keeps_all_writes_inside_the_throwaway_subtree() {
        // The Windows #[ignore] integration test relies on this: with a test
        // root, NO entry touches a real `Software\Classes\.<ext>` key.
        //
        // This guard used to read `|| e.key == "Software\\RegisteredApplications"`
        // — it whitelisted the ONE write that escaped, so a test named "keeps ALL
        // writes inside the throwaway subtree" proved nothing about the only
        // entry that left it. That is exactly why the escape went unnoticed: the
        // key is a fixed Windows location, so containment there is the VALUE
        // NAME, and the value name was hard-coded to the real one.
        let es = registry_entries(
            &ClaimType::ALL,
            EXE,
            "Software\\SCR1B3-Test\\Classes",
            "Software\\SCR1B3-Test\\App",
            TEST_APP_NAME,
        );
        for e in &es {
            let contained = e.key.starts_with("Software\\SCR1B3-Test\\")
                || (e.key == "Software\\RegisteredApplications" && e.name == TEST_APP_NAME);
            assert!(
                contained,
                "entry escaped the test sandbox: key={} name={}",
                e.key, e.name
            );
        }
    }

    #[test]
    fn a_test_root_never_writes_the_real_registered_application_value() {
        // The specific damage: `HKCU\Software\RegisteredApplications\SCR1B3` is
        // the user's real Default-Apps registration. A test that writes it
        // repoints their install at a throwaway path, and the test's own cleanup
        // then deletes the value outright.
        let es = registry_entries(
            &ClaimType::ALL,
            EXE,
            "Software\\SCR1B3-Test\\Classes",
            "Software\\SCR1B3-Test\\App",
            TEST_APP_NAME,
        );
        assert!(
            !es.iter()
                .any(|e| e.key == "Software\\RegisteredApplications" && e.name == "SCR1B3"),
            "a throwaway-root build must never name the REAL registration value"
        );
        // …and it must still register itself under its own name, or the
        // integration test is asserting against a set it never wrote.
        assert!(
            es.iter()
                .any(|e| e.key == "Software\\RegisteredApplications"
                    && e.name == TEST_APP_NAME
                    && e.data == "Software\\SCR1B3-Test\\App\\Capabilities"),
            "the test-scoped registration entry must still be emitted"
        );
    }

    #[test]
    fn empty_types_still_registers_the_app_capabilities() {
        let es = entries(&[]);
        assert!(es
            .iter()
            .any(|e| e.key.ends_with("Capabilities") && e.name == "ApplicationName"));
        // ...but declares no per-extension ProgID.
        assert!(!es.iter().any(|e| e.key.contains("\\shell\\open\\command")));
        // ...nor any of the file-OPENING surfaces. An app that claims nothing
        // must not advertise itself as an opener: no `Applications\<exe>` entry
        // and no "Edit with SCR1B3" verb on every file in the system.
        assert!(
            !es.iter().any(|e| e.key.contains("\\Applications\\")),
            "an empty claim set must not write an Applications\\<exe> surface"
        );
        assert!(
            !es.iter().any(|e| e.key.contains("\\*\\shell\\")),
            "an empty claim set must not add an Edit-with verb to every file"
        );
    }

    #[test]
    fn exe_file_name_takes_the_leaf_of_every_path_dialect() {
        assert_eq!(exe_file_name(r"C:\Apps\SCR 1B3\scr1b3.exe"), "scr1b3.exe");
        assert_eq!(exe_file_name("/usr/local/bin/scr1b3"), "scr1b3");
        // The WiX dialect: the directory half is a property token closed by `]`.
        assert_eq!(exe_file_name("[APPLICATIONFOLDER]scr1b3.exe"), "scr1b3.exe");
        // A bare name is already the leaf.
        assert_eq!(exe_file_name("scr1b3.exe"), "scr1b3.exe");
    }

    #[test]
    fn the_applications_key_advertises_the_app_and_its_supported_types() {
        // Gap: `Applications\scr1b3.exe`, `SupportedTypes` and `FriendlyAppName`
        // did not exist at all, so SCR1B3 never appeared in Explorer's
        // "Open with ▸ Choose another app" list. Assert the EMITTED ENTRIES, not
        // a message: a prose assertion here would pass with nothing written.
        let es = entries(&[ClaimType::PlainText]);
        let apps = "Software\\Classes\\Applications\\scr1b3.exe";
        assert!(
            es.iter()
                .any(|e| e.key == apps && e.name == "FriendlyAppName" && e.data == "SCR1B3"),
            "no FriendlyAppName under {apps}"
        );
        let cmd = es
            .iter()
            .find(|e| e.key == format!("{apps}\\shell\\open\\command"))
            .expect("Applications open command");
        assert_eq!(cmd.name, "", "open command is the key's default value");
        assert_eq!(cmd.data, format!("\"{EXE}\" \"%1\""), "exe and %1 quoted");
        for ext in ClaimType::PlainText.windows_extensions() {
            assert!(
                es.iter()
                    .any(|e| e.key == format!("{apps}\\SupportedTypes")
                        && e.name == format!(".{ext}")),
                "SupportedTypes is missing .{ext} — Open-with would not offer SCR1B3"
            );
        }
    }

    #[test]
    fn every_claimed_extension_is_marked_as_perceived_text() {
        let es = entries(&ClaimType::ALL);
        for c in ClaimType::ALL {
            for ext in c.windows_extensions() {
                assert!(
                    es.iter()
                        .any(|e| e.key == format!("Software\\Classes\\.{ext}")
                            && e.name == "PerceivedType"
                            && e.data == "text"),
                    "no PerceivedType=text on .{ext}"
                );
            }
        }
    }

    #[test]
    fn the_star_verb_offers_edit_with_scr1b3_on_any_file() {
        let es = entries(&[ClaimType::Markdown]);
        let verb = "Software\\Classes\\*\\shell\\SCR1B3";
        let label = es
            .iter()
            .find(|e| e.key == verb && e.name.is_empty())
            .expect("the * verb's label");
        assert_eq!(
            label.data, "Edit with SCR1B3",
            "the context-menu label is the user-visible text"
        );
        assert!(
            es.iter()
                .any(|e| e.key == verb && e.name == "Icon" && e.data == format!("{EXE},0")),
            "the verb has no icon"
        );
        let cmd = es
            .iter()
            .find(|e| e.key == format!("{verb}\\command"))
            .expect("the * verb's command");
        assert_eq!(cmd.data, format!("\"{EXE}\" \"%1\""));
    }

    #[test]
    fn the_new_surfaces_stay_inside_a_throwaway_class_root() {
        // The `Applications\<exe>` key and the `*` verb are both under
        // `class_root`, so the #[ignore]d HKCU integration test still cannot
        // touch a real installation. Assert it directly — the generic sandbox
        // test would also pass if these surfaces were simply never emitted.
        let es = registry_entries(
            &ClaimType::ALL,
            EXE,
            "Software\\SCR1B3-Test\\Classes",
            "Software\\SCR1B3-Test\\App",
            TEST_APP_NAME,
        );
        assert!(
            es.iter()
                .any(|e| e.key == "Software\\SCR1B3-Test\\Classes\\Applications\\scr1b3.exe"),
            "the Applications surface is not emitted under a test root"
        );
        assert!(
            es.iter()
                .any(|e| e.key == "Software\\SCR1B3-Test\\Classes\\*\\shell\\SCR1B3"),
            "the * verb is not emitted under a test root"
        );
        for e in &es {
            assert!(
                e.key.starts_with("Software\\SCR1B3-Test\\")
                    || (e.key == "Software\\RegisteredApplications" && e.name == TEST_APP_NAME),
                "entry escaped the test sandbox: key={} name={}",
                e.key,
                e.name
            );
        }
    }
}

#[cfg(test)]
mod unregister_tests {
    use super::*;

    const EXE: &str = r"C:\Apps\SCR 1B3\scr1b3.exe";
    const CLASSES: &str = "Software\\Classes";
    const APP: &str = "Software\\SCR1B3";

    fn deletes(desired: &[ClaimType]) -> Vec<RegDelete> {
        unregister_entries(desired, EXE, CLASSES, APP)
    }

    fn has_value(ds: &[RegDelete], key: &str, name: &str) -> bool {
        ds.iter()
            .any(|d| d.key == key && d.name.as_deref() == Some(name))
    }

    #[test]
    fn an_unclaimed_group_has_every_trace_of_it_removed() {
        // The gap: unticking a group in Settings removed NOTHING, so a user who
        // opted out of source code kept the association forever. Assert the
        // emitted REMOVALS — all five surfaces, not just the ProgID key.
        let ds = deletes(&[ClaimType::PlainText]);
        assert!(
            ds.contains(&RegDelete {
                key: "Software\\Classes\\SCR1B3.source".into(),
                name: None,
            }),
            "the un-claimed group's ProgID subtree is not removed"
        );
        for ext in ClaimType::SourceCode.windows_extensions() {
            assert!(
                has_value(
                    &ds,
                    &format!("Software\\Classes\\.{ext}\\OpenWithProgids"),
                    "SCR1B3.source"
                ),
                "OpenWithProgids for .{ext} keeps the stale ProgID"
            );
            assert!(
                has_value(
                    &ds,
                    "Software\\SCR1B3\\Capabilities\\FileAssociations",
                    &format!(".{ext}")
                ),
                "FileAssociations keeps .{ext}"
            );
            assert!(
                has_value(
                    &ds,
                    "Software\\Classes\\Applications\\scr1b3.exe\\SupportedTypes",
                    &format!(".{ext}")
                ),
                "SupportedTypes keeps .{ext}"
            );
            assert!(
                has_value(&ds, &format!("Software\\Classes\\.{ext}"), "PerceivedType"),
                "the PerceivedType overlay on .{ext} is not restored"
            );
        }
    }

    #[test]
    fn a_still_claimed_group_is_never_removed() {
        let ds = deletes(&[ClaimType::PlainText]);
        assert!(
            !ds.iter()
                .any(|d| d.key == "Software\\Classes\\SCR1B3.txt" && d.name.is_none()),
            "a CLAIMED group's ProgID must never be deleted"
        );
        // …and its own ProgID is never stripped from its own extensions.
        for ext in ClaimType::PlainText.windows_extensions() {
            assert!(
                !has_value(
                    &ds,
                    &format!("Software\\Classes\\.{ext}\\OpenWithProgids"),
                    "SCR1B3.txt"
                ),
                "the owning ProgID was scheduled for removal from its own .{ext}"
            );
            assert!(
                !has_value(&ds, &format!("Software\\Classes\\.{ext}"), "PerceivedType"),
                "a claimed extension's PerceivedType must survive"
            );
        }
    }

    #[test]
    fn a_migrated_extension_loses_its_previous_owners_progid() {
        // THE upgrade bug: seven extensions moved SourceCode -> ConfigData. A
        // user registered before the move still has `SCR1B3.source` in their
        // `OpenWithProgids`, and because BOTH groups are still claimed the
        // un-claimed-group sweep never touches it. Pin the exact migrated set.
        let ds = deletes(&ClaimType::ALL);
        for ext in ["toml", "yaml", "yml", "ini", "csv", "xml", "conf"] {
            assert!(
                ClaimType::ConfigData.windows_extensions().contains(&ext),
                ".{ext} is expected to be owned by ConfigData now"
            );
            assert!(
                has_value(
                    &ds,
                    &format!("Software\\Classes\\.{ext}\\OpenWithProgids"),
                    "SCR1B3.source"
                ),
                "the pre-migration SCR1B3.source ProgID survives on .{ext}"
            );
            // …while the CURRENT owner is left in place.
            assert!(
                !has_value(
                    &ds,
                    &format!("Software\\Classes\\.{ext}\\OpenWithProgids"),
                    "SCR1B3.config"
                ),
                "the current owner's ProgID was scheduled for removal from .{ext}"
            );
        }
    }

    #[test]
    fn every_removal_names_something_scr1b3_itself_writes() {
        // The safety invariant: an unregister must never remove another
        // application's registry state. Every emitted removal is either one of
        // our own keys, one of our own `SCR1B3.*` ProgID values, an extension
        // entry under our app root, or the reversible per-user PerceivedType
        // overlay.
        let ds = deletes(&[ClaimType::PlainText]);
        for d in &ds {
            let ours = d.key.starts_with("Software\\SCR1B3")
                || d.key.starts_with("Software\\Classes\\SCR1B3.")
                || d.key
                    .starts_with("Software\\Classes\\Applications\\scr1b3.exe");
            let our_value = match d.name.as_deref() {
                Some(n) => n.starts_with("SCR1B3.") || n == "PerceivedType" || n.starts_with('.'),
                None => false,
            };
            assert!(
                ours || our_value,
                "removal targets state SCR1B3 does not own: key={} name={:?}",
                d.key,
                d.name
            );
        }
    }

    #[test]
    fn claiming_everything_still_cleans_the_migrated_extensions_only() {
        // With every group claimed there is no un-claimed sweep at all, so the
        // ONLY removals are the migration strips — and there must still be some,
        // or the upgrade path silently does nothing.
        let ds = deletes(&ClaimType::ALL);
        assert!(
            !ds.iter().any(|d| d.name.is_none()),
            "no ProgID subtree may be deleted when every group is claimed"
        );
        assert!(!ds.is_empty(), "the migration sweep produced no removals");
        assert!(ds.iter().all(|d| d.key.ends_with("\\OpenWithProgids")
            && d.name.as_deref().is_some_and(|n| n.starts_with("SCR1B3."))));
    }

    #[test]
    fn a_test_root_keeps_every_removal_inside_the_throwaway_subtree() {
        let ds = unregister_entries(
            &[ClaimType::PlainText],
            EXE,
            "Software\\SCR1B3-Test\\Classes",
            "Software\\SCR1B3-Test\\App",
        );
        assert!(!ds.is_empty(), "no removals to contain");
        for d in &ds {
            assert!(
                d.key.starts_with("Software\\SCR1B3-Test\\"),
                "removal escaped the test sandbox: {}",
                d.key
            );
        }
    }

    /// Unticking EVERY group must also take away the app-level surfaces, which
    /// are the mirror image of `registry_entries`' `!types.is_empty()` gate.
    ///
    /// Missed on the first pass: the per-group sweep removed all five ProgIDs but
    /// left `Applications\scr1b3.exe` and the `*` verb behind, so a user who
    /// opted out completely still had "Edit with SCR1B3" on the context menu of
    /// every file on the machine — the most visible surface of all.
    #[test]
    fn opting_out_of_everything_also_removes_the_app_level_surfaces() {
        let ds = deletes(&[]);
        assert!(
            ds.contains(&RegDelete {
                key: "Software\\Classes\\Applications\\scr1b3.exe".into(),
                name: None,
            }),
            "the Applications\\<exe> key survives a full opt-out"
        );
        assert!(
            ds.contains(&RegDelete {
                key: "Software\\Classes\\*\\shell\\SCR1B3".into(),
                name: None,
            }),
            "the Edit-with verb survives a full opt-out"
        );
        // …and they must NOT be removed while anything is still claimed, or a
        // partial opt-out would strip the surface the remaining groups need.
        let partial = deletes(&[ClaimType::PlainText]);
        assert!(
            !partial.iter().any(|d| d.key
                == "Software\\Classes\\Applications\\scr1b3.exe"
                && d.name.is_none()),
            "the Applications key must survive while a group is still claimed"
        );
        assert!(
            !partial
                .iter()
                .any(|d| d.key == "Software\\Classes\\*\\shell\\SCR1B3" && d.name.is_none()),
            "the Edit-with verb must survive while a group is still claimed"
        );
    }

    #[test]
    fn a_missing_key_is_a_benign_delete_but_an_unexplained_failure_is_not() {
        assert!(delete_error_is_benign(
            "ERROR: The system was unable to find the specified registry key or value."
        ));
        assert!(delete_error_is_benign(
            "ERROR: The system cannot find the file specified."
        ));
        // An unexplained non-zero exit must NOT be swallowed — that is the
        // silent-pass this module was already fixed for once.
        assert!(!delete_error_is_benign(""));
        assert!(!delete_error_is_benign("ERROR: Access is denied."));
    }

    #[test]
    fn the_cleanup_note_is_appended_only_when_something_failed_and_agrees_in_number() {
        let base = "SCR1B3 is registered.".to_string();
        assert_eq!(
            append_cleanup_note(base.clone(), 0),
            base,
            "a clean cleanup must not add a note"
        );
        assert!(
            append_cleanup_note(base.clone(), 1).contains("(1 stale registry entry could not"),
            "singular for one"
        );
        assert!(
            append_cleanup_note(base.clone(), 3).contains("(3 stale registry entries could not"),
            "plural for many"
        );
        // The original message survives intact — the note is additive.
        assert!(append_cleanup_note(base.clone(), 2).starts_with(&base));
    }

    #[test]
    fn the_digest_tracks_every_change_to_the_work_a_registration_would_do() {
        let mk = |types: &[ClaimType], exe: &str| {
            let es = registry_entries(types, exe, CLASSES, APP, "SCR1B3");
            let ds = unregister_entries(types, exe, CLASSES, APP);
            entries_digest(&es, &ds)
        };
        let base = mk(&[ClaimType::PlainText], EXE);
        assert_eq!(base, mk(&[ClaimType::PlainText], EXE), "deterministic");
        assert_ne!(
            base,
            mk(&[ClaimType::PlainText], r"D:\Moved\scr1b3.exe"),
            "a MOVED exe must change the digest — every open command is stale"
        );
        assert_ne!(
            base,
            mk(&[ClaimType::PlainText, ClaimType::Json], EXE),
            "a changed claim set must change the digest"
        );
        // The property the schema-version alternative would NOT have: adding a
        // registry surface changes the digest, so an upgrading user whose exe
        // and claims are unchanged is still re-registered.
        let mut es = registry_entries(&[ClaimType::PlainText], EXE, CLASSES, APP, "SCR1B3");
        let ds = unregister_entries(&[ClaimType::PlainText], EXE, CLASSES, APP);
        let before = entries_digest(&es, &ds);
        es.push(RegEntry {
            key: "Software\\Classes\\SCR1B3.txt\\NewSurface".into(),
            name: String::new(),
            data: "x".into(),
            claim: Some("plain_text"),
        });
        assert_ne!(
            before,
            entries_digest(&es, &ds),
            "a NEW registry surface must invalidate the stamp"
        );
        // …and so must a change to the removals alone.
        let mut ds2 = ds.clone();
        ds2.push(RegDelete {
            key: "Software\\Classes\\SCR1B3.gone".into(),
            name: None,
        });
        assert_ne!(before, entries_digest(&es[..es.len() - 1], &ds2));
    }

    #[test]
    fn the_digest_separates_its_fields_so_a_shift_between_them_is_visible() {
        // A naive concatenation digest collides when content moves across a
        // field boundary ("ab"+"c" == "a"+"bc"). The record/field separators
        // are what prevent that, and nothing else asserts they are present.
        let a = [RegEntry {
            key: "K".into(),
            name: "AB".into(),
            data: String::new(),
            claim: None,
        }];
        let b = [RegEntry {
            key: "K".into(),
            name: "A".into(),
            data: "B".into(),
            claim: None,
        }];
        assert_ne!(entries_digest(&a, &[]), entries_digest(&b, &[]));
    }
}
