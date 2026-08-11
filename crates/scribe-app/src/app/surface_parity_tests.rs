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
//! Falsification ledger. Every cell below was OBSERVED red once before being
//! committed — 39 mutations of the PRODUCT source, 39 killed, each hash-verified
//! as applied and hash-verified as restored (a mutate-and-restore pass that
//! never confirms the file changed reports fake kills).
//!
//! Cuts (25), one per present cell: the mode publish on each of S-FOLD / S-RO /
//! S-ROPE / G-*; `EditorMode::Standard` swapped for `Rope`, which is what proves
//! the badge-LESS assertion discriminates rather than passing on absence;
//! `show_fold_view`; the diagnostics painter on each of S-ROPE / S-TE / G-ROPE /
//! G-TE; the metrics publish on S-TE, on `finish_embedded_scroll` (S-RO+S-ROPE)
//! and on the grid apply; the gutter feed on S-TE and G-TE plus the CLEAR on the
//! rope/browse arms and — since the fold gap below was closed — on the fold arm
//! too, cut at BOTH its pin and its navigation consequence; the focus->active
//! sync; the S-TE auto-focus; and — for the R2 hover row — the tooltip's span
//! lookup on each of the THREE painters that carry it (G-TE's, S-TE's, and the
//! one S-ROPE and G-ROPE share). Those three are cut at the LOOKUP rather than
//! at the painter, so the squiggle keeps rendering: R1 stayed green through all
//! three, which is what proves the hover row measures something R1 cannot see.
//! For the R23 promotion row: the grid pane's stable `TextEdit` id, the
//! clipboard-image paste hook, and the focus->active sync (that last one as
//! R23's control, so a broken sync cannot be mistaken for the gap R23 pins).
//!
//! Grafts (14), one per still-expected-absent cell, each a rehearsal of the edit
//! that will one day flip its pin: a guessed-height publish into the fold arm;
//! the fold check hoisted above the grid fork; a rope pane made reachable by the
//! focus sync; auto-focus added to the grid pane; a minimal ink painter into
//! each of S-FOLD / S-RO / G-RO; an arm made to publish `RopeMmap`; a minimal
//! hover registration into each of S-FOLD / S-RO / G-RO; and — for R23 — the
//! promoted rope pane reclaiming `pane_editor_id`, the `editor_focused` gate
//! opened, and a pane-rect click-to-activate that reaches a rope pane without
//! touching focus at all.
//!
//! One of those grafts earned its keep immediately. The R23 hook cell first
//! SURVIVED the gate-opening graft, and the reason was not the product: a
//! successful image paste emits its own `Event::Paste` on the next frame, and
//! `image_paste_gesture` suppresses the image branch for 20 frames after any
//! text paste — so the second paste was refused by that grace window and the
//! cell was passing for the wrong reason. Draining the window in the fixture is
//! what made the graft turn it red. A survived graft is the signal that an
//! absence pin is measuring its fixture rather than its wire.
//!
//! The ninth graft — `fold_view` added to the gutter-clear predicate — is no
//! longer a rehearsal. It was the real defect this matrix found, and landing it
//! turned its expected-absent pin into the enforcement assertion the R4 row now
//! carries, so it moved into the cut list above.
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
/// The pin is therefore TWO-sided, and the second side closed a live defect
/// this matrix found on its first run:
///
/// * the two `TextEdit` arms must REPLACE the poison with this frame's rows;
/// * **all five** non-`TextEdit` arms — the four rope / browse arms AND the
///   fold preview — must CLEAR it, which is what makes their absence honest
///   rather than stale.
///
/// The fold cell was originally pinned as EXPECTED-ABSENT: the clear predicate
/// read `(rope_arm || read_only)` with `fold_view` missing, while the PANEL's
/// own gate on the very next line already was `!fold_view`. Folding therefore
/// hid the gutter and left its feed pointing at the pre-fold rows — and worse
/// than one-buffer-stale, because the projection OMITS folded lines, so row
/// index `i` no longer denoted document line `i` at all. Adding `fold_view` to
/// the predicate flipped that cell to enforcement, and this assertion is that
/// flip: the poison must now be GONE for `SFold` exactly as it is for the rope
/// and browse arms.
///
/// The paint is not the property that matters, so it is not the property this
/// module proves alone:
/// `folding_hands_go_to_line_and_find_navigate_the_row_pitch_estimate` below
/// drives the actual `goto_line` and `find_navigate` jumps after a folded frame
/// and pins the fallback Y, because a test that only observed the hidden panel
/// would have passed on the broken code.
#[test]
fn the_external_gutter_feed_is_written_by_textedit_arms_and_cleared_by_every_other_arm() {
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
            Arm::SRope | Arm::GRope | Arm::SRo | Arm::GRo | Arm::SFold => assert!(
                left.is_empty(),
                "{arm:?} lays out no app-drawn gutter, so it must CLEAR the feed \
                 rather than leave the previous surface's rows behind — \
                 go-to-line and find-navigate read `line_gutter[line0]` and \
                 would jump to the old buffer's Y. It left {left:?}"
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// R4b — the gutter feed's NAVIGATION consequence, driven end to end
// ---------------------------------------------------------------------------

/// The panel being hidden is not the property that matters.
///
/// `find_nav.rs` reads `line_gutter[line0]` as the PREFERRED scroll source for
/// go-to-line (`goto_line`, which bookmark jumps and the CLI `file:LINE` jump
/// also drive) and for find-navigate (`scroll_to_offset`). Both fall through to
/// a `line0 * (editor_size * line_height)` row-pitch estimate only when the
/// lookup MISSES. So a fold arm that hides the panel without clearing the feed
/// leaves all three jumping to a Y taken from the pre-fold rows — and the fold
/// projection omits the folded-away lines, so those indices do not denote
/// document lines at all.
///
/// This drives the jump itself rather than observing the paint, because a test
/// that only asserted the gutter panel was invisible would have passed on the
/// broken code: `!fold_view` was already on the panel's gate while `fold_view`
/// was still missing from the clear predicate. That gap is exactly the shape of
/// the half-fix `e45341b` rejected for the rope arm, where hiding the strip was
/// explicitly not enough for the same reason.
///
/// The control is load-bearing: it jumps ONCE with the poison still in place,
/// before any folded frame, and requires the poisoned Y back. Without it this
/// test would pass just as happily if `goto_line` had stopped consulting
/// `line_gutter` altogether, which is a vacuous green, not a fix.
#[test]
fn folding_hands_go_to_line_and_find_navigate_the_row_pitch_estimate() {
    // A line deep inside the brace scope line 0 opens — i.e. one the fold
    // projection collapses away, which is precisely when a surviving row index
    // denotes nothing.
    const TARGET_1BASED: usize = 120;
    // A Y no real row of this fixture carries, so a surviving read is
    // unmistakable rather than merely off by a little.
    const POISON_Y: f32 = 777.0;
    // `matrix_text` line 4 + 137 -> document line index 141, matched by a query
    // that occurs exactly once.
    const FIND_QUERY: &str = "filler line 137,";
    const FIND_LINE0: usize = 141;

    let mut app = app_for(Arm::SFold);
    let p = Probe::new();
    p.settle(&mut app);

    let size = app.config.fonts.clamped_editor_size();
    let lh = app.config.fonts.clamped_line_height();
    let pitch = size * lh;

    // Long enough that `line_gutter.get(line0)` HITS for both targets — a short
    // vector would miss and take the fallback for the wrong reason.
    let poison = vec![POISON_Y; 205];

    // ---- Control: the preferred-source branch is live and this fixture reaches
    // it. If this does not come back as the poison, nothing below discriminates.
    app.line_gutter.clone_from(&poison);
    app.pending_scroll = None;
    app.goto_line(TARGET_1BASED);
    assert_eq!(
        app.pending_scroll,
        Some(POISON_Y),
        "control: a populated `line_gutter` IS the preferred scroll source, so \
         a jump must read it. It did not, which means this test can no longer \
         tell a cleared feed from an unread one — fix the control before \
         trusting the assertions below"
    );

    // ---- One folded frame, with the feed poisoned going in.
    app.line_gutter.clone_from(&poison);
    let out = p.idle(&mut app);
    assert_arm(&app, &out, Arm::SFold);

    // ---- Go-to-line (also the bookmark-jump and CLI-jump pipe).
    app.pending_scroll = None;
    app.goto_line(TARGET_1BASED);
    assert_ne!(
        app.pending_scroll,
        Some(POISON_Y),
        "a go-to-line taken while folded read the PRE-FOLD gutter row and \
         scrolled to a stale Y. The fold arm must clear `line_gutter` like the \
         rope and browse arms do"
    );
    assert_eq!(
        app.pending_scroll,
        Some((TARGET_1BASED - 1) as f32 * pitch),
        "with the feed cleared, go-to-line must take the \
         `line0 * (editor_size * line_height)` row-pitch estimate — the \
         specific fallback, not merely 'not the poison'"
    );

    // ---- Find-navigate, which reaches the SECOND reader (`scroll_to_offset`).
    app.line_gutter.clone_from(&poison);
    let out = p.idle(&mut app);
    assert_arm(&app, &out, Arm::SFold);

    FIND_QUERY.clone_into(&mut app.find_query);
    assert_eq!(
        app.find_matches_active().len(),
        1,
        "fixture: the find query must match exactly once, or the jump below is \
         not the jump this test names"
    );
    app.find_match_idx = 0;
    app.pending_scroll = None;
    app.find_navigate(true);
    assert_ne!(
        app.pending_scroll,
        Some(POISON_Y),
        "find-navigate reads the same feed through `scroll_to_offset`; folding \
         must not leave it a stale Y either"
    );
    assert_eq!(
        app.pending_scroll,
        Some(FIND_LINE0 as f32 * pitch),
        "find-navigate must land on the row-pitch estimate for the match's own \
         line"
    );
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

// ---------------------------------------------------------------------------
// R2 — the diagnostic HOVER: the message the ink stands for
// ---------------------------------------------------------------------------

/// The message `err_on_line_1` must surface, severity-prefixed.
const ERR_HOVER: &str = "error: cannot find value `error` in this scope";

/// The prefix alone, for the absence and off-row assertions: those must fail on
/// ANY diagnostic tooltip, not merely on this exact wording.
const ERR_HOVER_PREFIX: &str = "error: cannot find value";

/// The text of the document line the fixture's error is published against, used
/// to LOCATE that line's painted row on screen — never to assert anything.
const ERR_LINE_TEXT: &str = "second line holds the error span";

/// A line two below it, carrying no diagnostic of its own. The off-row control
/// hovers here.
const CLEAN_LINE_TEXT: &str = "fourth line holds the info notice";

/// The character column the hover is taken at: the middle of the published span
/// (chars 5..20), so the pointer is unambiguously ON the diagnostic rather than
/// merely somewhere on its line — the "same line, wrong character" bug the hover
/// resolver exists to avoid.
const ERR_HOVER_COL: usize = 12;

/// Rows shorter than this are the MINIMAP's, which paints the same buffer text
/// minified (~3px rows against the editor's ~17px). A height floor is what
/// discriminates the editor's own row from that copy; the caller additionally
/// requires the survivor to be UNIQUE, so an ambiguity fails loudly instead of
/// silently picking the wrong one.
const EDITOR_ROW_MIN_H: f32 = 8.0;

/// The one editor-sized painted ROW whose text contains `needle`: its screen
/// rect and the row itself (for `x_offset`).
///
/// Read back from the frame's own shape list rather than guessed from the
/// window, which is what lets ONE target rule serve all seven arms. The
/// `TextEdit` arms lay the whole buffer out as a single galley and report the
/// line as one `PlacedRow` inside it; the rope and browse arms report one galley
/// per row they painted; the fold arm projects a different line set at a
/// different origin entirely. What every one of them has in common is that the
/// erroring line is painted as a row somewhere on screen — and that row is where
/// a user would put the pointer.
///
/// This derives a pointer POSITION (an input). It asserts nothing about the
/// overlay, so it cannot supply the wire the hover cells measure.
fn painted_row(
    out: &egui::FullOutput,
    needle: &str,
) -> (egui::Rect, std::sync::Arc<egui::epaint::text::Row>) {
    let mut hits: Vec<(egui::Rect, std::sync::Arc<egui::epaint::text::Row>)> = Vec::new();
    for clipped in &out.shapes {
        super::grid_parity_tests::walk_shape(&clipped.shape, &mut |s| {
            if let egui::Shape::Text(t) = s {
                for prow in &t.galley.rows {
                    if prow.row.text().contains(needle) {
                        let rect =
                            egui::Rect::from_min_size(t.pos + prow.pos.to_vec2(), prow.row.size);
                        if rect.height() >= EDITOR_ROW_MIN_H {
                            hits.push((rect, prow.row.clone()));
                        }
                    }
                }
            }
        });
    }
    assert_eq!(
        hits.len(),
        1,
        "the frame must paint exactly ONE editor-sized row carrying {needle:?} to \
         aim the pointer at; it painted {} ({:?}). Zero means the arm never \
         rendered that line, so a no-tooltip verdict would measure the FIXTURE \
         rather than the overlay; more than one means the target is ambiguous",
        hits.len(),
        hits.iter().map(|(r, _)| *r).collect::<Vec<_>>()
    );
    hits.pop().expect("exactly one hit asserted above")
}

/// What `arm` painted while the pointer sat on the middle of the published error
/// span, and — when asked for — what it painted while the pointer sat on a clean
/// row two lines further down.
struct ArmHover {
    on_span: String,
    off_row: Option<String>,
}

/// Publish one error, settle `arm`, and hover it.
///
/// Two preconditions make a silent result mean "the arm painted no tooltip" and
/// nothing else: the arm is proven to have rendered (`assert_arm`), and the
/// published diagnostic is proven to resolve onto a real byte span. Without the
/// latter a silent tooltip could mean the RESOLVER produced nothing, and
/// grafting a hover into an expected-absent arm would not turn its pin red —
/// which is the one property an absence pin has to have.
fn arm_hover(arm: Arm, with_control: bool) -> ArmHover {
    let mut app = app_for(arm);
    app.diagnostics = err_on_line_1();
    let (p, out) = settled(&mut app, arm);
    assert!(
        !app.diagnostic_spans_for_active(app.active).is_empty(),
        "{arm:?}: the fixture must resolve the published diagnostic onto a real \
         byte span, otherwise a silent hover measures the RESOLVER rather than \
         the arm"
    );

    let (rect, row) = painted_row(&out, ERR_LINE_TEXT);
    let target = egui::pos2(rect.min.x + row.x_offset(ERR_HOVER_COL), rect.center().y);
    // Twice: egui raises a tooltip for a pointer that was ALREADY there, so a
    // single move frame shows nothing on any arm.
    p.hover(&mut app, target);
    let on_span = painted_text(&p.hover(&mut app, target));

    let off_row = with_control.then(|| {
        let (clean, crow) = painted_row(&p.idle(&mut app), CLEAN_LINE_TEXT);
        assert!(
            !clean.intersects(rect),
            "{arm:?}: the control row must be a DIFFERENT row from the erroring \
             one (error {rect:?}, control {clean:?})"
        );
        let away = egui::pos2(clean.min.x + crow.x_offset(ERR_HOVER_COL), clean.center().y);
        p.hover(&mut app, away);
        painted_text(&p.hover(&mut app, away))
    });

    ArmHover { on_span, off_row }
}

/// The ink says WHERE; only the hover says WHAT. Exactly the four editable arms
/// surface the message, and the three read-only arms surface nothing.
///
/// R1 above pins the squiggle, and a squiggle alone is the two status-bar
/// integers' worth of information placed on the right line: it does not name the
/// problem. Cutting the tooltip block out of either painter leaves R1 completely
/// green, so without this row the whole payload of the diagnostics overlay is
/// unratcheted on three of the four arms that carry it — `grid_parity_tests`
/// covers the grid `TextEdit` pane alone.
///
/// The two rope arms are the ones this most needed to reach. Their painter's own
/// doc comment asserts that the hover tooltip is "deliberately the SAME" as the
/// `TextEdit` overlay's, and `EditorMode::Rope`'s entry-notice tells the user
/// inline diagnostics still work — two user-facing claims with nothing behind
/// them. The single-pane `TextEdit` arm was equally unpinned.
///
/// The three read-only arms are expected-absent, and deliberately pinned at a
/// point where a user WOULD hover: `painted_row` proves each of them really
/// paints the erroring line on screen, so the silence is the missing overlay
/// rather than a missing line, and grafting a hover registration into any of
/// them turns this red.
#[test]
fn the_diagnostic_hover_names_the_problem_on_exactly_the_four_editable_arms() {
    // Every cell is evaluated and reported, rather than the first failure
    // aborting the row. Two arms share ONE painter here (`SRope` and `GRope`
    // both reach `paint_rope_pane_diagnostics`), so a run that stopped at the
    // first red would name one of them and leave the other's verdict unstated —
    // and a matrix that cannot say WHICH cells went red is not a ratchet over
    // them.
    let mut red: Vec<String> = Vec::new();

    for arm in [Arm::SRope, Arm::STe, Arm::GRope, Arm::GTe] {
        let h = arm_hover(arm, true);
        if !h.on_span.contains(ERR_HOVER) {
            red.push(format!(
                "{arm:?}: must paint the severity-prefixed message while the \
                 pointer sits on the published span — the squiggle says WHERE, \
                 the hover is the only thing that says WHAT. The frame painted \
                 no such text"
            ));
        }
        if h.off_row
            .expect("the control was requested")
            .contains(ERR_HOVER_PREFIX)
        {
            red.push(format!(
                "{arm:?}: hovering a clean row two lines below must paint NO \
                 diagnostic tooltip — a tooltip that follows the pointer \
                 anywhere on the surface is not resolving the span, and would \
                 make the presence cell pass without the resolver"
            ));
        }
    }

    for arm in [Arm::SFold, Arm::SRo, Arm::GRo] {
        if arm_hover(arm, false).on_span.contains(ERR_HOVER_PREFIX) {
            red.push(format!(
                "{arm:?}: pinned expected-absent for the diagnostic hover — it \
                 paints no squiggle (R1) and registers no hover rect, so a \
                 pointer sitting directly on the erroring text names nothing. A \
                 tooltip appeared, so the arm grew an overlay — flip this pin to \
                 enforcement rather than weakening it"
            ));
        }
    }

    assert!(
        red.is_empty(),
        "{} cell(s) red:\n{}",
        red.len(),
        red.join("\n")
    );
}

// ---------------------------------------------------------------------------
// R23 — the grid pane that is PROMOTED to the rope editor while the user is
//       typing in it
// ---------------------------------------------------------------------------

/// The command modifier the `editor_focused`-gated editor hooks are bound to.
const CTRL: egui::Modifiers = egui::Modifiers::COMMAND;

/// A grid whose panes start UNDER a real 1 KiB rope threshold.
///
/// Every other fixture in this file pins `rope_editor_auto_threshold_bytes = 0`
/// so each arm is entered EXPLICITLY and cannot drift onto a neighbour. This one
/// deliberately does the opposite: the transition is the subject. A pane starts
/// as `GTe`, the buffer grows past the threshold, and the pane becomes `GRope`
/// underneath a user who is mid-sentence — which is the production path (the
/// default threshold is 16 MiB and a paste, a generated file or an appended log
/// crosses it without asking).
///
/// Both tabs are markdown list lines, and long enough to fill the pane: a click
/// 40% down a SHORT buffer lands past the end of the widget, gives the pane no
/// focus, and would make every cell below pass vacuously — the fixture-geometry
/// trap `grid_parity_tests::diag_grid_text` documents.
fn promotable_grid() -> ScribeApp {
    // 60 x 15 bytes = 900 bytes, comfortably under the 1 KiB threshold below.
    let under = "- short buffer\n".repeat(60);
    let mut cfg = grid_config();
    cfg.editor.grid_enabled = true;
    cfg.editor.rope_editor_auto_threshold_bytes = 1024;
    cfg.editor.experimental_rope_editor = false;
    cfg.spellcheck.enabled = false;
    let mut app = ScribeApp::new_test(cfg);
    app.tabs[0].text.clone_from(&under);
    app.tabs.push(EditorTab::scratch());
    app.tabs[1].text = under;
    app.active = 0;
    app
}

/// The same list shape, past the threshold. Still list items on purpose: a
/// grown buffer with nothing to toggle would make the chord cell below pass
/// because there was no checkbox to make, not because the chord never fired.
fn over_threshold_list() -> String {
    (0..400)
        .map(|i| format!("- line {i:04} ................................\n"))
        .collect()
}

/// A settled `promotable_grid`, with the handles every promotion cell needs.
///
/// The three cells below are written as three tests rather than one, because
/// they share a root cause and therefore share candidate fixes: restoring focus
/// on the promoted pane changes what the OTHER cells' controls observe, so a
/// single test would abort in one cell's control while another cell's verdict
/// went unstated. Split, each graft turns exactly the cell it rehearses red.
struct Promotable {
    app: ScribeApp,
    p: Probe,
    te0: egui::Id,
    te1: egui::Id,
    rect0: egui::Rect,
    body0: egui::Pos2,
    body1: egui::Pos2,
}

/// Settle the fixture and prove pane 0 really starts as `GTe` — without that,
/// every promotion cell could pass on a pane that was a rope pane all along.
fn promotable() -> Promotable {
    promotable_in(None)
}

/// As [`promotable`], with a notes vault configured — the clipboard-image paste
/// hook refuses outright without one, so its cell needs a real destination.
fn promotable_in(vault: Option<&std::path::Path>) -> Promotable {
    let mut app = promotable_grid();
    app.config.notes.vault_dir = vault.map(std::path::Path::to_path_buf);
    let p = Probe::new();
    p.settle(&mut app);

    let doc0 = app.tabs[0].doc_id;
    let doc1 = app.tabs[1].doc_id;
    let rect0 = super::grid_parity_tests::pane_rect(&app, doc0).expect("pane 0 laid out");
    let rect1 = super::grid_parity_tests::pane_rect(&app, doc1).expect("pane 1 laid out");

    let out = p.idle(&mut app);
    assert!(
        !painted_text(&out).contains("[ ROPE ]"),
        "precondition: pane 0 starts UNDER the threshold, i.e. as G-TE"
    );
    assert!(
        app.tabs[0].rope_state.is_none(),
        "precondition: …and has built no rope state yet"
    );

    Promotable {
        te0: grid_methods::pane_editor_id(doc0),
        te1: grid_methods::pane_editor_id(doc1),
        rect0,
        // 40% down, below the pane's header chip and well inside the body.
        body0: egui::pos2(rect0.center().x, rect0.top() + rect0.height() * 0.4),
        body1: egui::pos2(rect1.center().x, rect1.top() + rect1.height() * 0.4),
        app,
        p,
    }
}

/// Grow pane 0's buffer past the threshold and prove the promotion happened.
fn promote(f: &mut Promotable) {
    f.app.tabs[0].text = over_threshold_list();
    f.p.idle(&mut f.app);
    let out = f.p.idle(&mut f.app);
    assert!(
        painted_text(&out).contains("[ ROPE ]"),
        "the grown pane must render through the owned-rope arm and say so"
    );
    assert!(
        f.app.tabs[0].rope_state.is_some(),
        "…and `rope_state` is created by that arm and by nothing else here"
    );
}

/// Cell 1 — the promotion silently takes the keyboard away.
///
/// This is the first of the three cells the matrix left as UNKNOWN rather than
/// guessed, and it is settled here by RUNNING: egui does NOT retain focus on
/// `pane_editor_id` across the swap. The `TextEdit` simply stops being created,
/// so its focus is dropped, and from the FIRST promoted frame `Memory::focused()`
/// is `None` — the rope pane registers under `drag_scroll::rope_editor_focus_id`
/// instead, which nothing here matches.
///
/// The click precondition is what makes the silence meaningful: the pane took
/// the keyboard a moment earlier, in the same fixture, from the same gesture.
///
/// Pinned as the current behaviour so a fix breaks it loudly — making the
/// promoted rope pane reclaim `pane_editor_id` turns this red.
#[test]
fn a_grid_pane_promoted_to_the_rope_editor_mid_session_loses_the_keyboard() {
    let mut f = promotable();
    f.p.mod_click(&mut f.app, f.body0, egui::Modifiers::NONE);
    f.p.idle(&mut f.app);
    assert!(
        f.p.ctx.memory(|m| m.has_focus(f.te0)),
        "precondition: clicking a G-TE pane's body at {:?} (pane rect {:?}) must \
         give ITS editor the keyboard",
        f.body0,
        f.rect0
    );
    assert_eq!(
        f.app.active, 0,
        "precondition: …and the focus->active sync must make it the active tab"
    );

    promote(&mut f);

    assert_eq!(
        f.p.ctx.memory(|m| m.focused()),
        None,
        "expected-absent: the promotion drops keyboard focus entirely — nothing \
         holds it at all, one frame after the same pane held it. Something does \
         now, so the swap carries focus across — flip this pin"
    );
    assert!(
        !f.p.ctx.memory(|m| m.has_focus(f.te0)),
        "expected-absent: …and in particular NOT the pane's own editor id"
    );
}

/// The PNGs a clipboard-image paste has landed in the vault so far.
fn attachment_pngs(vault: &std::path::Path) -> usize {
    std::fs::read_dir(vault.join("attachments")).map_or(0, |d| {
        d.filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|x| x == "png"))
            .count()
    })
}

/// A tiny opaque clipboard image, handed to the paste hook through the
/// `note_capture` test seam.
fn clipboard_image() -> crate::app::note_capture::ClipboardImage {
    crate::app::note_capture::ClipboardImage {
        width: 2,
        height: 2,
        rgba: vec![0x40u8; 2 * 2 * 4],
    }
}

/// Cell 2 — and every capability gated on that focus goes with it.
///
/// `render_grid_central_panel` runs ONE block `if !active_read_only &&
/// editor_focused`, where `editor_focused` is
/// `has_focus(pane_editor_id(active_doc))`. Behind it sit the markdown chords
/// (Ctrl+B / Ctrl+I / Ctrl+` / Ctrl+Shift+X / Ctrl+Enter) AND the
/// clipboard-image paste hook. Cell 1 showed that focus is gone one frame after
/// the promotion, so this entire block stops running — silently, with the
/// buffer still editable and no signal that half the editor's chords went away.
///
/// The clipboard-image paste is the witness, and the choice is deliberate. The
/// chords are the more obvious one, but they cannot be pinned honestly here:
/// widening the gate does NOT bring a chord back, because the drain
/// (`apply_pending_caret_ops` -> `active_line_span`) also needs the `TextEdit`
/// caret state that a promoted pane no longer maintains. An assertion that no
/// available fix can turn red would silently survive the fix it exists to
/// detect — the one thing an absence pin must never do. The paste hook has no
/// such second dependency: it writes the PNG into the vault before it ever
/// touches a caret, so it is gated on `editor_focused` and nothing else, and
/// opening that gate turns this red.
///
/// Asserted on the FILE, never on `pending_insert_text`: the latch was
/// historically the thing that got set and never drained, so a latch assertion
/// is exactly the one that cannot see this class of bug.
#[test]
fn a_promoted_grid_pane_silently_stops_answering_the_focus_gated_editor_hooks() {
    let vault = tempfile::tempdir().expect("temp vault");
    let mut f = promotable_in(Some(vault.path()));
    f.p.mod_click(&mut f.app, f.body0, egui::Modifiers::NONE);
    f.p.idle(&mut f.app);

    // The RELEASE alone, which is the only event `egui_winit` emits for an
    // image-only clipboard — it special-cases the paste chord on key-DOWN and
    // returns before pushing any `Event::Key`.
    //
    // The trailing idle frames are LOAD-BEARING, and finding out why is the
    // reason this cell is trustworthy. A successful image paste inserts its
    // markdown by emitting `Event::Paste("![pasted image](…)")` on the NEXT
    // frame, and `image_paste_gesture` records every text paste to suppress the
    // image branch for `PASTE_IMAGE_TEXT_GRACE_FRAMES` (20) — the window that
    // joins a real clipboard's key-down `Paste` to its later `V` release. So a
    // second paste taken within 20 frames of the first is refused BY THAT
    // WINDOW, whatever the focus gate says. Without this drain the
    // expected-absent assertion below passed for the wrong reason: opening the
    // `editor_focused` gate did not turn it red, because the grace window was
    // suppressing the paste and the pin was measuring the fixture rather than
    // the promotion.
    let paste = |f: &mut Promotable| {
        crate::app::note_capture::test_hooks::set_next_image(clipboard_image());
        f.p.frame(
            &mut f.app,
            CTRL,
            vec![egui::Event::Key {
                key: egui::Key::V,
                physical_key: None,
                pressed: false,
                repeat: false,
                modifiers: CTRL,
            }],
        );
        // One to deliver the insertion, then past the grace window.
        for _ in 0..24 {
            f.p.idle(&mut f.app);
        }
    };

    assert_eq!(
        attachment_pngs(vault.path()),
        0,
        "precondition: the vault starts with no attachments"
    );
    paste(&mut f);
    assert_eq!(
        attachment_pngs(vault.path()),
        1,
        "precondition: Ctrl+V with an image on the clipboard must land a PNG in \
         the vault while the G-TE pane holds focus — if the hook does nothing \
         HERE, the assertion below measures nothing"
    );

    promote(&mut f);

    paste(&mut f);
    assert_eq!(
        attachment_pngs(vault.path()),
        1,
        "expected-absent: the promotion silently switches off every hook behind \
         the `editor_focused` gate — the same paste that landed an attachment \
         moments ago now writes nothing, and the markdown chords go with it. A \
         second PNG appeared, so the gate reaches the promoted pane now — flip \
         this pin"
    );
}

/// Cell 3 — and clicking the pane does not give any of it back.
///
/// The focus->active sync matches `pane_editor_id` alone, so a promoted pane is
/// unreachable: `self.active` stays on whichever pane was last a `TextEdit`, and
/// the status bar's counters, encoding, language, EOL, caret readout and the
/// diagnostics span resolution all keep describing THAT pane while the user
/// looks at this one.
///
/// R22 pins the same gap for a pane that was always a rope pane. This is the
/// direction a user actually reaches it — the pane was theirs, and the app
/// quietly stopped agreeing.
///
/// `active` is moved to the sibling first, and that move is a hard control:
/// without it "still 0" would be indistinguishable from "came back to 0".
#[test]
fn a_promoted_grid_pane_can_no_longer_be_clicked_into_activity() {
    let mut f = promotable();
    promote(&mut f);

    f.p.mod_click(&mut f.app, f.body1, egui::Modifiers::NONE);
    f.p.idle(&mut f.app);
    assert_eq!(
        f.app.active, 1,
        "control: the sibling pane is still a `TextEdit`, so clicking IT must \
         still move `active` — if this fails the sync is broken outright and the \
         assertion below is not measuring the promotion"
    );
    assert!(
        f.p.ctx.memory(|m| m.has_focus(f.te1)),
        "control: …and that pane's editor holds the keyboard"
    );

    f.p.mod_click(&mut f.app, f.body0, egui::Modifiers::NONE);
    f.p.idle(&mut f.app);
    assert_eq!(
        f.app.active, 1,
        "expected-absent: clicking the PROMOTED pane's body at {:?} (pane rect \
         {:?}) must NOT make it active — the focus->active sync matches \
         `pane_editor_id` alone and this pane no longer registers one. It became \
         active, so the pane is reachable again — flip this pin (and R22's half \
         with it)",
        f.body0, f.rect0
    );
}
