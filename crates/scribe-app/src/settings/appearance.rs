//! Theme, window chrome, accessibility zoom, and toolbar look.
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
        "Appearance",
        &["theme", "follow os", "frameless", "toolbar icons"],
    ) {
        head(
            ui,
            "Appearance",
            "Theme, window chrome, and toolbar look. Changes apply live.",
        );
        settings_grid(ui, "settings-appearance", |ui| {
            if row_visible(q, "theme") {
                // Phase 17 T17.2: theme picker over the built-ins + a free text
                // field for user themes under <config_dir>/themes/<name>.toml.
                ui.label("Theme").on_hover_text(
                    "Pick the active colour theme from the built-ins, or type a user theme \
                     name below. Changes apply live.",
                );
                ui.horizontal(|ui| {
                    let names = scribe_core::theme::Theme::builtin_names();
                    // Prev/next arrows that cycle the built-in themes IN PLACE
                    // (no dropdown open). They are kept STATIONARY by pinning the
                    // ComboBox between them to a FIXED width, so the arrows never
                    // shift as the selected theme name changes length. Phosphor
                    // carets render from the icon atlas (a raw "<"/">" glyph can
                    // tofu in some UI fonts). `rem_euclid` wraps around the list;
                    // a custom (non-built-in) theme name steps onto the first/last
                    // built-in so the arrows always have a defined landing spot
                    // (mirrors C0PL4ND's theme step arrows).
                    let step = |config: &mut Config, delta: isize| {
                        let next = step_theme_index(names, &config.appearance.theme, delta);
                        config.appearance.theme = names[next].to_string();
                        // Parity with the dropdown path below — the arrows are
                        // the same explicit pick by another control.
                        take_theme_ownership(config);
                    };
                    if ui
                        .add(egui::Button::new(egui_phosphor::thin::CARET_LEFT))
                        .on_hover_text("Previous theme")
                        .clicked()
                    {
                        step(config, -1);
                        changed = true;
                    }
                    egui::ComboBox::from_id_salt("theme-picker")
                        // Fixed width => the flanking arrows are stationary and a
                        // long theme name is clipped inside the box rather than
                        // pushing the "next" arrow to the right.
                        .width(168.0)
                        .selected_text(config.appearance.theme.clone())
                        .show_ui(ui, |ui| {
                            for name in names {
                                let picked = ui
                                    .selectable_value(
                                        &mut config.appearance.theme,
                                        (*name).to_string(),
                                        *name,
                                    )
                                    .changed();
                                if picked {
                                    take_theme_ownership(config);
                                    changed = true;
                                }
                            }
                        })
                        .response
                        .on_hover_text(
                            "Choose a built-in colour theme, or use the arrows to cycle.",
                        );
                    if ui
                        .add(egui::Button::new(egui_phosphor::thin::CARET_RIGHT))
                        .on_hover_text("Next theme")
                        .clicked()
                    {
                        step(config, 1);
                        changed = true;
                    }
                });
                changed |=
                    reset_to_default(ui, &mut config.appearance.theme, &def.appearance.theme);
                ui.end_row();
            }
            if row_visible(q, "theme custom name") {
                ui.label("…or user theme name");
                let name_changed = ui
                    .text_edit_singleline(&mut config.appearance.theme)
                    .on_hover_text(
                        "If a TOML at <config_dir>/themes/<name>.toml exists it overrides the \
                         built-in; otherwise the built-in by the same name (or wired-noir) is used.",
                    )
                    .changed();
                if name_changed {
                    // Naming a user theme is as explicit a pick as choosing a
                    // built-in, and the OS-follow substitution silences it just
                    // as completely — so it takes ownership on the same terms.
                    take_theme_ownership(config);
                    changed = true;
                }
                ui.end_row();
            }
            if row_visible(q, "ui scale zoom accessibility a11y") {
                // M6 — whole-app accessibility zoom, applied via ctx.set_zoom_factor
                // once per frame. Clamped to 0.5..=3.0 with a NaN/inf guard by
                // Config::effective_ui_scale so a wild value can't blank the window.
                ui.label("UI scale").on_hover_text(
                    "Zoom the entire interface — text, chrome, and controls — for readability. \
                     1.0 is the standard size; the range is 0.5× to 3×.",
                );
                changed |= stepped_slider(ui, true, &mut config.ui_scale, 0.5..=3.0, 0.1);
                changed |= reset_to_default(ui, &mut config.ui_scale, &def.ui_scale);
                ui.end_row();
            }
            if row_visible(q, "background colour color app override") {
                // #88 — app background colour, independent of the theme.
                ui.label("App background").on_hover_text(
                    "Override the app background colour independently of the theme. Switching \
                     themes resets this to the new theme's background.",
                );
                ui.horizontal(|ui| {
                    let mut col = config
                        .appearance
                        .background_override
                        .as_deref()
                        .and_then(parse_hex_color)
                        .unwrap_or(egui::Color32::from_rgb(0x0d, 0x0b, 0x14));
                    if ui.color_edit_button_srgba(&mut col).changed() {
                        config.appearance.background_override =
                            Some(format!("#{:02x}{:02x}{:02x}", col.r(), col.g(), col.b()));
                        changed = true;
                    }
                    if config.appearance.background_override.is_some()
                        && ui
                            .small_button("Follow theme")
                            .on_hover_text("Clear the override; follow the theme's background.")
                            .clicked()
                    {
                        config.appearance.background_override = None;
                        changed = true;
                    }
                });
                ui.end_row();
            }
            // #106 — link toggle + the note (editor well) background.
            changed |= grid_bool(
                ui,
                q,
                "note background link app editor separate together",
                "Link app & note backgrounds",
                "ON: the note (editor) background follows the app background — one control \
                 changes both. OFF: set the note background separately below.",
                &mut config.appearance.link_backgrounds,
                &def.appearance.link_backgrounds,
            );
            if row_visible(q, "note background link app editor separate together") {
                let linked = config.appearance.link_backgrounds;
                ui.label("Note background").on_hover_text(
                    "Background colour of the note/editor text area (used when 'Link app & \
                     note backgrounds' is off).",
                );
                ui.add_enabled_ui(!linked, |ui| {
                    ui.horizontal(|ui| {
                        let mut col = config
                            .appearance
                            .note_background_override
                            .as_deref()
                            .and_then(parse_hex_color)
                            .unwrap_or(egui::Color32::from_rgb(0x0d, 0x0b, 0x14));
                        if ui.color_edit_button_srgba(&mut col).changed() {
                            config.appearance.note_background_override =
                                Some(format!("#{:02x}{:02x}{:02x}", col.r(), col.g(), col.b()));
                            changed = true;
                        }
                        if config.appearance.note_background_override.is_some()
                            && ui
                                .small_button("Follow theme")
                                .on_hover_text("Clear the note override; follow the theme.")
                                .clicked()
                        {
                            config.appearance.note_background_override = None;
                            changed = true;
                        }
                    });
                });
                ui.end_row();
            }
            changed |= grid_bool(
                ui,
                q,
                "follow os dark light",
                "Follow OS dark/light",
                "Automatically switch between a light and dark theme to match the operating \
                 system's appearance setting.",
                &mut config.appearance.follow_os_theme,
                &def.appearance.follow_os_theme,
            );
            changed |= grid_bool(
                ui,
                q,
                "frameless window",
                "Frameless window (restart to apply)",
                "Draw the window without the OS title bar (a custom in-app title bar is used). \
                 Known Windows limitation: with a glass/mica/vibrancy backdrop the DWM can re-add \
                 the native min/max/close buttons over the custom title bar (a doubled caption). \
                 If you see that, turn frameless OFF — the native frame composes cleanly with the \
                 backdrop.",
                &mut config.appearance.frameless,
                &def.appearance.frameless,
            );
            changed |= grid_bool(
                ui,
                q,
                "toolbar in titlebar compact chrome",
                "Toolbar in the title bar",
                "Move the quick-access toolbar into the custom title bar (between the app name and \
                 the window buttons) and hide the separate toolbar row — a compact single-row \
                 chrome. Requires the frameless window.",
                &mut config.appearance.toolbar_in_titlebar,
                &def.appearance.toolbar_in_titlebar,
            );
            changed |= grid_bool(
                ui,
                q,
                "status bar bottom show hide",
                "Show the bottom status bar",
                "Show the status bar along the bottom of the window (cursor position, encoding, \
                 line endings, spellcheck and diagnostics counts). Turn it off for a more \
                 distraction-free editing surface without entering full zen mode.",
                &mut config.appearance.show_status_bar,
                &def.appearance.show_status_bar,
            );
            changed |= grid_bool(
                ui,
                q,
                "toolbar icons words phosphor",
                "Toolbar shows icons instead of words",
                "When off, the quick-access toolbar renders text labels (the default). When on, \
                 items render as Phosphor Thin icon glyphs — compact, brand-aligned.",
                &mut config.appearance.toolbar_icons,
                &def.appearance.toolbar_icons,
            );
            changed |= grid_bool(
                ui,
                q,
                "kanji jp glyph japanese instrument label",
                "Toolbar — show kanji instrument labels",
                "Adds a small, dim kanji to each toolbar action whose canonical Japanese term \
                 is verified (e.g. New=新, Save=保, Find=検). English-redundant — the kanji \
                 never replaces the label.",
                &mut config.appearance.jp_glyph_labels,
                &def.appearance.jp_glyph_labels,
            );
        });
        // Full in-app theme creator/editor: seeds from the active theme, live
        // colour pickers grouped by UI/Syntax with a live preview, then Save
        // writes an editable user theme TOML and switches to it. Supersedes the
        // old export-button + hidden colour-list flow.
        if row_visible(
            q,
            "theme create edit customize export palette colour color user editor",
        ) {
            // #10 — a separator + heading delimits the toolbar toggles above from
            // the theme-creator section below, so the colour editor reads as its
            // own distinct block rather than running on from the toggle list.
            ui.separator();
            ui.label(egui::RichText::new("Create / edit a theme").strong());
            ui.add_space(4.0);
            changed |= crate::theme_editor::show(ui, config);
        }
        space(ui);
    }
    changed
}
