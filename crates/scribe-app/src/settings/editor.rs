//! Indentation, display, scroll, layout, and save/session behaviour.
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
        "Editor",
        &[
            "tab width",
            "insert spaces",
            "line numbers",
            "word wrap",
            "minimap",
            "restore session",
            "scroll speed",
            "animate jump scrolls",
            "middle click autoscroll",
            "autoscroll sensitivity",
        ],
    ) {
        head(
            ui,
            "Editor",
            "Indentation, what's shown around the text, the tab bar, and save / \
             session behaviour.",
        );

        // -- Indentation --
        group(
            ui,
            "Indentation",
            "Tabs vs spaces, and how wide one indent step is.",
        );
        ui.add_space(4.0);
        settings_grid(ui, "settings-editor-indentation", |ui| {
            if row_visible(q, "tab width") {
                ui.label("Tab width")
                    .on_hover_text("How many columns a tab character occupies.");
                changed |= stepped_slider(ui, true, &mut config.editor.tab_width, 1..=8, 1.0);
                changed |=
                    reset_to_default(ui, &mut config.editor.tab_width, &def.editor.tab_width);
                ui.end_row();
            }
            changed |= grid_bool(
                ui,
                q,
                "insert spaces",
                "Insert spaces (Tab key)",
                "Insert spaces instead of a tab character when you press Tab.",
                &mut config.editor.insert_spaces,
                &def.editor.insert_spaces,
            );
        });
        ui.add_space(6.0);

        // -- Display --
        group(
            ui,
            "Display",
            "What is shown around the text — line numbers, minimap, wrapping, whitespace.",
        );
        ui.add_space(4.0);
        settings_grid(ui, "settings-editor-display", |ui| {
            changed |= grid_bool(
                ui,
                q,
                "line numbers",
                "Line numbers",
                "Show a line-number gutter to the left of the editor.",
                &mut config.editor.show_line_numbers,
                &def.editor.show_line_numbers,
            );
            changed |= grid_bool(
                ui,
                q,
                "change bar",
                "Change bar",
                "Mark edited lines in the gutter: amber for unsaved edits, green \
                 once saved, none for untouched lines (Notepad++ style).",
                &mut config.editor.show_change_bar,
                &def.editor.show_change_bar,
            );
            changed |= grid_bool(
                ui,
                q,
                "word wrap",
                "Word wrap",
                "Wrap long lines to the editor width instead of scrolling horizontally.",
                &mut config.editor.word_wrap,
                &def.editor.word_wrap,
            );
            changed |= grid_bool(
                ui,
                q,
                "minimap",
                "Minimap",
                "Show a zoomed-out overview of the whole file alongside the editor for \
                 quick navigation.",
                &mut config.editor.show_minimap,
                &def.editor.show_minimap,
            );
            changed |= grid_bool(
                ui,
                q,
                "render whitespace markers",
                "Render whitespace markers (spaces · tabs)",
                "Draw faint markers for spaces and tabs so invisible whitespace is \
                 visible. Applies to the experimental rope editor.",
                &mut config.editor.render_whitespace,
                &def.editor.render_whitespace,
            );
            changed |= grid_bool(
                ui,
                q,
                "snippets tab trigger expand prefix",
                "Tab-trigger snippets",
                "Expand a snippet when Tab is pressed right after a known prefix \
                 from snippets.toml in the config folder. Applies to the in-house \
                 editor.",
                &mut config.editor.snippets_enabled,
                &def.editor.snippets_enabled,
            );
            changed |= grid_bool(
                ui,
                q,
                "current line highlight caret row band",
                "Highlight current line",
                "Draw a faint band across the line the caret is on.",
                &mut config.editor.current_line_highlight,
                &def.editor.current_line_highlight,
            );
            changed |= grid_bool(
                ui,
                q,
                "indent guides vertical lines",
                "Indent guides",
                "Draw faint vertical guide lines at each indent level.",
                &mut config.editor.indent_guides,
                &def.editor.indent_guides,
            );
            changed |= grid_bool(
                ui,
                q,
                "bracket match highlight pair",
                "Bracket-match highlight",
                "Box the bracket next to the caret and its matching partner.",
                &mut config.editor.bracket_match,
                &def.editor.bracket_match,
            );
            changed |= grid_bool(
                ui,
                q,
                "highlight selection occurrences",
                "Highlight occurrences",
                "When text is selected, box every other matching run in view.",
                &mut config.editor.highlight_selection_occurrences,
                &def.editor.highlight_selection_occurrences,
            );
            changed |= grid_bool(
                ui,
                q,
                "trailing whitespace highlight",
                "Trailing whitespace",
                "Tint trailing spaces/tabs on each line (distinct from \
                 \"render whitespace\", which shows all whitespace).",
                &mut config.editor.highlight_trailing_whitespace,
                &def.editor.highlight_trailing_whitespace,
            );
            changed |= grid_bool(
                ui,
                q,
                "smooth scroll wheel easing",
                "Smooth scrolling",
                "Ease wheel scrolling. Turn off for snappier, discrete-notch scrolling.",
                &mut config.editor.smooth_scroll,
                &def.editor.smooth_scroll,
            );
            if row_visible(q, "caret style cursor shape bar block underline") {
                use scribe_core::config::CaretStyle;
                let styles = [
                    (CaretStyle::Bar, "bar"),
                    (CaretStyle::Block, "block"),
                    (CaretStyle::Underline, "underline"),
                ];
                ui.label("Caret style")
                    .on_hover_text("Shape of the text caret: thin bar, full block, or underline.");
                let cur = styles
                    .iter()
                    .position(|(s, _)| *s == config.editor.caret_style);
                let sel = styles
                    .iter()
                    .find(|(s, _)| *s == config.editor.caret_style)
                    .map(|(_, l)| *l)
                    .unwrap_or("bar");
                changed |= stepper_combo(
                    ui,
                    "caret-style",
                    168.0,
                    "caret style",
                    styles.len(),
                    cur,
                    sel,
                    |i| styles[i].1.to_string(),
                    |i| config.editor.caret_style = styles[i].0,
                );
                changed |=
                    reset_to_default(ui, &mut config.editor.caret_style, &def.editor.caret_style);
                ui.end_row();
            }
            if row_visible(q, "caret width cursor thickness") {
                ui.label("Caret width")
                    .on_hover_text("Caret thickness for the bar/underline styles (points).");
                changed |= stepped_slider(ui, true, &mut config.editor.caret_width, 1.0..=4.0, 0.5);
                changed |=
                    reset_to_default(ui, &mut config.editor.caret_width, &def.editor.caret_width);
                ui.end_row();
            }
            if row_visible(q, "scrollbar style chrome auto thin hidden") {
                use scribe_core::config::ScrollbarStyle;
                let styles = [
                    (ScrollbarStyle::Auto, "auto"),
                    (ScrollbarStyle::Thin, "thin"),
                    (ScrollbarStyle::Hidden, "hidden"),
                ];
                ui.label("Scrollbar style")
                    .on_hover_text("Editor scrollbar chrome: default, a slim bar, or hidden.");
                let cur = styles
                    .iter()
                    .position(|(s, _)| *s == config.editor.scrollbar_style);
                let sel = styles
                    .iter()
                    .find(|(s, _)| *s == config.editor.scrollbar_style)
                    .map(|(_, l)| *l)
                    .unwrap_or("auto");
                changed |= stepper_combo(
                    ui,
                    "scrollbar-style",
                    168.0,
                    "scrollbar style",
                    styles.len(),
                    cur,
                    sel,
                    |i| styles[i].1.to_string(),
                    |i| config.editor.scrollbar_style = styles[i].0,
                );
                changed |= reset_to_default(
                    ui,
                    &mut config.editor.scrollbar_style,
                    &def.editor.scrollbar_style,
                );
                ui.end_row();
            }
        });
        ui.add_space(6.0);

        // -- Scroll --
        group(
            ui,
            "Scroll",
            "Mouse-wheel speed, jump-scroll animation, and middle-click autoscroll.",
        );
        ui.add_space(4.0);
        settings_grid(ui, "settings-editor-scroll", |ui| {
            if row_visible(q, "scroll speed") {
                ui.label("Scroll speed").on_hover_text(
                    "Mouse-wheel scrolling speed. egui's default (40) feels slow next to \
                     Windows; 75 is the SCR1B3 default.",
                );
                changed |= stepped_slider(ui, true, &mut config.scroll.speed, 10.0..=200.0, 1.0);
                changed |= reset_to_default(ui, &mut config.scroll.speed, &def.scroll.speed);
                ui.end_row();
            }
            changed |= grid_bool(
                ui,
                q,
                "animate jump scrolls",
                "Animate jump scrolls",
                "Ease programmatic jumps (go-to-line, find-next) instead of snapping instantly.",
                &mut config.scroll.animate_jumps,
                &def.scroll.animate_jumps,
            );
            changed |= grid_bool(
                ui,
                q,
                "middle click autoscroll",
                "Middle-click autoscroll",
                "Click the mouse wheel, then move the pointer away from the click point to \
                 scroll continuously (Windows-style). Any click exits.",
                &mut config.scroll.autoscroll,
                &def.scroll.autoscroll,
            );
            if row_visible(q, "autoscroll sensitivity") {
                ui.label("Autoscroll sensitivity").on_hover_text(
                    "How fast middle-click autoscroll drifts per pixel of pointer offset \
                     from the click point.",
                );
                changed |= stepped_slider(
                    ui,
                    true,
                    &mut config.scroll.autoscroll_sensitivity,
                    2.0..=15.0,
                    1.0,
                );
                changed |= reset_to_default(
                    ui,
                    &mut config.scroll.autoscroll_sensitivity,
                    &def.scroll.autoscroll_sensitivity,
                );
                ui.end_row();
            }
            if row_visible(q, "autoscroll dead zone deadzone") {
                ui.label("Autoscroll dead zone").on_hover_text(
                    "Radius (px) around the middle-click origin where the pointer produces NO \
                     scrolling — a still zone so small jitters don't drift the page.",
                );
                changed |= stepped_slider(
                    ui,
                    true,
                    &mut config.scroll.autoscroll_dead_zone,
                    4.0..=40.0,
                    1.0,
                );
                changed |= reset_to_default(
                    ui,
                    &mut config.scroll.autoscroll_dead_zone,
                    &def.scroll.autoscroll_dead_zone,
                );
                ui.end_row();
            }
            changed |= grid_bool(
                ui,
                q,
                "drag select autoscroll wheel edge",
                "Drag-select autoscroll",
                "While selecting with the left button held, roll the wheel — or hold the \
                 pointer near the top/bottom edge — to scroll and keep extending the \
                 selection past the visible area.",
                &mut config.scroll.drag_autoscroll,
                &def.scroll.drag_autoscroll,
            );
            changed |= grid_bool(
                ui,
                q,
                "scroll past end beyond last line",
                "Scroll past end",
                "Allow scrolling a little beyond the last line so it can sit at a \
                 comfortable height instead of pinned to the bottom.",
                &mut config.scroll.scroll_past_end,
                &def.scroll.scroll_past_end,
            );
            if row_visible(q, "caret scroll off surrounding lines scrolloff") {
                ui.label("Caret scroll-off").on_hover_text(
                    "Keep the caret at least this many lines from the top/bottom edge when \
                     navigating by keyboard (Vim scrolloff). 0 disables.",
                );
                changed |=
                    stepped_slider(ui, true, &mut config.scroll.caret_scroll_off, 0..=12, 1.0);
                changed |= reset_to_default(
                    ui,
                    &mut config.scroll.caret_scroll_off,
                    &def.scroll.caret_scroll_off,
                );
                ui.end_row();
            }
        });
        ui.add_space(6.0);

        // -- Layout --
        group(
            ui,
            "Layout",
            "Where the tab bar sits and how the editor surface is arranged.",
        );
        ui.add_space(4.0);
        settings_grid(ui, "settings-editor-layout", |ui| {
            if row_visible(q, "tab bar position top bottom left right") {
                // T18.4: position the open-tab strip relative to the editor.
                use scribe_core::config::TabBarPosition;
                let positions = [
                    (TabBarPosition::Top, "top"),
                    (TabBarPosition::Bottom, "bottom"),
                    (TabBarPosition::Left, "left"),
                    (TabBarPosition::Right, "right"),
                ];
                ui.label("Tab bar position")
                    .on_hover_text("Where the strip of open-file tabs sits around the editor.");
                let cur = positions
                    .iter()
                    .position(|(p, _)| *p == config.editor.tab_bar_position);
                let sel = positions
                    .iter()
                    .find(|(p, _)| *p == config.editor.tab_bar_position)
                    .map(|(_, s)| *s)
                    .unwrap_or("top");
                changed |= stepper_combo(
                    ui,
                    "tab-bar-position",
                    168.0,
                    "tab bar position",
                    positions.len(),
                    cur,
                    sel,
                    |i| positions[i].1.to_string(),
                    |i| {
                        // Switching TO a Left/Right bar from a non-side one defaults
                        // "rotate side tabs" ON (the requested default for vertical
                        // bars); the user can turn it back off. No effect Top/Bottom.
                        let prev = config.editor.tab_bar_position;
                        let pos = positions[i].0;
                        config.editor.tab_bar_position = pos;
                        if pos.is_vertical() && !prev.is_vertical() {
                            config.editor.side_tabs_rotated = true;
                        }
                    },
                );
                changed |= reset_to_default(
                    ui,
                    &mut config.editor.tab_bar_position,
                    &def.editor.tab_bar_position,
                );
                ui.end_row();
            }
            if row_visible(
                q,
                "side tab orientation vertical horizontal rotate left right",
            ) {
                // #82 — only meaningful when the tab bar is on the Left/Right;
                // greyed otherwise so the dependency is obvious.
                let is_side = config.editor.tab_bar_position.is_vertical();
                ui.add_enabled_ui(is_side, |ui| {
                    changed |= ui
                        .checkbox(
                            &mut config.editor.side_tabs_rotated,
                            "Rotate side tabs (vertical text)",
                        )
                        .on_hover_text(
                            "When the tab bar is on the Left or Right: ON rotates each tab's \
                             label 90° so the text reads vertically, while the tabs stay in a \
                             single column. OFF keeps the labels horizontal. No effect for \
                             Top/Bottom.",
                        )
                        .changed();
                });
                ui.label("");
                changed |= reset_to_default(
                    ui,
                    &mut config.editor.side_tabs_rotated,
                    &def.editor.side_tabs_rotated,
                );
                ui.end_row();
            }
            if row_visible(
                q,
                "wrap note title two lines side bar left right multi-line",
            ) {
                // Effective ONLY when the tab bar is on the Left/Right AND in
                // HORIZONTAL orientation (rotated OFF): a title too long for one
                // line then wraps to a 2nd line (max two; the 2nd truncates with
                // an ellipsis if even two don't fit). Greyed otherwise so the
                // double dependency (side bar + not rotated) is obvious.
                let is_horizontal_side = config.editor.tab_bar_position.is_vertical()
                    && !config.editor.side_tabs_rotated;
                ui.add_enabled_ui(is_horizontal_side, |ui| {
                    changed |= ui
                        .checkbox(
                            &mut config.editor.side_tabs_wrap_two_lines,
                            "Wrap note titles to 2 lines (side bar)",
                        )
                        .on_hover_text(
                            "When the tab bar is on the Left or Right with horizontal labels: \
                             ON lets a title that doesn't fit on one line wrap onto a second \
                             line (max two lines; the second is truncated with … if the title \
                             is longer still). OFF keeps each side-bar title on a single line, \
                             truncated with … when the bar is narrower than the title. No \
                             effect for Top/Bottom or the rotated (vertical-text) side variant.",
                        )
                        .changed();
                });
                ui.label("");
                changed |= reset_to_default(
                    ui,
                    &mut config.editor.side_tabs_wrap_two_lines,
                    &def.editor.side_tabs_wrap_two_lines,
                );
                ui.end_row();
            }
            changed |= grid_bool(
                ui,
                q,
                "multi-note grid panes split editor central",
                "Multi-note grid (experimental)",
                "Render every open tab as a movable / resizable pane in the central editor. \
                 Drag tabs between panes to rearrange; drag the splitter to resize.",
                &mut config.editor.grid_enabled,
                &def.editor.grid_enabled,
            );
            changed |= grid_bool(
                ui,
                q,
                "experimental rope editor owned cursor undo keystone",
                "Experimental rope editor",
                "Use the in-house rope editor for normal files instead of the default egui \
                 text widget. Own caret, selection, and persistent-capable undo. \
                 Experimental: no IME / mouse-selection parity yet.",
                &mut config.editor.experimental_rope_editor,
                &def.editor.experimental_rope_editor,
            );
        });
        ui.add_space(6.0);

        // -- Save & Session --
        group(
            ui,
            "Save & Session",
            "Autosave, session restore, and on-save cleanup.",
        );
        ui.add_space(4.0);
        settings_grid(ui, "settings-editor-save", |ui| {
            changed |= grid_bool(
                ui,
                q,
                "restore session reopen saved files tabs",
                "Reopen saved files from last session",
                "Reopens the SAVED files/tabs you had open when you last closed SCR1B3. \
                 (Distinct from 'Restore unsaved notes' below, which recovers never-saved \
                 buffers — the two are independent and you can use either, both, or neither.)",
                &mut config.editor.restore_session,
                &def.editor.restore_session,
            );
            changed |= grid_bool(
                ui,
                q,
                "session backup hot exit unsaved restore crash recovery",
                "Restore unsaved notes after restart",
                "Keeps a backup of UNSAVED buffers (including never-saved scratch notes) so \
                 they come back after a restart or crash — no save needed. Backups live in \
                 the config 'backup' folder and are deleted once you save. (Distinct from \
                 'Reopen saved files' above.) On by default.",
                &mut config.editor.session_backup,
                &def.editor.session_backup,
            );
            changed |= grid_bool(
                ui,
                q,
                "auto save autosave",
                "Auto-save (after a short pause)",
                "Automatically save dirty file-backed buffers a few seconds after you stop \
                 typing. Untitled buffers are never auto-saved. Off by default.",
                &mut config.editor.auto_save,
                &def.editor.auto_save,
            );
            changed |= grid_bool(
                ui,
                q,
                "trim trailing whitespace on save",
                "Trim trailing whitespace on save",
                "Remove trailing spaces and tabs at the end of every line when a file is saved.",
                &mut config.editor.trim_trailing_whitespace_on_save,
                &def.editor.trim_trailing_whitespace_on_save,
            );
            changed |= grid_bool(
                ui,
                q,
                "final newline ensure on save",
                "Ensure final newline on save",
                "Make sure the file ends with exactly one newline character when saved.",
                &mut config.editor.final_newline_on_save,
                &def.editor.final_newline_on_save,
            );
            changed |= grid_bool(
                ui,
                q,
                "restore cursor caret position per file",
                "Restore caret position per file",
                "Remember where the caret was in each file and jump back there when you \
                 reopen it.",
                &mut config.editor.restore_cursor_position,
                &def.editor.restore_cursor_position,
            );
            if row_visible(q, "default save format markdown plain text extension") {
                use scribe_core::config::DefaultSaveFormat;
                ui.label("Default save format").on_hover_text(
                    "The file type a brand-new note suggests in the Save dialog. Markdown \
                     (.md) by default — you can still pick any name or extension when saving.",
                );
                let formats = DefaultSaveFormat::ALL;
                let cur = formats
                    .iter()
                    .position(|f| *f == config.integration.default_save_format);
                let sel = config.integration.default_save_format.ui_label();
                changed |= stepper_combo(
                    ui,
                    "default-save-format",
                    168.0,
                    "save format",
                    formats.len(),
                    cur,
                    sel,
                    |i| formats[i].ui_label().to_string(),
                    |i| config.integration.default_save_format = formats[i],
                );
                changed |= reset_to_default(
                    ui,
                    &mut config.integration.default_save_format,
                    &def.integration.default_save_format,
                );
                ui.end_row();
            }
        });
        space(ui);
    }
    changed
}
