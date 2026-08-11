//! Spellcheck language and dictionary behaviour.
//!
//! Lifted verbatim out of `super::render_sections`, which had grown to
//! ~2,300 lines of eleven independent category pages. The body is
//! unchanged: it sat at this same indent level inside the parent
//! function, so the move needed no reindentation and no rewrite.

use super::*;

pub(super) fn render(
    ui: &mut egui::Ui,
    config: &mut Config,
    def: &Config,
    sel: &str,
    q: &str,
) -> bool {
    let mut changed = false;
    if section_visible(
        sel,
        q,
        "Spellcheck",
        &[
            "spellcheck",
            "language",
            "comments",
            "strings",
            "identifiers",
        ],
    ) {
        head(
            ui,
            "Spellcheck (offline)",
            "Dictionary spellchecking that runs entirely on-device — no network.",
        );
        settings_grid(ui, "settings-spellcheck", |ui| {
            changed |= grid_bool(
                ui,
                q,
                "spellcheck enable",
                "Enable",
                "Turn on the offline spell checker for editor text.",
                &mut config.spellcheck.enabled,
                &def.spellcheck.enabled,
            );
            let on = config.spellcheck.enabled;
            if row_visible(q, "spellcheck language dictionary") {
                ui.label("Language").on_hover_text(
                    "Dictionary language code (e.g. en_US). en_US is built in; for any other \
                     code, drop a matching <code>.txt word list in the config dict/ folder.",
                );
                ui.add_enabled_ui(on, |ui| {
                    changed |= ui
                        .text_edit_singleline(&mut config.spellcheck.language)
                        .on_hover_text(
                            "Dictionary language code. en_US ships built in. For another \
                             language, place <code>.txt (one word per line) in the dict/ folder \
                             of your config directory; it is loaded automatically.",
                        )
                        .changed();
                });
                changed |= reset_to_default(
                    ui,
                    &mut config.spellcheck.language,
                    &def.spellcheck.language,
                );
                ui.end_row();
            }
            if row_visible(q, "spellcheck check comments") {
                ui.add_enabled_ui(on, |ui| {
                    changed |= ui
                        .checkbox(&mut config.spellcheck.check_comments, "Check comments")
                        .on_hover_text("Spell-check words inside code comments.")
                        .changed();
                });
                ui.label("");
                changed |= reset_to_default(
                    ui,
                    &mut config.spellcheck.check_comments,
                    &def.spellcheck.check_comments,
                );
                ui.end_row();
            }
            if row_visible(q, "spellcheck check strings") {
                ui.add_enabled_ui(on, |ui| {
                    changed |= ui
                        .checkbox(&mut config.spellcheck.check_strings, "Check strings")
                        .on_hover_text("Spell-check words inside string literals.")
                        .changed();
                });
                ui.label("");
                changed |= reset_to_default(
                    ui,
                    &mut config.spellcheck.check_strings,
                    &def.spellcheck.check_strings,
                );
                ui.end_row();
            }
            if row_visible(q, "spellcheck check identifiers") {
                ui.add_enabled_ui(on, |ui| {
                    changed |= ui
                        .checkbox(
                            &mut config.spellcheck.check_identifiers,
                            "Check identifiers",
                        )
                        .on_hover_text(
                            "Spell-check variable and function names (splits camelCase / \
                             snake_case).",
                        )
                        .changed();
                });
                ui.label("");
                changed |= reset_to_default(
                    ui,
                    &mut config.spellcheck.check_identifiers,
                    &def.spellcheck.check_identifiers,
                );
                ui.end_row();
            }
            if row_visible(q, "spellcheck custom dictionary word list") {
                ui.label("Custom dictionary").on_hover_text(
                    "Optional path to your own word list; every word in it is always treated \
                     as correct (layered on top of the base dictionary).",
                );
                ui.add_enabled_ui(on, |ui| {
                    let mut s = config
                        .spellcheck
                        .custom_dict_path
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default();
                    if ui
                        .text_edit_singleline(&mut s)
                        .on_hover_text(
                            "Absolute path to a newline-separated .txt word list (one word per \
                             line). Leave empty for none.",
                        )
                        .changed()
                    {
                        config.spellcheck.custom_dict_path = if s.trim().is_empty() {
                            None
                        } else {
                            Some(std::path::PathBuf::from(s.trim()))
                        };
                        changed = true;
                    }
                });
                changed |= reset_to_default(
                    ui,
                    &mut config.spellcheck.custom_dict_path,
                    &def.spellcheck.custom_dict_path,
                );
                ui.end_row();
            }
        });
        space(ui);
    }
    changed
}
