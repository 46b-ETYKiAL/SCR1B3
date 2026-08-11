//! The SEVEN-ARM editor-surface capability PARITY MATRIX.
//!
//! `grid_parity_tests` pins the grid `TextEdit` pane against the single-pane
//! `TextEdit` — one arm against one arm. The central editor surface actually
//! forks into **seven** arms, and a capability present on one is routinely
//! absent from four others with no user-visible signal that it went away.
//!
//! The fork is a single `if self.grid_tree.is_some()` (`frame_tick.rs:2675`).
//! Below it, four single-pane arms are selected once per FRAME; above it, three
//! grid arms are selected once per PANE, so one grid frame can render all three
//! simultaneously and every `is_active`-gated capability resolves against
//! exactly one of them. Every cell here is therefore stated as "the arm the
//! ACTIVE pane rendered", never "the arm".
//!
//! | Code | Arm | Selected by |
//! |---|---|---|
//! | `SFold` | single-pane folded read-only preview | `self.fold_view` |
//! | `SRo`   | single-pane read-only huge-file browse | `doc.is_read_only_large()` |
//! | `SRope` | single-pane owned rope editor | `use_rope_editor(..)` |
//! | `STe`   | single-pane egui `TextEdit` | fall-through |
//! | `GRo`   | grid pane, read-only browse | `doc.is_read_only_large()` |
//! | `GRope` | grid pane, owned rope editor | `use_rope_editor(..)` |
//! | `GTe`   | grid pane, egui `TextEdit` | fall-through |
//!
//! Two rules every test here obeys, both learned the hard way:
//!
//! 1. **No cell supplies its own wire.** The state a test presets is strictly
//!    UPSTREAM of the wire under test — `app.diagnostics` (the language
//!    server's output) and `app.tabs[i].text` (the document) are legal;
//!    writing `line_gutter` or `scroll_metrics` and then asserting the surface
//!    reads them is not, because those ARE the outputs under test. The only
//!    writes to an output field here are POISON sentinels, whose whole purpose
//!    is to be observed GONE.
//! 2. **Absence is pinned as loudly as presence.** A matrix that omits a gap is
//!    not a ratchet. Every known-absent cell is an explicit expected-absent
//!    pin, written so that GRAFTING the missing call turns it red — the pin is
//!    a rehearsal of the exact edit that will one day flip it to enforcement.
//!    A pin that cannot be grafted red would silently survive the fix it exists
//!    to detect.
//!
//! Scope, stated so no row is over-read: these pin the RENDERING wire, not the
//! language server (every diagnostics fixture injects `app.diagnostics`
//! directly), and the shape LIST, not the composited frame — presence and
//! position, never appearance. Pixels belong to the GPU-gated `visual_qa`
//! harness.
#![allow(clippy::wildcard_imports)]
use super::frame_tick::EditorMode;
use super::grid_parity_tests::{
    diag, error_color, grid_config, painted_text, squiggle_segments, Probe,
};
use super::*;

// ---------------------------------------------------------------------------
// The arm enumeration
// ---------------------------------------------------------------------------

/// One of the seven arms the central editor surface can render through.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Arm {
    SFold,
    SRo,
    SRope,
    STe,
    GRo,
    GRope,
    GTe,
}

impl Arm {
    /// Every arm, so a row is written as a loop and cannot silently omit one.
    /// A capability row that covers six arms and forgets the seventh is the
    /// shape of hole this whole module exists to close.
    const ALL: [Arm; 7] = [
        Arm::SFold,
        Arm::SRo,
        Arm::SRope,
        Arm::STe,
        Arm::GRo,
        Arm::GRope,
        Arm::GTe,
    ];

    /// Whether this arm renders behind the `grid_tree.is_some()` fork.
    fn grid(self) -> bool {
        matches!(self, Arm::GRo | Arm::GRope | Arm::GTe)
    }

    /// The status-bar badge this arm's surface must publish — `None` for the
    /// full-feature `TextEdit` default, where NO badge is itself the "nothing
    /// is degraded" signal (`EditorMode::Standard.badge()`).
    fn badge(self) -> Option<&'static str> {
        match self {
            Arm::SFold => EditorMode::Fold.badge(),
            Arm::SRo | Arm::GRo => EditorMode::ReadOnlyLarge.badge(),
            Arm::SRope | Arm::GRope => EditorMode::Rope.badge(),
            Arm::STe | Arm::GTe => EditorMode::Standard.badge(),
        }
    }
}

/// Every badge string the status bar can render, used to prove a badge-less arm
/// publishes NO badge rather than merely not publishing the one expected.
const ALL_BADGES: [&str; 4] = ["[ ROPE ]", "[ MMAP ]", "[ READ-ONLY ]", "[ FOLDED ]"];

// ---------------------------------------------------------------------------
// One fixture buffer, chosen so a single text serves every row
// ---------------------------------------------------------------------------

/// The fixture buffer.
///
/// Four constraints, all load-bearing:
///
/// * lines 0..3 are long, so a diagnostic span on any of them is unambiguous
///   and a later-line / later-column differential is arithmetic, not opinion;
/// * line 0 opens a brace scope that closes at the end of the buffer, so the
///   FOLD arm has a real region to project (`▾ L1 (`) instead of an empty
///   toolbar — a fold surface with nothing to fold looks the same as no fold
///   surface at all;
/// * 200 filler lines, so the content genuinely overflows any pane (the scroll
///   metrics have a real range to measure) and a click anywhere in the body
///   lands on the widget rather than past the end of a short buffer — the
///   fixture-geometry trap `grid_parity_tests::diag_grid_text` documents;
/// * no filler line carries a diagnostic, so appending them cannot move the ink.
fn matrix_text() -> String {
    let mut s = String::from(
        "fn scope_opens_here() { // 0123456789abcdefghij\n\
         second line holds the error span ........\n\
         third line holds a warning token ........\n\
         fourth line holds the info notice .......\n",
    );
    for i in 0..200 {
        s.push_str(&format!(
            "    filler line {i:03}, no diagnostic published against it\n"
        ));
    }
    s.push_str("}\n");
    s
}

/// An app fixture built to render exactly `arm`.
///
/// `rope_editor_auto_threshold_bytes = 0` on every arm on purpose: it disables
/// the AUTOMATIC swap entirely, so each arm opts into its surface EXPLICITLY
/// and no fixture can drift onto a neighbouring arm when the fixture text
/// changes size. The rope arms select `show_editable` through
/// `experimental_rope_editor`, which reaches the identical code path the
/// threshold would (`use_rope_editor` ORs the two) without a 16 MiB buffer.
fn app_for(arm: Arm) -> ScribeApp {
    let mut cfg = grid_config();
    cfg.editor.grid_enabled = arm.grid();
    cfg.editor.rope_editor_auto_threshold_bytes = 0;
    cfg.editor.experimental_rope_editor = matches!(arm, Arm::SRope | Arm::GRope);
    // Spellcheck's squiggle uses the SAME `#e53e3e` as the default `error`
    // colour, so leaving it on would put error-coloured segments in every
    // CONTROL frame and hollow out every zero-ink assertion in this file.
    cfg.spellcheck.enabled = false;
    let mut app = ScribeApp::new_test(cfg);
    let text = matrix_text();
    app.tabs[0].text.clone_from(&text);
    if matches!(arm, Arm::SRo | Arm::GRo) {
        // The browse arms are selected by `doc.is_read_only_large()`, which
        // `Document::open` sets only on a >= 256 MiB file. The `test-hooks`
        // seam reaches that state without one.
        //
        // The fixture ALSO leaves `tab.text` filled, which production does NOT
        // for a browse doc (its bytes live only in the document rope). That is
        // deliberate: `diagnostic_spans_for_active` resolves against
        // `tab.text`, so an empty one would resolve ZERO spans and the
        // expected-absent diagnostics pin below would pass for a reason other
        // than the missing painter — i.e. it could never be grafted red, which
        // is the one property an absence pin has to have.
        app.tabs[0].doc = Document::read_only_large_for_test(None, &text);
    }
    if arm == Arm::SFold {
        app.fold_view = true;
    }
    app
}

/// Prove the fixture rendered the arm it claims to — AND that the arm published
/// its own mode badge (row R19, the one row that is green on all seven).
///
/// Every cell in this file calls this first. Without it each one could pass
/// vacuously: a fixture that quietly fell through to a neighbouring arm
/// produces a frame that looks exactly like a correct one for an absence pin,
/// and a frame with no pane at all looks exactly like a passing zero-ink
/// control. The badge is read from the text the status bar actually PAINTED,
/// not from the ctx-data slot it is sequenced through, so a mode that is
/// published but never rendered fails here.
fn assert_arm(app: &ScribeApp, out: &egui::FullOutput, arm: Arm) {
    assert_eq!(
        app.grid_tree.is_some(),
        arm.grid(),
        "{arm:?} must render on the {} side of the `grid_tree.is_some()` fork",
        if arm.grid() { "grid" } else { "single-pane" }
    );
    let painted = painted_text(out);
    match arm.badge() {
        Some(badge) => {
            let needle = format!("[ {badge} ]");
            assert!(
                painted.contains(&needle),
                "{arm:?} must publish the {needle} badge — the status bar painted \
                 no such text, so either the arm did not render or it published \
                 no mode at all"
            );
        }
        None => {
            for other in ALL_BADGES {
                assert!(
                    !painted.contains(other),
                    "{arm:?} is the full-feature default and must be BADGE-LESS \
                     (no badge IS the `nothing is degraded` signal); the status \
                     bar painted {other} — a badge left over from another arm is \
                     the stale-badge bug the publish exists to prevent"
                );
            }
        }
    }
    // …and a second, arm-specific witness, so the badge alone cannot carry the
    // precondition. A badge is ctx-data the PREVIOUS frame wrote; these are
    // states only this frame's arm could have produced.
    match arm {
        Arm::SRope | Arm::GRope => assert!(
            app.tabs[app.active].rope_state.is_some(),
            "{arm:?}: `rope_state` is created by the owned-rope arm and by \
             nothing else here, so its absence means the rope arm never ran"
        ),
        Arm::SRo | Arm::GRo => assert!(
            app.tabs[app.active].doc.is_read_only_large(),
            "{arm:?}: the browse arm is selected by this flag alone"
        ),
        Arm::SFold => assert!(
            painted.contains("▾ L1 ("),
            "SFold must paint the folded PROJECTION's region toggle — a fold \
             toolbar with no region to fold is indistinguishable from no fold \
             surface at all"
        ),
        Arm::STe | Arm::GTe => {}
    }
}

/// Settle `arm`'s fixture and hand back the frame it painted, arm proven.
fn settled(app: &mut ScribeApp, arm: Arm) -> (Probe, egui::FullOutput) {
    let p = Probe::new();
    // `sync_grid_state` allocates doc ids on the first frame, the panes lay out
    // on the second, and the status bar renders BEFORE the central panel — so
    // the badge is last frame's publish and nothing is measurable before the
    // third frame.
    p.settle(app);
    let out = p.idle(app);
    assert_arm(app, &out, arm);
    (p, out)
}

// ---------------------------------------------------------------------------
// R19 — editor-mode publish (the one row green on all seven arms)
// ---------------------------------------------------------------------------

/// Every arm must publish the surface it rendered, and a badge-less arm must
/// publish NO badge.
///
/// This is simultaneously the R19 row and the precondition every other cell in
/// this file leans on, which is why it is stated first and separately: if this
/// fails, every absence pin below is suspect.
#[test]
fn every_arm_publishes_the_editor_mode_it_rendered() {
    for arm in Arm::ALL {
        let mut app = app_for(arm);
        let (_p, _out) = settled(&mut app, arm);
    }
}

/// `EditorMode::RopeMmap` is a DEAD surface: no arm can reach it.
///
/// `BufferModeSeen::Mmap` is reported only when `Buffer::as_rope()` is `None`,
/// and every arm builds a `Buffer::from_text` or `Buffer::Rope(..)` — even the
/// browse arms, because `Document::open` decodes the mmap into a `Rope`
/// immediately and drops it. So the `MMAP` badge, its hover and its
/// entry-notice are unreachable from the app while their code claims otherwise.
///
/// Pinned as UNREACHABILITY rather than deleted: the day a real mmap-backed
/// buffer is wired through, this fails loudly and the mode stops being a lie.
#[test]
fn no_arm_can_reach_the_mmap_editor_mode() {
    for arm in Arm::ALL {
        let mut app = app_for(arm);
        let (_p, out) = settled(&mut app, arm);
        assert!(
            !painted_text(&out).contains("[ MMAP ]"),
            "{arm:?} published the MMAP mode, which no arm should be able to \
             reach — if a mmap-backed buffer was genuinely wired through, flip \
             this pin to enforcement rather than weakening it"
        );
    }
}

// ---------------------------------------------------------------------------
// R1 — inline diagnostics ink
// ---------------------------------------------------------------------------

const ERR: u8 = crate::app::diagnostics_overlay::SEVERITY_ERROR;

/// A published error on line 1, well inside that line's length.
fn err_on_line_1() -> Vec<Diagnostic> {
    vec![diag(
        1,
        5,
        1,
        20,
        ERR,
        "cannot find value `error` in this scope",
    )]
}

/// The error-coloured ink `arm` painted for `diags`, with the arm proven AND
/// the span resolution proven.
///
/// The span precondition is what makes the ABSENCE cells graftable: without it
/// a zero-ink result could mean "the resolver produced no spans", and grafting
/// a painter into the arm would not turn the pin red. With it, zero ink can
/// only mean the arm never painted the spans it was handed.
fn arm_ink(arm: Arm, diags: Vec<Diagnostic>) -> Vec<[egui::Pos2; 2]> {
    let mut app = app_for(arm);
    let expect_spans = !diags.is_empty();
    app.diagnostics = diags;
    let err = error_color(&app);
    let (_p, out) = settled(&mut app, arm);
    assert_eq!(
        !app.diagnostic_spans_for_active(app.active).is_empty(),
        expect_spans,
        "{arm:?}: the fixture must resolve the published diagnostics onto real \
         byte spans, otherwise a zero-ink verdict measures the RESOLVER rather \
         than the arm's painter"
    );
    squiggle_segments(&out, err)
}

/// Exactly four of the seven arms paint the inline diagnostic squiggle.
///
/// The three that do not are the read-only surfaces: `RopeEditor::show` on a
/// non-editable path lays out no per-row galley, so there is nothing to
/// position ink against, and the fold preview is an `interactive(false)`
/// projection whose rows are not the document's. Those three are pinned as
/// expected-absent — with the span precondition above, grafting a painter call
/// into any of them turns this red, which is exactly what should happen the day
/// one of them grows a row-geometry report.
///
/// The G-ROPE cell is the one that recently MOVED: it was absent while the Rope
/// entry-notice actively promised diagnostics still worked, and it is the arm a
/// buffer is auto-promoted into past `rope_editor_auto_threshold_bytes`.
#[test]
fn diagnostics_ink_is_painted_by_exactly_the_four_editable_arms() {
    for arm in [Arm::SRope, Arm::STe, Arm::GRope, Arm::GTe] {
        assert!(
            !arm_ink(arm, err_on_line_1()).is_empty(),
            "{arm:?} must paint a squiggle for a published error; the frame \
             carried no error-coloured line segments at all"
        );
        let control = arm_ink(arm, Vec::new());
        assert!(
            control.is_empty(),
            "{arm:?}: an otherwise identical app with NO diagnostics must paint \
             no error-coloured ink — {} segments found, so the assertion above \
             is not measuring the diagnostics",
            control.len()
        );
    }

    for arm in [Arm::SFold, Arm::SRo, Arm::GRo] {
        let ink = arm_ink(arm, err_on_line_1());
        assert!(
            ink.is_empty(),
            "{arm:?} is pinned expected-absent for diagnostics ink (no per-row \
             galley to position against). It painted {} error-coloured \
             segments. If a row-geometry report was genuinely added, flip this \
             pin to enforcement — do not weaken it",
            ink.len()
        );
    }
}

// ---------------------------------------------------------------------------
// R15 — scroll-metrics publish (poisoned)
// ---------------------------------------------------------------------------

/// A value `scroll_metrics` can never legitimately hold, written immediately
/// before the frame under test.
///
/// Poisoning is mandatory, not defensive. `scroll_metrics` persists across
/// frames, so "the metrics look plausible" passes on whatever the PREVIOUS
/// frame left there — and the seeded `(0.0, 1.0, 1.0)` default is plausible
/// enough to fool a shape check. An arm that publishes nothing is
/// indistinguishable from one that publishes correctly unless the field is
/// known-bad going in.
const POISON: (f32, f32, f32) = (-1.0, -1.0, -1.0);

/// Six of the seven arms publish the measurement the minimap's viewport
/// indicator and both scroll assists read; the fold preview deliberately does
/// not.
///
/// The fold cell is BY-DESIGN, not a gap: the projection's content height is
/// not observable from outside the widget, and `frame_tick` states that
/// publishing a guessed height "would make the minimap lie". The pin exists so
/// that a future well-meant guessed publish fails loudly — grafting a
/// `finish_embedded_scroll` with an estimated height into the fold arm is
/// precisely the change the comment forbids, and it turns this red.
#[test]
fn every_arm_but_the_fold_preview_publishes_scroll_metrics() {
    for arm in Arm::ALL {
        let mut app = app_for(arm);
        let p = Probe::new();
        p.settle(&mut app);
        app.scroll_metrics = POISON;
        let out = p.idle(&mut app);
        assert_arm(&app, &out, arm);

        let (off, content_h, view_h) = app.scroll_metrics;
        if arm == Arm::SFold {
            assert_eq!(
                app.scroll_metrics, POISON,
                "SFold must publish NO scroll metrics — a guessed content height \
                 makes the minimap lie, which is worse than leaving it alone. \
                 It wrote {:?}",
                app.scroll_metrics
            );
            continue;
        }
        assert_ne!(
            app.scroll_metrics, POISON,
            "{arm:?} must publish scroll metrics; the poison sentinel survived \
             the frame, so nothing wrote them"
        );
        assert!(
            view_h > 100.0 && view_h < 760.0,
            "{arm:?}: the view height must be the SURFACE's height, got {view_h}"
        );
        assert!(
            content_h > view_h * 2.0,
            "{arm:?}: a 205-line buffer must measure as content far taller than \
             one viewport (content {content_h}, view {view_h})"
        );
        assert_eq!(off, 0.0, "{arm:?}: an untouched surface sits at the top");
    }
}

// ---------------------------------------------------------------------------
// R4 — the external gutter's row feed (poisoned)
// ---------------------------------------------------------------------------

/// `line_gutter` is fed by the `TextEdit` arms alone — and the rope/browse arms
/// must actively CLEAR it rather than leave it stale.
///
/// This is the matrix's stale-VALUE cell, and the reason every reader of a
/// persisted field here poisons first. `line_gutter` is not only the external
/// gutter's row source: `find_nav` prefers it for go-to-line, find-navigate and
/// bookmark jumps. An arm that neither writes nor clears it therefore does not
/// merely lose its gutter marks — it sends every jump to the PREVIOUS buffer's
/// Y, which is worse than missing and looks identical to working from the
/// outside.
///
/// The pin is therefore THREE-sided, and the third side is a live defect this
/// matrix found on its first run:
///
/// * the two `TextEdit` arms must REPLACE the poison with this frame's rows;
/// * the four rope / browse arms must CLEAR it — that clear is the recently
///   landed half of the fix, and it is what makes their absence honest rather
///   than stale;
/// * **the fold arm does NEITHER.** The clear predicate is
///   `(rope_arm || read_only)`; `fold_view` is not in it, while the PANEL's own
///   gate at the next line is `!fold_view`. So folding hides the gutter and
///   leaves its feed pointing at the pre-fold rows — and the projection has
///   FOLDED lines, so those row indices do not even correspond to document
///   lines any more. A go-to-line or find-navigate while folded reads
///   `line_gutter[line0]` and jumps to a Y that means nothing.
///
/// That last cell is pinned as the CURRENT (wrong) behaviour, not asserted
/// away: the poison is expected to SURVIVE. Adding `fold_view` to the clear
/// predicate — the one-token fix, exactly parallel to the rope/browse half —
/// turns this red, which is the whole point of pinning it.
#[test]
fn the_external_gutter_feed_is_written_by_textedit_arms_and_cleared_by_all_but_fold() {
    for arm in Arm::ALL {
        let mut app = app_for(arm);
        let p = Probe::new();
        p.settle(&mut app);
        // A row list no real frame can produce: a single NEGATIVE Y, for a
        // 205-line buffer. Poisoning is mandatory — `line_gutter` persists
        // across frames, so "it looks populated" passes on whatever the
        // previous frame left there.
        app.line_gutter = vec![-1.0];
        let out = p.idle(&mut app);
        assert_arm(&app, &out, arm);
        let left = app.line_gutter.clone();

        match arm {
            Arm::STe | Arm::GTe => assert!(
                left.len() > 1 && left.iter().all(|y| *y >= 0.0),
                "{arm:?} must feed the external gutter this frame's rows; it \
                 left {left:?} — a single negative Y IS the poison sentinel, so \
                 nothing wrote the feed"
            ),
            Arm::SRope | Arm::GRope | Arm::SRo | Arm::GRo => assert!(
                left.is_empty(),
                "{arm:?} lays out no app-drawn gutter, so it must CLEAR the feed \
                 rather than leave the previous surface's rows behind — \
                 go-to-line and find-navigate read `line_gutter[line0]` and \
                 would jump to the old buffer's Y. It left {left:?}"
            ),
            Arm::SFold => assert_eq!(
                left,
                vec![-1.0],
                "EXPECTED-ABSENT (live gap): the fold arm neither writes nor \
                 clears the gutter feed — the clear predicate is \
                 `(rope_arm || read_only)` and `fold_view` is not in it, while \
                 the panel's own gate on the next line IS `!fold_view`. The \
                 poison must therefore SURVIVE the frame. It did not, which \
                 means the clear was extended to cover folding: that is the \
                 fix — flip this pin to `left.is_empty()` and move it up into \
                 the arm above. Do not weaken it"
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// R21 — fold is STRUCTURALLY unreachable behind the grid fork
// ---------------------------------------------------------------------------

/// Turning the grid on does not merely leave the fold preview without a pane —
/// it silently IGNORES an already-true `fold_view`.
///
/// `if self.fold_view` sits inside the `else` of the `grid_tree.is_some()`
/// fork, so with the grid on the flag is never read: no folded projection, and
/// no `FOLDED` badge either, because that publish is inside the same `else`.
/// The toolbar's Folded toggle therefore becomes a silent no-op with no signal
/// of any kind.
///
/// Pinned as the current SILENCE so the fix breaks it loudly, whichever fix
/// lands: hoisting the `fold_view` check above the fork makes the projection
/// appear, and giving the toggle a disabled/explained state makes the badge or
/// a toast appear. Either way this test has to be revisited, which is the point.
#[test]
fn turning_the_grid_on_silently_ignores_an_already_folded_view() {
    let mut app = app_for(Arm::GTe);
    app.fold_view = true;
    let p = Probe::new();
    p.settle(&mut app);
    let out = p.idle(&mut app);
    let painted = painted_text(&out);

    assert!(
        app.fold_view,
        "precondition: the fold flag is set and nothing cleared it — the point \
         is that it is IGNORED, not reset"
    );
    assert!(
        app.grid_tree.is_some(),
        "precondition: the grid owns the central surface"
    );
    assert!(
        !painted.contains("▾ L1 (") && !painted.contains("expand all"),
        "expected-absent: with the grid on, a true `fold_view` renders no fold \
         surface at all. A fold toolbar appeared, so the check was hoisted \
         above the fork — flip this pin to enforcement"
    );
    assert!(
        !painted.contains("[ FOLDED ]"),
        "expected-absent: …and no FOLDED badge either, so the user gets no \
         signal that the toggle did nothing. A badge appeared, so the silence \
         was fixed — flip this pin"
    );
}

// ---------------------------------------------------------------------------
// R22 — focus->active sync reaches only the `TextEdit` pane
// ---------------------------------------------------------------------------

/// A two-pane grid whose panes both render through `arm`, with the SECOND tab
/// unpinned so its pane owns the only "Pin note" glyph.
fn two_pane_grid(arm: Arm) -> ScribeApp {
    assert!(arm.grid(), "two_pane_grid is for the grid arms only");
    let mut app = app_for(arm);
    let text = matrix_text();
    app.tabs.push(EditorTab::scratch());
    app.tabs[1].text.clone_from(&text);
    if arm == Arm::GRo {
        app.tabs[1].doc = Document::read_only_large_for_test(None, &text);
    }
    app.active = 0;
    app
}

/// Clicking a pane makes it active — but ONLY if it is a `TextEdit` pane.
///
/// The focus->active sync matches on `pane_editor_id`, which is handed to the
/// `TextEdit` widget alone; the rope and browse panes register their focus
/// under `drag_scroll::rope_editor_focus_id` instead. So a rope pane can never
/// become active by being clicked, and every `active`-keyed surface — the
/// status-bar counters, encoding, language, EOL, the caret readout, and the
/// diagnostics span resolution — keeps describing a pane the user has left.
/// That is precisely the class of bug the G-TE sync was landed to fix; two of
/// the three grid arms never received it.
///
/// The G-TE half is the ENFORCE pin and the G-ROPE half the expected-absent
/// one, in one test on purpose: they share a fixture and a gesture, so the
/// pair proves the click itself lands. Extending the sync to
/// `rope_editor_focus_id` turns the second half red.
#[test]
fn only_a_textedit_grid_pane_can_be_clicked_into_activity() {
    for (arm, expect_active) in [(Arm::GTe, 1usize), (Arm::GRope, 0usize)] {
        let mut app = two_pane_grid(arm);
        let p = Probe::new();
        p.settle(&mut app);
        assert_eq!(app.active, 0, "{arm:?} precondition: pane 0 starts active");

        let other = app.tabs[1].doc_id;
        let rect =
            super::grid_parity_tests::pane_rect(&app, other).expect("the second pane laid out");
        // Below the pane's header chip, so the click lands on the editor body
        // rather than on the chip's controls.
        let body = egui::pos2(rect.center().x, rect.top() + rect.height() * 0.6);
        p.mod_click(&mut app, body, egui::Modifiers::NONE);
        p.idle(&mut app);

        assert_eq!(
            app.active,
            expect_active,
            "{arm:?}: clicking pane 1's body at {body:?} (pane rect {rect:?}) \
             must {} make it active. GTe is the enforce pin; GRope is pinned \
             expected-absent because the sync matches only `pane_editor_id` — \
             if the sync was extended to `rope_editor_focus_id`, flip that pin",
            if expect_active == 1 { "" } else { "NOT" }
        );
    }
}

// ---------------------------------------------------------------------------
// R26-R33 — the `TextEdit`-only convenience block, and the badge that lies
// ---------------------------------------------------------------------------

/// The grid's `TextEdit` pane is missing a block of single-pane conveniences —
/// and reports itself as fully-featured while doing so.
///
/// Unlike the rope arms, there is NO user-visible signal here: the grid
/// `TextEdit` pane publishes `EditorMode::Standard`, which by the badge's own
/// contract means "nothing is degraded". That claim is false for this pane
/// today, and the two halves of this test are deliberately coupled so the day
/// any one convenience is ported, BOTH have to be revisited together — the
/// absence pin fails, and the reviewer is forced to look at the badge claim in
/// the same edit rather than porting a feature and leaving the signal wrong.
#[test]
fn the_grid_textedit_pane_is_missing_the_single_pane_conveniences_and_says_it_is_not() {
    // Auto-focus-on-launch is the witness, chosen because it is decidable with
    // no config flag, no language detection and no clipboard: the single-pane
    // arm calls `request_focus()` on the editor so typing works immediately on
    // launch — "no click required" — and the grid pane never does. Read from
    // `Memory::focused()` after settling with NO pointer event of any kind, so
    // a focus that arrived from a click cannot be mistaken for the auto-focus.
    let mut te = app_for(Arm::STe);
    let (te_probe, _te_out) = settled(&mut te, Arm::STe);
    assert!(
        te_probe.ctx.memory(|m| m.focused()).is_some(),
        "precondition: the single-pane TextEdit arm auto-focuses its editor on \
         launch, with no click — without this the comparison below measures \
         nothing"
    );

    let mut grid = app_for(Arm::GTe);
    let (grid_probe, pane) = settled(&mut grid, Arm::GTe);
    let grid_painted = painted_text(&pane);
    assert_eq!(
        grid_probe.ctx.memory(|m| m.focused()),
        None,
        "expected-absent: the grid TextEdit pane does not auto-focus on launch, \
         so a freshly-opened split view swallows the user's first keystrokes \
         until they click a pane. Something took focus, so the convenience is \
         being ported — flip this pin AND revisit the badge assertion below in \
         the same edit"
    );
    // The lack of focus must not be an artefact of the pane never laying out:
    // the arm assertion above already proved a pane rendered and published its
    // mode, and the ink row proves the same fixture paints into it.
    assert!(
        super::grid_parity_tests::pane_rect(&grid, grid.tabs[0].doc_id).is_some(),
        "precondition: the pane actually laid out, so `focused() == None` is the \
         missing auto-focus and not a missing pane"
    );

    // …and the coupled half: the pane still claims to be fully-featured.
    assert_eq!(
        EditorMode::Standard.badge(),
        None,
        "the full-feature default is badge-less by contract"
    );
    for badge in ALL_BADGES {
        assert!(
            !grid_painted.contains(badge),
            "the grid TextEdit pane publishes Standard — i.e. `nothing is \
             degraded` — while the convenience block above is absent. It \
             painted {badge}, so the pane grew a mode of its own: that is the \
             OTHER acceptable fix, and this pin must move with it"
        );
    }
}
