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
pub(crate) mod assoc_stamp;
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
///
/// Two properties this deliberately has, both absent before:
///
/// - **It converges.** The work is skipped outright when nothing has changed,
///   via the persisted stamp in [`assoc_stamp`]. Previously every launch rewrote
///   all 182 byte-identical values; the surfaces added alongside this would have
///   made that 351 writes plus the removal sweep, so converging is what keeps the
///   more complete registration from being a much larger regression.
/// - **It is OFF the main thread.** Each value is a synchronous `reg.exe` spawn,
///   so running the set inline delayed the first frame by seconds — the same
///   freeze the Settings path already moved to a worker to avoid. The thread is
///   detached: nothing depends on its result, and a process that exits before it
///   finishes simply leaves the stamp unwritten and retries next launch.
pub fn reregister_on_startup(config: &scribe_core::config::IntegrationConfig) {
    let types = config.claimed_types();
    if !startup_reregister_types(config.register_file_types, &types) {
        return;
    }
    #[cfg(windows)]
    {
        use windows_entries::{APP_NAME, APP_ROOT, CLASS_ROOT};

        let Some(exe) = std::env::current_exe()
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
        else {
            tracing::warn!(
                target: "scribe::integration",
                "the running program path is unavailable; skipping the startup refresh"
            );
            return;
        };
        let config_dir = scribe_core::Config::config_dir();
        std::thread::spawn(move || {
            // `register_under` is called with the SAME `exe` the stamp is
            // computed from, so the stamp can never describe a registration
            // performed against a different path.
            let outcome = assoc_stamp::startup_refresh(config_dir.as_deref(), &exe, &types, |t| {
                windows::register_under(t, &exe, CLASS_ROOT, APP_ROOT, APP_NAME)
            });
            match outcome {
                assoc_stamp::StartupOutcome::AlreadyCurrent => tracing::debug!(
                    target: "scribe::integration",
                    "file associations already current; no registry work performed"
                ),
                assoc_stamp::StartupOutcome::Registered { count } => tracing::debug!(
                    target: "scribe::integration",
                    count,
                    "refreshed file associations at startup"
                ),
                // A failed silent re-register is not fatal — the user can re-run
                // it from Settings. Record it so a persistent failure is
                // diagnosable rather than silently swallowed.
                assoc_stamp::StartupOutcome::Failed { failures } => tracing::warn!(
                    target: "scribe::integration",
                    failures,
                    "startup file-association refresh had failures"
                ),
            }
        });
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
    const WXS: &str = include_str!("../../wix/main.wxs");

    /// The exe path the MSI writes: the install directory as a WiX property
    /// token. `exe_file_name` understands the `]` terminator, so the generated
    /// `Applications\scr1b3.exe` key matches the one the app writes at runtime.
    const WIX_EXE: &str = "[APPLICATIONFOLDER]scr1b3.exe";

    fn unescape_xml_attr(s: &str) -> String {
        // `&amp;` LAST: doing it first would turn `&amp;quot;` into a quote.
        s.replace("&quot;", "\"")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&")
    }

    /// Every `<RegistryValue>` in the .wxs as `(key, name, value)`, with `name`
    /// empty for the key's default value (the attribute is omitted there, which
    /// is exactly how [`super::windows_entries::RegEntry`] spells it too).
    ///
    /// A deliberately small hand parser rather than an XML dependency: the file
    /// is generated in one shape, and the alternative was adding a dev-dependency
    /// to read six attributes.
    fn wxs_registry_rows() -> Vec<(String, String, String)> {
        let mut out = Vec::new();
        for chunk in WXS.split("<RegistryValue").skip(1) {
            let el = &chunk[..chunk
                .find("/>")
                .expect("an unterminated RegistryValue element")];
            let key = wxs_attr(el, "Key").expect("RegistryValue without a Key");
            out.push((
                key,
                wxs_attr(el, "Name").unwrap_or_default(),
                wxs_attr(el, "Value").expect("RegistryValue without a Value"),
            ));
        }
        out
    }

    /// Read one attribute out of a `<RegistryValue …>` element body.
    ///
    /// The needle's LEADING SPACE anchors the match to an attribute boundary.
    /// Note honestly what that does and does not buy, because the first version
    /// of this comment claimed a bug it cannot actually have: it does NOT stop
    /// `Key` matching inside `KeyPath`, since `Key="` requires `=` immediately
    /// after `Key` and `KeyPath="` has `P` there — removing the space is an
    /// EQUIVALENT mutation for this element grammar (verified by mutation:
    /// dropping it leaves the whole suite green). Nor can `X="` occur inside an
    /// attribute VALUE, because a raw `"` cannot appear in well-formed XML.
    /// It is kept as cheap anchoring against a future attribute whose name ENDS
    /// with another's (`Name` inside a hypothetical `FriendlyName`), which is the
    /// one case that would genuinely mis-parse.
    ///
    /// The part that IS load-bearing is the unescape ORDER in
    /// [`unescape_xml_attr`], pinned by `wxs_attr_reads_attributes_and_unescapes_
    /// entities_in_the_right_order`.
    fn wxs_attr(el: &str, name: &str) -> Option<String> {
        let needle = format!(" {name}=\"");
        let start = el.find(&needle)? + needle.len();
        let end = start + el[start..].find('"')?;
        Some(unescape_xml_attr(&el[start..end]))
    }

    /// Rows the association contract owns. The installer also writes an
    /// unrelated `Software\ItashaCorp\SCR1B3\installed` marker as the Start-Menu
    /// component's KeyPath, which is not an association and must not be dragged
    /// into the comparison.
    fn is_association_row(key: &str) -> bool {
        key.starts_with("Software\\Classes")
            || key.starts_with("Software\\SCR1B3")
            || key == "Software\\RegisteredApplications"
    }

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

    /// The `.wxs` attribute reader must pick the right attribute and unescape
    /// entities in the right ORDER.
    ///
    /// The drift test's whole value depends on this helper: if it silently
    /// misreads, both directions of the comparison degrade into comparing
    /// nonsense against nonsense and still pass. The `&amp;`-LAST ordering is the
    /// real trap — unescaping `&amp;` first rewrites `&amp;quot;` into `&quot;`
    /// and then into a quote, corrupting any value that legitimately contains the
    /// TEXT `&quot;`. The value below carries exactly that sequence, so the
    /// ordering is genuinely exercised rather than merely asserted.
    #[test]
    fn wxs_attr_reads_attributes_and_unescapes_entities_in_the_right_order() {
        let el = concat!(
            r#" Root="HKCU" KeyPath="yes" Key="Software\Classes\.txt" "#,
            r#"Name="SCR1B3.txt" Value="&amp;quot; &amp; &quot;q&quot; &lt;x&gt;" Type="string" "#
        );
        assert_eq!(
            wxs_attr(el, "Key").as_deref(),
            Some("Software\\Classes\\.txt"),
            "a `Key` needle must not read the `KeyPath` attribute"
        );
        assert_eq!(wxs_attr(el, "KeyPath").as_deref(), Some("yes"));
        assert_eq!(wxs_attr(el, "Name").as_deref(), Some("SCR1B3.txt"));
        assert_eq!(
            wxs_attr(el, "Value").as_deref(),
            Some("&quot; & \"q\" <x>"),
            "`&amp;` must be unescaped LAST, or `&amp;quot;` collapses to a quote"
        );
        assert_eq!(wxs_attr(el, "Absent"), None);
    }

    /// The MSI must register EXACTLY what the app registers — no more, no less.
    ///
    /// Unlike `scr1b3.desktop` and `Info.plist`, which were already pinned here,
    /// the `.wxs` was pinned by nothing: it wrote a single `installed=1` marker
    /// and no associations at all, so a fresh MSI install left SCR1B3 absent from
    /// Open-with and Default Apps until the user found the Settings button. The
    /// fix is only half the work — without this test the installer table and the
    /// runtime builder would drift apart silently the first time a surface is
    /// added to one of them.
    ///
    /// Checked in BOTH directions on purpose. Forwards alone would pass while the
    /// installer wrote extra keys the app never writes and therefore never
    /// removes; backwards alone would pass with an installer that registers
    /// nothing.
    #[test]
    fn wix_installer_registers_what_the_app_registers() {
        let expected = super::windows_entries::registry_entries(
            &ClaimType::ALL,
            WIX_EXE,
            super::windows_entries::CLASS_ROOT,
            super::windows_entries::APP_ROOT,
            super::windows_entries::APP_NAME,
        );
        let rows = wxs_registry_rows();
        assert!(
            rows.len() > 300,
            "the parser found only {} RegistryValue rows — it is not reading the \
             file (a silently-empty parse would make every assertion below vacuous)",
            rows.len()
        );

        let actual: Vec<(String, String, String)> = rows
            .into_iter()
            .filter(|(k, _, _)| is_association_row(k))
            .collect();

        for e in &expected {
            let want = (e.key.clone(), e.name.clone(), e.data.clone());
            assert!(
                actual.contains(&want),
                "wix/main.wxs does not register what the app registers — missing \
                 RegistryValue Key={:?} Name={:?} Value={:?}. Regenerate the \
                 FileAssociations component from registry_entries.",
                e.key,
                e.name,
                e.data
            );
        }
        for row in &actual {
            assert!(
                expected
                    .iter()
                    .any(|e| e.key == row.0 && e.name == row.1 && e.data == row.2),
                "wix/main.wxs registers something the app does not, so nothing \
                 ever rewrites or removes it: Key={:?} Name={:?} Value={:?}",
                row.0,
                row.1,
                row.2
            );
        }
        assert_eq!(
            actual.len(),
            expected.len(),
            "the installer and the app must write the same NUMBER of association \
             values (duplicates in the .wxs would pass both directions above)"
        );
    }

    /// The component carrying the association rows must actually be installed.
    /// A `<Component>` that no `<Feature>` references is compiled into the MSI
    /// and never installed — the table would exist and do nothing, which is the
    /// same "registers nothing" outcome with a passing parity test.
    #[test]
    fn the_file_association_component_is_referenced_by_the_installed_feature() {
        assert!(
            WXS.contains("<Component Id=\"FileAssociations\""),
            "the FileAssociations component is gone"
        );
        assert!(
            WXS.contains("<ComponentRef Id=\"FileAssociations\" />"),
            "the FileAssociations component is not referenced by any Feature, so \
             the MSI would install none of its registry values"
        );
    }

    /// The `.wxs` exe token must be the install-directory property, not a path
    /// baked at authoring time. A literal path would install associations
    /// pointing at whatever machine generated the file.
    #[test]
    fn the_installer_registers_the_install_directory_executable() {
        let rows = wxs_registry_rows();
        let cmd = rows
            .iter()
            .find(|(k, _, _)| k == "Software\\Classes\\SCR1B3.txt\\shell\\open\\command")
            .expect("the .txt ProgID open command");
        assert_eq!(
            cmd.2,
            format!("\"{WIX_EXE}\" \"%1\""),
            "the installer's open command must use the [APPLICATIONFOLDER] token"
        );
        assert!(
            rows.iter()
                .any(|(k, _, _)| k == "Software\\Classes\\Applications\\scr1b3.exe"),
            "the Applications key must resolve to the bare exe name, not the \
             property token — exe_file_name's `]` rule is what guarantees that"
        );
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

    /// The `mutation-in-diff` shard count is stated THREE times and all three
    /// must agree: the matrix entry list, the `SHARDS` the pre-flight cap
    /// multiplies by, and the denominator in `--shard N/D`.
    ///
    /// A divergence is silent in the worst direction. If the matrix lists fewer
    /// entries than the denominator, the missing shards' mutants are never run
    /// and every job still reports green — the gate claims to have tested the
    /// diff while a slice of it was skipped. If `SHARDS` disagrees with either,
    /// the pre-flight either refuses a PR it could have handled or waves
    /// through one it cannot finish, which is the 6-hour silent cancellation
    /// the pre-flight exists to prevent.
    ///
    /// Scoped to the `mutation-in-diff` job body: `mutation-app` legitimately
    /// shards 12 ways, so a whole-file scan would conflate the two.
    #[test]
    fn the_mutation_in_diff_shard_count_agrees_in_all_three_places() {
        let ci = include_str!("../../../../.github/workflows/ci.yml");
        let start = ci
            .find("\n  mutation-in-diff:")
            .expect("the mutation-in-diff job must exist");
        let end = start
            + ci[start + 1..]
                .find("\n  mutation:")
                .expect("mutation-in-diff must be followed by the mutation job");
        let job = &ci[start..end];

        // 1. the `--shard N/D` denominator
        let flag = "--shard ${{ matrix.shard }}/";
        let after = job
            .split(flag)
            .nth(1)
            .expect("the mutation step must pass --shard");
        let denominator: usize = after
            .chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
            .parse()
            .expect("--shard denominator must be a number");

        // 2. the `SHARDS:` the pre-flight cap multiplies by
        let after = job
            .split("SHARDS: ")
            .nth(1)
            .expect("the pre-flight must declare SHARDS");
        let declared: usize = after
            .chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
            .parse()
            .expect("SHARDS must be a number");

        // 3. the matrix entries actually dispatched
        let after = job
            .split("shard:")
            .nth(1)
            .expect("the matrix must declare shard entries");
        let list = &after[..after.find(']').expect("the shard list must be closed")];
        let mut entries: Vec<usize> = list
            .trim_start()
            .trim_start_matches('[')
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.parse().expect("every shard entry must be a number"))
            .collect();
        entries.sort_unstable();

        assert_eq!(
            entries.len(),
            denominator,
            "the matrix dispatches {} shard(s) but `--shard N/{denominator}` \
             splits the work {denominator} ways — the shards with no matrix \
             entry are NEVER RUN and the job still reports green",
            entries.len()
        );
        assert_eq!(
            declared, denominator,
            "the pre-flight cap multiplies MAX_PER_SHARD by {declared} while the \
             work is split {denominator} ways, so the refusal threshold does not \
             match what the gate can actually test"
        );
        assert_eq!(
            entries,
            (0..denominator).collect::<Vec<_>>(),
            "the shard entries must be exactly 0..{denominator} with no gaps or \
             duplicates — cargo-mutants indexes shards from 0, so a gap silently \
             drops that shard's mutants and a duplicate wastes a runner"
        );
    }
}
