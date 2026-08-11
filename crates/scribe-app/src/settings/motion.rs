//! Animation, CRT screen, ambient node-mesh, tape accents, and caret motion.
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
        "Motion",
        &["motion", "animation", "blink", "fade", "cursor"],
    ) {
        head(
            ui,
            "Motion",
            "Subtle interface animation. Turn off for a fully static UI.",
        );
        // Master OFF by default — calm-surface principle (DECISION-2026-005);
        // animation is opt-in so idle frames cost the same as plain egui.
        // -- Animation --
        group(
            ui,
            "Animation",
            "Master switch, and the speed of the editor's chrome transitions.",
        );
        ui.add_space(4.0);
        settings_grid(ui, "settings-motion-master", |ui| {
            changed |= grid_bool(
                ui,
                q,
                "motion animation enable",
                "Enable animations",
                "Master switch. When off, transitions are instant (no fades) and the text \
                 caret stays steady — idle frames cost the same as plain egui.",
                &mut config.motion.enabled,
                &def.motion.enabled,
            );
            let on = config.motion.enabled;
            if row_visible(q, "motion animation speed intensity ui transition") {
                ui.label("UI transition speed").on_hover_text(
                    "Scale how long the editor's CHROME transitions take — hover fades, panel \
                     and collapsible expand/collapse, combobox/menu fades, and value-change \
                     lerps. 0 makes every transition instant; 1 is egui's full transition time \
                     and 2 is double that. This does NOT control the retro visual effects \
                     (flicker / VHS / mesh) — those have their own per-effect speed sliders below.",
                );
                changed |= stepped_slider(ui, on, &mut config.motion.intensity, 0.0..=2.0, 0.1);
                changed |=
                    reset_to_default(ui, &mut config.motion.intensity, &def.motion.intensity);
                ui.end_row();
            }
        });
        ui.add_space(6.0);

        // `on` gates every effect below in lock-step with the master toggle,
        // read once here so each group's grid closure can capture it.
        let on = config.motion.enabled;

        // -- CRT screen --
        group(
            ui,
            "CRT screen",
            "Scanlines, flicker, and the boot glitch over the whole surface.",
        );
        ui.add_space(4.0);
        settings_grid(ui, "settings-motion-crt", |ui| {
            if row_visible(q, "crt scanlines retro motion effect") {
                ui.add_enabled_ui(on, |ui| {
                    changed |= ui
                        .checkbox(&mut config.motion.crt_scanlines, "CRT scanlines")
                        .on_hover_text(
                            "Draw subtle drifting horizontal scanlines over the editor for a \
                             retro CRT look (a calm animated post-effect).",
                        )
                        .changed();
                });
                ui.label("");
                changed |= reset_to_default(
                    ui,
                    &mut config.motion.crt_scanlines,
                    &def.motion.crt_scanlines,
                );
                ui.end_row();
            }
            if row_visible(q, "scanline darkness strength") {
                ui.label("Scanline darkness").on_hover_text(
                    "How dark the CRT scanlines are — 0 is invisible, 1 is strong dark bands.",
                );
                changed |= stepped_slider(
                    ui,
                    on && config.motion.crt_scanlines,
                    &mut config.motion.scanline_darkness,
                    0.0..=1.0,
                    0.1,
                );
                changed |= reset_to_default(
                    ui,
                    &mut config.motion.scanline_darkness,
                    &def.motion.scanline_darkness,
                );
                ui.end_row();
            }
            if row_visible(q, "screen flicker motion effect") {
                ui.add_enabled_ui(on, |ui| {
                    changed |= ui
                        .checkbox(&mut config.motion.flicker, "Screen flicker")
                        .on_hover_text("Subtle CRT-style brightness flicker over the whole window.")
                        .changed();
                });
                ui.label("");
                changed |= reset_to_default(ui, &mut config.motion.flicker, &def.motion.flicker);
                ui.end_row();
            }
            if row_visible(q, "flicker strength motion") {
                ui.label("Flicker strength")
                    .on_hover_text("How strong the screen flicker is (capped low for comfort).");
                changed |= stepped_slider(
                    ui,
                    on && config.motion.flicker,
                    &mut config.motion.flicker_strength,
                    0.0..=0.20,
                    0.01,
                );
                changed |= reset_to_default(
                    ui,
                    &mut config.motion.flicker_strength,
                    &def.motion.flicker_strength,
                );
                ui.end_row();
            }
            if row_visible(q, "flicker speed cadence motion") {
                ui.label("Flicker speed").on_hover_text(
                    "How fast the screen flicker pulses. 1 is the standard cadence; lower is a \
                     slower shimmer and higher flickers faster. Independent of the strength.",
                );
                changed |= stepped_slider(
                    ui,
                    on && config.motion.flicker,
                    &mut config.motion.flicker_speed,
                    0.25..=3.0,
                    0.1,
                );
                changed |= reset_to_default(
                    ui,
                    &mut config.motion.flicker_speed,
                    &def.motion.flicker_speed,
                );
                ui.end_row();
            }
            if row_visible(q, "boot glitch startup motion effect") {
                ui.add_enabled_ui(on, |ui| {
                    changed |= ui
                        .checkbox(&mut config.motion.boot_glitch, "Boot glitch")
                        .on_hover_text(
                            "A one-shot glitch sweep plays for a moment when the app launches.",
                        )
                        .changed();
                });
                ui.label("");
                changed |=
                    reset_to_default(ui, &mut config.motion.boot_glitch, &def.motion.boot_glitch);
                ui.end_row();
            }
        });
        ui.add_space(6.0);

        // -- Ambient node-mesh --
        group(
            ui,
            "Ambient node-mesh",
            "The drifting Wired node-mesh background lattice.",
        );
        ui.add_space(4.0);
        settings_grid(ui, "settings-motion-mesh", |ui| {
            if row_visible(q, "wired node mesh ambient background motion") {
                ui.add_enabled_ui(on, |ui| {
                    changed |= ui
                        .checkbox(
                            &mut config.motion.wired_ambient,
                            "Wired node-mesh background",
                        )
                        .on_hover_text(
                            "Draw an animated node-mesh ambient background behind the editor.",
                        )
                        .changed();
                });
                ui.label("");
                changed |= reset_to_default(
                    ui,
                    &mut config.motion.wired_ambient,
                    &def.motion.wired_ambient,
                );
                ui.end_row();
            }
            if row_visible(q, "node mesh density motion") {
                ui.label("Mesh density").on_hover_text(
                    "How many nodes the wired-mesh background draws (sparse to dense). \
                     Higher values scale the node count with the window size.",
                );
                changed |= stepped_slider(
                    ui,
                    on && config.motion.wired_ambient,
                    &mut config.motion.mesh_density,
                    0.0..=2.0,
                    0.1,
                );
                changed |= reset_to_default(
                    ui,
                    &mut config.motion.mesh_density,
                    &def.motion.mesh_density,
                );
                ui.end_row();
            }
            if row_visible(q, "node mesh brightness motion") {
                ui.label("Mesh brightness").on_hover_text(
                    "How bright the wired-mesh lines and nodes are. 1 is the standard look; \
                     lower dims the lattice toward invisible, higher makes it pop.",
                );
                changed |= stepped_slider(
                    ui,
                    on && config.motion.wired_ambient,
                    &mut config.motion.mesh_brightness,
                    0.0..=3.0,
                    0.1,
                );
                changed |= reset_to_default(
                    ui,
                    &mut config.motion.mesh_brightness,
                    &def.motion.mesh_brightness,
                );
                ui.end_row();
            }
            if row_visible(q, "mesh drift speed motion") {
                ui.label("Mesh drift speed").on_hover_text(
                    "How fast the wired-mesh nodes drift. 1 is the standard rate; lower is a \
                     slower, calmer breathe and higher makes the lattice shift faster.",
                );
                changed |= stepped_slider(
                    ui,
                    on && config.motion.wired_ambient,
                    &mut config.motion.mesh_drift_speed,
                    0.25..=3.0,
                    0.1,
                );
                changed |= reset_to_default(
                    ui,
                    &mut config.motion.mesh_drift_speed,
                    &def.motion.mesh_drift_speed,
                );
                ui.end_row();
            }
            if row_visible(q, "node mesh colour color theme motion") {
                ui.label("Mesh colour").on_hover_text(
                    "Colour of the wired-mesh nodes and links. By default it follows the \
                     active theme's accent; pick a colour to pin it, then use 'Reset to \
                     theme' to go back to following the theme.",
                );
                ui.add_enabled_ui(on && config.motion.wired_ambient, |ui| {
                    ui.horizontal(|ui| {
                        // Seed the picker with the active theme's accent so the
                        // "follow theme" state shows the colour it will actually
                        // paint. Resolving the built-in by name is best-effort — a
                        // user TOML theme (not a built-in) falls back to the
                        // shipped default accent seed.
                        let theme_accent =
                            scribe_core::theme::Theme::builtin(&config.appearance.theme)
                                .map(|t| {
                                    let [r, g, b, _] = t
                                        .ui(
                                            "accent",
                                            scribe_core::theme::Rgba::new(0x00, 0xe5, 0xff, 255),
                                        )
                                        .to_array();
                                    egui::Color32::from_rgb(r, g, b)
                                })
                                .unwrap_or(egui::Color32::from_rgb(0x00, 0xe5, 0xff));
                        let mut col = config
                            .motion
                            .mesh_color
                            .map(|[r, g, b]| egui::Color32::from_rgb(r, g, b))
                            .unwrap_or(theme_accent);
                        if ui.color_edit_button_srgba(&mut col).changed() {
                            config.motion.mesh_color = Some([col.r(), col.g(), col.b()]);
                            changed = true;
                        }
                        // The reset appears only once the mesh colour has been
                        // pinned away from the theme (mirrors the App-background
                        // "Follow theme" affordance above).
                        if config.motion.mesh_color.is_some()
                            && ui
                                .small_button("Reset to theme")
                                .on_hover_text(
                                    "Clear the custom colour; follow the theme's accent.",
                                )
                                .clicked()
                        {
                            config.motion.mesh_color = None;
                            changed = true;
                        }
                    });
                });
                ui.end_row();
            }
        });
        ui.add_space(6.0);

        // -- Tape & motion accents --
        group(
            ui,
            "Tape & motion accents",
            "VHS tracking bands sweeping down the window.",
        );
        ui.add_space(4.0);
        settings_grid(ui, "settings-motion-tape", |ui| {
            if row_visible(q, "vhs tracking lines motion effect") {
                ui.add_enabled_ui(on, |ui| {
                    changed |= ui
                        .checkbox(&mut config.motion.vhs_tracking, "VHS tracking lines")
                        .on_hover_text(
                            "Faint bright bands sweep down the window like analogue tape tracking.",
                        )
                        .changed();
                });
                ui.label("");
                changed |= reset_to_default(
                    ui,
                    &mut config.motion.vhs_tracking,
                    &def.motion.vhs_tracking,
                );
                ui.end_row();
            }
            if row_visible(q, "vhs drift speed tracking motion") {
                ui.label("VHS drift speed").on_hover_text(
                    "How fast the VHS tracking bands sweep down the window. 1 is the standard \
                     rate; lower drifts more slowly and higher sweeps faster.",
                );
                changed |= stepped_slider(
                    ui,
                    on && config.motion.vhs_tracking,
                    &mut config.motion.vhs_speed,
                    0.25..=3.0,
                    0.1,
                );
                changed |=
                    reset_to_default(ui, &mut config.motion.vhs_speed, &def.motion.vhs_speed);
                ui.end_row();
            }
        });
        ui.add_space(6.0);

        // -- Caret --
        group(
            ui,
            "Caret",
            "Blink and the phosphor ghost-trail behind the text caret.",
        );
        ui.add_space(4.0);
        settings_grid(ui, "settings-motion-caret", |ui| {
            if row_visible(q, "cursor blink motion") {
                ui.add_enabled_ui(on, |ui| {
                    changed |= ui
                        .checkbox(&mut config.motion.cursor_blink, "Blink the text cursor")
                        .on_hover_text(
                            "Blink the text caret instead of showing it steady. Disable for a \
                             calmer, motion-free caret.",
                        )
                        .changed();
                });
                ui.label("");
                changed |= reset_to_default(
                    ui,
                    &mut config.motion.cursor_blink,
                    &def.motion.cursor_blink,
                );
                ui.end_row();
            }
            if row_visible(q, "caret cursor trail motion effect") {
                ui.add_enabled_ui(on, |ui| {
                    changed |= ui
                        .checkbox(&mut config.motion.caret_trail, "Caret ghost-trail")
                        .on_hover_text("A fading echo follows the caret as it moves.")
                        .changed();
                });
                ui.label("");
                changed |=
                    reset_to_default(ui, &mut config.motion.caret_trail, &def.motion.caret_trail);
                ui.end_row();
            }
            if row_visible(q, "caret cursor trail intensity motion effect") {
                ui.label("Caret-trail intensity").on_hover_text(
                    "How far the caret ghost-trail reaches — from a faint short flick to a \
                     bold, long comet tail.",
                );
                changed |= stepped_slider(
                    ui,
                    on && config.motion.caret_trail,
                    &mut config.motion.caret_trail_intensity,
                    0.0..=2.0,
                    0.1,
                );
                changed |= reset_to_default(
                    ui,
                    &mut config.motion.caret_trail_intensity,
                    &def.motion.caret_trail_intensity,
                );
                ui.end_row();
            }
        });
        space(ui);
    }
    changed
}
