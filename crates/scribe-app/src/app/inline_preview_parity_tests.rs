//! ARM PARITY for the inline (hybrid) markdown preview.
//!
//! `md_preview::inline`'s own tests pin the two pure halves — source → spans, and
//! spans → layout-section formats. They cannot tell you whether the styling
//! actually REACHES the surface a user is typing into, on which of the editor's
//! seven arms. That is what this file is for, and it is the question this
//! codebase gets wrong most expensively: a capability present on one arm and
//! silently absent from four others.
//!
//! ## The arms, and which ones this feature covers
//!
//! The central editor surface forks on `grid_tree.is_some()`, and each side picks
//! a widget (see `grid_parity_tests` and the seven-arm matrix). The dividing line
//! for THIS feature is not the pane fork — it is which WIDGET lays the text out:
//!
//! | Arm | Widget | Inline markdown |
//! |---|---|---|
//! | single-pane `TextEdit` | egui `TextEdit` + `make_layouter` | **yes** |
//! | grid-pane `TextEdit`   | egui `TextEdit` + `make_layouter` | **yes** |
//! | single-pane fold view  | egui `TextEdit` + `make_layouter` | **yes** |
//! | single-pane owned rope | `scribe_render::RopeEditor`       | no |
//! | grid-pane owned rope   | `scribe_render::RopeEditor`       | no |
//! | single-pane browse (read-only large) | `scribe_render::RopeEditor` | no |
//! | grid-pane browse (read-only large)   | `scribe_render::RopeEditor` | no |
//!
//! `make_layouter` has exactly three call sites, one per covered arm, and the
//! markdown test lives INSIDE it (keyed off the same `ext` the highlighter uses) —
//! so no covered arm can opt itself out, and a fourth `TextEdit` arm would inherit
//! the feature by construction rather than by remembering. That is the whole
//! reason the wire was threaded into the layouter instead of applied at the call
//! sites.
//!
//! The `RopeEditor` arms are a different crate with its own span pipeline
//! (`HlSpan`, not `LayoutJob`), and they are the DEGRADED large-buffer surfaces
//! that already announce themselves with a `[ ROPE ]` / `[ READ-ONLY ]` badge. The
//! absence is pinned, not assumed.
//!
//! **Stated coverage boundary.** Two of the four rope-family arms — the read-only
//! BROWSE arms — are not independently constructed here. They are selected by
//! `doc.is_read_only_large()`, which `Document::open` sets only on a ≥256 MiB
//! file; reaching it in a test needs a `Document` seam that this branch does not
//! have and that the in-flight `test/capability-parity-matrix` branch is already
//! adding. Adding a second one would be exactly the parallel copy this repo keeps
//! paying for. They render through the SAME `scribe_render::RopeEditor` the two
//! owned-rope arms below prove unstyled, so the argument covers them; the PIN does
//! not, and this paragraph is here so nobody reads the table above as more
//! measured than it is.
//!
//! ## Falsification ledger
//!
//! Every assertion in this file and in `md_preview::inline::tests` was OBSERVED
//! RED once, by cutting the product wire it measures, before being committed.
//!
//! Cuts (each reverted, the file hash verified back to its original):
//!
//! * `restyle_job`'s call in `make_layouter` removed → the three presence cells
//!   go red and the two absence cells stay green (which is what proves the
//!   absence cells are not passing for the same reason).
//! * `is_markdown_ext` forced `false` → the three presence cells go red, so they
//!   measure the markdown gate and not merely "some section differs".
//! * `is_markdown_ext` forced `true` → `a_non_markdown_note_is_never_restyled`
//!   goes red.
//! * the heading branch of `inline::apply` stripped of its `font_id.size` scale →
//!   every presence cell goes red, since the size differential is the observable.
//! * the inline-palette terms removed from the layouter's cache key →
//!   `toggling_the_setting_takes_effect_on_the_same_text` goes red (the stale
//!   cached job is served).
//! * `spans` computed from `self.tabs[..].text` instead of the laid-out string in
//!   the fold arm (the projected-offset hazard, hand-applied) →
//!   `the_fold_view_styles_the_projection_it_actually_laid_out` goes red.
//!
//! Grafts (each reverted), one per absence cell — the rehearsal of the state in
//! which the pin MUST fail, so a pin that could never go red is not shipped:
//!
//! * `experimental_rope_editor` forced off in the single-pane rope fixture, so the
//!   same document falls through to the `TextEdit` arm →
//!   `the_owned_rope_arm_is_not_inline_styled` goes red.
//! * the same graft on the grid rope fixture → its cell goes red.
//!
//! ## Discipline
//!
//! No cell supplies its own wire. The state each test presets is strictly
//! UPSTREAM of what it measures: a real `.md` file on disk (so `language_hint`
//! resolves markdown the production way), the document text, the config flag, and
//! `fold_view`. Nothing writes a span, a `LayoutJob`, or a section and then
//! asserts one was read.
//!
//! Scope: these pin the layout SECTIONS the arm actually painted — presence,
//! extent and relative size — never appearance. Pixels belong to the GPU-gated
//! `visual_qa` harness.
#![allow(clippy::wildcard_imports)]
use super::grid_parity_tests::{walk_shape, Probe};
use super::*;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// The heading whose SIZE is the observable, and a body line to measure it
/// against. Both strings are unique in the frame, so locating them in a galley
/// cannot collide with chrome text.
const HEADING_WORD: &str = "AlphaHeading";
const BODY_WORD: &str = "plainbodyline";

/// The fixture note.
///
/// Two heading sections, because the fold arm needs a real region to collapse —
/// a fold view with nothing folded projects the document unchanged, and would
/// pass the projection test for the wrong reason.
fn note_text() -> String {
    format!(
        "# {HEADING_WORD}\n\
         first body of the alpha section\n\
         second body of the alpha section\n\
         # BetaHeading\n\
         {BODY_WORD} carries no markdown at all\n"
    )
}

/// A base config with the grid off, the rope swap fully manual, and the update
/// check + spellcheck disabled so nothing else paints into the frame.
///
/// `rope_editor_auto_threshold_bytes = 0` disables the AUTOMATIC swap entirely, so
/// each fixture opts into its widget EXPLICITLY and cannot drift onto a
/// neighbouring arm when the fixture text changes length.
fn base_config() -> Config {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    cfg.appearance.frameless = false;
    cfg.editor.grid_enabled = false;
    cfg.editor.rope_editor_auto_threshold_bytes = 0;
    cfg.editor.experimental_rope_editor = false;
    cfg.editor.inline_markdown_preview = true;
    cfg.spellcheck.enabled = false;
    // The minimap paints a SECOND galley of the same document at ~3pt with a
    // single uniform section. Left on, it would break the uniqueness precondition
    // in `editor_galley` and — far worse — an absence assertion that happened to
    // pick it up would pass vacuously, because a one-section galley is uniform by
    // construction whatever the editor did.
    cfg.editor.show_minimap = false;
    cfg
}

/// A live temp directory plus the app whose single tab is a REAL `.md` file
/// inside it.
///
/// The file is opened through `EditorTab::from_path` → `Document::open`, the same
/// path production uses, because the feature keys off `doc.language_hint()` — an
/// extension the fixture faked would prove nothing about how a real note resolves.
/// The `TempDir` is returned so it outlives the app.
fn app_with_note(cfg: Config, name: &str, text: &str) -> (tempfile::TempDir, ScribeApp) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(name);
    std::fs::write(&path, text).expect("write note");
    let mut app = ScribeApp::new_test(cfg);
    app.tabs[0] = EditorTab::from_path(path).expect("open note");
    (dir, app)
}

/// Settle the app and hand back the frame it painted.
///
/// Three frames: `sync_grid_state` allocates doc ids on the first, the panes lay
/// out on the second, so nothing is measurable before the third.
fn settled(app: &mut ScribeApp) -> egui::FullOutput {
    let p = Probe::new();
    p.settle(app);
    p.idle(app)
}

/// Every galley this frame painted that carries `needle`.
///
/// The editor's galley is found by its CONTENT rather than by position or index:
/// a frame contains chrome galleys too, and an index would silently start
/// measuring the toolbar the first time the layout changed.
fn galleys_with(out: &egui::FullOutput, needle: &str) -> Vec<std::sync::Arc<egui::Galley>> {
    let mut hits = Vec::new();
    for clipped in &out.shapes {
        walk_shape(&clipped.shape, &mut |s| {
            if let egui::Shape::Text(t) = s {
                if t.galley.text().contains(needle) {
                    hits.push(t.galley.clone());
                }
            }
        });
    }
    hits
}

/// The one galley carrying the document body, asserted unique.
///
/// Uniqueness is the precondition, not a nicety: if the note text appeared in two
/// galleys (a pane plus a preview, say) a size differential could be read off two
/// different widgets and mean nothing.
fn editor_galley(out: &egui::FullOutput, needle: &str) -> std::sync::Arc<egui::Galley> {
    let mut hits = galleys_with(out, needle);
    assert_eq!(
        hits.len(),
        1,
        "expected exactly one painted galley carrying {needle:?}, found {}",
        hits.len()
    );
    hits.remove(0)
}

/// The font size of the layout section covering the first occurrence of `needle`
/// in `galley`, or `None` when the text is not there.
fn size_of(galley: &egui::Galley, needle: &str) -> Option<f32> {
    let at = galley.job.text.find(needle)?;
    galley
        .job
        .sections
        .iter()
        .find(|s| s.byte_range.start <= at && at < s.byte_range.end)
        .map(|s| s.format.font_id.size)
}

/// Assert the arm rendered its heading VISIBLY larger than its body text.
///
/// The differential is deliberately relative: it needs no knowledge of the
/// configured font size, and it cannot pass on a frame where every section grew
/// (a font-size change) or where nothing rendered at all (both lookups fail).
fn assert_inline_styled(out: &egui::FullOutput, arm: &str) {
    let g = editor_galley(out, BODY_WORD);
    let head = size_of(&g, HEADING_WORD)
        .unwrap_or_else(|| panic!("{arm}: the heading text was never laid out"));
    let body = size_of(&g, BODY_WORD)
        .unwrap_or_else(|| panic!("{arm}: the body text was never laid out"));
    assert!(
        head > body,
        "{arm}: the heading must render larger than the body — got {head} vs {body}, \
         i.e. the inline preview never reached this arm"
    );
}

/// Assert NO section of the arm's editor galley was heading-scaled.
///
/// Stated as "every section is the same size" rather than "the heading is not
/// larger", so it also fails if the styling landed on the WRONG bytes here.
fn assert_not_inline_styled(out: &egui::FullOutput, arm: &str) {
    let g = editor_galley(out, BODY_WORD);
    let sizes: Vec<f32> = g
        .job
        .sections
        .iter()
        .map(|s| s.format.font_id.size)
        .collect();
    let first = *sizes.first().unwrap_or(&0.0);
    assert!(
        sizes.iter().all(|s| (s - first).abs() < 1e-3),
        "{arm}: this arm renders through `scribe_render::RopeEditor`, which has no \
         inline-markdown path — a size-varying galley means the feature leaked \
         onto it (or the fixture fell through to a TextEdit arm): {sizes:?}"
    );
}

// ---------------------------------------------------------------------------
// Present — the three `TextEdit`-family arms
// ---------------------------------------------------------------------------

#[test]
fn the_single_pane_text_edit_renders_markdown_in_place() {
    // The default arm — what almost every note is edited on.
    let (_dir, mut app) = app_with_note(base_config(), "note.md", &note_text());
    let out = settled(&mut app);
    assert!(
        app.grid_tree.is_none(),
        "fixture must be on the single-pane side of the `grid_tree.is_some()` fork"
    );
    assert_inline_styled(&out, "single-pane TextEdit");
}

#[test]
fn a_grid_pane_renders_markdown_in_place_too() {
    // Splitting the view must not silently downgrade the editor — the exact
    // regression `grid_parity_tests` was written for, one feature later.
    let mut cfg = base_config();
    cfg.editor.grid_enabled = true;
    let (_dir, mut app) = app_with_note(cfg, "note.md", &note_text());
    let out = settled(&mut app);
    assert!(
        app.grid_tree.is_some(),
        "fixture must be on the grid side of the `grid_tree.is_some()` fork"
    );
    assert_inline_styled(&out, "grid pane TextEdit");
}

#[test]
fn the_fold_view_renders_markdown_in_place() {
    let mut cfg = base_config();
    cfg.editor.grid_enabled = false;
    let (_dir, mut app) = app_with_note(cfg, "note.md", &note_text());
    app.fold_view = true;
    let out = settled(&mut app);
    assert!(app.fold_view, "fixture must be on the fold arm");
    assert_inline_styled(&out, "fold view");
}

#[test]
fn the_fold_view_styles_the_projection_it_actually_laid_out() {
    // THE hazard cell.
    //
    // The fold view lays out a PROJECTION whose byte offsets do not match the
    // document — which is why a span OVERLAY (diagnostics, whose ranges arrive
    // from the language server in DOCUMENT coordinates) must never be pointed at
    // it: it would underline the wrong text.
    //
    // Inline markdown is not an overlay. Its spans are derived from the very
    // string being laid out, so the direction of the data flow is reversed and the
    // mismatch cannot arise. This test is what makes that a measured claim: with a
    // section COLLAPSED, every byte after it sits at a different offset in the
    // projection than in the document, so styling computed from the document would
    // land on the wrong bytes and the second heading would not be scaled.
    let (_dir, mut app) = app_with_note(base_config(), "note.md", &note_text());
    app.fold_view = true;
    // Collapse the FIRST heading section — upstream state (a user's fold click),
    // not the wire under test.
    let regions = crate::editor_features::fold_regions_for(&app.tabs[0].text, Some("md"));
    let first = regions
        .first()
        .expect("the fixture must have a foldable heading section");
    app.folds.insert(first.start_line);

    let out = settled(&mut app);
    let g = editor_galley(&out, BODY_WORD);
    // Precondition: the projection really did elide something. Without this the
    // test would pass vacuously the day folding stops working.
    assert!(
        g.job.text.len() < app.tabs[0].text.len(),
        "the fold must actually collapse lines, else the projection equals the \
         document and this test proves nothing: projected {} vs document {}",
        g.job.text.len(),
        app.tabs[0].text.len()
    );
    // Precondition: the offsets genuinely disagree, so document-derived spans
    // would be wrong rather than accidentally right.
    let doc_at = app.tabs[0].text.find("BetaHeading").expect("in document");
    let proj_at = g.job.text.find("BetaHeading").expect("in projection");
    assert_ne!(
        doc_at, proj_at,
        "the fold must MOVE the second heading, else document- and \
         projection-derived spans would coincide"
    );
    // And the styling landed on the projection's own heading.
    let head = size_of(&g, "BetaHeading").expect("the second heading was laid out");
    let body = size_of(&g, BODY_WORD).expect("the body was laid out");
    assert!(
        head > body,
        "the second heading must be scaled in the PROJECTION — got {head} vs \
         {body}, i.e. the spans were computed against document offsets and landed \
         on the wrong bytes"
    );
}

// ---------------------------------------------------------------------------
// Expected-absent — the owned-rope arms
// ---------------------------------------------------------------------------

#[test]
fn the_owned_rope_arm_is_not_inline_styled() {
    // EXPECTED-ABSENT PIN. `scribe_render::RopeEditor` builds its own spans from
    // `HlSpan` and never sees a `LayoutJob`, so the feature does not reach it.
    // Grafted red by flipping `experimental_rope_editor` off, which drops the same
    // document onto the `TextEdit` arm.
    let mut cfg = base_config();
    cfg.editor.experimental_rope_editor = true;
    let (_dir, mut app) = app_with_note(cfg, "note.md", &note_text());
    let out = settled(&mut app);
    assert!(
        app.tabs[app.active].rope_state.is_some(),
        "precondition: `rope_state` is created by the owned-rope arm and by nothing \
         else, so its absence means this fixture never reached that arm"
    );
    assert_not_inline_styled(&out, "single-pane owned rope");
}

#[test]
fn a_grid_rope_pane_is_not_inline_styled_either() {
    let mut cfg = base_config();
    cfg.editor.grid_enabled = true;
    cfg.editor.experimental_rope_editor = true;
    let (_dir, mut app) = app_with_note(cfg, "note.md", &note_text());
    let out = settled(&mut app);
    assert!(
        app.grid_tree.is_some(),
        "precondition: fixture must be on the grid side of the fork"
    );
    assert!(
        app.tabs[app.active].rope_state.is_some(),
        "precondition: the grid pane must have rendered the owned-rope arm"
    );
    assert_not_inline_styled(&out, "grid pane owned rope");
}

// ---------------------------------------------------------------------------
// The gates: file type, and the setting
// ---------------------------------------------------------------------------

#[test]
fn a_non_markdown_note_is_never_restyled() {
    // `# heading` is a COMMENT in a shell script and a directive in a dozen other
    // languages. Styling it as a heading would be actively wrong, so the gate is
    // the file's extension, decided once inside `make_layouter`.
    let (_dir, mut app) = app_with_note(base_config(), "note.txt", &note_text());
    let out = settled(&mut app);
    assert_eq!(
        app.tabs[0].doc.language_hint().as_deref(),
        Some("txt"),
        "precondition: the fixture must resolve as a non-markdown file"
    );
    assert_not_inline_styled(&out, "a .txt note");
}

#[test]
fn turning_the_setting_off_removes_the_styling() {
    let mut cfg = base_config();
    cfg.editor.inline_markdown_preview = false;
    let (_dir, mut app) = app_with_note(cfg, "note.md", &note_text());
    let out = settled(&mut app);
    assert_not_inline_styled(&out, "inline preview disabled");
}

#[test]
fn toggling_the_setting_takes_effect_on_the_same_text() {
    // The cache cell. `make_layouter` memoises the whole `LayoutJob` on a content
    // hash; the inline palette is an INPUT to that job, so leaving it out of the
    // key serves the previously-styled job forever on unchanged text. The user
    // would flip the switch in Settings, watch nothing happen, and have no way to
    // clear it short of an edit.
    //
    // One app, one context, text never touched — only the setting moves.
    let (_dir, mut app) = app_with_note(base_config(), "note.md", &note_text());
    let p = Probe::new();
    p.settle(&mut app);
    let on = p.idle(&mut app);
    assert_inline_styled(&on, "setting on");

    app.config.editor.inline_markdown_preview = false;
    let off = p.idle(&mut app);
    assert_not_inline_styled(&off, "setting toggled off, same text");

    app.config.editor.inline_markdown_preview = true;
    let back = p.idle(&mut app);
    assert_inline_styled(&back, "setting toggled back on, same text");
}

#[test]
fn the_styling_never_changes_the_text_the_surface_laid_out() {
    // The feature's load-bearing promise, asserted where it matters: on the real
    // arm, against the real buffer. The caret, selection, find/replace and the
    // diagnostics painter all address this string by byte offset, so a single byte
    // of drift would silently mis-target every one of them.
    let text = note_text();
    let (_dir, mut app) = app_with_note(base_config(), "note.md", &text);
    let out = settled(&mut app);
    let g = editor_galley(&out, BODY_WORD);
    assert_eq!(
        g.job.text.trim_end(),
        text.trim_end(),
        "the laid-out string must be the document, byte for byte"
    );
    // ...and the sections must still TILE it: a gap would drop glyphs, an overlap
    // would double-draw them.
    let mut at = 0usize;
    for s in &g.job.sections {
        assert_eq!(s.byte_range.start, at, "section gap/overlap at byte {at}");
        at = s.byte_range.end;
    }
    assert_eq!(at, g.job.text.len(), "sections must cover the whole text");
}

