//! File-type associations and the OS default-handler registration.
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
        "Default app",
        &[
            "default",
            "file type",
            "association",
            "open with",
            "handler",
        ],
    ) {
        head(
            ui,
            "Default app",
            "Open text & code files in SCR1B3 by default.",
        );
        group(
            ui,
            "File types",
            "Choose which kinds of file SCR1B3 should handle.",
        );

        // Checklist bound to the persisted claim set. The stored value is
        // `Option<Vec<_>>`: UNSET means "all" (the first-run default), whereas an
        // explicitly EMPTY selection means "none". Collapsing those two states was
        // the bug where unticking every box silently re-ticked them next frame.
        let mut selected = config.integration.claimed_types();
        for ct in ClaimType::ALL {
            let mut on = selected.contains(&ct);
            if ui.checkbox(&mut on, ct.label()).changed() {
                if on {
                    if !selected.contains(&ct) {
                        selected.push(ct);
                    }
                } else {
                    selected.retain(|c| *c != ct);
                }
                // Persist the EXPLICIT selection so a later load reflects exactly
                // what the user picked. `set_claimed_types` always records `Some`,
                // so clearing every box stays cleared instead of resolving back to
                // the unset-means-all default.
                config.integration.set_claimed_types(&selected);
                changed = true;
            }
        }
        space(ui);

        // Honest per-OS copy: on Windows the app can register + deep-link, but
        // only the user can confirm the default in the system UI.
        let os_note = if cfg!(windows) {
            "Windows requires you to confirm the choice in its Settings window — \
             SCR1B3 will open it for you. (No app can change the default for you.)"
        } else if cfg!(target_os = "macos") {
            "SCR1B3 will be registered for these types; macOS asks you to confirm \
             the default once in Finder (Get Info ▸ Open With ▸ Change All)."
        } else {
            "SCR1B3 will be set as the default for these file types."
        };
        ui.label(egui::RichText::new(os_note).weak().small());

        // A registration already running? (present handle ⇒ show a spinner, keep
        // the button disabled so a second click can't spawn a duplicate worker.)
        let pending: Option<RegShared> = ui
            .ctx()
            .data(|d| d.get_temp::<RegShared>(register_pending_id()));
        let enabled = !selected.is_empty() && pending.is_none();
        let btn = ui.add_enabled(
            enabled,
            egui::Button::new(if cfg!(windows) {
                "Register SCR1B3 & open Default Apps…"
            } else {
                "Set SCR1B3 as the default"
            }),
        );
        if btn.clicked() {
            config.integration.register_file_types = true;
            config.integration.last_registration_unix = Some(crate::app::now_unix());
            changed = true;
            // Run registration OFF the UI thread — it spawns several `reg.exe`
            // processes and would otherwise FREEZE the window for a few seconds
            // with no feedback ("nothing seems to be happening"). The worker
            // writes its result into a shared slot and wakes the UI.
            let shared: RegShared = std::sync::Arc::default();
            let sink = shared.clone();
            let types = selected.clone();
            let ctx = ui.ctx().clone();
            std::thread::spawn(move || {
                let report = crate::integration::register(&types);
                if let Ok(mut slot) = sink.lock() {
                    *slot = Some(report);
                }
                ctx.request_repaint(); // wake the UI to pick up the result
            });
            ui.ctx().data_mut(|d| {
                d.insert_temp(register_pending_id(), shared);
                d.remove::<String>(default_app_status_id()); // clear a stale status
            });
        }

        // Poll the in-flight registration: show a spinner while it runs, then
        // stash its result message and drop the handle when it finishes.
        if let Some(shared) = pending {
            let done = shared.lock().ok().and_then(|mut slot| slot.take());
            if let Some(report) = done {
                ui.ctx().data_mut(|d| {
                    d.insert_temp(default_app_status_id(), report.message.clone());
                    d.remove::<RegShared>(register_pending_id());
                });
            } else {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        egui::RichText::new("Registering… opening the Windows Default Apps page")
                            .small(),
                    );
                });
                ui.ctx().request_repaint(); // keep polling until the worker finishes
            }
        }

        // Surface the most-recent completed attempt's status.
        if let Some(msg) = ui
            .ctx()
            .data(|d| d.get_temp::<String>(default_app_status_id()))
        {
            ui.add_space(4.0);
            ui.label(egui::RichText::new(msg).small());
        }
        space(ui);
    }
    changed
}
