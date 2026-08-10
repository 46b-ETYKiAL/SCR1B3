//! Command-dispatch methods for `ScribeApp`: `execute_builtin` (the single
//! route for every `BuiltinCommand` from the palette / keyboard) and
//! `drain_pending_editor_action`. Extracted from `mod.rs` (A-01 wave 3 —
//! behavior-preserving move; methods widened to `pub(super)` for the parent
//! + sibling call-sites incl `frame_tick`).
#![allow(clippy::wildcard_imports)]

use super::*;

impl ScribeApp {
    /// Dispatch a [`BuiltinCommand`] selected from the command palette.
    ///
    /// SCOPE — read this before assuming a shortcut and its palette entry share
    /// an implementation. This is the palette's dispatch route, NOT a universal
    /// one. The keyboard path is a separate surface: `keyboard_input` sets
    /// `Pending` (`act.*`) flags that `apply_deferred_actions` handles with its
    /// own handler per action, and a few shortcuts (`TOGGLE_ZEN`,
    /// `TOGGLE_MD_PREVIEW`) are re-implemented inline in `keyboard_input`
    /// without touching either. So an `act.*` action has TWO implementations,
    /// and they can drift — which is precisely what happened to CycleTheme:
    /// the `act.cycle_theme` copy called `reapply_theme`, this one did not, and
    /// the palette entry persisted a theme the window never painted. That arm
    /// now delegates here, so CycleTheme has one implementation; the rest of
    /// the `act.*` set does not, and this comment does not claim otherwise.
    ///
    /// Commands that need an egui `ctx` (which this method does not take) set a
    /// `pending_*` flag for a drain that has one — see `pending_theme_reapply`
    /// and the caret-command bridges.
    ///
    /// Touches `self.config` then persists via `save_config` so toggles survive
    /// a restart.
    pub(super) fn execute_builtin(&mut self, cmd: BuiltinCommand) {
        // Action-log every command dispatch so a session is diagnosable: a
        // command the user invoked that "did nothing" still leaves a trace here.
        crate::action_log::record("cmd", &format!("{cmd:?}"));
        match cmd {
            BuiltinCommand::NewFile => self.new_tab(),
            BuiltinCommand::OpenFile => self.open_dialog(),
            BuiltinCommand::OpenFolder => {
                if let Some(folder) = super::dialogs::pick_folder() {
                    self.status = format!("folder: {}", folder.display());
                    self.open_folder_root(folder);
                }
            }
            BuiltinCommand::OpenRecentFolder => {
                self.recent_folders_open = true;
                self.recent_folders_selected = 0;
            }
            BuiltinCommand::Save => self.save_active(),
            BuiltinCommand::ReportIssue => self.issue_intake.open_fresh(),
            BuiltinCommand::ConvertToMarkdown => self.convert_to_markdown_active(),
            BuiltinCommand::ExportAsHtml => self.export_html_active(),
            BuiltinCommand::SetLineEndingsLf => self.set_active_eol(scribe_core::eol::Eol::Lf),
            BuiltinCommand::SetLineEndingsCrlf => self.set_active_eol(scribe_core::eol::Eol::Crlf),
            BuiltinCommand::SetLineEndingsCr => self.set_active_eol(scribe_core::eol::Eol::Cr),
            BuiltinCommand::CloseActiveTab => self.close_tab(self.active),
            BuiltinCommand::CloseAllTabs => {
                self.tabs.clear();
                self.tabs.push(EditorTab::scratch());
                self.active = 0;
            }
            BuiltinCommand::CycleTabNext => {
                if !self.tabs.is_empty() {
                    self.active = (self.active + 1) % self.tabs.len();
                }
            }
            BuiltinCommand::CycleTabPrev => {
                if !self.tabs.is_empty() {
                    self.active = if self.active == 0 {
                        self.tabs.len() - 1
                    } else {
                        self.active - 1
                    };
                }
            }
            BuiltinCommand::ToggleSplitView => {
                // Unified with the grid: "split" is the multi-pane view of the
                // open tabs (side-by-side for two, a grid for more).
                self.config.editor.grid_enabled = !self.config.editor.grid_enabled;
                self.save_config();
            }
            BuiltinCommand::ToggleMinimap => {
                self.config.editor.show_minimap = !self.config.editor.show_minimap;
                self.save_config();
            }
            BuiltinCommand::ToggleZen => {
                // Runtime session state — no config write (zen is never persisted).
                self.zen_mode = !self.zen_mode;
                if self.zen_mode {
                    self.find_open = false;
                    self.find_in_files_open = false;
                }
            }
            BuiltinCommand::ToggleMarkdownPreview => {
                self.md_preview_open = !self.md_preview_open;
            }
            BuiltinCommand::ToggleDiffView => {
                self.diff_view_open = !self.diff_view_open;
            }
            BuiltinCommand::ToggleNotesPane => {
                self.notes_pane_open = !self.notes_pane_open;
            }
            BuiltinCommand::ToggleSpellcheck => {
                self.config.spellcheck.enabled = !self.config.spellcheck.enabled;
                self.save_config();
            }
            BuiltinCommand::ToggleWordWrap => {
                self.config.editor.word_wrap = !self.config.editor.word_wrap;
                self.save_config();
            }
            BuiltinCommand::ToggleLineNumbers => {
                self.config.editor.show_line_numbers = !self.config.editor.show_line_numbers;
                self.save_config();
            }
            BuiltinCommand::ToggleChangeBar => {
                self.config.editor.show_change_bar = !self.config.editor.show_change_bar;
                self.save_config();
            }
            BuiltinCommand::OpenSettings => {
                self.settings_open = true;
            }
            BuiltinCommand::OpenFind => {
                self.find_open = true;
                self.focus_find = true;
            }
            BuiltinCommand::OpenPalette => {
                // Self-referential entry — leaves the palette open as it was.
                self.palette_open = true;
                self.focus_palette = true;
            }
            BuiltinCommand::CycleTheme => {
                let names = scribe_core::theme::Theme::builtin_names();
                if !names.is_empty() {
                    let cur = &self.config.appearance.theme;
                    let idx = names.iter().position(|n| *n == cur.as_str()).unwrap_or(0);
                    let next = names[(idx + 1) % names.len()].to_string();
                    self.config.appearance.theme = next.clone();
                    // An explicit cycle TAKES OWNERSHIP of the theme, so the
                    // automatic mode has to yield.
                    //
                    // With `follow_os_theme` on — which is the SHIPPED DEFAULT —
                    // `appearance.theme` is not authoritative: on a light OS
                    // `effective_theme_name` returns `ghost-paper`
                    // unconditionally, and on a dark OS it substitutes
                    // `wired-noir` whenever the cycled name is a light theme. So
                    // cycling advanced the config, persisted it, repainted the
                    // IDENTICAL theme, and the status bar named a theme the
                    // window was not showing — a bound command that lies.
                    //
                    // The alternative (leave the toggle on and report that the OS
                    // is driving) keeps the automatic mode but leaves the command
                    // permanently inert on a light desktop, which is worse: the
                    // user invoked a direct manipulation and got nothing. Here
                    // the choice is explicit, announced in the status line, and
                    // reversible from Settings → "Follow OS dark/light".
                    //
                    // EVERY explicit pick does this, the Settings theme picker
                    // included (`settings::take_theme_ownership`). The picker
                    // was once exempted on the grounds that the "Follow OS
                    // dark/light" checkbox sits directly beneath it, so a user
                    // choosing there could see why the picked theme may not
                    // paint. That reasoned about the wrong thing: a nearby
                    // checkbox explains the automatic mode, it does not stop the
                    // pick being silently overridden — the picker still showed a
                    // theme the window was not painting. Its real value is as
                    // the visible one-click way back, and it now unticks in
                    // place when the pick takes ownership.
                    let was_following = std::mem::take(&mut self.config.appearance.follow_os_theme);
                    self.save_config();
                    // Writing the config name is only half of a theme change:
                    // `self.theme` (what the window actually paints from) is
                    // assigned in exactly ONE place — `reapply_theme` — and
                    // that needs a `ctx` this method does not have. Without
                    // this flag the palette entry persisted the new theme and
                    // the window kept rendering the old one until restart.
                    self.pending_theme_reapply = true;
                    self.status = if was_following {
                        format!("theme: {next} (stopped following the OS light/dark setting)")
                    } else {
                        format!("theme: {next}")
                    };
                }
            }
            BuiltinCommand::StartLsp => self.start_lsp_for_active(),
            BuiltinCommand::FoldAll => {
                if self.active < self.tabs.len() {
                    let text = self.tabs[self.active].text.clone();
                    // P2-4: markdown/text notes fold by heading section.
                    let lang = self.tabs[self.active].doc.language_hint();
                    let regions = crate::editor_features::fold_regions_for(&text, lang.as_deref());
                    self.folds = regions.iter().map(|r| r.start_line).collect();
                    self.fold_view = true;
                    self.status = format!("folded {} region(s)", regions.len());
                }
            }
            BuiltinCommand::ExpandAll => {
                self.folds.clear();
                self.status = String::from("expanded all");
            }
            BuiltinCommand::OpenPluginManager => {
                self.plugin_manager
                    .ensure_defaults(Config::config_dir().as_deref());
                self.plugin_manager.open = true;
            }
            BuiltinCommand::SortLines => {
                let active = self.active.min(self.tabs.len().saturating_sub(1));
                if active < self.tabs.len() && !self.tabs[active].doc.is_read_only_large() {
                    let sorted = scribe_core::text_ops::sort_lines(&self.tabs[active].text);
                    if sorted != self.tabs[active].text {
                        self.tabs[active].set_text_keep_undo(sorted);
                        self.tabs[active].doc.mark_dirty();
                        self.status = "sorted lines (A-Z)".to_string();
                        // P2-C: reordering lines invalidates every caret offset.
                        self.mc_clear_carets();
                    }
                }
            }
            BuiltinCommand::SortLinesUnique => self.apply_buffer_transform(
                "sorted lines (unique)",
                scribe_core::text_ops::sort_lines_unique,
            ),
            BuiltinCommand::TrimTrailingWhitespace => self.apply_buffer_transform(
                "trimmed trailing whitespace",
                scribe_core::text_ops::trim_trailing_whitespace,
            ),
            BuiltinCommand::EnsureFinalNewline => self.apply_buffer_transform(
                "ensured a final newline",
                scribe_core::text_ops::ensure_final_newline,
            ),
            BuiltinCommand::ConvertIndentToSpaces => {
                let w = self.config.editor.tab_width;
                self.apply_buffer_transform("converted indentation to spaces", |t| {
                    scribe_core::text_ops::tabs_to_spaces(t, w)
                });
            }
            BuiltinCommand::ConvertIndentToTabs => {
                let w = self.config.editor.tab_width;
                self.apply_buffer_transform("converted indentation to tabs", |t| {
                    scribe_core::text_ops::spaces_to_tabs(t, w)
                });
            }
            BuiltinCommand::RevealInExplorer => {
                let active = self.active.min(self.tabs.len().saturating_sub(1));
                let dir = self
                    .tabs
                    .get(active)
                    .and_then(|t| t.doc.path())
                    .and_then(|p| p.parent())
                    .map(|d| d.to_path_buf());
                match dir {
                    Some(d) => open_in_file_manager(&d),
                    None => {
                        self.toast = Some(
                            "Save this note first to show it in your file manager.".to_string(),
                        )
                    }
                }
            }
            BuiltinCommand::CopyFilePath => {
                let active = self.active.min(self.tabs.len().saturating_sub(1));
                let path = self
                    .tabs
                    .get(active)
                    .and_then(|t| t.doc.path())
                    .map(|p| p.display().to_string());
                match path {
                    Some(p) => match write_clipboard_text(&p) {
                        Ok(()) => self.status = format!("copied path: {p}"),
                        Err(e) => {
                            tracing::warn!("copy file path to clipboard failed: {e}");
                            self.toast =
                                Some("Couldn't copy the path to the clipboard. Try again.".into());
                        }
                    },
                    None => {
                        self.toast = Some("Save this note first to copy its file path.".to_string())
                    }
                }
            }
            // These three need the egui TextEditState (ctx), unavailable here;
            // set a flag that frame_tick drains (see the act.* keyboard path).
            BuiltinCommand::JumpMatchingBracket => self.pending_jump_bracket = true,
            BuiltinCommand::InsertDateTime => self.pending_insert_datetime = true,
            BuiltinCommand::DuplicateSelection => self.pending_dup_selection = true,
            // Clipboard / history actions: record the request; `frame_tick`
            // drains it into the focused editor as a native egui event.
            BuiltinCommand::Copy => self.pending_editor_action = Some(EditorAction::Copy),
            BuiltinCommand::Cut => self.pending_editor_action = Some(EditorAction::Cut),
            BuiltinCommand::Paste => self.pending_editor_action = Some(EditorAction::Paste),
            BuiltinCommand::Undo => self.pending_editor_action = Some(EditorAction::Undo),
            BuiltinCommand::Redo => self.pending_editor_action = Some(EditorAction::Redo),
            BuiltinCommand::ToggleBookmark => self.toggle_bookmark(),
            BuiltinCommand::NextBookmark => self.navigate_bookmark(1),
            BuiltinCommand::PrevBookmark => self.navigate_bookmark(-1),
            BuiltinCommand::GoToSymbol => {
                self.goto_symbol_open = true;
                self.focus_goto_symbol = true;
                self.goto_symbol_selected = 0;
                self.goto_symbol_query.clear();
            }
            // ---- Note-usability actions (need the egui caret; drained in
            // frame_tick like the other pending_* caret commands) ----
            BuiltinCommand::ToggleTaskCheckbox => self.pending_toggle_task = true,
            BuiltinCommand::ToggleBold => self.pending_wrap_marker = Some("**"),
            BuiltinCommand::ToggleItalic => self.pending_wrap_marker = Some("*"),
            BuiltinCommand::ToggleInlineCode => self.pending_wrap_marker = Some("`"),
            BuiltinCommand::ToggleStrikethrough => self.pending_wrap_marker = Some("~~"),
            BuiltinCommand::UppercaseSelection => self.pending_case = Some(1),
            BuiltinCommand::LowercaseSelection => self.pending_case = Some(0),
            BuiltinCommand::TitlecaseSelection => self.pending_case = Some(2),
            BuiltinCommand::FormatTable => self.pending_format_table = true,
            BuiltinCommand::NewChecklistNote => {
                self.new_note_from_template(super::text_ops_methods::NoteTemplate::Checklist)
            }
            BuiltinCommand::NewMeetingNote => {
                self.new_note_from_template(super::text_ops_methods::NoteTemplate::Meeting)
            }
            // A daily note is a real dated file in the vault, not a scratch
            // buffer — so it routes through `new_daily_note`, which falls back
            // to the seeded buffer only when there is no vault to write to.
            BuiltinCommand::NewDailyNote => self.new_daily_note(),
            BuiltinCommand::PasteImageAttachment => self.paste_image_attachment(),
            BuiltinCommand::RenameNote => self.rename_note_active(),
        }
    }

    /// Whether the active tab currently renders through the in-house rope
    /// editor rather than egui's `TextEdit`.
    ///
    /// Mirrors the branch order `frame_tick` takes for the central surface: the
    /// tiling grid and the folded preview each own their own (TextEdit-backed)
    /// surface, a read-only huge file browses through the NON-editable rope
    /// path, and only then does `use_rope_editor` decide. Kept in lockstep with
    /// that branch so a delivered editor action lands where the caret actually
    /// is — the rope path auto-engages past `rope_editor_auto_threshold_bytes`
    /// (16 MiB by default), so this is not an opt-in-only corner.
    pub(super) fn active_editor_is_rope(&self) -> bool {
        if self.grid_tree.is_some() || self.fold_view {
            return false;
        }
        let Some(tab) = self.tabs.get(self.active) else {
            return false;
        };
        if tab.doc.is_read_only_large() {
            return false;
        }
        use_rope_editor(
            self.config.editor.experimental_rope_editor,
            tab.text.len(),
            self.config.editor.rope_editor_auto_threshold_bytes,
        )
    }

    /// Drain a palette / menu-requested clipboard-history action into the editor
    /// that is ACTUALLY rendering the active tab. Called at the top of
    /// `frame_tick`, before any panel renders, so the editor (shown later in the
    /// same frame) sees the action. `Paste` reads the OS clipboard via
    /// `arboard`; a read failure surfaces a toast rather than panicking.
    ///
    /// Delivery is mode-aware because the two central-editor paths claim
    /// different widget ids and consume input differently:
    ///
    /// * **rope path** — the action is queued on the tab's `RopeEditorState`
    ///   and applied by `show_editable` regardless of focus. The rope editor's
    ///   id is `ui.id().with("scr1b3-rope-editable")`, derived from the `Ui` it
    ///   is shown in and therefore not reproducible here; and focusing it would
    ///   be wrong anyway — an explicit command must not yank the caret around.
    /// * **`TextEdit` path** — the event is pushed onto egui's input queue and
    ///   the central editor is focused so `TextEdit` performs it natively. The
    ///   id MUST carry the active tab's `doc_id` salt, because that is the id
    ///   `frame_tick` builds the widget with (per-note `TextEditState`).
    /// * **grid path** — the panes are `TextEdit`s with egui-auto-generated ids
    ///   that cannot be named from here, so the event is pushed WITHOUT
    ///   touching focus: whichever pane the user is in handles it.
    pub(super) fn drain_pending_editor_action(&mut self, ctx: &egui::Context) {
        // A `ctx`-less command (the palette's CycleTheme) that needs one. Kept
        // here with the other pending_* drains so every dispatch site of
        // `execute_builtin` is covered by a single drain rather than three.
        self.drain_pending_theme_reapply(ctx);
        // A right-click pick on the ROPE editor's context menu arrives as a
        // ctx-data request (scribe-render owns no clipboard dependency). Fold it
        // into the same pending slot the palette uses so both routes share one
        // drain — and so an already-pending palette pick is never dropped.
        if let Some(req) = scribe_render::take_rope_action_request(ctx) {
            let action = match req {
                scribe_render::RopeEditorAction::Copy => EditorAction::Copy,
                scribe_render::RopeEditorAction::Cut => EditorAction::Cut,
                scribe_render::RopeEditorAction::Paste => EditorAction::Paste,
                scribe_render::RopeEditorAction::Undo => EditorAction::Undo,
                scribe_render::RopeEditorAction::Redo => EditorAction::Redo,
            };
            self.pending_editor_action.get_or_insert(action);
        }
        // A command that produced TEXT to insert (currently the image-paste
        // attachment link) rides the same delivery path as a clipboard paste:
        // the file is already on disk, only the caret insertion is deferred.
        // Delivered as `Event::Paste` because that is what both editor surfaces
        // already implement as "insert this string at the caret, replacing the
        // selection" — no second insertion path to keep in lockstep.
        if let Some(text) = self.pending_insert_text.take() {
            self.deliver_editor_event(ctx, egui::Event::Paste(text));
        }
        let Some(action) = self.pending_editor_action.take() else {
            return;
        };
        let event = match action {
            EditorAction::Copy => egui::Event::Copy,
            EditorAction::Cut => egui::Event::Cut,
            EditorAction::Paste => match read_clipboard_text() {
                Ok(text) => egui::Event::Paste(text),
                Err(e) => {
                    tracing::warn!("clipboard read for paste failed: {e}");
                    self.toast = Some(
                        "Couldn't read the clipboard. Copy the text again, then paste.".into(),
                    );
                    return;
                }
            },
            EditorAction::Undo => key_event(egui::Key::Z, egui::Modifiers::COMMAND),
            EditorAction::Redo => key_event(
                egui::Key::Z,
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
            ),
        };
        self.deliver_editor_event(ctx, event);
    }

    /// Deliver one synthesised egui event to whichever surface is rendering the
    /// active tab. Extracted so every command that has to reach the caret
    /// (clipboard actions, the pasted-image link) uses ONE mode-aware delivery
    /// path rather than each re-deriving the rope/TextEdit/grid split.
    fn deliver_editor_event(&mut self, ctx: &egui::Context, event: egui::Event) {
        if self.active_editor_is_rope() {
            let tab = &mut self.tabs[self.active];
            tab.rope_state
                .get_or_insert_with(scribe_render::RopeEditorState::new)
                .inject_event(event);
            // The queue is drained by the editor's next paint; make sure one
            // happens even if nothing else requested it.
            ctx.request_repaint();
            return;
        }
        ctx.input_mut(|i| i.events.push(event));
        if self.grid_tree.is_none() {
            // Focus the editor so the pushed event is delivered to it. The
            // `doc_id` salt is what `frame_tick` keys the widget on; an
            // un-salted id belongs to NO widget, so focusing it both failed to
            // deliver the action and stole focus from the real editor.
            if let Some(tab) = self.tabs.get(self.active) {
                let editor_id = egui::Id::new("scr1b3-central-editor").with(tab.doc_id);
                ctx.memory_mut(|m| m.request_focus(editor_id));
            }
        }
    }
}
