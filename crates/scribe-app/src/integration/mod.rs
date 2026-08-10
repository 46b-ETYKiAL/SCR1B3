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

    /// A POSIX shell to run `packaging/*.sh` with: `sh` on PATH (Linux/macOS),
    /// else the Git-for-Windows one — the same interpreter every `shell: bash`
    /// workflow step runs under, and the one `sh packaging/…` resolves to on the
    /// windows runner.
    ///
    /// A host with NO shell is a hard failure, never a skip. The whole point of
    /// the tests below is that the signing gate can be shown to FAIL; a test
    /// that quietly does not run would re-create, in the test layer, the exact
    /// defect it exists to catch.
    fn posix_shell() -> std::process::Command {
        const CANDIDATES: [&str; 6] = [
            "sh",
            "bash",
            r"C:\Program Files\Git\usr\bin\sh.exe",
            r"C:\Program Files\Git\bin\sh.exe",
            r"C:\Program Files\Git\bin\bash.exe",
            r"C:\Program Files (x86)\Git\bin\sh.exe",
        ];
        for c in CANDIDATES {
            if std::process::Command::new(c)
                .args(["-c", "exit 0"])
                .output()
                .is_ok_and(|o| o.status.success())
            {
                return std::process::Command::new(c);
            }
        }
        panic!(
            "no POSIX shell found (tried {CANDIDATES:?}) — cannot execute \
             packaging/require-signing-key.sh, so the release signing gate is \
             UNVERIFIED on this host. Install a shell rather than skipping: an \
             unfalsifiable gate is the defect this test exists to prevent."
        );
    }

    /// Run `packaging/require-signing-key.sh` for one ref, as the release
    /// workflow does when `MINISIGN_SECRET_KEY` is empty. Returns its exit code
    /// and everything it printed.
    fn run_signing_guard(ref_type: &str, ref_name: &str) -> (Option<i32>, String) {
        // Forward slashes: CARGO_MANIFEST_DIR is backslashed on Windows and the
        // MSYS argument translation mangles a mixed-separator path.
        let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../packaging/require-signing-key.sh")
            .to_string_lossy()
            .replace('\\', "/");
        let out = posix_shell()
            .arg(&script)
            .env("GITHUB_REF_TYPE", ref_type)
            .env("GITHUB_REF_NAME", ref_name)
            // The workflow only reaches the guard when the secret is empty; make
            // that precondition explicit rather than inherited from the host.
            .env_remove("MINISIGN_SECRET_KEY")
            .output()
            .unwrap_or_else(|e| panic!("could not run {script}: {e}"));
        let log = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.code(), log)
    }

    /// THE falsification this gate was missing. Release publishing verifies its
    /// own signatures — but the whole block sat behind an early `exit 0` taken
    /// whenever `MINISIGN_SECRET_KEY` was empty, so the signing AND its
    /// self-verify were skipped together and the release still went green,
    /// publishing artifacts the fail-closed in-app updater REJECTS. A gate that
    /// cannot fail is not a gate; this test proves this one can.
    #[test]
    fn the_signing_gate_fails_on_a_stable_tag_with_no_key() {
        let (code, log) = run_signing_guard("tag", "v0.4.63");
        assert_eq!(
            code,
            Some(1),
            "an unsigned STABLE tag must FAIL the release — every deployed \
             client would reject the artifacts and auto-update would silently \
             stop working. Guard output:\n{log}"
        );
        assert!(
            log.contains("::error::"),
            "the failure must annotate the run as an error so it is visible in \
             the GitHub UI, not just a non-zero exit. Guard output:\n{log}"
        );
    }

    /// The other half: the gate must fail ONLY where it should. A guard that
    /// failed everything would be "shown red" while making every dispatch run
    /// and every rc tag unshippable — and the non-tag path is an open owner
    /// decision that this change deliberately does not pre-empt.
    #[test]
    fn the_signing_gate_still_tolerates_a_prerelease_tag_and_a_non_tag_ref() {
        for (ref_type, ref_name) in [
            ("tag", "v0.4.63-rc.1"),
            ("tag", "v0.4.63-pre"),
            ("branch", "master"),
            ("", ""),
        ] {
            let (code, log) = run_signing_guard(ref_type, ref_name);
            assert_eq!(
                code,
                Some(0),
                "{ref_type}/{ref_name} must still be allowed to ship unsigned \
                 (rc/pre builds are opt-in downloads, and the non-tag path is \
                 unchanged by design). Guard output:\n{log}"
            );
            assert!(
                log.contains("::warning::"),
                "shipping unsigned must still be WARNED about, never silent — \
                 the updater will reject these artifacts. Guard output:\n{log}"
            );
        }
    }

    /// The guard is only worth anything if the workflow actually calls it. This
    /// pins the wiring: inside the empty-secret branch, `require-signing-key.sh`
    /// must run BEFORE the `exit 0` that skips signing + self-verify — and it
    /// must be invoked at a path that RESOLVES.
    ///
    /// The path half is not pedantry, it is the defect this test failed to
    /// catch. The step read `sh packaging/require-signing-key.sh` while the
    /// `release` job checks out with `path: src` and sets no
    /// `working-directory:`/`defaults:`, so $PWD was $GITHUB_WORKSPACE and the
    /// file was one level down at `src/packaging/…`. `sh` on a missing file
    /// exits 127 under `set -e`, so a PRERELEASE tag with no key HARD-FAILED
    /// instead of warning and continuing as documented, and a stable tag failed
    /// before printing the ::error:: that explains why — the right outcome for
    /// the wrong reason, which is indistinguishable from the gate working.
    ///
    /// A `contains("packaging/require-signing-key.sh")` cannot see any of that:
    /// it matches `src/packaging/…` and a bare `packaging/…` identically. So the
    /// checkout `path:` is read out of the job and asserted as the PREFIX of the
    /// invoked path, which is the property that actually has to hold.
    #[test]
    fn the_release_signing_step_calls_the_key_guard_before_returning_early() {
        const RELEASE: &str = include_str!("../../../../.github/workflows/release.yml");
        const SCRIPT: &str = "packaging/require-signing-key.sh";

        // The `release` job is the last in the file, so its body runs to EOF.
        let job_start = RELEASE
            .find("\n  release:")
            .expect("release.yml must declare a `release` job");
        let job = &RELEASE[job_start..];

        // The checkout `path:` the job's own tree lives under. `None` = a root
        // checkout, in which case the bare script path is the correct one.
        let checkout_path = job.split("\n          path: ").nth(1).map(|tail| {
            tail.lines()
                .next()
                .expect("a `path:` value must be on its line")
                .trim()
                .to_string()
        });
        let expected = match checkout_path.as_deref() {
            // `dist` is download-artifact's destination, not a source checkout;
            // if the first `path:` in the job is that, the source tree is at the
            // root and the bare path is right.
            Some(p) if p != "dist" => format!("{p}/{SCRIPT}"),
            _ => SCRIPT.to_string(),
        };

        let marker = "if [ -z \"${MINISIGN_SECRET_KEY:-}\" ]; then";
        let start = job
            .find(marker)
            .expect("release.yml must still branch on an empty MINISIGN_SECRET_KEY");
        let branch = &job[start..];
        let end = branch
            .find("\n          fi")
            .expect("the empty-secret branch must be closed");
        let branch = &branch[..end];

        let guard = branch.find(SCRIPT).unwrap_or_else(|| {
            panic!(
                "the empty-secret branch of release.yml no longer calls \
                 {SCRIPT}, so an unsigned STABLE tag publishes green \
                 again:\n{branch}"
            )
        });
        assert!(
            branch.contains(&format!("sh {expected}")),
            "the branch invokes {SCRIPT} but not at `{expected}`, which is where \
             the job's own checkout puts it ({checkout_path:?}). `sh` on a path \
             that does not resolve exits 127 under `set -e`, so the gate fails \
             for the WRONG reason: a prerelease tag hard-fails instead of \
             warning, and a stable tag never prints its ::error::.\n{branch}"
        );
        let early_exit = branch
            .find("exit 0")
            .expect("the branch must still skip signing when the key is absent");
        assert!(
            guard < early_exit,
            "the guard must run BEFORE the `exit 0`; after it, it is \
             unreachable and the gate is decorative:\n{branch}"
        );
    }

    /// A hyphen is not a prerelease. The classifier was
    /// `case "${REF_NAME}" in *-*)`, and release.yml triggers on `v*`, so every
    /// stable tag that merely CONTAINED a hyphen took the unsigned-is-fine path
    /// and published green artifacts every deployed client rejects.
    ///
    /// These are the tags that were mis-sorted. They are real shapes, not
    /// contrivances: a `-final`/`-hotfix` suffix on a two-field version and a
    /// date-style tag.
    #[test]
    fn a_hyphenated_non_semver_tag_is_stable_and_must_fail_unsigned() {
        for tag in ["v1.0-final", "v0.5-hotfix", "v2026-08-10", "v1.2.3.4-x"] {
            let (code, log) = run_signing_guard("tag", tag);
            assert_eq!(
                code,
                Some(1),
                "{tag} carries no SemVer prerelease segment, so it is a STABLE \
                 tag and must FAIL unsigned. Classifying it as a prerelease \
                 ships a release the fail-closed updater rejects. Guard \
                 output:\n{log}"
            );
        }
    }

    /// The other side of the same predicate: a well-formed SemVer prerelease —
    /// including one carrying build metadata — must still be tolerated, or the
    /// fix above would have been "reject everything", which passes the test
    /// above while making every rc tag unshippable.
    #[test]
    fn a_wellformed_semver_prerelease_is_still_tolerated() {
        for tag in ["v0.4.63-rc.1", "0.4.63-rc.1", "v1.2.3-alpha.2+build.5"] {
            let (code, log) = run_signing_guard("tag", tag);
            assert_eq!(
                code,
                Some(0),
                "{tag} IS a SemVer prerelease and must still be allowed to ship \
                 unsigned with a warning. Guard output:\n{log}"
            );
        }
    }

    /// Run `packaging/semver-tag-class.sh` for one ref, exactly as both the
    /// signing guard and release.yml's publish step do. Returns its verdict.
    fn run_tag_class(ref_type: &str, ref_name: &str) -> String {
        let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../packaging/semver-tag-class.sh")
            .to_string_lossy()
            .replace('\\', "/");
        let out = posix_shell()
            .arg(&script)
            .env("GITHUB_REF_TYPE", ref_type)
            .env("GITHUB_REF_NAME", ref_name)
            .output()
            .unwrap_or_else(|e| panic!("could not run {script}: {e}"));
        assert!(
            out.status.success(),
            "the classifier must always answer, never fail: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// THE invariant that closes the unsigned-published-as-latest hole.
    ///
    /// Two independently-defensible behaviours combined into it: the signing
    /// guard deliberately TOLERATES an unsigned prerelease (rc builds are
    /// opt-in downloads, not auto-update targets), while the publish step had
    /// no notion of a prerelease at all and forced `--latest` on every ref. So
    /// `v0.5.0-rc.1` with no signing key published UNSIGNED artifacts as the
    /// repo's CURRENT release — served by every download link and offered to
    /// the updater, which is built to reject exactly those bytes.
    ///
    /// The general form of the bug is a DISAGREEMENT: an artifact
    /// simultaneously "unsigned because prerelease" and "published because
    /// stable". So the property asserted here is the agreement itself:
    ///
    ///     class == "stable"  <=>  an unsigned build is FORBIDDEN (guard exits 1)
    ///
    /// Both sides now read the same file, so they cannot drift — but that is an
    /// implementation detail. This pins the RELATIONSHIP, so it still holds if
    /// either side is ever reimplemented.
    #[test]
    fn the_signing_guard_and_the_publish_step_agree_on_what_a_prerelease_is() {
        // Deliberately spans both sides of the predicate, including the shapes
        // a naive "contains a hyphen" test gets wrong.
        for (ref_type, tag) in [
            ("tag", "v0.4.63"),               // stable
            ("tag", "v1.2.3"),                // stable
            ("tag", "v0.4.63-rc.1"),          // prerelease
            ("tag", "v0.5.0-hotfix"),         // well-formed SemVer prerelease
            ("tag", "v1.2.3-alpha.2+build.5"), // prerelease w/ build metadata
            ("tag", "v1.0-final"),            // NOT SemVer -> stable
            ("tag", "v0.5-hotfix"),           // NOT SemVer -> stable
            ("tag", "v2026-08-10"),           // date tag -> stable
            ("branch", "master"),             // dispatch build
        ] {
            let class = run_tag_class(ref_type, tag);
            let (code, log) = run_signing_guard(ref_type, tag);
            let unsigned_forbidden = code == Some(1);
            assert_eq!(
                class == "stable",
                unsigned_forbidden,
                "{ref_type}/{tag}: the classifier says `{class}` but the signing \
                 guard {} an unsigned build (exit {code:?}). These two MUST \
                 agree: if they disagree, an artifact can be both `unsigned \
                 because prerelease` and `published as latest because stable`, \
                 which is exactly the hole this pair of checks exists to close. \
                 Guard output:\n{log}",
                if unsigned_forbidden { "FORBIDS" } else { "ALLOWS" }
            );
        }
    }

    /// The agreement above is only worth anything if the publish step actually
    /// consults the shared classifier and acts on it. This pins that wiring.
    ///
    /// The step previously ended in an unconditional
    /// `gh release edit … --draft=false --latest`, so a prerelease was promoted
    /// to Latest regardless. A `contains("--latest")` cannot tell that apart
    /// from the correct version, so the two branches are read out and checked
    /// for OPPOSITE markings.
    #[test]
    fn the_publish_step_marks_a_prerelease_and_withholds_latest() {
        const RELEASE: &str = include_str!("../../../../.github/workflows/release.yml");
        const SCRIPT: &str = "packaging/semver-tag-class.sh";

        let job_start = RELEASE
            .find("\n  release:")
            .expect("release.yml must declare a `release` job");
        let job = &RELEASE[job_start..];

        // Same checkout-path reasoning as the signing-guard wiring test: the
        // job checks out with `path: src`, so a bare path would not resolve.
        let checkout_path = job.split("\n          path: ").nth(1).map(|tail| {
            tail.lines()
                .next()
                .expect("a `path:` value must be on its line")
                .trim()
                .to_string()
        });
        let expected = match checkout_path.as_deref() {
            Some(p) if p != "dist" => format!("{p}/{SCRIPT}"),
            _ => SCRIPT.to_string(),
        };

        let step_start = job
            .find("- name: Create GitHub Release")
            .expect("release.yml must still have a `Create GitHub Release` step");
        let step = &job[step_start..];

        assert!(
            step.contains(&format!("sh {expected}")),
            "the publish step must derive the release class from `{expected}`, \
             the SAME file the signing guard reads. Restating the predicate \
             inline lets the two drift apart, and the drift is the bug.\n{step}"
        );

        let branch_start = step
            .find("if [ \"$class\" = \"stable\" ]; then")
            .expect(
                "the publish step must branch on the release class; without a \
                 branch it marks every ref the same way, which is the defect",
            );
        let branch = &step[branch_start..];
        let else_at = branch
            .find("\n          else")
            .expect("the class branch must have an else");
        let stable = &branch[..else_at];
        let prerelease = &branch[else_at..];

        assert!(
            stable.contains("--latest") && !stable.contains("--latest=false"),
            "a STABLE tag must still be promoted to Latest — withholding it \
             from everything would 'fix' the hole by making no release ever \
             current:\n{stable}"
        );
        assert!(
            prerelease.contains("--latest=false"),
            "a PRERELEASE must NOT be promoted to Latest, or unsigned rc \
             artifacts are served as the current release:\n{prerelease}"
        );
        assert!(
            prerelease.contains("--prerelease") && !prerelease.contains("--prerelease=false"),
            "a PRERELEASE must be MARKED as one, so the GitHub UI and the API \
             both report it correctly:\n{prerelease}"
        );
    }

    /// Both msi jobs must resolve the installer version by the SAME mechanism.
    ///
    /// ci.yml's `msi-build` exists so that CI green predicts release green. It
    /// used `cargo pkgid --offline -p scribe-app` while release.yml's
    /// `windows-msi` read Cargo.toml directly — the same answer today, but two
    /// different mechanisms, so CI could not fail for a reason the tag-time job
    /// can. A mirrored gate whose mirror differs in the one step that computes
    /// what gets stamped into the package is not a mirror.
    #[test]
    fn both_msi_jobs_resolve_the_version_the_same_way() {
        const CI: &str = include_str!("../../../../.github/workflows/ci.yml");
        const RELEASE: &str = include_str!("../../../../.github/workflows/release.yml");

        // To end of LINE, not to the first `)"` — the shared `sed -E
        // 's/.*"([^"]+)".*/\1/'` contains a `)"` of its own, so stopping there
        // would compare only a prefix and could call two different tails equal.
        let extract = |job: &str, label: &str| -> String {
            job.split("VER=\"$(")
                .nth(1)
                .unwrap_or_else(|| panic!("the `{label}` job must resolve a VER"))
                .lines()
                .next()
                .expect("the VER assignment must be on one line")
                .trim()
                .to_string()
        };
        let ci_ver = extract(job_body(CI, "msi-build", "gate"), "msi-build");
        let rel_ver = extract(
            job_body(RELEASE, "windows-msi", "linux-installers"),
            "windows-msi",
        );
        assert_eq!(
            ci_ver, rel_ver,
            "the two msi jobs compute the installer version differently, so the \
             CI job cannot fail for a version-resolution reason the tag-time job \
             can — the exact blind spot the mirrored job exists to remove"
        );
    }

    /// Slice one job body out of a workflow, from its `  <name>:` header to the
    /// header of the job that follows it. Both bounds are named explicitly (the
    /// same shape `the_mutation_in_diff_shard_count_agrees_in_all_three_places`
    /// uses) so a rename cannot silently shrink the window to nothing and make
    /// every `contains` assertion below vacuously... fail — which is the safe
    /// direction, and why `expect` is used rather than a default.
    fn job_body<'a>(workflow: &'a str, job: &str, next_job: &str) -> &'a str {
        let start = workflow
            .find(&format!("\n  {job}:"))
            .unwrap_or_else(|| panic!("the `{job}` job must exist"));
        let end = start
            + workflow[start + 1..]
                .find(&format!("\n  {next_job}:"))
                .unwrap_or_else(|| panic!("`{job}` must be followed by `{next_job}`"));
        &workflow[start..end]
    }

    /// `light` links a package only if EVERY `<ComponentGroupRef>` in the .wxs
    /// resolves to a group some fragment DEFINES. `main.wxs` references
    /// `LicenseComponents`, and nothing in this repo authors that group by hand:
    /// it exists solely as the output of a `heat` harvest over the staged license
    /// tree (a hand-written list of 22 font directories would rot the moment a
    /// font is added or dropped).
    ///
    /// A workflow that runs candle+light but NOT the harvest therefore cannot
    /// link at all — it dies with
    ///   `main.wxs(520) : error LGHT0094 : Unresolved reference to symbol
    ///    'WixComponentGroup:LicenseComponents' in section 'Product:*'`
    /// — which is exactly how ci.yml's `msi-build` job stood: deterministically
    /// red on every single push, because the harvest step lived only in
    /// release.yml. This test is what makes that impossible to reintroduce.
    ///
    /// Asserted over BOTH msi jobs and over every referenced group, and it covers
    /// the two adjacent ways to fail identically: generating the fragment and
    /// then not LINKING it, and harvesting with `-var var.X` while candle is
    /// never given the matching `-dX=` (CNDL0150).
    #[test]
    fn both_msi_jobs_resolve_every_component_group_main_wxs_references() {
        const CI: &str = include_str!("../../../../.github/workflows/ci.yml");
        const RELEASE: &str = include_str!("../../../../.github/workflows/release.yml");

        // Every `<ComponentGroupRef Id="…">` in main.wxs.
        let refs: Vec<String> = WXS
            .split("<ComponentGroupRef")
            .skip(1)
            .map(|tail| {
                let after = tail
                    .split("Id=\"")
                    .nth(1)
                    .expect("a ComponentGroupRef must carry an Id");
                after[..after.find('"').expect("the Id must be quoted")].to_string()
            })
            .collect();
        // Non-vacuity: with no refs collected every loop below is skipped and the
        // test passes no matter what the workflows do.
        assert!(
            !refs.is_empty(),
            "main.wxs declares no <ComponentGroupRef> at all — either the license \
             components were dropped from the package, or this test's parser no \
             longer matches the source and is now asserting nothing"
        );

        for (workflow, job_name, next_job) in [
            (CI, "msi-build", "gate"),
            (RELEASE, "windows-msi", "linux-installers"),
        ] {
            let job = job_body(workflow, job_name, next_job);
            for id in &refs {
                let cg = format!("-cg {id} ");
                let generated = job.contains(&cg);
                let authored = WXS.contains(&format!("<ComponentGroup Id=\"{id}\""));
                assert!(
                    generated || authored,
                    "the `{job_name}` job never defines the component group \
                     `{id}` that main.wxs references: it neither harvests it \
                     (`heat … -cg {id}`) nor is it authored in the .wxs. `light` \
                     cannot resolve the reference, so the job fails with LGHT0094 \
                     on EVERY run. Add the heat harvest (mirror the other msi \
                     job) — do NOT drop the ComponentGroupRef: the license texts \
                     are a shipping obligation, not decoration."
                );
                if !generated {
                    continue;
                }

                // The harvested fragment must be compiled AND handed to `light`.
                // Generating `licenses.wxs` and never linking `licenses.wixobj`
                // fails with the very same LGHT0094.
                let after = &job[job.find(&cg).expect("just matched")..];
                let frag = after
                    .split("-out ")
                    .nth(1)
                    .expect("the heat harvest must name an -out fragment")
                    .split_whitespace()
                    .next()
                    .expect("the -out fragment must be a filename");
                let obj = frag.replace(".wxs", ".wixobj");
                let light = &job[job.find("light.exe").expect("the job must run light")..];
                assert!(
                    light.contains(&obj),
                    "the `{job_name}` job harvests `{id}` into {frag} but never \
                     passes {obj} to light — the fragment is generated and then \
                     dropped, which fails with LGHT0094 exactly as if it had \
                     never been harvested at all"
                );

                // `heat -var var.X` emits `$(var.X)` into the fragment, so candle
                // must be given `-dX=…` for both the fragment AND main.wxs.
                if let Some(tail) = after.split("-var var.").nth(1) {
                    let var: String = tail
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_')
                        .collect();
                    assert!(
                        job.matches(&format!("-d{var}=")).count() >= 2,
                        "the `{job_name}` job harvests with `-var var.{var}` but \
                         does not pass `-d{var}=` to BOTH candle invocations \
                         (main.wxs and the fragment) — the one that misses it \
                         fails with CNDL0150 (undefined preprocessor variable)"
                    );
                }
            }
        }
    }
}
