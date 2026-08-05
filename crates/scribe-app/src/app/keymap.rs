//! Resolve the user's `[keybindings]` config into egui chords the input layer
//! matches against.
//!
//! `scribe-core` owns the combo GRAMMAR ([`Chord::parse`]) and stays free of any
//! UI dependency, so it hands back a canonical key TOKEN (`"n"`, `"f11"`,
//! `"arrowup"`). This module is the other half: it binds that token to an
//! [`egui::Key`] and answers "did the user press the chord bound to <action>
//! this frame?".
//!
//! Modifier matching is EXACT — a chord bound to `mod+o` does not fire when Shift
//! is also held. That is what keeps `mod+o` (open file) and `mod+shift+o` (go to
//! symbol) distinct without the hand-written `!i.modifiers.shift` guards the
//! hard-wired handler used to need.

use super::*;
use scribe_core::config::{Chord, Keybindings};

/// Action names, matching [`Keybindings::entries`] exactly.
///
/// The input layer refers to actions through these consts rather than bare string
/// literals so a typo is a compile error, not an action that silently never
/// fires. `action_names_match_the_config_schema` pins the two lists together in
/// BOTH directions, so a binding added to the config schema without a const here
/// (or vice versa) fails the suite.
pub(super) mod action {
    pub const NEW_FILE: &str = "new_file";
    pub const OPEN_FILE: &str = "open_file";
    pub const SAVE: &str = "save";
    pub const SAVE_AS: &str = "save_as";
    pub const FIND: &str = "find";
    pub const FIND_IN_FILES: &str = "find_in_files";
    pub const REPLACE: &str = "replace";
    pub const COMMAND_PALETTE: &str = "command_palette";
    pub const FUZZY_FINDER: &str = "fuzzy_finder";
    pub const GOTO_LINE: &str = "goto_line";
    pub const GOTO_SYMBOL: &str = "goto_symbol";
    pub const RECENT_FILES: &str = "recent_files";
    pub const CLOSE_TAB: &str = "close_tab";
    pub const NEXT_TAB: &str = "next_tab";
    pub const PREV_TAB: &str = "prev_tab";
    /// By-index tab activation, 1..=9. Indexed by `n - 1` through
    /// [`GOTO_TAB`], which is what lets the input layer wire nine chords in one
    /// loop instead of nine copy-pasted blocks.
    pub const GOTO_TAB_1: &str = "goto_tab_1";
    pub const GOTO_TAB_2: &str = "goto_tab_2";
    pub const GOTO_TAB_3: &str = "goto_tab_3";
    pub const GOTO_TAB_4: &str = "goto_tab_4";
    pub const GOTO_TAB_5: &str = "goto_tab_5";
    pub const GOTO_TAB_6: &str = "goto_tab_6";
    pub const GOTO_TAB_7: &str = "goto_tab_7";
    pub const GOTO_TAB_8: &str = "goto_tab_8";
    pub const GOTO_TAB_9: &str = "goto_tab_9";

    /// The nine by-index tab actions in index order: `GOTO_TAB[i]` activates
    /// the tab at 0-based index `i`.
    ///
    /// NOT test-only (unlike [`ALL`]): the input layer iterates it, so a tenth
    /// action added to the schema without a slot here simply never fires —
    /// which `action_names_match_the_config_schema` catches via [`ALL`]. That
    /// the ORDER is the index order (and not, say, transposed) is pinned by
    /// `each_goto_tab_action_answers_to_its_own_number_key`.
    pub const GOTO_TAB: &[&str] = &[
        GOTO_TAB_1, GOTO_TAB_2, GOTO_TAB_3, GOTO_TAB_4, GOTO_TAB_5, GOTO_TAB_6, GOTO_TAB_7,
        GOTO_TAB_8, GOTO_TAB_9,
    ];
    pub const REOPEN_TAB: &str = "reopen_tab";
    pub const TOGGLE_GRID: &str = "toggle_grid";
    pub const TOGGLE_COMMENT: &str = "toggle_comment";
    pub const JUMP_BRACKET: &str = "jump_bracket";
    pub const TOGGLE_FULLSCREEN: &str = "toggle_fullscreen";
    pub const TOGGLE_ZEN: &str = "toggle_zen";
    pub const CYCLE_THEME: &str = "cycle_theme";
    pub const TOGGLE_MINIMAP: &str = "toggle_minimap";
    pub const TOGGLE_MD_PREVIEW: &str = "toggle_md_preview";
    pub const FOLD_ALL: &str = "fold_all";
    pub const EXPAND_ALL: &str = "expand_all";
    pub const INCREASE_FONT: &str = "increase_font";
    pub const DECREASE_FONT: &str = "decrease_font";
    pub const RESET_FONT: &str = "reset_font";
    pub const MOVE_LINE_UP: &str = "move_line_up";
    pub const MOVE_LINE_DOWN: &str = "move_line_down";
    pub const DUPLICATE_LINE: &str = "duplicate_line";
    pub const JOIN_LINES: &str = "join_lines";
    pub const TOGGLE_BOOKMARK: &str = "toggle_bookmark";
    pub const NEXT_BOOKMARK: &str = "next_bookmark";
    pub const PREV_BOOKMARK: &str = "prev_bookmark";

    /// Every action const, for the schema-parity test. Test-only: the runtime
    /// refers to each action by name, never by iterating this list.
    #[cfg(test)]
    pub const ALL: &[&str] = &[
        NEW_FILE,
        OPEN_FILE,
        SAVE,
        SAVE_AS,
        FIND,
        FIND_IN_FILES,
        REPLACE,
        COMMAND_PALETTE,
        FUZZY_FINDER,
        GOTO_LINE,
        GOTO_SYMBOL,
        RECENT_FILES,
        CLOSE_TAB,
        NEXT_TAB,
        PREV_TAB,
        GOTO_TAB_1,
        GOTO_TAB_2,
        GOTO_TAB_3,
        GOTO_TAB_4,
        GOTO_TAB_5,
        GOTO_TAB_6,
        GOTO_TAB_7,
        GOTO_TAB_8,
        GOTO_TAB_9,
        REOPEN_TAB,
        TOGGLE_GRID,
        TOGGLE_COMMENT,
        JUMP_BRACKET,
        TOGGLE_FULLSCREEN,
        TOGGLE_ZEN,
        CYCLE_THEME,
        TOGGLE_MINIMAP,
        TOGGLE_MD_PREVIEW,
        FOLD_ALL,
        EXPAND_ALL,
        INCREASE_FONT,
        DECREASE_FONT,
        RESET_FONT,
        MOVE_LINE_UP,
        MOVE_LINE_DOWN,
        DUPLICATE_LINE,
        JOIN_LINES,
        TOGGLE_BOOKMARK,
        NEXT_BOOKMARK,
        PREV_BOOKMARK,
    ];
}

/// Map a canonical key token from [`Chord::parse`] onto an [`egui::Key`].
///
/// Resolved against egui's OWN key table rather than a hand-copied match, so the
/// accepted spellings track the egui version in the lockfile instead of drifting
/// from it. Two spellings are accepted per key:
/// - [`egui::Key::name`] — the display name (`"Backslash"`, `"Up"`, `"0"`, `"["`).
/// - the variant name via `Debug` (`"ArrowUp"`, `"Num0"`, `"OpenBracket"`), which
///   is the spelling [`egui::Key::from_name`] documents and our defaults use.
///
/// Both are compared case-insensitively, which is what lets the lowercase tokens
/// the config grammar produces (`"arrowup"`) resolve. `every_default_binding_
/// resolves_to_the_expected_key` pins EVERY shipped default, so a future egui
/// rename fails the suite rather than silently killing a shortcut.
fn key_from_token(token: &str) -> Option<egui::Key> {
    egui::Key::ALL.iter().copied().find(|k| {
        k.name().eq_ignore_ascii_case(token) || format!("{k:?}").eq_ignore_ascii_case(token)
    })
}

/// How a key reads on a keyboard, for display in the cheatsheet / palette.
///
/// [`egui::Key::name`] is already right for letters (`"N"`), digits (`"0"`),
/// arrows (`"Up"`), `"Tab"` and `"F11"`; only punctuation needs overriding,
/// because a user looks for `Ctrl+\`, not `Ctrl+Backslash`.
fn key_display(key: egui::Key) -> &'static str {
    match key {
        egui::Key::Backslash => "\\",
        egui::Key::Slash => "/",
        egui::Key::OpenBracket => "[",
        egui::Key::CloseBracket => "]",
        egui::Key::Period => ".",
        egui::Key::Comma => ",",
        egui::Key::Equals => "=",
        egui::Key::Minus => "-",
        egui::Key::Plus => "+",
        egui::Key::Semicolon => ";",
        egui::Key::Quote => "'",
        egui::Key::Backtick => "`",
        egui::Key::Colon => ":",
        egui::Key::Pipe => "|",
        egui::Key::Questionmark => "?",
        egui::Key::Exclamationmark => "!",
        egui::Key::OpenCurlyBracket => "{",
        egui::Key::CloseCurlyBracket => "}",
        other => other.name(),
    }
}

/// Localize a HARD-CODED chord string (the `chord` / `shortcut` fallback text of
/// a non-rebindable shortcut) for the current platform.
///
/// Those strings are written `"Ctrl+C"`, but the handlers behind them key off
/// `egui::Modifiers::COMMAND` (e.g. Undo is `Key::Z + COMMAND`) and `ALT` — which
/// are Cmd and Option on macOS. So the literal text was already wrong for every
/// macOS user, before any of this was rebindable. Rebindable rows render through
/// [`ResolvedChord::display`], which is already platform-correct; without the same
/// treatment here the cheatsheet would read half "Cmd+E", half "Ctrl+C" on macOS.
///
/// A no-op everywhere else.
pub(super) fn platform_chord_text(text: &str) -> String {
    if cfg!(target_os = "macos") {
        text.replace("Ctrl+", "Cmd+").replace("Alt+", "Option+")
    } else {
        text.to_string()
    }
}

/// The config combo string for a physical press of `key` with `mods` held.
///
/// The INVERSE of the token -> [`egui::Key`] step [`Keymap::resolve`] performs,
/// and the whole reason the settings keyboard page can capture a chord by
/// listening rather than making the user type `"mod+shift+openbracket"` by hand.
///
/// The key is spelled with the variant-name form (`"arrowup"`, `"num0"`,
/// `"openbracket"`) — the spelling [`key_from_token`] accepts and the shipped
/// defaults use — and the modifiers are emitted in [`Chord::canonical`] order, so
/// a captured combo is byte-identical to how the same chord would be written by
/// hand and collides with an alias-written twin the way `Keybindings::validate`
/// expects. `a_captured_chord_round_trips_back_to_the_key_that_was_pressed` pins
/// the round-trip over egui's ENTIRE key table.
///
/// `mods.command` (not `ctrl`) is read because that is the flag
/// [`Keymap::pressed`] matches against — capturing Cmd on macOS and Ctrl
/// elsewhere, exactly like `mod`.
pub(super) fn combo_from_press(key: egui::Key, mods: egui::Modifiers) -> String {
    let mut out = String::new();
    if mods.command {
        out.push_str("mod+");
    }
    if mods.alt {
        out.push_str("alt+");
    }
    if mods.shift {
        out.push_str("shift+");
    }
    out.push_str(&format!("{key:?}").to_ascii_lowercase());
    out
}

/// How the stored combo string `combo` reads on this platform, e.g.
/// `"Ctrl+Shift+F"`.
///
/// `None` exactly when the combo cannot fire — blank, unparseable, or naming a
/// key that is not on the keyboard. The settings page renders that as an explicit
/// "won't fire" warning instead of a plausible-looking chord, which is the whole
/// point: a rebinding UI that pretty-prints a dead binding is worse than none.
///
/// Shares [`Chord::parse`] + [`key_from_token`] + [`ResolvedChord::display`] with
/// the live matcher, so what Settings shows and what the editor fires can never
/// be two different answers.
pub(super) fn display_combo(combo: &str) -> Option<String> {
    let c = Chord::parse(combo)?;
    let key = key_from_token(&c.key)?;
    Some(
        ResolvedChord {
            cmd: c.cmd,
            shift: c.shift,
            alt: c.alt,
            key,
        }
        .display(),
    )
}

/// A chord resolved all the way to an [`egui::Key`] plus its required modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ResolvedChord {
    cmd: bool,
    shift: bool,
    alt: bool,
    key: egui::Key,
}

impl ResolvedChord {
    /// Render as a user-facing chord, e.g. `"Ctrl+Shift+F"`.
    ///
    /// `mod` prints per platform — **Cmd** on macOS, **Ctrl** elsewhere — which
    /// also fixes the old hard-coded tables, whose literal `"Ctrl+…"` strings were
    /// wrong for every macOS user regardless of rebinding.
    fn display(self) -> String {
        let mut out = String::new();
        if self.cmd {
            out.push_str(if cfg!(target_os = "macos") {
                "Cmd+"
            } else {
                "Ctrl+"
            });
        }
        if self.alt {
            out.push_str(if cfg!(target_os = "macos") {
                "Option+"
            } else {
                "Alt+"
            });
        }
        if self.shift {
            out.push_str("Shift+");
        }
        out.push_str(key_display(self.key));
        out
    }

    /// The live [`egui::Modifiers`] a physical press of this chord arrives with.
    ///
    /// Mirrors what `egui_winit` writes on `WindowEvent::ModifiersChanged`
    /// (0.34.3 `src/lib.rs:462-478`): off macOS `command` and `ctrl` are the SAME
    /// physical key, so both flags are set; on macOS `command` rides with
    /// `mac_cmd`. The combo grammar has no way to spell a macOS-only Ctrl, so
    /// `ctrl` is never set alone. Needed because the clipboard predicates below
    /// read `modifiers.ctrl` for one of their arms, and a chord that only set
    /// `command` would miss it.
    fn live_modifiers(self) -> egui::Modifiers {
        let mut m = egui::Modifiers {
            alt: self.alt,
            shift: self.shift,
            ..egui::Modifiers::NONE
        };
        if self.cmd {
            m.command = true;
            if cfg!(target_os = "macos") {
                m.mac_cmd = true;
            } else {
                m.ctrl = true;
            }
        }
        m
    }
}

// ---- chords the windowing layer EATS before egui ever sees them ----
//
// `egui_winit::State::on_keyboard_input` (0.34.3 `src/lib.rs:962`) special-cases
// cut / copy / paste on the key **DOWN** and `return`s at lines 1015 / 1018 /
// 1026 — BEFORE the `self.egui_input.events.push(egui::Event::Key { … })` at
// line 1030. For a chord one of those three predicates answers `true` for,
// `Event::Key { pressed: true }` is therefore NEVER emitted, and a binding
// matched with `i.key_pressed(…)` is dead code in the shipped app.
//
// The load-bearing detail — and the reason a whole CLASS of bindings can rot
// silently — is that none of the three predicates excludes Shift or Alt. They
// test `modifiers.command && keycode == X/C/V`, so Ctrl+**Shift**+V is eaten
// exactly like Ctrl+V. `toggle_md_preview` shipped on `mod+shift+v` and could
// never fire; a test that synthesises the key press (rather than replaying what
// the windowing layer really delivers) cannot see that, which is how it lasted.
//
// The three functions below are TRANSCRIPTIONS of egui-winit 0.34.3
// `is_cut_command` (line 1305), `is_copy_command` (1311) and `is_paste_command`
// (1317). They are the single source both the user-facing diagnostic
// ([`Keymap::swallowed_chord_messages`]) and the test-side delivery simulator
// ([`egui_winit_key_down`]) read, so the warning and the guard can never drift
// apart.

/// egui-winit 0.34.3 `src/lib.rs:1305`.
fn is_cut_command(modifiers: egui::Modifiers, keycode: egui::Key) -> bool {
    keycode == egui::Key::Cut
        || (modifiers.command && keycode == egui::Key::X)
        || (cfg!(target_os = "windows") && modifiers.shift && keycode == egui::Key::Delete)
}

/// egui-winit 0.34.3 `src/lib.rs:1311`.
fn is_copy_command(modifiers: egui::Modifiers, keycode: egui::Key) -> bool {
    keycode == egui::Key::Copy
        || (modifiers.command && keycode == egui::Key::C)
        || (cfg!(target_os = "windows") && modifiers.ctrl && keycode == egui::Key::Insert)
}

/// egui-winit 0.34.3 `src/lib.rs:1317`.
fn is_paste_command(modifiers: egui::Modifiers, keycode: egui::Key) -> bool {
    keycode == egui::Key::Paste
        || (modifiers.command && keycode == egui::Key::V)
        || (cfg!(target_os = "windows") && modifiers.shift && keycode == egui::Key::Insert)
}

/// Which clipboard command the windowing layer turns `chord` into, or `None`
/// when the chord is genuinely deliverable as a key press.
///
/// `Some(_)` means the chord is UNMATCHABLE by [`Keymap::pressed`] on this
/// platform, no matter how the action behind it is written.
fn swallowed_by(chord: ResolvedChord) -> Option<&'static str> {
    let m = chord.live_modifiers();
    if is_cut_command(m, chord.key) {
        Some("Cut")
    } else if is_copy_command(m, chord.key) {
        Some("Copy")
    } else if is_paste_command(m, chord.key) {
        Some("Paste")
    } else {
        None
    }
}

/// The events `egui_winit` really pushes for a key **DOWN** of `key` + `mods`.
///
/// Test-side only, and the whole point of it: `Driver::key` and the local
/// `press` helpers synthesise `Event::Key { pressed: true }` unconditionally, so
/// a binding on a swallowed chord passes them while being dead in the shipped
/// app. Driving a test through THIS instead replays what production receives —
/// for a swallowed chord, no key press at all.
///
/// Models an EMPTY clipboard for the paste arm (egui-winit only pushes
/// `Event::Paste` when `clipboard.get()` yields non-empty text, at
/// `src/lib.rs:1020-1025`) because the clipboard's contents are irrelevant here:
/// the `return` at line 1026 is unconditional, so no `Event::Key` is emitted
/// either way.
#[cfg(test)]
pub(super) fn egui_winit_key_down(key: egui::Key, mods: egui::Modifiers) -> Vec<egui::Event> {
    // egui-winit 0.34.3 `src/lib.rs:1011-1036`, the `if pressed` arm.
    if is_cut_command(mods, key) {
        return vec![egui::Event::Cut];
    }
    if is_copy_command(mods, key) {
        return vec![egui::Event::Copy];
    }
    if is_paste_command(mods, key) {
        return Vec::new();
    }
    vec![egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: mods,
    }]
}

/// The user's keymap, resolved once per `[keybindings]` change.
///
/// Entries are declaration-ordered, parallel to [`Keybindings::entries`]. An
/// action whose combo is blank / unparseable / names an unknown key resolves to
/// `None` and simply never fires — `Keybindings::validate` is what surfaces that
/// to the user.
/// Deliberately NOT `Default`: an empty keymap answers "no" to every action, so a
/// defaulted one would silently disable every shortcut in the editor. Build it
/// with [`Keymap::resolve`] from a real [`Keybindings`] — for the stock chords,
/// `Keymap::resolve(&Keybindings::default())`.
#[derive(Debug, Clone)]
pub(super) struct Keymap {
    chords: Vec<(&'static str, Option<ResolvedChord>)>,
}

impl Keymap {
    /// Resolve every binding in `kb` into a matchable chord.
    pub(super) fn resolve(kb: &Keybindings) -> Self {
        let chords = kb
            .entries()
            .iter()
            .map(|(name, combo)| {
                let resolved = Chord::parse(combo).and_then(|c| {
                    key_from_token(&c.key).map(|key| ResolvedChord {
                        cmd: c.cmd,
                        shift: c.shift,
                        alt: c.alt,
                        key,
                    })
                });
                (*name, resolved)
            })
            .collect();
        Self { chords }
    }

    fn chord(&self, action: &str) -> Option<ResolvedChord> {
        self.chords
            .iter()
            .find(|(name, _)| *name == action)
            .and_then(|(_, chord)| *chord)
    }

    /// Actions whose combo is a well-formed chord but names a key that does not
    /// exist (`mod+nosuchkey`), reported as ready-made user-facing messages.
    ///
    /// [`Keybindings::validate`] cannot catch this and should not try: it lives in
    /// `scribe-core` and owns the combo GRAMMAR, while the key TABLE belongs to the
    /// UI layer — to a grammar check, `nosuchkey` is a perfectly good key token.
    /// Only resolution knows better. Reporting it here is what stops an unknown
    /// key name from becoming exactly the silent dead shortcut the validation
    /// exists to prevent.
    ///
    /// Blank and unparseable combos are NOT reported here — `validate` already
    /// covers those, and double-reporting one binding would be noise.
    pub(super) fn unknown_key_messages(kb: &Keybindings) -> Vec<String> {
        kb.entries()
            .iter()
            .filter(|(_, combo)| {
                // Only combos that parse (so not Empty/Invalid) but whose key is
                // unresolvable.
                Chord::parse(combo).is_some_and(|c| key_from_token(&c.key).is_none())
            })
            .map(|(action, combo)| {
                format!("'{action}' is bound to '{combo}', which is not a key on your keyboard — it cannot be triggered")
            })
            .collect()
    }

    /// Actions bound to a chord the windowing layer turns into a clipboard
    /// command before egui sees it (see [`swallowed_by`]), reported as ready-made
    /// user-facing messages.
    ///
    /// The third way a well-formed binding can be dead, and the only one that is
    /// invisible to every other check: the combo parses, the key exists, the
    /// settings row pretty-prints it — and it can still never fire, because
    /// `egui_winit` consumes the key-down as Cut / Copy / Paste and returns.
    /// Left unreported, a user who rebinds an action onto Ctrl+C gets silence and
    /// no way to find out why; that is exactly how `toggle_md_preview` sat dead
    /// on `mod+shift+v`.
    ///
    /// Neither [`Keybindings::validate`] nor [`Keymap::unknown_key_messages`] can
    /// cover this: the first owns the combo GRAMMAR and the second the key TABLE,
    /// while this is a property of the WINDOWING layer — which only this module
    /// knows about. Blank / unparseable / unknown-key combos are not repeated
    /// here; they are already reported and cannot reach a resolved chord anyway.
    pub(super) fn swallowed_chord_messages(kb: &Keybindings) -> Vec<String> {
        kb.entries()
            .iter()
            .filter_map(|(action, combo)| {
                let chord = Chord::parse(combo).and_then(|c| {
                    key_from_token(&c.key).map(|key| ResolvedChord {
                        cmd: c.cmd,
                        shift: c.shift,
                        alt: c.alt,
                        key,
                    })
                })?;
                let eaten_as = swallowed_by(chord)?;
                Some(format!(
                    "'{action}' is bound to '{combo}', which your system delivers to the editor as \
                     {eaten_as} — the shortcut can never fire. Bind it to a different combo."
                ))
            })
            .collect()
    }

    /// How the chords for `actions` currently read, for help surfaces.
    ///
    /// Returns `None` when `actions` is empty — the caller's hard-coded string is
    /// then correct, because the shortcut is not rebindable (Esc, F1, Ctrl+C …).
    /// Several actions render joined by `" / "`, which is how the font-zoom row
    /// shows in/out/reset as one line.
    ///
    /// An action that resolves to nothing renders as `"unbound"` rather than being
    /// skipped: the whole point of reading this off the live keymap is that the
    /// help never claims a key that would not work. If EVERY action is unbound the
    /// row says so honestly.
    pub(super) fn display_for(&self, actions: &[&str]) -> Option<String> {
        if actions.is_empty() {
            return None;
        }
        let shown: Vec<String> = actions
            .iter()
            .map(|a| {
                self.chord(a)
                    .map_or_else(|| "unbound".to_string(), ResolvedChord::display)
            })
            .collect();
        Some(shown.join(" / "))
    }

    /// Did the user press the chord bound to `action` this frame?
    ///
    /// Modifiers must match EXACTLY, so `mod+o` does not fire on Ctrl+Shift+O.
    pub(super) fn pressed(&self, i: &egui::InputState, action: &str) -> bool {
        let Some(c) = self.chord(action) else {
            return false;
        };
        let mods_ok = |shift: bool| {
            i.modifiers.command == c.cmd && i.modifiers.alt == c.alt && i.modifiers.shift == shift
        };
        if i.key_pressed(c.key) && mods_ok(c.shift) {
            return true;
        }
        // Shifted-symbol tolerance. On most layouts `+` IS Shift+`=`, so a press
        // of Ctrl+`+` arrives as (Key::Plus, shift: true). Exact matching alone
        // would stop the default `mod+equals` zoom-in from firing for anyone who
        // types Ctrl++ — which the hard-wired handler accepted (it tested
        // `Plus || Equals`). Accept the Plus press for an `equals` chord that
        // does not itself ask for Shift, and keep the other modifiers exact.
        if c.key == egui::Key::Equals
            && !c.shift
            && i.key_pressed(egui::Key::Plus)
            && i.modifiers.command == c.cmd
            && i.modifiers.alt == c.alt
        {
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run one frame carrying `key` + `mods` and ask the keymap whether `action`
    /// fired. `pressed` takes an `&InputState`, which only egui can build.
    fn fired(km: &Keymap, action: &str, key: egui::Key, mods: egui::Modifiers) -> bool {
        let ctx = egui::Context::default();
        let mut out = false;
        let input = egui::RawInput {
            modifiers: mods,
            events: vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: mods,
            }],
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            out = ctx.input(|i| km.pressed(i, action));
        });
        out
    }

    const SHIFT: egui::Modifiers = egui::Modifiers::SHIFT;
    const CMD: egui::Modifiers = egui::Modifiers::COMMAND;

    // ---- shifted-symbol tolerance ----
    //
    // The `Key::Plus` branch of `pressed` had no test at all: four mutants in it
    // survived the whole suite (its `== Equals`, its `!c.shift`, and BOTH of its
    // modifier equalities could be inverted undetected). It is a real feature —
    // on most layouts `+` IS Shift+`=`, so Ctrl+`+` must zoom in — and every one
    // of those inversions is a user-visible break.

    #[test]
    fn ctrl_plus_fires_the_zoom_in_chord_bound_to_mod_equals() {
        // THE reason the branch exists. Ctrl+`+` arrives as (Plus, shift: true)
        // while the chord is `mod+equals` (no shift), so exact matching alone
        // would drop it.
        let km = Keymap::resolve(&Keybindings::default());
        assert!(
            fired(&km, action::INCREASE_FONT, egui::Key::Plus, CMD | SHIFT),
            "Ctrl++ must zoom in — it is the same physical key as Ctrl+="
        );
    }

    #[test]
    fn ctrl_equals_still_fires_zoom_in_by_exact_match() {
        // The tolerance must not be the ONLY way in.
        let km = Keymap::resolve(&Keybindings::default());
        assert!(fired(&km, action::INCREASE_FONT, egui::Key::Equals, CMD));
    }

    #[test]
    fn ctrl_plus_does_not_fire_chords_bound_to_other_keys() {
        // The tolerance is scoped to `equals` chords. If it applied to every
        // OTHER key instead, Ctrl+`+` would fire New File, Save, and the rest at
        // once — pressing one key would run half the editor.
        let km = Keymap::resolve(&Keybindings::default());
        for action in [action::NEW_FILE, action::SAVE, action::FIND] {
            assert!(
                !fired(&km, action, egui::Key::Plus, CMD | SHIFT),
                "Ctrl++ must not fire '{action}' — the Plus tolerance is only for `equals` chords"
            );
        }
    }

    #[test]
    fn a_plus_without_the_command_modifier_does_not_zoom() {
        // Typing a literal `+` into a note is Shift+`=` and nothing else. If the
        // tolerance stopped checking `command`, every `+` typed would resize the
        // font.
        let km = Keymap::resolve(&Keybindings::default());
        assert!(
            !fired(&km, action::INCREASE_FONT, egui::Key::Plus, SHIFT),
            "a bare + is a character, not a zoom"
        );
    }

    #[test]
    fn alt_plus_does_not_zoom() {
        // The chord is `mod+equals`: alt is NOT part of it, so Ctrl+Alt+`+` is a
        // different chord and must not zoom.
        let km = Keymap::resolve(&Keybindings::default());
        assert!(!fired(
            &km,
            action::INCREASE_FONT,
            egui::Key::Plus,
            CMD | SHIFT | egui::Modifiers::ALT
        ));
    }

    // ---- exact modifier matching (the PRIMARY branch) ----
    //
    // `pressed`'s contract is the first line of its docstring: "Modifiers must
    // match EXACTLY, so `mod+o` does not fire on Ctrl+Shift+O." Nothing tested
    // it. Every `&&` in the primary branch could be flipped to `||` with the
    // whole suite green (cargo-mutants 312a / 312b / 314:33 all survived).
    //
    // The reason they all hid: every test above presses `Key::Plus` — the WRONG
    // key — so `i.key_pressed(c.key)` is always false, which short-circuits the
    // primary branch and masks everything inside it. Those tests only ever
    // exercised the Plus-tolerance arm. The shape that discriminates is the
    // opposite: press the RIGHT key with the WRONG modifiers, so `mods_ok` is
    // the term that has to decide.
    //
    // `find` is `mod+f` and `find_in_files` is `mod+shift+f` — they differ by
    // exactly one modifier, so a `mods_ok` that stops being exact collapses the
    // two chords into one.

    const ALT: egui::Modifiers = egui::Modifiers::ALT;

    #[test]
    fn a_chord_does_not_fire_when_an_extra_modifier_is_held() {
        let km = Keymap::resolve(&Keybindings::default());
        assert!(
            fired(&km, action::FIND, egui::Key::F, CMD),
            "precondition: Ctrl+F fires find"
        );
        assert!(
            !fired(&km, action::FIND, egui::Key::F, CMD | SHIFT),
            "Ctrl+Shift+F must not fire the plain `mod+f` chord — that is \
             find-in-files, a different action"
        );
        assert!(
            !fired(&km, action::FIND, egui::Key::F, CMD | ALT),
            "Ctrl+Alt+F must not fire `mod+f` either"
        );
    }

    #[test]
    fn a_chord_does_not_fire_when_a_required_modifier_is_missing() {
        let km = Keymap::resolve(&Keybindings::default());
        assert!(
            !fired(&km, action::FIND, egui::Key::F, egui::Modifiers::NONE),
            "a bare `f` is a character being typed into a note, not a Find"
        );
        assert!(
            !fired(&km, action::FIND, egui::Key::F, SHIFT),
            "Shift+F is a capital F, not a Find"
        );
        assert!(
            !fired(&km, action::FIND, egui::Key::F, ALT),
            "Alt+F is not `mod+f`"
        );
    }

    #[test]
    fn a_shifted_chord_is_not_fired_by_its_unshifted_twin() {
        // The other direction: `mod+shift+f` must need its Shift, or Find and
        // Find-in-Files both fire on one keypress.
        let km = Keymap::resolve(&Keybindings::default());
        assert!(
            fired(&km, action::FIND_IN_FILES, egui::Key::F, CMD | SHIFT),
            "precondition: Ctrl+Shift+F fires find-in-files"
        );
        assert!(
            !fired(&km, action::FIND_IN_FILES, egui::Key::F, CMD),
            "Ctrl+F must not fire the `mod+shift+f` action"
        );
    }

    // ---- the ten actions added on top of the original 35 ----
    //
    // Save-As, and the nine by-index tab switches. The two pins above
    // (`action_names_match_the_config_schema` and
    // `every_default_binding_resolves_to_the_expected_key`) were both updated
    // for them rather than merely widened; these are the behavioural tests that
    // say what the new chords must DO.

    #[test]
    fn ctrl_shift_s_is_save_as_and_ctrl_s_is_still_save() {
        // Save and Save-As differ by exactly one modifier, so they are the pair
        // most at risk of collapsing into each other. Both directions, because
        // only one of them going wrong is the interesting failure: a Ctrl+S that
        // also opens the Save-As dialog, or a Ctrl+Shift+S that silently
        // overwrites the original file instead of asking for a new name.
        let km = Keymap::resolve(&Keybindings::default());
        assert!(fired(&km, action::SAVE, egui::Key::S, CMD));
        assert!(
            !fired(&km, action::SAVE, egui::Key::S, CMD | SHIFT),
            "Ctrl+Shift+S must NOT save in place — that is Save As"
        );
        assert!(fired(&km, action::SAVE_AS, egui::Key::S, CMD | SHIFT));
        assert!(
            !fired(&km, action::SAVE_AS, egui::Key::S, CMD),
            "Ctrl+S must NOT open the Save-As dialog"
        );
    }

    #[test]
    fn each_goto_tab_action_answers_to_its_own_number_key_and_no_other() {
        // The failure this catches is a TRANSPOSED table — `GOTO_TAB[2]` wired
        // to Ctrl+7 — which is invisible to any test that only checks "some tab
        // action fired". Pressing Ctrl+N must fire the Nth action and leave the
        // other eight cold, so the full 9x9 matrix is asserted.
        let km = Keymap::resolve(&Keybindings::default());
        let num_keys = [
            egui::Key::Num1,
            egui::Key::Num2,
            egui::Key::Num3,
            egui::Key::Num4,
            egui::Key::Num5,
            egui::Key::Num6,
            egui::Key::Num7,
            egui::Key::Num8,
            egui::Key::Num9,
        ];
        assert_eq!(action::GOTO_TAB.len(), num_keys.len());
        for (pressed_idx, key) in num_keys.iter().enumerate() {
            for (action_idx, tab_action) in action::GOTO_TAB.iter().enumerate() {
                assert_eq!(
                    fired(&km, tab_action, *key, CMD),
                    pressed_idx == action_idx,
                    "Ctrl+{} must fire '{tab_action}' iff it is tab {}",
                    pressed_idx + 1,
                    pressed_idx + 1
                );
            }
        }
        // Ctrl+0 belongs to the font reset, not to a tab — the tab band is
        // 1..=9 and must not creep onto the digit next to it.
        for tab_action in action::GOTO_TAB {
            assert!(!fired(&km, tab_action, egui::Key::Num0, CMD));
        }
    }

    #[test]
    fn a_bare_number_key_does_not_switch_tabs() {
        // Typing "1" into a note is a character. If the chord lost its command
        // modifier, every digit typed would jump the user to another tab.
        let km = Keymap::resolve(&Keybindings::default());
        for mods in [egui::Modifiers::NONE, SHIFT, ALT] {
            assert!(!fired(&km, action::GOTO_TAB_1, egui::Key::Num1, mods));
        }
    }

    #[test]
    fn action_names_match_the_config_schema() {
        // Bidirectional parity: every const names a real binding, and every
        // binding in the schema has a const. A binding added to `Keybindings`
        // without wiring an action here fails HERE, which is the check that was
        // missing when the whole `[keybindings]` section shipped unwired.
        let kb = Keybindings::default();
        let schema: Vec<&str> = kb.entries().iter().map(|(n, _)| *n).collect();
        for name in action::ALL {
            assert!(
                schema.contains(name),
                "action const '{name}' is not a field in the Keybindings schema"
            );
        }
        for name in &schema {
            assert!(
                action::ALL.contains(name),
                "Keybindings field '{name}' has no action const — it cannot be wired to input"
            );
        }
        assert_eq!(
            action::ALL.len(),
            schema.len(),
            "action list must not duplicate"
        );
    }

    #[test]
    fn every_default_binding_resolves_to_the_expected_key() {
        // Pins the token -> egui::Key mapping for EVERY shipped default (the
        // `expect.len() == action::ALL.len()` assert below is what keeps this
        // table exhaustive rather than merely long). This is
        // the guard on `key_from_token` reading egui's own tables: an egui rename
        // (or a Debug-format change) breaks this test instead of silently
        // resolving a shortcut to `None` and killing it at runtime.
        let km = Keymap::resolve(&Keybindings::default());
        let expect: &[(&str, bool, bool, bool, egui::Key)] = &[
            // (action, cmd, shift, alt, key)
            (action::NEW_FILE, true, false, false, egui::Key::N),
            (action::OPEN_FILE, true, false, false, egui::Key::O),
            (action::SAVE, true, false, false, egui::Key::S),
            (action::SAVE_AS, true, true, false, egui::Key::S),
            (action::FIND, true, false, false, egui::Key::F),
            (action::FIND_IN_FILES, true, true, false, egui::Key::F),
            (action::REPLACE, true, false, false, egui::Key::H),
            (action::COMMAND_PALETTE, true, true, false, egui::Key::P),
            (action::FUZZY_FINDER, true, false, false, egui::Key::P),
            (action::GOTO_LINE, true, false, false, egui::Key::G),
            (action::GOTO_SYMBOL, true, true, false, egui::Key::O),
            (action::RECENT_FILES, true, false, false, egui::Key::R),
            (action::CLOSE_TAB, true, false, false, egui::Key::W),
            (action::NEXT_TAB, true, false, false, egui::Key::Tab),
            (action::PREV_TAB, true, true, false, egui::Key::Tab),
            // Ctrl+1..9 — pinned per index, so a transposed default (goto_tab_3
            // resolving to Num7) fails HERE instead of quietly switching the
            // user to the wrong tab.
            (action::GOTO_TAB_1, true, false, false, egui::Key::Num1),
            (action::GOTO_TAB_2, true, false, false, egui::Key::Num2),
            (action::GOTO_TAB_3, true, false, false, egui::Key::Num3),
            (action::GOTO_TAB_4, true, false, false, egui::Key::Num4),
            (action::GOTO_TAB_5, true, false, false, egui::Key::Num5),
            (action::GOTO_TAB_6, true, false, false, egui::Key::Num6),
            (action::GOTO_TAB_7, true, false, false, egui::Key::Num7),
            (action::GOTO_TAB_8, true, false, false, egui::Key::Num8),
            (action::GOTO_TAB_9, true, false, false, egui::Key::Num9),
            (action::REOPEN_TAB, true, true, false, egui::Key::R),
            (
                action::TOGGLE_GRID,
                true,
                false,
                false,
                egui::Key::Backslash,
            ),
            (action::TOGGLE_COMMENT, true, false, false, egui::Key::Slash),
            (action::JUMP_BRACKET, true, false, false, egui::Key::M),
            (
                action::TOGGLE_FULLSCREEN,
                false,
                false,
                false,
                egui::Key::F11,
            ),
            (action::TOGGLE_ZEN, true, false, false, egui::Key::Period),
            (action::CYCLE_THEME, true, true, false, egui::Key::T),
            (action::TOGGLE_MINIMAP, true, true, false, egui::Key::M),
            // Ctrl+E. The original `mod+shift+v` resolved perfectly well and
            // still never fired — the windowing layer ate the press as Paste —
            // which is why this pin is now backed by
            // `every_default_binding_survives_the_windowing_layer` rather than
            // resolution alone.
            (action::TOGGLE_MD_PREVIEW, true, false, false, egui::Key::E),
            (action::FOLD_ALL, true, true, false, egui::Key::OpenBracket),
            (
                action::EXPAND_ALL,
                true,
                true,
                false,
                egui::Key::CloseBracket,
            ),
            (action::INCREASE_FONT, true, false, false, egui::Key::Equals),
            (action::DECREASE_FONT, true, false, false, egui::Key::Minus),
            (action::RESET_FONT, true, false, false, egui::Key::Num0),
            (action::MOVE_LINE_UP, false, false, true, egui::Key::ArrowUp),
            (
                action::MOVE_LINE_DOWN,
                false,
                false,
                true,
                egui::Key::ArrowDown,
            ),
            (action::DUPLICATE_LINE, true, true, false, egui::Key::D),
            (action::JOIN_LINES, true, false, false, egui::Key::J),
            (action::TOGGLE_BOOKMARK, true, false, false, egui::Key::F2),
            (action::NEXT_BOOKMARK, false, false, false, egui::Key::F2),
            (action::PREV_BOOKMARK, false, true, false, egui::Key::F2),
        ];
        assert_eq!(
            expect.len(),
            action::ALL.len(),
            "every action must be pinned here"
        );
        for (name, cmd, shift, alt, key) in expect {
            let got = km
                .chord(name)
                .unwrap_or_else(|| panic!("default binding '{name}' must resolve to a chord"));
            assert_eq!(
                got,
                ResolvedChord {
                    cmd: *cmd,
                    shift: *shift,
                    alt: *alt,
                    key: *key
                },
                "default binding '{name}' resolved to the wrong chord"
            );
        }
    }

    #[test]
    fn chords_render_with_this_platforms_modifier_names() {
        // Both render paths — resolved chords and hard-coded fallback text — must
        // agree on what `mod` is called here, or the cheatsheet reads half
        // "Cmd+E" and half "Ctrl+C" on macOS.
        let km = Keymap::resolve(&Keybindings::default());
        let save = km.display_for(&[action::SAVE]).expect("save is bound");
        let zoom = km
            .display_for(&[
                action::INCREASE_FONT,
                action::DECREASE_FONT,
                action::RESET_FONT,
            ])
            .expect("font zoom is bound");
        if cfg!(target_os = "macos") {
            assert_eq!(save, "Cmd+S");
            assert_eq!(zoom, "Cmd+= / Cmd+- / Cmd+0");
            assert_eq!(platform_chord_text("Ctrl+C"), "Cmd+C");
            assert_eq!(platform_chord_text("Ctrl+Alt+X"), "Cmd+Option+X");
            assert_eq!(
                km.display_for(&[action::MOVE_LINE_UP]).unwrap(),
                "Option+Up"
            );
        } else {
            assert_eq!(save, "Ctrl+S");
            assert_eq!(zoom, "Ctrl+= / Ctrl+- / Ctrl+0");
            assert_eq!(platform_chord_text("Ctrl+C"), "Ctrl+C", "no-op off macOS");
            assert_eq!(platform_chord_text("Ctrl+Alt+X"), "Ctrl+Alt+X");
            assert_eq!(km.display_for(&[action::MOVE_LINE_UP]).unwrap(), "Alt+Up");
        }
        // Punctuation reads as the key on the keyboard, not egui's variant name.
        assert!(
            km.display_for(&[action::TOGGLE_GRID])
                .unwrap()
                .ends_with('\\'),
            "toggle_grid must render as the backslash key"
        );
        // An unbound action says so rather than naming a key that does nothing.
        let km = Keymap::resolve(&Keybindings {
            save: String::new(),
            ..Default::default()
        });
        assert_eq!(km.display_for(&[action::SAVE]).as_deref(), Some("unbound"));
        // No actions => the caller's static text is correct.
        assert_eq!(km.display_for(&[]), None);
    }

    #[test]
    fn key_from_token_accepts_both_spellings_and_rejects_junk() {
        assert_eq!(key_from_token("arrowup"), Some(egui::Key::ArrowUp));
        assert_eq!(key_from_token("up"), Some(egui::Key::ArrowUp));
        assert_eq!(key_from_token("num0"), Some(egui::Key::Num0));
        assert_eq!(key_from_token("0"), Some(egui::Key::Num0));
        assert_eq!(key_from_token("openbracket"), Some(egui::Key::OpenBracket));
        assert_eq!(key_from_token("f11"), Some(egui::Key::F11));
        assert_eq!(key_from_token("n"), Some(egui::Key::N));
        assert_eq!(key_from_token("nope"), None);
        assert_eq!(key_from_token(""), None);
    }

    // ---- chord CAPTURE (the settings-page inverse of `resolve`) ----

    #[test]
    fn a_captured_chord_round_trips_back_to_the_key_that_was_pressed() {
        // The load-bearing property of `combo_from_press`: whatever the user
        // physically pressed must come back out of the config string. Run it over
        // egui's ENTIRE key table so a key whose Debug spelling `key_from_token`
        // cannot resolve (now or after an egui bump) fails HERE — instead of
        // shipping a rebind UI that silently writes a dead binding.
        for key in egui::Key::ALL {
            let combo = combo_from_press(*key, egui::Modifiers::NONE);
            let chord = Chord::parse(&combo)
                .unwrap_or_else(|| panic!("captured '{combo}' must parse back into a chord"));
            assert_eq!(
                key_from_token(&chord.key),
                Some(*key),
                "captured '{combo}' must resolve back to the key that was pressed"
            );
        }
    }

    #[test]
    fn a_captured_chord_records_exactly_the_modifiers_that_were_held() {
        // Modifier fidelity in BOTH directions: a held modifier must appear, and
        // an unheld one must not. A capture that dropped Shift would rebind
        // `mod+shift+f` as `mod+f` and collide with Find.
        assert_eq!(combo_from_press(egui::Key::F, CMD | SHIFT), "mod+shift+f");
        assert_eq!(combo_from_press(egui::Key::F, CMD), "mod+f");
        assert_eq!(combo_from_press(egui::Key::F, SHIFT), "shift+f");
        assert_eq!(combo_from_press(egui::Key::F, ALT), "alt+f");
        assert_eq!(
            combo_from_press(egui::Key::F11, egui::Modifiers::NONE),
            "f11"
        );
        // Canonical ORDER (mod, alt, shift), so a captured chord is byte-equal to
        // the same chord written by hand and conflict detection sees one combo.
        let captured = combo_from_press(egui::Key::K, CMD | ALT | SHIFT);
        assert_eq!(captured, "mod+alt+shift+k");
        assert_eq!(
            Chord::parse(&captured).unwrap().canonical(),
            captured,
            "a captured combo must already BE canonical"
        );
    }

    #[test]
    fn a_captured_chord_fires_the_action_it_was_bound_to() {
        // Capture -> store -> resolve -> match, end to end through the real
        // matcher: pressing Ctrl+Alt+K after binding SAVE to a capture of
        // Ctrl+Alt+K must fire save, and the OLD default must not.
        let combo = combo_from_press(egui::Key::K, CMD | ALT);
        let km = Keymap::resolve(&Keybindings {
            save: combo,
            ..Default::default()
        });
        assert!(
            fired(&km, action::SAVE, egui::Key::K, CMD | ALT),
            "the captured chord must fire the action it was captured for"
        );
        assert!(
            !fired(&km, action::SAVE, egui::Key::S, CMD),
            "the replaced default must stop firing"
        );
    }

    // ---- combo DISPLAY (what the settings row and the cheatsheet show) ----

    #[test]
    fn display_combo_renders_a_stored_binding_the_way_the_cheatsheet_does() {
        // Settings and the cheatsheet must agree, or the two surfaces teach
        // different keys for one action.
        let km = Keymap::resolve(&Keybindings::default());
        for action in [action::SAVE, action::TOGGLE_GRID, action::MOVE_LINE_UP] {
            let stored = Keybindings::default()
                .get(action)
                .expect("a default binding")
                .to_string();
            assert_eq!(
                display_combo(&stored).as_deref(),
                km.display_for(&[action]).as_deref(),
                "'{action}' must read the same in Settings and the cheatsheet"
            );
        }
    }

    #[test]
    fn display_combo_refuses_to_pretty_print_a_binding_that_cannot_fire() {
        // The three ways a binding is dead. Each must yield None so the settings
        // row can warn, rather than rendering a chord the editor will never match.
        assert_eq!(display_combo(""), None, "blank");
        assert_eq!(display_combo("   "), None, "whitespace-only");
        assert_eq!(display_combo("mod"), None, "modifiers with no key");
        assert_eq!(display_combo("a+b"), None, "two non-modifier keys");
        assert_eq!(
            display_combo("mod+nosuchkey"),
            None,
            "not a key on the keyboard"
        );
        // …and a live one still renders.
        assert!(display_combo("mod+s").is_some());
    }

    // ---- the windowing layer eats whole chords (THE class guard) ----
    //
    // A binding can resolve to a perfectly good `ResolvedChord`, pretty-print in
    // Settings, and STILL be unreachable: `egui_winit::State::on_keyboard_input`
    // turns cut / copy / paste chords into `Event::Cut` / `Copy` / `Paste` on the
    // key DOWN and returns before pushing the `Event::Key`. `toggle_md_preview`
    // shipped on `mod+shift+v` — swallowed as Paste, because `is_paste_command`
    // tests `command && V` and never excludes Shift — and was dead for its whole
    // life.
    //
    // Nothing caught it because every test helper in this crate (`fired` above,
    // `Driver::key`, the local `press`) SYNTHESISES the key press. A binding that
    // production never receives a press for still tests green against them. The
    // tests below are the ones that discriminate: they build their events with
    // `egui_winit_key_down`, so what the dispatcher sees is what the windowing
    // layer would really have delivered.

    /// `fired`, but over an explicit event list instead of a synthesised press.
    fn fired_from(
        km: &Keymap,
        action: &str,
        mods: egui::Modifiers,
        events: Vec<egui::Event>,
    ) -> bool {
        let ctx = egui::Context::default();
        let mut out = false;
        let input = egui::RawInput {
            modifiers: mods,
            events,
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            out = ctx.input(|i| km.pressed(i, action));
        });
        out
    }

    #[test]
    fn every_default_binding_survives_the_windowing_layer() {
        // THE guard this whole class needs: for all 45 actions, push the chord
        // through the real delivery path and require the dispatcher to still see
        // it. A future default (or an egui-winit bump that widens a predicate)
        // that lands on a swallowed chord fails HERE — at the point the binding
        // is chosen — instead of shipping as a key that does nothing.
        //
        // Asserted end to end (`Keymap::pressed` over the delivered events), not
        // merely "an Event::Key exists": the contract is that the ACTION fires,
        // and only running the matcher over the real event stream proves it.
        let km = Keymap::resolve(&Keybindings::default());
        for name in action::ALL {
            let chord = km
                .chord(name)
                .unwrap_or_else(|| panic!("default binding '{name}' must resolve"));
            let mods = chord.live_modifiers();
            let delivered = egui_winit_key_down(chord.key, mods);
            assert!(
                delivered.iter().any(|e| matches!(
                    e,
                    egui::Event::Key { key, pressed: true, .. } if *key == chord.key
                )),
                "'{name}' is bound to {} — the windowing layer eats that chord as {:?} and \
                 never emits a key press, so the action can never fire. Bind it elsewhere.",
                chord.display(),
                swallowed_by(chord)
            );
            assert!(
                fired_from(&km, name, mods, delivered),
                "'{name}' ({}) must fire from the events production really delivers",
                chord.display()
            );
        }
    }

    #[test]
    fn the_windowing_layer_really_does_eat_the_clipboard_chords() {
        // The control that keeps the guard above from being vacuous. If
        // `egui_winit_key_down` simply forwarded everything, the guard would pass
        // for a binding on Ctrl+V and prove nothing. Each chord here must produce
        // NO key press — including the shifted and alted variants, which is the
        // whole surprise: the predicates test `command && <key>` and stop there.
        let cases: &[(egui::Key, egui::Modifiers, &str)] = &[
            (egui::Key::X, CMD, "Cut"),
            (egui::Key::X, CMD | SHIFT, "Cut"),
            (egui::Key::X, CMD | ALT, "Cut"),
            (egui::Key::C, CMD, "Copy"),
            (egui::Key::C, CMD | SHIFT, "Copy"),
            (egui::Key::C, CMD | ALT, "Copy"),
            (egui::Key::V, CMD, "Paste"),
            (egui::Key::V, CMD | SHIFT, "Paste"),
            (egui::Key::V, CMD | ALT, "Paste"),
            // The dedicated media keys are eaten with no modifier at all.
            (egui::Key::Cut, egui::Modifiers::NONE, "Cut"),
            (egui::Key::Copy, egui::Modifiers::NONE, "Copy"),
            (egui::Key::Paste, egui::Modifiers::NONE, "Paste"),
        ];
        for (key, mods, eaten_as) in cases {
            let chord = ResolvedChord {
                cmd: mods.command,
                shift: mods.shift,
                alt: mods.alt,
                key: *key,
            };
            assert_eq!(
                swallowed_by(chord),
                Some(*eaten_as),
                "{chord:?} must be reported as eaten by {eaten_as}"
            );
            let delivered = egui_winit_key_down(*key, chord.live_modifiers());
            assert!(
                !delivered
                    .iter()
                    .any(|e| matches!(e, egui::Event::Key { pressed: true, .. })),
                "{chord:?} must deliver NO key press; got {delivered:?}"
            );
        }
        // Windows also routes the legacy Insert/Delete clipboard chords, and
        // those arms key off Shift/Ctrl rather than the command modifier.
        for (key, mods) in [
            (egui::Key::Delete, SHIFT),
            (egui::Key::Insert, CMD),
            (egui::Key::Insert, SHIFT),
        ] {
            let chord = ResolvedChord {
                cmd: mods.command,
                shift: mods.shift,
                alt: mods.alt,
                key,
            };
            assert_eq!(
                swallowed_by(chord).is_some(),
                cfg!(target_os = "windows"),
                "{chord:?} is a Windows-only clipboard chord — eaten there, live elsewhere"
            );
        }
        // …and an ordinary chord is NOT eaten, or the predicate would condemn
        // every binding and the guard above would be unsatisfiable.
        assert_eq!(
            swallowed_by(ResolvedChord {
                cmd: true,
                shift: false,
                alt: false,
                key: egui::Key::E,
            }),
            None
        );
    }

    #[test]
    fn the_markdown_preview_chord_moved_off_the_paste_chord() {
        // The specific regression. Both directions, because only re-pinning the
        // new chord would let the old one creep back beside it.
        let kb = Keybindings::default();
        let km = Keymap::resolve(&kb);
        let dead = ResolvedChord {
            cmd: true,
            shift: true,
            alt: false,
            key: egui::Key::V,
        };
        assert_eq!(
            swallowed_by(dead),
            Some("Paste"),
            "mod+shift+v is eaten as Paste — that is why the binding moved"
        );
        assert_ne!(
            kb.toggle_md_preview, "mod+shift+v",
            "the preview toggle must not go back onto the paste chord"
        );
        let live = km
            .chord(action::TOGGLE_MD_PREVIEW)
            .expect("the preview toggle is bound");
        assert_eq!(swallowed_by(live), None);
        assert!(
            fired_from(
                &km,
                action::TOGGLE_MD_PREVIEW,
                live.live_modifiers(),
                egui_winit_key_down(live.key, live.live_modifiers()),
            ),
            "the preview toggle must fire from real delivery, not just a synthesised press"
        );
    }

    #[test]
    fn swallowed_chord_messages_names_a_rebind_onto_a_clipboard_chord() {
        // The user-facing half. A rebind onto Ctrl+C parses, resolves, and
        // pretty-prints — every other check passes it — so without this the user
        // gets silence and no way to learn why.
        let clean = Keymap::swallowed_chord_messages(&Keybindings::default());
        assert!(
            clean.is_empty(),
            "the shipped defaults must be free of swallowed chords: {clean:?}"
        );

        let msgs = Keymap::swallowed_chord_messages(&Keybindings {
            save: "mod+c".into(),
            find: "mod+shift+x".into(),
            ..Default::default()
        });
        let joined = msgs.join(" | ");
        assert_eq!(msgs.len(), 2, "one message per swallowed binding: {joined}");
        assert!(
            msgs.iter().any(|m| m.contains("'save'")
                && m.contains("mod+c")
                && m.contains("Copy")
                && m.contains("can never fire")),
            "must name the action, the combo, and what eats it: {joined}"
        );
        assert!(
            msgs.iter()
                .any(|m| m.contains("'find'") && m.contains("mod+shift+x") && m.contains("Cut")),
            "a SHIFTED clipboard chord is eaten too and must be reported: {joined}"
        );

        // Not double-reported: blank / unparseable / unknown-key combos are
        // `validate`'s and `unknown_key_messages`' business and never reach a
        // resolved chord anyway.
        assert!(Keymap::swallowed_chord_messages(&Keybindings {
            save: String::new(),
            find: "mod".into(),
            replace: "mod+nosuchkey".into(),
            ..Default::default()
        })
        .is_empty());
    }

    #[test]
    fn an_unresolvable_binding_yields_no_chord() {
        // A combo naming a key egui does not have must resolve to None (the action
        // never fires) rather than falling back to some other key.
        let km = Keymap::resolve(&Keybindings {
            save: "mod+nosuchkey".into(),
            ..Default::default()
        });
        assert_eq!(km.chord(action::SAVE), None);
        // Unrelated bindings still resolve.
        assert!(km.chord(action::FIND).is_some());
    }
}
