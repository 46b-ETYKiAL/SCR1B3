//! Editor overlay/popup methods for `ScribeApp`: modal keyboard ownership,
//! the autocomplete popup (open/accept), the minimap, and the fold view.
//! Extracted from `mod.rs` (A-01 wave 3 — behavior-preserving move; methods
//! widened to `pub(super)` for the parent + sibling call-sites incl
//! `frame_tick`).
#![allow(clippy::wildcard_imports)]

use super::*;

/// Base point-size the minimap galley is laid out at to MEASURE the document's
/// intrinsic minimap height before fit-to-height scaling.
const MINIMAP_BASE_PT: f32 = 3.0;
/// Floor on the *drawn* minimap font size. Below this, glyphs stop carrying
/// useful information. Documents so tall that fit-to-height would shrink the
/// font under this floor keep the floor and switch to the co-scrolling
/// proportional-slider model so the minimap stays legible (the threshold is
/// `natural_h > (BASE/MIN) * panel_h`, i.e. ~3× the panel height).
const MINIMAP_MIN_PT: f32 = 1.0;

/// Geometry of the minimap viewport indicator + content offset, in panel-local
/// pixels. Returned by [`minimap_geometry`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct MinimapGeom {
    /// Indicator-box top, as an offset from the panel top (px).
    pub ind_top: f32,
    /// Indicator-box height (px).
    pub ind_h: f32,
    /// Vertical offset (≤ 0) to translate the drawn galley by so its rows
    /// co-scroll with the indicator when the document is taller than the panel.
    /// `0.0` whenever the whole document fits the panel.
    pub map_offset: f32,
}

/// Compute the minimap viewport-indicator geometry from the editor's real scroll
/// metrics and the ACTUAL drawn minimap-galley height.
///
/// This is the load-bearing accuracy invariant, kept as a pure free function (no
/// egui state, no GPU) so the scale-unification can be unit-tested: the indicator
/// is `view_h/content_h` of `drawn_h` at offset `off_y/content_h` — the SAME
/// `drawn_h` the content is painted at. The historical bug multiplied the
/// indicator by an extra `scale` factor the content draw never applied; that can
/// never reappear without this function failing its tests.
///
/// * `scroll` — editor metrics `(off_y, content_h, view_h)` in editor px.
/// * `panel_h` — minimap panel inner height (px).
/// * `drawn_h` — actual pixel height of the minimap galley as painted (already
///   scaled to fit, or floored-and-taller for huge files).
pub(super) fn minimap_geometry(scroll: (f32, f32, f32), panel_h: f32, drawn_h: f32) -> MinimapGeom {
    let (off_y, content_h, view_h) = scroll;
    let content_h = content_h.max(1.0);
    let panel_h = panel_h.max(1.0);
    let drawn_h = drawn_h.max(1.0);
    let off_y = off_y.max(0.0);
    // Indicator height = editor's visible fraction of the document mapped onto
    // the drawn minimap content — the SAME scale as the content.
    let ind_h = ((view_h / content_h) * drawn_h).clamp(0.0, drawn_h);
    if drawn_h <= panel_h + 0.5 {
        // Fit-to-height (the normal case after scaling, and all short docs): the
        // whole document occupies the panel; the box maps 1:1 onto the content.
        let ind_top = ((off_y / content_h) * drawn_h).clamp(0.0, (drawn_h - ind_h).max(0.0));
        MinimapGeom {
            ind_top,
            ind_h,
            map_offset: 0.0,
        }
    } else {
        // Huge file: drawn content is taller than the panel even at the floored
        // font. Co-scroll content + slider (VS Code proportional model) so the
        // slider stays in view and overlays the matching rows.
        let scroll_range = (content_h - view_h).max(1.0);
        let f = (off_y / scroll_range).clamp(0.0, 1.0);
        let slider_travel = (panel_h - ind_h).max(0.0);
        let map_travel = (drawn_h - panel_h).max(0.0);
        MinimapGeom {
            ind_top: f * slider_travel,
            ind_h,
            map_offset: -(f * map_travel),
        }
    }
}

/// A vertical lane of the minimap's overview ruler.
///
/// Each decoration class owns its own lane so a search hit can never hide an
/// error, and an error can never hide a change — the failure mode of painting
/// them all in one strip is that the marker you most needed to see is the one
/// that got overdrawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MarkLane {
    /// Lines edited this session. Left edge — the same side as the gutter change
    /// bar it mirrors, so the eye reads them as the same information.
    Change,
    /// Live find-bar matches. Centre.
    Search,
    /// LSP diagnostics for the active document. Right edge.
    Error,
}

/// Horizontal geometry `(x_offset_from_panel_left, width)` of `lane` in a panel
/// `width` px wide.
///
/// Proportional with a clamp, because the minimap is user-resizable across
/// `48..=260` px (`width_range`): a fixed pixel width is either invisible at
/// 260 or eats the whole strip at 48.
pub(super) fn mark_lane(width: f32, lane: MarkLane) -> (f32, f32) {
    let w = (width * 0.12).clamp(2.0, 6.0);
    let x = match lane {
        MarkLane::Change => 0.0,
        MarkLane::Search => ((width - w) * 0.5).max(0.0),
        MarkLane::Error => (width - w).max(0.0),
    };
    (x, w.min(width.max(0.0)))
}

/// Panel-local Y of the overview mark for 0-based `line`.
///
/// Uses the SAME transform the minimap content is painted with — the galley is
/// drawn at `rect.top() + map_offset` with height `drawn_h` — so a mark sits over
/// the row it describes rather than floating near it. That shared-scale
/// discipline is the same invariant [`minimap_geometry`] exists to protect.
///
/// Exact when `editor.word_wrap` is OFF, which is the mode the minimap already
/// documents as one minimap row per logical line. With wrap ON both the editor
/// and the minimap wrap, so line-fraction is proportional rather than exact —
/// the same approximation the viewport indicator already makes.
pub(super) fn mark_y(line: usize, total_lines: usize, drawn_h: f32, map_offset: f32) -> f32 {
    let total = total_lines.max(1) as f32;
    let frac = (line as f32 / total).clamp(0.0, 1.0);
    map_offset + frac * drawn_h
}

/// Minimum vertical separation between two painted marks, in px. Below this the
/// two rectangles are the same pixel row and the second adds nothing.
const MARK_MIN_GAP: f32 = 2.0;
/// Hard ceiling on painted marks per lane. A 200k-line file with a one-character
/// search query would otherwise queue a shape per hit and stall the frame; the
/// merge below normally gets there first, this is the backstop.
const MARK_MAX_PER_LANE: usize = 512;

/// Collapse marks that would land on the same pixel row, preserving order.
///
/// Returns at most [`MARK_MAX_PER_LANE`] entries. `ys` need not be sorted; it is
/// sorted here because match/diagnostic sources do not guarantee line order.
pub(super) fn merge_marks(mut ys: Vec<f32>, min_gap: f32) -> Vec<f32> {
    ys.retain(|y| y.is_finite());
    ys.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mut out: Vec<f32> = Vec::new();
    for y in ys {
        if out.last().is_none_or(|last| y - last >= min_gap) {
            out.push(y);
            if out.len() >= MARK_MAX_PER_LANE {
                break;
            }
        }
    }
    out
}

/// 0-based line index of each byte offset in `offsets`, for `text`.
///
/// One forward pass over the buffer for the whole batch (offsets are sorted
/// first), rather than a `text[..off].lines().count()` per offset — that form is
/// O(n·m) and would re-walk a large document once per search hit.
pub(super) fn lines_of_offsets(text: &str, offsets: &[usize]) -> Vec<usize> {
    let mut sorted: Vec<usize> = offsets.to_vec();
    sorted.sort_unstable();
    let mut out = Vec::with_capacity(sorted.len());
    let mut line = 0usize;
    let mut cursor = 0usize;
    let bytes = text.as_bytes();
    for off in sorted {
        let off = off.min(bytes.len());
        while cursor < off {
            if bytes[cursor] == b'\n' {
                line += 1;
            }
            cursor += 1;
        }
        out.push(line);
    }
    out
}

impl ScribeApp {
    /// True when a modal with a focused text field or arrow-key navigation
    /// currently owns the keyboard (#72). The editor-surface completion popup
    /// must defer to these so its ↑↓/Enter interception cannot steal the modal
    /// field's keys. Kept as one method so the set is defined in exactly one
    /// place. NOTE: the passive display modals (welcome, cheatsheet, recent —
    /// no text entry, no arrow navigation) are deliberately EXCLUDED; they have
    /// no keys for completion to conflict with, and the first-run welcome flag
    /// must not suppress completion in the editor behind it.
    pub(super) fn modal_owns_keyboard(&self) -> bool {
        self.find_open
            || self.palette_open
            || self.settings_open
            || self.fuzzy_open
            || self.goto_open
            || self.goto_symbol_open
    }

    /// Open the identifier-completion popup for the prefix ending at `char_idx`
    /// in the active buffer. Sources suggestions from the buffer's own words
    /// (zero network / LSP dependency).
    pub(super) fn open_completion(&mut self, active: usize, char_idx: Option<usize>) {
        let Some(ci) = char_idx else {
            self.completion = None;
            return;
        };
        let text = &self.tabs[active].text;
        let byte = char_to_byte(text, ci);
        let (start, prefix) = crate::editor_features::prefix_before(text, byte);
        let items = crate::editor_features::word_completions(text, &prefix, 8);
        self.completion = (!items.is_empty()).then_some(Completion {
            prefix_start: start,
            items,
            selected: 0,
        });
    }

    /// Insert the selected completion, replacing the typed prefix.
    pub(super) fn accept_completion(&mut self, active: usize, char_idx: Option<usize>) {
        let Some(c) = self.completion.take() else {
            return;
        };
        let Some(ci) = char_idx else { return };
        let Some(item) = c.items.get(c.selected).cloned() else {
            return;
        };
        let text = &mut self.tabs[active].text;
        let byte = char_to_byte(text, ci);
        // `c.prefix_start` is a byte offset captured a frame EARLIER; the buffer
        // may have mutated since (e.g. an async edit between popup-open and
        // accept), leaving it mid-multibyte-char. `replace_range` panics on a
        // non-boundary offset → `panic = "abort"`. `char_to_byte` already clamps
        // `byte` to a boundary; re-validate `prefix_start` the same way before
        // splicing. On a stale offset we drop the completion rather than crash.
        if c.prefix_start <= byte && byte <= text.len() && text.is_char_boundary(c.prefix_start) {
            text.replace_range(c.prefix_start..byte, &item);
        }
        self.tabs[active].edit_gen = self.tabs[active].edit_gen.wrapping_add(1);
    }

    /// 0-based lines of the active buffer carrying a change-bar state, as
    /// `(unsaved, saved)`.
    ///
    /// Reads the SAME `change_states` the gutter change bar paints, so the
    /// overview ruler and the gutter can never disagree about which lines moved.
    /// Empty when the user has the change bar switched off — one toggle, both
    /// surfaces. Call [`Self::ensure_change_states`] first.
    pub(super) fn overview_change_lines(&self) -> (Vec<usize>, Vec<usize>) {
        if !self.config.editor.show_change_bar || self.active >= self.tabs.len() {
            return (Vec::new(), Vec::new());
        }
        let mut unsaved = Vec::new();
        let mut saved = Vec::new();
        for (line, state) in self.tabs[self.active].change_states.iter().enumerate() {
            match state {
                crate::change_bar::LineChange::Unsaved => unsaved.push(line),
                crate::change_bar::LineChange::Saved => saved.push(line),
                crate::change_bar::LineChange::None => {}
            }
        }
        (unsaved, saved)
    }

    /// 0-based lines of the active buffer holding a live find-bar match.
    ///
    /// Gated on the find bar being OPEN, not merely on the query being non-empty:
    /// the query string survives closing the bar, and leaving its marks painted
    /// afterwards would show hits for a search the user has finished with.
    pub(super) fn overview_search_lines(&self) -> Vec<usize> {
        if !self.find_open || self.active >= self.tabs.len() {
            return Vec::new();
        }
        let matches = self.find_matches_active();
        if matches.is_empty() {
            return Vec::new();
        }
        let offsets: Vec<usize> = matches.iter().map(|m| m.start).collect();
        lines_of_offsets(&self.tabs[self.active].text, &offsets)
    }

    /// `(0-based line, LSP severity)` for every error/warning diagnostic that
    /// belongs to the ACTIVE document.
    ///
    /// The uri filter is load-bearing: `self.diagnostics` is whatever the server
    /// last published, which after a tab switch is still the PREVIOUS file's
    /// diagnostics. Painting those over this document would mark lines that have
    /// nothing wrong with them. Info/hint (severity 3/4) are dropped — an
    /// overview ruler is a "where is the damage" instrument, and hint noise is
    /// what makes people stop trusting one.
    pub(super) fn overview_error_lines(&self) -> Vec<(usize, u8)> {
        if self.diagnostics.is_empty() || self.active >= self.tabs.len() {
            return Vec::new();
        }
        let Some(uri) = self.tabs[self.active].doc.path().map(path_to_uri) else {
            return Vec::new();
        };
        self.diagnostics
            .iter()
            .filter(|d| d.uri == uri && (d.severity == 1 || d.severity == 2))
            .map(|d| (d.line as usize, d.severity))
            .collect()
    }

    /// Paint the overview-ruler marks (changes / search hits / diagnostics) over
    /// the minimap content.
    ///
    /// Drawn AFTER the viewport indicator so a mark inside the current viewport
    /// stays readable through the indicator's translucent fill — the marks are
    /// the navigation signal, the indicator is context.
    fn paint_overview_marks(
        &self,
        painter: &egui::Painter,
        rect: egui::Rect,
        drawn_h: f32,
        map_offset: f32,
        total_lines: usize,
    ) {
        let width = rect.width();
        let lane = |lane: MarkLane, lines: Vec<usize>, color: Color32| {
            if lines.is_empty() {
                return;
            }
            let (dx, w) = mark_lane(width, lane);
            let ys = merge_marks(
                lines
                    .into_iter()
                    .map(|l| mark_y(l, total_lines, drawn_h, map_offset))
                    .collect(),
                MARK_MIN_GAP,
            );
            for y in ys {
                let top = rect.top() + y;
                // Skip marks scrolled outside the panel in the co-scrolling
                // huge-file regime; the painter clips, but not queueing the shape
                // at all is what keeps a 200k-line file cheap.
                if top < rect.top() - 2.0 || top > rect.bottom() + 2.0 {
                    continue;
                }
                painter.rect_filled(
                    egui::Rect::from_min_size(
                        egui::pos2(rect.left() + dx, top),
                        egui::vec2(w, MARK_MIN_GAP),
                    ),
                    0.0,
                    color,
                );
            }
        };

        let (unsaved, saved) = self.overview_change_lines();
        lane(
            MarkLane::Change,
            saved,
            ui_color(
                &self.theme,
                "change_bar_saved",
                Rgba::new(0x6f, 0xb8, 0x9a, 255),
            ),
        );
        lane(
            MarkLane::Change,
            unsaved,
            ui_color(
                &self.theme,
                "change_bar_unsaved",
                Rgba::new(0xf2, 0xb3, 0x3d, 255),
            ),
        );
        lane(
            MarkLane::Search,
            self.overview_search_lines(),
            ui_color(
                &self.theme,
                "minimap_search",
                Rgba::new(0xd8, 0xc7, 0x4a, 255),
            ),
        );
        let diagnostics = self.overview_error_lines();
        lane(
            MarkLane::Error,
            diagnostics
                .iter()
                .filter(|(_, sev)| *sev == 2)
                .map(|(l, _)| *l)
                .collect(),
            ui_color(&self.theme, "warning", Rgba::new(0xfb, 0xbf, 0x24, 255)),
        );
        // Errors last so a line that carries both an error and a warning reads
        // as the error.
        lane(
            MarkLane::Error,
            diagnostics
                .iter()
                .filter(|(_, sev)| *sev == 1)
                .map(|(l, _)| *l)
                .collect(),
            ui_color(&self.theme, "error", Rgba::new(0xe0, 0x5c, 0x5c, 255)),
        );
    }

    /// Render the minimap strip (rightmost): a memoized fit-to-height overview of
    /// the active document with an accurate viewport indicator; click/drag scrolls
    /// the editor.
    ///
    /// Accuracy invariant (the fix): the minimap CONTENT and the viewport
    /// INDICATOR share ONE scale. The document is squished to the panel height
    /// (fit-to-height, the user's request) by laying the drawn galley out at a
    /// scaled font; the indicator is then `view_h/content_h` of the SAME drawn
    /// height at offset `off_y/content_h`. Previously the content was drawn at its
    /// natural height while the indicator carried an extra `* scale` factor, so for
    /// any document taller than the panel the highlight sat over the wrong rows.
    pub(super) fn show_minimap(&mut self, ctx: &egui::Context, panel: Color32, accent: Color32) {
        // The overview ruler paints the change lane from the SAME cache the
        // gutter change bar uses. `frame_tick` refreshes it further down the
        // frame, so without this the minimap would trail the gutter by one edit.
        let active = self.active;
        self.ensure_change_states(active);
        egui::SidePanel::right("minimap")
            // #86 — the Map view is now user-resizable (was a fixed exact_width).
            // The minimap galley re-lays out to `available_size` each frame, so
            // it tracks the dragged width. Floor keeps it legible; ceiling stops
            // it eating the editor.
            .default_width(110.0)
            .width_range(48.0..=260.0)
            .resizable(true)
            .frame(egui::Frame::default().fill(panel).inner_margin(4.0))
            .show(ctx, |ui| {
                ui.label(RichText::new("MAP").color(accent).small().monospace());
                let avail = ui.available_size();
                let map_fg = Color32::from_rgb(0x8a, 0x88, 0x99);
                // P2: match the minimap's wrap behaviour to the editor's so a
                // logical line maps to the right minimap row. Editor word_wrap OFF
                // → NO wrap (one minimap row per logical line, exactly like the
                // editor, which scrolls horizontally), so the editor-pixel scroll
                // fraction maps proportionally onto the minimap. word_wrap ON →
                // wrap at the panel width (both galleys wrap).
                let word_wrap = self.config.editor.word_wrap;
                let wrap_w = if word_wrap { avail.x } else { f32::INFINITY };
                // (1) NATURAL galley at MINIMAP_BASE_PT — used to MEASURE the
                // document's intrinsic minimap height, and drawn directly for
                // short documents. Memoized (edit_gen, doc_id, width, word_wrap):
                // the owned String is built ONLY on a cache miss.
                let natural = {
                    use std::hash::{Hash, Hasher};
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    self.tabs[self.active].edit_gen.hash(&mut h);
                    self.tabs[self.active].doc_id.raw().hash(&mut h);
                    avail.x.to_bits().hash(&mut h);
                    (word_wrap as u8).hash(&mut h);
                    let key = h.finish();
                    let mut slot = self.minimap_cache.borrow_mut();
                    match slot.as_ref() {
                        Some((k, g)) if *k == key => g.clone(),
                        _ => {
                            // egui 0.34: layout caches into the FontsView so it now
                            // needs `&mut`; use fonts_mut(...) instead of fonts(...).
                            let g = ui.fonts_mut(|f| {
                                f.layout(
                                    self.tabs[self.active].text.clone(),
                                    FontId::monospace(MINIMAP_BASE_PT),
                                    map_fg,
                                    wrap_w,
                                )
                            });
                            *slot = Some((key, g.clone()));
                            g
                        }
                    }
                };
                let natural_h = natural.size().y.max(1.0);
                let (rect, resp) = ui.allocate_exact_size(avail, egui::Sense::click_and_drag());
                let panel_h = rect.height().max(1.0);
                let panel_h_q = panel_h.round().max(1.0);
                // (2)+(3) DRAWN galley. Short docs (already ≤ panel) draw at the
                // natural size. Tall docs are squished to the panel height
                // (fit-to-height — the user's request): lay out at a scaled font,
                // then apply ONE correction toward the panel height because egui's
                // per-row height is sub-linear at tiny sizes (a pure linear guess
                // under-fills). The font is floored at `MINIMAP_MIN_PT`; genuinely
                // huge files (taller than the floor allows) keep the floor and
                // co-scroll via the proportional-slider branch in `minimap_geometry`.
                // The accuracy invariant does NOT depend on the fill being exact —
                // the indicator always uses the ACTUAL `drawn_h` below.
                let drawn = if natural_h <= panel_h_q {
                    natural.clone()
                } else {
                    use std::hash::{Hash, Hasher};
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    self.tabs[self.active].edit_gen.hash(&mut h);
                    self.tabs[self.active].doc_id.raw().hash(&mut h);
                    avail.x.to_bits().hash(&mut h);
                    (word_wrap as u8).hash(&mut h);
                    panel_h_q.to_bits().hash(&mut h);
                    let key = h.finish();
                    let mut slot = self.minimap_draw_cache.borrow_mut();
                    match slot.as_ref() {
                        Some((k, g)) if *k == key => g.clone(),
                        _ => {
                            let text = self.tabs[self.active].text.clone();
                            let lay = |pt: f32, ui: &egui::Ui| {
                                ui.fonts_mut(|f| {
                                    f.layout(text.clone(), FontId::monospace(pt), map_fg, wrap_w)
                                })
                            };
                            // Linear first guess for the font that makes
                            // `drawn_h == panel_h`, clamped to the legibility floor.
                            let mut pt = (MINIMAP_BASE_PT * panel_h_q / natural_h)
                                .clamp(MINIMAP_MIN_PT, MINIMAP_BASE_PT);
                            let mut g = lay(pt, ui);
                            // One correction toward the panel height — but only when
                            // not floored (a floored huge file co-scrolls instead).
                            if pt > MINIMAP_MIN_PT {
                                let dh = g.size().y.max(1.0);
                                if (dh - panel_h_q).abs() > panel_h_q * 0.03 {
                                    let corrected = (pt * panel_h_q / dh)
                                        .clamp(MINIMAP_MIN_PT, MINIMAP_BASE_PT);
                                    if (corrected - pt).abs() > f32::EPSILON {
                                        pt = corrected;
                                        g = lay(pt, ui);
                                    }
                                }
                            }
                            *slot = Some((key, g.clone()));
                            g
                        }
                    }
                };
                let drawn_h = drawn.size().y.max(1.0);
                // (4) ONE shared geometry for content offset + indicator box.
                let geom = minimap_geometry(self.scroll_metrics, panel_h, drawn_h);
                // (5) Draw the content, translated by `map_offset` (co-scroll for
                // huge files), clipped to the panel rect.
                let painter = ui.painter_at(rect);
                painter.add(egui::epaint::TextShape::new(
                    egui::pos2(rect.left(), rect.top() + geom.map_offset),
                    drawn.clone(),
                    map_fg,
                ));
                // (6) Viewport indicator — same scale as the content above.
                let ind_rect = egui::Rect::from_min_size(
                    egui::pos2(rect.left(), rect.top() + geom.ind_top),
                    egui::vec2(rect.width(), geom.ind_h.max(6.0)),
                );
                painter.rect_filled(
                    ind_rect,
                    2.0,
                    Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 40),
                );
                // (6b) Overview-ruler decorations: changes, find hits, and LSP
                // errors as marks in three non-overlapping lanes, so a long
                // document is navigable without scrolling to look for them.
                // Shares `drawn_h` + `map_offset` with the content above, so a
                // mark sits over the row it describes.
                // The MEMOIZED line count (keyed on edit_gen + doc_id), not a
                // fresh `lines().count()` — that is an O(n) walk of the whole
                // buffer, and paying it every frame is exactly the per-frame
                // cost this file's other caches exist to avoid.
                let total_lines = self.doc_counts_active(self.active).0.max(1);
                self.paint_overview_marks(&painter, rect, drawn_h, geom.map_offset, total_lines);
                // (7) Click/drag → scroll the editor. P3: a drag that BEGINS on the
                // indicator box grabs it and moves by pointer delta (mapped through
                // the shared scale); a press elsewhere is an absolute scrub that
                // centres the viewport on the clicked fraction.
                let (off_y, content_h, view_h) = self.scroll_metrics;
                let max_off = (content_h - view_h).max(0.0);
                if resp.drag_started() {
                    self.minimap_drag_box = resp
                        .interact_pointer_pos()
                        .is_some_and(|p| ind_rect.contains(p));
                }
                if resp.dragged() && self.minimap_drag_box {
                    // Relative: pointer Δ in panel px → minimap fraction → editor px.
                    let editor_dy = resp.drag_delta().y / drawn_h * content_h;
                    let base = self.pending_scroll.unwrap_or(off_y);
                    self.pending_scroll = Some((base + editor_dy).clamp(0.0, max_off));
                } else if let Some(p) = resp.interact_pointer_pos() {
                    let frac = ((p.y - rect.top()) / panel_h).clamp(0.0, 1.0);
                    self.pending_scroll =
                        Some((frac * content_h - view_h * 0.5).clamp(0.0, max_off));
                }
                if resp.drag_stopped() {
                    self.minimap_drag_box = false;
                }
            });
    }

    /// Render the folded read-only preview: per-region toggles plus the
    /// brace-collapsed projection of the active buffer.
    pub(super) fn show_fold_view(&mut self, ui: &mut egui::Ui, font: FontId, ext: Option<&str>) {
        // Wave-3: borrow instead of cloning the whole buffer every frame the
        // fold view is shown. `fold_regions`/`project_folded` take &str, and the
        // toolbar closure below only captures `self.folds` (disjoint from
        // `self.tabs` under edition-2021 closure capture), so the borrow holds.
        let text = &self.tabs[self.active].text;
        // P2-4: markdown/text notes fold by heading section; code by braces.
        let regions = crate::editor_features::fold_regions_for(text, ext);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("FOLDS").small().monospace());
            if ui.small_button("fold all").clicked() {
                self.folds = regions.iter().map(|r| r.start_line).collect();
            }
            if ui.small_button("expand all").clicked() {
                self.folds.clear();
            }
            for r in &regions {
                let folded = self.folds.contains(&r.start_line);
                let label = format!(
                    "{} L{} ({})",
                    if folded { "▸" } else { "▾" },
                    r.start_line + 1,
                    r.hidden_len()
                );
                if ui.small_button(label).clicked() {
                    if folded {
                        self.folds.remove(&r.start_line);
                    } else {
                        self.folds.insert(r.start_line);
                    }
                }
            }
        });
        ui.separator();
        let (mut projected, _map) =
            crate::editor_features::project_folded(text, &regions, &self.folds);
        let line_height = self.config.fonts.clamped_line_height();
        let hl = &self.hl;
        let word_wrap = self.config.editor.word_wrap;
        let layout_fg = ui_color(&self.theme, "foreground", Rgba::new(0xc8, 0xd6, 0xdc, 255));
        // #D — themeable URL colour + link-detection toggle (fold view).
        let detect_links = self.config.editor.detect_links;
        let url_color = scribe_render::color32(self.theme.syntax_color(
            "url",
            self.theme.ui("accent", Rgba::new(0x4c, 0xc2, 0xff, 255)),
        ));
        let mut layouter = make_layouter(
            hl,
            &self.hl_cache,
            &self.hl_galley_cache,
            &self.hl_inc_cache,
            ext,
            font,
            line_height,
            word_wrap,
            layout_fg,
            url_color,
            detect_links,
        );
        egui::ScrollArea::both()
            .id_salt("fold-scroll")
            .show(ui, |ui| {
                let editor = egui::TextEdit::multiline(&mut projected)
                    .code_editor()
                    .desired_width(f32::INFINITY)
                    .desired_rows(30)
                    .interactive(false)
                    .layouter(&mut layouter);
                ui.add_sized(ui.available_size(), editor);
            });
    }
}

#[cfg(test)]
mod overview_mark_tests {
    use super::{
        lines_of_offsets, mark_lane, mark_y, merge_marks, minimap_geometry, MarkLane,
        MARK_MAX_PER_LANE, MARK_MIN_GAP,
    };

    const EPS: f32 = 1e-3;

    #[test]
    fn a_mark_sits_over_the_row_it_describes() {
        // The load-bearing accuracy invariant, mirroring the one
        // `minimap_geometry` protects: the mark transform and the CONTENT
        // transform must be the same one. The content galley is painted at
        // `rect.top() + map_offset` with height `drawn_h`, so line L must land
        // at `map_offset + (L/total) * drawn_h` — nothing else.
        let (total, drawn_h) = (1000usize, 700.0_f32);
        for line in [0usize, 1, 250, 500, 999] {
            let y = mark_y(line, total, drawn_h, 0.0);
            assert!(
                (y - (line as f32 / total as f32) * drawn_h).abs() < EPS,
                "line {line} mapped to {y}, which is not its fraction of the drawn content"
            );
        }
        // Halfway down a 1000-line document is halfway down the drawn map.
        assert!((mark_y(500, 1000, 700.0, 0.0) - 350.0).abs() < EPS);
    }

    #[test]
    fn marks_co_scroll_with_the_content_on_a_huge_file() {
        // In the proportional-slider regime the content is translated by
        // `map_offset`; a mark that ignored it would drift off its row by the
        // full scroll distance — the exact bug class `minimap_geometry`'s
        // history records.
        let panel_h = 700.0;
        let drawn_h = 2_100.0;
        let content_h = 200_000.0;
        let view_h = 800.0;
        let geom = minimap_geometry(
            ((content_h - view_h) * 0.5, content_h, view_h),
            panel_h,
            drawn_h,
        );
        assert!(
            geom.map_offset < 0.0,
            "this fixture must be the co-scroll regime"
        );
        let unscrolled = mark_y(5_000, 10_000, drawn_h, 0.0);
        let scrolled = mark_y(5_000, 10_000, drawn_h, geom.map_offset);
        assert!(
            (scrolled - (unscrolled + geom.map_offset)).abs() < EPS,
            "a mark must be translated by exactly the content's map_offset"
        );
    }

    #[test]
    fn mark_y_clamps_a_line_past_the_end_of_the_document() {
        // A diagnostic can outlive the edit that shortened the buffer; an
        // out-of-range line must clamp to the bottom, never paint off-panel at
        // an unbounded Y.
        let y = mark_y(9_999, 100, 700.0, 0.0);
        assert!(
            (y - 700.0).abs() < EPS,
            "an out-of-range line must clamp, got {y}"
        );
        // And an empty document must not divide by zero.
        assert!(mark_y(0, 0, 700.0, 0.0).is_finite());
    }

    #[test]
    fn the_three_lanes_never_overlap_at_any_supported_panel_width() {
        // The minimap is user-resizable over `width_range(48.0..=260.0)`. If two
        // lanes overlap at some width, one decoration class silently overpaints
        // another — an error hidden behind a search hit is the failure this
        // separation exists to prevent.
        for w in [48.0_f32, 60.0, 110.0, 180.0, 260.0] {
            let (cx, cw) = mark_lane(w, MarkLane::Change);
            let (sx, sw) = mark_lane(w, MarkLane::Search);
            let (ex, ew) = mark_lane(w, MarkLane::Error);
            assert!(cx + cw <= sx + EPS, "change/search overlap at width {w}");
            assert!(sx + sw <= ex + EPS, "search/error overlap at width {w}");
            assert!(cx >= 0.0, "change lane starts off-panel at width {w}");
            assert!(
                ex + ew <= w + EPS,
                "error lane runs past the panel edge at width {w}"
            );
            for lane_w in [cw, sw, ew] {
                assert!(
                    lane_w >= 2.0,
                    "a sub-2px lane is invisible; got {lane_w} at width {w}"
                );
            }
        }
    }

    #[test]
    fn merge_marks_collapses_the_same_pixel_row_and_keeps_distinct_ones() {
        // Dense hits in one pixel row.
        let merged = merge_marks(vec![10.0, 10.4, 10.9, 11.5], MARK_MIN_GAP);
        assert_eq!(merged, vec![10.0], "one pixel row must yield one mark");
        // Rows further apart than the gap all survive, in order.
        let merged = merge_marks(vec![30.0, 10.0, 20.0], MARK_MIN_GAP);
        assert_eq!(
            merged,
            vec![10.0, 20.0, 30.0],
            "distinct rows must survive and come back sorted"
        );
    }

    #[test]
    fn merge_marks_bounds_the_shape_count_on_a_pathological_document() {
        // A one-character query over a 200k-line file. Without the cap this
        // queues a shape per hit and stalls the frame.
        let ys: Vec<f32> = (0..200_000).map(|i| i as f32 * 100.0).collect();
        let merged = merge_marks(ys, MARK_MIN_GAP);
        assert_eq!(
            merged.len(),
            MARK_MAX_PER_LANE,
            "the per-lane cap must bound the painted shapes"
        );
    }

    #[test]
    fn merge_marks_drops_non_finite_values_instead_of_poisoning_the_sort() {
        // A NaN reaches the sort comparator as "not orderable"; letting it
        // through would make the merge order (and therefore the output) depend
        // on the sort's internal pivot choice.
        let merged = merge_marks(vec![f32::NAN, 10.0, f32::INFINITY, 40.0], MARK_MIN_GAP);
        assert_eq!(merged, vec![10.0, 40.0]);
    }

    #[test]
    fn lines_of_offsets_maps_byte_offsets_to_their_zero_based_lines() {
        //          0         1  2                3
        let text = "alpha\nbeta\n\ndelta\n";
        // byte offsets of: 'a'lpha, 'b'eta, the empty line, 'd'elta, mid-"delta"
        let offsets = [0usize, 6, 11, 12, 15];
        assert_eq!(lines_of_offsets(text, &offsets), vec![0, 1, 2, 3, 3]);
    }

    #[test]
    fn lines_of_offsets_is_order_independent_and_clamps_past_the_end() {
        let text = "a\nb\nc\n";
        // Unsorted input — search/diagnostic sources do not promise line order.
        let mut got = lines_of_offsets(text, &[4, 0, 2]);
        got.sort_unstable();
        assert_eq!(got, vec![0, 1, 2]);
        // Past-the-end offset must clamp to the last line, not panic or index out.
        assert_eq!(lines_of_offsets(text, &[9_999]), vec![3]);
        assert!(lines_of_offsets("", &[0]) == vec![0]);
        assert!(lines_of_offsets(text, &[]).is_empty());
    }

    #[test]
    fn lines_of_offsets_handles_multibyte_text() {
        // Byte offsets, not char offsets: a naive char-count walk would report
        // the wrong line for anything after a non-ASCII character.
        let text = "日本語\nsecond\n";
        let second_line_start = "日本語\n".len(); // 10 bytes
        assert_eq!(lines_of_offsets(text, &[0, second_line_start]), vec![0, 1]);
    }
}

#[cfg(test)]
mod overview_source_tests {
    use super::super::*;
    use crate::app::ScribeApp;
    use scribe_core::Config;

    #[test]
    fn change_marks_report_the_same_lines_as_the_gutter_change_bar() {
        let mut app = ScribeApp::new_test(Config::default());
        app.config.editor.show_change_bar = true;
        app.tabs[0].session_baseline = "a\nb\nc\nd\n".into();
        app.tabs[0].saved_baseline = "a\nb\nc\nd\n".into();
        app.tabs[0].set_text("a\nCHANGED\nc\nd\n".to_string());
        app.tabs[0].change_gen = None;
        app.ensure_change_states(0);
        let (unsaved, saved) = app.overview_change_lines();
        assert_eq!(
            unsaved,
            vec![1],
            "only the edited line may carry an unsaved mark"
        );
        assert!(saved.is_empty(), "nothing has been saved this session");
        // The overview must agree with the gutter, line for line.
        let gutter: Vec<usize> = app.tabs[0]
            .change_states
            .iter()
            .enumerate()
            .filter(|(_, s)| **s == crate::change_bar::LineChange::Unsaved)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(unsaved, gutter);
    }

    #[test]
    fn change_marks_are_empty_when_the_user_turned_the_change_bar_off() {
        // One toggle governs both surfaces; an overview lane that ignored it
        // would resurrect a feature the user switched off.
        let mut app = ScribeApp::new_test(Config::default());
        app.config.editor.show_change_bar = true;
        app.tabs[0].session_baseline = "a\nb\n".into();
        app.tabs[0].saved_baseline = "a\nb\n".into();
        app.tabs[0].set_text("a\nZ\n".to_string());
        app.tabs[0].change_gen = None;
        app.ensure_change_states(0);
        assert_eq!(app.overview_change_lines().0, vec![1]);
        app.config.editor.show_change_bar = false;
        assert_eq!(
            app.overview_change_lines(),
            (Vec::new(), Vec::new()),
            "the overview must honour the change-bar toggle"
        );
    }

    #[test]
    fn search_marks_track_the_live_find_query() {
        let mut app = ScribeApp::new_test(Config::default());
        app.tabs[0].set_text("needle\nhay\nneedle\nhay\n".to_string());
        app.find_open = true;
        app.find_query = "needle".to_string();
        assert_eq!(
            app.overview_search_lines(),
            vec![0, 2],
            "every matching line must get a mark"
        );
        app.find_query = "nothing-here".to_string();
        assert!(
            app.overview_search_lines().is_empty(),
            "a query with no hits must mark nothing"
        );
    }

    #[test]
    fn search_marks_disappear_when_the_find_bar_closes() {
        // `find_query` outlives the bar being closed. Marks that outlived it too
        // would show hits for a search the user has finished with.
        let mut app = ScribeApp::new_test(Config::default());
        app.tabs[0].set_text("needle\nhay\n".to_string());
        app.find_open = true;
        app.find_query = "needle".to_string();
        assert_eq!(app.overview_search_lines(), vec![0]);
        app.find_open = false;
        assert!(
            app.overview_search_lines().is_empty(),
            "closing the find bar must clear its marks"
        );
    }

    #[test]
    fn error_marks_only_cover_the_active_document() {
        // `self.diagnostics` holds whatever the server last published — after a
        // tab switch that is the PREVIOUS file. Painting those here would mark
        // lines of this document that have nothing wrong with them.
        let dir = tempfile::tempdir().unwrap();
        let mine = dir.path().join("mine.rs");
        std::fs::write(&mine, "fn a() {}\nfn b() {}\nfn c() {}\n").unwrap();
        let mut app = ScribeApp::new_test(Config::default());
        app.tabs[0] = EditorTab::from_path(mine.clone()).unwrap();
        app.active = 0;
        let mine_uri = path_to_uri(&mine);
        let other_uri = path_to_uri(&dir.path().join("someone-elses.rs"));
        app.diagnostics = vec![
            scribe_core::lsp::protocol::Diagnostic {
                uri: mine_uri.clone(),
                line: 1,
                character: 0,
                end_line: 1,
                end_character: 4,
                severity: 1,
                message: "boom".into(),
            },
            scribe_core::lsp::protocol::Diagnostic {
                uri: other_uri,
                line: 2,
                character: 0,
                end_line: 2,
                end_character: 4,
                severity: 1,
                message: "not mine".into(),
            },
        ];
        assert_eq!(
            app.overview_error_lines(),
            vec![(1, 1)],
            "only the active document's diagnostics may be marked"
        );
    }

    #[test]
    fn error_marks_keep_errors_and_warnings_and_drop_hint_noise() {
        let dir = tempfile::tempdir().unwrap();
        let mine = dir.path().join("mine.rs");
        std::fs::write(&mine, "a\nb\nc\nd\n").unwrap();
        let mut app = ScribeApp::new_test(Config::default());
        app.tabs[0] = EditorTab::from_path(mine.clone()).unwrap();
        app.active = 0;
        let uri = path_to_uri(&mine);
        app.diagnostics = (1u8..=4)
            .map(|sev| scribe_core::lsp::protocol::Diagnostic {
                uri: uri.clone(),
                line: u32::from(sev),
                character: 0,
                end_line: u32::from(sev),
                end_character: 1,
                severity: sev,
                message: "m".into(),
            })
            .collect();
        assert_eq!(
            app.overview_error_lines(),
            vec![(1, 1), (2, 2)],
            "errors + warnings are marked; info/hint are ruler noise and are not"
        );
    }

    #[test]
    fn error_marks_are_empty_for_an_unsaved_scratch_buffer() {
        // No path means no uri means no diagnostic can belong to it. Without the
        // path guard an empty uri would match and mark a scratch note.
        let mut app = ScribeApp::new_test(Config::default());
        app.diagnostics = vec![scribe_core::lsp::protocol::Diagnostic {
            uri: String::new(),
            line: 0,
            character: 0,
            end_line: 0,
            end_character: 1,
            severity: 1,
            message: "m".into(),
        }];
        assert!(app.tabs[0].doc.path().is_none());
        assert!(app.overview_error_lines().is_empty());
    }
}

#[cfg(test)]
mod minimap_geom_tests {
    use super::{minimap_geometry, MINIMAP_BASE_PT, MINIMAP_MIN_PT};

    const EPS: f32 = 1e-3;

    #[test]
    fn geometry_kills_branch_and_arith_mutants() {
        // Targets the four un-covered arithmetic/branch mutants no existing test
        // exercised: the FIT/HUGE selector `drawn_h <= panel_h + 0.5`, the FIT
        // ind_top clamp bound `(drawn_h - ind_h)`, and the HUGE `scroll_range`/`f`.
        // (1) FIT/HUGE selector. panel=100, drawn=100.4 -> FIT -> map_offset==0.
        //   `+ -> *`: 100.4<=50 false -> HUGE -> map_offset=-0.2 (killed);
        //   `+ -> -`: 100.4<=99.5 false -> HUGE -> map_offset=-0.2 (killed).
        let g = minimap_geometry((450.0, 1000.0, 100.0), 100.0, 100.4);
        assert!((g.map_offset - 0.0).abs() < EPS);

        // (2) FIT ind_top clamp upper bound `(drawn_h - ind_h)`. Scroll to the
        //   bottom so the raw top exceeds the bound and the clamp bites: ind_h=10,
        //   orig ind_top=90.0; `- -> +` widens the bound to 110 -> ind_top=100.0.
        let g = minimap_geometry((1000.0, 1000.0, 100.0), 200.0, 100.0);
        assert!((g.ind_h - 10.0).abs() < EPS);
        assert!((g.ind_top - 90.0).abs() < EPS);

        // (3) HUGE scroll_range + f. panel=100, drawn=300 -> HUGE. ind_h=30,
        //   orig scroll_range=900, f=0.5, ind_top=35.0, map_offset=-100.0.
        //   `- -> /`: scroll_range=1000/100=10 -> f clamps to 1 -> ind_top=70 (killed);
        //   `/ -> *`: f=450*900 -> clamps to 1 -> ind_top=70 (killed).
        let g = minimap_geometry((450.0, 1000.0, 100.0), 100.0, 300.0);
        assert!((g.ind_h - 30.0).abs() < EPS);
        assert!((g.ind_top - 35.0).abs() < EPS);
        assert!((g.map_offset + 100.0).abs() < EPS);
    }

    /// The core regression guard for the original bug: the indicator's
    /// fraction-of-drawn-content MUST equal the editor's visible fraction. The
    /// pre-fix code multiplied the indicator by an extra `scale` while the
    /// content was drawn un-scaled, so `ind_top/drawn_h` diverged from
    /// `off_y/content_h` for any document taller than the panel.
    #[test]
    fn indicator_fraction_matches_editor_when_fit() {
        // Tall document squished to fit a 700px panel: drawn_h == panel_h.
        let panel_h = 700.0;
        let drawn_h = 700.0; // after fit-to-height scaling
        let content_h = 12_000.0; // editor px (tall doc)
        let view_h = 800.0;
        // Scroll to the MIDDLE of the document.
        let off_y = (content_h - view_h) * 0.5;
        let g = minimap_geometry((off_y, content_h, view_h), panel_h, drawn_h);
        // Box top fraction of drawn content == scroll-offset fraction of doc.
        assert!(
            (g.ind_top / drawn_h - off_y / content_h).abs() < EPS,
            "ind_top/drawn_h={} must equal off_y/content_h={}",
            g.ind_top / drawn_h,
            off_y / content_h
        );
        // Box height fraction == visible fraction.
        assert!(
            (g.ind_h / drawn_h - view_h / content_h).abs() < EPS,
            "ind_h/drawn_h={} must equal view_h/content_h={}",
            g.ind_h / drawn_h,
            view_h / content_h
        );
        assert!(g.map_offset.abs() < EPS, "fit case must not co-scroll");
        // The box stays fully inside the panel.
        assert!(g.ind_top >= 0.0 && g.ind_top + g.ind_h <= panel_h + 0.5);
    }

    /// The pre-fix arithmetic, reproduced, must DISAGREE with the editor's true
    /// fraction for a tall doc — proving the test would have caught the bug.
    #[test]
    fn buggy_extra_scale_factor_would_fail() {
        let panel_h = 700.0_f32;
        let content_h = 12_000.0_f32;
        let view_h = 800.0_f32;
        let off_y = (content_h - view_h) * 0.5;
        // Natural (un-scaled) minimap height for this doc, > panel (the bug regime).
        let map_h = 3600.0_f32;
        let scale = (panel_h / map_h).min(1.0); // < 1
                                                // Old buggy indicator top in panel space:
        let buggy_ind_top = (off_y / content_h) * map_h * scale;
        let buggy_drawn_h = map_h; // content was DRAWN at natural height
        let editor_frac = off_y / content_h;
        let buggy_frac = buggy_ind_top / buggy_drawn_h;
        assert!(
            (buggy_frac - editor_frac).abs() > 0.1,
            "the old extra-scale formula must visibly diverge from the editor fraction"
        );
    }

    #[test]
    fn short_doc_top_and_bottom() {
        // Short doc: drawn_h < panel_h, scale clamps to 1.
        let panel_h = 700.0;
        let drawn_h = 200.0;
        let content_h = 1_000.0;
        let view_h = 700.0;
        // Top of document.
        let top = minimap_geometry((0.0, content_h, view_h), panel_h, drawn_h);
        assert!(top.ind_top.abs() < EPS);
        assert!(top.map_offset.abs() < EPS);
        // Bottom of document — box bottom flush with content bottom, never past it.
        let max_off = content_h - view_h;
        let bot = minimap_geometry((max_off, content_h, view_h), panel_h, drawn_h);
        assert!(bot.ind_top + bot.ind_h <= drawn_h + EPS);
    }

    #[test]
    fn huge_file_coscrolls_and_keeps_slider_in_panel() {
        // Drawn content taller than panel even after the font floor.
        let panel_h = 700.0;
        let drawn_h = 2_100.0; // 3× the panel → proportional-slider regime
        let content_h = 200_000.0;
        let view_h = 800.0;
        let max_off = content_h - view_h;
        // Top: no offset, slider at panel top.
        let top = minimap_geometry((0.0, content_h, view_h), panel_h, drawn_h);
        assert!(top.ind_top.abs() < EPS);
        assert!(top.map_offset.abs() < EPS);
        // Bottom: slider pinned at panel bottom, content scrolled fully up.
        let bot = minimap_geometry((max_off, content_h, view_h), panel_h, drawn_h);
        assert!(
            (bot.ind_top + bot.ind_h - panel_h).abs() < 1.0,
            "slider bottom must reach the panel bottom"
        );
        assert!(
            (bot.map_offset + (drawn_h - panel_h)).abs() < 1.0,
            "content must scroll up by (drawn_h - panel_h)"
        );
        // Slider always within the panel.
        for frac in [0.0_f32, 0.25, 0.5, 0.75, 1.0] {
            let g = minimap_geometry((frac * max_off, content_h, view_h), panel_h, drawn_h);
            assert!(g.ind_top >= -EPS && g.ind_top + g.ind_h <= panel_h + 1.0);
            assert!(g.map_offset <= EPS);
        }
    }

    #[test]
    fn font_floor_threshold_is_three_panels() {
        // Sanity on the constants that pick fit-vs-coscroll: the floor kicks in
        // at natural_h ≈ (BASE/MIN) × panel_h.
        let ratio = MINIMAP_BASE_PT / MINIMAP_MIN_PT;
        assert!((ratio - 3.0).abs() < EPS);
    }
}
