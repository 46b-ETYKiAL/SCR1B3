//! Update mode, manual check, and the update-status panel.
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
    updater: &mut crate::updater::Updater,
    sel: &str,
    q: &str,
) -> bool {
    let mut changed = false;
    if section_visible(sel, q, "Updates", &["update", "mode", "notify", "auto"]) {
        head(
            ui,
            "Updates",
            "Check for new SCR1B3 releases. A check reads only the public GitHub releases \
             API and sends no identifiers — no analytics, no telemetry. Off and Manual \
             never touch the network on their own; Notify and Auto check once per launch \
             when due (Notify shows a toast; Auto asks before installing).",
        );
        // Show the running version so the result of a check is concretely verifiable.
        ui.label(
            egui::RichText::new(format!("You are running v{}.", env!("CARGO_PKG_VERSION")))
                .weak()
                .small(),
        );
        ui.add_space(4.0);
        settings_grid(ui, "settings-updates", |ui| {
            if row_visible(q, "update mode notify auto manual off") {
                let modes = [
                    (UpdateMode::Off, "off"),
                    (UpdateMode::Notify, "notify"),
                    (UpdateMode::Manual, "manual"),
                    (UpdateMode::Auto, "auto"),
                ];
                ui.label("Mode").on_hover_text(
                    "When SCR1B3 checks for updates: off (never), manual (only when you press \
                     Check for updates), notify (check once per launch, show a toast if a newer \
                     version exists), auto (check once per launch, ask before installing). A \
                     check reads only the public GitHub releases API and sends no identifiers.",
                );
                let cur = modes.iter().position(|(m, _)| *m == config.updates.mode);
                let sel = modes
                    .iter()
                    .find(|(m, _)| *m == config.updates.mode)
                    .map(|(_, s)| *s)
                    .unwrap_or("notify");
                changed |= stepper_combo(
                    ui,
                    "update-mode",
                    168.0,
                    "update mode",
                    modes.len(),
                    cur,
                    sel,
                    |i| modes[i].1.to_string(),
                    |i| config.updates.mode = modes[i].0,
                );
                changed |= reset_to_default(ui, &mut config.updates.mode, &def.updates.mode);
                ui.end_row();
            }
            if row_visible(q, "check interval hours") {
                ui.label("Check interval (hours)")
                    .on_hover_text("How often, in hours, to check for a new release (1–168).");
                changed |= stepped_slider(
                    ui,
                    true,
                    &mut config.updates.check_interval_hours,
                    1..=168,
                    1.0,
                );
                changed |= reset_to_default(
                    ui,
                    &mut config.updates.check_interval_hours,
                    &def.updates.check_interval_hours,
                );
                ui.end_row();
            }
        });
        if row_visible(q, "check for updates now install update") {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let busy = updater.is_busy();
                if ui
                    .add_enabled(!busy, egui::Button::new("Check for updates"))
                    .on_hover_text(
                        "Ask the public GitHub releases API whether a newer version exists. \
                         No identifiers are sent.",
                    )
                    .clicked()
                {
                    updater.start_check(ui.ctx(), crate::updater::LaunchKind::Manual);
                    // NOTE: a manual check deliberately does NOT stamp
                    // `last_check_unix`. That field is the AUTO-mode interval
                    // throttle; letting a manual press write it used to suppress
                    // the on-launch Notify check for 24h, so the user could
                    // relaunch and never be told a release was out. Notify now
                    // checks every launch regardless of this field, and Auto's
                    // throttle should not be reset by a manual press.
                }
                render_update_status(ui, updater);
            });
            ui.add_space(2.0);
            if ui
                .link("View all releases on GitHub")
                .on_hover_text("Open the SCR1B3 releases page in your browser.")
                .clicked()
            {
                ui.ctx()
                    .open_url(egui::OpenUrl::new_tab(crate::app::RELEASES_URL));
            }
        }
        space(ui);
    }
    changed
}
