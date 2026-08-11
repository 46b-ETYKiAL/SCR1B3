//! Always-on-top, transparency, and glass.
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
        "Window",
        &["mode", "opacity", "tint", "glass", "mica"],
    ) {
        head(
            ui,
            "Window",
            "Always-on-top, and translucency / glass for the window background.",
        );

        // -- Always on top --
        group(ui, "Always on top", "Keep the window above other windows.");
        ui.add_space(4.0);
        settings_grid(ui, "settings-window-aot", |ui| {
            changed |= grid_bool(
                ui,
                q,
                "always on top window above",
                "Always on top",
                "Keep the SCR1B3 window above other windows.",
                &mut config.window.always_on_top,
                &def.window.always_on_top,
            );
        });
        ui.add_space(6.0);

        // -- Transparency / glass --
        group(
            ui,
            "Transparency",
            "Make the window see-through (the desktop shows behind it).",
        );
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(
                "Reveal the desktop through the window. Use the opacity slider for how \
                 see-through it is and the tint to colour it — changes apply immediately.",
            )
            .weak()
            .small(),
        );
        ui.add_space(2.0);
        settings_grid(ui, "settings-window-glass", |ui| {
            // Single on/off switch — off by default (opaque is fast).
            changed |= grid_bool(
                ui,
                q,
                "transparency enable window see-through desktop",
                "Enable window transparency",
                "Make the window see-through so the desktop shows behind it. Use the opacity \
                 slider for how see-through, and the tint below to colour it. Applies immediately.",
                &mut config.window.transparency_enabled,
                &def.window.transparency_enabled,
            );
            let tos = config.window.transparency_enabled;
            if row_visible(q, "window opacity transparent") {
                let translucent = tos;
                ui.label("Opacity").on_hover_text(
                    "How see-through the window is — 1.0 is fully opaque, lower is more \
                     transparent. Only active for translucent modes.",
                );
                // Floor at 0.0 for MAXIMUM transparency — the chrome/panel fills
                // fully vanish; the editor text is painted opaque on top so it
                // stays legible even at zero.
                changed |=
                    stepped_slider(ui, translucent, &mut config.window.opacity, 0.0..=1.0, 0.1);
                changed |= reset_to_default(ui, &mut config.window.opacity, &def.window.opacity);
                ui.end_row();
            }
            changed |= grid_bool(
                ui,
                q,
                "window tint enable colour on off toggle",
                "Enable window tint",
                "Blend the tint colour into the app window background. Turn off to \
                 remove the tint without losing your colour + strength. The tint \
                 only shows once Tint strength is above 0.",
                &mut config.window.tint_enabled,
                &def.window.tint_enabled,
            );
            // The tint sub-controls are gated by the tint MASTER toggle
            // (`tint_enabled`), NOT by transparency: the tint is colour-math on
            // the panel/editor background fills and applies in BOTH opaque and
            // translucent window modes (see `render_support::panel_fill`, which
            // blends the tint BEFORE the translucency alpha). Gating them on
            // `transparency_enabled` was the regression that left the Tint
            // colour + strength greyed-out and "not functioning" whenever
            // transparency was off — even though the tint would have worked.
            let tint_on = config.window.tint_enabled;
            if row_visible(q, "window tint colour hex") {
                ui.label("Tint").on_hover_text(
                    "Colour tint blended into the app window background, as a hex \
                     code (e.g. #1a1a2e). Works in both opaque and transparent \
                     window modes.",
                );
                ui.add_enabled_ui(tint_on, |ui| {
                    ui.horizontal(|ui| {
                        // Click the swatch → egui colour picker pop-out; the hex
                        // field stays for exact/paste entry. The two are kept in
                        // sync (picker writes the hex back).
                        let mut col = parse_hex_color(&config.window.tint)
                            .unwrap_or(egui::Color32::from_rgb(0x08, 0x06, 0x0d));
                        if ui
                            .color_edit_button_srgba(&mut col)
                            .on_hover_text("Pick the tint colour.")
                            .changed()
                        {
                            config.window.tint =
                                format!("#{:02x}{:02x}{:02x}", col.r(), col.g(), col.b());
                            changed = true;
                        }
                        changed |= ui
                            .add(
                                egui::TextEdit::singleline(&mut config.window.tint)
                                    .desired_width(96.0),
                            )
                            .on_hover_text(
                                "Hex colour (e.g. #1a1a2e), or click the swatch to pick.",
                            )
                            .changed();
                    });
                });
                changed |= reset_to_default(ui, &mut config.window.tint, &def.window.tint);
                ui.end_row();
            }
            if row_visible(q, "window tint strength") {
                ui.label("Tint strength").on_hover_text(
                    "How strongly the tint colour is blended over the surface — 0 is none, \
                     1 is full.",
                );
                changed |= stepped_slider(
                    ui,
                    tint_on,
                    &mut config.window.tint_strength,
                    0.0..=1.0,
                    0.1,
                );
                changed |= reset_to_default(
                    ui,
                    &mut config.window.tint_strength,
                    &def.window.tint_strength,
                );
                ui.end_row();
            }
        });
        space(ui);
    }
    changed
}
