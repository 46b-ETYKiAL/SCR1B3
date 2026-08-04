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
