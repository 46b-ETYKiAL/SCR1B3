//! The Settings → Keyboard page: view and rebind every `[keybindings]` action.
//!
//! The keymap has been authoritative since `[keybindings]` was wired into
//! [`super::keyboard_input`] — but the only way to change it was to hand-edit
//! `scr1b3.toml`, which meant knowing the action names, the combo grammar, and
//! egui's key spellings. This page is the missing surface: one row per action,
//! its CURRENT chord, click-to-capture rebinding, conflict surfacing, and
//! per-row / global restore-defaults.
//!
//! Three things make it honest rather than decorative:
//!
//! - **It writes the live config.** A row edits `config.keybindings` in place,
//!   the host persists via `save_config`, and `handle_keyboard_shortcuts`
//!   re-resolves its [`Keymap`](super::keymap::Keymap) the moment
//!   `keymap_src != config.keybindings`. A rebind is live on the NEXT keypress —
//!   there is no restart, and no separate "apply" step that could silently not
//!   apply.
//! - **It never pretty-prints a dead binding.** The chord shown is
//!   [`keymap::display_combo`], which returns `None` for exactly the combos the
//!   matcher cannot fire (blank, unparseable, unknown key). Those rows say
//!   "unbound" / "won't fire" and carry the reason.
//! - **The editor's shortcut layer stands down while capturing.** Pressing
//!   Ctrl+S to REBIND save must not also save the file. [`capture_active`] is
//!   what `handle_keyboard_shortcuts` checks; it is scoped to the open Settings
//!   window so a stale flag can never lock the user out of their own shortcuts.

use super::keymap::{action, combo_from_press, display_combo};
use eframe::egui;
use scribe_core::config::{Chord, KeybindingIssue, Keybindings};
use scribe_core::Config;

/// Human labels for the rebindable actions, in display groups.
///
/// The schema is a flat struct; one undifferentiated wall of rows is unreadable,
/// so the page groups them the way a user thinks about them. `every_action_has_a_label`
/// pins this table to the schema in BOTH directions — a binding added to
/// `Keybindings` without a label here (or a label for an action that no longer
/// exists) fails the suite instead of becoming a row the user never sees.
const GROUPS: &[(&str, &[(&str, &str)])] = &[
    (
        "Files and tabs",
        &[
            (action::NEW_FILE, "New file"),
            (action::OPEN_FILE, "Open file"),
            (action::SAVE, "Save"),
            (action::SAVE_AS, "Save as…"),
            (action::CLOSE_TAB, "Close tab"),
            (action::REOPEN_TAB, "Reopen closed tab"),
            (action::NEXT_TAB, "Next tab"),
            (action::PREV_TAB, "Previous tab"),
            (action::RECENT_FILES, "Recent files"),
        ],
    ),
    (
        // Their own group: nine near-identical rows inlined above would bury
        // the seven distinct file/tab actions they sit next to.
        "Go to tab by number",
        &[
            (action::GOTO_TAB_1, "Go to tab 1"),
            (action::GOTO_TAB_2, "Go to tab 2"),
            (action::GOTO_TAB_3, "Go to tab 3"),
            (action::GOTO_TAB_4, "Go to tab 4"),
            (action::GOTO_TAB_5, "Go to tab 5"),
            (action::GOTO_TAB_6, "Go to tab 6"),
            (action::GOTO_TAB_7, "Go to tab 7"),
            (action::GOTO_TAB_8, "Go to tab 8"),
            (action::GOTO_TAB_9, "Go to tab 9"),
        ],
    ),
    (
        "Find and navigate",
        &[
            (action::FIND, "Find in this file"),
            (action::FIND_IN_FILES, "Find in files"),
            (action::REPLACE, "Find and replace"),
            (action::COMMAND_PALETTE, "Command palette"),
            (action::FUZZY_FINDER, "Fuzzy file finder"),
            (action::GOTO_LINE, "Go to line"),
            (action::GOTO_SYMBOL, "Go to symbol"),
            (action::JUMP_BRACKET, "Jump to matching bracket"),
            (action::TOGGLE_BOOKMARK, "Toggle bookmark"),
            (action::NEXT_BOOKMARK, "Next bookmark"),
            (action::PREV_BOOKMARK, "Previous bookmark"),
        ],
    ),
    (
        "Editing",
        &[
            (action::TOGGLE_COMMENT, "Toggle comment"),
            (action::MOVE_LINE_UP, "Move line up"),
            (action::MOVE_LINE_DOWN, "Move line down"),
            (action::DUPLICATE_LINE, "Duplicate line"),
            (action::JOIN_LINES, "Join lines"),
            (action::FOLD_ALL, "Fold all"),
            (action::EXPAND_ALL, "Expand all"),
        ],
    ),
    (
        "View",
        &[
            (action::TOGGLE_GRID, "Toggle note grid"),
            (action::TOGGLE_ZEN, "Toggle zen mode"),
            (action::TOGGLE_FULLSCREEN, "Toggle fullscreen"),
            (action::TOGGLE_MINIMAP, "Toggle minimap"),
            (action::TOGGLE_MD_PREVIEW, "Toggle markdown preview"),
            (action::CYCLE_THEME, "Cycle theme"),
            (action::INCREASE_FONT, "Increase font size"),
            (action::DECREASE_FONT, "Decrease font size"),
            (action::RESET_FONT, "Reset font size"),
        ],
    ),
];

/// Every row label on this page, for the settings search.
///
/// The search filters ACROSS categories, so "duplicate line" has to be able to
/// surface the Keyboard page from the Appearance page. Feeding the real row
/// labels to `section_visible` is what makes that work without a second,
/// drift-prone keyword list.
pub(crate) fn labels() -> Vec<&'static str> {
    GROUPS
        .iter()
        .flat_map(|(_, rows)| rows.iter().map(|(_, label)| *label))
        .collect()
}

/// egui temp-data key holding the action currently capturing a chord (absent ⇒
/// no capture in flight). Lives in ctx data rather than a struct field for the
/// same reason the rest of the settings pane does: [`show`] is a free function
/// that owns no state of its own.
fn capture_id() -> egui::Id {
    egui::Id::new("scr1b3_keybind_capture")
}

/// The action currently waiting for a keypress, if any.
fn capture_target(ctx: &egui::Context) -> Option<String> {
    ctx.data(|d| d.get_temp::<String>(capture_id()))
}

/// Is a chord capture in flight this frame?
///
/// `handle_keyboard_shortcuts` consults this (gated on the Settings window being
/// open) and stands down while it is true: a user pressing Ctrl+S to REBIND save
/// must not also save the file, and pressing the Escape that cancels a capture
/// must not also close every overlay in the editor.
pub(crate) fn capture_active(ctx: &egui::Context) -> bool {
    capture_target(ctx).is_some()
}

/// Abandon any in-flight capture.
///
/// Called when the Settings window closes so a capture the user walked away from
/// cannot leave the editor's shortcut layer suppressed.
pub(crate) fn clear_capture(ctx: &egui::Context) {
    ctx.data_mut(|d| d.remove::<String>(capture_id()));
}

/// What a capturing frame decided from the raw key events.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Captured {
    /// Escape — abandon the capture, leave the binding as it was.
    Cancel,
    /// Bind this combo (already in canonical `mod+alt+shift+key` form).
    Bind(String),
}

/// Read this frame's key events as a capture decision.
///
/// Pure over the event slice (rather than reaching into an `InputState`) so the
/// decision table is unit-testable without driving a UI: a frame with no key
/// press keeps waiting, Escape cancels, and anything else binds. Modifier keys
/// never arrive as [`egui::Key`] events in egui, so holding Ctrl alone simply
/// produces no event and the capture keeps waiting — which is the behaviour a
/// user expects while reaching for the second half of a chord.
fn capture_from_events(events: &[egui::Event]) -> Option<Captured> {
    events.iter().find_map(|e| match e {
        egui::Event::Key {
            key,
            pressed: true,
            modifiers,
            ..
        } => Some(if *key == egui::Key::Escape {
            Captured::Cancel
        } else {
            Captured::Bind(combo_from_press(*key, *modifiers))
        }),
        _ => None,
    })
}

/// Why a row's binding is broken, if it is.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RowIssue {
    /// No key bound at all — the action is unreachable.
    Unbound,
    /// The combo cannot be parsed into a chord.
    Unreadable,
    /// The combo parses but names a key that is not on the keyboard.
    UnknownKey,
    /// Other actions share this chord; at most one of them can fire.
    Conflict(Vec<&'static str>),
}

impl RowIssue {
    /// The warning a user reads next to the row.
    fn message(&self) -> String {
        match self {
            RowIssue::Unbound => "No key bound — this action cannot be triggered.".to_string(),
            RowIssue::Unreadable => {
                "This key combo cannot be read — the action will never fire.".to_string()
            }
            RowIssue::UnknownKey => {
                "That is not a key on your keyboard — the action will never fire.".to_string()
            }
            RowIssue::Conflict(others) => format!(
                "Conflict: the same keys are also bound to {}. Only one of them can fire.",
                others
                    .iter()
                    .map(|a| label_for(a).to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

/// Every broken row, keyed by action. Computed once per frame.
///
/// Reuses [`Keybindings::validate`] — the same check the config banner shows —
/// so Settings and the banner can never disagree about what is wrong, and adds
/// the one problem `validate` deliberately cannot see: a well-formed combo whose
/// key does not exist. `validate` lives in `scribe-core` and owns the combo
/// GRAMMAR; the key TABLE belongs to this (UI) layer.
fn issues_by_action(kb: &Keybindings) -> Vec<(&'static str, RowIssue)> {
    let mut out: Vec<(&'static str, RowIssue)> = Vec::new();
    // Unknown-key first, so it wins the row lookup over a co-occurring conflict:
    // "that key does not exist" is the more actionable of the two.
    for (name, combo) in kb.entries() {
        if Chord::parse(combo).is_some() && display_combo(combo).is_none() {
            out.push((name, RowIssue::UnknownKey));
        }
    }
    for issue in kb.validate() {
        match issue {
            KeybindingIssue::Empty { action } => out.push((action, RowIssue::Unbound)),
            KeybindingIssue::Invalid { action, .. } => out.push((action, RowIssue::Unreadable)),
            KeybindingIssue::Conflict { actions, .. } => {
                for a in &actions {
                    let others: Vec<&'static str> =
                        actions.iter().copied().filter(|o| o != a).collect();
                    out.push((a, RowIssue::Conflict(others)));
                }
            }
        }
    }
    out
}

/// The human label for `action`, or the raw action name if it somehow has none
/// (`every_action_has_a_label` makes that unreachable for real bindings).
fn label_for(action: &str) -> &str {
    GROUPS
        .iter()
        .flat_map(|(_, rows)| rows.iter())
        .find(|(a, _)| *a == action)
        .map_or(action, |(_, label)| *label)
}

/// What a row's chord button reads.
///
/// `None` for the stored combo means the matcher cannot fire it, so the button
/// says so rather than rendering a chord that does nothing.
fn chord_button_text(combo: &str, capturing: bool) -> String {
    if capturing {
        return "press keys…".to_string();
    }
    display_combo(combo).unwrap_or_else(|| {
        if combo.trim().is_empty() {
            "unbound".to_string()
        } else {
            format!("{combo} (won't fire)")
        }
    })
}

/// Render the Keyboard settings page. Returns `true` when a binding changed, so
/// the host persists (`save_config`) exactly like every other settings page.
pub(crate) fn show(ui: &mut egui::Ui, config: &mut Config, q: &str) -> bool {
    let mut changed = false;
    let capturing = capture_target(ui.ctx());
    let warn = ui.visuals().warn_fg_color;
    let muted = ui.visuals().weak_text_color();

    // A capture in flight consumes this frame's keys. Read BEFORE the rows so the
    // new chord renders on the same frame it was pressed.
    if let Some(target) = capturing.clone() {
        if let Some(decision) = ui.input(|i| capture_from_events(&i.events)) {
            match decision {
                Captured::Cancel => {}
                Captured::Bind(combo) => {
                    changed |= config.keybindings.set(&target, &combo);
                }
            }
            clear_capture(ui.ctx());
        }
        // Deliberately NO `request_repaint` here. A keypress is itself an input
        // event, so egui already repaints on the frame the chord lands — polling
        // for it would spin the UI forever (the window would never reach a
        // steady state) to buy nothing.
    }

    let defaults = Keybindings::default();
    let issues = issues_by_action(&config.keybindings);

    ui.horizontal(|ui| {
        let differs = config.keybindings != defaults;
        if ui
            .add_enabled(differs, egui::Button::new("Restore all defaults"))
            .on_hover_text(if differs {
                "Put every shortcut back to the key it ships with."
            } else {
                "Every shortcut is already the one it ships with."
            })
            .clicked()
        {
            config.keybindings = defaults.clone();
            clear_capture(ui.ctx());
            changed = true;
        }
        ui.label(
            egui::RichText::new("Changes apply immediately — no restart.")
                .color(muted)
                .small(),
        );
    });

    // Every problem in one banner, so a conflict is visible even when the two
    // colliding rows are in different groups (or filtered out by the search).
    if !issues.is_empty() {
        ui.add_space(6.0);
        for message in dedup_messages(&issues) {
            ui.label(egui::RichText::new(format!("⚠ {message}")).color(warn));
        }
    }

    // Each group renders its OWN `Grid`, and a Grid auto-sizes column 0 to the
    // widest label IT contains. With a per-group `min_col_width` the chord
    // buttons and the unbind/restore icons therefore started at a DIFFERENT x in
    // every group — "Files and tabs" (widest: "Reopen closed tab") sat visibly
    // left of "Find and navigate" (widest: "Jump to matching bracket"), so the
    // page read as four misaligned tables rather than one. Measuring the widest
    // label across EVERY visible row and using it as the shared floor makes all
    // groups share one column grid. Found by rendering the page and looking at
    // it — no unit test can see a column that is 40 px off.
    let label_col = label_column_width(ui, q);
    for (group, rows) in GROUPS {
        let visible: Vec<&(&str, &str)> = rows
            .iter()
            .filter(|(_, label)| crate::settings::row_visible(q, label))
            .collect();
        if visible.is_empty() {
            continue;
        }
        ui.add_space(10.0);
        ui.label(egui::RichText::new(*group).strong());
        ui.separator();
        egui::Grid::new(format!("settings-keys-{group}"))
            .num_columns(4)
            .spacing([16.0, 8.0])
            .min_col_width(label_col)
            .show(ui, |ui| {
                for (act, label) in visible {
                    changed |= binding_row(ui, config, &defaults, &issues, act, label, warn);
                }
            });
    }
    changed
}

/// Width of the label column, shared by every group's grid.
///
/// The widest CURRENTLY-VISIBLE row label, measured in the live font, plus a
/// small gutter — floored at the previous constant so a narrow search result
/// ("Save") cannot collapse the page into a cramped strip.
fn label_column_width(ui: &egui::Ui, q: &str) -> f32 {
    const FLOOR: f32 = 150.0;
    const GUTTER: f32 = 12.0;
    let font = egui::TextStyle::Body.resolve(ui.style());
    let widest = GROUPS
        .iter()
        .flat_map(|(_, rows)| rows.iter())
        .filter(|(_, label)| crate::settings::row_visible(q, label))
        .map(|(_, label)| {
            ui.painter()
                .layout_no_wrap((*label).to_string(), font.clone(), egui::Color32::WHITE)
                .size()
                .x
        })
        .fold(0.0_f32, f32::max);
    (widest + GUTTER).max(FLOOR)
}

/// The issue messages to show in the banner, each once. A conflict yields one
/// message per participating action; the banner needs it said once.
fn dedup_messages(issues: &[(&'static str, RowIssue)]) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for (act, issue) in issues {
        let msg = format!("{}: {}", label_for(act), issue.message());
        let already = match issue {
            RowIssue::Conflict(others) => others.iter().any(|o| {
                seen.iter()
                    .any(|s| s.starts_with(&format!("{}: ", label_for(o))))
            }),
            _ => false,
        };
        if !already && !seen.contains(&msg) {
            seen.push(msg);
        }
    }
    seen
}

/// One binding row: label, click-to-capture chord button, unbind, restore.
#[allow(clippy::too_many_arguments)]
fn binding_row(
    ui: &mut egui::Ui,
    config: &mut Config,
    defaults: &Keybindings,
    issues: &[(&'static str, RowIssue)],
    act: &str,
    label: &str,
    warn: egui::Color32,
) -> bool {
    let mut changed = false;
    let capturing = capture_target(ui.ctx()).as_deref() == Some(act);
    let combo = config.keybindings.get(act).unwrap_or("").to_string();
    let default_combo = defaults.get(act).unwrap_or("").to_string();
    let issue = issues.iter().find(|(a, _)| *a == act).map(|(_, i)| i);

    ui.label(label);

    ui.horizontal(|ui| {
        let text = chord_button_text(&combo, capturing);
        let mut rich = egui::RichText::new(text).monospace();
        if issue.is_some() {
            rich = rich.color(warn);
        }
        if ui
            .button(rich)
            .on_hover_text(
                "Click, then press the keys you want. Escape cancels; the change \
                 applies immediately.",
            )
            .clicked()
        {
            ui.ctx()
                .data_mut(|d| d.insert_temp(capture_id(), act.to_string()));
        }
        if let Some(issue) = issue {
            ui.label(egui::RichText::new("⚠").color(warn))
                .on_hover_text(issue.message());
        }
    });

    // Unbind: an explicit, visible "this action has no key" state. Blank is a
    // legitimate choice, and `validate` surfaces it rather than hiding it.
    let bound = !combo.trim().is_empty();
    if ui
        .add_enabled(
            bound,
            egui::Button::new(egui::RichText::new("✕").small()).frame(false),
        )
        .on_hover_text(if bound {
            "Remove this shortcut (the action keeps working from menus)."
        } else {
            "Already unbound"
        })
        .clicked()
    {
        changed |= config.keybindings.set(act, "");
    }

    let differs = combo != default_combo;
    if ui
        .add_enabled(
            differs,
            egui::Button::new(egui::RichText::new("↺").small()).frame(false),
        )
        .on_hover_text(if differs {
            "Restore this shortcut's default key."
        } else {
            "Already default"
        })
        .clicked()
    {
        changed |= config.keybindings.set(act, &default_combo);
    }
    ui.end_row();
    changed
}

#[cfg(test)]
mod tests;
