//! Typeface, size and spacing, and markdown colouring.
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
        "Fonts",
        &["size", "line height", "family", "font theme"],
    ) {
        head(
            ui,
            "Fonts",
            "Editor font family, text size, and line spacing. (Ligatures are off — \
             the renderer does no OpenType shaping.)",
        );
        // -- Typeface --
        group(
            ui,
            "Typeface",
            "Fonts for the editor text and the app interface.",
        );
        ui.add_space(4.0);
        settings_grid(ui, "settings-fonts-typeface", |ui| {
            if row_visible(q, "font family theme editor note") {
                ui.label("Note font")
                    .on_hover_text("Font for the note/editor text. Applies live, no restart.");
                let fams = crate::app::FONT_FAMILIES;
                let cur = fams
                    .iter()
                    .position(|(d, _)| *d == config.fonts.editor_family.as_str());
                let sel = config.fonts.editor_family.clone();
                changed |= stepper_combo(
                    ui,
                    "note-font-picker",
                    168.0,
                    "note font",
                    fams.len(),
                    cur,
                    &sel,
                    |i| fams[i].0.to_string(),
                    |i| config.fonts.editor_family = fams[i].0.to_string(),
                );
                changed |= reset_to_default(
                    ui,
                    &mut config.fonts.editor_family,
                    &def.fonts.editor_family,
                );
                ui.end_row();
            }
            if row_visible(q, "ui font app interface family") {
                ui.label("App UI font").on_hover_text(
                    "Font for the app interface (toolbar, settings, status). 'System default' \
                     keeps the built-in UI font. Applies live.",
                );
                let fams = crate::app::FONT_FAMILIES;
                let cur = if config.fonts.ui_family == "System default" {
                    Some(0)
                } else {
                    fams.iter()
                        .position(|(d, _)| *d == config.fonts.ui_family.as_str())
                        .map(|i| i + 1)
                };
                let sel = config.fonts.ui_family.clone();
                changed |= stepper_combo(
                    ui,
                    "ui-font-picker",
                    168.0,
                    "app UI font",
                    fams.len() + 1,
                    cur,
                    &sel,
                    |i| {
                        if i == 0 {
                            "System default".to_string()
                        } else {
                            fams[i - 1].0.to_string()
                        }
                    },
                    |i| {
                        config.fonts.ui_family = if i == 0 {
                            "System default".to_string()
                        } else {
                            fams[i - 1].0.to_string()
                        };
                    },
                );
                changed |= reset_to_default(ui, &mut config.fonts.ui_family, &def.fonts.ui_family);
                ui.end_row();
            }
            if row_visible(q, "note colour color theme syntax text") {
                ui.label("Note colour theme").on_hover_text(
                    "Colour scheme for the note text / syntax highlighting, separate from the \
                     app theme. Applies live.",
                );
                let themes = crate::app::NOTE_THEMES;
                let cur = themes
                    .iter()
                    .position(|n| *n == config.editor.note_theme.as_str());
                let sel = config.editor.note_theme.clone();
                changed |= stepper_combo(
                    ui,
                    "note-theme-picker",
                    168.0,
                    "note colour theme",
                    themes.len(),
                    cur,
                    &sel,
                    |i| themes[i].to_string(),
                    |i| config.editor.note_theme = themes[i].to_string(),
                );
                changed |=
                    reset_to_default(ui, &mut config.editor.note_theme, &def.editor.note_theme);
                ui.end_row();
            }
        });
        ui.add_space(6.0);

        // -- Size & spacing --
        group(ui, "Size & spacing", "Editor text size and line spacing.");
        ui.add_space(4.0);
        settings_grid(ui, "settings-fonts-size", |ui| {
            if row_visible(q, "editor size") {
                ui.label("Size")
                    .on_hover_text("Font size of the editor text, in points.");
                ui.horizontal(|ui| {
                    if ui.small_button("-").on_hover_text("Smaller").clicked() {
                        config.fonts.editor_size =
                            (config.fonts.editor_size - 1.0).clamp(8.0, 32.0);
                        changed = true;
                    }
                    changed |= ui
                        .add(egui::Slider::new(&mut config.fonts.editor_size, 8.0..=32.0))
                        .changed();
                    if ui.small_button("+").on_hover_text("Larger").clicked() {
                        config.fonts.editor_size =
                            (config.fonts.editor_size + 1.0).clamp(8.0, 32.0);
                        changed = true;
                    }
                });
                changed |=
                    reset_to_default(ui, &mut config.fonts.editor_size, &def.fonts.editor_size);
                ui.end_row();
            }
            if row_visible(q, "line height") {
                ui.label("Line height").on_hover_text(
                    "Vertical spacing between lines, as a multiple of the font size. Note: the \
                     text caret + selection are exactly this tall, so a larger value also makes \
                     them taller than the glyphs. ~1.2 keeps the caret tight to the text.",
                );
                ui.horizontal(|ui| {
                    if ui.small_button("-").on_hover_text("Tighter").clicked() {
                        config.fonts.line_height = (config.fonts.line_height - 0.1).clamp(1.0, 2.5);
                        changed = true;
                    }
                    changed |= ui
                        .add(egui::Slider::new(&mut config.fonts.line_height, 1.0..=2.5))
                        .changed();
                    if ui.small_button("+").on_hover_text("Looser").clicked() {
                        config.fonts.line_height = (config.fonts.line_height + 0.1).clamp(1.0, 2.5);
                        changed = true;
                    }
                });
                changed |=
                    reset_to_default(ui, &mut config.fonts.line_height, &def.fonts.line_height);
                ui.end_row();
            }
        });
        ui.add_space(6.0);

        // -- Markdown colouring --
        group(
            ui,
            "Markdown colouring",
            "Colour extra note tokens the syntax grammar leaves plain.",
        );
        ui.add_space(4.0);
        settings_grid(ui, "settings-fonts-markdown", |ui| {
            changed |= grid_bool(
                ui,
                q,
                "rich markdown note colouring tokens master switch",
                "Rich markdown colouring",
                "Colour extra note tokens the syntax grammar leaves plain — divider \
                 lines, #tags, ~~strikethrough~~, task boxes and table pipes. The \
                 per-token switches below tune individual passes; turn this off to \
                 disable them all at once.",
                &mut config.editor.md_rich_coloring,
                &def.editor.md_rich_coloring,
            );
            changed |= grid_bool(
                ui,
                q,
                "markdown colour divider lines separators rules setext",
                "  • Divider lines (----  ====//)",
                "Colour decorative divider lines: ----, ====//====//, * * *, setext \
                 underlines, and box-drawing rules. Active when Rich markdown \
                 colouring is on.",
                &mut config.editor.md_color_dividers,
                &def.editor.md_color_dividers,
            );
            changed |= grid_bool(
                ui,
                q,
                "markdown colour hashtags tags",
                "  • #tags",
                "Colour #tag tokens in the editor. Active when Rich markdown \
                 colouring is on.",
                &mut config.editor.md_color_tags,
                &def.editor.md_color_tags,
            );
            changed |= grid_bool(
                ui,
                q,
                "markdown colour strikethrough",
                "  • ~~strikethrough~~",
                "Colour ~~strikethrough~~ spans. Active when Rich markdown \
                 colouring is on.",
                &mut config.editor.md_color_strikethrough,
                &def.editor.md_color_strikethrough,
            );
            changed |= grid_bool(
                ui,
                q,
                "markdown colour task boxes checkboxes",
                "  • Task boxes [ ] [x]",
                "Colour GFM task checkboxes at the start of a list item. Active when \
                 Rich markdown colouring is on.",
                &mut config.editor.md_color_task_boxes,
                &def.editor.md_color_task_boxes,
            );
            changed |= grid_bool(
                ui,
                q,
                "markdown colour table pipes cell separators",
                "  • Table pipes |",
                "Colour the | cell separators in table rows. Active when Rich \
                 markdown colouring is on.",
                &mut config.editor.md_color_table_pipes,
                &def.editor.md_color_table_pipes,
            );
        });
        space(ui);
    }
    changed
}
