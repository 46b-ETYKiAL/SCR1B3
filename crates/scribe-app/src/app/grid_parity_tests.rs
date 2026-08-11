//! Grid/split-pane ⇄ single-pane editor PARITY, and the status-bar editor-mode
//! badge.
//!
//! Two linked holes these pin:
//!
//! 1. **The grid pane was a bare `TextEdit`.** No widget id, no right-click
//!    menu, no scroll metrics, no page/drag scroll assists, no multi-cursor, no
//!    rope path, and no editor-mode publish — so turning split view on silently
//!    downgraded the editor and nothing in the suite noticed.
//! 2. **The mode badge went stale.** `frame_tick::publish_editor_mode`'s doc
//!    comment asserted "every editor path calls this"; the grid path called it
//!    never, and there were ZERO `EditorMode` / badge assertions anywhere in the
//!    crate, so the claim could rot indefinitely.
//!
//! Every test here asserts what the USER would see — the badge text actually
//! rendered into the status bar, the status-bar counters actually describing the
//! pane that was clicked, the pane text actually changed by the menu pick —
//! rather than an internal flag. Each was verified by cutting its wire and
//! confirming the failure before being committed.
#![allow(clippy::wildcard_imports)]
use super::frame_tick::EditorMode;
use super::*;
use egui_kittest::kittest::Queryable as _;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A grid config whose rope-swap threshold is tiny, so a few KiB of text is
/// enough to cross the "large file" cliff that the badge exists to announce.
fn grid_config() -> Config {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    cfg.appearance.frameless = false;
    cfg.editor.grid_enabled = true;
    cfg.editor.rope_editor_auto_threshold_bytes = 1024;
    cfg
}

/// A two-pane grid app whose two tabs carry the supplied texts.
fn grid_app(first: &str, second: &str) -> ScribeApp {
    let mut app = ScribeApp::new_test(grid_config());
    app.tabs[0].text = first.to_string();
    app.tabs.push(EditorTab::scratch());
    app.tabs[1].text = second.to_string();
    app
}

fn harness(app: ScribeApp) -> egui_kittest::Harness<'static, ScribeApp> {
    egui_kittest::Harness::builder()
        // Wide + tall: two panes each well over the 220px narrow-header threshold.
        .with_size(egui::Vec2::new(1280.0, 760.0))
        .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app)
}

/// A body-point inside the pane of the tab that is currently the SOLE unpinned
/// one — located from its (therefore unique) "Pin note" glyph rather than from
/// guessed geometry, the same isolation trick `grid_pane_tests` uses.
fn unpinned_pane_body(h: &egui_kittest::Harness<'static, ScribeApp>) -> egui::Pos2 {
    let pin = h.get_by_label(egui_phosphor::thin::PUSH_PIN).rect();
    egui::pos2(pin.center().x, pin.center().y + 140.0)
}

fn click_at(h: &mut egui_kittest::Harness<'static, ScribeApp>, pos: egui::Pos2, secondary: bool) {
    let button = if secondary {
        egui::PointerButton::Secondary
    } else {
        egui::PointerButton::Primary
    };
    h.input_mut().events.push(egui::Event::PointerMoved(pos));
    for pressed in [true, false] {
        h.input_mut().events.push(egui::Event::PointerButton {
            pos,
            button,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
    }
    h.run();
}

/// One `frame_tick` on a raw context (no kittest), returning nothing — used for
/// the toast tests, which need several frames on ONE context so the ctx-data the
/// mode publish is sequenced through survives between them.
fn raw_frame(app: &mut ScribeApp, ctx: &egui::Context) {
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(1100.0, 720.0),
        )),
        ..Default::default()
    };
    let _ = ctx.run(input, |ctx| app.frame_tick(ctx));
}

/// Enough text to cross `grid_config`'s 1 KiB rope threshold, as `lines` lines.
fn big_text(lines: usize) -> String {
    (0..lines)
        .map(|i| format!("line {i:04} ................................"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The stored caret head for a grid pane's editor, or `None` when the widget
/// never stored a cursor.
fn pane_caret(ctx: &egui::Context, doc_id: crate::grid::DocId) -> Option<usize> {
    let st = egui::TextEdit::load_state(ctx, grid_methods::pane_editor_id(doc_id))?;
    st.cursor.char_range().map(|r| r.primary.index)
}

fn set_pane_caret(ctx: &egui::Context, doc_id: crate::grid::DocId, at: usize) {
    let id = grid_methods::pane_editor_id(doc_id);
    let mut st = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
    st.cursor.set_char_range(Some(egui::text::CCursorRange::one(
        egui::text::CCursor::new(at),
    )));
    st.store(ctx, id);
}

/// 0-based line holding char index `at`.
fn line_of(text: &str, at: usize) -> usize {
    text.chars().take(at).filter(|c| *c == '\n').count()
}

// ---------------------------------------------------------------------------
// Defect 2 — the editor-mode badge
// ---------------------------------------------------------------------------

/// The grid path must publish the surface its ACTIVE pane rendered on.
///
/// It published nothing at all, so the status bar kept rendering whatever the
/// last single-pane frame had left in ctx-data. With the grid on from the first
/// frame, that default is `Standard` — i.e. NO badge — so a multi-MiB note in a
/// pane sat on the degraded rope editor while the status bar said everything was
/// fine.
#[test]
fn grid_publishes_the_active_panes_mode() {
    let app = grid_app(&big_text(400), "small");
    let mut h = harness(app);
    h.run();
    h.run();
    h.run();
    assert!(
        h.query_by_label("[ ROPE ]").is_some(),
        "the active pane is past the rope threshold, so the status bar must show \
         the ROPE badge; it showed no badge at all because the grid path never \
         published a mode"
    );
}

/// …and the publish is not a one-way latch: dropping back under the threshold
/// must CLEAR the badge, exactly as the single-pane `Standard` publish does.
#[test]
fn grid_badge_clears_when_the_active_pane_returns_to_a_small_buffer() {
    let app = grid_app(&big_text(400), "small");
    let mut h = harness(app);
    h.run();
    h.run();
    h.run();
    assert!(
        h.query_by_label("[ ROPE ]").is_some(),
        "precondition: the big pane badges as ROPE"
    );
    h.state_mut().tabs[0].text = "now tiny".into();
    h.run();
    h.run();
    h.run();
    assert!(
        h.query_by_label("[ ROPE ]").is_none(),
        "back under the threshold the pane renders a full-feature TextEdit, so the \
         badge must clear — a badge that only ever latches degraded modes is the \
         stale badge this wiring exists to prevent"
    );
}

/// The badge's hover has to name what the user actually loses.
///
/// It named four features (breadcrumbs, sticky scroll, spellcheck overlay,
/// completion popup). The rope branch `return`s before the entire remainder of
/// the `TextEdit` body, so roughly a dozen more die with it. Each needle below
/// is a capability whose code sits AFTER that early return and therefore does
/// not run on the rope path.
#[test]
fn the_rope_badge_hover_names_the_features_that_actually_die() {
    let hover = EditorMode::Rope.hover();
    for needle in [
        // the original four, kept
        "breadcrumbs",
        "sticky scroll",
        "spellcheck",
        "completion",
        // …and the ones the old wording silently omitted
        "multi-cursor",
        "right-click menu",
        "indent guides",
        "column rulers",
        "bracket-match",
        "current-line highlight",
        "auto-pair",
        "auto-indent",
        "Ln/Col",
        "bookmark dots",
    ] {
        assert!(
            hover.contains(needle),
            "the ROPE hover under-reports the damage: it never mentions {needle:?}"
        );
    }
    // The full-feature default stays deliberately badge-less and its hover stays
    // a one-liner — "no badge" IS the "nothing is degraded" signal.
    assert_eq!(EditorMode::Standard.badge(), None);
    assert!(EditorMode::Standard
        .hover()
        .contains("all editing features"));
}

/// Crossing the threshold is a swap the user never asked for, so it is announced
/// ONCE rather than left for whoever thinks to hover a four-letter badge.
#[test]
fn crossing_the_rope_threshold_raises_a_one_shot_notice() {
    let ctx = egui::Context::default();
    let mut cfg = grid_config();
    cfg.editor.grid_enabled = false; // the single-pane path, for isolation
    let mut app = ScribeApp::new_test(cfg);
    app.tabs[0].text = big_text(400);

    raw_frame(&mut app, &ctx);
    let toast = app
        .toast
        .clone()
        .expect("the unrequested swap into the rope editor must announce itself");
    assert!(
        toast.contains("large-file threshold"),
        "the notice must say WHY the editor changed, got {toast:?}"
    );

    // One-shot: a steady-state frame in the same mode must not keep re-raising it.
    app.toast = None;
    raw_frame(&mut app, &ctx);
    assert_eq!(
        app.toast, None,
        "the notice fires on the TRANSITION, not on every frame in the mode"
    );
}

/// …and the surfaces the user asked for stay silent.
#[test]
fn a_user_requested_surface_raises_no_swap_notice() {
    assert_eq!(
        EditorMode::Fold.entry_notice(),
        None,
        "folding is an explicit user action — announcing it is noise"
    );
    assert_eq!(
        EditorMode::Standard.entry_notice(),
        None,
        "the full-feature default is not a degradation"
    );
    assert!(
        EditorMode::Rope.entry_notice().is_some() && EditorMode::RopeMmap.entry_notice().is_some(),
        "the automatic large-file swap is the one the user never requested"
    );
}

// ---------------------------------------------------------------------------
// Defect 1 — grid pane parity
// ---------------------------------------------------------------------------

/// Clicking a pane must make it the tab the rest of the app talks about.
///
/// Nothing moved `self.active` in grid view, so the status bar's counters (and
/// encoding, language, EOL, caret readout) described whichever tab happened to
/// be active when the grid opened — never the pane being typed in. Asserted on
/// the RENDERED counter string, not on `self.active`.
#[test]
fn clicking_a_grid_pane_makes_the_status_bar_describe_that_pane() {
    // Distinct counts so the status-bar segment identifies the tab unambiguously.
    let mut app = grid_app("one\ntwo\nthree", "solo");
    // Pin tab 0 so tab 1's pane owns the only "Pin note" glyph → its body is
    // locatable without guessing the grid geometry.
    app.tabs[0].pinned = true;
    let mut h = harness(app);
    h.run();
    h.run();
    assert!(
        h.query_by_label("3 ln · 3 w · 13 ch").is_some(),
        "precondition: the status bar starts on tab 0"
    );

    let body = unpinned_pane_body(&h);
    click_at(&mut h, body, false);
    h.run();
    h.run();

    assert_eq!(
        h.state().active,
        1,
        "the clicked pane became the active tab"
    );
    assert!(
        h.query_by_label("1 ln · 1 w · 4 ch").is_some(),
        "the status bar must now describe the pane the user clicked into"
    );
}

/// The pane's right-click menu must exist AND its pick must be drained and
/// applied — against the pane that was clicked.
///
/// The pane had no `context_menu` at all, and even if it had, nothing in the
/// grid path drained `editor_ctx_cmd_id` or the `pending_*` latches the drain
/// raises: choosing a caret command in split view latched forever and fired only
/// when the user turned the grid back off.
#[test]
fn a_grid_panes_right_click_menu_applies_its_pick_to_that_pane() {
    let mut app = grid_app("- untouched", "- item");
    app.tabs[0].pinned = true; // tab 1 owns the unique Pin glyph
    let mut h = harness(app);
    h.run();
    h.run();

    let body = unpinned_pane_body(&h);
    // Focus + place the caret, then open the menu over the same point.
    click_at(&mut h, body, false);
    h.run();
    click_at(&mut h, body, true);
    h.run();

    h.get_by_label("Toggle task  [ ] / [x]").click();
    h.run(); // the pick is published to ctx-data
    h.run(); // …drained + applied by the grid path
    h.run();

    assert_eq!(
        h.state().tabs[1].text,
        "- [ ] item",
        "the menu pick must reach the pane it was opened on"
    );
    assert_eq!(
        h.state().tabs[0].text,
        "- untouched",
        "and must NOT touch the other pane"
    );
}

/// `scroll_metrics` is the measurement the minimap's viewport indicator and both
/// scroll assists read. The grid never wrote it, so it stayed on whatever the
/// last single-pane frame left there (or the `(0.0, 1.0, 1.0)` seed) — a minimap
/// that lies and assists that cannot compute a scroll range.
#[test]
fn a_grid_pane_records_the_scroll_metrics_the_minimap_reads() {
    // Under the rope threshold so this exercises the TextEdit pane; long enough
    // that the content genuinely overflows the pane.
    let mut cfg = grid_config();
    cfg.editor.rope_editor_auto_threshold_bytes = 0; // never auto-swap
    let mut app = ScribeApp::new_test(cfg);
    app.tabs[0].text = big_text(300);
    app.tabs.push(EditorTab::scratch());
    let mut h = harness(app);
    h.run();
    h.run();

    let (off, content_h, view_h) = h.state().scroll_metrics;
    assert!(
        view_h > 100.0 && view_h < 760.0,
        "view height must be the PANE's height, got {view_h}"
    );
    assert!(
        content_h > view_h * 2.0,
        "300 lines must measure as content far taller than one pane \
         (content {content_h}, view {view_h})"
    );
    assert_eq!(off, 0.0, "an untouched pane sits at the top");
}

/// PageUp/PageDown are implemented by `page_key_assist`, which rides
/// `drag_scroll_assist`. The grid called neither, so the page keys did nothing
/// at all in split view.
#[test]
fn page_down_in_a_grid_pane_moves_the_caret_a_whole_page() {
    let mut cfg = grid_config();
    cfg.editor.rope_editor_auto_threshold_bytes = 0; // TextEdit pane
    let mut app = ScribeApp::new_test(cfg);
    app.tabs[0].text = big_text(300);
    app.tabs.push(EditorTab::scratch());
    app.tabs[1].pinned = true; // tab 0 owns the unique Pin glyph
    let mut h = harness(app);
    h.run();
    h.run();
    // AFTER the first frames: `sync_grid_state` allocates the real doc ids, so a
    // pre-run snapshot would address the `DocId(0)` sentinel and silently miss.
    let doc0 = h.state().tabs[0].doc_id;

    let body = unpinned_pane_body(&h);
    click_at(&mut h, body, false);
    set_pane_caret(&h.ctx, doc0, 0);
    h.run();

    h.input_mut().events.push(egui::Event::Key {
        key: egui::Key::PageDown,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    });
    h.run();

    let caret = pane_caret(&h.ctx, doc0).expect("the pane editor stored a caret");
    let text = h.state().tabs[0].text.clone();
    let line = line_of(&text, caret);
    assert!(
        line >= 5,
        "PageDown must move the caret a PAGE, not a line — landed on line {line}"
    );
}

/// Multi-cursor was entirely absent from the grid: the pre-render key
/// interception never ran, so a keystroke went to egui's single primary caret
/// and the secondaries were neither edited nor drawn.
#[test]
fn a_typed_character_replays_at_every_caret_in_a_grid_pane() {
    let mut cfg = grid_config();
    cfg.editor.rope_editor_auto_threshold_bytes = 0; // TextEdit pane
    let mut app = ScribeApp::new_test(cfg);
    app.tabs[0].text = "abab".into();
    app.tabs.push(EditorTab::scratch());
    app.tabs[1].pinned = true; // tab 0 owns the unique Pin glyph
    let mut h = harness(app);
    h.run();
    h.run();
    // AFTER the first frames — `sync_grid_state` allocates the real doc ids.
    let doc0 = h.state().tabs[0].doc_id;

    let body = unpinned_pane_body(&h);
    click_at(&mut h, body, false);
    h.run();

    // Primary caret after index 1, one secondary after index 3.
    set_pane_caret(&h.ctx, doc0, 1);
    h.state_mut()
        .multi_cursor
        .set_secondaries(vec![crate::multi_cursor::Caret::at(3)]);
    h.state_mut().mc_owner_doc = Some(doc0);

    h.input_mut()
        .events
        .push(egui::Event::Text("X".to_string()));
    h.run();

    let text = h.state().tabs[0].text.clone();
    assert_eq!(
        text.matches('X').count(),
        2,
        "the keystroke must be replayed at the primary AND the secondary caret; \
         got {text:?} (one X means egui applied it to the primary only, i.e. the \
         multi-cursor interception never ran for this pane)"
    );
}

/// The pane editor's id is derived from the DOCUMENT, not from the tiles `Ui`
/// path — so it survives every rearrangement of the grid, and two panes can
/// never collide on one `TextEditState`.
#[test]
fn pane_editor_ids_are_document_scoped_and_distinct() {
    let a = grid_methods::pane_editor_id(crate::grid::DocId(1));
    let b = grid_methods::pane_editor_id(crate::grid::DocId(2));
    assert_ne!(a, b, "two panes must not share one caret/undo state");
    assert_eq!(
        a,
        grid_methods::pane_editor_id(crate::grid::DocId(1)),
        "the id must be reproducible from the doc id alone"
    );
    // …and distinct from the single-pane editor's id for the same document, so
    // toggling the grid cannot make the two surfaces fight over one state.
    assert_ne!(
        a,
        egui::Id::new("scr1b3-central-editor").with(crate::grid::DocId(1))
    );
}

// ---------------------------------------------------------------------------
// Defect 3 — the per-pane editor OVERLAYS
//
// Three capabilities the single-pane editor has and the grid pane did not,
// because all three live inside the `else` arm of `frame_tick`'s
// `if self.grid_tree.is_some() { … } else { … }` fork:
//
//   A. the inline LSP diagnostic squiggle + hover (grid_render.rs and
//      grid_methods.rs did not contain the string "diagnostic" at all, so with
//      split view on the user was back to the two status-bar integers that do
//      not say WHICH line is wrong — exactly the problem the overlay exists to
//      solve);
//   B. the `[[wiki-link]]` Ctrl+click follow;
//   C. the Ctrl+V clipboard-image attachment paste;
//
// …plus D, found while wiring C: the markdown chords (Ctrl+B / Ctrl+I /
// Ctrl+` / Ctrl+Shift+X / Ctrl+Enter) are intercepted in that SAME single-pane
// block, so they did nothing in split view even though the grid already
// drained the `pending_*` latches they raise.
//
// Every test below asserts the OBSERVABLE outcome — the squiggle segments
// actually painted into the frame, the tooltip text actually painted, the note
// actually created on disk, the markdown actually in the buffer — never an
// intermediate latch. Each was verified by cutting its wire and confirming the
// failure before being committed.
// ---------------------------------------------------------------------------

/// A raw headless context that hands back the frame's SHAPE LIST.
///
/// `e2e::Driver` discards `FullOutput`, and `egui_kittest`'s renderer needs a
/// GPU these tests deliberately do not require. The overlays pinned here are
/// PAINT, so the shape list is the observable outcome: a squiggle is a run of
/// `Shape::LineSegment`s in the severity colour (`render_support::paint_squiggle`
/// emits nothing else), and a tooltip is a `Shape::Text` carrying the message.
/// Both are decidable from the shapes alone, with no pixels and no GPU.
struct Probe {
    ctx: egui::Context,
}

impl Probe {
    fn new() -> Self {
        Self {
            ctx: egui::Context::default(),
        }
    }

    fn frame(
        &self,
        app: &mut ScribeApp,
        modifiers: egui::Modifiers,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(1100.0, 720.0),
            )),
            modifiers,
            events,
            ..Default::default()
        };
        self.ctx.run(input, |ctx| app.frame_tick(ctx))
    }

    fn idle(&self, app: &mut ScribeApp) -> egui::FullOutput {
        self.frame(app, egui::Modifiers::NONE, Vec::new())
    }

    /// Settle the app: `sync_grid_state` allocates doc ids on the first frame
    /// and the panes lay out on the second, so nothing is measurable before
    /// the third.
    fn settle(&self, app: &mut ScribeApp) {
        self.idle(app);
        self.idle(app);
    }

    fn hover(&self, app: &mut ScribeApp, pos: egui::Pos2) -> egui::FullOutput {
        self.frame(
            app,
            egui::Modifiers::NONE,
            vec![egui::Event::PointerMoved(pos)],
        )
    }

    /// Move + press + release at `pos` in ONE frame — the same shape the
    /// single-pane link tests use.
    fn mod_click(
        &self,
        app: &mut ScribeApp,
        pos: egui::Pos2,
        modifiers: egui::Modifiers,
    ) -> egui::FullOutput {
        self.frame(
            app,
            modifiers,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers,
                },
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers,
                },
            ],
        )
    }
}

/// egui nests shapes in `Shape::Vec`, so a flat scan of `FullOutput::shapes`
/// misses everything a panel painted.
fn walk_shape(shape: &egui::Shape, f: &mut impl FnMut(&egui::Shape)) {
    if let egui::Shape::Vec(inner) = shape {
        for s in inner {
            walk_shape(s, f);
        }
    } else {
        f(shape);
    }
}

/// Every `LineSegment` painted this frame whose stroke is EXACTLY `color`.
///
/// The squiggle is the only thing that paints 1px line segments in a severity
/// colour over the editor; the gutter's diagnostic marker is a `rect_filled`
/// and the secondary-caret painter uses a 1.5px accent stroke, so neither can
/// be counted here. Every assertion below is additionally differential against
/// a diagnostics-free control render, so any future same-colour painter would
/// have to be diagnostics-DEPENDENT to fool it.
fn squiggle_segments(out: &egui::FullOutput, color: Color32) -> Vec<[egui::Pos2; 2]> {
    let mut hits = Vec::new();
    for clipped in &out.shapes {
        walk_shape(&clipped.shape, &mut |s| {
            if let egui::Shape::LineSegment { points, stroke } = s {
                if stroke.color == color {
                    hits.push(*points);
                }
            }
        });
    }
    hits
}

/// Every string this frame actually painted as text — the tooltip's own body
/// included, since `show_tooltip_at_pointer` lays its label out into the frame.
fn painted_text(out: &egui::FullOutput) -> String {
    let mut acc = String::new();
    for clipped in &out.shapes {
        walk_shape(&clipped.shape, &mut |s| {
            if let egui::Shape::Text(t) = s {
                acc.push_str(t.galley.text());
                acc.push('\n');
            }
        });
    }
    acc
}

/// Where the pane showing `doc` actually laid out this frame.
///
/// Read back from the live tile tree rather than guessed from the window size:
/// egui_tiles picks its own column count from the container's aspect ratio, so
/// a hard-coded "the second pane is on the right" would silently pass or fail
/// on a layout change instead of testing the overlay.
fn pane_rect(app: &ScribeApp, doc: crate::grid::DocId) -> Option<egui::Rect> {
    let tree = app.grid_tree.as_ref()?;
    let id = tree.tiles.iter().find_map(|(id, tile)| match tile {
        egui_tiles::Tile::Pane(p) if p.doc_id == doc => Some(*id),
        _ => None,
    })?;
    tree.tiles.rect(id)
}

fn segments_bbox(segs: &[[egui::Pos2; 2]]) -> egui::Rect {
    assert!(!segs.is_empty(), "no ink to measure");
    let mut r = egui::Rect::NOTHING;
    for [a, b] in segs {
        r.extend_with(*a);
        r.extend_with(*b);
    }
    r
}

// ---- A: inline LSP diagnostics --------------------------------------------

/// Four lines of EXACTLY 32 characters, so a column index maps to an x offset
/// by a single multiply and a mis-placed squiggle is arithmetic, not opinion.
const DIAG_SRC: &str = "0123456789abcdefghijklmnopqrstuv\n\
                        second line holds the error span\n\
                        third line holds a warning token\n\
                        fourth line holds the info notic\n";

fn diag(line: u32, ch: u32, end_line: u32, end_ch: u32, severity: u8, message: &str) -> Diagnostic {
    Diagnostic {
        uri: "file:///grid-diag.txt".into(),
        line,
        character: ch,
        end_line,
        end_character: end_ch,
        severity,
        message: message.into(),
    }
}

/// A grid config that renders the plain `TextEdit` pane and paints no OTHER
/// red ink.
///
/// Spellcheck is OFF deliberately: its squiggle uses the SAME `#e53e3e` as the
/// default `error` colour, so leaving it on would put error-coloured segments
/// in the CONTROL frame and hollow out the zero-ink assertion.
fn diag_grid_config() -> Config {
    let mut cfg = grid_config();
    cfg.editor.rope_editor_auto_threshold_bytes = 0; // never auto-swap to the rope pane
    cfg.spellcheck.enabled = false;
    cfg
}

/// Two panes, both holding [`DIAG_SRC`], with `diags` published. Both panes
/// carry the SAME text so the only thing that can move the ink between them is
/// which pane is ACTIVE.
/// [`DIAG_SRC`] followed by enough filler that the text fills a whole pane.
///
/// The diagnostics are all published against lines 0-3, so APPENDING lines
/// cannot move them — but it does mean a click anywhere in the pane body lands
/// on the `TextEdit` rather than on the empty space below a four-line buffer.
/// That mattered: `the_grid_squiggle_follows_the_pane_the_user_clicked_into`
/// clicks 60% of the way down the pane, which with the bare four lines fell
/// past the end of the widget, gave the pane editor no focus, and failed its
/// own `active == 1` precondition — a fixture-geometry bug that looked exactly
/// like the focus->active sync being broken.
fn diag_grid_text() -> String {
    let mut s = DIAG_SRC.to_string();
    for _ in 0..200 {
        s.push_str("filler line, no diagnostic published against it\n");
    }
    s
}

fn diag_grid_app(diags: Vec<Diagnostic>) -> ScribeApp {
    let mut app = ScribeApp::new_test(diag_grid_config());
    let text = diag_grid_text();
    app.tabs[0].text.clone_from(&text);
    app.tabs.push(EditorTab::scratch());
    app.tabs[1].text = text;
    app.active = 0;
    app.diagnostics = diags;
    app
}

fn error_color(app: &ScribeApp) -> Color32 {
    ui_color(&app.theme, "error", Rgba::new(0xe5, 0x3e, 0x3e, 255))
}

/// The frame's error-coloured ink after settling, for `diags`.
fn diag_ink(diags: Vec<Diagnostic>) -> Vec<[egui::Pos2; 2]> {
    let mut app = diag_grid_app(diags);
    let err = error_color(&app);
    let p = Probe::new();
    p.settle(&mut app);
    let out = p.idle(&mut app);
    squiggle_segments(&out, err)
}

/// The overlay must actually PAINT in a grid pane — and the control proves the
/// ink is the diagnostics' and nothing else's.
///
/// Before this wiring the grid path never mentioned `diagnostic` at all: a
/// rendered probe with `grid_enabled` showed gutter bars and the `1e / 3`
/// counter and NO squiggle on any line, because the paint block sits in the
/// single-pane arm of the `grid_tree.is_some()` fork.
#[test]
fn a_grid_pane_paints_the_inline_diagnostic_squiggle() {
    let ink = diag_ink(vec![diag(
        1,
        22,
        1,
        27,
        crate::app::diagnostics_overlay::SEVERITY_ERROR,
        "cannot find value `error` in this scope",
    )]);
    assert!(
        !ink.is_empty(),
        "a published error diagnostic must paint a squiggle in the active grid \
         pane; the frame carried no error-coloured line segments at all"
    );

    let control = diag_ink(Vec::new());
    assert!(
        control.is_empty(),
        "an otherwise identical app with NO diagnostics must paint no \
         error-coloured ink — {} segments found, so the assertion above is not \
         measuring the diagnostics",
        control.len()
    );
}

/// …and it lands where the server said, not at a fixed spot.
///
/// Differential and constant-free: a diagnostic on a LATER line must ink lower
/// than one on an earlier line, and one starting at a LATER column must ink to
/// the right of one at the start of a line. A painter that ignored the span
/// (or resolved it against the wrong text) passes the presence test above and
/// fails both of these.
#[test]
fn the_grid_squiggle_lands_on_the_line_and_columns_the_server_named() {
    const ERR: u8 = crate::app::diagnostics_overlay::SEVERITY_ERROR;
    let first = segments_bbox(&diag_ink(vec![diag(0, 0, 0, 5, ERR, "at the very start")]));
    let later = segments_bbox(&diag_ink(vec![diag(
        2,
        20,
        2,
        30,
        ERR,
        "further down and right",
    )]));
    assert!(
        later.min.y > first.max.y,
        "a diagnostic on line 2 must ink BELOW one on line 0 \
         (line 0 ink {first:?}, line 2 ink {later:?})"
    );
    assert!(
        later.min.x > first.max.x,
        "a diagnostic starting at column 20 must ink to the RIGHT of one \
         covering columns 0..5 (line 0 ink {first:?}, line 2 ink {later:?})"
    );
}

/// A grid config whose panes render through the OWNED ROPE editor.
///
/// The sibling [`diag_grid_config`] sets `rope_editor_auto_threshold_bytes = 0`
/// precisely to keep the rope arm OUT of the way; this is its mirror.
/// `experimental_rope_editor` rather than a 16 MiB fixture on purpose: the
/// threshold selects the SAME `show_editable` path, and a real 16 MiB buffer
/// would make the test minutes long for no extra coverage.
fn diag_grid_rope_config() -> Config {
    let mut cfg = grid_config();
    cfg.editor.experimental_rope_editor = true;
    cfg.spellcheck.enabled = false; // its squiggle is the SAME #e53e3e
    cfg
}

fn diag_grid_rope_app(diags: Vec<Diagnostic>) -> ScribeApp {
    let mut app = ScribeApp::new_test(diag_grid_rope_config());
    let text = diag_grid_text();
    app.tabs[0].text.clone_from(&text);
    app.tabs.push(EditorTab::scratch());
    app.tabs[1].text = text;
    app.active = 0;
    app.diagnostics = diags;
    app
}

/// The frame's error-coloured ink after settling, for `diags` — with the
/// grid-rope preconditions asserted, so an empty result can only mean "no ink",
/// never "no pane".
fn rope_pane_diag_ink(diags: Vec<Diagnostic>) -> Vec<[egui::Pos2; 2]> {
    let mut app = diag_grid_rope_app(diags);
    let err = error_color(&app);
    let p = Probe::new();
    p.settle(&mut app);
    let out = p.idle(&mut app);
    assert!(
        app.grid_tree.is_some(),
        "precondition: the grid owns the central surface, so the single-pane \
         arm (which does paint) is not what produced this frame"
    );
    let active = app.active;
    assert!(
        app.tabs[active].rope_state.is_some(),
        "precondition: the active pane rendered through the OWNED ROPE arm — \
         `rope_state` is created by that arm and by nothing else here. Without \
         this, a blank or missing pane would look exactly like a passing \
         zero-ink control."
    );
    squiggle_segments(&out, err)
}

/// The grid's owned-rope arm must paint the inline diagnostic squiggle.
///
/// It painted nothing: the arm built the rope, called `show_editable`, recorded
/// scroll metrics and returned. That is the arm a buffer is AUTO-PROMOTED into
/// past `rope_editor_auto_threshold_bytes` (16 MiB by default) — i.e. exactly
/// the large files where a language server earns its keep — so a big note in a
/// pane silently lost every squiggle, every gutter mark and the hover, leaving
/// the two status-bar integers that do not say WHICH line is wrong. The Rope
/// entry-notice tells the user "inline diagnostics still work"; on this surface
/// that claim was false.
#[test]
fn a_grid_rope_pane_paints_the_inline_diagnostic_squiggle() {
    let ink = rope_pane_diag_ink(vec![diag(
        1,
        22,
        1,
        27,
        crate::app::diagnostics_overlay::SEVERITY_ERROR,
        "cannot find value `error` in this scope",
    )]);
    assert!(
        !ink.is_empty(),
        "a published error diagnostic must paint a squiggle in the active grid \
         ROPE pane; the frame carried no error-coloured line segments at all"
    );

    let control = rope_pane_diag_ink(Vec::new());
    assert!(
        control.is_empty(),
        "an otherwise identical rope-pane app with NO diagnostics must paint no \
         error-coloured ink — {} segments found, so the assertion above is not \
         measuring the diagnostics",
        control.len()
    );
}

/// …and it lands where the server said, not at a fixed spot.
///
/// Differential and constant-free, mirroring
/// `the_grid_squiggle_lands_on_the_line_and_columns_the_server_named`: a
/// diagnostic on a LATER line must ink lower, and one starting at a LATER column
/// must ink further right. A painter that ignored the span — or resolved it
/// against the wrong text, which is the specific failure mode of reusing the
/// ACTIVE tab's spans in a multi-pane surface — passes the presence test above
/// and fails both of these.
#[test]
fn the_grid_rope_squiggle_lands_on_the_line_and_columns_the_server_named() {
    const ERR: u8 = crate::app::diagnostics_overlay::SEVERITY_ERROR;
    let first = segments_bbox(&rope_pane_diag_ink(vec![diag(
        0,
        0,
        0,
        5,
        ERR,
        "at the very start",
    )]));
    let later = segments_bbox(&rope_pane_diag_ink(vec![diag(
        2,
        20,
        2,
        30,
        ERR,
        "further down and right",
    )]));
    assert!(
        later.min.y > first.max.y,
        "a diagnostic on line 2 must ink BELOW one on line 0 (first {first:?}, \
         later {later:?})"
    );
    assert!(
        later.min.x > first.max.x,
        "a diagnostic starting at column 20 must ink RIGHT of one starting at \
         column 0 (first {first:?}, later {later:?})"
    );
}

/// The SINGLE-PANE rope path must still paint its overlay after the extraction.
///
/// `ScribeApp::paint_rope_diagnostics` now delegates to the same free function
/// the grid rope arm calls, so the two surfaces cannot drift. That refactor had
/// no running guard: the only tests covering the single-pane rope overlay
/// (`visual_qa::rope_path_paints_the_inline_diagnostic_overlay` and its control)
/// are `#[ignore]`d GPU renders, and a test category nothing runs offers zero
/// protection. This is the headless shape-list equivalent, so a delegation that
/// silently stopped painting turns the suite RED on any host.
///
/// `grid_tree.is_none()` is asserted as a PRECONDITION: it is what distinguishes
/// this from the grid-pane tests above — without it, a fixture that quietly took
/// the grid path would be measuring the arm it is not supposed to cover.
#[test]
fn the_single_pane_rope_path_still_paints_the_inline_diagnostic_squiggle() {
    fn single_pane_rope_ink(diags: Vec<Diagnostic>) -> Vec<[egui::Pos2; 2]> {
        let mut cfg = grid_config();
        cfg.editor.grid_enabled = false; // the single-pane arm, not the grid
        cfg.editor.experimental_rope_editor = true;
        cfg.spellcheck.enabled = false; // its squiggle is the SAME #e53e3e
        let mut app = ScribeApp::new_test(cfg);
        app.tabs[0].text = diag_grid_text();
        app.diagnostics = diags;

        let err = error_color(&app);
        let p = Probe::new();
        p.settle(&mut app);
        let out = p.idle(&mut app);
        assert!(
            app.grid_tree.is_none(),
            "precondition: the SINGLE-PANE arm must own the surface — with a \
             grid tree this would be measuring the pane path instead"
        );
        assert!(
            app.tabs[0].rope_state.is_some(),
            "precondition: the rope arm must be the one rendering — `rope_state` \
             is created by that arm and by nothing else here"
        );
        squiggle_segments(&out, err)
    }

    let ink = single_pane_rope_ink(vec![diag(
        1,
        22,
        1,
        27,
        crate::app::diagnostics_overlay::SEVERITY_ERROR,
        "cannot find value `error` in this scope",
    )]);
    assert!(
        !ink.is_empty(),
        "the single-pane rope path must still paint a squiggle after \
         `paint_rope_diagnostics` was reduced to a delegate"
    );

    let control = single_pane_rope_ink(Vec::new());
    assert!(
        control.is_empty(),
        "an otherwise identical single-pane rope app with NO diagnostics must \
         paint no error-coloured ink — {} segments found, so the assertion \
         above is not measuring the diagnostics",
        control.len()
    );
}

/// Hovering the squiggle must NAME the problem. Two integers in the status bar
/// do not tell the user why a line is wrong; the tooltip is the whole payload.
///
/// Asserted on the text the frame actually painted, so a tooltip that is built
/// but never shown fails.
#[test]
fn hovering_a_grid_panes_squiggle_names_the_problem() {
    const ERR: u8 = crate::app::diagnostics_overlay::SEVERITY_ERROR;
    let mut app = diag_grid_app(vec![diag(1, 22, 1, 27, ERR, "cannot find value `error`")]);
    let err = error_color(&app);
    let p = Probe::new();
    p.settle(&mut app);
    let ink = segments_bbox(&squiggle_segments(&p.idle(&mut app), err));

    // The hover rect the painter registers spans the whole galley ROW, with the
    // squiggle along its BOTTOM edge — so aim just above the ink, inside the row.
    let target = egui::pos2(ink.center().x, ink.max.y - 5.0);
    p.hover(&mut app, target);
    let out = p.hover(&mut app, target);
    let painted = painted_text(&out);
    assert!(
        painted.contains("error: cannot find value `error`"),
        "hovering the squiggle must paint the severity-prefixed message; the \
         frame painted no such text (hovered {target:?}, ink {ink:?})"
    );

    // …and hovering off the underline must NOT: a tooltip that follows the
    // pointer anywhere on the line is the "same line, wrong character" bug the
    // hover resolver exists to avoid.
    let away = egui::pos2(ink.min.x, ink.max.y + 220.0);
    p.hover(&mut app, away);
    let off = p.hover(&mut app, away);
    assert!(
        !painted_text(&off).contains("error: cannot find value"),
        "the diagnostic tooltip must not show while the pointer is off the \
         squiggle"
    );
}

/// The squiggle is `active`-keyed, so it must follow the pane the user clicked
/// into.
///
/// This is the trap commit `7595dea` named: nothing used to move `self.active`
/// in grid view, so every `active`-keyed surface described whichever tab was
/// active when the grid opened. `diag_spans` are resolved from `self.active`,
/// so resolving them BEFORE the focus->active sync would paint the squiggle in
/// the pane the user just left. Both panes carry identical text here, so the
/// span is the same either way and the ONLY thing that can move the ink is
/// which pane owns it.
#[test]
fn the_grid_squiggle_follows_the_pane_the_user_clicked_into() {
    const ERR: u8 = crate::app::diagnostics_overlay::SEVERITY_ERROR;
    let mut app = diag_grid_app(vec![diag(1, 22, 1, 27, ERR, "cannot find value `error`")]);
    let err = error_color(&app);
    let p = Probe::new();
    p.settle(&mut app);
    let before = segments_bbox(&squiggle_segments(&p.idle(&mut app), err));
    assert_eq!(app.active, 0, "precondition: pane 0 starts active");

    // Into the SECOND pane's body — located from the live tile tree, not from a
    // guessed half of the window. Below the pane's header chip so the click
    // lands on the editor rather than on the chip's controls.
    let other = app.tabs[1].doc_id;
    let rect = pane_rect(&app, other).expect("the second pane laid out");
    let body = egui::pos2(rect.center().x, rect.top() + rect.height() * 0.6);
    p.mod_click(&mut app, body, egui::Modifiers::NONE);
    p.idle(&mut app);
    assert_eq!(
        app.active, 1,
        "precondition: clicking the second pane's body at {body:?} (pane rect \
         {rect:?}) must make it active"
    );

    let after = segments_bbox(&squiggle_segments(&p.idle(&mut app), err));
    let moved = (after.center() - before.center()).length();
    assert!(
        moved > 100.0,
        "the squiggle must move into the pane the user is now editing; it \
         stayed put ({before:?} -> {after:?}, moved {moved}px)"
    );
}

// ---- B / C / D: link follow, image paste, markdown chords ------------------

struct Vault {
    _root: tempfile::TempDir,
    path: std::path::PathBuf,
}

fn vault() -> Vault {
    let root = tempfile::tempdir().expect("temp root");
    let path = root.path().join("vault");
    std::fs::create_dir_all(&path).expect("vault dir");
    Vault { _root: root, path }
}

/// A SINGLE-tab grid app with a vault configured.
///
/// One tab on purpose: a one-pane grid fills the window, so the click geometry
/// below matches the single-pane link tests exactly and the tests measure the
/// wiring rather than the tiling. The code path is the same either way — what
/// selects it is `grid_tree.is_some()`, asserted as a precondition in each test.
fn grid_vault_app(v: &std::path::Path) -> ScribeApp {
    let mut cfg = grid_config();
    cfg.editor.rope_editor_auto_threshold_bytes = 0;
    cfg.notes.vault_dir = Some(v.to_path_buf());
    ScribeApp::new_test(cfg)
}

/// A buffer whose every byte sits inside a link span, so the pointer hit-test
/// cannot land in a gap between links. Taller than the window (any y lands on
/// text) and each line is ~60 chars — wide enough that [`CLICK`]'s x is mid-row,
/// narrow enough not to soft-wrap.
fn wall_of(link: &str) -> String {
    let line = link.repeat(60_usize.div_ceil(link.len()));
    assert!(
        (60..90).contains(&line.chars().count()),
        "fixture geometry: {line:?}"
    );
    (0..200)
        .map(|_| line.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Well inside the pane body, on both axes (below the pane header chip).
const CLICK: egui::Pos2 = egui::Pos2::new(150.0, 380.0);
const CMD: egui::Modifiers = egui::Modifiers::COMMAND;

fn solid_image(w: usize, h: usize) -> crate::app::note_capture::ClipboardImage {
    crate::app::note_capture::ClipboardImage {
        width: w,
        height: h,
        rgba: vec![0x40u8; w * h * 4],
    }
}

/// Focus the one grid pane by clicking into it, and prove the click landed —
/// every chord/paste hook below is gated on the pane editor owning focus, so a
/// silently-unfocused pane would make them all vacuously "pass" as no-ops.
fn focus_the_pane(p: &Probe, app: &mut ScribeApp) {
    let doc = app.tabs[0].doc_id;
    p.mod_click(app, CLICK, egui::Modifiers::NONE);
    p.idle(app);
    assert!(
        p.ctx
            .memory(|m| m.has_focus(grid_methods::pane_editor_id(doc))),
        "precondition: clicking the pane body must give its editor keyboard focus"
    );
}

/// Ctrl+clicking a `[[wiki-link]]` IN A GRID PANE opens the note — creating it
/// when it does not exist, exactly as the single-pane editor does.
///
/// The grid pane has its own editor render path with its own overlay pass, and
/// the link hit-test was never wired into it. Asserted on the file that appears
/// and the tab that ends up active, never on the intermediate ctx-data stash —
/// which is also what proves the pane writes the SAME slot the single-pane
/// drain reads: a divergent key would leave the note uncreated.
#[test]
fn ctrl_clicking_a_wikilink_in_a_grid_pane_opens_the_note() {
    let v = vault();
    let mut app = grid_vault_app(&v.path);
    app.tabs[0].text = wall_of("[[Target]]");

    let p = Probe::new();
    p.settle(&mut app);
    assert!(
        app.grid_tree.is_some(),
        "precondition: the grid path is the one under test"
    );
    assert!(
        !v.path.join("Target.md").exists(),
        "precondition: the link target does not exist yet"
    );

    p.mod_click(&mut app, CLICK, CMD);
    p.idle(&mut app); // the stashed follow is drained at the top of the frame
    p.idle(&mut app); // …and the pane the new tab gained lays out

    let target = v.path.join("Target.md");
    assert!(
        target.exists(),
        "the clicked [[Target]] must be created in the vault"
    );
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "# Target\n",
        "created through open_or_create_wikilink, seeded with its heading"
    );
    // …and it is OPEN and ON SCREEN. The single-pane test asserts the target is
    // the ACTIVE tab; in grid view `self.active` deliberately follows KEYBOARD
    // FOCUS (the fix commit `7595dea` landed), and focus stays in the pane the
    // user Ctrl+clicked in — so the note arrives as its own pane beside the
    // source note rather than replacing what the user was reading. Asserting
    // `active` here would be asserting the single-pane behaviour against the
    // grid's, so assert what the user actually sees instead: a tab for the
    // target, with a pane of its own.
    let opened = app
        .tabs
        .iter()
        .find(|t| t.doc.path() == Some(target.as_path()))
        .expect("the followed link must be open in a tab");
    assert!(
        pane_rect(&app, opened.doc_id).is_some(),
        "…and that tab must be gridded, i.e. actually visible to the user"
    );
}

/// A plain (unmodified) click over a wiki-link must NOT follow it — the
/// modifier is the whole consent gesture, same as the single-pane URL arm.
#[test]
fn a_plain_click_on_a_wikilink_in_a_grid_pane_opens_nothing() {
    let v = vault();
    let mut app = grid_vault_app(&v.path);
    app.tabs[0].text = wall_of("[[Target]]");

    let p = Probe::new();
    p.settle(&mut app);
    p.mod_click(&mut app, CLICK, egui::Modifiers::NONE);
    p.idle(&mut app);

    assert!(
        !v.path.join("Target.md").exists(),
        "an unmodified click must not create or open the note"
    );
}

/// The traversal gate is the resolver's, not a second copy: a link that escapes
/// the vault is refused and writes nothing outside it. This is the security
/// property that would be lost if the grid pane grew its OWN follow path.
#[test]
fn a_traversal_wikilink_clicked_in_a_grid_pane_is_refused() {
    let v = vault();
    let outside = v.path.parent().unwrap().to_path_buf();
    let mut app = grid_vault_app(&v.path);
    app.tabs[0].text = wall_of("[[../escaped]]");

    let p = Probe::new();
    p.settle(&mut app);
    p.mod_click(&mut app, CLICK, CMD);
    p.idle(&mut app);

    assert!(
        !outside.join("escaped.md").exists(),
        "nothing may be written outside the vault"
    );
    assert!(
        app.toast
            .as_deref()
            .unwrap_or("")
            .contains("Can't open that link"),
        "the refusal is surfaced, not silent: {:?}",
        app.toast
    );
}

/// Hovering a wiki-link in a grid pane names the TARGET before the click — an
/// aliased link must not hide where it goes.
#[test]
fn hovering_a_wikilink_in_a_grid_pane_previews_its_target() {
    let v = vault();
    let mut app = grid_vault_app(&v.path);
    app.tabs[0].text = wall_of("[[Target|shown]]");

    let p = Probe::new();
    p.settle(&mut app);
    p.hover(&mut app, CLICK);
    let out = p.hover(&mut app, CLICK);
    assert!(
        painted_text(&out).contains("[[Target]]"),
        "the hover must name the link TARGET, not its display alias"
    );
}

/// Ctrl+V with an image on the clipboard pastes the attachment INTO THE GRID
/// PANE'S buffer. The end of the wire — `pending_insert_text.is_some()` would
/// still pass with the drain deleted.
#[test]
fn ctrl_v_with_a_clipboard_image_pastes_into_a_grid_pane() {
    let v = vault();
    let mut app = grid_vault_app(&v.path);
    app.tabs[0].text = "before\n".repeat(80);

    let p = Probe::new();
    p.settle(&mut app);
    assert!(
        app.grid_tree.is_some(),
        "precondition: the grid path is live"
    );
    focus_the_pane(&p, &mut app);

    crate::app::note_capture::test_hooks::set_next_image(solid_image(2, 2));
    // The RELEASE alone, which is the ONLY event `egui_winit` emits for an
    // image-only clipboard: it special-cases the paste chord on key-DOWN and
    // returns before pushing any `Event::Key`. A press-watching hook is dead
    // code that a press+release test helper would still pass.
    p.frame(
        &mut app,
        CMD,
        vec![egui::Event::Key {
            key: egui::Key::V,
            physical_key: None,
            pressed: false,
            repeat: false,
            modifiers: CMD,
        }],
    );
    p.idle(&mut app); // deliver the queued insertion
    p.idle(&mut app); // …and let the pane editor settle it

    let text = app.tabs[0].text.clone();
    assert!(
        text.contains("![pasted image](attachments/pasted-"),
        "Ctrl+V must insert the attachment markdown into the pane, got {text:?}"
    );
    let name = text
        .split_once("](")
        .and_then(|(_, t)| t.split_once(')'))
        .map(|(path, _)| path.to_string())
        .expect("a link target");
    assert!(
        v.path.join(&name).exists(),
        "the link names a PNG that is really there: {name}"
    );
}

/// A clipboard carrying TEXT pastes the text: the image branch must not also
/// fire on the key release and drop an unwanted attachment into the pane.
#[test]
fn a_text_paste_in_a_grid_pane_is_not_hijacked_by_the_image_branch() {
    let v = vault();
    let mut app = grid_vault_app(&v.path);
    app.tabs[0].text = "before\n".repeat(80);

    let p = Probe::new();
    p.settle(&mut app);
    focus_the_pane(&p, &mut app);

    crate::app::note_capture::test_hooks::set_next_image(solid_image(2, 2));
    // The real gesture shape: `Event::Paste` on the key-down frame, the `V`
    // release a frame later.
    p.frame(&mut app, CMD, vec![egui::Event::Paste("hello".into())]);
    p.frame(
        &mut app,
        CMD,
        vec![egui::Event::Key {
            key: egui::Key::V,
            physical_key: None,
            pressed: false,
            repeat: false,
            modifiers: CMD,
        }],
    );
    p.idle(&mut app);
    p.idle(&mut app);

    assert!(
        !v.path.join("attachments").exists(),
        "a text paste must not write an image attachment"
    );
    assert!(
        !app.tabs[0].text.contains("![pasted image]"),
        "…nor insert attachment markdown: {:?}",
        app.tabs[0].text
    );
}

/// Ctrl+SHIFT+V is a different binding (markdown preview) — the image branch
/// must not claim its release in a grid pane either.
#[test]
fn ctrl_shift_v_does_not_paste_an_image_in_a_grid_pane() {
    let v = vault();
    let mut app = grid_vault_app(&v.path);
    app.tabs[0].text = "before\n".repeat(80);

    let p = Probe::new();
    p.settle(&mut app);
    focus_the_pane(&p, &mut app);

    let shift_cmd = CMD | egui::Modifiers::SHIFT;
    crate::app::note_capture::test_hooks::set_next_image(solid_image(2, 2));
    p.frame(
        &mut app,
        shift_cmd,
        vec![egui::Event::Key {
            key: egui::Key::V,
            physical_key: None,
            pressed: false,
            repeat: false,
            modifiers: shift_cmd,
        }],
    );
    p.idle(&mut app);
    p.idle(&mut app);

    assert!(
        !v.path.join("attachments").exists(),
        "Ctrl+Shift+V must not paste an image attachment"
    );
}

/// D — the markdown chords are intercepted in the same single-pane block as the
/// paste hook, so Ctrl+B did nothing in split view.
///
/// The grid already drained `pending_wrap_marker`; only the interception was
/// missing, which is exactly the shape of bug a "the latch was set" assertion
/// cannot see. Asserted on the BUFFER.
#[test]
fn ctrl_b_wraps_the_selection_in_a_grid_pane() {
    let v = vault();
    let mut app = grid_vault_app(&v.path);
    app.tabs[0].text = "hello world\n".repeat(80);

    let p = Probe::new();
    p.settle(&mut app);
    focus_the_pane(&p, &mut app);

    // Select `hello` through the pane's own (document-scoped) editor state.
    let doc = app.tabs[0].doc_id;
    let id = grid_methods::pane_editor_id(doc);
    let mut st = egui::TextEdit::load_state(&p.ctx, id).unwrap_or_default();
    st.cursor.set_char_range(Some(egui::text::CCursorRange::two(
        egui::text::CCursor::new(0),
        egui::text::CCursor::new(5),
    )));
    st.store(&p.ctx, id);

    p.frame(
        &mut app,
        CMD,
        vec![egui::Event::Key {
            key: egui::Key::B,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: CMD,
        }],
    );
    p.idle(&mut app);

    assert!(
        app.tabs[0].text.starts_with("**hello**"),
        "Ctrl+B must bold the pane's selection, got {:?}",
        &app.tabs[0].text[..app.tabs[0].text.len().min(40)]
    );
}
