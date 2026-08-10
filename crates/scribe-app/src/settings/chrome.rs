//! Shared page chrome for the settings category pages.
//!
//! These three were closures local to `render_sections`. They are lifted to
//! free functions — unchanged in body and in call shape — so each category page
//! can live in its own module and still emit byte-identical chrome. They
//! capture nothing, so the lift is a move, not a rewrite.

use eframe::egui;

/// Vertical gap between category pages.
pub(super) fn space(ui: &mut egui::Ui) {
    ui.add_space(12.0)
}

/// Sub-group header inside a category page (Copland-style #102): a strong
/// single-concept label, a muted one-line "what it controls" sentence, and a
/// thin rule — mirroring Copland's CONFIG.md section formatting so every
/// group reads as a self-explanatory section.
pub(super) fn group(ui: &mut egui::Ui, label: &str, desc: &str) {
    ui.add_space(8.0);
    ui.label(egui::RichText::new(label).strong());
    if !desc.is_empty() {
        ui.label(egui::RichText::new(desc).weak().small());
    }
    ui.separator();
}

/// Category page header: the heading plus a muted one-line description of what
/// the page covers, so each section is self-explanatory at a glance (#69).
pub(super) fn head(ui: &mut egui::Ui, title: &str, desc: &str) {
    ui.heading(title);
    ui.label(egui::RichText::new(desc).weak().small());
    ui.add_space(2.0);
}
