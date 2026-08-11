//! Telemetry posture and opt-in crash & issue reporting (W1TN3SS).
//!
//! Lifted verbatim out of `super::render_sections`, which had grown to
//! ~2,300 lines of eleven independent category pages. The body is
//! unchanged: it sat at this same indent level inside the parent
//! function, so the move needed no reindentation and no rewrite.

use super::*;

pub(super) fn render(ui: &mut egui::Ui, config: &mut Config, sel: &str, q: &str) -> bool {
    let mut changed = false;
    if section_visible(
        sel,
        q,
        "Privacy",
        &["privacy", "clear", "data", "recent", "session", "forget"],
    ) {
        head(
            ui,
            "Privacy",
            "SCR1B3 is telemetry-free — everything stays on your device and nothing about you \
             is sent. The only local state that records what you've worked on is the \
             recent-files list and the session-restore snapshot (which keeps unsaved buffers on \
             disk so they survive a restart). You can erase both here.",
        );
        if row_visible(
            q,
            "clear local data recent files session restore forget unsaved",
        ) {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(format!(
                    "Recent files remembered: {}. Session restore keeps on-disk copies of \
                     unsaved buffers.",
                    config.editor.recent_files.len()
                ))
                .weak()
                .small(),
            );
            ui.add_space(4.0);
            let cleared_id = egui::Id::new("scr1b3_privacy_cleared");
            if ui
                .button("Clear local data")
                .on_hover_text(
                    "Erase the recent-files (MRU) list AND the session-restore snapshot, \
                     including the on-disk copies of any unsaved buffers. Open documents and \
                     SAVED files are NOT touched; your settings and themes are kept.",
                )
                .clicked()
            {
                config.editor.recent_files.clear();
                let removed = scribe_core::Config::config_dir()
                    .map(|dir| scribe_core::session::clear_session_state(&dir))
                    .unwrap_or(0);
                changed = true; // persist the emptied recent-files list
                ui.ctx().data_mut(|d| d.insert_temp(cleared_id, removed));
            }
            if let Some(removed) = ui.ctx().data(|d| d.get_temp::<usize>(cleared_id)) {
                ui.label(
                    egui::RichText::new(format!(
                        "Cleared — removed {removed} session file(s) and emptied the recent-files \
                         list."
                    ))
                    .small()
                    .weak(),
                );
            }
        }

        // ---- Opt-in crash & issue reporting (W1TN3SS) ----
        // Two INDEPENDENT streams, each default OFF, each with its own consent
        // posture — never bundled under one toggle. The copy is deliberately
        // consent-framed: "a report you choose to send", never beacon/telemetry/
        // always-on/tracking language.
        group(
            ui,
            "Crash & issue reporting (opt-in)",
            "Off by default. Nothing is ever sent without your say-so. Each report is captured \
             on your device first; you see and can edit the exact text before it leaves, and \
             you choose whether to send it.",
        );
        if row_visible(
            q,
            "crash report reporting opt-in send error panic diagnostics",
        ) {
            ui.add_space(2.0);
            ui.label(egui::RichText::new("Crash reports").strong());
            ui.label(
                egui::RichText::new(
                    "When SCR1B3 closes unexpectedly, capture a short technical report (the \
                     error message and where in our code it happened) so it can be fixed.",
                )
                .weak()
                .small(),
            );
            changed |=
                reporting_mode_selector(ui, "reporting-crash", &mut config.reporting.crash_reports);
            ui.add_space(8.0);
        }
        if row_visible(
            q,
            "manual issue feedback reporting opt-in send report problem",
        ) {
            ui.label(egui::RichText::new("Manual issue reports").strong());
            ui.label(
                egui::RichText::new(
                    "When you choose to report a problem yourself, this controls whether that \
                     report may be sent. You always write and review it first.",
                )
                .weak()
                .small(),
            );
            changed |= reporting_mode_selector(
                ui,
                "reporting-manual",
                &mut config.reporting.manual_issues,
            );
            ui.add_space(8.0);
        }
        if row_visible(q, "what we never collect privacy explainer reporting") {
            // The "what we never collect" panel — the single highest-trust
            // artifact a privacy-first app ships (privacy-consent.md §3).
            egui::CollapsingHeader::new("What a report never contains")
                .id_salt("scr1b3_reporting_never_collect")
                .default_open(true)
                .show(ui, |ui| {
                    for line in [
                        "Your documents, notes, or any file contents — never included.",
                        "Your file paths or folder names — stripped before you ever see the report.",
                        "Your username, computer name, or home-directory path — removed.",
                        "Any device, install, or tracking ID — there is none; reports are not \
                         linkable to you or to each other.",
                        "Your IP address is never stored by us beyond the moment of upload.",
                    ] {
                        ui.label(egui::RichText::new(format!("• {line}")).small());
                    }
                    ui.add_space(2.0);
                    ui.label(
                        egui::RichText::new(
                            "A report carries only a short, sanitized error message, where in \
                             OUR code it happened, and your OS + app version — and only if you \
                             send it.",
                        )
                        .weak()
                        .small(),
                    );
                });
        }
        space(ui);
    }
    changed
}
