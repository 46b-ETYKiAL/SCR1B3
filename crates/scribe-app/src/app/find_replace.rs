//! Find/replace surface: in-buffer Replace (F-008) plus the find-in-files run/drain/open-result trio. Bodies moved verbatim from the `app` god-module (A-01 decomposition); `use super::*` re-exports the types these methods touch.
#![allow(clippy::wildcard_imports)]
use super::*;

impl ScribeApp {
    /// The find bar's matching-mode row: the `regex` / `match case` /
    /// `whole word` checkboxes plus the inline invalid-regex error line.
    ///
    /// Lives here (not inline in the frame body) so the whole find/replace
    /// surface — state, query construction, replacement, and the controls that
    /// drive them — stays in one file. `frame_tick` calls this once from the
    /// find bar; that call is the wire the toggles ride, and cutting it is what
    /// `find_toggle_tests` detects.
    ///
    /// Each checkbox binds DIRECTLY to the field
    /// [`find_query_flags`](ScribeApp::find_query_flags) reads, so there is no
    /// copy step that could go stale.
    pub(super) fn find_bar_options_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.find_regex, "regex").on_hover_text(
                "Treat the query as a regular expression ($1 capture refs work in the replacement)",
            );
            ui.checkbox(&mut self.find_case_sensitive, "match case")
                .on_hover_text("Match upper/lower case exactly");
            ui.checkbox(&mut self.find_whole_word, "whole word")
                .on_hover_text("Match only whole words (\\b boundaries)");
        });
        // An invalid regex is reported HERE, next to the toggle that enabled it
        // — never swallowed into a bare "no matches" and never a panic.
        if let Some(err) = self.find_error_text() {
            ui.colored_label(Color32::from_rgb(0xe5, 0x3e, 0x3e), err);
        }
    }

    /// F-008 — Replace `find_query` with `replace_query` in the active
    /// buffer. `all=true` replaces every match; `all=false` replaces only the
    /// first. Matches with the SAME engine AND the SAME toggles the find bar
    /// highlights with ([`find_query_flags`](ScribeApp::find_query_flags) —
    /// regex / match-case / whole-word), so Replace touches exactly what the
    /// user sees highlighted. Skips when the find field is empty.
    ///
    /// Root cause of the prior divergence: this used `str::replace` /
    /// `str::find`, which are case-SENSITIVE, while the find bar highlights
    /// case-INSENSITIVELY — so "Replace All" silently skipped the case-variant
    /// matches the find bar had highlighted (find said 3, replace changed 1).
    /// Unifying both surfaces on the one engine eliminates the divergence.
    ///
    /// The substitution runs through [`scribe_core::search::replace_n`]
    /// (`Some(1)` for "Replace next", `None` for "Replace all") rather than a
    /// hand-rolled splice, so there is ONE replacement engine. `replace_n`
    /// gates `$1` capture expansion on `q.regex`: with the regex toggle ON a
    /// capture-group replacement works, and with it OFF a `$` in the
    /// replacement lands verbatim.
    ///
    /// An invalid regex reports the compile error on the bar's inline error
    /// line and leaves the buffer untouched — never a panic, and never a silent
    /// fallback to a literal search.
    pub(super) fn replace_in_active(&mut self, all: bool) {
        if self.find_query.is_empty() || self.active >= self.tabs.len() {
            return;
        }
        let pat = self.find_query.clone();
        let rep = self.replace_query.clone();
        let q = self.find_query_flags();
        let matches = match scribe_core::search::find_all(&self.tabs[self.active].text, &q) {
            Ok(m) => m,
            Err(e) => {
                let msg = format!("bad regex: {e}");
                *self.find_error.borrow_mut() = Some(msg.clone());
                self.status = msg;
                return;
            }
        };
        *self.find_error.borrow_mut() = None;
        if matches.is_empty() {
            self.status = format!("no match for '{pat}'");
            return;
        }
        let limit = if all { None } else { Some(1) };
        let replaced =
            match scribe_core::search::replace_n(&self.tabs[self.active].text, &q, &rep, limit) {
                Ok(s) => s,
                Err(e) => {
                    let msg = format!("bad regex: {e}");
                    *self.find_error.borrow_mut() = Some(msg.clone());
                    self.status = msg;
                    return;
                }
            };
        // Through the `set_text` seam, NEVER `tabs[i].text = ...`. A direct
        // write leaves the persistent `rope_buf` holding PRE-replace content;
        // on the rope path `frame_tick` only rebuilds the rope when
        // `rope_buf.is_none()`, so the stale rope survives the replace and the
        // next keystroke's `tab.text = rope.to_string()` write-back silently
        // restores the pre-replace buffer — the user's replacement is gone.
        // `set_text` clears `rope_buf`, invalidates `rope_state` (so Undo
        // cannot resurrect a buffer that no longer exists) and bumps
        // `edit_gen`, which is what invalidates the gen-keyed minimap /
        // spellcheck / change-bar caches — so no separate bump is needed here.
        self.tabs[self.active].set_text(replaced);
        self.status = if all {
            format!("replaced {} x '{pat}' -> '{rep}'", matches.len())
        } else {
            format!("replaced '{pat}' -> '{rep}'")
        };
        // P2-C: a find/replace splice moves offsets out from under any
        // multi-cursor set — drop the now-stale carets.
        self.mc_clear_carets();
    }

    /// Wave-5 / 4-02: run the project-wide search over the open folder into the
    /// results pane. Reuses the in-buffer find engine + the open file-tree root.
    ///
    /// 4-02 — the fs walk + per-file scan runs OFF the egui frame thread on a
    /// spawned worker (`find_in_files::spawn_search`), streaming results back
    /// over `find_in_files_rx`. The UI shows partial results as they arrive and
    /// never blocks on a big tree. Starting a new search drops the previous
    /// receiver (assigning `Some(rx)` over the old one), which makes the orphaned
    /// worker's next send fail and stop the walk — the latest query supersedes
    /// the old one without an explicit cancellation flag.
    pub(super) fn run_find_in_files(&mut self, ctx: &egui::Context) {
        self.find_in_files_error = None;
        self.find_in_files_results.clear();
        // PA-02: a fresh search invalidates the old keyboard-selection index.
        self.find_in_files_selected = 0;
        // Dropping any in-flight receiver supersedes the previous search.
        self.find_in_files_rx = None;
        self.find_in_files_running = false;
        let Some(root) = self.file_tree_root.clone() else {
            self.find_in_files_error = Some("open a folder first".into());
            return;
        };
        // Every flag comes from the panel's own toggles. `case_sensitive` and
        // `whole_word` were hard-coded `false` here, so two of the three modes
        // the engine supports were unreachable from the project-search panel.
        let query = scribe_core::search::Query {
            pattern: self.find_in_files_query.clone(),
            regex: self.find_in_files_regex,
            case_sensitive: self.find_in_files_case_sensitive,
            whole_word: self.find_in_files_whole_word,
        };
        if query.pattern.is_empty() {
            return;
        }
        // Surface a bad regex once (the per-file search swallows it silently).
        if query.regex {
            if let Err(e) = scribe_core::search::find_all("", &query) {
                self.find_in_files_error = Some(format!("bad regex: {e}"));
                return;
            }
        }
        // Spawn the walk off-thread; each streamed batch requests a repaint so
        // the partial results land promptly even while the user is idle.
        let ctx = ctx.clone();
        self.find_in_files_rx = Some(crate::find_in_files::spawn_search(root, query, move || {
            ctx.request_repaint();
        }));
        self.find_in_files_running = true;
        self.status = "searching…".into();
    }

    /// 4-02 — drain any batches the off-thread project-find worker streamed back,
    /// appending them to the results pane. Called once per frame from
    /// `frame_tick`. Non-blocking (`try_recv`); when `Done` arrives (or the
    /// channel disconnects) the receiver is dropped and the running flag clears.
    pub(super) fn drain_find_in_files(&mut self) {
        let Some(rx) = self.find_in_files_rx.as_ref() else {
            return;
        };
        let mut finished = false;
        loop {
            match rx.try_recv() {
                Ok(crate::find_in_files::SearchMsg::Batch(batch)) => {
                    self.find_in_files_results.extend(batch);
                }
                Ok(crate::find_in_files::SearchMsg::Done) => {
                    finished = true;
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    finished = true;
                    break;
                }
            }
        }
        if finished {
            self.find_in_files_rx = None;
            self.find_in_files_running = false;
            self.status = format!("{} match(es)", self.find_in_files_results.len());
        }
    }

    /// Wave-5: open `path` in a tab (reusing the normal open path) then scroll to
    /// 1-based `line` (the click-to-open target from the results pane).
    pub(super) fn open_find_in_files_result(&mut self, path: PathBuf, line: usize) {
        self.open_path(path);
        self.goto_line(line.max(1));
    }
}
