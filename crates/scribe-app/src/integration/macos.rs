//! macOS file-handler registration.
//!
//! There is no per-user write to perform here: a macOS app declares the document
//! types it handles in its bundle's `Info.plist` (`CFBundleDocumentTypes` /
//! `LSItemContentTypes`), which Launch Services reads when the bundle is
//! installed. Setting the DEFAULT is then a manual Finder step (Get Info ▸ Open
//! With ▸ Change All) — we do not make an `unsafe` Launch-Services call from this
//! `forbid(unsafe_code)` crate.
//!
//! So the honest job of `register` on macOS is to VERIFY, not to assert: read the
//! running bundle's `Info.plist` and report only the claim groups whose UTIs it
//! actually declares. This module exists because the previous implementation
//! reported EVERY requested group as `registered`, `failed: []`, and "SCR1B3 is
//! registered for these file types." while doing nothing at all — the same
//! fake-success bug that was already hunted down on the Windows twin (see
//! `windows_entries::summarize` and its
//! `a_total_failure_is_never_reported_as_success` test). A user running an
//! unbundled binary, or a bundle whose plist lost a UTI, was told the
//! registration succeeded and then found SCR1B3 missing from "Open With" with no
//! explanation.
//!
//! Like `linux`, the decision half is compiled on every OS (`cfg(any(test, …))`)
//! so it is unit-testable on any host; only the bundle lookup is macOS-gated.

use super::RegisterReport;
use scribe_core::config::ClaimType;

/// PURE: derive the honest report from the requested groups and the running
/// bundle's `Info.plist` body (`None` = not running from an `.app` bundle).
///
/// A group counts as registered only when EVERY UTI it claims appears as an
/// `LSItemContentTypes` entry in the plist — that is the whole of what makes
/// macOS treat SCR1B3 as a handler for it. Anything else is reported as a
/// failure with the reason, never as success.
pub(crate) fn summarize_bundle(requested: &[ClaimType], plist: Option<&str>) -> RegisterReport {
    if requested.is_empty() {
        return RegisterReport {
            message: "No file types selected to register.".into(),
            ..Default::default()
        };
    }

    let Some(body) = plist else {
        // No bundle => Launch Services has no record of ANY of these types.
        // Nothing was registered and nothing can be; say so.
        return RegisterReport {
            registered: Vec::new(),
            failed: requested
                .iter()
                .map(|t| {
                    (
                        t.key().to_string(),
                        "SCR1B3 is not running from an .app bundle, so macOS has no \
                         record of the file types it handles"
                            .to_string(),
                    )
                })
                .collect(),
            needs_user_action: false,
            message: "Registration FAILED — SCR1B3 is not running from an .app bundle, so \
                      macOS has no record of the file types it handles. Move SCR1B3.app \
                      into /Applications and run it from there."
                .into(),
        };
    };

    let mut registered: Vec<String> = Vec::new();
    let mut failed: Vec<(String, String)> = Vec::new();
    for t in requested {
        let missing: Vec<&str> = t
            .macos_utis()
            .iter()
            .copied()
            .filter(|uti| !declares_uti(body, uti))
            .collect();
        if missing.is_empty() {
            registered.push(t.key().to_string());
        } else {
            failed.push((
                t.key().to_string(),
                format!(
                    "the app bundle's Info.plist does not declare {}",
                    missing.join(", ")
                ),
            ));
        }
    }

    let first_err = failed
        .first()
        .map(|(k, e)| format!(" First problem: {e} ({k})."))
        .unwrap_or_default();
    let n = failed.len();
    let groups = if n == 1 { "group" } else { "groups" };

    let message = if failed.is_empty() {
        format!(
            "SCR1B3's app bundle declares {} file-type group{}. macOS does not let an app \
             make itself the default — select a file in Finder, press \u{2318}I, expand \
             \"Open with\", choose SCR1B3, and click \"Change All\u{2026}\".",
            registered.len(),
            if registered.len() == 1 { "" } else { "s" }
        )
    } else if registered.is_empty() {
        format!(
            "Registration FAILED — SCR1B3's app bundle declares none of the selected file \
             types ({n} {groups} undeclared).{first_err}"
        )
    } else {
        format!(
            "Declared {} of {} file-type groups. FAILED: {} ({n} {groups} undeclared).\
             {first_err} For the ones that worked: select a file in Finder, press \u{2318}I, \
             expand \"Open with\", choose SCR1B3, and click \"Change All\u{2026}\".",
            registered.len(),
            requested.len(),
            failed
                .iter()
                .map(|(k, _)| k.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        )
    };

    RegisterReport {
        // Only send the user to Finder when at least one group really is
        // declared — telling them to pick SCR1B3 for a type the bundle never
        // claimed would send them looking for an entry that isn't there.
        needs_user_action: !registered.is_empty(),
        registered,
        failed,
        message,
    }
}

/// Is `uti` declared as an `LSItemContentTypes` entry in this plist body?
///
/// Matched as the full `<string>…</string>` element rather than a bare substring:
/// a loose `contains` would also match the UTI inside a comment or a `CFBundle…`
/// value and report a declaration the bundle does not actually make.
fn declares_uti(plist: &str, uti: &str) -> bool {
    plist.contains(&format!("<string>{uti}</string>"))
}

/// Read the running bundle's `Info.plist`, if there is one.
///
/// The executable of a bundled app lives at `…/SCR1B3.app/Contents/MacOS/scr1b3`,
/// so the plist is two levels up. A bare (unbundled) binary — `cargo run`, a
/// loose copy — has no plist, which is exactly the case the caller must report as
/// a failure rather than a success.
#[cfg(target_os = "macos")]
fn bundle_info_plist() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let macos_dir = exe.parent()?;
    if macos_dir.file_name()? != "MacOS" {
        return None;
    }
    let plist = macos_dir.parent()?.join("Info.plist");
    plist.is_file().then_some(plist)
}

/// Verify the running bundle declares `types` and report honestly.
#[cfg(target_os = "macos")]
pub fn register(types: &[ClaimType]) -> RegisterReport {
    let body = bundle_info_plist().and_then(|p| std::fs::read_to_string(p).ok());
    summarize_bundle(types, body.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The plist SCR1B3 actually ships — the same file the packaging-consistency
    /// test in `mod.rs` binds to `ClaimType`.
    const SHIPPED: &str = include_str!("../../../../packaging/macos/Info.plist");

    /// A bundle-less plist: valid, but declaring no document types at all.
    const EMPTY_PLIST: &str = "<?xml version=\"1.0\"?>\n<plist><dict>\
                               <key>CFBundleName</key><string>SCR1B3</string>\
                               </dict></plist>";

    /// THE regression this pins. `macos_register` returned every requested type
    /// as `registered`, `failed: []`, and "SCR1B3 is registered for these file
    /// types." while performing ZERO work — byte-identical to the Windows
    /// fake-success bug pinned by
    /// `windows_entries::tests::a_total_failure_is_never_reported_as_success`.
    /// Nothing may be claimed as registered when the bundle declares nothing,
    /// and the message must say so.
    #[test]
    fn a_bundle_that_declares_nothing_is_never_reported_as_success() {
        let out = summarize_bundle(&ClaimType::ALL, Some(EMPTY_PLIST));
        assert!(
            out.registered.is_empty(),
            "nothing is registered when the bundle declares no document types, got: {:?}",
            out.registered
        );
        assert_eq!(
            out.failed.len(),
            ClaimType::ALL.len(),
            "every requested group must be reported as failed, got: {:?}",
            out.failed
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
            !out.needs_user_action,
            "do not send the user to Finder to pick an entry the bundle never declared"
        );
    }

    /// Running as a loose binary (no `.app` bundle) registers nothing. This was
    /// the same silent lie: the old code reported success for a build that
    /// Launch Services had never seen.
    #[test]
    fn an_unbundled_binary_is_a_failure_not_a_success() {
        let out = summarize_bundle(&[ClaimType::PlainText, ClaimType::Markdown], None);
        assert!(
            out.registered.is_empty(),
            "nothing registered without a bundle"
        );
        assert_eq!(
            out.failed.len(),
            2,
            "both requested groups are reported failed, got: {:?}",
            out.failed
        );
        assert!(
            out.failed.iter().all(|(_, e)| e.contains("app bundle")),
            "each failure must name the reason, got: {:?}",
            out.failed
        );
        let m = out.message.to_lowercase();
        assert!(
            m.contains("fail"),
            "message must state the failure: {}",
            out.message
        );
        assert!(
            !m.contains("scr1b3 is registered"),
            "message must NOT claim success, got: {}",
            out.message
        );
        assert!(!out.needs_user_action);
    }

    /// A partial declaration reports exactly what landed and what did not —
    /// never all-or-nothing.
    #[test]
    fn a_partial_declaration_reports_what_landed_and_what_did_not() {
        // Declares plain-text only; Markdown's UTI is absent.
        let partial = "<plist><dict><key>LSItemContentTypes</key><array>\
                       <string>public.plain-text</string></array></dict></plist>";
        let out = summarize_bundle(&[ClaimType::PlainText, ClaimType::Markdown], Some(partial));
        assert_eq!(out.registered, vec!["plain_text".to_string()]);
        assert_eq!(out.failed.len(), 1, "got: {:?}", out.failed);
        assert_eq!(out.failed[0].0, "markdown");
        assert!(
            out.failed[0].1.contains("net.daringfireball.markdown"),
            "the failure names the missing UTI, got: {}",
            out.failed[0].1
        );
        assert!(
            out.message.contains("Declared 1 of 2"),
            "honest count, got: {}",
            out.message
        );
        assert!(
            out.message.contains("markdown"),
            "names the failed group, got: {}",
            out.message
        );
        assert!(
            out.needs_user_action,
            "one group DID land, so the Finder follow-up is real"
        );
    }

    /// A group whose UTIs are only PARTIALLY declared is not registered — a
    /// half-declared group does not open every file in it.
    #[test]
    fn a_group_missing_one_of_its_utis_is_not_registered() {
        // ConfigData claims public.xml AND public.comma-separated-values-text.
        let half = "<plist><dict><key>LSItemContentTypes</key><array>\
                    <string>public.xml</string></array></dict></plist>";
        let out = summarize_bundle(&[ClaimType::ConfigData], Some(half));
        assert!(
            out.registered.is_empty(),
            "a partially-declared group is not registered, got: {:?}",
            out.registered
        );
        assert!(
            out.failed[0]
                .1
                .contains("public.comma-separated-values-text"),
            "the failure names the one missing UTI, got: {}",
            out.failed[0].1
        );
    }

    /// The plist SCR1B3 actually ships declares every claimed group, so a real
    /// bundled run reports a clean success — and stays honest about the fact
    /// that macOS still needs the Finder step.
    #[test]
    fn the_shipped_bundle_declares_every_claimed_group() {
        let out = summarize_bundle(&ClaimType::ALL, Some(SHIPPED));
        assert!(
            out.failed.is_empty(),
            "shipped plist covers all: {:?}",
            out.failed
        );
        assert_eq!(out.registered.len(), ClaimType::ALL.len());
        assert!(
            out.needs_user_action,
            "macOS still needs Get Info ▸ Open With ▸ Change All"
        );
        assert!(
            out.message
                .contains("does not let an app make itself the default"),
            "the success message stays honest about the manual step, got: {}",
            out.message
        );
        assert!(
            !out.message.to_lowercase().contains("fail"),
            "a clean run must not mention failure, got: {}",
            out.message
        );
    }

    /// A loose `contains` would count a UTI mentioned anywhere in the plist —
    /// including a comment — as a declaration. Only a real
    /// `<string>…</string>` element counts.
    #[test]
    fn a_uti_only_mentioned_in_a_comment_is_not_a_declaration() {
        let commented =
            "<plist><dict><!-- public.plain-text is not declared here --></dict></plist>";
        let out = summarize_bundle(&[ClaimType::PlainText], Some(commented));
        assert!(
            out.registered.is_empty(),
            "a commented-out UTI is not a declaration, got: {:?}",
            out.registered
        );
    }

    /// Requesting nothing is a no-op, not a success claim.
    ///
    /// The message is asserted EXACTLY, not merely for the absence of the old
    /// fake-success sentence: without the empty-request branch the counting
    /// message reads "SCR1B3's app bundle declares 0 file-type groups. macOS does
    /// not let an app make itself the default — select a file in Finder…", which
    /// satisfies every weaker assertion while telling a user who selected nothing
    /// to go and configure Finder.
    #[test]
    fn no_requested_types_claims_nothing() {
        let out = summarize_bundle(&[], Some(SHIPPED));
        assert!(out.registered.is_empty());
        assert!(out.failed.is_empty());
        assert!(!out.needs_user_action);
        assert_eq!(
            out.message, "No file types selected to register.",
            "an empty request is answered as a no-op, not as a 0-of-0 success"
        );
    }

    /// The count and its noun must agree in the failure sentences (the same
    /// grammar flip the Windows twin pins).
    #[test]
    fn the_failure_count_and_its_noun_agree() {
        let one = summarize_bundle(&[ClaimType::PlainText], Some(EMPTY_PLIST));
        assert!(
            one.message.contains("(1 group undeclared)"),
            "singular for one group, got: {}",
            one.message
        );
        let two = summarize_bundle(
            &[ClaimType::PlainText, ClaimType::Markdown],
            Some(EMPTY_PLIST),
        );
        assert!(
            two.message.contains("(2 groups undeclared)"),
            "plural for two groups, got: {}",
            two.message
        );
    }

    /// The mutation pardon for this file's two mac-only items rests entirely on
    /// them NOT compiling on the ubuntu mutation runner. This module itself
    /// compiles everywhere under `test` (its pure `summarize_bundle` half is
    /// where the fake-success regression lives), so the gate is per-ITEM, and
    /// only the per-item gate makes those mutants vacuous.
    ///
    /// If either item ever loses its gate it compiles on the runner, its
    /// mutants become real signal, and the pardon would start hiding a genuine
    /// gap. Fail here so that cannot happen quietly — the same premise check
    /// `windows_module_is_cfg_gated_so_the_mutation_exclusion_stays_honest`
    /// carries for `integration/windows.rs`.
    #[test]
    fn the_mac_only_items_stay_cfg_gated_so_the_mutation_pardon_stays_honest() {
        let src = include_str!("macos.rs");
        // ASSEMBLED, never written as a literal. This test reads its OWN file,
        // so a literal needle would sit in the source and `contains` would
        // match the needle itself — passing no matter what the real
        // declarations said.
        let gate = ["#[cfg(target_os = ", "\"macos\"", ")]"].concat();
        for item in ["fn bundle_info_plist(", "pub fn register("] {
            let needle = format!("{gate}\n{item}");
            assert_eq!(
                src.matches(needle.as_str()).count(),
                1,
                "`{item}` is no longer directly {gate}-gated (or this test now \
                 self-matches). If it compiles off macOS its mutants are real \
                 signal: DROP the matching `macos\\.rs` entries from \
                 `exclude_re` in .cargo/mutants.toml and delete this test, \
                 rather than leaving a pardon that silently covers a file it \
                 claims to have proven vacuous."
            );
        }
    }
}
