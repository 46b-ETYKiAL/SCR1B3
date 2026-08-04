//! Offscreen render-to-image VISUAL QA harness.
//!
//! These tests render the REAL `ScribeApp` frame to a PNG via egui_kittest's
//! wgpu backend, so the app's actual pixels can be inspected (the change bar,
//! occurrence boxes, gutter, status bar, modals, …) — the one thing the
//! AccessKit-based e2e tests cannot do.
//!
//! They are `#[ignore]` by default and gated behind a real-adapter probe, so a
//! GPU-less CI runner never runs (or fails) them. Run on a GPU host with:
//!   cargo test -p scribe-app --features … visual_qa -- --ignored --nocapture
//! Each scene prints the PNG path it wrote.

use super::gpu_probe::gpu_available;
use super::*;
use egui_kittest::Harness;

fn out_dir() -> std::path::PathBuf {
    let d = std::env::temp_dir().join("scr1b3-visual-qa");
    let _ = std::fs::create_dir_all(&d);
    d
}

/// Render `app`'s frame to `<temp>/scr1b3-visual-qa/<name>.png`. Returns the
/// path, or `None` when no GPU adapter is available (clean skip).
fn render_scene(name: &str, w: f32, h: f32, app: ScribeApp) -> Option<std::path::PathBuf> {
    if !gpu_available() {
        eprintln!("[visual-qa] no GPU adapter; skipping `{name}`");
        return None;
    }
    let mut harness: Harness<'static, ScribeApp> = Harness::builder()
        .with_size(egui::vec2(w, h))
        .wgpu()
        .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app);
    // A few frames so the one-frame-lagged gutter Ys + galley layout settle.
    for _ in 0..5 {
        harness.step();
    }
    let img = harness
        .render()
        .expect("kittest wgpu render of the real ScribeApp frame must succeed");
    let path = out_dir().join(format!("{name}.png"));
    img.save(&path).expect("save visual-qa png");
    eprintln!(
        "[visual-qa] wrote {} ({}x{})",
        path.display(),
        img.width(),
        img.height()
    );
    Some(path)
}

/// Base config for QA scenes: first-run done (no welcome modal) and motion
/// disabled so the frame is static (no perpetual repaint → render is stable).
fn qa_config() -> Config {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    cfg.motion.enabled = false;
    cfg
}

const SAMPLE: &str = "fn main() {\n    let x = 1;\n    let y = 2;\n    println!(\"{x} {y}\");\n}\n";

/// Notes (PKM) side pane OPEN over a small temp vault. Writes three markdown
/// notes — one carrying `[[Ideas]]` wiki-links and `#project/alpha` tags, plus
/// the linked `Ideas.md` (so the backlinks pane has something), and a `daily.md`.
/// Points the config vault at that dir, opens the linking note as the active tab,
/// and toggles `notes_pane_open`. Read the PNG: the left "NOTES" pane must show
/// the vault path, the three-note list, and the "links out" / "backlinks"
/// sections populated from the active note.
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn scene_notes_pane() {
    let vault = out_dir().join("qa-vault");
    let _ = std::fs::create_dir_all(&vault);
    std::fs::write(
        vault.join("Home.md"),
        "# Home\n\nSee [[Ideas]] and [[daily]]. #project/alpha #inbox\n",
    )
    .expect("write Home.md");
    std::fs::write(
        vault.join("Ideas.md"),
        "# Ideas\n\nA linked note. Back to [[Home]]. #project/alpha\n",
    )
    .expect("write Ideas.md");
    std::fs::write(vault.join("daily.md"), "# 2026-07-22\n\nDaily note.\n")
        .expect("write daily.md");

    let mut cfg = qa_config();
    cfg.notes.vault_dir = Some(vault.clone());
    let mut app = ScribeApp::new_test(cfg);
    app.tabs.clear();
    // Active tab = the linking note, so "links out" (Ideas/daily) and its own
    // backlink (from Ideas) both populate.
    let t = EditorTab::from_path(vault.join("Home.md")).expect("open Home.md");
    app.tabs.push(t);
    app.active = 0;
    app.notes_pane_open = true;
    render_scene("notes_pane", 1100.0, 720.0, app);
}

#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn scene_default() {
    let mut app = ScribeApp::new_test(qa_config());
    app.tabs.clear();
    let mut t = EditorTab::scratch();
    t.text = SAMPLE.to_string();
    t.session_baseline = SAMPLE.to_string();
    t.saved_baseline = SAMPLE.to_string();
    app.tabs.push(t);
    app.active = 0;
    render_scene("default", 1100.0, 720.0, app);
}

/// A markdown-rich note sample used by the note-colouring QA scenes — exercises
/// headings, setext underlines, `----` thematic breaks, decorative `====//`
/// dividers, bold/italic/code/strike, quotes, lists, task boxes, links, `#tags`,
/// and tables.
const MD_COLOR_SAMPLE: &str = r#"# ATX Heading 1
## ATX Heading 2
### ATX Heading 3

Setext Heading 1
================

Setext Heading 2
----------------

Thematic break below:

----

Decorative divider:

====//====//====//

Some **bold**, *italic*, `inline code` and ~~strikethrough~~ text.

> A blockquote line.

- bullet item
- [ ] unchecked task
- [x] checked task
1. numbered item

```rust
fn code() { let x = 1; }
```

A [link](https://example.com), a #tag, and a bare https://bare.example.com URL.

| col a | col b |
|:-----:|-------|
| 1     | 2     |
"#;

/// Render the markdown sample under a given note theme. Writes a temp `.md` so
/// `language_hint` routes to the markdown grammar; sets `note_theme` so the
/// frame applies the chosen palette.
fn render_markdown_scene(scene: &str, note_theme: &str, rich: bool) {
    let path = out_dir().join(format!("{scene}.md"));
    std::fs::write(&path, MD_COLOR_SAMPLE).expect("write md sample");
    let mut cfg = qa_config();
    cfg.editor.note_theme = note_theme.to_string();
    cfg.editor.md_rich_coloring = rich;
    let mut app = ScribeApp::new_test(cfg);
    app.tabs.clear();
    let t = EditorTab::from_path(path).expect("open md sample");
    app.tabs.push(t);
    app.active = 0;
    render_scene(scene, 1100.0, 900.0, app);
}

/// The default note theme with ALL markdown-colouring passes on — confirms
/// `----` / `====//` dividers, `#tags`, `~~strikethrough~~`, task boxes, and
/// table pipes are all coloured.
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn scene_markdown_coloring() {
    render_markdown_scene("markdown_coloring", "base16-eighties.dark", true);
}

/// Master switch OFF — proves every extra pass can be disabled: the same note
/// falls back to plain syntect grammar highlighting (dividers/tags/strike/tasks/
/// tables uncoloured).
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn scene_markdown_no_coloring() {
    render_markdown_scene("markdown_no_coloring", "base16-eighties.dark", false);
}

/// A newly-added popular palette (Dracula) — confirms the new note themes load
/// and colour markdown (headings/bold/italic/link/code/divider).
#[test]
#[ignore = "GPU render; run with --ignore on a host with a wgpu adapter"]
fn scene_markdown_dracula() {
    render_markdown_scene("markdown_dracula", "Dracula", true);
}

/// NARROW window + a LONG status string — proves the bottom status-bar filename
/// (right-aligned `self.status`, e.g. "opened /…/file.rs") TRUNCATES with an
/// ellipsis instead of overflowing leftward and overlapping the left-side
/// indicators (EOL / encoding / language / counts / caret). Read the PNG: the
/// left segments must stay legible and the status text must end in "…" at the
/// boundary, never paint over the left text.
#[test]
#[ignore = "GPU render"]
fn scene_status_bar_narrow() {
    let mut app = ScribeApp::new_test(qa_config());
    app.tabs.clear();
    let mut t = EditorTab::scratch();
    t.text = SAMPLE.to_string();
    t.session_baseline = SAMPLE.to_string();
    t.saved_baseline = SAMPLE.to_string();
    app.tabs.push(t);
    app.active = 0;
    // A long status path that, on a narrow window, would previously overflow the
    // right_to_left status segment leftward across the left indicators.
    app.status =
        "opened /workspace/projects/very/deep/nested/path/to/a/long_file_name.rs".to_string();
    render_scene("status_bar_narrow", 480.0, 360.0, app);
}

/// Change bar: line 2 edited+saved (green), line 3 edited+unsaved (amber),
/// the rest untouched (no stripe).
#[test]
#[ignore = "GPU render"]
fn scene_change_bar() {
    let mut app = ScribeApp::new_test(qa_config());
    app.tabs.clear();
    let mut t = EditorTab::scratch();
    // current text
    t.text = SAMPLE.to_string();
    // session baseline differs on lines 2 AND 3 (both edited this session)
    t.session_baseline =
        "fn main() {\n    let A = 1;\n    let B = 2;\n    println!(\"{x} {y}\");\n}\n".to_string();
    // saved baseline matches line 2 (so it's Saved/green) but still differs on
    // line 3 (so it stays Unsaved/amber).
    t.saved_baseline =
        "fn main() {\n    let x = 1;\n    let B = 2;\n    println!(\"{x} {y}\");\n}\n".to_string();
    t.change_gen = None;
    app.tabs.push(t);
    app.active = 0;
    render_scene("change_bar", 1100.0, 720.0, app);
}

/// Find bar open with a query → the highlight-all match washes (same paint
/// path the selection-occurrence boxes reuse).
#[test]
#[ignore = "GPU render"]
fn scene_find_bar() {
    let mut app = ScribeApp::new_test(qa_config());
    app.tabs.clear();
    let mut t = EditorTab::scratch();
    t.text = SAMPLE.to_string();
    t.session_baseline = SAMPLE.to_string();
    t.saved_baseline = SAMPLE.to_string();
    app.tabs.push(t);
    app.active = 0;
    app.find_open = true;
    app.find_query = "let".to_string();
    render_scene("find_bar", 1100.0, 720.0, app);
}

/// Trailing-whitespace tint + column rulers, with content that has trailing
/// spaces on a couple of lines.
#[test]
#[ignore = "GPU render"]
fn scene_trailing_ws_and_rulers() {
    let mut cfg = qa_config();
    cfg.editor.highlight_trailing_whitespace = true;
    cfg.editor.rulers = vec![20, 40];
    let mut app = ScribeApp::new_test(cfg);
    app.tabs.clear();
    let mut t = EditorTab::scratch();
    t.text = "fn main() {   \n    let x = 1;\n    let y = 2;    \n}\n".to_string();
    t.session_baseline = t.text.clone();
    t.saved_baseline = t.text.clone();
    app.tabs.push(t);
    app.active = 0;
    render_scene("trailing_ws_rulers", 1100.0, 720.0, app);
}

/// Settings window open (Editor section) — checks the settings layout +
/// widths + the new change-bar / occurrence / trailing-ws toggles render.
#[test]
#[ignore = "GPU render"]
fn scene_settings() {
    let mut app = ScribeApp::new_test(qa_config());
    app.tabs.clear();
    let mut t = EditorTab::scratch();
    t.text = SAMPLE.to_string();
    t.session_baseline = SAMPLE.to_string();
    t.saved_baseline = SAMPLE.to_string();
    app.tabs.push(t);
    app.active = 0;
    app.settings_open = true;
    render_scene("settings", 1100.0, 720.0, app);
}

/// Render the Settings window open on a SPECIFIC category page by clicking its
/// nav tab, so the new stepper-arrow dropdowns, −/+ slider buttons, and grouped
/// sections are visible for visual QA. Returns the PNG path (None on no GPU).
fn render_settings_category(name: &str, category: &str) -> Option<std::path::PathBuf> {
    use egui_kittest::kittest::Queryable as _;
    if !gpu_available() {
        eprintln!("[visual-qa] no GPU adapter; skipping `{name}`");
        return None;
    }
    let mut app = ScribeApp::new_test(qa_config());
    app.tabs.clear();
    let mut t = EditorTab::scratch();
    t.text = SAMPLE.to_string();
    app.tabs.push(t);
    app.active = 0;
    app.settings_open = true;
    let mut harness: Harness<'static, ScribeApp> = Harness::builder()
        .with_size(egui::vec2(920.0, 860.0))
        .wgpu()
        .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app);
    for _ in 0..3 {
        harness.step();
    }
    // Click the category tab in the settings left-nav, then let it settle.
    // "Appearance" is the DEFAULT page, already shown — clicking it is redundant
    // and its page heading duplicates the tab label (ambiguous `get_by_label`),
    // so only click when switching AWAY from the default.
    if category != "Appearance" {
        harness.get_by_label(category).click();
        for _ in 0..6 {
            harness.step();
        }
    }
    let img = harness
        .render()
        .expect("kittest wgpu render of the settings page must succeed");
    let path = out_dir().join(format!("{name}.png"));
    img.save(&path).expect("save visual-qa png");
    eprintln!(
        "[visual-qa] wrote {} ({}x{})",
        path.display(),
        img.width(),
        img.height()
    );
    Some(path)
}

/// Motion page — the biggest regroup: 5 grouped sections, and every slider now
/// carries −/+ buttons.
#[test]
#[ignore = "GPU render"]
fn scene_settings_motion() {
    render_settings_category("settings_motion", "Motion");
}

/// Fonts page — the note/UI/theme dropdowns now carry ◀/▶ stepper arrows, plus
/// the Size/Line-height ±sliders, under 3 grouped sections.
#[test]
#[ignore = "GPU render"]
fn scene_settings_fonts() {
    render_settings_category("settings_fonts", "Fonts");
}

/// Appearance page — theme stepper, UI-scale ±slider, 4 grouped sections.
#[test]
#[ignore = "GPU render"]
fn scene_settings_appearance() {
    render_settings_category("settings_appearance", "Appearance");
}

/// Keyboard page — the rebinding UI. This page shipped WITHOUT a visual scene,
/// so its rows had never actually been looked at: the whole point of the page is
/// that each row is click-to-capture, and a row that renders as a dead label
/// (which is what the rows looked like BEFORE they were rebindable) is
/// indistinguishable from a working one in a passing unit test.
#[test]
#[ignore = "GPU render"]
fn scene_settings_keyboard() {
    render_settings_category("settings_keyboard", "Keyboard");
}

/// Several tabs incl. a dirty one + a pinned one — checks the tab strip
/// layout, the dirty `*` marker, the pin glyph, and active-tab styling.
#[test]
#[ignore = "GPU render"]
fn scene_tabs() {
    let mut app = ScribeApp::new_test(qa_config());
    app.tabs.clear();
    for (i, name) in ["main.rs", "lib.rs", "notes.md", "config.toml"]
        .iter()
        .enumerate()
    {
        let mut t = EditorTab::scratch();
        t.text = format!("// {name}\n{SAMPLE}");
        t.session_baseline = t.text.clone();
        t.saved_baseline = t.text.clone();
        t.doc_id = crate::grid::DocId(i as u64);
        app.tabs.push(t);
    }
    app.tabs[0].pinned = true;
    // Make tab 2 look dirty (text diverges from the saved doc mirror).
    app.tabs[2].text.push_str("\nunsaved edit\n");
    app.active = 1;
    render_scene("tabs", 1100.0, 720.0, app);
}

/// Top-bar button chrome parity (Fix 1). The LEFT toolbar buttons must now read
/// as FRAMELESS — transparent when idle, matching the RIGHT window-caption
/// buttons — with a persistent accent fill ONLY on a toggled-ON toggle. This
/// scene turns the minimap + word-wrap toggles ON, so the PNG should show those
/// two carrying a low-alpha accent background while `>_`, new/open/save/find/
/// split/⋯ and the OFF toggles are all transparent (no filled button boxes).
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn scene_toolbar_frameless() {
    let mut cfg = qa_config();
    // Two toggles ON so the accent on-fill is visible next to frameless buttons.
    cfg.editor.show_minimap = true;
    cfg.editor.word_wrap = true;
    // Ensure a rich set of quick-access items is on the bar so the frameless
    // treatment is visible across plain buttons AND toggles.
    cfg.toolbar.items = [
        "new",
        "open",
        "save",
        "find",
        "split",
        "minimap",
        "wrap",
        "linenumbers",
        "spellcheck",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    let mut app = ScribeApp::new_test(cfg);
    app.tabs.clear();
    let mut t = EditorTab::scratch();
    t.text = SAMPLE.to_string();
    t.session_baseline = SAMPLE.to_string();
    t.saved_baseline = SAMPLE.to_string();
    app.tabs.push(t);
    app.active = 0;
    render_scene("toolbar_frameless", 1100.0, 200.0, app);
}

/// Split-view divider (Fix 2). With `grid_enabled` and two open notes, the grid
/// lays them side-by-side; the PNG should now show a thin theme-accent line down
/// the boundary BETWEEN the two panes (instead of the old empty 4 px gap), and
/// no line on the outer edges. Read the vertical seam at the window mid-line.
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn scene_split_divider() {
    let mut cfg = qa_config();
    cfg.editor.grid_enabled = true;
    let mut app = ScribeApp::new_test(cfg);
    app.tabs.clear();
    for (i, (name, body)) in [
        ("left.rs", "// left pane\n"),
        ("right.rs", "// right pane\n"),
    ]
    .iter()
    .enumerate()
    {
        let mut t = EditorTab::scratch();
        t.text = format!("{body}{SAMPLE}");
        t.session_baseline = t.text.clone();
        t.saved_baseline = t.text.clone();
        // Distinct doc ids so the grid lays out two separate panes (sync would
        // assign these anyway; setting them keeps the scene deterministic).
        t.doc_id = crate::grid::DocId(i as u64 + 1);
        let _ = name;
        app.tabs.push(t);
    }
    app.active = 0;
    render_scene("split_divider", 1100.0, 720.0, app);
}

/// Highlight-all-occurrences: a single-line buffer with the word `let`
/// repeated; inject a selection of the FIRST `let` so the other two get the
/// occurrence box. Selection is set on the egui TextEditState between frames.
#[test]
#[ignore = "GPU render"]
fn scene_highlight_occurrences() {
    if !gpu_available() {
        eprintln!("[visual-qa] no GPU adapter; skipping `highlight_occurrences`");
        return;
    }
    let mut app = ScribeApp::new_test(qa_config());
    app.tabs.clear();
    let mut t = EditorTab::scratch();
    t.text = "let aaa = 1; let bbb = 2; let ccc = 3;\n".to_string();
    t.session_baseline = t.text.clone();
    t.saved_baseline = t.text.clone();
    app.tabs.push(t);
    app.active = 0;

    // Inject a selection of the first `let` (chars 0..3) on frame 2 — once the
    // editor's TextEditState exists — from inside the frame closure (where the
    // egui Context is in scope; the Harness exposes no ctx accessor).
    let mut frame = 0u32;
    let mut harness: Harness<'static, ScribeApp> = Harness::builder()
        .with_size(egui::vec2(1100.0, 300.0))
        .wgpu()
        .build_state(
            move |ctx, app: &mut ScribeApp| {
                app.frame_tick(ctx);
                frame += 1;
                if frame == 1 {
                    // The editor's state is keyed on the SALTED id — `.with(doc_id)`,
                    // added so a selection in one note stops leaking into every other
                    // note. This scene still used the bare id, so `load_state` returned
                    // None, the `if let` swallowed it, and the selection was NEVER
                    // injected: the scene named `highlight_occurrences` has been
                    // rendering a golden with no selection and no occurrence boxes —
                    // it never exercised the feature it exists for.
                    //
                    // `request_focus` sat OUTSIDE that `if let`, so it fired anyway and
                    // focused an id no widget owns. That leaves the AccessKit tree
                    // naming a focused node absent from the node list, which is exactly
                    // the panic this scene hit the first time it was ever run.
                    //
                    // `expect` rather than `if let`: a silent skip is what hid this.
                    let id =
                        egui::Id::new("scr1b3-central-editor").with(app.tabs[app.active].doc_id);
                    let mut st = egui::TextEdit::load_state(ctx, id).expect(
                        "the central editor must have stored a TextEditState by frame 1 \
                         — if this is None the id has drifted again",
                    );
                    st.cursor.set_char_range(Some(egui::text::CCursorRange {
                        primary: egui::text::CCursor::new(3),
                        secondary: egui::text::CCursor::new(0),
                        h_pos: None,
                    }));
                    st.store(ctx, id);
                    ctx.memory_mut(|m| m.request_focus(id));
                }
            },
            app,
        );
    for _ in 0..5 {
        harness.step();
    }
    let img = harness.render().expect("wgpu render");
    let path = out_dir().join("highlight_occurrences.png");
    img.save(&path).expect("save png");
    eprintln!(
        "[visual-qa] wrote {} ({}x{})",
        path.display(),
        img.width(),
        img.height()
    );
}

/// Spell-check scoping on a REAL `.rs` file: language_hint = "rust" →
/// `SpellScope` restricts the check to comments/strings (check_identifiers
/// defaults false), so keywords (`fn`/`let`/`println`) must NOT be squiggled —
/// only the typos in the comment + the string. (The earlier scratch-tab QA
/// scene had no extension → whole-text fallback → keywords got flagged; this
/// scene proves real-file behavior.)
#[test]
#[ignore = "GPU render"]
fn scene_spellcheck_code() {
    use std::io::Write as _;
    let mut f = tempfile::Builder::new()
        .suffix(".rs")
        .tempfile()
        .expect("temp .rs");
    write!(
        f,
        "fn main() {{\n    // ths sentnce has speling typoz\n    let x = 1;\n    println!(\"helllo wrld\");\n}}\n"
    )
    .unwrap();
    let path = f.path().to_path_buf();
    let mut app = ScribeApp::new_test(qa_config());
    app.tabs.clear();
    let tab = EditorTab::from_path(path).expect("open temp .rs");
    app.tabs.push(tab);
    app.active = 0;
    render_scene("spellcheck_code", 1100.0, 400.0, app);
    drop(f); // keep the temp file alive until after the render
}

/// Minimap viewport-indicator accuracy: a LONG document scrolled toward the
/// bottom. The highlight box must overlay the minimap rows whose text equals the
/// editor's visible lines (the fix: content + indicator share one fit-to-height
/// scale). Renders the real frame so the alignment is in the PNG for inspection.
///
/// Each line is numbered so the visible band in the editor can be read off and
/// cross-checked against the highlighted minimap region. Drives `pending_scroll`
/// from inside the frame so the editor scrolls to ~70% before the capture frame.
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn scene_minimap_scrolled() {
    if !gpu_available() {
        eprintln!("[visual-qa] no GPU adapter; skipping `minimap_scrolled`");
        return;
    }
    let mut cfg = qa_config();
    cfg.editor.show_minimap = true;
    cfg.editor.word_wrap = false; // exercise the no-wrap P2 mapping path
    let mut app = ScribeApp::new_test(cfg);
    app.tabs.clear();
    let mut t = EditorTab::scratch();
    // 400 distinctly-numbered lines so the visible band is legible in the PNG.
    let mut body = String::new();
    for i in 1..=400 {
        body.push_str(&format!(
            "line {i:03}  fn item_{i:03}() {{ /* row {i:03} */ }}\n"
        ));
    }
    t.text = body.clone();
    t.session_baseline = body.clone();
    t.saved_baseline = body;
    app.tabs.push(t);
    app.active = 0;

    let mut frame = 0u32;
    let mut harness: Harness<'static, ScribeApp> = Harness::builder()
        .with_size(egui::vec2(1100.0, 720.0))
        .wgpu()
        .build_state(
            move |ctx, app: &mut ScribeApp| {
                app.frame_tick(ctx);
                frame += 1;
                // Once the editor has reported its real content height, scroll to
                // ~70% of the scrollable range and hold there for the capture.
                if frame >= 2 {
                    let (_off, content_h, view_h) = app.scroll_metrics;
                    let max_off = (content_h - view_h).max(0.0);
                    app.pending_scroll = Some(max_off * 0.7);
                }
            },
            app,
        );
    for _ in 0..6 {
        harness.step();
    }
    let img = harness.render().expect("wgpu render");
    let path = out_dir().join("minimap_scrolled.png");
    img.save(&path).expect("save png");
    eprintln!(
        "[visual-qa] wrote {} ({}x{})",
        path.display(),
        img.width(),
        img.height()
    );
}

/// Tint OFF baseline — a note with visible body text, no window tint. Read this
/// alongside `tint_strong_red` below: the background surfaces (titlebar,
/// toolbar, gutter, status bar) are the theme's dark chrome and the body text is
/// the theme foreground. It is the BEFORE frame for the tint bug fix.
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn scene_tint_off() {
    let mut cfg = qa_config();
    cfg.window.tint = "#ff0000".to_string();
    cfg.window.tint_strength = 0.0; // OFF
    let mut app = ScribeApp::new_test(cfg);
    app.tabs.clear();
    let mut t = EditorTab::scratch();
    t.text = SAMPLE.to_string();
    t.session_baseline = SAMPLE.to_string();
    t.saved_baseline = SAMPLE.to_string();
    app.tabs.push(t);
    app.active = 0;
    render_scene("tint_off", 1100.0, 720.0, app);
}

/// Tint ON, STRONG red (#ff0000 @ 0.8) — the AFTER frame. The fix blends the
/// tint into the BACKGROUND fill colours (`panel_fill` chrome), so the chrome
/// surfaces shift clearly toward red while the body-text glyphs keep their
/// original theme foreground hue (the tint never touches glyph colours). Read
/// this next to `tint_off`: chrome background = clearly red-shifted; the code
/// glyphs (`fn`, `let`, `println!`, identifiers) = unchanged hue.
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn scene_tint_strong_red() {
    let mut cfg = qa_config();
    cfg.window.tint = "#ff0000".to_string();
    cfg.window.tint_strength = 0.8; // STRONG
    let mut app = ScribeApp::new_test(cfg);
    app.tabs.clear();
    let mut t = EditorTab::scratch();
    t.text = SAMPLE.to_string();
    t.session_baseline = SAMPLE.to_string();
    t.saved_baseline = SAMPLE.to_string();
    app.tabs.push(t);
    app.active = 0;
    render_scene("tint_strong_red", 1100.0, 720.0, app);
}

/// Same strong-red tint but the `Enable window tint` toggle is OFF — proves the
/// master switch removes the tint from the main window even with a colour +
/// strength set. Read the PNG: the app background must be the plain (dark) theme
/// colour, not red.
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn scene_tint_disabled() {
    let mut cfg = qa_config();
    cfg.window.tint = "#ff0000".to_string();
    cfg.window.tint_strength = 0.8;
    cfg.window.tint_enabled = false; // toggle OFF
    let mut app = ScribeApp::new_test(cfg);
    app.tabs.clear();
    let mut t = EditorTab::scratch();
    t.text = SAMPLE.to_string();
    t.session_baseline = SAMPLE.to_string();
    t.saved_baseline = SAMPLE.to_string();
    app.tabs.push(t);
    app.active = 0;
    render_scene("tint_disabled", 1100.0, 720.0, app);
}

/// DIAGNOSTIC — strong tint with the SETTINGS WINDOW OPEN (opaque mode). The
/// bug report: the tint appears on the Settings popup but NOT the main app.
/// Read the PNG: the main app chrome + editor well must be tinted AND the
/// Settings window must NOT be tinted.
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn scene_tint_settings_open() {
    let mut cfg = qa_config();
    cfg.window.tint = "#ff0000".to_string();
    cfg.window.tint_strength = 0.8;
    let mut app = ScribeApp::new_test(cfg);
    app.tabs.clear();
    let mut t = EditorTab::scratch();
    t.text = SAMPLE.to_string();
    t.session_baseline = SAMPLE.to_string();
    t.saved_baseline = SAMPLE.to_string();
    app.tabs.push(t);
    app.active = 0;
    app.settings_open = true;
    render_scene("tint_settings_open", 1100.0, 720.0, app);
}

/// DIAGNOSTIC — strong tint in GLASS (translucent) mode. Reproduces the mode
/// where the tinted panel fill is composited at reduced opacity, to check
/// whether the main-app tint washes out vs the opaque Settings window.
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn scene_tint_glass_settings_open() {
    let mut cfg = qa_config();
    cfg.window.tint = "#ff0000".to_string();
    cfg.window.tint_strength = 0.8;
    cfg.window.transparency_enabled = true;
    cfg.window.opacity = 0.5;
    let mut app = ScribeApp::new_test(cfg);
    app.tabs.clear();
    let mut t = EditorTab::scratch();
    t.text = SAMPLE.to_string();
    t.session_baseline = SAMPLE.to_string();
    t.saved_baseline = SAMPLE.to_string();
    app.tabs.push(t);
    app.active = 0;
    app.settings_open = true;
    render_scene("tint_glass_settings_open", 1100.0, 720.0, app);
}

/// ISSUE-1 verify — transparency ON + tint ON + strong red @ 0.8 opacity. The
/// tint MUST visibly colour the translucent window chrome/editor-well (the
/// panels carry the tinted RGB and the opacity alpha), while the body-text
/// glyphs keep their untinted theme hue. Read the PNG next to `tint_off`: the
/// (semi-transparent) chrome background is clearly red-shifted.
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn scene_transparent_tinted() {
    let mut cfg = qa_config();
    cfg.window.transparency_enabled = true;
    cfg.window.tint_enabled = true;
    cfg.window.tint = "#ff0000".to_string();
    cfg.window.tint_strength = 0.8;
    cfg.window.opacity = 0.8;
    let mut app = ScribeApp::new_test(cfg);
    app.tabs.clear();
    let mut t = EditorTab::scratch();
    t.text = SAMPLE.to_string();
    t.session_baseline = SAMPLE.to_string();
    t.saved_baseline = SAMPLE.to_string();
    app.tabs.push(t);
    app.active = 0;
    render_scene("transparent_tinted", 1100.0, 720.0, app);
}

/// ISSUE-2 verify — transparency ON at the LOWEST opacity (0.05, near the 0.0
/// floor). Every background surface (chrome panels, editor well, and the
/// resting/weak widget fills) must be near-fully see-through so the window is
/// maximally transparent; only the editor glyphs + interactive controls stay
/// legible. Read the PNG's alpha channel: the background is ~13/255 or lower.
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn scene_transparent_min_opacity() {
    let mut cfg = qa_config();
    cfg.window.transparency_enabled = true;
    cfg.window.opacity = 0.05;
    let mut app = ScribeApp::new_test(cfg);
    app.tabs.clear();
    let mut t = EditorTab::scratch();
    t.text = SAMPLE.to_string();
    t.session_baseline = SAMPLE.to_string();
    t.saved_baseline = SAMPLE.to_string();
    app.tabs.push(t);
    app.active = 0;
    render_scene("transparent_min_opacity", 1100.0, 720.0, app);
}

/// #82 — rotate-ON side tabs MID-DRAG: the drop-insertion hairline must sit in
/// the GAP between two stacked tab chips, never inside a chip's outline. Forces
/// the drag pointer into the gap between chip 0 and chip 1 via the test hook,
/// then renders the REAL frame so the indicator is in the PNG for visual QA.
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn scene_rotated_sidetab_drop_indicator() {
    use super::tab_strip_render::{TEST_FORCE_SIDE_TAB_DRAG, TEST_ROTATED_TAB_RECTS};

    fn rotated_app() -> ScribeApp {
        let mut cfg = qa_config();
        cfg.appearance.frameless = false;
        cfg.editor.tab_bar_position = scribe_core::config::TabBarPosition::Left;
        cfg.editor.side_tabs_rotated = true;
        let mut app = ScribeApp::new_test(cfg);
        app.tabs.clear();
        for i in 0..3 {
            let mut t = EditorTab::scratch();
            t.text = format!("document {i}\nbody line\n");
            app.tabs.push(t);
        }
        // Active = the BOTTOM tab so the chip-0/chip-1 gap (where the drop line
        // paints) is flanked by MUTED tabs — the accent drop hairline can't be
        // confused with the active tab's accent outline.
        app.active = 2;
        app
    }

    const W: f32 = 900.0;
    const H: f32 = 600.0;

    // Phase 1 (CPU): render to capture the chip rects; aim the forced pointer at
    // the gap between chip 0 and chip 1.
    TEST_FORCE_SIDE_TAB_DRAG.with(|c| c.set(None));
    TEST_ROTATED_TAB_RECTS.with(|r| r.borrow_mut().clear());
    {
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::vec2(W, H))
            .build_state(
                |ctx, app: &mut ScribeApp| app.frame_tick(ctx),
                rotated_app(),
            );
        h.run();
        h.run();
    }
    let rects = TEST_ROTATED_TAB_RECTS.with(|r| r.borrow().clone());
    assert!(
        rects.len() >= 2,
        "need >=2 rotated chips for the drop-indicator scene"
    );
    let pointer = egui::pos2(
        rects[0].center().x,
        (rects[0].center().y + rects[1].center().y) * 0.5,
    );
    TEST_FORCE_SIDE_TAB_DRAG.with(|c| c.set(Some(pointer)));

    // Phase 2 (GPU): render the real frame with the forced drag → the insertion
    // hairline paints in the chip-0/chip-1 gap; saved to PNG for inspection.
    let path = render_scene("rotated_sidetab_drop_indicator", W, H, rotated_app());
    TEST_FORCE_SIDE_TAB_DRAG.with(|c| c.set(None));
    if let Some(p) = path {
        eprintln!("[#82] rotated drop-indicator scene -> {}", p.display());
    }
}

// ---------------------------------------------------------------------------
// v0.4.58 note-tab-bar wave — every dock position + both side variants, so the
// four fixes can be verified in a PNG:
//   Fix 1/2: the "+" button is frameless-until-hover + centred (like a top-bar
//            button), NOT the old grey framed `small_button` slab.
//   Fix 3:   a non-selected tab shows a faint hover fill (hover scenes).
//   Fix 4:   a 1px theme-tinted divider separates adjacent tabs in EVERY
//            position — including left/right (both non-rotated and rotated).
//   Follow-up 1/2: horizontal side-bar titles ellipsise (shrink) / wrap to 2
//            lines (opt-in).
// ---------------------------------------------------------------------------

/// Build a note-tab-bar QA scene app: native (non-frameless) chrome, the given
/// dock position / rotation / 2-line option, and one real file tab per title
/// (real files so `title()` shows distinct names). `pinned` marks tab indices
/// pinned; `active` selects the accented tab.
fn tabbar_scene_app(
    position: scribe_core::config::TabBarPosition,
    rotated: bool,
    two_line: bool,
    titles: &[&str],
    active: usize,
    pinned: &[usize],
) -> ScribeApp {
    let mut cfg = qa_config();
    cfg.appearance.frameless = false;
    cfg.editor.tab_bar_position = position;
    cfg.editor.side_tabs_rotated = rotated;
    cfg.editor.side_tabs_wrap_two_lines = two_line;
    let mut app = ScribeApp::new_test(cfg);
    app.tabs.clear();
    for (i, name) in titles.iter().enumerate() {
        let path = out_dir().join(name);
        std::fs::write(&path, format!("// {name}\ncontent line\n")).expect("write scene file");
        let mut t = EditorTab::from_path(path).expect("open scene file");
        t.doc_id = crate::grid::DocId(i as u64 + 1);
        if pinned.contains(&i) {
            t.pinned = true;
        }
        app.tabs.push(t);
    }
    app.active = active.min(app.tabs.len().saturating_sub(1));
    app
}

/// Like [`render_scene`] but injects a pointer hover at `hover` before the final
/// capture, so a hover-only affordance (Fix 3's non-selected tab highlight)
/// appears in the PNG.
fn render_scene_hover(
    name: &str,
    w: f32,
    h: f32,
    app: ScribeApp,
    hover: egui::Pos2,
) -> Option<std::path::PathBuf> {
    if !gpu_available() {
        eprintln!("[visual-qa] no GPU adapter; skipping `{name}`");
        return None;
    }
    let mut harness: Harness<'static, ScribeApp> = Harness::builder()
        .with_size(egui::vec2(w, h))
        .wgpu()
        .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app);
    for _ in 0..5 {
        harness.step();
    }
    harness.hover_at(hover);
    for _ in 0..3 {
        harness.step();
    }
    let img = harness.render().expect("kittest wgpu render must succeed");
    let path = out_dir().join(format!("{name}.png"));
    img.save(&path).expect("save visual-qa png");
    eprintln!("[visual-qa] wrote {} (hover)", path.display());
    Some(path)
}

const TABBAR_TITLES: &[&str] = &["main.rs", "lib.rs", "notes.md", "config.toml"];

/// TOP dock — the four fixes' baseline: a horizontal strip with a frameless "+"
/// at the row end and vertical dividers between chips. Read: the "+" has NO grey
/// box at idle; thin lines sit between adjacent tabs.
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn scene_tabbar_top() {
    let app = tabbar_scene_app(
        scribe_core::config::TabBarPosition::Top,
        false,
        false,
        TABBAR_TITLES,
        1,
        &[0],
    );
    render_scene("tabbar_top", 1000.0, 240.0, app);
}

/// BOTTOM dock — same horizontal strip, docked above the status bar.
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn scene_tabbar_bottom() {
    let app = tabbar_scene_app(
        scribe_core::config::TabBarPosition::Bottom,
        false,
        false,
        TABBAR_TITLES,
        1,
        &[0],
    );
    render_scene("tabbar_bottom", 1000.0, 300.0, app);
}

/// LEFT dock, HORIZONTAL labels (non-rotated). Read: HORIZONTAL dividers between
/// stacked tabs (the left/right-divider fix), the active tab accented, a pinned
/// tab (dimmed grip), and the frameless centred "+" below the column.
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn scene_tabbar_left_horizontal() {
    let app = tabbar_scene_app(
        scribe_core::config::TabBarPosition::Left,
        false,
        false,
        TABBAR_TITLES,
        1,
        &[0],
    );
    render_scene("tabbar_left_horizontal", 900.0, 560.0, app);
}

/// RIGHT dock, HORIZONTAL labels — mirror of the left scene; confirms the
/// dividers + frameless "+" also render on the right edge.
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn scene_tabbar_right_horizontal() {
    let app = tabbar_scene_app(
        scribe_core::config::TabBarPosition::Right,
        false,
        false,
        TABBAR_TITLES,
        1,
        &[0],
    );
    render_scene("tabbar_right_horizontal", 900.0, 560.0, app);
}

/// LEFT dock, ROTATED (vertical-text) variant. Read: HORIZONTAL dividers between
/// the stacked rotated chips (Fix 4 for the rotated variant) and the frameless
/// centred "+" at the column foot.
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn scene_tabbar_left_rotated() {
    let app = tabbar_scene_app(
        scribe_core::config::TabBarPosition::Left,
        true,
        false,
        TABBAR_TITLES,
        1,
        &[0],
    );
    render_scene("tabbar_left_rotated", 900.0, 560.0, app);
}

/// Fix 3 hover — LEFT horizontal bar with the pointer over the (non-selected)
/// SECOND tab. Read: that tab carries a faint hover fill (lighter than the
/// active tab's accent), painted BEHIND its label text. Hover coordinate targets
/// the 2nd row of the left column.
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn scene_tabbar_left_hover() {
    let app = tabbar_scene_app(
        scribe_core::config::TabBarPosition::Left,
        false,
        false,
        TABBAR_TITLES,
        0,
        &[],
    );
    // The left column starts just below the top toolbar; the 2nd tab sits a bit
    // lower. (Verified against the rendered PNG.)
    render_scene_hover(
        "tabbar_left_hover",
        900.0,
        560.0,
        app,
        egui::pos2(60.0, 92.0),
    );
}

/// Follow-up 1 — a LONG title on a LEFT horizontal bar. The panel opens at its
/// clamped fit width, but a title wider than that ELLIPSISES on one line
/// ("a-very-long-…"). This proves the truncating galley renders; the shrink
/// interaction is pinned by the `tabbar_resize_tests` interaction test.
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn scene_tabbar_left_narrow_ellipsis() {
    let titles = &[
        "a-very-long-note-title-that-overflows-the-bar.md",
        "short.md",
        "another-fairly-long-filename-here.rs",
    ];
    let app = tabbar_scene_app(
        scribe_core::config::TabBarPosition::Left,
        false,
        false,
        titles,
        1,
        &[],
    );
    render_scene("tabbar_left_narrow_ellipsis", 900.0, 480.0, app);
}

/// Follow-up 2 — the SAME long titles with the "Wrap note titles to 2 lines"
/// option ON. Read: a title too long for one line now WRAPS to a second line
/// (chip grows taller); a title too long for even two lines elides the 2nd row.
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn scene_tabbar_left_two_lines() {
    let titles = &[
        "a-very-long-note-title-that-overflows-the-bar.md",
        "short.md",
        "another-fairly-long-filename-here.rs",
    ];
    let app = tabbar_scene_app(
        scribe_core::config::TabBarPosition::Left,
        false,
        true, // 2-line wrap ON
        titles,
        1,
        &[],
    );
    render_scene("tabbar_left_two_lines", 900.0, 480.0, app);
}

// ═══════════════════════════════════════════════════════════════════════════
// Inline LSP diagnostics — the overlay that had never been LOOKED at.
//
// `diagnostics_overlay.rs` unit-tests the placement rules against the covered
// TEXT, and `lsp_and_preview_wiring_tests` proves a frame carrying diagnostics
// does not panic. Neither of those can see a squiggle painted under the WRONG
// span, on the wrong row, or in the wrong colour — a paint bug there produces a
// perfectly green suite and a visibly wrong editor.
//
// So these scenes render the REAL frame and then MEASURE it. The measurement is
// differential: every scene renders twice, once with the diagnostics and once
// with an otherwise identical app carrying NONE, and classifies the difference.
// The overlay composites `alpha * severity_colour` over exactly the control
// pixel, so the added colour vector at an inked pixel is PARALLEL to
// `severity_colour - control_pixel` — which makes "is this pixel error-red ink"
// a decidable question rather than a colour-distance guess, and makes it
// impossible for existing chrome (or the spell squiggle, which happens to be the
// SAME red) to be counted as diagnostic ink.
//
// Geometry is likewise never hard-coded: a CALIBRATION render underlines one
// whole known-length line, and the character-cell width plus the text origin are
// read back off that render. Every expected x is then `origin + column * width`
// in PRODUCTION geometry — if the editor font, padding, or gutter width changes,
// the calibration moves with it and the assertions stay true.
// ═══════════════════════════════════════════════════════════════════════════

use image::RgbaImage;

/// Four lines of EXACTLY 32 characters, so a column index maps to an x offset by
/// a single multiply and a mis-placed squiggle is arithmetic, not opinion.
const DIAG_SRC: &str = "0123456789abcdefghijklmnopqrstuv\n\
                        second line holds the error span\n\
                        third line holds a warning token\n\
                        fourth line holds the info notic\n";
const DIAG_LINE_LEN: u32 = 32;

/// `error` occupies columns 22..27 of line 1 of [`DIAG_SRC`].
const ERR_COLS: (u32, u32) = (22, 27);
/// `warning` occupies columns 19..26 of line 2.
const WARN_COLS: (u32, u32) = (19, 26);
/// `info` occupies columns 22..26 of line 3.
const INFO_COLS: (u32, u32) = (22, 26);

fn diag(line: u32, ch: u32, end_line: u32, end_ch: u32, severity: u8, message: &str) -> Diagnostic {
    Diagnostic {
        uri: "file:///qa-diagnostics.txt".into(),
        line,
        character: ch,
        end_line,
        end_character: end_ch,
        severity,
        message: message.into(),
    }
}

/// The three diagnostics the inline scene publishes, one per severity, each
/// covering a word whose columns are known.
fn inline_diags() -> Vec<Diagnostic> {
    vec![
        diag(
            1,
            ERR_COLS.0,
            1,
            ERR_COLS.1,
            super::diagnostics_overlay::SEVERITY_ERROR,
            "cannot find value `error` in this scope",
        ),
        diag(
            2,
            WARN_COLS.0,
            2,
            WARN_COLS.1,
            super::diagnostics_overlay::SEVERITY_WARNING,
            "unused variable: `warning`",
        ),
        diag(
            3,
            INFO_COLS.0,
            3,
            INFO_COLS.1,
            super::diagnostics_overlay::SEVERITY_INFO,
            "consider naming this",
        ),
    ]
}

/// Config for the diagnostic scenes.
///
/// Spellcheck is OFF deliberately: its squiggle is painted in the SAME
/// `#e53e3e` as the default error colour, so leaving it on would put
/// error-coloured ink in the CONTROL frame and hollow out the zero-ink
/// assertion. The minimap is OFF so the editor width — and therefore the soft
/// wrap point — depends only on the window size.
fn diag_config(word_wrap: bool) -> Config {
    let mut cfg = qa_config();
    cfg.spellcheck.enabled = false;
    cfg.editor.show_minimap = false;
    cfg.editor.word_wrap = word_wrap;
    cfg
}

/// A scratch tab holding `text` with `diags` published against it. Both
/// baselines match the text so the change bar (amber — the SAME colour as a
/// warning) never paints and cannot be counted as diagnostic ink.
fn diag_app(text: &str, diags: Vec<Diagnostic>, word_wrap: bool) -> ScribeApp {
    let mut app = ScribeApp::new_test(diag_config(word_wrap));
    app.tabs.clear();
    let mut t = EditorTab::scratch();
    t.text = text.to_string();
    t.session_baseline = text.to_string();
    t.saved_baseline = text.to_string();
    app.tabs.push(t);
    app.active = 0;
    app.diagnostics = diags;
    app
}

/// The severity colours the overlay actually resolves, read through the same
/// theme lookup `frame_tick` uses — NOT re-declared hex constants, which would
/// silently desync from the app the moment a theme defined an `error` key.
fn severity_colors(app: &ScribeApp) -> [Color32; 3] {
    [
        ui_color(&app.theme, "error", Rgba::new(0xe5, 0x3e, 0x3e, 255)),
        ui_color(&app.theme, "warning", Rgba::new(0xf2, 0xb3, 0x3d, 255)),
        ui_color(&app.theme, "accent", Rgba::new(0x6f, 0xb8, 0x9a, 255)),
    ]
}

/// Render a frame and also hand back the pixels. Saves the PNG so it can be
/// READ — the whole point of the exercise.
fn render_frame(name: &str, w: f32, h: f32, app: ScribeApp) -> Option<RgbaImage> {
    if !gpu_available() {
        eprintln!("[visual-qa] no GPU adapter; skipping `{name}` (NOT a pass)");
        return None;
    }
    let mut harness: Harness<'static, ScribeApp> = Harness::builder()
        .with_size(egui::vec2(w, h))
        .wgpu()
        .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app);
    for _ in 0..5 {
        harness.step();
    }
    let img = harness
        .render()
        .expect("kittest wgpu render of the real ScribeApp frame must succeed");
    let path = out_dir().join(format!("{name}.png"));
    img.save(&path).expect("save visual-qa png");
    eprintln!(
        "[visual-qa] wrote {} ({}x{})",
        path.display(),
        img.width(),
        img.height()
    );
    Some(img)
}

/// Rows to consider when hunting for ink: everything between the tab strip and
/// the status bar. The status bar legitimately turns the diagnostic COUNT amber
/// when errors are present — that is chrome, not overlay.
fn analysis_rows(img: &RgbaImage) -> std::ops::Range<u32> {
    60..img.height().saturating_sub(60)
}

/// Least-squares fit of the per-pixel difference onto `target - control`.
///
/// Returns `(alpha, residual)`. An alpha-composited overlay gives residual ~0
/// with alpha in `0..=1`; anything else (a glyph that moved, another overlay's
/// colour) leaves a large residual.
fn ink_fit(shot: &RgbaImage, control: &RgbaImage, x: u32, y: u32, target: Color32) -> (f32, f32) {
    let a = shot.get_pixel(x, y).0;
    let b = control.get_pixel(x, y).0;
    let d = [
        f32::from(a[0]) - f32::from(b[0]),
        f32::from(a[1]) - f32::from(b[1]),
        f32::from(a[2]) - f32::from(b[2]),
    ];
    let dir = [
        f32::from(target.r()) - f32::from(b[0]),
        f32::from(target.g()) - f32::from(b[1]),
        f32::from(target.b()) - f32::from(b[2]),
    ];
    let dd = dir[0].mul_add(dir[0], dir[1].mul_add(dir[1], dir[2] * dir[2]));
    if dd < 400.0 {
        // Target and background are the same colour here: undecidable.
        return (0.0, f32::MAX);
    }
    let alpha = d[0].mul_add(dir[0], d[1].mul_add(dir[1], d[2] * dir[2])) / dd;
    let resid = (alpha.mul_add(-dir[0], d[0]).powi(2)
        + alpha.mul_add(-dir[1], d[1]).powi(2)
        + alpha.mul_add(-dir[2], d[2]).powi(2))
    .sqrt();
    (alpha, resid)
}

/// Smallest per-channel change that counts as ink at all (below this it is
/// GPU/driver dither, not paint).
const MIN_INK_DELTA: f32 = 20.0;
/// Faintest alpha accepted as deliberate ink.
const MIN_INK_ALPHA: f32 = 0.30;
/// Largest off-axis residual an alpha-composited pixel may carry.
const MAX_INK_RESID: f32 = 12.0;

fn changed_enough(shot: &RgbaImage, control: &RgbaImage, x: u32, y: u32) -> bool {
    let a = shot.get_pixel(x, y).0;
    let b = control.get_pixel(x, y).0;
    (0..3)
        .map(|c| (f32::from(a[c]) - f32::from(b[c])).abs())
        .fold(0.0f32, f32::max)
        >= MIN_INK_DELTA
}

/// Every pixel in the analysis band that reads as ink of `targets[which]` AND
/// reads better as that severity than as either of the other two. The
/// best-of-three rule is what makes "the error squiggle is red, not amber" a
/// real assertion rather than a colour-distance coincidence.
fn ink_pixels(
    shot: &RgbaImage,
    control: &RgbaImage,
    targets: &[Color32; 3],
    which: usize,
) -> Vec<(u32, u32)> {
    assert_eq!(
        shot.dimensions(),
        control.dimensions(),
        "the shot and its control must be the same size"
    );
    let mut out = Vec::new();
    for y in analysis_rows(shot) {
        for x in 0..shot.width() {
            if !changed_enough(shot, control, x, y) {
                continue;
            }
            let fits: Vec<(f32, f32)> = targets
                .iter()
                .map(|t| ink_fit(shot, control, x, y, *t))
                .collect();
            let (alpha, resid) = fits[which];
            if !(MIN_INK_ALPHA..=1.30).contains(&alpha) || resid > MAX_INK_RESID {
                continue;
            }
            let best_other = fits
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != which)
                .map(|(_, (_, r))| *r)
                .fold(f32::MAX, f32::min);
            if best_other <= resid.mul_add(2.0, 4.0) {
                continue; // not decisively THIS severity
            }
            out.push((x, y));
        }
    }
    out
}

/// Group ink into horizontal bands (one per painted galley row). A gap of more
/// than `gap` blank scanlines starts a new band.
fn ink_bands(pixels: &[(u32, u32)], gap: u32) -> Vec<(u32, u32)> {
    let mut ys: Vec<u32> = pixels.iter().map(|(_, y)| *y).collect();
    ys.sort_unstable();
    ys.dedup();
    let mut bands: Vec<(u32, u32)> = Vec::new();
    for y in ys {
        match bands.last_mut() {
            Some(b) if y <= b.1 + gap => b.1 = y,
            _ => bands.push((y, y)),
        }
    }
    bands
}

/// The x extent of the ink inside one band, restricted to `x_range`.
fn band_x_extent(
    pixels: &[(u32, u32)],
    band: (u32, u32),
    x_range: &std::ops::Range<u32>,
) -> Option<(u32, u32)> {
    let xs: Vec<u32> = pixels
        .iter()
        .filter(|(x, y)| *y >= band.0 && *y <= band.1 && x_range.contains(x))
        .map(|(x, _)| *x)
        .collect();
    Some((*xs.iter().min()?, *xs.iter().max()?))
}

/// First column of the editor TEXT, split off the gutter.
///
/// A diagnostic inks two well-separated things: a short bar at the gutter's left
/// edge and the squiggle in the text area. The widest horizontal gap between
/// inked columns is the gutter-to-text boundary, so the text origin is measured,
/// not assumed — and forgetting to exclude the gutter (which is exactly how the
/// first draft of the soft-wrap scene mis-measured the character cell by 50%) is
/// no longer possible by construction.
fn text_origin(ink: &[(u32, u32)]) -> u32 {
    let mut xs: Vec<u32> = ink.iter().map(|(x, _)| *x).collect();
    assert!(!xs.is_empty(), "no ink to split");
    xs.sort_unstable();
    xs.dedup();
    let (mut split, mut best_gap) = (xs[0], 0u32);
    for pair in xs.windows(2) {
        if pair[1] - pair[0] > best_gap {
            best_gap = pair[1] - pair[0];
            split = pair[1];
        }
    }
    assert!(
        best_gap > 10,
        "expected a clear horizontal gap between the gutter mark and the \
         squiggle; the largest gap between inked columns was {best_gap}px \
         (is the gutter mark missing?)"
    );
    split
}

/// Text-origin x and character-cell width, READ OFF a render in which one whole
/// known-length line is underlined. Everything downstream is expressed in these
/// production-measured units, never in constants copied out of the paint code.
#[derive(Debug, Clone, Copy)]
struct CellGeometry {
    /// x of the first character cell's left edge (as the squiggle paints it).
    origin_x: f32,
    /// Width of one monospace character cell, in pixels.
    cell_w: f32,
}

impl CellGeometry {
    fn x_of(self, column: u32) -> f32 {
        f32::from(u16::try_from(column).expect("column fits")).mul_add(self.cell_w, self.origin_x)
    }
}

/// Screen columns of the gutter — everything left of the text origin.
fn gutter_range(geom: CellGeometry) -> std::ops::Range<u32> {
    0..(geom.origin_x as u32).saturating_sub(4)
}

/// Screen columns of the editor text area.
fn text_range(geom: CellGeometry, width: u32) -> std::ops::Range<u32> {
    (geom.origin_x as u32).saturating_sub(3)..width
}

const SCENE_W: f32 = 900.0;
const SCENE_H: f32 = 400.0;

/// Render the control frame (no diagnostics) for [`DIAG_SRC`].
fn diag_control(name: &str, word_wrap: bool, text: &str) -> Option<RgbaImage> {
    render_frame(
        &format!("{name}_control"),
        SCENE_W,
        SCENE_H,
        diag_app(text, Vec::new(), word_wrap),
    )
}

/// Underline ALL of line 0 and read the character-cell geometry back off the
/// pixels. This is the production-geometry anchor for every x assertion below.
fn calibrate(control: &RgbaImage) -> Option<CellGeometry> {
    let app = diag_app(
        DIAG_SRC,
        vec![diag(
            0,
            0,
            0,
            DIAG_LINE_LEN,
            super::diagnostics_overlay::SEVERITY_ERROR,
            "whole first line",
        )],
        false,
    );
    let targets = severity_colors(&app);
    let shot = render_frame("diagnostics_calibration", SCENE_W, SCENE_H, app)?;
    let ink = ink_pixels(&shot, control, &targets, 0);
    assert!(
        !ink.is_empty(),
        "the calibration render underlined a whole line and produced NO \
         error-red ink — the overlay painted nothing at all"
    );
    // The gutter bar is ink too; split it off before measuring the cell.
    let split = text_origin(&ink);
    let max_x = ink.iter().map(|(x, _)| *x).max().expect("ink columns");
    let geom = CellGeometry {
        origin_x: f32::from(u16::try_from(split).expect("x fits")),
        cell_w: (max_x - split) as f32 / DIAG_LINE_LEN as f32,
    };
    eprintln!(
        "[diag-cal] gutter|text split={split} right={max_x} cell_w={:.3}px",
        geom.cell_w
    );
    Some(geom)
}

fn assert_close(what: &str, observed: f32, expected: f32, tol: f32) {
    assert!(
        (observed - expected).abs() <= tol,
        "{what}: observed {observed:.1}px, expected {expected:.1}px (tolerance {tol}px)"
    );
}

/// Tolerance on a single measured edge: the squiggle is antialiased, so its
/// inked extent spreads about a pixel beyond the geometric endpoint, and the
/// calibration carries the same spread on both sides.
const EDGE_TOL: f32 = 3.0;

// ─────────────────────────── the scenes ───────────────────────────

/// THE scene: one diagnostic per severity, each under a known word. Read the
/// PNG — three squiggles under `error`, `warning` and `info` on lines 2/3/4,
/// three coloured bars in the gutter, and NOTHING under the other words.
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn diag_scene_inline_overlay() {
    let Some(control) = diag_control("diagnostics_inline", false, DIAG_SRC) else {
        return;
    };
    let app = diag_app(DIAG_SRC, inline_diags(), false);
    let targets = severity_colors(&app);
    let Some(shot) = render_frame("diagnostics_inline", SCENE_W, SCENE_H, app) else {
        return;
    };
    for (i, name) in ["error", "warning", "info"].iter().enumerate() {
        let n = ink_pixels(&shot, &control, &targets, i).len();
        eprintln!("[diag-inline] {name} ink pixels: {n}");
        assert!(n > 0, "no {name}-coloured ink was painted anywhere");
    }
}

/// The single most important property, and the one a text-level unit test
/// cannot see: the squiggle covers the diagnostic's RANGE, not the whole line.
///
/// Every expected x comes from the calibration render (origin + column ×
/// cell-width), so this stays true across font, padding and gutter changes —
/// and fails loudly if the paint path ever underlines the full row again.
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn diag_squiggle_covers_the_range_not_the_whole_line() {
    let Some(control) = diag_control("diagnostics_range", false, DIAG_SRC) else {
        return;
    };
    let Some(geom) = calibrate(&control) else {
        return;
    };
    let app = diag_app(DIAG_SRC, inline_diags(), false);
    let targets = severity_colors(&app);
    let Some(shot) = render_frame("diagnostics_range", SCENE_W, SCENE_H, app) else {
        return;
    };
    let text = text_range(geom, shot.width());
    let full_line_w = geom.cell_w * DIAG_LINE_LEN as f32;

    for (i, (name, cols)) in [
        ("error", ERR_COLS),
        ("warning", WARN_COLS),
        ("info", INFO_COLS),
    ]
    .iter()
    .enumerate()
    {
        // Text area only: the gutter bar is ink too, and it sits on the same
        // rows, so counting it would blur "one underlined row" into "one
        // marked line".
        let ink: Vec<(u32, u32)> = ink_pixels(&shot, &control, &targets, i)
            .into_iter()
            .filter(|(x, _)| text.contains(x))
            .collect();
        let bands = ink_bands(&ink, 3);
        assert_eq!(
            bands.len(),
            1,
            "{name}: a single-line diagnostic must underline exactly one row, \
             got bands {bands:?}"
        );
        let (x0, x1) = band_x_extent(&ink, bands[0], &text)
            .unwrap_or_else(|| panic!("{name}: no ink in the text area"));
        eprintln!("[diag-range] {name} band={:?} x={x0}..{x1}", bands[0]);
        assert_close(
            &format!("{name} squiggle start (column {})", cols.0),
            x0 as f32,
            geom.x_of(cols.0),
            EDGE_TOL,
        );
        assert_close(
            &format!("{name} squiggle end (column {})", cols.1),
            x1 as f32,
            geom.x_of(cols.1),
            EDGE_TOL,
        );
        // Stated the other way round, so a regression that underlined the whole
        // row is named as such instead of as an off-by-N.
        let width = (x1 - x0) as f32;
        assert!(
            width < full_line_w * 0.5,
            "{name}: the squiggle is {width:.0}px wide out of a {full_line_w:.0}px \
             line — it is underlining the LINE, not the {}-character range",
            cols.1 - cols.0
        );
    }
}

/// The control render must contain ZERO diagnostic ink. Paired with the scene
/// above it makes every "there is ink here" assertion meaningful: the ink is
/// caused by the diagnostics and by nothing else on the frame.
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn diag_free_frame_paints_no_diagnostic_ink() {
    let Some(control) = diag_control("diagnostics_zero", false, DIAG_SRC) else {
        return;
    };
    // A second render of the SAME zero-diagnostic app. Differencing two control
    // frames must yield no ink at all — if it does, the classifier is picking up
    // render noise and every positive result below would be worthless.
    let Some(control2) = render_frame(
        "diagnostics_zero_b",
        SCENE_W,
        SCENE_H,
        diag_app(DIAG_SRC, Vec::new(), false),
    ) else {
        return;
    };
    let targets = severity_colors(&diag_app(DIAG_SRC, Vec::new(), false));
    for (i, name) in ["error", "warning", "info"].iter().enumerate() {
        let n = ink_pixels(&control2, &control, &targets, i).len();
        assert_eq!(
            n, 0,
            "two diagnostic-free renders differ by {n} {name}-coloured pixels; \
             the ink classifier is reading noise"
        );
    }

    // …and CONTAINMENT, which is the half that is not a tautology. Publishing
    // diagnostics on lines 2, 3 and 4 must change nothing on line 1 — not its
    // glyphs (no reflow), not its gutter (no mark on an undiagnosed line), not
    // the row where its own underline would sit. "Zero ink in the control" only
    // says the classifier is quiet; this says the overlay stays inside the lines
    // it was published for.
    let Some(shot) = render_frame(
        "diagnostics_zero_shot",
        SCENE_W,
        SCENE_H,
        diag_app(DIAG_SRC, inline_diags(), false),
    ) else {
        return;
    };
    // Row pitch measured from two DIAGNOSED rows, so line 1's band is derived
    // from the render rather than from the editor's line-height constant.
    let err = ink_bands(&ink_pixels(&shot, &control, &targets, 0), 3);
    let warn = ink_bands(&ink_pixels(&shot, &control, &targets, 1), 3);
    assert_eq!(err.len(), 1, "error ink bands: {err:?}");
    assert_eq!(warn.len(), 1, "warning ink bands: {warn:?}");
    let pitch = warn[0].0 - err[0].0;
    assert!(pitch > 4, "implausible row pitch {pitch}px");
    // One whole row above the first diagnosed row: line 1, top to underline.
    let y0 = err[0].1 - 2 * pitch + 2;
    let y1 = err[0].1 - pitch;
    let untouched = changed_pixels_in(
        &shot,
        &control,
        egui::Rect::from_min_max(
            egui::pos2(0.0, y0 as f32),
            egui::pos2(shot.width() as f32, y1 as f32),
        ),
    );
    eprintln!("[diag-zero] pitch={pitch} undiagnosed row y={y0}..{y1} changed={untouched}");
    assert_eq!(
        untouched, 0,
        "publishing diagnostics on lines 2-4 changed {untouched} pixels on the \
         UNDIAGNOSED line 1 (rows {y0}..{y1}, full width) — the overlay is \
         painting outside the lines it was given"
    );
}

/// Severities must be visually distinguishable, not three shades of the same
/// thing. Each band is classified against ALL THREE colours and must win as its
/// own severity — an overlay that painted every squiggle red passes a
/// "there is ink" check and fails this one.
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn diag_each_severity_paints_its_own_colour() {
    let Some(control) = diag_control("diagnostics_severity", false, DIAG_SRC) else {
        return;
    };
    let Some(geom) = calibrate(&control) else {
        return;
    };
    let app = diag_app(DIAG_SRC, inline_diags(), false);
    let targets = severity_colors(&app);
    let Some(shot) = render_frame("diagnostics_severity", SCENE_W, SCENE_H, app) else {
        return;
    };
    let text = text_range(geom, shot.width());
    let mut rows: Vec<(usize, u32)> = Vec::new();
    for i in 0..3 {
        let ink: Vec<(u32, u32)> = ink_pixels(&shot, &control, &targets, i)
            .into_iter()
            .filter(|(x, _)| text.contains(x))
            .collect();
        assert!(
            !ink.is_empty(),
            "severity {i} produced no ink that classifies decisively as its own \
             colour — the three severities are not visually distinguishable"
        );
        let bands = ink_bands(&ink, 3);
        assert_eq!(bands.len(), 1, "severity {i} bands: {bands:?}");
        rows.push((i, bands[0].0));
    }
    // Each severity sits on its own row, in source order (error line 1, warning
    // line 2, info line 3).
    assert!(
        rows[0].1 < rows[1].1 && rows[1].1 < rows[2].1,
        "the severities must land on descending source lines, got {rows:?}"
    );
}

/// The gutter bar marks the START line of a diagnostic and no other. A gutter
/// mark on the wrong line sends the user to the wrong place in the file, and the
/// unit test for `gutter_marks` cannot see which row it was actually drawn on.
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn diag_gutter_bar_marks_the_start_line_only() {
    let Some(control) = diag_control("diagnostics_gutter", false, DIAG_SRC) else {
        return;
    };
    let Some(geom) = calibrate(&control) else {
        return;
    };
    // One multi-line error, lines 1 → 3. The squiggle covers three rows; the
    // gutter must mark exactly ONE.
    let app = diag_app(
        DIAG_SRC,
        vec![diag(
            1,
            10,
            3,
            8,
            super::diagnostics_overlay::SEVERITY_ERROR,
            "unclosed delimiter",
        )],
        false,
    );
    let targets = severity_colors(&app);
    let Some(shot) = render_frame("diagnostics_gutter", SCENE_W, SCENE_H, app) else {
        return;
    };
    let ink = ink_pixels(&shot, &control, &targets, 0);
    let gutter = gutter_range(geom);
    let text = text_range(geom, shot.width());

    let gutter_ink: Vec<(u32, u32)> = ink
        .iter()
        .copied()
        .filter(|(x, _)| gutter.contains(x))
        .collect();
    let text_ink: Vec<(u32, u32)> = ink
        .iter()
        .copied()
        .filter(|(x, _)| text.contains(x))
        .collect();
    let gutter_bands = ink_bands(&gutter_ink, 3);
    let text_bands = ink_bands(&text_ink, 3);
    eprintln!("[diag-gutter] gutter bands={gutter_bands:?} text bands={text_bands:?}");
    assert_eq!(
        gutter_bands.len(),
        1,
        "a diagnostic spanning three lines must leave ONE gutter mark (on its \
         start line), got {gutter_bands:?}"
    );
    assert_eq!(
        text_bands.len(),
        3,
        "the same diagnostic must underline all THREE rows it covers, got \
         {text_bands:?}"
    );
    // …and that one mark is level with the FIRST underlined row, to well inside
    // a row height. Row height is measured from the spacing of the underlined
    // rows, so this does not encode the editor's line height either. The gutter
    // bar is centred on its row while the squiggle sits at the row's bottom, so
    // the expected offset between them is half a row — which makes a mark drawn
    // one line off (the classic gutter regression) a full row out of tolerance.
    let row_h = (text_bands[1].0 - text_bands[0].0) as f32;
    let gutter_mid = (gutter_bands[0].0 + gutter_bands[0].1) as f32 / 2.0;
    let squiggle_mid = (text_bands[0].0 + text_bands[0].1) as f32 / 2.0;
    assert_close(
        "gutter mark centre vs the first underlined row (half a row apart)",
        squiggle_mid - gutter_mid,
        row_h * 0.5,
        row_h * 0.35,
    );
}

/// A multi-line range underlines every row it covers, starting at the diagnostic
/// column on the first row, spanning the whole middle row, and stopping at the
/// diagnostic column on the last — the behaviour the per-row paint loop exists
/// for, and the one an "underline the span" implementation gets wrong by
/// dropping the diagnostic entirely.
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn diag_multi_line_range_underlines_each_row_from_the_right_column() {
    let Some(control) = diag_control("diagnostics_multiline", false, DIAG_SRC) else {
        return;
    };
    let Some(geom) = calibrate(&control) else {
        return;
    };
    const START_COL: u32 = 10;
    const END_COL: u32 = 8;
    let app = diag_app(
        DIAG_SRC,
        vec![diag(
            1,
            START_COL,
            3,
            END_COL,
            super::diagnostics_overlay::SEVERITY_ERROR,
            "unclosed delimiter",
        )],
        false,
    );
    let targets = severity_colors(&app);
    let Some(shot) = render_frame("diagnostics_multiline", SCENE_W, SCENE_H, app) else {
        return;
    };
    let text = text_range(geom, shot.width());
    let ink: Vec<(u32, u32)> = ink_pixels(&shot, &control, &targets, 0)
        .into_iter()
        .filter(|(x, _)| text.contains(x))
        .collect();
    let bands = ink_bands(&ink, 3);
    assert_eq!(
        bands.len(),
        3,
        "expected three underlined rows, got {bands:?}"
    );

    let e0 = band_x_extent(&ink, bands[0], &text).expect("row 0 ink");
    let e1 = band_x_extent(&ink, bands[1], &text).expect("row 1 ink");
    let e2 = band_x_extent(&ink, bands[2], &text).expect("row 2 ink");
    eprintln!("[diag-multiline] rows: {e0:?} {e1:?} {e2:?}");

    assert_close(
        "first row starts at the diagnostic column",
        e0.0 as f32,
        geom.x_of(START_COL),
        EDGE_TOL,
    );
    assert_close(
        "first row runs to the end of the line",
        e0.1 as f32,
        geom.x_of(DIAG_LINE_LEN),
        EDGE_TOL,
    );
    assert_close(
        "middle row starts at column 0",
        e1.0 as f32,
        geom.x_of(0),
        EDGE_TOL,
    );
    assert_close(
        "middle row runs to the end of the line",
        e1.1 as f32,
        geom.x_of(DIAG_LINE_LEN),
        EDGE_TOL,
    );
    assert_close(
        "last row starts at column 0",
        e2.0 as f32,
        geom.x_of(0),
        EDGE_TOL,
    );
    assert_close(
        "last row stops at the diagnostic end column",
        e2.1 as f32,
        geom.x_of(END_COL),
        EDGE_TOL,
    );
}

/// Soft wrapping: with word-wrap on, a range that crosses a wrap boundary must
/// continue on the next VISUAL row. This is the case most likely to be wrong,
/// because a soft-wrapped row is not a source line and nothing in the text-level
/// unit tests models one.
///
/// The wrap point itself is measured from a calibration render that underlines
/// the WHOLE long line, so the assertion does not encode a guess about where the
/// editor happens to break.
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn diag_soft_wrapped_range_continues_on_the_next_visual_row() {
    let src = format!("{}\n", "abcdefghij".repeat(20));
    let Some(control) = diag_control("diagnostics_wrapped", true, &src) else {
        return;
    };
    // Calibration: underline the entire (wrapped) line to learn where the rows
    // are and how wide each visual row's text runs.
    let cal_app = diag_app(
        &src,
        vec![diag(
            0,
            0,
            0,
            200,
            super::diagnostics_overlay::SEVERITY_ERROR,
            "the whole wrapped line",
        )],
        true,
    );
    let targets = severity_colors(&cal_app);
    let Some(cal) = render_frame("diagnostics_wrapped_calibration", SCENE_W, SCENE_H, cal_app)
    else {
        return;
    };
    let cal_ink = ink_pixels(&cal, &control, &targets, 0);
    // Split the gutter bar off FIRST — including it in the row extents is what
    // made the first draft of this scene measure a 12.8px character cell for an
    // 8.4px font and then pick two columns that never straddled anything.
    let split = text_origin(&cal_ink);
    let text = split.saturating_sub(3)..cal.width();
    let cal_ink: Vec<(u32, u32)> = cal_ink
        .into_iter()
        .filter(|(x, _)| text.contains(x))
        .collect();
    let cal_bands = ink_bands(&cal_ink, 3);
    assert!(
        cal_bands.len() >= 2,
        "the wrap scene needs a line that actually wraps; the whole-line \
         underline occupied {} row(s)",
        cal_bands.len()
    );
    let full_x = cal_bands
        .iter()
        .map(|b| band_x_extent(&cal_ink, *b, &text).expect("calibration row ink"))
        .collect::<Vec<_>>();
    // Row 0 of the calibration spans the whole first visual row, so its right
    // edge IS the wrap point.
    let (row0_left, row0_right) = full_x[0];
    eprintln!("[diag-wrap] text origin={split} calibration rows: {full_x:?}");

    // A range that starts a few characters before the wrap and ends a few after
    // it. Chars-per-row is read off the calibration, never assumed.
    let text_left = row0_left as f32;
    let text_right = row0_right as f32;
    let cell_w = {
        // Underline exactly ten characters at the start of the line and measure.
        let ten = diag_app(
            &src,
            vec![diag(
                0,
                0,
                0,
                10,
                super::diagnostics_overlay::SEVERITY_ERROR,
                "ten characters",
            )],
            true,
        );
        let Some(ten_shot) = render_frame("diagnostics_wrapped_ten", SCENE_W, SCENE_H, ten) else {
            return;
        };
        let ten_ink: Vec<(u32, u32)> = ink_pixels(&ten_shot, &control, &targets, 0)
            .into_iter()
            .filter(|(x, _)| text.contains(x))
            .collect();
        let bands = ink_bands(&ten_ink, 3);
        assert_eq!(
            bands.len(),
            1,
            "a ten-character range fits one row: {bands:?}"
        );
        let e = band_x_extent(&ten_ink, bands[0], &text).expect("ten ink");
        (e.1 - e.0) as f32 / 10.0
    };
    let chars_per_row = ((text_right - text_left) / cell_w).round() as u32;
    eprintln!("[diag-wrap] cell_w={cell_w:.3} chars_per_row={chars_per_row}");
    assert!(
        chars_per_row > 8,
        "implausible wrap width {chars_per_row} characters"
    );

    // Eight characters either side of the estimated wrap column, so the range
    // still straddles even if the estimate is a character or two out.
    const HALF: u32 = 8;
    let c0 = chars_per_row - HALF;
    let c1 = chars_per_row + HALF;
    let app = diag_app(
        &src,
        vec![diag(
            0,
            c0,
            0,
            c1,
            super::diagnostics_overlay::SEVERITY_ERROR,
            "a range straddling the wrap",
        )],
        true,
    );
    let Some(shot) = render_frame("diagnostics_wrapped", SCENE_W, SCENE_H, app) else {
        return;
    };
    let ink: Vec<(u32, u32)> = ink_pixels(&shot, &control, &targets, 0)
        .into_iter()
        .filter(|(x, _)| text.contains(x))
        .collect();
    let bands = ink_bands(&ink, 3);
    assert_eq!(
        bands.len(),
        2,
        "a range straddling a soft wrap must underline TWO visual rows, got \
         {bands:?} — a span-based painter drops it or runs it off the edge"
    );
    let a = band_x_extent(&ink, bands[0], &text).expect("upper row ink");
    let b = band_x_extent(&ink, bands[1], &text).expect("lower row ink");
    eprintln!("[diag-wrap] straddling rows: {a:?} {b:?}");
    // The upper row runs INTO the wrap: it stops exactly where the whole-line
    // calibration stopped, i.e. at the last character that fits.
    assert_close(
        "upper row runs to the wrap point",
        a.1 as f32,
        text_right,
        EDGE_TOL,
    );
    // …and it does NOT start at the row origin — a painter that gave up and
    // underlined the whole visual row would.
    assert!(
        (a.0 as f32) > cell_w.mul_add(f32::from(HALF as u16), text_left),
        "the upper row's underline starts at x={} — it is underlining the whole \
         visual row, not the last {HALF} characters before the wrap",
        a.0
    );
    // The lower row restarts at the left margin: the continuation begins at the
    // wrapped row's first character, not at the source column.
    assert_close(
        "lower row restarts at the left margin",
        b.0 as f32,
        text_left,
        EDGE_TOL,
    );
    // The strongest statement, and the one that does not depend on knowing the
    // exact wrap column: across the two rows the underline is exactly as long as
    // the range is wide. Nothing is lost at the seam and nothing is drawn twice.
    let painted = (a.1 - a.0) as f32 + (b.1 - b.0) as f32;
    let expected = cell_w * f32::from(u16::try_from(c1 - c0).expect("range fits"));
    assert_close(
        "total underlined width across the two wrapped rows",
        painted,
        expected,
        EDGE_TOL + cell_w,
    );
}

/// Hovering the squiggle opens the tooltip; hovering a character that is NOT
/// underlined does not. The paired negative is what makes this a test of the
/// galley-resolved hit test rather than of "a tooltip exists".
#[test]
#[ignore = "GPU render; run with --ignored on a host with a wgpu adapter"]
fn diag_hover_opens_the_tooltip_on_the_squiggle_only() {
    let Some(control) = diag_control("diagnostics_hover", false, DIAG_SRC) else {
        return;
    };
    let Some(geom) = calibrate(&control) else {
        return;
    };
    let app = diag_app(DIAG_SRC, inline_diags(), false);
    let Some(base) = render_frame("diagnostics_hover_base", SCENE_W, SCENE_H, app) else {
        return;
    };
    // Row y of the error squiggle, from the un-hovered render.
    let targets = severity_colors(&diag_app(DIAG_SRC, inline_diags(), false));
    let ink: Vec<(u32, u32)> = ink_pixels(&base, &control, &targets, 0)
        .into_iter()
        .filter(|(x, _)| text_range(geom, base.width()).contains(x))
        .collect();
    let band = ink_bands(&ink, 3)[0];
    // Aim a couple of pixels ABOVE the underline, i.e. at the glyph itself.
    let on = egui::pos2(
        geom.x_of(ERR_COLS.0 + 1),
        (band.0 + band.1) as f32 / 2.0 - 6.0,
    );
    let off = egui::pos2(geom.x_of(2), (band.0 + band.1) as f32 / 2.0 - 6.0);

    let on_shot = render_hover_frame(
        "diagnostics_hover_on",
        SCENE_W,
        SCENE_H,
        diag_app(DIAG_SRC, inline_diags(), false),
        on,
    );
    let off_shot = render_hover_frame(
        "diagnostics_hover_off",
        SCENE_W,
        SCENE_H,
        diag_app(DIAG_SRC, inline_diags(), false),
        off,
    );
    let (Some(on_shot), Some(off_shot)) = (on_shot, off_shot) else {
        return;
    };
    // Count changes ONLY where a tooltip would sit: the panel egui opens below
    // and to the right of the pointer. Deliberately NOT a whole-frame diff — a
    // hover also draws the mouse cursor and reveals the scroll bar, which is
    // ~2,200 changed pixels of pure noise that swamped the first draft of this
    // assertion and made a correct app look broken.
    let region = |p: egui::Pos2| {
        egui::Rect::from_min_max(
            egui::pos2(p.x + 18.0, p.y + 10.0),
            egui::pos2(p.x + 360.0, p.y + 45.0),
        )
    };
    let on_changed = changed_pixels_in(&on_shot, &base, region(on));
    let off_changed = changed_pixels_in(&off_shot, &base, region(off));
    eprintln!("[diag-hover] tooltip-region changes: on={on_changed} off={off_changed}");
    // Measured margin on a correct build: 2,200 changed pixels on, 0 off. The
    // floor is set well below the former and the ceiling well above the latter,
    // so neither is a hair-trigger — but a tooltip that fails to open, or one
    // that opens anywhere on the line, moves the number by three orders of
    // magnitude and is caught.
    assert!(
        on_changed > 800,
        "hovering the underlined identifier changed only {on_changed} pixels in \
         the tooltip region — the diagnostic tooltip did not open"
    );
    assert!(
        off_changed < 200,
        "hovering column 2 of the SAME line — a character the diagnostic does \
         NOT cover — changed {off_changed} pixels in the tooltip region; the \
         hover is following the line, not the span"
    );
}

/// Render with the pointer parked at `hover`.
fn render_hover_frame(
    name: &str,
    w: f32,
    h: f32,
    app: ScribeApp,
    hover: egui::Pos2,
) -> Option<RgbaImage> {
    if !gpu_available() {
        eprintln!("[visual-qa] no GPU adapter; skipping `{name}` (NOT a pass)");
        return None;
    }
    let mut harness: Harness<'static, ScribeApp> = Harness::builder()
        .with_size(egui::vec2(w, h))
        .wgpu()
        .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app);
    for _ in 0..5 {
        harness.step();
    }
    harness.hover_at(hover);
    for _ in 0..4 {
        harness.step();
    }
    let img = harness.render().expect("kittest wgpu render must succeed");
    let path = out_dir().join(format!("{name}.png"));
    img.save(&path).expect("save visual-qa png");
    eprintln!("[visual-qa] wrote {} (hover {hover:?})", path.display());
    Some(img)
}

/// How many pixels differ meaningfully between two frames inside `rect`.
fn changed_pixels_in(a: &RgbaImage, b: &RgbaImage, rect: egui::Rect) -> usize {
    let x0 = rect.min.x.max(0.0) as u32;
    let y0 = rect.min.y.max(0.0) as u32;
    let x1 = (rect.max.x.max(0.0) as u32).min(a.width());
    let y1 = (rect.max.y.max(0.0) as u32).min(a.height());
    let mut n = 0;
    for y in y0..y1 {
        for x in x0..x1 {
            if changed_enough(a, b, x, y) {
                n += 1;
            }
        }
    }
    n
}
