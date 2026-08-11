//! Plugin enablement and the deep link into the plugin manager.
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
    if section_visible(sel, q, "Plugins", &["plugin", "mod"]) {
        head(
            ui,
            "Plugins",
            "Enable plugins and open the manager. Plugins are local and \
             signature-verified.",
        );
        settings_grid(ui, "settings-plugins", |ui| {
            changed |= grid_bool(
                ui,
                q,
                "plugin mod enable system",
                "Enable plugin/mod system",
                "Allow SCR1B3 to load plugins / mods from the plugins directory at startup.",
                &mut config.plugins.enabled,
                &def.plugins.enabled,
            );
        });
        ui.label(
            egui::RichText::new("Drop mods into the plugins dir — see PLUGINS.md")
                .weak()
                .small(),
        );
        // F-039 — open the plugin manager (Loaded / Registry / Install). The
        // request is stashed in egui temp data; the host reads + clears it
        // after `show` returns so it can open its own modal state.
        if ui
            .button("Manage plugins…")
            .on_hover_text(
                "Open the plugin manager to view loaded plugins, browse the registry, and \
                 install new ones.",
            )
            .clicked()
        {
            ui.ctx()
                .data_mut(|d| d.insert_temp(open_plugin_manager_id(), true));
        }
        space(ui);
    }
    changed
}
