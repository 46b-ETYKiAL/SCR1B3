//! Notes / PKM surface: the vault note-list pane, cross-note operator search,
//! and clickable wiki-links + backlinks.
//!
//! This wires the pure `scribe_core::notes` parser foundation into the running
//! app. The one security invariant: EVERY wiki-link / title the UI turns into a
//! filesystem path goes through `notes::vault_path::resolve_in_vault`, which
//! rejects `..` / absolute / drive-letter targets — a link can never escape the
//! configured vault. The pane never bypasses that chokepoint.
//!
//! The vault walk + per-note metadata (title, tags, outgoing links, body) build
//! a `NoteDoc` index; the note-list, search filter, and backlink collection all
//! read from that one index. The index is rescanned when the pane opens, when
//! the vault changes, or on an explicit refresh — not every frame.

#![allow(clippy::wildcard_imports)]

use super::*;
use egui::{Color32, RichText};
use scribe_core::notes::{completion, meta, query, tag_tree, vault_path, wikilink};
use std::path::{Path, PathBuf};

/// Note-like file extensions the vault walk indexes. A vault holds prose notes,
/// so this is deliberately narrow (not "every text file").
const NOTE_EXTS: &[&str] = &["md", "markdown", "mdown", "mkd", "txt", "text"];

/// Directories never descended into (build output / VCS / caches). Mirrors the
/// find-in-files skip list so the note index and project search agree.
const SKIP_DIRS: &[&str] = &[
    "target",
    "node_modules",
    "build",
    "dist",
    "out",
    "__pycache__",
    ".git",
    ".obsidian",
];

/// Caps so a huge folder can't stall the scan or exhaust memory. Notes are
/// small; a vault with more than this is almost certainly a mis-pointed folder.
const MAX_NOTES: usize = 20_000;
const MAX_NOTE_BYTES: u64 = 1 << 20; // 1 MiB — a prose note never approaches this
const MAX_BODY_KEPT: usize = 256 * 1024; // cap the body kept in the index for search

/// One indexed note: absolute path, vault-relative display path, derived title,
/// tag set, outgoing wiki-link targets, and the (capped) body for free-text
/// search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NoteDoc {
    pub path: PathBuf,
    /// Vault-relative, `/`-separated (e.g. `projects/roadmap.md`).
    pub rel_display: String,
    pub title: String,
    pub tags: Vec<String>,
    /// Outgoing wiki-link targets (raw, non-empty, non-embed), in document order.
    pub links: Vec<String>,
    /// Body text (capped) for free-text search matching.
    pub body: String,
}

impl NoteDoc {
    /// Build the borrowed view the search matcher consumes.
    fn as_ref(&self) -> query::NoteRef<'_> {
        query::NoteRef {
            rel_path: &self.rel_display,
            title: &self.title,
            tags: &self.tags,
            body: &self.body,
        }
    }
}

/// Walk `vault` and build the note index. Hidden entries (basename starting with
/// `.`) and `SKIP_DIRS` are pruned; only `NOTE_EXTS` files are read; oversized
/// files and the `MAX_NOTES` cap bound the work. Deterministic order (sorted by
/// relative path) so the list is stable frame-to-frame.
pub(crate) fn scan_vault(vault: &Path) -> Vec<NoteDoc> {
    let mut out: Vec<NoteDoc> = Vec::new();
    let mut stack = vec![vault.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if out.len() >= MAX_NOTES {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            let base = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if base.starts_with('.') {
                continue;
            }
            let Ok(meta_fs) = entry.path().symlink_metadata() else {
                continue;
            };
            if meta_fs.is_dir() {
                if !SKIP_DIRS.contains(&base) {
                    stack.push(p);
                }
            } else if meta_fs.is_file() {
                if meta_fs.len() > MAX_NOTE_BYTES || !has_note_ext(&p) {
                    continue;
                }
                if let Some(doc) = build_doc(vault, &p) {
                    out.push(doc);
                    if out.len() >= MAX_NOTES {
                        break;
                    }
                }
            }
        }
    }
    out.sort_by_key(|d| d.rel_display.to_lowercase());
    out
}

/// True when `path` has a recognised note extension (case-insensitive).
fn has_note_ext(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .is_some_and(|e| NOTE_EXTS.contains(&e.as_str()))
}

/// Read + parse one note file into a [`NoteDoc`]. Returns `None` when the file is
/// unreadable (binary / permission) — never panics.
fn build_doc(vault: &Path, path: &Path) -> Option<NoteDoc> {
    let content = std::fs::read_to_string(path).ok()?;
    let rel_display = relative_display(vault, path);
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("note");
    let title = meta::note_title(&content, stem);
    let tags = meta::note_tags(&content);
    let links = wikilink::extract_wikilinks(&content)
        .into_iter()
        .filter(|l| !l.embed && !l.target.is_empty())
        .map(|l| l.target)
        .collect();
    let mut body = content;
    let cut = body_cap_end(&body, MAX_BODY_KEPT);
    body.truncate(cut);
    Some(NoteDoc {
        path: path.to_path_buf(),
        rel_display,
        title,
        tags,
        links,
        body,
    })
}

/// The byte index at which to cut `body` so the kept prefix is at most `max`
/// bytes AND ends on a `char` boundary (never mid-UTF-8). Returns `body.len()`
/// when the body already fits, so the caller's `truncate` is then a no-op.
///
/// Pure + unit-tested, and deliberately expressed as a reverse boundary SEARCH
/// rather than a hand-rolled `end -= 1` loop: the loop form has an index-walk
/// whose off-by-one/step mutations are either unobservable or non-terminating,
/// so the cut index could regress with nothing to catch it. A wrong answer here
/// is not cosmetic — `String::truncate` PANICS on a non-boundary index.
pub(crate) fn body_cap_end(body: &str, max: usize) -> usize {
    if body.len() <= max {
        return body.len();
    }
    // Index 0 is always a char boundary, so the search always finds one.
    (0..=max)
        .rev()
        .find(|&i| body.is_char_boundary(i))
        .unwrap_or(0)
}

/// The vault-relative, `/`-separated display path for `path`. Falls back to the
/// file name when `path` is not under `vault` (should not happen for a walk of
/// the vault, but never panics).
fn relative_display(vault: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(vault).unwrap_or(path);
    rel.to_string_lossy().replace('\\', "/")
}

/// The notes matching `q` (an already-parsed query), sorted stably. When the
/// query is empty every note is returned. Pure over the index — testable.
pub(crate) fn filter_docs<'a>(docs: &'a [NoteDoc], q: &query::NoteQuery) -> Vec<&'a NoteDoc> {
    docs.iter().filter(|d| q.matches(d.as_ref())).collect()
}

/// Collect the notes that link TO `target_path` (backlinks): a note B backlinks
/// to A when one of B's outgoing wiki-links resolves (through the vault-safety
/// gate) to A's path. Comparison uses the host FS identity helper so a
/// casefold-/separator-different path on Windows still matches.
pub(crate) fn collect_backlinks<'a>(
    docs: &'a [NoteDoc],
    vault: &Path,
    target_path: &Path,
) -> Vec<&'a NoteDoc> {
    let target_canon =
        std::fs::canonicalize(target_path).unwrap_or_else(|_| target_path.to_path_buf());
    docs.iter()
        .filter(|d| {
            // Never list a note as its own backlink.
            !paths_equal(&d.path, target_path)
                && d.links.iter().any(|link| {
                    vault_path::resolve_in_vault(vault, link, Some("md"))
                        .ok()
                        .map(|resolved| {
                            let rc = std::fs::canonicalize(&resolved).unwrap_or(resolved);
                            paths_equal(&rc, &target_canon)
                        })
                        .unwrap_or(false)
                })
        })
        .collect()
}

/// Host-FS path identity (case-/separator-insensitive on Windows).
fn paths_equal(a: &Path, b: &Path) -> bool {
    scribe_core::path_norm::paths_equal_for_compare(a, b)
}

/// The backlink rows (path + title) to list for the active note, or an empty
/// list when either the vault or the active note's path is unknown.
///
/// Split out of the pane render so the "no vault OR no file-backed active tab ⇒
/// no backlinks" rule is pure and unit-testable — inside the render closure its
/// only evidence would be painted pixels.
pub(crate) fn backlink_rows(
    docs: &[NoteDoc],
    vault: Option<&Path>,
    active_path: Option<&Path>,
) -> Vec<(PathBuf, String)> {
    match (vault, active_path) {
        (Some(v), Some(ap)) => collect_backlinks(docs, v, ap)
            .into_iter()
            .map(|d| (d.path.clone(), d.title.clone()))
            .collect(),
        _ => Vec::new(),
    }
}

/// Fraction of the pane's remaining height the note LIST scroll area may take,
/// leaving the rest for the links-out / backlinks sections below it.
const NOTES_LIST_HEIGHT_FRACTION: f32 = 0.55;

/// Max height of the note-list scroll area given the pane's `available` height.
/// Extracted so the split is a tested number rather than an inline literal whose
/// only witness is a rendered frame.
pub(crate) fn notes_list_height(available: f32) -> f32 {
    available * NOTES_LIST_HEIGHT_FRACTION
}

/// Whether the note list should show its "no notes match" hint: only once a
/// vault IS configured (with no vault the pane already shows the
/// choose-a-folder prompt, and "no notes match" would misdescribe it) AND the
/// filter selected nothing.
pub(crate) fn show_no_match_hint(vault_configured: bool, filtered_len: usize) -> bool {
    vault_configured && filtered_len == 0
}

/// Whether the note-list row for `row_path` is the note currently open in the
/// active tab (drawn selected).
pub(crate) fn is_active_row(active_path: Option<&Path>, row_path: &Path) -> bool {
    active_path == Some(row_path)
}

impl ScribeApp {
    /// Rescan the configured vault into `self.note_index`, recording which vault
    /// the index is for. A no-op (clears the index) when no vault is configured.
    ///
    /// There is deliberately no `vault.is_dir()` pre-check: [`scan_vault`] on a
    /// path that is not a readable directory already yields an empty index and
    /// the root is recorded either way, so a guard would produce byte-identical
    /// state on both arms — an untestable branch that only looks like a safety
    /// net.
    pub(super) fn notes_refresh_index(&mut self) {
        match self.config.notes.vault_dir.clone() {
            Some(vault) => {
                self.note_index = scan_vault(&vault);
                self.note_index_root = Some(vault);
            }
            None => {
                self.note_index.clear();
                self.note_index_root = None;
            }
        }
    }

    /// Ensure the index matches the current vault (rescan on first open or on a
    /// vault change). Cheap when already in sync.
    pub(super) fn notes_ensure_index(&mut self) {
        if self.note_index_root.as_deref() != self.config.notes.vault_dir.as_deref() {
            self.notes_refresh_index();
        }
    }

    /// Resolve a wiki-link `target` inside the vault, creating the note (seeded
    /// with a `# Title` heading) when it does not yet exist, then open it in a
    /// tab. A target that escapes the vault (`..`, absolute, drive letter) is
    /// REJECTED at `resolve_in_vault` and surfaced as a toast — never followed.
    pub(super) fn open_or_create_wikilink(&mut self, target: &str) {
        let Some(vault) = self.config.notes.vault_dir.clone() else {
            self.toast = Some("Choose a notes folder first (the Notes pane).".into());
            return;
        };
        match vault_path::resolve_in_vault(&vault, target, Some("md")) {
            Ok(path) => {
                if !path.exists() {
                    if let Some(parent) = path.parent() {
                        if let Err(e) = std::fs::create_dir_all(parent) {
                            tracing::warn!("create note dir failed: {e}");
                            self.toast = Some("Couldn't create the note's folder.".into());
                            return;
                        }
                    }
                    let heading = target.rsplit(['/', '\\']).next().unwrap_or(target).trim();
                    let seed = format!("# {heading}\n");
                    if let Err(e) = std::fs::write(&path, seed) {
                        tracing::warn!("create note failed: {e}");
                        self.toast = Some("Couldn't create the note.".into());
                        return;
                    }
                    // The vault changed on disk — force a rescan so the new note
                    // shows in the list and backlinks resolve to it.
                    self.note_index_root = None;
                }
                self.open_path(path);
                self.notes_ensure_index();
            }
            Err(reject) => {
                // The security-relevant path: a traversal/absolute link is refused
                // with the reason, never opened.
                self.toast = Some(format!("Can't open that link — {}.", reject.message()));
            }
        }
    }

    /// Render the vault note-list pane (left side panel), the operator search
    /// box, the active note's outgoing wiki-links, and its backlinks. Wired into
    /// `frame_tick` behind the `notes_pane_open` toggle.
    pub(super) fn render_notes_pane(
        &mut self,
        ctx: &egui::Context,
        panel: Color32,
        accent: Color32,
        muted: Color32,
    ) {
        if !self.notes_pane_open {
            return;
        }
        self.notes_ensure_index();

        // Pre-compute everything that reads the index / active tab BEFORE the
        // panel closure, so the closure borrows no `self` field it also mutates.
        let vault = self.config.notes.vault_dir.clone();
        let parsed = query::parse(&self.notes_filter);
        let filtered: Vec<(PathBuf, String, String)> = filter_docs(&self.note_index, &parsed)
            .into_iter()
            .map(|d| (d.path.clone(), d.title.clone(), d.rel_display.clone()))
            .collect();
        let total = self.note_index.len();

        // Active-note context: outgoing links from the LIVE buffer + backlinks.
        let active = self.active.min(self.tabs.len().saturating_sub(1));
        let active_text = self
            .tabs
            .get(active)
            .map(|t| t.text.clone())
            .unwrap_or_default();
        let active_path = self
            .tabs
            .get(active)
            .and_then(|t| t.doc.path().map(Path::to_path_buf));
        let outgoing = outgoing_links(&active_text);
        let backlinks: Vec<(PathBuf, String)> =
            backlink_rows(&self.note_index, vault.as_deref(), active_path.as_deref());

        // The tag tree + the completion pools it feeds. Both are derived from the
        // index (no stored state), so they can never go stale against it.
        let tag_nodes = tag_tree::build(self.note_index.iter().map(|d| d.tags.as_slice()));
        let selected_tag = selected_tag_from_filter(&self.notes_filter);
        let tag_pool: Vec<String> = tag_nodes.iter().map(|n| n.tag.clone()).collect();
        let title_pool: Vec<String> = self.note_index.iter().map(|d| d.title.clone()).collect();

        // Take the filter string out so the text field can borrow it mutably
        // without touching `self` inside the closure.
        let mut filter = std::mem::take(&mut self.notes_filter);
        let mut open_target: Option<PathBuf> = None;
        let mut follow_link: Option<String> = None;
        let mut set_filter: Option<String> = None;
        let mut pick_folder = false;
        let mut do_refresh = false;
        let mut close_pane = false;

        egui::SidePanel::left("notes-pane")
            .resizable(true)
            .default_width(260.0)
            .frame(egui::Frame::default().fill(panel).inner_margin(6.0))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("NOTES").color(accent).small().monospace());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("×").on_hover_text("Close the notes pane").clicked() {
                            close_pane = true;
                        }
                        if ui.small_button("⟳").on_hover_text("Rescan the vault").clicked() {
                            do_refresh = true;
                        }
                        if ui.small_button("📁").on_hover_text("Choose the notes folder").clicked() {
                            pick_folder = true;
                        }
                    });
                });
                match &vault {
                    Some(v) => {
                        ui.label(
                            RichText::new(v.display().to_string())
                                .color(muted)
                                .small()
                                .monospace(),
                        )
                        .on_hover_text(v.display().to_string());
                    }
                    None => {
                        ui.label(
                            RichText::new("No notes folder chosen yet.")
                                .color(muted)
                                .small(),
                        );
                        if ui.button("Choose a notes folder…").clicked() {
                            pick_folder = true;
                        }
                    }
                }
                ui.separator();

                // ---- operator search / filter ----
                ui.horizontal(|ui| {
                    ui.label(RichText::new("search").color(muted).small().monospace());
                    ui.text_edit_singleline(&mut filter)
                        .on_hover_text(
                            "Filter notes. Operators: tag:x  path:x  title:x  \"quoted phrase\"  -negation.  \
                             Type #… or [[… for completions.",
                        );
                });
                // ---- completions for a half-typed `#tag` / `[[note` ----
                // The caret is the end of the box (a single-line field the user
                // is typing into), so the trailing token is what gets completed.
                if let Some(suggestion) =
                    completion::complete(&filter, filter.len(), &tag_pool, &title_pool, MAX_SUGGESTIONS)
                {
                    ui.horizontal_wrapped(|ui| {
                        for candidate in &suggestion.candidates {
                            if ui
                                .small_button(suggestion_chip_label(suggestion.trigger, candidate))
                                .on_hover_text("Complete this into a search operator")
                                .clicked()
                            {
                                set_filter = Some(completion::apply(
                                    &filter,
                                    &suggestion,
                                    &filter_replacement(suggestion.trigger, candidate),
                                ));
                            }
                        }
                    });
                }
                ui.label(
                    RichText::new(format!("{} of {} notes", filtered.len(), total))
                        .color(muted)
                        .small(),
                );
                ui.separator();

                // ---- the note list ----
                egui::ScrollArea::vertical()
                    .max_height(notes_list_height(ui.available_height()))
                    .id_salt("notes-list-scroll")
                    .show(ui, |ui| {
                        if show_no_match_hint(vault.is_some(), filtered.len()) {
                            ui.label(RichText::new("no notes match").color(muted).small());
                        }
                        for (path, title, rel) in &filtered {
                            let is_active = is_active_row(active_path.as_deref(), path.as_path());
                            let row = ui.selectable_label(
                                is_active,
                                RichText::new(title).monospace().small(),
                            );
                            if row.on_hover_text(rel).clicked() {
                                open_target = Some(path.clone());
                            }
                        }
                    });

                // ---- the nested tag tree (click a node to filter the list) ----
                ui.separator();
                ui.collapsing(
                    RichText::new(format!("tags ({})", tag_nodes.len()))
                        .color(accent)
                        .small()
                        .monospace(),
                    |ui| {
                        if tag_nodes.is_empty() {
                            ui.label(RichText::new("no #tags in this vault").color(muted).small());
                        }
                        for node in &tag_nodes {
                            ui.horizontal(|ui| {
                                #[allow(clippy::cast_precision_loss)]
                                ui.add_space(node.depth as f32 * TAG_TREE_INDENT);
                                let is_selected =
                                    tag_row_is_selected(selected_tag.as_deref(), &node.tag);
                                let clicked = ui
                                    .selectable_label(
                                        is_selected,
                                        RichText::new(format!("#{}", node.segment))
                                            .monospace()
                                            .small(),
                                    )
                                    .on_hover_text(format!(
                                        "Filter the list to #{} (and anything nested under it)",
                                        node.tag
                                    ))
                                    .clicked();
                                ui.label(
                                    RichText::new(node.note_count.to_string())
                                        .color(muted)
                                        .small(),
                                );
                                if clicked {
                                    set_filter = Some(tag_filter_for(
                                        &node.tag,
                                        selected_tag.as_deref(),
                                    ));
                                }
                            });
                        }
                    },
                );

                // ---- outgoing links + backlinks for the active note ----
                ui.separator();
                ui.collapsing(
                    RichText::new(format!("links out ({})", outgoing.len()))
                        .color(accent)
                        .small()
                        .monospace(),
                    |ui| {
                        if outgoing.is_empty() {
                            ui.label(RichText::new("no [[wiki-links]] here").color(muted).small());
                        }
                        for link in &outgoing {
                            if ui
                                .add(egui::Label::new(
                                    RichText::new(link.display_text()).monospace().small(),
                                ).sense(egui::Sense::click()))
                                // The hover names the TARGET, not the label: an
                                // aliased link must not hide where it goes.
                                .on_hover_text(format!(
                                    "Open [[{}]] (creating it if missing)",
                                    link.target
                                ))
                                .clicked()
                            {
                                follow_link = Some(link.target.clone());
                            }
                        }
                    },
                );
                ui.collapsing(
                    RichText::new(format!("backlinks ({})", backlinks.len()))
                        .color(accent)
                        .small()
                        .monospace(),
                    |ui| {
                        if backlinks.is_empty() {
                            ui.label(
                                RichText::new("nothing links here yet").color(muted).small(),
                            );
                        }
                        for (path, title) in &backlinks {
                            if ui
                                .selectable_label(false, RichText::new(title).monospace().small())
                                .clicked()
                            {
                                open_target = Some(path.clone());
                            }
                        }
                    },
                );
            });

        // Restore the (possibly edited) filter and apply the deferred actions.
        // A tag-tree / completion click OVERRIDES what the text box held this
        // frame: both were computed from the same string the user is looking at,
        // and the click is the newer intent.
        self.notes_filter = set_filter.unwrap_or(filter);
        if close_pane {
            self.notes_pane_open = false;
        }
        if pick_folder {
            if let Some(folder) = super::dialogs::pick_folder() {
                self.config.notes.vault_dir = Some(folder.clone());
                self.save_config();
                self.notes_refresh_index();
                self.status = format!("notes vault: {}", folder.display());
            }
        }
        if do_refresh {
            self.notes_refresh_index();
            self.status = format!("rescanned vault: {} notes", self.note_index.len());
        }
        if let Some(path) = open_target {
            self.open_path(path);
        }
        if let Some(target) = follow_link {
            self.open_or_create_wikilink(&target);
        }
    }
}

/// One row of the active note's "links out" list: what to SHOW and what to OPEN.
///
/// The two are deliberately separate. `[[projects/roadmap|Q3 plan]]` should read
/// as "Q3 plan" (its author-chosen label) while still opening
/// `projects/roadmap` — showing the raw target throws the alias away, and
/// following the label would resolve a note that does not exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OutgoingLink {
    /// The raw link target — the string that goes through the vault-safety gate
    /// and is opened. Never derived from the label.
    pub target: String,
    /// The `#heading` anchor the link points at, if any.
    pub heading: Option<String>,
    /// [`wikilink::WikiLink::label`]: the `|alias` when the author wrote one,
    /// otherwise the target.
    pub label: String,
}

impl OutgoingLink {
    /// The text the row displays: the link's label, plus its `#heading` anchor
    /// when it points into a section (otherwise the anchor is invisible and two
    /// links into different sections of one note look identical).
    pub(crate) fn display_text(&self) -> String {
        match &self.heading {
            Some(h) => format!("{} › {h}", self.label),
            None => self.label.clone(),
        }
    }
}

/// The distinct, non-embed, non-empty wiki-links in `text`, in first-seen order.
/// Used to list the active note's outgoing links.
///
/// De-duplication is by TARGET, not by label: `[[Ideas]]` and `[[Ideas|notions]]`
/// open the same note, so they are one row (the first spelling's label wins).
pub(crate) fn outgoing_links(text: &str) -> Vec<OutgoingLink> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for link in wikilink::extract_wikilinks(text) {
        if link.embed || link.target.is_empty() {
            continue;
        }
        if seen.insert(link.target.clone()) {
            out.push(OutgoingLink {
                label: link.label(),
                heading: link.heading.clone(),
                target: link.target,
            });
        }
    }
    out
}

/// The tag currently selected in the tag tree, read back OUT of the filter
/// string so the tree's highlight can never disagree with what the list shows.
///
/// A tag is "selected" only when the filter is exactly one un-negated `tag:`
/// clause — the shape [`tag_filter_for`] writes. Anything the user has typed on
/// top of it (`tag:project roadmap`, `-tag:project`) leaves no node highlighted,
/// because no single node describes that result.
///
/// Deliberately parses with `query::parse` rather than string-matching `tag:`:
/// a second grammar here would drift from the one that actually filters.
pub(crate) fn selected_tag_from_filter(filter: &str) -> Option<String> {
    match query::parse(filter).clauses.as_slice() {
        [query::Clause {
            negated: false,
            term: query::Term::Tag(tag),
        }] => Some(tag.clone()),
        _ => None,
    }
}

/// Whether the tag-tree row for `node_tag` should draw as the selected one.
///
/// Split out of the row renderer because the selected state is drawn by
/// `Button::selectable`, and egui 0.34 does NOT report `selected` in that
/// widget's `WidgetInfo` — so the highlight reaches the accessibility tree
/// nowhere and is observable only as painted pixels. Inline, the rule was
/// therefore untestable; named, it is a pure predicate with a test, and the
/// row renderer is left with no decision of its own to get wrong.
///
/// It is the exact mirror of [`tag_filter_for`]'s toggle-off condition: the row
/// that a click would CLEAR is the row that draws as selected.
pub(crate) fn tag_row_is_selected(selected: Option<&str>, node_tag: &str) -> bool {
    selected == Some(node_tag)
}

/// The filter string a click on tag-tree node `node_tag` should produce.
/// Clicking the already-selected node CLEARS the filter (toggle off) rather than
/// re-applying it, so the tree can undo itself without reaching for the text box.
pub(crate) fn tag_filter_for(node_tag: &str, selected: Option<&str>) -> String {
    if selected == Some(node_tag) {
        String::new()
    } else {
        format!("tag:{node_tag}")
    }
}

/// The label a completion chip shows: the candidate in the sigil form the user
/// is typing, so the chip reads as a continuation of the token rather than as
/// the query operator it will expand to.
pub(crate) fn suggestion_chip_label(trigger: completion::Trigger, candidate: &str) -> String {
    match trigger {
        completion::Trigger::Tag => format!("#{candidate}"),
        completion::Trigger::Note => format!("[[{candidate}]]"),
    }
}

/// The search-filter text an accepted completion expands to. The sigils are
/// SHORTHAND for the operators the query grammar already understands — `#x`
/// means `tag:x`, `[[Name` means `title:"Name"` — so accepting a chip produces
/// a filter the parser genuinely matches on rather than literal sigil text that
/// would fall through to a free-text search.
///
/// The note form is quoted because a title may contain spaces, which would
/// otherwise tokenise into a second, unrelated clause.
pub(crate) fn filter_replacement(trigger: completion::Trigger, candidate: &str) -> String {
    match trigger {
        completion::Trigger::Tag => format!("tag:{candidate}"),
        completion::Trigger::Note => format!("title:\"{candidate}\""),
    }
}

/// How many completion chips the search box offers at once.
const MAX_SUGGESTIONS: usize = 6;

/// Indent, in points, per tag-tree nesting level.
const TAG_TREE_INDENT: f32 = 10.0;

#[cfg(test)]
mod tests {
    use crate::app::ScribeApp;
    use scribe_core::config::Config;

    /// A temp vault with two cross-linked notes.
    fn temp_vault() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp vault");
        std::fs::write(
            dir.path().join("Home.md"),
            "# Home\n\nSee [[Ideas]]. #project\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("Ideas.md"),
            "# Ideas\n\nBack to [[Home]].\n",
        )
        .unwrap();
        dir
    }

    fn app_with_vault(vault: &std::path::Path) -> ScribeApp {
        let mut cfg = Config::default();
        cfg.editor.first_run_completed = true;
        cfg.notes.vault_dir = Some(vault.to_path_buf());
        ScribeApp::new_test(cfg)
    }

    /// `notes_ensure_index` must actually scan the configured vault into the
    /// index — the wire that makes the note list real. A cut wire would leave the
    /// index empty and the pane blank.
    #[test]
    fn ensure_index_populates_from_the_vault() {
        let vault = temp_vault();
        let mut app = app_with_vault(vault.path());
        assert!(app.note_index.is_empty(), "index starts empty");
        app.notes_ensure_index();
        assert_eq!(app.note_index.len(), 2, "both notes indexed");
        let titles: Vec<&str> = app.note_index.iter().map(|d| d.title.as_str()).collect();
        assert!(titles.contains(&"Home") && titles.contains(&"Ideas"));
    }

    /// The ToggleNotesPane command must flip the pane's visibility — the wire
    /// between the command palette / keybind and the pane render.
    #[test]
    fn toggle_command_flips_the_pane() {
        let vault = temp_vault();
        let mut app = app_with_vault(vault.path());
        assert!(!app.notes_pane_open);
        app.execute_builtin(crate::app::commands::BuiltinCommand::ToggleNotesPane);
        assert!(app.notes_pane_open, "toggle opens the pane");
        app.execute_builtin(crate::app::commands::BuiltinCommand::ToggleNotesPane);
        assert!(!app.notes_pane_open, "toggle closes it again");
    }

    /// SECURITY: a wiki-link that tries to escape the vault (`..`, absolute) must
    /// be REFUSED — no file created outside the vault, a toast shown instead.
    #[test]
    fn wikilink_traversal_cannot_escape_the_vault() {
        let vault = temp_vault();
        let mut app = app_with_vault(vault.path());
        let before = std::fs::read_dir(vault.path()).unwrap().count();
        app.open_or_create_wikilink("../../pwned");
        assert!(
            app.toast.is_some(),
            "a traversal link is refused with a toast, not followed"
        );
        // No note was created (inside OR by escaping) for the rejected link.
        let after = std::fs::read_dir(vault.path()).unwrap().count();
        assert_eq!(
            before, after,
            "no file created for a rejected traversal link"
        );
        assert!(
            !vault.path().parent().unwrap().join("pwned.md").exists(),
            "the link must not have written outside the vault"
        );
    }
}
