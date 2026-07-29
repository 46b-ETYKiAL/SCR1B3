//! Pure generation of the per-user (HKCU) registry entries that register SCR1B3
//! as a file handler. The exe path + roots are injected, so the full key/value
//! set is unit-testable on ANY OS (the executor in `windows.rs` is the only
//! Windows-only part). Compiled under `#[cfg(any(test, windows))]`.

use scribe_core::config::ClaimType;

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
        }
    }
    out
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
        for e in &es {
            match e.claim {
                None => assert!(
                    e.key.ends_with("Capabilities") || e.key == "Software\\RegisteredApplications",
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
    }
}
