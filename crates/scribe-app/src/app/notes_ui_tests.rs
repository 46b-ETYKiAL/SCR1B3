//! Behavioural tests for the notes / PKM surface (`app/notes_ui.rs`).
//!
//! The pane's *pure* layer — the vault walk, the per-note index build, the
//! search filter, backlink collection, and the small render-decision helpers —
//! is what decides whether a note is findable, whether a link is followable, and
//! whether the surface can lose data. None of that is observable in a painted
//! frame, so it is pinned here with real temp vaults rather than at the render
//! layer.
//!
//! Every constant the vault walk gates on (`MAX_NOTE_BYTES`, `MAX_BODY_KEPT`) is
//! re-stated as a LITERAL in these tests, deliberately never imported: a test
//! that reads the same constant as the code under test moves with it, so a
//! wrong constant would still look right. The literals below are the contract.

use super::notes_ui::{
    backlink_rows, body_cap_end, collect_backlinks, filter_docs, filter_replacement, is_active_row,
    notes_list_height, outgoing_links, scan_vault, selected_tag_from_filter, show_no_match_hint,
    suggestion_chip_label, tag_filter_for, tag_row_is_selected, NoteDoc,
};
use super::ScribeApp;
use scribe_core::notes::{completion, query};
use scribe_core::Config;
use std::path::{Path, PathBuf};

/// `MAX_NOTE_BYTES` — the per-file size ceiling the vault walk applies, restated
/// as a literal (1 MiB). Never import the const: see the module docs.
const MAX_NOTE_BYTES_LITERAL: usize = 1_048_576;
/// `MAX_BODY_KEPT` — the indexed-body ceiling, restated as a literal (256 KiB).
const MAX_BODY_KEPT_LITERAL: usize = 262_144;

fn app_with_vault(vault: Option<&Path>) -> ScribeApp {
    let mut cfg = Config::default();
    cfg.editor.first_run_completed = true;
    cfg.notes.vault_dir = vault.map(Path::to_path_buf);
    ScribeApp::new_test(cfg)
}

/// The `rel_display` of every indexed note, sorted — the note list as the user
/// sees it.
fn rel_paths(docs: &[NoteDoc]) -> Vec<String> {
    let mut v: Vec<String> = docs.iter().map(|d| d.rel_display.clone()).collect();
    v.sort();
    v
}

// ───────────────────────── the vault walk: what gets indexed ────────────────

/// Only note-like extensions are indexed, and the extension test is what does
/// the excluding.
///
/// Without this, a vault that also holds source/binary files would list them as
/// "notes" (and read every one of them into the search index).
#[test]
fn the_vault_walk_indexes_only_note_extensions() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("prose.md"), "# Prose\n").unwrap();
    std::fs::write(dir.path().join("plain.txt"), "plain\n").unwrap();
    std::fs::write(dir.path().join("code.rs"), "fn main() {}\n").unwrap();
    std::fs::write(dir.path().join("pic.png"), "not really a png").unwrap();

    let docs = scan_vault(dir.path());

    assert_eq!(
        rel_paths(&docs),
        vec!["plain.txt".to_string(), "prose.md".to_string()],
        "exactly the note-extension files are indexed — .rs/.png are not notes"
    );
    // `relative_display` must produce the vault-RELATIVE, `/`-separated path,
    // not an absolute one and not a placeholder.
    assert!(
        docs.iter().all(|d| !d.rel_display.contains('\\')
            && !d.rel_display.is_empty()
            && Path::new(&d.rel_display).is_relative()),
        "rel_display is vault-relative with `/` separators: {:?}",
        rel_paths(&docs)
    );
}

/// The walk descends real sub-directories but prunes the build/VCS skip list and
/// hidden entries — and the pruning is what excludes them, not the extension
/// test (all four files below are `.md`).
#[test]
fn the_vault_walk_descends_subdirs_and_prunes_skipped_and_hidden_ones() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("top.md"), "# Top\n").unwrap();
    for (sub, name) in [
        ("projects", "roadmap.md"),
        ("target", "generated.md"),
        ("node_modules", "vendored.md"),
        (".obsidian", "workspace.md"),
    ] {
        std::fs::create_dir_all(dir.path().join(sub)).unwrap();
        std::fs::write(dir.path().join(sub).join(name), "# Sub\n").unwrap();
    }

    let docs = scan_vault(dir.path());

    assert_eq!(
        rel_paths(&docs),
        vec!["projects/roadmap.md".to_string(), "top.md".to_string()],
        "a real subdir is descended; target/ node_modules/ and dot-dirs are pruned"
    );
}

/// The size ceiling is an EXCLUSIVE `>` test: a note of exactly `MAX_NOTE_BYTES`
/// is still indexed, one byte more is not.
///
/// The exact-boundary file is the only input that distinguishes `>` from `>=` —
/// a mid-band note proves neither.
#[test]
fn the_size_ceiling_admits_a_note_of_exactly_the_cap_and_rejects_one_byte_more() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("small.md"), "# Small\n").unwrap();
    std::fs::write(
        dir.path().join("exactly-at-cap.md"),
        "a".repeat(MAX_NOTE_BYTES_LITERAL),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("one-over-cap.md"),
        "a".repeat(MAX_NOTE_BYTES_LITERAL + 1),
    )
    .unwrap();

    let docs = scan_vault(dir.path());

    assert_eq!(
        rel_paths(&docs),
        vec!["exactly-at-cap.md".to_string(), "small.md".to_string()],
        "a note of exactly {MAX_NOTE_BYTES_LITERAL} bytes is indexed; one byte over is skipped"
    );
}

/// An oversized note is skipped, but a small one is indexed whole — the pair
/// pins that the ceiling is a real byte count, not a degenerate 0 or a value
/// that admits everything.
#[test]
fn a_short_note_keeps_its_whole_body_in_the_index() {
    let dir = tempfile::tempdir().unwrap();
    // Comfortably larger than any plausible mis-derived cap, far under the real
    // 256 KiB one.
    let body = format!("# Short\n\n{}\n", "word ".repeat(1_000));
    std::fs::write(dir.path().join("short.md"), &body).unwrap();

    let docs = scan_vault(dir.path());

    assert_eq!(docs.len(), 1);
    assert_eq!(
        docs[0].body, body,
        "a note well under the body cap is indexed in full (searchable end to end)"
    );
}

/// A note over the body cap is truncated to the cap, cut on a `char` boundary.
///
/// The fixture is built from a 3-byte character so the cap index lands INSIDE a
/// character — the only shape that exercises the boundary walk at all. With a
/// 1-byte-per-char fixture the walk is a no-op and proves nothing.
#[test]
fn an_over_long_note_body_is_capped_on_a_char_boundary() {
    let dir = tempfile::tempdir().unwrap();
    // 'あ' is 3 bytes; 262_144 = 3*87_381 + 1, so byte 262_144 is one byte into
    // a character and the cut must step back to 262_143.
    let content = "あ".repeat(90_000);
    assert!(content.len() > MAX_BODY_KEPT_LITERAL);
    std::fs::write(dir.path().join("long.md"), &content).unwrap();

    let docs = scan_vault(dir.path());

    assert_eq!(docs.len(), 1);
    let kept = &docs[0].body;
    assert_eq!(
        kept.len(),
        262_143,
        "the body is cut at the last char boundary at or below the 256 KiB cap"
    );
    assert_eq!(
        kept.chars().count(),
        87_381,
        "the kept prefix is whole characters — never a split multi-byte char"
    );
    assert!(
        kept.chars().all(|c| c == 'あ') && content.starts_with(kept.as_str()),
        "the kept text is a prefix of the note, not a re-encoding"
    );
}

/// `body_cap_end` directly: it returns the whole length when the body fits, and
/// otherwise the last char boundary at or below the cap.
#[test]
fn body_cap_end_returns_a_char_boundary_at_or_below_the_cap() {
    assert_eq!(body_cap_end("abc", 10), 3, "a body that fits is not cut");
    assert_eq!(
        body_cap_end("abc", 3),
        3,
        "a body exactly at the cap is not cut"
    );
    assert_eq!(
        body_cap_end("abcd", 3),
        3,
        "an ASCII body cuts exactly at the cap"
    );
    // "あああ" = 9 bytes; a cap of 4 lands inside the second char → step to 3.
    assert_eq!(
        body_cap_end("あああ", 4),
        3,
        "a cap landing inside a multi-byte char steps back to its start"
    );
    assert_eq!(
        body_cap_end("あああ", 3),
        3,
        "a cap exactly on a char boundary is kept"
    );
    assert_eq!(
        body_cap_end("あ", 1),
        0,
        "no boundary above 0 → cut to empty"
    );
}

// ───────────────────────── outgoing links: embeds and empties ───────────────

/// A note's indexed outgoing links exclude embeds (`![[x]]`) and links that name
/// no target (`[[#heading]]`).
///
/// `[[#Section]]` is used for the empty-target case on purpose: a bare `[[]]` is
/// discarded by the parser upstream, so a test using it would never reach the
/// empty-target filter and would pass with that filter deleted.
#[test]
fn indexed_links_exclude_embeds_and_target_less_links() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("hub.md"),
        "# Hub\n\nSee [[Ideas]] and ![[diagram.png]] and [[#Section]] and [[Ideas]].\n",
    )
    .unwrap();

    let docs = scan_vault(dir.path());

    assert_eq!(docs.len(), 1);
    assert_eq!(
        docs[0].links,
        vec!["Ideas".to_string(), "Ideas".to_string()],
        "only real, target-bearing, non-embed links are indexed (duplicates kept in \
         document order — de-duplication is the pane's job, not the index's)"
    );
}

/// `outgoing_links` — the active note's "links out" list — keeps the first
/// occurrence of each distinct target and drops embeds and target-less links.
#[test]
fn outgoing_links_keep_first_seen_and_drop_embeds_and_empties() {
    let out =
        outgoing_links("[[Home]] ![[cover.png]] [[Ideas]] [[Home]] [[#Section]] [[Ideas|alias]]");
    assert_eq!(
        out.iter().map(|l| l.target.as_str()).collect::<Vec<_>>(),
        vec!["Home", "Ideas"],
        "distinct targets in first-seen order; no embed, no empty target, and an \
         aliased link folds onto its target"
    );
    assert_eq!(
        out.iter().map(|l| l.label.as_str()).collect::<Vec<_>>(),
        vec!["Home", "Ideas"],
        "the FIRST spelling's label wins — the later `|alias` does not rename the row"
    );
}

/// The row SHOWS the author's label but OPENS the raw target. Conflating the two
/// either throws the alias away (showing `projects/roadmap`) or resolves a note
/// that does not exist (opening `Q3 plan`).
#[test]
fn an_aliased_link_shows_its_alias_and_still_targets_the_real_note() {
    let out = outgoing_links("[[projects/roadmap|Q3 plan]]");
    assert_eq!(out.len(), 1);
    assert_eq!(
        out[0].label, "Q3 plan",
        "the row reads as the author wrote it"
    );
    assert_eq!(
        out[0].target, "projects/roadmap",
        "but the followed target is the raw path, never the label"
    );
    assert_eq!(out[0].display_text(), "Q3 plan");
}

/// A `#heading` anchor is part of what a link names, so it must be visible:
/// without it, two links into different sections of one note render identically.
#[test]
fn a_heading_anchor_is_shown_alongside_the_label() {
    let out = outgoing_links("[[Roadmap#Q3]] and [[Notes#Q4|Later]]");
    assert_eq!(
        out.iter().map(|l| l.display_text()).collect::<Vec<_>>(),
        vec!["Roadmap › Q3".to_string(), "Later › Q4".to_string()],
        "the anchor rides along with the label (aliased or not)"
    );
    assert_eq!(
        out[0].target, "Roadmap",
        "the anchor is NOT part of the target that gets opened"
    );
    assert_eq!(out[1].target, "Notes");
    // A link with no anchor shows the bare label — the ` › ` is not unconditional.
    assert_eq!(outgoing_links("[[Plain]]")[0].display_text(), "Plain");
}

// ───────────────────────────── search / filter ──────────────────────────────

fn query_vault() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("projects")).unwrap();
    std::fs::write(
        dir.path().join("projects").join("roadmap.md"),
        "# Roadmap\n\n#project shipping soon\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("groceries.md"),
        "# Groceries\n\n#errand milk and bread\n",
    )
    .unwrap();
    dir
}

/// The filter really applies each operator to the right field. An empty query
/// returns the whole index (the note list is not silently empty).
#[test]
fn the_search_filter_applies_tag_title_path_and_free_text_operators() {
    let dir = query_vault();
    let docs = scan_vault(dir.path());
    assert_eq!(docs.len(), 2, "fixture: two notes");

    let hits = |q: &str| {
        let parsed = query::parse(q);
        let mut v: Vec<String> = filter_docs(&docs, &parsed)
            .into_iter()
            .map(|d| d.rel_display.clone())
            .collect();
        v.sort();
        v
    };

    assert_eq!(
        hits(""),
        vec![
            "groceries.md".to_string(),
            "projects/roadmap.md".to_string()
        ],
        "an empty query lists every note"
    );
    assert_eq!(
        hits("tag:project"),
        vec!["projects/roadmap.md".to_string()],
        "tag: matches the note's tag set"
    );
    assert_eq!(
        hits("title:Groceries"),
        vec!["groceries.md".to_string()],
        "title: matches the derived title"
    );
    assert_eq!(
        hits("path:projects"),
        vec!["projects/roadmap.md".to_string()],
        "path: matches the vault-relative path"
    );
    assert_eq!(
        hits("bread"),
        vec!["groceries.md".to_string()],
        "bare text matches the note body"
    );
    assert_eq!(
        hits("-tag:project"),
        vec!["groceries.md".to_string()],
        "a negated clause excludes"
    );
    assert!(
        hits("tag:project title:Groceries").is_empty(),
        "clauses are AND-ed"
    );
}

// ─────────────────────────────── backlinks ──────────────────────────────────

fn backlink_vault() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Home.md"), "# Home\n\nSee [[Ideas]].\n").unwrap();
    std::fs::write(
        dir.path().join("Ideas.md"),
        "# Ideas\n\nBack to [[Home]].\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("Loner.md"), "# Loner\n\nNo links at all.\n").unwrap();
    std::fs::write(
        dir.path().join("Selfie.md"),
        "# Selfie\n\nI am [[Selfie]].\n",
    )
    .unwrap();
    dir
}

fn titles(rows: &[&NoteDoc]) -> Vec<String> {
    let mut v: Vec<String> = rows.iter().map(|d| d.title.clone()).collect();
    v.sort();
    v
}

/// Backlinks are exactly the notes whose links RESOLVE to the target — a note
/// with no links is not a backlink, and the resolution is what selects them.
#[test]
fn backlinks_are_the_notes_that_actually_link_to_the_target() {
    let dir = backlink_vault();
    let docs = scan_vault(dir.path());

    let back = collect_backlinks(&docs, dir.path(), &dir.path().join("Ideas.md"));
    assert_eq!(
        titles(&back),
        vec!["Home".to_string()],
        "only Home links to Ideas — Loner (no links) and Selfie (links elsewhere) \
         must not be listed"
    );
}

/// A note that links to ITSELF is never listed as its own backlink.
#[test]
fn a_note_is_never_its_own_backlink() {
    let dir = backlink_vault();
    let docs = scan_vault(dir.path());

    let back = collect_backlinks(&docs, dir.path(), &dir.path().join("Selfie.md"));
    assert!(
        back.is_empty(),
        "Selfie links to itself; a self-link must not render as a backlink, got {:?}",
        titles(&back)
    );
}

/// The backlinks section needs BOTH a configured vault and a file-backed active
/// note; with either missing it lists nothing rather than guessing.
#[test]
fn backlink_rows_require_both_a_vault_and_an_active_path() {
    let dir = backlink_vault();
    let docs = scan_vault(dir.path());
    let ideas = dir.path().join("Ideas.md");

    let rows = backlink_rows(&docs, Some(dir.path()), Some(&ideas));
    assert_eq!(
        rows,
        vec![(dir.path().join("Home.md"), "Home".to_string())],
        "with both known, the real backlink rows are produced (path AND title)"
    );

    assert!(
        backlink_rows(&docs, None, Some(&ideas)).is_empty(),
        "no vault → no backlinks"
    );
    assert!(
        backlink_rows(&docs, Some(dir.path()), None).is_empty(),
        "no file-backed active note → no backlinks"
    );
    assert!(backlink_rows(&docs, None, None).is_empty());
}

// ─────────────────────── index lifecycle on the app ─────────────────────────

/// Clearing the vault clears the index — a stale index must not keep listing
/// notes from a folder the user has unset.
#[test]
fn dropping_the_vault_clears_the_index_and_its_recorded_root() {
    let dir = backlink_vault();
    let mut app = app_with_vault(Some(dir.path()));
    app.notes_ensure_index();
    assert_eq!(app.note_index.len(), 4, "fixture indexed");

    app.config.notes.vault_dir = None;
    app.notes_ensure_index();

    assert!(app.note_index.is_empty(), "index cleared with the vault");
    assert!(
        app.note_index_root.is_none(),
        "the recorded root is cleared too, so the next ensure does not think it is in sync"
    );
}

/// Pointing the vault at a NEW folder re-scans; the index tracks the vault it
/// was built for rather than sticking to the first one.
#[test]
fn changing_the_vault_rescans_into_the_new_folder() {
    let first = backlink_vault();
    let second = query_vault();
    let mut app = app_with_vault(Some(first.path()));
    app.notes_ensure_index();
    assert_eq!(app.note_index.len(), 4);

    app.config.notes.vault_dir = Some(second.path().to_path_buf());
    app.notes_ensure_index();

    assert_eq!(
        rel_paths(&app.note_index),
        vec![
            "groceries.md".to_string(),
            "projects/roadmap.md".to_string()
        ],
        "the index is rebuilt from the newly chosen vault"
    );
}

// ───────────────────── following a wiki-link (create vs open) ───────────────

/// Following a link to a note that does not exist yet CREATES it, seeded with
/// its title heading, and opens it.
#[test]
fn following_a_link_to_a_missing_note_creates_and_opens_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app_with_vault(Some(dir.path()));

    app.open_or_create_wikilink("Fresh Note");

    let created = dir.path().join("Fresh Note.md");
    assert!(
        created.is_file(),
        "the note must be created on disk, not merely opened"
    );
    assert_eq!(
        std::fs::read_to_string(&created).unwrap(),
        "# Fresh Note\n",
        "the new note is seeded with its title as a heading"
    );
    assert!(
        app.tabs
            .iter()
            .any(|t| t.doc.path().is_some_and(|p| p.ends_with("Fresh Note.md"))),
        "the created note is opened in a tab"
    );
    assert!(
        app.toast.is_none(),
        "creating a valid note raises no error toast"
    );
}

/// Following a link to an EXISTING note opens it and leaves its content
/// untouched. This is the data-loss guard: the seed write must be reachable ONLY
/// for a note that does not exist.
#[test]
fn following_a_link_to_an_existing_note_never_overwrites_it() {
    let dir = tempfile::tempdir().unwrap();
    let existing = dir.path().join("Ideas.md");
    let original = "# Ideas\n\nYears of irreplaceable notes.\n";
    std::fs::write(&existing, original).unwrap();
    let mut app = app_with_vault(Some(dir.path()));

    app.open_or_create_wikilink("Ideas");

    assert_eq!(
        std::fs::read_to_string(&existing).unwrap(),
        original,
        "an existing note must NEVER be re-seeded — that would destroy its content"
    );
    assert!(
        app.tabs
            .iter()
            .any(|t| t.doc.path().is_some_and(|p| p.ends_with("Ideas.md"))),
        "the existing note is opened"
    );
}

/// With no vault configured, following a link explains what to do instead of
/// writing a note somewhere arbitrary.
#[test]
fn following_a_link_with_no_vault_configured_creates_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app_with_vault(None);

    app.open_or_create_wikilink("Anything");

    assert!(
        app.toast.is_some(),
        "the user is told to choose a notes folder"
    );
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        0,
        "nothing is written anywhere"
    );
}

// ───────────────────── pure render decisions (no painted pixels) ────────────

/// The note list takes a fixed fraction of the pane's remaining height, leaving
/// room for the links-out / backlinks sections beneath it.
#[test]
fn the_note_list_takes_a_fixed_fraction_of_the_available_height() {
    assert!((notes_list_height(200.0) - 110.0).abs() < f32::EPSILON);
    assert!((notes_list_height(400.0) - 220.0).abs() < f32::EPSILON);
    assert!((notes_list_height(0.0)).abs() < f32::EPSILON);
    assert!(
        notes_list_height(300.0) < 300.0,
        "the list never claims the whole pane — the link sections must fit below"
    );
}

/// "no notes match" is shown only when a vault IS configured and the filter
/// selected nothing; with no vault the pane already shows the choose-a-folder
/// prompt and this hint would misdescribe the state.
#[test]
fn the_no_match_hint_needs_both_a_vault_and_an_empty_result() {
    assert!(
        show_no_match_hint(true, 0),
        "vault set + nothing matched → hint"
    );
    assert!(!show_no_match_hint(true, 3), "some notes matched → no hint");
    assert!(
        !show_no_match_hint(false, 0),
        "no vault → the choose-a-folder prompt owns this state, not 'no notes match'"
    );
    assert!(!show_no_match_hint(false, 7));
}

/// A note-list row is drawn selected only for the note actually open in the
/// active tab.
#[test]
fn only_the_open_note_is_the_active_row() {
    let a = PathBuf::from("/vault/Home.md");
    let b = PathBuf::from("/vault/Ideas.md");
    assert!(is_active_row(Some(&a), &a), "the open note's row is active");
    assert!(
        !is_active_row(Some(&a), &b),
        "a different note's row is not"
    );
    assert!(
        !is_active_row(None, &a),
        "with no file-backed active tab no row is active"
    );
}

// ──────────────── tag tree + completion: the pure decisions ─────────────────

/// The tree's highlight is read back OUT of the filter string, so it can only
/// ever agree with what the list is actually showing. Anything more complex than
/// a single positive `tag:` clause leaves NO node highlighted — no node
/// describes that result.
#[test]
fn a_node_is_selected_only_for_a_lone_positive_tag_clause() {
    assert_eq!(
        selected_tag_from_filter("tag:project"),
        Some("project".to_string())
    );
    assert_eq!(
        selected_tag_from_filter("tag:project/frontend"),
        Some("project/frontend".to_string()),
        "a nested node is selectable too"
    );
    assert_eq!(
        selected_tag_from_filter("tag:Project"),
        Some("project".to_string()),
        "the operator VALUE is case-folded by the parser, and the tree's nodes are \
         lowercase too, so the two meet"
    );
    assert_eq!(
        selected_tag_from_filter("TAG:project"),
        None,
        "the operator KEY is case-SENSITIVE — `TAG:` is free text, and highlighting a \
         node for it would claim a selection the list does not have"
    );
    assert_eq!(
        selected_tag_from_filter(""),
        None,
        "an empty filter selects nothing"
    );
    assert_eq!(
        selected_tag_from_filter("-tag:project"),
        None,
        "an EXCLUDED tag must not highlight as if it were the selection"
    );
    assert_eq!(
        selected_tag_from_filter("tag:project roadmap"),
        None,
        "extra typed terms narrow the result past what the node claims"
    );
    assert_eq!(
        selected_tag_from_filter("title:project"),
        None,
        "a different operator is not a tag selection"
    );
}

/// Clicking a node applies its filter; clicking the SELECTED node clears it, so
/// the tree can undo itself without the user reaching for the text box.
#[test]
fn clicking_the_selected_node_clears_the_filter_and_any_other_node_replaces_it() {
    assert_eq!(tag_filter_for("project", None), "tag:project");
    assert_eq!(
        tag_filter_for("project", Some("errand")),
        "tag:project",
        "a different selection is replaced, not toggled off"
    );
    assert_eq!(
        tag_filter_for("project", Some("project")),
        "",
        "re-clicking the selected node clears the filter"
    );
    assert_eq!(
        tag_filter_for("project", Some("project/frontend")),
        "tag:project",
        "a DESCENDANT being selected is not the same node — clicking the parent selects it"
    );
}

/// A chip reads as the sigil the user is typing but expands to the operator the
/// query grammar actually matches on. Expanding to literal sigil text would fall
/// through to a free-text search and silently return the wrong notes.
#[test]
fn a_chip_reads_as_a_sigil_and_expands_to_a_query_operator() {
    assert_eq!(
        suggestion_chip_label(completion::Trigger::Tag, "project"),
        "#project"
    );
    assert_eq!(
        suggestion_chip_label(completion::Trigger::Note, "Roadmap"),
        "[[Roadmap]]"
    );
    assert_eq!(
        filter_replacement(completion::Trigger::Tag, "project"),
        "tag:project"
    );
    assert_eq!(
        filter_replacement(completion::Trigger::Note, "Morning Pages"),
        "title:\"Morning Pages\"",
        "a title is quoted — unquoted, its spaces would split into a second clause"
    );
    // And the expansion really is a query the parser understands as that field.
    let q = query::parse(&filter_replacement(
        completion::Trigger::Note,
        "Morning Pages",
    ));
    assert_eq!(q.clauses.len(), 1, "one clause, not one per word");
    assert_eq!(
        selected_tag_from_filter(&filter_replacement(completion::Trigger::Tag, "project")),
        Some("project".to_string()),
        "an accepted tag chip lands on a filter the tree then highlights"
    );
}

// ─────────────────── the pane itself, through the real render loop ──────────

use egui_kittest::kittest::Queryable as _;

fn harness(app: ScribeApp) -> egui_kittest::Harness<'static, ScribeApp> {
    egui_kittest::Harness::builder()
        .with_size(egui::Vec2::new(1100.0, 760.0))
        .build_state(|ctx, app: &mut ScribeApp| app.frame_tick(ctx), app)
}

/// The pane renders its note list only while it is OPEN — and when it is open it
/// really lists the vault's notes.
///
/// Both halves matter: a pane that renders nothing when open is a dead feature,
/// and a pane that renders when closed steals a left side-panel's worth of the
/// editor. One `notes_pane_open` flag decides both, so both are asserted.
#[test]
fn the_notes_pane_renders_its_vault_only_while_open() {
    let dir = backlink_vault();

    // Closed (the default): no pane, no note titles.
    let mut closed = harness(app_with_vault(Some(dir.path())));
    closed.run();
    assert!(
        closed.query_by_label("NOTES").is_none(),
        "a closed notes pane must not render — it would eat editor width"
    );
    assert!(
        closed.query_by_label("Loner").is_none(),
        "no note rows render while the pane is closed"
    );

    // Open: the pane header and the vault's notes are on screen.
    let mut open = harness(app_with_vault(Some(dir.path())));
    open.state_mut().notes_pane_open = true;
    open.run();
    assert!(
        open.query_by_label("NOTES").is_some(),
        "the open pane renders its header"
    );
    for title in ["Home", "Ideas", "Loner", "Selfie"] {
        assert!(
            open.query_by_label(title).is_some(),
            "the open pane lists the vault note {title:?}"
        );
    }
}

// ─────────────── the tag tree + search box, driven like a user ──────────────

/// A vault whose tags nest, so the tree has a real parent with two children plus
/// an unrelated root.
fn tag_vault() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("projects")).unwrap();
    std::fs::write(
        dir.path().join("projects").join("roadmap.md"),
        "# Roadmap\n\n#project/frontend shipping soon\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("projects").join("api.md"),
        "# API\n\n#project/backend endpoints\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("groceries.md"),
        "# Groceries\n\n#errand milk and bread\n",
    )
    .unwrap();
    dir
}

/// Open the pane on `dir` and expand the collapsed section named `section`.
fn pane_with_section(dir: &Path, section: &str) -> egui_kittest::Harness<'static, ScribeApp> {
    let mut h = harness(app_with_vault(Some(dir)));
    h.state_mut().notes_pane_open = true;
    h.run();
    h.get_by_label(section).click();
    h.run();
    h
}

/// The note titles currently rendered in the pane's list.
fn visible_titles(h: &egui_kittest::Harness<'static, ScribeApp>) -> Vec<&'static str> {
    ["Roadmap", "API", "Groceries"]
        .into_iter()
        .filter(|t| h.query_by_label(t).is_some())
        .collect()
}

/// The tag tree materialises the PARENT of a tag nobody wrote, and clicking a
/// node filters the list to it — the whole point of the surface.
///
/// `#project` is never written in any note (only `#project/frontend` and
/// `#project/backend` are), so a tree built from raw tags alone would not offer
/// it at all, and the two-note filter behind it would be unreachable.
#[test]
fn clicking_a_tag_tree_node_filters_the_note_list_to_that_tag_and_its_children() {
    let dir = tag_vault();
    let mut h = pane_with_section(dir.path(), "tags (4)");

    assert_eq!(
        visible_titles(&h),
        vec!["Roadmap", "API", "Groceries"],
        "the unfiltered pane lists every note"
    );
    for row in ["#errand", "#project", "#frontend", "#backend"] {
        assert!(
            h.query_by_label(row).is_some(),
            "the tag tree renders the row {row:?} (parents materialised, children by segment)"
        );
    }

    h.get_by_label("#project").click();
    h.run();

    assert_eq!(
        h.state().notes_filter,
        "tag:project",
        "the click writes the filter the list is actually driven by"
    );
    assert_eq!(
        visible_titles(&h),
        vec!["Roadmap", "API"],
        "both notes nested under #project survive; the #errand note is filtered out"
    );
}

/// Re-clicking the selected node clears the filter — the tree undoes itself.
#[test]
fn re_clicking_the_selected_tag_node_restores_the_whole_list() {
    let dir = tag_vault();
    let mut h = pane_with_section(dir.path(), "tags (4)");

    h.get_by_label("#errand").click();
    h.run();
    assert_eq!(h.state().notes_filter, "tag:errand");
    assert_eq!(visible_titles(&h), vec!["Groceries"]);

    h.get_by_label("#errand").click();
    h.run();
    assert_eq!(
        h.state().notes_filter,
        "",
        "clicking the selected node again clears the filter"
    );
    assert_eq!(
        visible_titles(&h),
        vec!["Roadmap", "API", "Groceries"],
        "and the whole vault is listed again"
    );
}

/// Typed search text must SURVIVE the frame. The filter string is moved out of
/// the app for the text field to borrow and moved back afterwards; if that
/// round-trip breaks, every keystroke is swallowed and the box appears frozen —
/// a failure no assertion over `parse`/`filter_docs` can see.
#[test]
fn text_typed_into_the_search_box_survives_the_frame_and_filters_the_list() {
    let dir = tag_vault();
    let mut h = harness(app_with_vault(Some(dir.path())));
    h.state_mut().notes_pane_open = true;
    h.run();

    search_box(&h).focus();
    h.run();
    search_box(&h).type_text("tag:errand");
    h.run();

    assert_eq!(
        h.state().notes_filter,
        "tag:errand",
        "the typed text is written back onto the app, not dropped with the frame"
    );
    assert_eq!(
        visible_titles(&h),
        vec!["Groceries"],
        "and it really drives the list"
    );
}

/// The pane's single-line search field. It is the only single-line text input in
/// the app while the find bar / palette / settings are closed.
fn search_box<'a>(h: &'a egui_kittest::Harness<'static, ScribeApp>) -> egui_kittest::Node<'a> {
    let mut inputs = h.get_all_by_role(egui::accesskit::Role::TextInput);
    let first = inputs.next().expect("the notes search field");
    assert!(
        inputs.next().is_none(),
        "fixture assumption: exactly one single-line text input is on screen"
    );
    first
}

/// Typing `#…` offers tag chips, and accepting one expands the sigil into the
/// `tag:` operator that actually filters. A chip that inserted literal `#project`
/// would fall through to a free-text body search and return the wrong notes.
#[test]
fn a_typed_tag_sigil_offers_a_chip_that_expands_into_a_working_filter() {
    let dir = tag_vault();
    let mut h = harness(app_with_vault(Some(dir.path())));
    h.state_mut().notes_pane_open = true;
    h.run();

    search_box(&h).focus();
    h.run();
    search_box(&h).type_text("#front");
    h.run();

    assert!(
        h.query_by_label("#project/frontend").is_some(),
        "the half-typed tag offers its completion as a chip"
    );
    assert!(
        h.query_by_label("#errand").is_none(),
        "a tag that does not match the typed prefix is not offered"
    );

    h.get_by_label("#project/frontend").click();
    h.run();

    assert_eq!(
        h.state().notes_filter,
        "tag:project/frontend",
        "accepting the chip replaces the whole `#front` token with the operator"
    );
    assert_eq!(visible_titles(&h), vec!["Roadmap"]);
}

/// Typing `[[…` offers note-title chips, and accepting one expands to a quoted
/// `title:` operator.
#[test]
fn a_typed_link_sigil_offers_a_chip_that_expands_into_a_title_filter() {
    let dir = tag_vault();
    let mut h = harness(app_with_vault(Some(dir.path())));
    h.state_mut().notes_pane_open = true;
    h.run();

    search_box(&h).focus();
    h.run();
    search_box(&h).type_text("[[Road");
    h.run();

    assert!(
        h.query_by_label("[[Roadmap]]").is_some(),
        "the open `[[` offers matching note titles"
    );
    assert!(
        h.query_by_label("[[Groceries]]").is_none(),
        "a title that does not match the typed prefix is not offered"
    );

    h.get_by_label("[[Roadmap]]").click();
    h.run();

    assert_eq!(
        h.state().notes_filter,
        "title:\"Roadmap\"",
        "accepting the chip replaces the whole `[[Road` token with a quoted operator"
    );
    assert_eq!(visible_titles(&h), vec!["Roadmap"]);
}

/// The "links out" row is drawn with the link's own LABEL and still opens its
/// raw target. Both halves are load-bearing: rendering the target throws the
/// author's alias away, and following the label would resolve a note that does
/// not exist (and, worse, CREATE it).
#[test]
fn the_links_out_row_shows_the_alias_and_opens_the_real_target() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("projects")).unwrap();
    std::fs::write(
        dir.path().join("projects").join("roadmap.md"),
        "# Roadmap\n\nthe plan\n",
    )
    .unwrap();
    let hub = dir.path().join("Hub.md");
    std::fs::write(&hub, "# Hub\n\nSee [[projects/roadmap|Q3 plan]].\n").unwrap();

    let mut app = app_with_vault(Some(dir.path()));
    app.open_path(hub);
    let mut h = harness(app);
    h.state_mut().notes_pane_open = true;
    h.run();
    h.get_by_label("links out (1)").click();
    h.run();

    assert!(
        h.query_by_label("Q3 plan").is_some(),
        "the row reads as the author's alias"
    );
    h.get_by_label("Q3 plan").click();
    h.run();

    let opened: Vec<String> = h
        .state()
        .tabs
        .iter()
        .filter_map(|t| t.doc.path().map(|p| p.display().to_string()))
        .collect();
    assert!(
        opened.iter().any(|p| p.ends_with("roadmap.md")),
        "clicking the aliased row opens the TARGET note, got {opened:?}"
    );
    assert!(
        !dir.path().join("Q3 plan.md").exists(),
        "the label must never be followed as a target — that would create a phantom note"
    );
}

/// The tag tree highlights the row the filter names — and only that row.
///
/// `tag_row_is_selected` is what `render_notes_pane` hands to
/// `Button::selectable`. It was inline as `selected_tag.as_deref() ==
/// Some(node.tag.as_str())` and its `==` mutant survived, because egui 0.34 does
/// NOT report `selected` in that widget's `WidgetInfo`: the highlight never
/// reaches the accessibility tree, so no label, geometry or click-outcome
/// assertion can see it. Naming the rule puts the decision somewhere a test can
/// reach — inverted to `!=` every row EXCEPT the active one lights up, and the
/// tree then tells the user the opposite of what the list is showing.
///
/// The rule is the mirror of `tag_filter_for`'s toggle-off condition, so the two
/// are asserted to AGREE: the row that draws selected is exactly the row whose
/// click clears the filter.
#[test]
fn the_tag_row_highlight_marks_the_filtered_tag_and_no_other() {
    assert!(
        tag_row_is_selected(Some("project"), "project"),
        "the row the filter names is the selected one"
    );
    assert!(
        !tag_row_is_selected(Some("project"), "project/frontend"),
        "a CHILD of the filtered tag is not itself the selection"
    );
    assert!(
        !tag_row_is_selected(Some("project"), "errand"),
        "an unrelated row is not selected"
    );
    assert!(
        !tag_row_is_selected(None, "project"),
        "with no tag filter no row is selected"
    );

    // The highlight and the click must describe the same row: whichever row
    // draws selected is the one a click TOGGLES OFF.
    for tag in ["project", "project/frontend", "errand"] {
        let selected = Some("project");
        assert_eq!(
            tag_row_is_selected(selected, tag),
            tag_filter_for(tag, selected).is_empty(),
            "the highlight for {tag:?} must agree with the click's toggle-off"
        );
    }
}
