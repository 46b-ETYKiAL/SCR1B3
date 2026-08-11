//! Proof that every control exposed in the Settings window — and every section a
//! user can write in `scr1b3.toml` — is actually WIRED to runtime behavior.
//!
//! A "dead" control is one that renders, persists, and reads back correctly while
//! **nothing consumes it**: the user moves a slider and the app does not change.
//! This guard fails the build when such a control exists.
//!
//! # What changed, and why
//!
//! This guard used to live in `settings.rs` and its whole consumer test was
//! `if src.contains(field)` over the raw concatenation of every `.rs` file in
//! `scribe-app/src` + `scribe-core/src` — comments, string literals and all test
//! code included. Naming a field in a doc comment, in a test, or inside a string
//! was enough to look wired. `KNOWN_DEAD` was empty, so `known_dead_controls_are_still_dead`
//! iterated zero times, and **nothing anywhere proved the guard could report a
//! dead control at all**: a `consumed` that had degraded to `true` for every
//! input would have been indistinguishable from a working one.
//!
//! The scanner now in `common/mod.rs` strips comments, literals, `#[cfg(test)]`
//! items and whole test-only files, and probes on identifier boundaries. The
//! §Falsification tests below are the part that matters: each one feeds the guard
//! a case that the old version passed and requires it to FAIL, and
//! [`the_guard_reports_a_control_nothing_reads`] requires the real guard, on the
//! real tree, to name a control nothing reads.
//!
//! `settings.rs` itself stays excluded from the corpus: it is the UI that renders
//! the control, so reading a field there is what makes it a control, not what
//! makes it wired.

mod common;

use std::collections::BTreeSet;
use std::path::Path;

use common::{
    cfg_attributes, contains_token, is_excluded, runtime_source, runtime_source_outside_config,
    strip_cfg_test_items, strip_comments_and_literals, stripped_files, test_module_names,
};

// ---------------------------------------------------------------------------
// Consumption probe
// ---------------------------------------------------------------------------

/// Config-module methods that read `self.<field>`, derived from the config
/// source itself.
///
/// A well-designed config field is often NOT read by name at the call site: it
/// is reached through a clamping or resolving accessor, so a malformed user
/// config cannot drive the UI out of band. `fonts.line_height` is read via
/// `clamped_line_height()`, `motion.enabled` via `effective_enabled()`.
///
/// This used to be a HAND-WRITTEN table, and that is precisely why it was
/// wrong: it listed 14 accessors and omitted those two, so the guard reported
/// `fonts.line_height` and `motion.enabled` as dead controls when both are
/// live and heavily used (`clamped_line_height` alone has five production call
/// sites). Acting on that verdict would have deleted two working features.
///
/// A hand-maintained table re-breaks every time someone adds an accessor, so
/// the mapping is now DERIVED: for each `impl` block in the config module, any
/// method whose body mentions `self.<field>` is an accessor for that field.
/// Adding a new accessor cannot create a false positive, because nobody has to
/// remember to register it.
fn accessors_reading(config_src: &str, field_leaf: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let needle = format!("self.{field_leaf}");
    // Method bodies are delimited by the next `fn ` at the same nesting; a
    // simple split on `fn ` is enough because we only need "does this body
    // mention the field", not a parse.
    for chunk in config_src.split("fn ").skip(1) {
        let Some(name_end) = chunk.find(['(', '<', ' ']) else {
            continue;
        };
        let name = chunk[..name_end].trim();
        if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            continue;
        }
        if contains_token(chunk, &needle) {
            found.insert(name.to_string());
        }
    }
    found
}

/// The config module's own source, stripped of comments and literals.
///
/// Computed once: it is the corpus [`accessors_reading`] mines for accessors.
fn config_source() -> &'static str {
    static SRC: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SRC.get_or_init(|| {
        // Every file the full corpus has but the outside-config corpus lacks IS
        // the config module — derived rather than hard-coded so a config-module
        // move cannot silently empty this.
        let mut out = String::new();
        for (path, body) in stripped_files(false) {
            let p = path.to_string_lossy().replace('\\', "/");
            if p.contains("/config/") || p.ends_with("/config.rs") {
                out.push_str(&body);
                out.push('\n');
            }
        }
        out
    })
}

/// A field is consumed if its `section.field` access — or any config accessor
/// that reads it — appears in production code.
fn consumed(src: &str, field: &str) -> bool {
    if contains_token(src, field) {
        return true;
    }
    let leaf = field.rsplit('.').next().unwrap_or(field);
    accessors_reading(config_source(), leaf)
        .iter()
        .any(|a| contains_token(src, a))
}

/// The fields in `fields` that `src` does NOT consume. This is the guard's
/// verdict function, extracted so the negative-control tests can prove it fires.
fn dead_controls<'a>(src: &str, fields: &[&'a str]) -> Vec<&'a str> {
    fields
        .iter()
        .copied()
        .filter(|f| !consumed(src, f))
        .collect()
}

/// Strip a synthetic snippet exactly as the real corpus is stripped.
fn scan(src: &str) -> String {
    strip_cfg_test_items(&strip_comments_and_literals(src))
}

// ---------------------------------------------------------------------------
// The audited surfaces
// ---------------------------------------------------------------------------

/// Every Settings-exposed config field that MUST have a runtime consumer.
const WIRED: &[&str] = &[
    "appearance.theme",
    "appearance.frameless",
    "appearance.toolbar_in_titlebar",
    "appearance.toolbar_icons",
    "appearance.jp_glyph_labels",
    "appearance.background_override",
    "appearance.note_background_override",
    "appearance.link_backgrounds",
    "fonts.editor_size",
    "fonts.line_height",
    "fonts.editor_family",
    "fonts.ui_family",
    "editor.note_theme",
    "editor.tab_width",
    "editor.insert_spaces",
    "editor.show_line_numbers",
    "editor.show_change_bar",
    "editor.word_wrap",
    "editor.show_minimap",
    "editor.render_whitespace",
    "editor.snippets_enabled",
    "editor.current_line_highlight",
    "editor.indent_guides",
    "editor.bracket_match",
    "editor.highlight_selection_occurrences",
    "editor.highlight_trailing_whitespace",
    "editor.smooth_scroll",
    "editor.caret_style",
    "editor.caret_width",
    "editor.scrollbar_style",
    "editor.tab_bar_position",
    "editor.side_tabs_rotated",
    "editor.side_tabs_wrap_two_lines",
    "editor.restore_session",
    "editor.grid_enabled",
    "editor.experimental_rope_editor",
    "editor.session_backup",
    "editor.auto_save",
    "editor.trim_trailing_whitespace_on_save",
    "editor.final_newline_on_save",
    "editor.restore_cursor_position",
    "window.always_on_top",
    "window.transparency_enabled",
    "window.opacity",
    "window.tint",
    "window.tint_strength",
    "spellcheck.enabled",
    "spellcheck.language",
    "spellcheck.check_comments",
    "spellcheck.check_strings",
    "spellcheck.check_identifiers",
    "spellcheck.custom_dict_path",
    "plugins.enabled",
    "toolbar.button_size_px",
    "toolbar.button_spacing_px",
    "toolbar.icon_size_px",
    "appearance.follow_os_theme",
    "updates.mode",
    "updates.check_interval_hours",
    "motion.enabled",
    "motion.intensity",
    "motion.cursor_blink",
    "motion.crt_scanlines",
    "motion.scanline_darkness",
    "motion.wired_ambient",
    "motion.mesh_density",
    "motion.mesh_brightness",
    "motion.vhs_tracking",
    "motion.vhs_speed",
    "motion.flicker",
    "motion.flicker_strength",
    "motion.flicker_speed",
    "motion.mesh_drift_speed",
    "motion.mesh_color",
    "motion.caret_trail",
    "motion.caret_trail_intensity",
    "motion.boot_glitch",
    "ui_scale",
];

/// Controls audited as DEAD (no runtime consumer yet). Shrinks as phases wire
/// them; an entry here that gains a consumer fails the guard (move it to WIRED).
/// Currently EMPTY: every Settings-exposed control has a runtime consumer.
/// Controls that could not be made to work (egui-impossible font-family /
/// ligatures, the bespoke motion catalog, OS reduced-motion / battery gates) were
/// removed rather than left as dead toggles, so there is nothing left to track.
///
/// An empty list makes `known_dead_controls_are_still_dead` a vacuous pass by
/// construction; [`the_guard_reports_a_control_nothing_reads`] is the test that
/// keeps the underlying detector honest regardless.
const KNOWN_DEAD: &[&str] = &[];

/// Every top-level key a user can write in `scr1b3.toml`, each of which MUST be
/// read by runtime code outside the config module.
///
/// `schema_version` is deliberately absent: it is migration metadata whose only
/// legitimate consumer (`Config::migrate`) lives inside the config module.
const WIRED_SECTIONS: &[&str] = &[
    "editor",
    "appearance",
    "fonts",
    "window",
    "updates",
    "spellcheck",
    "plugins",
    "toolbar",
    "motion",
    "scroll",
    "reporting",
    "integration",
    "keybindings",
    "ui_scale",
];

// ---------------------------------------------------------------------------
// The guards
// ---------------------------------------------------------------------------

#[test]
fn every_wired_setting_has_a_runtime_consumer() {
    let src = runtime_source();
    let dead = dead_controls(&src, WIRED);
    assert!(
        dead.is_empty(),
        "DEAD CONTROL(S): {dead:?} are exposed in Settings but no production CODE \
         reads them (comments, string literals and test code do not count)",
    );
}

#[test]
fn known_dead_controls_are_still_dead() {
    let src = runtime_source();
    for &field in KNOWN_DEAD {
        assert!(
            !consumed(&src, field),
            "`{field}` now has a consumer -- wire-up done; remove it from KNOWN_DEAD and add to WIRED",
        );
    }
}

/// The section-level companion to `every_wired_setting_has_a_runtime_consumer`.
///
/// That guard only audits controls exposed in the Settings WINDOW, so a config
/// surface with no Settings UI is invisible to it. `[keybindings]` shipped that
/// way: 35 rebindable actions that parsed, validated, and were read by nothing,
/// while every Settings-exposed control was correctly wired and the guard stayed
/// green. This checks the other axis — a whole section that nothing consumes.
#[test]
fn every_config_section_has_a_runtime_consumer() {
    let src = runtime_source_outside_config();
    for &section in WIRED_SECTIONS {
        // A section is consumed if its field access appears, or — for one read
        // only through a config METHOD — if that method does. `ui_scale` is
        // reached via `Config::effective_ui_scale` (the clamp/NaN guard), so the
        // literal `.ui_scale` never appears at the call site.
        let hit = contains_token(&src, &format!(".{section}"))
            || match section {
                "ui_scale" => contains_token(&src, "effective_ui_scale"),
                _ => false,
            };
        assert!(
            hit,
            "DEAD CONFIG SECTION: `[{section}]` can be written in scr1b3.toml but no \
             production code outside the config module reads it — it is a false promise",
        );
    }
}

// ---------------------------------------------------------------------------
// Falsification: the guard must be able to FAIL
// ---------------------------------------------------------------------------

/// The negative control for the whole file. If `consumed` ever degrades back to
/// something that answers `true` for everything — the exact defect this guard was
/// rewritten to remove — this is the test that goes red.
#[test]
fn the_guard_reports_a_control_nothing_reads() {
    let src = runtime_source();
    let sentinel = "appearance.no_such_control_that_nothing_reads";
    assert_eq!(
        dead_controls(&src, &[sentinel]),
        vec![sentinel],
        "the dead-control detector must report a field that no code reads; if it \
         reports nothing here it cannot report anything anywhere",
    );
}

/// A field named only in a COMMENT is not wired. This is the blind spot that made
/// the previous `src.contains(field)` guard unable to fail.
#[test]
fn a_comment_only_mention_is_not_a_consumer() {
    for comment in [
        "// honour cfg.editor.tab_width here\nfn f() {}\n",
        "/// Reads `editor.tab_width` one day.\nfn f() {}\n",
        "//! Module note about editor.tab_width.\nfn f() {}\n",
        "/* block: cfg.editor.tab_width */\nfn f() {}\n",
        "/* outer /* nested editor.tab_width */ still comment */\nfn f() {}\n",
    ] {
        assert!(
            !consumed(&scan(comment), "editor.tab_width"),
            "a mention inside a comment must not count as a consumer: {comment:?}",
        );
    }
    // Positive control: the same field, actually read.
    assert!(
        consumed(
            &scan("fn f(c: &Config) -> usize { c.editor.tab_width }\n"),
            "editor.tab_width"
        ),
        "a real field access must still count as a consumer",
    );
}

/// A field read only inside `#[cfg(test)]` code is not wired.
#[test]
fn a_test_only_mention_is_not_a_consumer() {
    let src = "\
fn prod() {}

#[cfg(test)]
mod tests {
    #[test]
    fn t(c: &Config) {
        assert_eq!(c.editor.tab_width, 4);
    }
}
";
    assert!(
        !consumed(&scan(src), "editor.tab_width"),
        "a read inside a #[cfg(test)] module must not count as a consumer",
    );
    // Positive control: the identical read outside the test gate.
    let real = src.replace("#[cfg(test)]\n", "");
    assert!(
        consumed(&scan(&real), "editor.tab_width"),
        "the identical read outside #[cfg(test)] must count",
    );
}

/// A whole FILE pulled in by `#[cfg(test)] mod NAME;` is not scanned at all.
#[test]
fn a_test_only_file_is_excluded_from_the_corpus() {
    let parent = strip_comments_and_literals(
        "#[cfg(test)]\nmod e2e;\n#[cfg(test)]\nmod qa_fixtures;\nmod real_module;\n",
    );
    let names = test_module_names(&parent);
    assert!(names.contains(&"e2e".to_string()));
    assert!(names.contains(&"qa_fixtures".to_string()));
    assert!(
        !names.contains(&"real_module".to_string()),
        "a production `mod` declaration must not be treated as test-only",
    );

    let mods: BTreeSet<String> = names.into_iter().collect();
    let none: [String; 0] = [];
    assert!(is_excluded(Path::new("src/app/e2e.rs"), &mods, &none));
    assert!(is_excluded(
        Path::new("src/app/qa_fixtures.rs"),
        &mods,
        &none
    ));
    assert!(!is_excluded(
        Path::new("src/app/real_module.rs"),
        &mods,
        &none
    ));
    // The settings UI is excluded for its own reason.
    assert!(is_excluded(
        Path::new("src/settings.rs"),
        &BTreeSet::new(),
        &none
    ));
}

/// The settings exclusion covers the whole MODULE, not just `settings.rs`.
///
/// The settings panel is being decomposed into per-page files under `settings/`.
/// Those pages render the same controls the old single file did, so they must be
/// excluded for the same reason — otherwise a `config.foo` read inside
/// `settings/appearance.rs` would count as a runtime consumer and a control that
/// nothing else reads would silently start looking wired. That is the quiet
/// failure this test exists to prevent: the guard would keep passing while it
/// had stopped guarding.
#[test]
fn every_file_in_the_settings_module_is_excluded_from_the_corpus() {
    let none: [String; 0] = [];
    let mods = BTreeSet::new();
    for page in [
        "src/settings.rs",
        "src/settings/chrome.rs",
        "src/settings/appearance.rs",
        "src/settings/editor.rs",
        // Nested a level deeper: still settings UI, still excluded.
        "src/settings/pages/appearance.rs",
    ] {
        assert!(
            is_excluded(Path::new(page), &mods, &none),
            "`{page}` is settings UI, so a config read inside it must not count \
             as a runtime consumer",
        );
    }
    // Windows path separators reach `is_excluded` from the real tree walk, which
    // builds absolute paths from CARGO_MANIFEST_DIR.
    assert!(is_excluded(
        Path::new(r"C:\repo\crates\scribe-app\src\settings\fonts.rs"),
        &mods,
        &none
    ));
}

/// Fail-proof for the test above: the exclusion is scoped to the settings
/// directory and does NOT swallow the rest of the tree.
///
/// Widening the key from a file name to a directory is only safe if it stays
/// anchored on that directory. A predicate that matched `settings` anywhere in
/// the path — or every file with a `settings` prefix — would drop real consumers
/// out of the corpus, and every dormant control would then look unwired at once.
#[test]
fn fail_proof_the_settings_exclusion_does_not_swallow_its_neighbours() {
    let none: [String; 0] = [];
    let mods = BTreeSet::new();
    for kept in [
        // A real consumer that merely NAMES settings.
        "src/app/settings_keys.rs",
        "src/app/settings_glue.rs",
        // A sibling directory one level up must be unaffected.
        "src/app/mod.rs",
        "src/config/mod.rs",
        // A directory that merely STARTS with `settings` is not the module.
        "src/settings_backup/appearance.rs",
    ] {
        assert!(
            !is_excluded(Path::new(kept), &mods, &none),
            "`{kept}` is not a settings-module page, so it must stay in the \
             corpus — dropping it would hide a real runtime consumer",
        );
    }
}

/// The real tree's test-only files really are absent from the corpus. This is the
/// end-to-end proof of the exclusion above, anchored on a symbol that exists only
/// inside a `#[cfg(test)] mod …;` file.
#[test]
fn the_real_corpus_excludes_the_real_test_files() {
    let raw = stripped_files(false)
        .into_iter()
        .map(|(_, s)| s)
        .collect::<Vec<_>>()
        .join("\n");
    let anchor = "scenario6_replace_all_at_scale_count_text_and_cache_invalidation";
    assert!(
        raw.contains(anchor),
        "fixture drift: the anchor symbol no longer exists in the tree; pick another \
         symbol that lives only in a `#[cfg(test)] mod …;` file",
    );
    assert!(
        !runtime_source().contains(anchor),
        "`{anchor}` lives only in a test-only file, so it must not appear in the \
         production corpus — the test-file exclusion has stopped working",
    );
}

/// A field named only inside a STRING literal is not wired.
#[test]
fn a_string_literal_mention_is_not_a_consumer() {
    for src in [
        "fn f() { log(\"cfg.editor.tab_width changed\"); }\n",
        "fn f() { log(r\"editor.tab_width\"); }\n",
        "fn f() { log(r#\"editor.tab_width\"#); }\n",
        "fn f() { log(b\"editor.tab_width\"); }\n",
    ] {
        assert!(
            !consumed(&scan(src), "editor.tab_width"),
            "a mention inside a string literal must not count as a consumer: {src:?}",
        );
    }
}

/// The probe is identifier-bounded, so a longer name never satisfies a shorter one.
#[test]
fn a_partial_identifier_match_is_not_a_consumer() {
    assert!(
        !consumed(
            &scan("fn f(c: &Config) -> usize { c.editor.tab_width_hint }\n"),
            "editor.tab_width"
        ),
        "`editor.tab_width_hint` must not satisfy the `editor.tab_width` probe",
    );
    assert!(
        !consumed(
            &scan("fn f(c: &Config) -> f32 { c.some_ui_scale_thing }\n"),
            "ui_scale"
        ),
        "`some_ui_scale_thing` must not satisfy the `ui_scale` probe",
    );
}

/// Lifetimes are code, not char literals — stripping must not eat the rest of a
/// line starting at `&'a`.
#[test]
fn lifetimes_survive_literal_stripping() {
    let scanned = scan("fn f<'a>(c: &'a Config) -> &'a str { &c.appearance.theme }\n");
    assert!(
        consumed(&scanned, "appearance.theme"),
        "a lifetime must not be mistaken for a char literal and swallow the code \
         after it; got: {scanned:?}",
    );
    // A real char literal still gets blanked.
    let with_char = strip_comments_and_literals("fn f() { let c = '.'; let s = 'x'; }\n");
    assert!(
        !with_char.contains('x'),
        "char literal must be blanked: {with_char:?}"
    );
}

/// `#[cfg(not(test))]` is PRODUCTION-only code and must survive the test strip;
/// `#[cfg(any(test, windows))]` is enabled in a real build and must survive too.
#[test]
fn non_test_cfg_forms_survive_the_test_strip() {
    let scanned = scan(
        "\
#[cfg(not(test))]
fn prod(c: &Config) -> usize { c.editor.tab_width }

#[cfg(any(test, windows))]
fn both(c: &Config) -> bool { c.window.always_on_top }
",
    );
    assert!(
        consumed(&scanned, "editor.tab_width"),
        "#[cfg(not(test))] is production code and must be scanned",
    );
    assert!(
        consumed(&scanned, "window.always_on_top"),
        "#[cfg(any(test, windows))] is enabled in a real build and must be scanned",
    );
}

/// The scanner only understands the exact `#[cfg(test)]` spelling. If a new
/// test-only spelling appears in the tree it would be scanned as production code
/// and could make a dead control look wired — so fail here and demand the scanner
/// be taught, rather than degrade silently.
#[test]
fn no_unhandled_test_only_cfg_form() {
    // Forms that are NOT test-only (they compile in a real build) or that the
    // scanner already handles exactly.
    let allowed = ["cfg(test)", "cfg(not(test))"];
    let mut offenders = Vec::new();
    for (path, src) in stripped_files(false) {
        for attr in cfg_attributes(&src) {
            if !contains_token(&attr, "test") {
                continue;
            }
            if allowed.contains(&attr.as_str()) || attr.starts_with("cfg(any(") {
                continue;
            }
            offenders.push(format!("{}: #[{attr}]", path.display()));
        }
    }
    assert!(
        offenders.is_empty(),
        "unhandled test-only cfg spelling(s) — teach `strip_cfg_test_items` about \
         them before they let dead controls look wired: {offenders:#?}",
    );
}
