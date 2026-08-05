//! Per-pane grid chrome + the per-pane EDITOR OVERLAYS.
//!
//! Two groups live here:
//!
//! 1. **The header chip** ([`render_pane_header`]) — extracted from
//!    `ScribeApp::render_grid_central_panel` (A-01). Behavior-neutral split:
//!    the chip layout (wide one-row vs narrow centered column, pin toggle,
//!    close button, drag handle) is moved verbatim out of the `render_body`
//!    closure. The caller passes the focused-pane `tab`, whether the pane is
//!    active, the two theme colours, and the shared per-frame `closes` buffer;
//!    the helper returns whether a tile drag started this frame.
//!
//! 2. **The galley overlays** ([`paint_pane_diagnostics`], [`pane_link_overlay`])
//!    and the [`image_paste_gesture`] hook — three capabilities that existed
//!    ONLY on the single-pane path in `frame_tick` because their code sits
//!    inside the `else` arm of `if self.grid_tree.is_some() { … } else { … }`.
//!    Turning split view on therefore silently removed all three: no inline
//!    diagnostic squiggle or hover (the user was back to the two status-bar
//!    integers that do not say WHICH line is wrong), no `[[wiki-link]]` click,
//!    and no Ctrl+V image-attachment paste. They live in this file rather than
//!    inline in `grid_methods` so the single-pane path can be pointed at the
//!    SAME functions (see the module note on `WIKILINK_FOLLOW_SLOT`).

use eframe::egui;
use egui::{Color32, RichText};
use std::cell::RefCell;

use super::diagnostics_overlay::{self, DiagSpan};
use super::{byte_to_char_index, char_to_byte, grip_handle, paint_squiggle, EditorTab};
use crate::grid::DocId;

// ---------------------------------------------------------------------------
// ctx-data slots shared with the single-pane path
// ---------------------------------------------------------------------------

/// The ctx-data key the in-editor `[[wiki-link]]` click stashes its TARGET into.
///
/// **This string MUST stay byte-identical to `frame_tick::editor_wikilink_follow_id`.**
/// The DRAIN — `open_or_create_wikilink`, at the top of `frame_tick` — is
/// unconditional and shared by both surfaces, so the grid pane deliberately
/// writes the SAME slot rather than growing a second follow mechanism. That is
/// the precedent the pane's right-click menu already set with
/// `editor_ctx_cmd_id`.
///
/// The literal is duplicated only because `frame_tick`'s copy is private to
/// that module. Nothing silently tolerates a divergence:
/// `ctrl_clicking_a_wikilink_in_a_grid_pane_opens_the_note` asserts the note is
/// created and opened, which can only happen if this key hits the real drain.
/// The clean fix is a one-word peer edit — widen `frame_tick`'s
/// `fn editor_wikilink_follow_id` to `pub(super)` and delete this constant.
const WIKILINK_FOLLOW_SLOT: &str = "scr1b3_editor_wikilink_follow";

/// Mirror of `frame_tick::last_text_paste_frame_id`'s key — see
/// [`WIKILINK_FOLLOW_SLOT`] for why the literal is duplicated. Same slot on
/// purpose: only one of the two surfaces renders on any given frame, and
/// sharing the slot means toggling the grid mid-gesture cannot strand a
/// half-recorded paste.
const LAST_TEXT_PASTE_SLOT: &str = "scr1b3_last_text_paste_frame";

/// Mirror of `frame_tick::PASTE_IMAGE_TEXT_GRACE_FRAMES`.
///
/// `egui_winit` emits `Event::Paste` on the paste key-DOWN and the `V` key
/// RELEASE one or more frames later, so the two halves of ONE gesture land in
/// different frames. This window joins them back together.
const PASTE_IMAGE_TEXT_GRACE_FRAMES: u64 = 20;

fn wikilink_follow_id() -> egui::Id {
    egui::Id::new(WIKILINK_FOLLOW_SLOT)
}

fn last_text_paste_frame_id() -> egui::Id {
    egui::Id::new(LAST_TEXT_PASTE_SLOT)
}

/// Render one grid pane's header chip and return whether a tile drag started.
///
/// Moved verbatim from the inline `render_body` closure: same wide/narrow
/// layout split, same pin/close controls, same drag-handle behaviour. The only
/// mechanical change is `tabs[idx]` -> `tab` and `is_active` arriving as a
/// parameter instead of being recomputed from `active_doc`.
pub(super) fn render_pane_header(
    ui: &mut egui::Ui,
    tab: &mut EditorTab,
    doc_id: DocId,
    is_active: bool,
    accent: Color32,
    muted: Color32,
    render_closes: &RefCell<Vec<DocId>>,
) -> bool {
    let mut drag_started = false;
    // #R5 — per-pane header rendered as a tab CHIP that mirrors the
    // top tab strip: a filled accent chip on the focused pane,
    // transparent otherwise; drag-handle ICON on the left, note name,
    // pin toggle, close ✕ on the far right. Pinned notes drop the
    // drag handle + ✕ (anchored, can't be moved/closed). All glyphs
    // are phosphor (the old ✕ / ⠿ were tofu).
    let pane_title = tab.title();
    let pinned = tab.pinned;
    let chip = egui::Frame::default()
        .inner_margin(egui::Margin::symmetric(8, 3))
        .corner_radius(egui::CornerRadius::same(5))
        .fill(if is_active {
            accent.linear_multiply(0.20)
        } else {
            Color32::TRANSPARENT
        });
    // Header layout adapts to pane width. A WIDE pane gets ONE row
    // (handle · name · pin, with the close ✕ on the far right); a
    // NARROW pane gets a single CENTERED column (name, then pin, then
    // close) so the controls never wrap into a ragged stack. All
    // glyphs are phosphor (now resolved in monospace too, so no tofu).
    let pin_glyph = if pinned {
        egui_phosphor::thin::PUSH_PIN_SLASH
    } else {
        egui_phosphor::thin::PUSH_PIN
    };
    if ui.available_width() >= 220.0 {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            chip.show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    // Drag handle — a neutral `muted` grip so it stays
                    // visible on BOTH the active (accent-tinted) and
                    // inactive chip backgrounds (an accent-coloured grip
                    // vanished against the active chip's accent fill).
                    let grip_color = muted;
                    if pinned {
                        grip_handle(ui, false, grip_color, false)
                            .on_hover_text("Pinned — drag disabled");
                    } else {
                        // `drag_started()` fires ONCE on drag start (egui_tiles
                        // expects a single `DragStarted`); a held-button check
                        // would re-fire every frame and wedge the tile drag.
                        let handle = grip_handle(ui, true, grip_color, false)
                            .on_hover_text("Drag to rearrange")
                            .on_hover_cursor(egui::CursorIcon::Grab);
                        if handle.drag_started() {
                            drag_started = true;
                        }
                    }
                    ui.label(RichText::new(&pane_title).monospace().color(if is_active {
                        accent
                    } else {
                        muted
                    }))
                    .on_hover_text(&pane_title);
                    if ui
                        .add(egui::Button::new(pin_glyph).frame(false).small())
                        .on_hover_text(if pinned { "Unpin note" } else { "Pin note" })
                        .clicked()
                    {
                        tab.pinned = !pinned;
                    }
                });
            });
            // Close at the far right — hidden on pinned notes.
            if !pinned {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(
                            egui::Button::new(egui_phosphor::thin::X)
                                .frame(false)
                                .small(),
                        )
                        .on_hover_text("Close pane")
                        .clicked()
                    {
                        render_closes.borrow_mut().push(doc_id);
                    }
                });
            }
        });
    } else {
        // NARROW pane: a single centered column — name, pin, close.
        // The name doubles as the drag handle (no room for a grip).
        chip.show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                let name = ui.add(
                    egui::Label::new(RichText::new(&pane_title).monospace().color(if is_active {
                        accent
                    } else {
                        muted
                    }))
                    .sense(if pinned {
                        egui::Sense::hover()
                    } else {
                        egui::Sense::click_and_drag()
                    }),
                );
                if !pinned && name.drag_started() {
                    drag_started = true;
                }
                if ui
                    .add(egui::Button::new(pin_glyph).frame(false).small())
                    .on_hover_text(if pinned { "Unpin note" } else { "Pin note" })
                    .clicked()
                {
                    tab.pinned = !pinned;
                }
                if !pinned
                    && ui
                        .add(
                            egui::Button::new(egui_phosphor::thin::X)
                                .frame(false)
                                .small(),
                        )
                        .on_hover_text("Close pane")
                        .clicked()
                {
                    render_closes.borrow_mut().push(doc_id);
                }
            });
        });
    }
    drag_started
}

// ---------------------------------------------------------------------------
// Per-pane editor overlays (single-pane parity)
// ---------------------------------------------------------------------------

/// The four severity colours the inline-diagnostic overlay resolves from the
/// theme, passed in so the painter never re-declares hex constants that could
/// desync from the app's own `ui_color` lookups.
#[derive(Clone, Copy)]
pub(super) struct DiagColors {
    pub error: Color32,
    pub warning: Color32,
    pub info: Color32,
    /// Anything else (a hint, or an unknown severity number).
    pub hint: Color32,
}

/// Paint a severity-coloured squiggle under every diagnostic span, and describe
/// the one under the pointer on hover.
///
/// The grid pane painted NONE of this: `render_grid_central_panel` never even
/// mentioned the word `diagnostic`, because the whole block lives in the
/// single-pane arm of `frame_tick`'s `if self.grid_tree.is_some()` fork. With
/// split view on, the only thing the user got from the language server was the
/// `1e / 3` status counter — two integers that do not say which line is wrong.
///
/// Painted per galley ROW rather than per span, so a multi-line diagnostic (an
/// unclosed delimiter, a type error spanning a match arm) underlines every line
/// it covers instead of being dropped for spanning rows — and so it follows soft
/// wrapping. Same rule as the single-pane painter.
pub(super) fn paint_pane_diagnostics(
    ui: &egui::Ui,
    out: &egui::text_edit::TextEditOutput,
    text: &str,
    spans: &[DiagSpan],
    colors: DiagColors,
) {
    if spans.is_empty() {
        return;
    }
    let painter = ui.painter();
    let origin = out.galley_pos.to_vec2();
    // Rects actually painted, so the hover test is "is the pointer over a
    // squiggle", not "is it somewhere on a line that has one".
    let mut painted: Vec<egui::Rect> = Vec::new();
    for span in spans {
        let color = match span.severity {
            diagnostics_overlay::SEVERITY_ERROR => colors.error,
            diagnostics_overlay::SEVERITY_WARNING => colors.warning,
            diagnostics_overlay::SEVERITY_INFO => colors.info,
            _ => colors.hint,
        };
        let c0 = byte_to_char_index(text, span.start);
        let c1 = byte_to_char_index(text, span.end);
        let mut row_start = 0usize;
        for prow in &out.galley.rows {
            let row_end = row_start + prow.char_count_including_newline();
            let s = c0.max(row_start);
            let e = c1.min(row_end);
            if s < e {
                let rx = origin.x + prow.pos.x;
                let x0 = rx + prow.row.x_offset(s - row_start);
                let x1 = rx + prow.row.x_offset(e - row_start);
                let top = origin.y + prow.pos.y;
                let bot = top + prow.row.size.y;
                paint_squiggle(painter, x0, x1, bot, color);
                painted.push(egui::Rect::from_min_max(
                    egui::pos2(x0, top),
                    egui::pos2(x1, bot),
                ));
            }
            row_start = row_end;
            if row_start >= c1 {
                break;
            }
        }
    }
    // Hover: name the problem. Resolved through the galley so the message
    // belongs to the character under the pointer, not merely to the same line.
    let Some(p) = ui.ctx().pointer_hover_pos() else {
        return;
    };
    if !painted.iter().any(|r| r.contains(p)) {
        return;
    }
    let cursor = out.galley.cursor_from_pos(p - out.galley_pos);
    let byte = char_to_byte(text, cursor.index);
    if let Some(msg) = diagnostics_overlay::hover_text(spans, byte) {
        egui::show_tooltip_at_pointer(
            ui.ctx(),
            out.response.layer_id,
            egui::Id::new("scr1b3-diagnostic-tooltip"),
            |ui| {
                ui.label(msg);
            },
        );
    }
}

/// Hover affordance + Ctrl/Cmd-click follow for the `http(s)` URLs and
/// `[[wiki-link]]`s under the pointer in a grid pane.
///
/// The pane had its own editor render path and no link pass at all, so the
/// click added for the single-pane editor simply did not exist in split view.
/// The wiki-link arm stashes into the SAME ctx-data slot the single-pane path
/// uses ([`WIKILINK_FOLLOW_SLOT`]) and is therefore followed by the SAME
/// unconditional drain — one mechanism, not two.
///
/// `vault_configured` gates the wiki-link arm: a link has nowhere to resolve to
/// without a vault, which is exactly the precondition `open_or_create_wikilink`
/// enforces.
///
/// Both scans are O(buffer), so the pointer-over-the-editor test comes FIRST —
/// otherwise they would run every frame of every session for a hit-test that
/// cannot happen.
pub(super) fn pane_link_overlay(
    ui: &egui::Ui,
    out: &egui::text_edit::TextEditOutput,
    text: &str,
    detect_links: bool,
    vault_configured: bool,
) {
    // Same buffer-size cap the single-pane overlay uses.
    const MAX_SCANNED: usize = 1_000_000;

    let Some(p) = ui
        .input(|i| i.pointer.hover_pos())
        .filter(|p| out.response.rect.contains(*p))
    else {
        return;
    };
    if text.len() > MAX_SCANNED {
        return;
    }
    let mut url_spans: Vec<(usize, usize, &str)> = Vec::new();
    if detect_links {
        let mut base = 0usize;
        for line in text.split_inclusive('\n') {
            for r in scribe_core::url_scan::detect_urls(line) {
                url_spans.push((base + r.start, base + r.end, &line[r]));
            }
            base += line.len();
        }
    }
    // An empty target (an intra-note anchor) names no note, so it is skipped.
    let link_spans: Vec<scribe_core::notes::wikilink::WikiLink> = if vault_configured {
        scribe_core::notes::wikilink::extract_wikilinks(text)
            .into_iter()
            .filter(|l| !l.target.is_empty())
            .collect()
    } else {
        Vec::new()
    };
    if url_spans.is_empty() && link_spans.is_empty() {
        return;
    }
    let ci = out.galley.cursor_from_pos(p - out.galley_pos).index;
    let byte = char_to_byte(text, ci);
    let cmd = ui.input(|i| i.modifiers.command);
    let clicked = ui.input(|i| i.pointer.primary_clicked());
    // A URL is never inside a link target, so the two arms are mutually
    // exclusive at a given byte: URL wins the `find`, the link arm is the else.
    if let Some(&(_, _, url)) = url_spans.iter().find(|(s, e, _)| byte >= *s && byte < *e) {
        if cmd {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        // Anti-phishing hover preview: the user sees where a link goes before
        // opening it.
        egui::show_tooltip_at_pointer(
            ui.ctx(),
            out.response.layer_id,
            egui::Id::new("scr1b3-url-tooltip"),
            |ui| {
                ui.label(if cmd {
                    url.to_string()
                } else {
                    format!("{url}  —  Ctrl+click to open")
                });
            },
        );
        // Open only on an explicit modifier-click, and only for http/https.
        if cmd && clicked && scribe_core::url_scan::is_clickable_url(url) {
            ui.ctx().open_url(egui::OpenUrl::new_tab(url.to_string()));
        }
    } else if let Some(link) = link_spans.iter().find(|l| byte >= l.start && byte < l.end) {
        if cmd {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        // The hover names the TARGET, not the label — an aliased link must not
        // hide where it goes.
        egui::show_tooltip_at_pointer(
            ui.ctx(),
            out.response.layer_id,
            egui::Id::new("scr1b3-wikilink-tooltip"),
            |ui| {
                ui.label(if cmd {
                    format!("[[{}]]", link.target)
                } else {
                    format!("[[{}]]  —  Ctrl+click to open", link.target)
                });
            },
        );
        // Stash, don't follow: `self.tabs` is mutably borrowed by the pane
        // renderer here. Drained at the top of the next frame.
        if cmd && clicked {
            ui.ctx()
                .data_mut(|d| d.insert_temp(wikilink_follow_id(), link.target.clone()));
        }
    }
}

/// Did this frame carry the Ctrl/Cmd+V gesture an IMAGE paste is observable
/// through?
///
/// The hook is the V key RELEASE, not `consume_key(COMMAND, V)`, and that is
/// NOT a stylistic choice: `egui_winit`'s `on_keyboard_input` special-cases the
/// paste chord on key-DOWN and returns immediately after pushing
/// `Event::Paste` — so `Event::Key { key: V, pressed: true }` NEVER reaches us,
/// and when the clipboard holds no TEXT it pushes nothing at all. On an
/// image-only clipboard the key release is therefore the ONLY observable event
/// of the whole gesture; a `consume_key` here would be permanently dead code
/// that STILL passes a `Driver::key`-driven test (that helper sends press AND
/// release). Shift is excluded so Ctrl+Shift+V keeps its own binding.
///
/// A clipboard carrying BOTH text and a bitmap pastes the TEXT: that already
/// happened on the key-down frame, so the release must not also drop an image
/// in. The text-paste frame is recorded in shared ctx-data and suppresses the
/// image branch for [`PASTE_IMAGE_TEXT_GRACE_FRAMES`].
pub(super) fn image_paste_gesture(ctx: &egui::Context) -> bool {
    let now = ctx.cumulative_pass_nr();
    let (text_pasted, paste_released) = ctx.input(|i| {
        (
            i.events
                .iter()
                .any(|e| matches!(e, egui::Event::Paste(t) if !t.is_empty())),
            i.events.iter().any(|e| {
                matches!(
                    e,
                    egui::Event::Key {
                        key: egui::Key::V,
                        pressed: false,
                        modifiers,
                        ..
                    } if modifiers.command && !modifiers.shift && !modifiers.alt
                )
            }),
        )
    });
    if text_pasted {
        ctx.data_mut(|d| d.insert_temp(last_text_paste_frame_id(), now));
    }
    if !paste_released {
        return false;
    }
    let text_won = ctx
        .data_mut(|d| {
            let id = last_text_paste_frame_id();
            let v = d.get_temp::<u64>(id);
            d.remove::<u64>(id);
            v
        })
        .is_some_and(|f| now.saturating_sub(f) <= PASTE_IMAGE_TEXT_GRACE_FRAMES);
    !text_won
}
