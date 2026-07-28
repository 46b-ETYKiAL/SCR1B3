//! Tests for the Settings → Keyboard page.
//!
//! The unit tests below pin the pure decision tables (capture, issue
//! classification, button text, label parity). The tests under `wiring` are the
//! ones that matter most: they drive the REAL Settings UI through
//! `egui_kittest` — clicking the chord button and pressing keys the way a user
//! does — and then assert the LIVE dispatcher fires the new chord. Each of them
//! fails if any link in the chain is cut: the row's capture click, the event
//! read, the `Keybindings::set` write, the `keymap_src` re-resolve, or the
//! matcher itself.

use super::*;

fn key_event(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }
}

const CMD: egui::Modifiers = egui::Modifiers::COMMAND;
const ALT: egui::Modifiers = egui::Modifiers::ALT;
const SHIFT: egui::Modifiers = egui::Modifiers::SHIFT;

// ---- label table parity ----

#[test]
fn every_action_has_a_label_and_every_label_a_real_action() {
    // Bidirectional parity with the schema. A binding added to `Keybindings`
    // without a row here would be INVISIBLE in Settings — rebindable in theory,
    // unreachable in practice. That is exactly the silent gap this page exists
    // to close, so it fails here instead.
    let kb = Keybindings::default();
    let schema: Vec<&str> = kb.entries().iter().map(|(n, _)| *n).collect();
    let listed: Vec<&str> = GROUPS
        .iter()
        .flat_map(|(_, rows)| rows.iter().map(|(a, _)| *a))
        .collect();

    for action in &schema {
        assert!(
            listed.contains(action),
            "binding '{action}' has no row on the Keyboard page — a user could \
             never rebind it"
        );
    }
    for action in &listed {
        assert!(
            schema.contains(action),
            "the Keyboard page lists '{action}', which is not a binding in the schema"
        );
    }
    assert_eq!(
        listed.len(),
        schema.len(),
        "no action may be listed twice (that would render two rows editing one field)"
    );
    // `labels()` (what the settings search filters on) must cover them all.
    assert_eq!(labels().len(), schema.len());
    // Labels are human text, not the raw snake_case action names.
    for (action, label) in GROUPS.iter().flat_map(|(_, rows)| rows.iter()) {
        assert_ne!(
            label, action,
            "'{action}' must have a human label, not its field name"
        );
        assert!(!label.is_empty());
    }
}

#[test]
fn label_for_resolves_every_action_and_falls_back_for_an_unknown_one() {
    assert_eq!(label_for(action::SAVE), "Save");
    assert_eq!(label_for(action::MOVE_LINE_UP), "Move line up");
    // An action with no row falls back to its own name rather than panicking or
    // silently rendering an empty label.
    assert_eq!(label_for("not_a_binding"), "not_a_binding");
}

// ---- capture decision table ----

#[test]
fn capture_binds_the_chord_that_was_pressed() {
    assert_eq!(
        capture_from_events(&[key_event(egui::Key::K, CMD | ALT)]),
        Some(Captured::Bind("mod+alt+k".to_string()))
    );
    assert_eq!(
        capture_from_events(&[key_event(egui::Key::F11, egui::Modifiers::NONE)]),
        Some(Captured::Bind("f11".to_string()))
    );
    assert_eq!(
        capture_from_events(&[key_event(egui::Key::ArrowUp, SHIFT)]),
        Some(Captured::Bind("shift+arrowup".to_string()))
    );
}

#[test]
fn capture_treats_escape_as_cancel_not_as_a_binding() {
    // Escape must never become a shortcut here: it is the universal "close the
    // overlay" key, and binding it would make an action fire on every dismiss.
    assert_eq!(
        capture_from_events(&[key_event(egui::Key::Escape, egui::Modifiers::NONE)]),
        Some(Captured::Cancel)
    );
    // Even with modifiers held — Ctrl+Esc is still a cancel, not `mod+escape`.
    assert_eq!(
        capture_from_events(&[key_event(egui::Key::Escape, CMD)]),
        Some(Captured::Cancel)
    );
}

#[test]
fn capture_keeps_waiting_when_no_key_was_pressed() {
    // A frame with no key press (the user is still reaching for the chord, or
    // only moved the mouse) must NOT resolve — otherwise the capture would
    // resolve to nothing and silently unbind the action.
    assert_eq!(capture_from_events(&[]), None);
    assert_eq!(
        capture_from_events(&[egui::Event::PointerMoved(egui::pos2(1.0, 2.0))]),
        None
    );
    // A key RELEASE is not a press.
    assert_eq!(
        capture_from_events(&[egui::Event::Key {
            key: egui::Key::S,
            physical_key: None,
            pressed: false,
            repeat: false,
            modifiers: CMD,
        }]),
        None
    );
}

#[test]
fn capture_takes_the_first_key_of_a_multi_key_frame() {
    // Two keys in one frame is a fast typist, not a two-key chord (egui has no
    // multi-key chord layer). Bind the first and stop, rather than letting the
    // last one silently win.
    let decision = capture_from_events(&[
        key_event(egui::Key::A, CMD),
        key_event(egui::Key::B, egui::Modifiers::NONE),
    ]);
    assert_eq!(decision, Some(Captured::Bind("mod+a".to_string())));
}

// ---- issue classification ----

#[test]
fn a_clean_keymap_reports_no_issues() {
    assert!(
        issues_by_action(&Keybindings::default()).is_empty(),
        "the shipped defaults must render with no warnings"
    );
}

#[test]
fn a_conflict_names_the_other_actions_sharing_the_chord() {
    // The headline feature: two actions on one chord, surfaced on BOTH rows,
    // each naming the OTHER (never itself).
    let mut kb = Keybindings::default();
    kb.set("save", "mod+k");
    kb.set("find", "ctrl+k"); // the SAME chord, written with an alias
    let issues = issues_by_action(&kb);

    let save = issues
        .iter()
        .find(|(a, _)| *a == "save")
        .map(|(_, i)| i)
        .expect("the save row must report the conflict");
    assert_eq!(save, &RowIssue::Conflict(vec!["find"]));
    let find = issues
        .iter()
        .find(|(a, _)| *a == "find")
        .map(|(_, i)| i)
        .expect("the find row must report it too");
    assert_eq!(find, &RowIssue::Conflict(vec!["save"]));
    // The message a user reads names the OTHER action by its human label.
    assert!(
        save.message().contains("Find in this file"),
        "the conflict message must name the colliding action: {}",
        save.message()
    );
    assert!(!save.message().contains("Save: "), "and not itself");
}

#[test]
fn an_unbound_unreadable_or_unknown_key_row_is_classified_distinctly() {
    // The three ways a binding is dead. Each gets its own message, because the
    // fix differs: bind something / fix the syntax / pick a real key.
    let mut kb = Keybindings::default();
    kb.set("save", "");
    kb.set("find", "a+b");
    kb.set("replace", "mod+nosuchkey");
    let issues = issues_by_action(&kb);
    let of = |a: &str| {
        issues
            .iter()
            .find(|(n, _)| *n == a)
            .map(|(_, i)| i.clone())
            .unwrap_or_else(|| panic!("'{a}' must be flagged"))
    };
    assert_eq!(of("save"), RowIssue::Unbound);
    assert_eq!(of("find"), RowIssue::Unreadable);
    assert_eq!(
        of("replace"),
        RowIssue::UnknownKey,
        "a combo that PARSES but names no real key is the case `validate` \
         cannot see — the page must catch it"
    );
    // Every message says the action will not fire, in plain language.
    for issue in [
        RowIssue::Unbound,
        RowIssue::Unreadable,
        RowIssue::UnknownKey,
    ] {
        let m = issue.message();
        assert!(
            m.contains("cannot be triggered") || m.contains("never fire"),
            "an unusable binding must say so: {m}"
        );
    }
}

#[test]
fn the_banner_states_a_conflict_once_not_once_per_participant() {
    // A conflict produces an issue on every participating row; the banner needs
    // it said once, or a 3-way collision shouts three times.
    let mut kb = Keybindings::default();
    kb.set("save", "mod+k");
    kb.set("find", "mod+k");
    let messages = dedup_messages(&issues_by_action(&kb));
    assert_eq!(
        messages.len(),
        1,
        "one conflict, one banner line: {messages:?}"
    );
    assert!(messages[0].contains("Save"));
}

// ---- what the row's button reads ----

#[test]
fn the_chord_button_reads_the_binding_and_never_lies_about_a_dead_one() {
    // A bound row shows the platform chord.
    let expected = display_combo("mod+s").expect("mod+s resolves");
    assert_eq!(chord_button_text("mod+s", false), expected);
    // Capturing overrides everything with the prompt.
    assert_eq!(chord_button_text("mod+s", true), "press keys…");
    // Blank says "unbound" — not an empty button the user cannot click.
    assert_eq!(chord_button_text("", false), "unbound");
    assert_eq!(chord_button_text("   ", false), "unbound");
    // A combo that cannot fire is shown VERBATIM plus the reason, so the user
    // can see what is in their config file and why it does nothing. Rendering a
    // plausible chord here would be the lie this whole page exists to prevent.
    assert_eq!(
        chord_button_text("mod+nosuchkey", false),
        "mod+nosuchkey (won't fire)"
    );
    assert_eq!(chord_button_text("a+b", false), "a+b (won't fire)");
}

// ---- the shared label-column width ----

/// Re-measure the widest visible label independently of the function under
/// test, so the assertions below compare against a real measurement rather
/// than a constant baked into the test.
fn widest_label_px(ui: &egui::Ui, q: &str) -> f32 {
    let font = egui::TextStyle::Body.resolve(ui.style());
    labels()
        .into_iter()
        .filter(|label| crate::settings::row_visible(q, label))
        .map(|label| {
            ui.painter()
                .layout_no_wrap(label.to_string(), font.clone(), egui::Color32::WHITE)
                .size()
                .x
        })
        .fold(0.0_f32, f32::max)
}

/// What one measuring frame observed.
#[derive(Default)]
struct Measured {
    /// Widest visible label for the unfiltered page.
    widest_all: f32,
    /// What the function under test returned for the unfiltered page.
    got_all: f32,
    /// Widest visible label when the search narrows the page to one short row.
    widest_narrow: f32,
    /// What the function under test returned for that narrow page.
    got_narrow: f32,
}

/// Measure inside a REAL frame.
///
/// `egui::__run_test_ui` cannot be used here: it calls
/// `set_fonts(FontDefinitions::empty())` to save CPU, so every
/// `layout_no_wrap` comes back **zero-wide** and every assertion about a text
/// width silently compares 0 against 0 — a fixture modelling the shape the bug
/// is not in. `egui_kittest`'s harness keeps the default fonts, so the widths
/// here are real. The preconditions below fail loudly if that ever regresses.
fn measure() -> Measured {
    let mut h = egui_kittest::Harness::new_ui_state(
        |ui, m: &mut Measured| {
            // A 40 px body pushes every label far past the 150 px floor. At the
            // default size the `.max(FLOOR)` clamp swallows a wrong gutter
            // (150 wins either way) — the clamp-masks-the-arithmetic trap.
            ui.style_mut()
                .text_styles
                .insert(egui::TextStyle::Body, egui::FontId::proportional(40.0));
            m.widest_all = widest_label_px(ui, "");
            m.got_all = label_column_width(ui, "");
            m.widest_narrow = widest_label_px(ui, "save");
            m.got_narrow = label_column_width(ui, "save");
        },
        Measured::default(),
    );
    h.run();
    std::mem::take(h.state_mut())
}

/// The gutter is ADDED to the widest visible label, and it is small.
///
/// The whole point of the shared column is that every group's grid starts at
/// the same x. A width that ignores the labels (a constant), that SUBTRACTS the
/// gutter, or that scales by it puts the chord buttons back where they were —
/// misaligned.
#[test]
fn the_label_column_is_the_widest_visible_label_plus_a_small_gutter() {
    let m = measure();
    assert!(
        m.widest_all > 200.0,
        "precondition: real fonts, and the enlarged body puts the widest label \
         well past the 150 px floor so the clamp cannot mask the gutter — got {}",
        m.widest_all
    );

    let gutter = m.got_all - m.widest_all;
    assert!(
        gutter > 0.0 && gutter <= 24.0,
        "the column must be the widest label ({}) plus a SMALL gutter, got {} \
         (delta {gutter})",
        m.widest_all,
        m.got_all
    );
}

/// The floor exists so a one-row search result cannot collapse the page into a
/// cramped strip — and it is a FLOOR, not the answer: the unfiltered page is
/// wider than it.
#[test]
fn a_narrow_search_result_is_floored_but_the_full_page_is_wider() {
    let m = measure();
    assert!(
        m.widest_narrow > 0.0 && m.widest_narrow < 138.0,
        "precondition: `Save` alone is narrower than the floor minus the gutter \
         — got {}",
        m.widest_narrow
    );
    assert_eq!(
        m.got_narrow, 150.0,
        "a narrow filter is floored, not collapsed"
    );
    assert!(
        m.got_all > 150.0,
        "the floor is not simply the answer for every query — the unfiltered \
         page measures {} ",
        m.got_all
    );
}

// ---- the "something changed" signal the host persists on ----

/// A row button's change must be REPORTED UP, not merely applied.
///
/// `show` returns `true` so the host calls `save_config`; the wiring tests
/// below all assert the live *dispatcher* follows a rebind, which reads
/// `config.keybindings` directly and is therefore true whether or not the
/// return value survived the walk back up. That leaves the per-row aggregation
/// (`changed |= binding_row(..)`) unasserted — flip it to `&=` and every row
/// edit becomes a change the host never writes to disk, with every existing
/// test still green. This drives the real page and reads the return value.
#[test]
fn a_row_button_change_is_reported_up_so_the_host_persists_it() {
    use egui_kittest::kittest::{NodeT as _, Queryable as _};

    struct Page {
        config: Config,
        /// ACCUMULATED across frames: `Harness::run` settles by running several
        /// frames, and the click lands in an earlier one than the last, so a
        /// plain assignment would be overwritten by the quiet final frame.
        reported: bool,
    }

    let mut config = Config::default();
    // One row off its default, so exactly one ↺ / one ✕ is enabled.
    config.keybindings.save = "mod+alt+k".into();

    let mut h = egui_kittest::Harness::new_ui_state(
        |ui, page: &mut Page| {
            page.reported |= show(ui, &mut page.config, "");
        },
        Page {
            config,
            reported: false,
        },
    );
    h.run();
    assert!(
        !h.state().reported,
        "a frame with no interaction reports no change — otherwise the host \
         would rewrite the config file every frame"
    );

    // Click the one ENABLED restore button (the Save row's).
    let mut clicked = false;
    for node in h.get_all_by_label("↺") {
        if !node.accesskit_node().is_disabled() {
            node.click();
            clicked = true;
            break;
        }
    }
    assert!(clicked, "precondition: the rebound row offers an enabled ↺");
    h.run();

    assert_eq!(
        h.state().config.keybindings.save,
        "mod+s",
        "the row restored the shipped chord"
    );
    assert!(
        h.state().reported,
        "…and `show` must REPORT it, or the host never persists the change"
    );
}

/// The same signal for the ✕ (unbind) button, which is the other half of the
/// per-row aggregation.
#[test]
fn unbinding_a_row_is_also_reported_up() {
    use egui_kittest::kittest::{NodeT as _, Queryable as _};

    struct Page {
        config: Config,
        /// Accumulated — see the sibling test.
        reported: bool,
    }

    let mut h = egui_kittest::Harness::new_ui_state(
        |ui, page: &mut Page| {
            page.reported |= show(ui, &mut page.config, "save");
        },
        Page {
            config: Config::default(),
            reported: false,
        },
    );
    h.run();
    assert!(!h.state().reported, "no interaction, no change");

    let mut clicked = false;
    for node in h.get_all_by_label("✕") {
        if !node.accesskit_node().is_disabled() {
            node.click();
            clicked = true;
            break;
        }
    }
    assert!(clicked, "precondition: a bound row offers an enabled ✕");
    h.run();

    assert_eq!(
        h.state().config.keybindings.save,
        "",
        "the row unbound the chord"
    );
    assert!(
        h.state().reported,
        "…and `show` must REPORT it, or the unbind is lost on restart"
    );
}

/// A row's warning must be ITS OWN.
///
/// `issues.iter().find(|(a, _)| *a == act)` is the only thing tying a row to
/// its problem, and nothing asserted it: the classification tests all check
/// `issues_by_action` (which row is broken) rather than what a ROW renders.
/// Flip that `==` to `!=` and the one broken row loses its warning while every
/// healthy row grows one — 34 false alarms — with the whole suite still green.
#[test]
fn only_the_broken_row_carries_a_warning_icon() {
    use egui_kittest::kittest::Queryable as _;

    struct Page {
        config: Config,
    }

    let mut config = Config::default();
    // Exactly one problem on the page: Save is unbound.
    config.keybindings.save = String::new();

    let mut h = egui_kittest::Harness::new_ui_state(
        |ui, page: &mut Page| {
            show(ui, &mut page.config, "");
        },
        Page { config },
    );
    h.run();

    let warned = h.get_all_by_label("⚠").count();
    assert_eq!(
        warned, 1,
        "exactly the ONE unbound row may carry a ⚠ — every other row is healthy"
    );
}

// ---- the wire: UI -> config -> live dispatcher ----

mod wiring {
    use super::super::*;
    use crate::app::e2e::Driver;
    use crate::app::keymap::action;
    use crate::app::ScribeApp;
    use egui_kittest::kittest::{NodeT as _, Queryable as _};
    use scribe_core::Config;

    const CMD: egui::Modifiers = egui::Modifiers::COMMAND;
    const ALT: egui::Modifiers = egui::Modifiers::ALT;

    /// The real app, Settings open on the Keyboard page, driven by kittest.
    fn harness() -> egui_kittest::Harness<'static, ScribeApp> {
        let mut cfg = Config::default();
        cfg.appearance.frameless = false;
        cfg.editor.first_run_completed = true;
        let app = ScribeApp::new_test(cfg);
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::Vec2::new(1280.0, 940.0))
            .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app);
        h.state_mut().settings_open = true;
        h.run();
        h.get_by_label("Keyboard").click();
        h.run();
        h
    }

    /// THE discriminating test. A user clicks Save's chord button in Settings,
    /// presses Ctrl+Alt+K, and the editor's dispatcher must fire Save on
    /// Ctrl+Alt+K — with no restart and no "apply" step.
    ///
    /// Cut ANY link and this fails: the button's capture click, the event read
    /// in `show`, the `Keybindings::set` write, the `keymap_src != keybindings`
    /// re-resolve in `handle_keyboard_shortcuts`, or the matcher.
    #[test]
    fn rebinding_through_the_settings_ui_makes_the_dispatcher_fire_the_new_chord() {
        let mut h = harness();
        let default_chord = crate::app::keymap::display_combo("mod+s").expect("mod+s resolves");
        assert_eq!(
            h.state().config.keybindings.save,
            "mod+s",
            "precondition: save ships on mod+s"
        );

        // Click Save's chord button — the row for "Save" is the only one showing
        // the default save chord.
        h.get_by_label(default_chord.as_str()).click();
        h.run();

        // Press the new chord, exactly as a user would.
        h.key_press_modifiers(CMD | ALT, egui::Key::K);
        h.run();

        assert_eq!(
            h.state().config.keybindings.save,
            "mod+alt+k",
            "the captured chord must be written to the live config"
        );

        // …and the LIVE dispatcher must now resolve it. This is the half a
        // keymap-struct-only test cannot see.
        let d = Driver::new();
        let app = h.state_mut();
        let (act, _) = d.shortcuts(app, egui::Key::K, CMD | ALT);
        assert!(
            act.save,
            "the rebound chord must fire Save in the real dispatcher — if this \
             fails, the Settings page is not wired to the input layer"
        );
        let (act, _) = d.shortcuts(app, egui::Key::S, CMD);
        assert!(
            !act.save,
            "and the replaced default must stop firing, or the user now has two \
             save chords instead of the one they chose"
        );
    }

    /// The rebind must survive into what the host persists. `save_config`
    /// serializes `self.config`, so asserting the config round-trips through
    /// `Config::save_to`/`load_from` proves the new chord is what lands on disk —
    /// no separate persistence path was invented for keybindings.
    #[test]
    fn a_rebind_is_written_through_the_existing_config_persistence() {
        let mut h = harness();
        let default_chord = crate::app::keymap::display_combo("mod+s").expect("mod+s resolves");
        h.get_by_label(default_chord.as_str()).click();
        h.run();
        h.key_press_modifiers(CMD | ALT, egui::Key::K);
        h.run();

        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("scr1b3.toml");
        h.state().config.save_to(&path).expect("config saves");
        let toml = std::fs::read_to_string(&path).expect("config file readable");
        let reloaded = Config::from_toml_str(&toml).expect("config loads");
        assert_eq!(
            reloaded.keybindings.save, "mod+alt+k",
            "the rebind must persist through the SAME config file every other \
             setting uses"
        );
        // And the reloaded config drives the same dispatcher answer.
        let mut app = ScribeApp::new_test(reloaded);
        let d = Driver::new();
        d.idle(&mut app);
        let (act, _) = d.shortcuts(&mut app, egui::Key::K, CMD | ALT);
        assert!(act.save, "a restarted editor must honour the saved rebind");
    }

    /// Rebinding Save must not ALSO save the file. The capture frame is exactly
    /// the frame where the editor's shortcut layer has to stand down.
    #[test]
    fn capturing_a_chord_does_not_fire_the_editor_action_for_those_keys() {
        let mut h = harness();
        let default_chord = crate::app::keymap::display_combo("mod+s").expect("mod+s resolves");
        h.get_by_label(default_chord.as_str()).click();
        h.run();
        assert!(
            crate::app::settings_keys::capture_active(&h.ctx),
            "clicking the chord button must start a capture"
        );

        // Ctrl+N is New file. During a capture it must rebind, not open a tab.
        let tabs_before = h.state().tabs.len();
        h.key_press_modifiers(CMD, egui::Key::N);
        h.run();

        assert_eq!(
            h.state().config.keybindings.save,
            "mod+n",
            "the keypress must land as a REBIND"
        );
        assert_eq!(
            h.state().tabs.len(),
            tabs_before,
            "and must NOT also run New file — the rebind UI cannot fire the \
             actions it is rebinding"
        );
    }

    /// Escape during a capture cancels and changes nothing.
    #[test]
    fn escape_cancels_a_capture_and_leaves_the_binding_alone() {
        let mut h = harness();
        let default_chord = crate::app::keymap::display_combo("mod+s").expect("mod+s resolves");
        h.get_by_label(default_chord.as_str()).click();
        h.run();
        h.key_press(egui::Key::Escape);
        h.run();

        assert_eq!(
            h.state().config.keybindings.save,
            "mod+s",
            "Escape must abandon the capture, not bind Escape and not unbind"
        );
        assert!(
            !crate::app::settings_keys::capture_active(&h.ctx),
            "and the capture must end, or the shortcut layer stays suppressed"
        );
    }

    /// Closing Settings mid-capture must not leave the editor's shortcuts dead.
    /// Without the `clear_capture` call this fails: Ctrl+N would never open a
    /// tab again for the rest of the session.
    #[test]
    fn closing_settings_mid_capture_restores_the_editors_shortcuts() {
        let mut h = harness();
        let default_chord = crate::app::keymap::display_combo("mod+s").expect("mod+s resolves");
        h.get_by_label(default_chord.as_str()).click();
        h.run();
        assert!(crate::app::settings_keys::capture_active(&h.ctx));

        h.state_mut().settings_open = false;
        h.run();

        assert!(
            !crate::app::settings_keys::capture_active(&h.ctx),
            "closing Settings must abandon the capture"
        );
        let tabs_before = h.state().tabs.len();
        h.key_press_modifiers(CMD, egui::Key::N);
        h.run();
        assert!(
            h.state().tabs.len() > tabs_before,
            "Ctrl+N must work again once Settings is closed"
        );
    }

    /// Per-row restore: the ↺ button puts one binding back and the dispatcher
    /// follows it back.
    #[test]
    fn restoring_one_binding_returns_that_chord_to_the_dispatcher() {
        let mut h = harness();
        h.state_mut().config.keybindings.save = "mod+alt+k".into();
        h.run();
        {
            let app = h.state_mut();
            let d = Driver::new();
            let (act, _) = d.shortcuts(app, egui::Key::S, CMD);
            assert!(!act.save, "precondition: Ctrl+S is no longer save");
        }

        // The Save row is the only one whose chord differs from its default, so
        // its ↺ is the only ENABLED restore button on the page.
        let rebound = crate::app::keymap::display_combo("mod+alt+k").expect("resolves");
        h.get_by_label(rebound.as_str());
        for node in h.get_all_by_label("↺") {
            if !node.accesskit_node().is_disabled() {
                node.click();
                break;
            }
        }
        h.run();

        assert_eq!(
            h.state().config.keybindings.save,
            "mod+s",
            "↺ must restore the shipped chord"
        );
        let app = h.state_mut();
        let d = Driver::new();
        let (act, _) = d.shortcuts(app, egui::Key::S, CMD);
        assert!(act.save, "and the dispatcher must follow it back");
    }

    /// Restore-all puts every binding back and the dispatcher follows.
    #[test]
    fn restore_all_defaults_returns_every_chord_to_the_dispatcher() {
        let mut h = harness();
        h.state_mut().config.keybindings.save = "mod+alt+k".into();
        h.state_mut().config.keybindings.new_file = "mod+alt+j".into();
        h.run();

        h.get_by_label("Restore all defaults").click();
        h.run();

        assert_eq!(
            h.state().config.keybindings,
            scribe_core::config::Keybindings::default()
        );
        let app = h.state_mut();
        let d = Driver::new();
        let (act, _) = d.shortcuts(app, egui::Key::S, CMD);
        assert!(act.save, "Ctrl+S must save again");
        let (act, _) = d.shortcuts(app, egui::Key::N, CMD);
        assert!(act.new, "Ctrl+N must open a new file again");
    }

    /// Unbinding through the ✕ must actually stop the action firing — not just
    /// blank a label while the old chord keeps working.
    #[test]
    fn unbinding_a_row_stops_the_dispatcher_firing_that_action() {
        let mut h = harness();
        {
            let app = h.state_mut();
            let d = Driver::new();
            let (act, _) = d.shortcuts(app, egui::Key::S, CMD);
            assert!(act.save, "precondition: Ctrl+S saves");
        }
        h.state_mut().config.keybindings.set(action::SAVE, "");
        h.run();

        assert!(
            h.state().config.keybindings.save.is_empty(),
            "the binding is cleared"
        );
        let app = h.state_mut();
        let d = Driver::new();
        let (act, _) = d.shortcuts(app, egui::Key::S, CMD);
        assert!(
            !act.save,
            "an unbound action must stop firing on its old chord"
        );
    }

    /// A stale capture flag must NOT suppress shortcuts while Settings is
    /// closed — the guard is `settings_open && capture_active`, and dropping the
    /// `settings_open` half would let one abandoned capture kill every shortcut.
    #[test]
    fn a_capture_flag_with_settings_closed_does_not_suppress_shortcuts() {
        let mut cfg = Config::default();
        cfg.editor.first_run_completed = true;
        let mut app = ScribeApp::new_test(cfg);
        let d = Driver::new();
        d.idle(&mut app);

        // Plant a capture flag directly, then leave Settings closed.
        let ctx = egui::Context::default();
        ctx.data_mut(|dd| {
            dd.insert_temp(egui::Id::new("scr1b3_keybind_capture"), "save".to_string())
        });
        assert!(!app.settings_open);

        let (act, _) = d.shortcuts(&mut app, egui::Key::S, CMD);
        assert!(
            act.save,
            "with Settings closed the editor's shortcuts must work regardless \
             of any leftover capture flag"
        );
    }
}
