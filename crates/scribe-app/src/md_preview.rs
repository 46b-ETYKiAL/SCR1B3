//! Markdown preview: `pulldown-cmark` events → a flat, document-order list of
//! styled [`MdBlock`]s that the egui side panel renders as native widgets.
//!
//! **No HTML, no webview, no JavaScript.** The CommonMark core plus four GFM
//! extensions are rendered (headings, paragraphs, emphasis/strong, inline +
//! fenced code, bullet and ordered lists, task lists, blockquotes/callouts,
//! links, horizontal rules, **tables**, **footnotes** and **math**); anything
//! else degrades to plain text. This keeps the crate `#![forbid(unsafe_code)]`
//! and adds zero attack surface beyond the pure-Rust parser.
//!
//! The design splits cleanly into three parts:
//!   1. [`parse`] — pure `&str -> Vec<MdBlock>`, no egui dependency, fully unit
//!      tested below. This is the load-bearing logic.
//!   2. [`show`] — walks the parsed blocks and emits egui widgets. It contains
//!      no parsing logic, so it cannot be the source of a markdown bug.
//!   3. [`cache`] — a one-entry, content-keyed cache in front of [`parse`], so
//!      an open preview pane does not re-parse the whole document on every
//!      frame. [`show`] goes through it; [`parse`] itself stays pure.
//!
//! Built against `pulldown-cmark` 0.13 (the 0.11 → 0.13 split moved every block
//! end-tag onto the [`pulldown_cmark::TagEnd`] enum; `Tag::Heading` carries a
//! `level` field; `Tag::List(Option<u64>)` carries the ordered-list start index;
//! `Tag::Link { dest_url, .. }`; `Tag::CodeBlock(CodeBlockKind)`).

mod cache;
mod math;

use egui::{Color32, RichText};
use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use std::cell::RefCell;
use std::collections::HashMap;

/// The markdown extensions this module understands, in ONE place so the block
/// parser, the HTML export and the preview cache key can never disagree.
///
/// Kept deliberately narrow: an extension is enabled only once there is code
/// that RENDERS it. Enabling a flag without a renderer is worse than leaving it
/// off — `pulldown-cmark` would consume the source markers (e.g. `~~`) and the
/// unhandled event would then drop the content silently.
const PARSER_OPTIONS: Options = Options::ENABLE_TASKLISTS
    .union(Options::ENABLE_TABLES)
    .union(Options::ENABLE_FOOTNOTES)
    .union(Options::ENABLE_MATH);

/// Render markdown source to a standalone, self-contained HTML document (for the
/// "Export as HTML" command). Uses pulldown-cmark's own HTML writer — pure Rust,
/// no webview, no network. A minimal embedded stylesheet keeps the output
/// readable on its own.
///
/// # Security (SEC-2 — stored XSS in the exported artifact)
///
/// Unlike the in-app preview (which renders a safe block model with no HTML),
/// the exported `.html` is a file the user opens in a **browser**, where any
/// raw HTML or dangerous-scheme URL from an untrusted markdown document would
/// EXECUTE. pulldown-cmark's default writer passes raw inline/block HTML through
/// verbatim and does not filter `javascript:`/`data:` hrefs, so an export of
/// attacker-supplied markdown could carry `<script>`, `<img onerror=…>`, and
/// `javascript:` links into the browser context of the exported file.
///
/// Defense (mirrors the in-app [`is_safe_link_scheme`] allowlist, no new crate):
///   * Raw-HTML passthrough is DISABLED — every [`Event::Html`] /
///     [`Event::InlineHtml`] is dropped from the stream before the writer sees
///     it, so no author-supplied `<script>`/`onerror=`/`<iframe>` survives.
///   * Every [`Tag::Link`]/[`Tag::Image`] destination is run through
///     [`is_safe_link_scheme`]; a disallowed scheme (`javascript:`, `data:`,
///     `file:`, …) is neutralised to an inert `#` anchor so it can never reach
///     the HTML writer as a live URL.
///   * A restrictive `Content-Security-Policy` meta tag is emitted as
///     defense-in-depth: even if a vector ever slipped past the event filter,
///     the browser is told to run no script and load nothing.
pub fn to_html(md: &str) -> String {
    let body = render_safe_html(md);
    format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta http-equiv=\"Content-Security-Policy\" \
         content=\"default-src 'none'; img-src 'self' http: https: mailto:; \
         style-src 'unsafe-inline'\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <style>\n\
         body{{max-width:46rem;margin:2rem auto;padding:0 1rem;\
         font:16px/1.6 system-ui,sans-serif;color:#1b1b1b}}\n\
         pre,code{{font-family:ui-monospace,monospace}}\n\
         pre{{background:#f4f4f4;padding:.75rem;overflow:auto;border-radius:6px}}\n\
         code{{background:#f4f4f4;padding:.1rem .3rem;border-radius:3px}}\n\
         pre code{{background:none;padding:0}}\n\
         blockquote{{border-left:3px solid #ccc;margin:0;padding-left:1rem;color:#555}}\n\
         table{{border-collapse:collapse}}td,th{{border:1px solid #ccc;padding:.3rem .6rem}}\n\
         th{{background:#f4f4f4}}\n\
         .math{{font-family:ui-serif,Georgia,serif;font-style:italic}}\n\
         .math-display{{display:block;text-align:center;margin:1rem 0}}\n\
         .footnote-definition{{font-size:.9em;color:#555}}\n\
         .footnote-definition p{{display:inline;margin:0}}\n\
         </style>\n</head>\n<body>\n{body}</body>\n</html>\n"
    )
}

/// Build the `<body>` HTML from markdown with raw-HTML passthrough disabled and
/// dangerous link/image schemes neutralised (see [`to_html`] for the threat
/// model). Pure `&str -> String`, no IO — unit-tested below.
fn render_safe_html(md: &str) -> String {
    // Same extension set as the in-app preview ([`PARSER_OPTIONS`]) so an export
    // is never a lossier document than what the user was just looking at. The
    // event filter below is unchanged and still covers the new element types:
    // raw HTML inside a table cell or a footnote body arrives as
    // `Event::InlineHtml`/`Event::Html` and is dropped, and a link inside a
    // table cell is an ordinary `Tag::Link` whose destination is neutralised.
    let safe_events = Parser::new_ext(md, PARSER_OPTIONS).filter_map(|ev| match ev {
        // Drop author-supplied raw HTML entirely. This is the `<script>` /
        // `<img onerror=…>` / `<iframe>` vector — markdown that embeds raw HTML
        // must NOT have it survive into a browser-opened export.
        Event::Html(_) | Event::InlineHtml(_) => None,
        // Neutralise dangerous-scheme link/image destinations to an inert `#`
        // anchor before the HTML writer turns them into a live `href`/`src`.
        Event::Start(Tag::Link {
            link_type,
            dest_url,
            title,
            id,
        }) => Some(Event::Start(Tag::Link {
            link_type,
            dest_url: neutralise_dest(dest_url),
            title,
            id,
        })),
        Event::Start(Tag::Image {
            link_type,
            dest_url,
            title,
            id,
        }) => Some(Event::Start(Tag::Image {
            link_type,
            dest_url: neutralise_dest(dest_url),
            title,
            id,
        })),
        other => Some(other),
    });
    let mut body = String::new();
    pulldown_cmark::html::push_html(&mut body, safe_events);
    body
}

/// Replace a link/image destination with an inert `#` anchor when its scheme is
/// not on the [`is_safe_link_scheme`] allowlist; otherwise pass it through. The
/// `#` fragment resolves against the document and can never invoke a protocol
/// handler (`javascript:`, `data:`, `file:`, …).
fn neutralise_dest(dest_url: pulldown_cmark::CowStr<'_>) -> pulldown_cmark::CowStr<'_> {
    if is_safe_link_scheme(&dest_url) {
        dest_url
    } else {
        pulldown_cmark::CowStr::Borrowed("#")
    }
}

/// A renderable block in document order. Inline styling within a block is
/// flattened to a sequence of [`MdRun`]s.
#[derive(Debug, Clone, PartialEq)]
pub enum MdBlock {
    /// `#`..`######` heading. `level` is 1–6.
    Heading { level: u8, text: String },
    /// A run of inline text (a paragraph).
    Paragraph(Vec<MdRun>),
    /// A fenced or indented code block. `lang` is the fence info-string (first
    /// word only), `None` when absent. `code` keeps original line breaks.
    CodeBlock { lang: Option<String>, code: String },
    /// One list item. `depth` is 0-based nesting. `marker` is the rendered
    /// bullet/ordinal prefix (e.g. `"•"` or `"3."`).
    ListItem {
        depth: u8,
        marker: String,
        runs: Vec<MdRun>,
    },
    /// A GFM task-list item (`- [ ]` / `- [x]`). `checked` is the box state;
    /// `source_line` is the 0-based line of the box in the source so a click in
    /// the preview can edit that exact line. `depth` is 0-based nesting.
    TaskItem {
        depth: u8,
        checked: bool,
        source_line: usize,
        runs: Vec<MdRun>,
    },
    /// A block-quoted paragraph.
    Quote(Vec<MdRun>),
    /// A horizontal rule (`---`).
    Rule,
    /// A GFM pipe table. `aligns` has one entry per column (from the delimiter
    /// row); `header` is one cell per column; `rows` is the body.
    ///
    /// Per the GFM spec the parser itself reconciles a ragged source row against
    /// the header width — a short row is padded with an EMPTY cell, a long one
    /// is truncated — so every row from [`parse`] has `header.len()` cells. The
    /// renderer still pads defensively, because [`MdBlock`] is public and a
    /// caller may hand it a table this module did not parse.
    Table {
        aligns: Vec<MdAlign>,
        header: Vec<Vec<MdRun>>,
        rows: Vec<Vec<Vec<MdRun>>>,
    },
    /// Display math (`$$…$$`). `tex` is the source between the delimiters,
    /// VERBATIM — transliteration to Unicode happens at render time so the
    /// original is never lost (see [`math`]).
    MathBlock { tex: String },
    /// A footnote definition (`[^label]: …`). Collected during the parse and
    /// emitted at the END of the block list, ordered by `number`, after a
    /// [`MdBlock::Rule`] separator — matching how GitHub renders a footnote
    /// section. `body` holds the definition's own blocks.
    FootnoteDef {
        number: usize,
        label: String,
        body: Vec<MdBlock>,
    },
}

/// Column alignment for a [`MdBlock::Table`]. Mirrors
/// [`pulldown_cmark::Alignment`] so the public block model never leaks a parser
/// type (the same reason `HeadingLevel` is mapped to a `u8`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MdAlign {
    /// No explicit alignment in the delimiter row.
    #[default]
    None,
    Left,
    Center,
    Right,
}

/// What an inline run *is*, beyond its styling. Most runs are plain
/// [`MdRunKind::Text`]; the other variants carry content the renderer must
/// treat specially rather than print literally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MdRunKind {
    /// Ordinary prose.
    #[default]
    Text,
    /// Inline math (`$…$`). [`MdRun::text`] is the VERBATIM TeX source.
    InlineMath,
    /// A footnote reference (`[^label]`). [`MdRun::text`] is the footnote's
    /// 1-based number as a string, matching its [`MdBlock::FootnoteDef`].
    FootnoteRef,
}

/// A styled inline run. `link` set means the whole run is a hyperlink.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MdRun {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub link: Option<String>,
    pub kind: MdRunKind,
}

/// Tracks one open ordered/unordered list level and, for ordered lists, the
/// next ordinal to emit.
#[derive(Clone, Copy)]
struct ListLevel {
    /// `Some(next_index)` for an ordered list, `None` for a bullet list.
    ordinal: Option<u64>,
}

/// An open list item being accumulated. Flushed to a [`MdBlock::ListItem`] or
/// [`MdBlock::TaskItem`] (when `task` is set) once its text is complete.
struct PendingItem {
    depth: u8,
    marker: String,
    flushed: bool,
    /// `Some((checked, source_line))` when a `TaskListMarker` was seen.
    task: Option<(bool, usize)>,
}

impl PendingItem {
    /// Build the block for this item from its accumulated `runs`.
    fn to_block(&self, runs: Vec<MdRun>) -> MdBlock {
        match self.task {
            Some((checked, source_line)) => MdBlock::TaskItem {
                depth: self.depth,
                checked,
                source_line,
                runs,
            },
            None => MdBlock::ListItem {
                depth: self.depth,
                marker: self.marker.clone(),
                runs,
            },
        }
    }
}

/// A GFM table being accumulated across its `Table`/`TableHead`/`TableRow`/
/// `TableCell` events.
#[derive(Default)]
struct TableBuilder {
    aligns: Vec<MdAlign>,
    header: Vec<Vec<MdRun>>,
    rows: Vec<Vec<Vec<MdRun>>>,
    /// Cells of the row currently being read (header or body).
    row: Vec<Vec<MdRun>>,
}

/// An open footnote definition. Its own blocks accumulate in `body` so they are
/// never interleaved into the main document flow.
struct PendingFootnote {
    label: String,
    number: usize,
    body: Vec<MdBlock>,
}

/// Route a finished block to the open footnote definition when there is one, or
/// to the document otherwise. Every block push in [`parse`] goes through this —
/// without it, a list or table inside a footnote would surface at the position
/// the footnote's *definition* happens to sit, not inside the note.
fn push_block(blocks: &mut Vec<MdBlock>, footnote: &mut Option<PendingFootnote>, block: MdBlock) {
    match footnote {
        Some(f) => f.body.push(block),
        None => blocks.push(block),
    }
}

/// The 1-based number for a footnote label, assigning the next free one on
/// first sight. Numbering follows order of first appearance, so a reference
/// and its definition always agree.
fn footnote_number(numbers: &mut HashMap<String, usize>, next: &mut usize, label: &str) -> usize {
    if let Some(n) = numbers.get(label) {
        return *n;
    }
    let n = *next;
    *next += 1;
    numbers.insert(label.to_owned(), n);
    n
}

/// Map the parser's column alignment onto the public [`MdAlign`].
fn map_align(a: &Alignment) -> MdAlign {
    match a {
        Alignment::None => MdAlign::None,
        Alignment::Left => MdAlign::Left,
        Alignment::Center => MdAlign::Center,
        Alignment::Right => MdAlign::Right,
    }
}

/// Byte offsets where each source line starts (index 0 = line 0).
fn compute_line_starts(src: &str) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (i, b) in src.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i + 1);
        }
    }
    starts
}

/// Map a byte offset to its 0-based source line via the precomputed line-start
/// table (binary search — the table is sorted ascending).
fn byte_to_line(line_starts: &[usize], offset: usize) -> usize {
    match line_starts.binary_search(&offset) {
        Ok(idx) => idx,
        Err(idx) => idx.saturating_sub(1),
    }
}

/// Parse markdown into the block model. Never panics; malformed or truncated
/// input yields a best-effort block list.
///
/// The parser is a small state machine over `pulldown-cmark` events: inline
/// events accumulate into `runs`, block-end events flush `runs` into the
/// appropriate [`MdBlock`]. Active inline styles (`bold`/`italic`/`code`/`link`)
/// are tracked as a flat set — CommonMark nests these but flattening to the
/// innermost active style is visually sufficient for a preview pane.
pub fn parse(src: &str) -> Vec<MdBlock> {
    let mut blocks: Vec<MdBlock> = Vec::new();
    let mut runs: Vec<MdRun> = Vec::new();

    let (mut bold, mut italic, code) = (false, false, false);
    let mut link: Option<String> = None;

    // Open list levels (outermost first). Used for depth + ordinal markers.
    let mut lists: Vec<ListLevel> = Vec::new();
    // Stack of open list items. An item's own text is flushed (emitted) when a
    // nested list begins, so a parent item appears BEFORE its children in
    // reading order; `flushed` guards against a second emit at item-end. `task`
    // is set (with the box state + source line) when a `TaskListMarker` event
    // follows the item start, turning the flushed block into a `TaskItem`.
    let mut pending: Vec<PendingItem> = Vec::new();
    // Map a source byte offset → 0-based line, for `TaskItem.source_line`.
    let line_starts = compute_line_starts(src);
    // Block-quote nesting depth; > 0 means the next flushed runs are a Quote.
    let mut quote_depth: u32 = 0;

    // Fenced/indented code-block accumulation.
    let mut code_lang: Option<String> = None;
    let mut in_code_block = false;
    let mut code_buf = String::new();

    // GFM table under construction (tables never nest, so one slot suffices).
    let mut table: Option<TableBuilder> = None;

    // Footnotes: the definition currently open, the completed ones (emitted at
    // the end of the document), and the label -> number assignment.
    let mut footnote: Option<PendingFootnote> = None;
    let mut footnotes: Vec<MdBlock> = Vec::new();
    let mut footnote_numbers: HashMap<String, usize> = HashMap::new();
    let mut next_footnote: usize = 1;

    // Append a styled inline run, dropping empty text.
    fn push_run(
        runs: &mut Vec<MdRun>,
        text: &str,
        bold: bool,
        italic: bool,
        code: bool,
        link: &Option<String>,
        kind: MdRunKind,
    ) {
        if !text.is_empty() {
            runs.push(MdRun {
                text: text.to_string(),
                bold,
                italic,
                code,
                link: link.clone(),
                kind,
            });
        }
    }

    for (ev, range) in Parser::new_ext(src, PARSER_OPTIONS).into_offset_iter() {
        match ev {
            // ---- Headings ---------------------------------------------------
            Event::Start(Tag::Heading { .. }) => runs.clear(),
            Event::End(TagEnd::Heading(level)) => {
                let text: String = runs.drain(..).map(|r| r.text).collect();
                push_block(
                    &mut blocks,
                    &mut footnote,
                    MdBlock::Heading {
                        level: heading_to_u8(level),
                        text,
                    },
                );
            }

            // ---- Paragraphs -------------------------------------------------
            Event::Start(Tag::Paragraph) => runs.clear(),
            Event::End(TagEnd::Paragraph) if !runs.is_empty() => {
                let taken = std::mem::take(&mut runs);
                let block = if quote_depth > 0 {
                    MdBlock::Quote(taken)
                } else {
                    MdBlock::Paragraph(taken)
                };
                push_block(&mut blocks, &mut footnote, block);
            }

            // ---- Code blocks ------------------------------------------------
            Event::Start(Tag::CodeBlock(kind)) => {
                in_code_block = true;
                code_buf.clear();
                code_lang = match kind {
                    // Info-string can be `rust ignore` — keep the first word only.
                    CodeBlockKind::Fenced(info) => {
                        let first = info.split_whitespace().next().unwrap_or("");
                        if first.is_empty() {
                            None
                        } else {
                            Some(first.to_string())
                        }
                    }
                    CodeBlockKind::Indented => None,
                };
            }
            Event::End(TagEnd::CodeBlock) => {
                in_code_block = false;
                // Trim the single trailing newline pulldown-cmark appends.
                let mut code = std::mem::take(&mut code_buf);
                if code.ends_with('\n') {
                    code.pop();
                }
                push_block(
                    &mut blocks,
                    &mut footnote,
                    MdBlock::CodeBlock {
                        lang: code_lang.take(),
                        code,
                    },
                );
            }

            // ---- Lists ------------------------------------------------------
            Event::Start(Tag::List(start)) => {
                // A nested list begins inside the current item: flush that item's
                // own text first so the parent is emitted before its children.
                let flushed = pending.last_mut().and_then(|item| {
                    if !item.flushed && !runs.is_empty() {
                        item.flushed = true;
                        Some(item.to_block(std::mem::take(&mut runs)))
                    } else {
                        None
                    }
                });
                if let Some(block) = flushed {
                    push_block(&mut blocks, &mut footnote, block);
                }
                lists.push(ListLevel { ordinal: start });
            }
            Event::End(TagEnd::List(_)) => {
                lists.pop();
            }
            Event::Start(Tag::Item) => {
                // Compute this item's depth + marker now (its ordinal position is
                // known at start); the text accumulates until the item is flushed.
                let depth = lists.len().saturating_sub(1) as u8;
                let marker = match lists.last_mut() {
                    Some(level) => match level.ordinal {
                        Some(n) => {
                            level.ordinal = Some(n + 1);
                            format!("{n}.")
                        }
                        None => "•".to_string(),
                    },
                    None => "•".to_string(),
                };
                runs.clear();
                pending.push(PendingItem {
                    depth,
                    marker,
                    flushed: false,
                    task: None,
                });
            }
            // GFM task box: `ENABLE_TASKLISTS` emits this right after the item
            // start. Record the box state + source line on the open item so it
            // flushes as a `TaskItem` (click-to-source edits this exact line).
            Event::TaskListMarker(checked) => {
                if let Some(item) = pending.last_mut() {
                    let line = byte_to_line(&line_starts, range.start);
                    item.task = Some((checked, line));
                }
            }
            Event::End(TagEnd::Item) => {
                if let Some(item) = pending.pop() {
                    if !item.flushed {
                        let block = item.to_block(std::mem::take(&mut runs));
                        push_block(&mut blocks, &mut footnote, block);
                    } else {
                        // Trailing text after a nested list (uncommon) is dropped
                        // rather than leaking into the next sibling item.
                        runs.clear();
                    }
                }
            }

            // ---- Block quotes ----------------------------------------------
            Event::Start(Tag::BlockQuote(_)) => {
                quote_depth += 1;
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                quote_depth = quote_depth.saturating_sub(1);
            }

            // ---- Tables -----------------------------------------------------
            Event::Start(Tag::Table(aligns)) => {
                table = Some(TableBuilder {
                    aligns: aligns.iter().map(map_align).collect(),
                    ..TableBuilder::default()
                });
                runs.clear();
            }
            Event::Start(Tag::TableHead) | Event::Start(Tag::TableRow) => {
                if let Some(t) = table.as_mut() {
                    t.row.clear();
                }
                runs.clear();
            }
            Event::End(TagEnd::TableHead) => {
                if let Some(t) = table.as_mut() {
                    let head = std::mem::take(&mut t.row);
                    t.header = head;
                }
            }
            Event::End(TagEnd::TableRow) => {
                if let Some(t) = table.as_mut() {
                    let row = std::mem::take(&mut t.row);
                    t.rows.push(row);
                }
            }
            Event::Start(Tag::TableCell) => runs.clear(),
            Event::End(TagEnd::TableCell) => {
                if let Some(t) = table.as_mut() {
                    t.row.push(std::mem::take(&mut runs));
                }
            }
            Event::End(TagEnd::Table) => {
                if let Some(t) = table.take() {
                    push_block(
                        &mut blocks,
                        &mut footnote,
                        MdBlock::Table {
                            aligns: t.aligns,
                            header: t.header,
                            rows: t.rows,
                        },
                    );
                }
            }

            // ---- Footnotes --------------------------------------------------
            Event::FootnoteReference(label) => {
                let n = footnote_number(&mut footnote_numbers, &mut next_footnote, &label);
                push_run(
                    &mut runs,
                    &n.to_string(),
                    bold,
                    italic,
                    code,
                    &link,
                    MdRunKind::FootnoteRef,
                );
            }
            Event::Start(Tag::FootnoteDefinition(label)) => {
                // `pulldown-cmark` never nests definitions, but closing any
                // still-open one keeps the state machine total rather than
                // silently discarding its accumulated body.
                if let Some(prev) = footnote.take() {
                    footnotes.push(MdBlock::FootnoteDef {
                        number: prev.number,
                        label: prev.label,
                        body: prev.body,
                    });
                }
                let number = footnote_number(&mut footnote_numbers, &mut next_footnote, &label);
                runs.clear();
                footnote = Some(PendingFootnote {
                    label: label.to_string(),
                    number,
                    body: Vec::new(),
                });
            }
            Event::End(TagEnd::FootnoteDefinition) => {
                if let Some(f) = footnote.take() {
                    footnotes.push(MdBlock::FootnoteDef {
                        number: f.number,
                        label: f.label,
                        body: f.body,
                    });
                }
            }

            // ---- Math -------------------------------------------------------
            Event::InlineMath(tex) => push_run(
                &mut runs,
                &tex,
                bold,
                italic,
                code,
                &link,
                MdRunKind::InlineMath,
            ),
            Event::DisplayMath(tex) => {
                // `$$…$$` can appear mid-paragraph, so flush any inline runs
                // already collected BEFORE emitting the block — otherwise the
                // text before the formula would be re-ordered after it.
                // Inside a table cell or a list item there is no place for a
                // standalone block, so it stays inline instead.
                if table.is_some() || !pending.is_empty() {
                    push_run(
                        &mut runs,
                        &tex,
                        bold,
                        italic,
                        code,
                        &link,
                        MdRunKind::InlineMath,
                    );
                } else {
                    if !runs.is_empty() {
                        let taken = std::mem::take(&mut runs);
                        let block = if quote_depth > 0 {
                            MdBlock::Quote(taken)
                        } else {
                            MdBlock::Paragraph(taken)
                        };
                        push_block(&mut blocks, &mut footnote, block);
                    }
                    push_block(
                        &mut blocks,
                        &mut footnote,
                        MdBlock::MathBlock {
                            tex: tex.to_string(),
                        },
                    );
                }
            }

            // ---- Inline styling --------------------------------------------
            Event::Start(Tag::Strong) => bold = true,
            Event::End(TagEnd::Strong) => bold = false,
            Event::Start(Tag::Emphasis) => italic = true,
            Event::End(TagEnd::Emphasis) => italic = false,
            Event::Start(Tag::Link { dest_url, .. }) => link = Some(dest_url.to_string()),
            Event::End(TagEnd::Link) => link = None,

            // ---- Leaf content ----------------------------------------------
            Event::Code(s) => push_run(&mut runs, &s, bold, italic, true, &link, MdRunKind::Text),
            Event::Text(s) => {
                if in_code_block {
                    code_buf.push_str(&s);
                } else {
                    push_run(&mut runs, &s, bold, italic, code, &link, MdRunKind::Text);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if in_code_block {
                    code_buf.push('\n');
                } else {
                    push_run(&mut runs, " ", bold, italic, code, &link, MdRunKind::Text);
                }
            }
            Event::Rule => push_block(&mut blocks, &mut footnote, MdBlock::Rule),

            // Everything else (raw HTML, images, definition lists) degrades to
            // its text content via the Text events already handled above.
            _ => {}
        }
    }

    // Flush any dangling runs from truncated input (e.g. an unclosed paragraph),
    // routing them into an unterminated footnote rather than the document.
    if !runs.is_empty() {
        let block = MdBlock::Paragraph(std::mem::take(&mut runs));
        push_block(&mut blocks, &mut footnote, block);
    }
    // Close a footnote definition left open by truncated input.
    if let Some(f) = footnote.take() {
        footnotes.push(MdBlock::FootnoteDef {
            number: f.number,
            label: f.label,
            body: f.body,
        });
    }

    // Footnote definitions render as a section at the END of the document,
    // ordered by number and separated by a rule — GitHub's layout, and the only
    // one that makes sense when a definition may sit anywhere in the source.
    if !footnotes.is_empty() {
        footnotes.sort_by_key(|b| match b {
            MdBlock::FootnoteDef { number, .. } => *number,
            _ => 0,
        });
        blocks.push(MdBlock::Rule);
        blocks.extend(footnotes);
    }

    blocks
}

/// Map a `pulldown-cmark` [`HeadingLevel`] to a 1–6 integer.
fn heading_to_u8(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

thread_local! {
    /// The preview pane's parse cache. Thread-local because egui is driven from
    /// one UI thread, and per-thread so parallel test threads never share state.
    static PREVIEW_CACHE: RefCell<cache::PreviewCache> =
        const { RefCell::new(cache::PreviewCache::new()) };
}

/// Deepest block nesting [`render_blocks`] will descend. Insurance only: the
/// only nesting [`parse`] can produce is a footnote body, and a footnote body
/// can never contain another footnote definition (those are routed to the top
/// level), so the tree is two levels deep by construction. The cap means a
/// future parse change can never turn into a stack overflow on a hostile file.
const MAX_BLOCK_DEPTH: u8 = 6;

/// Shared state threaded through the block renderer.
struct RenderCtx {
    accent: Color32,
    muted: Color32,
    /// Source lines of task checkboxes clicked this frame.
    clicked: Vec<usize>,
    /// Monotonic counter giving each table a unique `egui::Grid` id.
    table_seq: usize,
}

/// Render markdown into an egui [`Ui`] as native widgets.
///
/// `accent` colours headings and links; `muted` colours code. Pass the active
/// theme's colours from the call site.
///
/// The parse result is cached (see [`cache`]) on the source text plus the active
/// parser options, so an open preview pane re-parses only when the document
/// actually changes — not on every frame it is redrawn.
///
/// Returns the source line(s) of any task checkbox the user CLICKED this frame
/// (empty when none). The caller toggles those source lines via
/// [`scribe_core::md_ops::toggle_task_on_lines`] so the click edits the real
/// markdown source (GitHub/GitLab "click edits source" behaviour), never a
/// hidden state.
pub fn show(ui: &mut egui::Ui, md: &str, accent: Color32, muted: Color32) -> Vec<usize> {
    let mut ctx = RenderCtx {
        accent,
        muted,
        clicked: Vec::new(),
        table_seq: 0,
    };
    // The cache borrow is held across rendering (rather than cloning the block
    // list every frame). That is sound because nothing reachable from the egui
    // closures below re-enters `show`.
    PREVIEW_CACHE.with(|cell| {
        let mut cache = cell.borrow_mut();
        render_blocks(ui, cache.blocks(md), &mut ctx, 0);
    });
    ctx.clicked
}

/// Emit one block list. Recurses only for a footnote definition's body.
fn render_blocks(ui: &mut egui::Ui, blocks: &[MdBlock], ctx: &mut RenderCtx, depth: u8) {
    for block in blocks {
        match block {
            MdBlock::TaskItem {
                depth: indent,
                checked,
                source_line,
                runs,
            } => {
                ui.horizontal_wrapped(|ui| {
                    ui.add_space(*indent as f32 * 16.0);
                    // A clickable checkbox; the boolean is local (the source is
                    // the single point of truth) — a click is reported up so the
                    // caller flips the source line.
                    let mut state = *checked;
                    if ui.checkbox(&mut state, "").changed() {
                        ctx.clicked.push(*source_line);
                    }
                    render_runs(ui, runs, ctx.accent, ctx.muted);
                });
            }
            MdBlock::Heading { level, text } => {
                let size = match *level {
                    1 => 26.0,
                    2 => 22.0,
                    3 => 18.0,
                    _ => 15.0,
                };
                ui.add(egui::Label::new(
                    RichText::new(text.as_str())
                        .size(size)
                        .strong()
                        .color(ctx.accent),
                ));
                ui.add_space(2.0);
            }
            MdBlock::Paragraph(runs) => {
                ui.horizontal_wrapped(|ui| render_runs(ui, runs, ctx.accent, ctx.muted));
                ui.add_space(4.0);
            }
            MdBlock::Quote(runs) => {
                // GFM/Obsidian callout: a blockquote whose first run begins with
                // `[!type]` renders with an accent title + indented body (no tag
                // DB — purely presentational).
                if let Some((title, body)) = callout_split(runs) {
                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        ui.label(RichText::new(title).color(ctx.accent).strong().small());
                        if !body.is_empty() {
                            ui.horizontal_wrapped(|ui| {
                                render_runs(ui, &body, ctx.accent, ctx.muted)
                            });
                        }
                    });
                } else {
                    ui.indent("md_quote", |ui| {
                        ui.horizontal_wrapped(|ui| {
                            render_runs(ui, runs, ctx.accent, ctx.muted);
                        });
                    });
                }
                ui.add_space(4.0);
            }
            MdBlock::CodeBlock { code, .. } => {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.add(egui::Label::new(
                        RichText::new(code.as_str()).monospace().color(ctx.muted),
                    ));
                });
                ui.add_space(4.0);
            }
            MdBlock::ListItem {
                depth: indent,
                marker,
                runs,
            } => {
                ui.horizontal_wrapped(|ui| {
                    ui.add_space(*indent as f32 * 16.0);
                    ui.label(RichText::new(marker.as_str()).color(ctx.muted));
                    render_runs(ui, runs, ctx.accent, ctx.muted);
                });
            }
            MdBlock::Rule => {
                ui.separator();
            }
            MdBlock::Table {
                aligns,
                header,
                rows,
            } => render_table(ui, aligns, header, rows, ctx),
            MdBlock::MathBlock { tex } => {
                // No typesetter is available (see the `math` module for why), so
                // display math gets a centred, emphasised Unicode rendering with
                // the original TeX one hover away.
                let pretty = math::math_to_unicode(tex);
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add(egui::Label::new(
                            RichText::new(pretty.as_str()).italics().size(17.0),
                        ))
                        .on_hover_text(format!("$${tex}$$"));
                    });
                });
                ui.add_space(4.0);
            }
            MdBlock::FootnoteDef {
                number,
                label,
                body,
            } => {
                let marker = RichText::new(format!("[{number}]"))
                    .small()
                    .strong()
                    .color(ctx.accent);
                match body.as_slice() {
                    // The overwhelmingly common shape — keep the marker and the
                    // note on one line instead of burning a row on the marker.
                    [MdBlock::Paragraph(runs)] => {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(marker).on_hover_text(label.as_str());
                            render_runs(ui, runs, ctx.accent, ctx.muted);
                        });
                    }
                    rest => {
                        ui.label(marker).on_hover_text(label.as_str());
                        if depth < MAX_BLOCK_DEPTH {
                            ui.indent(("md_footnote", *number), |ui| {
                                render_blocks(ui, rest, ctx, depth + 1);
                            });
                        }
                    }
                }
                ui.add_space(2.0);
            }
        }
    }
}

/// Alignment declared for column `col`, or [`MdAlign::None`] when the source's
/// delimiter row had fewer columns than a body row.
fn align_at(aligns: &[MdAlign], col: usize) -> MdAlign {
    aligns.get(col).copied().unwrap_or_default()
}

/// Render a GFM table as an `egui::Grid`. Ragged rows are padded with blank
/// cells so the column grid stays aligned — the parser never invents content.
fn render_table(
    ui: &mut egui::Ui,
    aligns: &[MdAlign],
    header: &[Vec<MdRun>],
    rows: &[Vec<Vec<MdRun>>],
    ctx: &mut RenderCtx,
) {
    let cols = header
        .len()
        .max(aligns.len())
        .max(rows.iter().map(Vec::len).max().unwrap_or(0));
    if cols == 0 {
        return;
    }
    ctx.table_seq += 1;
    let grid_id = ("md_preview_table", ctx.table_seq);
    egui::Grid::new(grid_id)
        .striped(true)
        .num_columns(cols)
        .show(ui, |ui| {
            if !header.is_empty() {
                for col in 0..cols {
                    render_cell(ui, align_at(aligns, col), header.get(col), ctx, true);
                }
                ui.end_row();
            }
            for row in rows {
                for col in 0..cols {
                    render_cell(ui, align_at(aligns, col), row.get(col), ctx, false);
                }
                ui.end_row();
            }
        });
    ui.add_space(4.0);
}

/// One table cell. `None` is a padding cell for a ragged row.
fn render_cell(
    ui: &mut egui::Ui,
    align: MdAlign,
    runs: Option<&Vec<MdRun>>,
    ctx: &mut RenderCtx,
    header: bool,
) {
    let Some(runs) = runs else {
        ui.label("");
        return;
    };
    let layout = match align {
        MdAlign::Right => egui::Layout::top_down(egui::Align::Max),
        MdAlign::Center => egui::Layout::top_down(egui::Align::Center),
        MdAlign::None | MdAlign::Left => egui::Layout::top_down(egui::Align::Min),
    };
    ui.with_layout(layout, |ui| {
        ui.horizontal(|ui| {
            render_runs_inner(ui, runs, ctx.accent, ctx.muted, header);
        });
    });
}

/// If a blockquote's flattened runs begin with a callout marker `[!type]`,
/// return `(title, body_runs)` where `title` is the upper-cased type and
/// `body` is the remaining content (the `[!type]` token + an optional inline
/// title stripped). Returns `None` for an ordinary blockquote.
fn callout_split(runs: &[MdRun]) -> Option<(String, Vec<MdRun>)> {
    let first = runs.first()?;
    let text = first.text.trim_start();
    let rest = text.strip_prefix("[!")?;
    let close = rest.find(']')?;
    let kind = &rest[..close];
    if kind.is_empty() || !kind.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    let title = format!("{} {}", callout_icon(kind), kind.to_uppercase());
    // The first run's remaining text after `[!type]` becomes part of the body.
    let after = rest[close + 1..].trim_start().to_string();
    let mut body: Vec<MdRun> = Vec::new();
    if !after.is_empty() {
        body.push(MdRun {
            text: after,
            ..first.clone()
        });
    }
    body.extend(runs.iter().skip(1).cloned());
    Some((title, body))
}

/// A small phosphor icon for a callout type (decorative; unknown types use a
/// neutral note glyph).
fn callout_icon(kind: &str) -> &'static str {
    use egui_phosphor::thin as ph;
    match kind.to_ascii_lowercase().as_str() {
        "warning" | "caution" | "danger" => ph::WARNING,
        "tip" | "hint" | "important" => ph::LIGHTBULB,
        "todo" => ph::CHECK_SQUARE,
        "question" | "faq" => ph::QUESTION,
        _ => ph::NOTE,
    }
}

/// S-05 (CWE-79 / CWE-939 — URL-scheme injection). Decide whether a markdown
/// link URL is safe to make CLICKABLE. Only a small allowlist of schemes is
/// permitted; everything else (`javascript:`, `data:`, `file:`, `vbscript:`,
/// …) is rendered as inert text so a malicious markdown document can never
/// hand the user a one-click code-execution / local-file / data-URI vector.
///
/// Allowed:
///   * `http:` / `https:` / `mailto:` (explicit safe schemes)
///   * relative & anchor links (no scheme at all — `./x`, `../x`, `#frag`,
///     `path/page.md`) — these resolve against the document, never a new
///     protocol handler.
///
/// Fail-CLOSED: an unknown scheme is rejected. The check is case-insensitive
/// and tolerant of leading ASCII whitespace / control bytes (the classic
/// `  JavaScript:` and `java\tscript:` obfuscation tricks).
///
/// Pure function — no egui, no IO — so it is exhaustively unit-tested below.
pub(crate) fn is_safe_link_scheme(url: &str) -> bool {
    // Per the WHATWG URL spec, ASCII whitespace and C0 control bytes (NUL,
    // TAB, CR, LF, …) are STRIPPED THROUGHOUT a URL before scheme parsing —
    // not merely from the front. So "java\tscript:" and "  java\nscript:"
    // both collapse to "javascript:" and must be rejected; a naive leading-
    // trim would let the embedded-tab form smuggle past. We remove every
    // ASCII whitespace/control byte, then parse the scheme from what remains.
    let trimmed: String = url
        .chars()
        .filter(|c| !(c.is_ascii_whitespace() || c.is_ascii_control()))
        .collect();

    // Find the scheme delimiter. Per RFC 3986 a scheme is
    // ALPHA *( ALPHA / DIGIT / "+" / "-" / "." ) followed by ':'. If there
    // is no ':' before the first '/', '?', '#' or whitespace, there is no
    // scheme → it is a relative/anchor link, which is always safe.
    let mut scheme = String::new();
    let mut has_scheme = false;
    for (i, c) in trimmed.char_indices() {
        if c == ':' {
            // A ':' as the FIRST char, or after a path separator, is not a
            // scheme delimiter (e.g. "./a:b" or "#a:b").
            has_scheme = i > 0;
            break;
        }
        // Anything that can't be part of a scheme means there is no scheme
        // (it's a relative path like "a/b.md" or "page?x=1#y").
        if c == '/' || c == '?' || c == '#' || c.is_ascii_whitespace() {
            break;
        }
        if c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.' {
            scheme.push(c.to_ascii_lowercase());
        } else {
            // A control/other byte inside the would-be scheme: not a valid
            // scheme → treat as relative (and the rest of the pipeline will
            // render it as text/relative, never a protocol handler).
            break;
        }
    }

    if !has_scheme {
        // No scheme → relative or anchor link → safe.
        return true;
    }

    // The first char of a scheme must be ALPHA (RFC 3986). A leading digit
    // means this wasn't a real scheme.
    if scheme
        .chars()
        .next()
        .is_none_or(|c| !c.is_ascii_alphabetic())
    {
        return false;
    }

    matches!(scheme.as_str(), "http" | "https" | "mailto")
}

/// Emit a sequence of styled inline runs into the current (typically
/// `horizontal_wrapped`) layout.
fn render_runs(ui: &mut egui::Ui, runs: &[MdRun], accent: Color32, muted: Color32) {
    render_runs_inner(ui, runs, accent, muted, false);
}

/// [`render_runs`] with an extra `force_bold`, used for table header cells so a
/// header can be emphasised without cloning and mutating every run.
fn render_runs_inner(
    ui: &mut egui::Ui,
    runs: &[MdRun],
    accent: Color32,
    muted: Color32,
    force_bold: bool,
) {
    for r in runs {
        // Non-prose runs carry content that must NOT be printed literally, so
        // they are dispatched before any of the plain-text handling below (in
        // particular before URL detection, which would otherwise chew on TeX).
        match r.kind {
            MdRunKind::FootnoteRef => {
                // No superscript in egui — a small accent `[n]` is the readable
                // equivalent, and matches the `[n]` marker on the definition.
                ui.label(RichText::new(format!("[{}]", r.text)).small().color(accent))
                    .on_hover_text(format!("footnote {}", r.text));
                continue;
            }
            MdRunKind::InlineMath => {
                let pretty = math::math_to_unicode(&r.text);
                ui.label(RichText::new(pretty).italics())
                    .on_hover_text(format!("${}$", r.text));
                continue;
            }
            MdRunKind::Text => {}
        }
        let bold = r.bold || force_bold;
        if let Some(url) = &r.link {
            if is_safe_link_scheme(url) {
                // egui handles its own link styling/underline; only colour it.
                ui.hyperlink_to(RichText::new(&r.text).color(accent), url);
            } else {
                // S-05 — disallowed scheme (javascript:/data:/file:/…). Render
                // the link TEXT as inert, visually-muted styled text so it is
                // NOT clickable and cannot open a protocol handler.
                ui.label(RichText::new(&r.text).color(muted).strikethrough())
                    .on_hover_text("link blocked: unsafe URL scheme");
            }
            continue;
        }
        // #D P2 — bare-autolink parity: pulldown-cmark does not turn a plain-text
        // `http(s)://…` (not wrapped in `<>` or `[]()`) into a link, so such URLs
        // in body text were inert in the preview while the editor made them
        // clickable. Reuse the editor's `detect_urls` + the same http/https
        // scheme allow-list here so preview matches the editor. Only for plain
        // (non-code, non-emphasis-mixed) runs; a run already carrying `link` took
        // the branch above.
        if !r.code {
            let ranges = scribe_core::url_scan::detect_urls(&r.text);
            if !ranges.is_empty() {
                // Emphasis carries onto the non-URL sub-segments.
                let styled = |s: &str| {
                    let mut rt = RichText::new(s);
                    if bold {
                        rt = rt.strong();
                    }
                    if r.italic {
                        rt = rt.italics();
                    }
                    rt
                };
                let mut pos = 0usize;
                for rng in ranges {
                    if rng.start > pos {
                        ui.label(styled(&r.text[pos..rng.start]));
                    }
                    let url = &r.text[rng.clone()];
                    // Bare autolinks are http/https by construction, but re-check
                    // the allow-list as defence-in-depth (mirrors the link branch).
                    if scribe_core::url_scan::is_clickable_url(url) {
                        ui.hyperlink_to(RichText::new(url).color(accent), url);
                    } else {
                        ui.label(styled(url));
                    }
                    pos = rng.end;
                }
                if pos < r.text.len() {
                    ui.label(styled(&r.text[pos..]));
                }
                continue;
            }
        }
        // `#tag` highlight (preview-only, no tag DB): a plain (non-code) run that
        // contains a `#tag` token is split so each tag renders in the accent
        // colour while the surrounding prose keeps the default style. URLs took
        // the branch above, so a run reaching here has no clickable link.
        if !r.code && !bold && !r.italic && contains_tag(&r.text) {
            render_text_with_tags(ui, &r.text, accent);
            continue;
        }
        let mut rt = RichText::new(&r.text);
        if bold {
            rt = rt.strong();
        }
        if r.italic {
            rt = rt.italics();
        }
        if r.code {
            rt = rt.monospace().color(muted);
        }
        ui.label(rt);
    }
}

/// True when `text` contains at least one `#tag` token (a `#` at a word
/// boundary followed by a tag char).
fn contains_tag(text: &str) -> bool {
    tag_spans(text).iter().any(|(_, _, is_tag)| *is_tag)
}

/// Split `text` into `(start, end, is_tag)` byte spans, where a tag is a `#`
/// preceded by start-of-string/whitespace and followed by 1+ tag chars
/// (alphanumeric / `-` / `_` / `/`). A bare `#` or `# heading` is not a tag.
fn tag_spans(text: &str) -> Vec<(usize, usize, bool)> {
    let bytes = text.as_bytes();
    let mut spans: Vec<(usize, usize, bool)> = Vec::new();
    let mut i = 0;
    let mut seg_start = 0;
    while i < bytes.len() {
        let at_boundary = i == 0 || bytes[i - 1].is_ascii_whitespace();
        if bytes[i] == b'#' && at_boundary {
            // Measure the tag body.
            let mut j = i + 1;
            while j < bytes.len() && is_tag_char(bytes[j]) {
                j += 1;
            }
            if j > i + 1 {
                if seg_start < i {
                    spans.push((seg_start, i, false));
                }
                spans.push((i, j, true));
                i = j;
                seg_start = j;
                continue;
            }
        }
        i += 1;
    }
    if seg_start < bytes.len() {
        spans.push((seg_start, bytes.len(), false));
    }
    spans
}

fn is_tag_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'/'
}

/// Render `text` with each `#tag` token coloured `accent`.
fn render_text_with_tags(ui: &mut egui::Ui, text: &str, accent: Color32) {
    for (start, end, is_tag) in tag_spans(text) {
        let slice = &text[start..end];
        if is_tag {
            ui.label(RichText::new(slice).color(accent).strong());
        } else {
            ui.label(RichText::new(slice));
        }
    }
}

/// Total number of real parses the preview cache has performed on this thread.
/// Test-only: lets the wiring test prove [`show`] goes THROUGH the cache rather
/// than calling [`parse`] directly.
#[cfg(test)]
fn cache_parses() -> u64 {
    PREVIEW_CACHE.with(|c| c.borrow().parses())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Collect a run-bearing block's text into one string for assertions.
    fn runs_text(runs: &[MdRun]) -> String {
        runs.iter().map(|r| r.text.as_str()).collect()
    }

    /// Borrowed view of one parsed table: alignments, header cells, body rows.
    /// Named rather than returned as a bare 3-tuple of nested `Vec`s, which
    /// clippy rightly calls out as unreadable at the call site.
    type TableView<'a> = (&'a [MdAlign], &'a [Vec<MdRun>], &'a [Vec<Vec<MdRun>>]);

    /// The first [`MdBlock::Table`] in `blocks`.
    fn first_table(blocks: &[MdBlock]) -> TableView<'_> {
        blocks
            .iter()
            .find_map(|b| match b {
                MdBlock::Table {
                    aligns,
                    header,
                    rows,
                } => Some((aligns.as_slice(), header.as_slice(), rows.as_slice())),
                _ => None,
            })
            .unwrap_or_else(|| panic!("expected a table, got {blocks:?}"))
    }

    /// Every [`MdBlock::FootnoteDef`] in document order.
    fn footnote_defs(blocks: &[MdBlock]) -> Vec<(usize, &str, &Vec<MdBlock>)> {
        blocks
            .iter()
            .filter_map(|b| match b {
                MdBlock::FootnoteDef {
                    number,
                    label,
                    body,
                } => Some((*number, label.as_str(), body)),
                _ => None,
            })
            .collect()
    }

    fn harness_showing(md: &'static str) -> egui_kittest::Harness<'static> {
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::Vec2::new(700.0, 900.0))
            .build_ui(move |ui| {
                show(
                    ui,
                    md,
                    Color32::from_rgb(0, 0xd0, 0xa0),
                    Color32::from_rgb(0x80, 0x80, 0x80),
                );
            });
        h.run();
        h
    }

    // ---- Footnotes -------------------------------------------------------

    #[test]
    fn footnote_reference_and_definition_are_structured_not_literal_text() {
        // Before ENABLE_FOOTNOTES the parser emitted `[`, `^1`, `]` as three
        // literal Text runs and the definition as an ordinary paragraph reading
        // "[^1]: The footnote body." — i.e. the markup leaked to the reader.
        let b = parse("Text with a note[^1].\n\n[^1]: The footnote body.\n");

        // The reference is a FootnoteRef run carrying the assigned number, and
        // the raw `[^1]` markup is gone from the prose.
        let para = b
            .iter()
            .find_map(|blk| match blk {
                MdBlock::Paragraph(runs) => Some(runs),
                _ => None,
            })
            .unwrap_or_else(|| panic!("expected a paragraph, got {b:?}"));
        let refs: Vec<&MdRun> = para
            .iter()
            .filter(|r| r.kind == MdRunKind::FootnoteRef)
            .collect();
        assert_eq!(refs.len(), 1, "one footnote reference, got {para:?}");
        assert_eq!(refs[0].text, "1");
        assert!(
            !runs_text(para).contains("[^1]"),
            "raw footnote markup must not survive into the prose: {para:?}"
        );

        // The definition is a FootnoteDef, at the END, behind a rule separator.
        let defs = footnote_defs(&b);
        assert_eq!(defs.len(), 1, "got {b:?}");
        assert_eq!(defs[0].0, 1, "numbered to match its reference");
        assert_eq!(defs[0].1, "1", "label preserved");
        assert!(
            matches!(defs[0].2.as_slice(), [MdBlock::Paragraph(runs)]
                if runs_text(runs) == "The footnote body."),
            "definition body, got {:?}",
            defs[0].2
        );
        assert!(
            matches!(b.last(), Some(MdBlock::FootnoteDef { .. })),
            "definitions render last, got {b:?}"
        );
        let rule_at = b.iter().position(|x| matches!(x, MdBlock::Rule));
        let def_at = b
            .iter()
            .position(|x| matches!(x, MdBlock::FootnoteDef { .. }));
        assert!(
            rule_at.is_some() && rule_at < def_at,
            "a rule separates the note section, got {b:?}"
        );
    }

    #[test]
    fn footnotes_are_numbered_by_first_appearance_and_emitted_in_number_order() {
        // References appear b-then-a, but the DEFINITIONS are written a-then-b.
        // Numbering must follow the references, and the emitted section must be
        // ordered by number — not by the order the definitions were written.
        let b = parse("See[^beta] then[^alpha].\n\n[^alpha]: A\n\n[^beta]: B\n");
        let defs = footnote_defs(&b);
        assert_eq!(defs.len(), 2, "got {b:?}");
        assert_eq!(
            (defs[0].0, defs[0].1),
            (1, "beta"),
            "first-referenced note is [1] and comes first, got {defs:?}"
        );
        assert_eq!((defs[1].0, defs[1].1), (2, "alpha"));
        // The in-text markers agree with the definition numbers.
        let markers: Vec<&str> = b
            .iter()
            .filter_map(|blk| match blk {
                MdBlock::Paragraph(runs) => Some(runs),
                _ => None,
            })
            .flatten()
            .filter(|r| r.kind == MdRunKind::FootnoteRef)
            .map(|r| r.text.as_str())
            .collect();
        assert_eq!(markers, vec!["1", "2"]);
    }

    #[test]
    fn a_footnote_body_stays_inside_the_note_and_never_leaks_into_the_document() {
        // A footnote whose body contains a LIST is the case that breaks without
        // the block router: the list items would surface in the main document
        // flow at the position the definition happens to sit.
        let b = parse("x[^n]\n\nafter\n\n[^n]: intro\n\n    - one\n    - two\n");
        let top_level_items = b
            .iter()
            .filter(|blk| matches!(blk, MdBlock::ListItem { .. }))
            .count();
        assert_eq!(
            top_level_items, 0,
            "no list item may appear at document level, got {b:?}"
        );
        let defs = footnote_defs(&b);
        assert_eq!(defs.len(), 1, "got {b:?}");
        let inner_items = defs[0]
            .2
            .iter()
            .filter(|blk| matches!(blk, MdBlock::ListItem { .. }))
            .count();
        assert_eq!(
            inner_items, 2,
            "both items belong to the note body, got {:?}",
            defs[0].2
        );
        // The prose that followed the reference is untouched.
        assert!(
            b.iter().any(|blk| matches!(blk, MdBlock::Paragraph(runs)
                if runs_text(runs) == "after")),
            "got {b:?}"
        );
    }

    #[test]
    fn a_truncated_footnote_definition_is_still_emitted() {
        // Input that ends mid-definition must not swallow the note entirely.
        let b = parse("x[^1]\n\n[^1]: dangling body with no trailing newline");
        let defs = footnote_defs(&b);
        assert_eq!(defs.len(), 1, "got {b:?}");
        assert!(
            format!("{:?}", defs[0].2).contains("dangling body"),
            "got {:?}",
            defs[0].2
        );
    }

    // ---- Tables ----------------------------------------------------------

    #[test]
    fn gfm_table_is_parsed_into_alignment_header_and_rows() {
        // Before ENABLE_TABLES the whole table collapsed into ONE paragraph of
        // literal pipe text ("| a | b |" + softbreak + …).
        let b = parse("| a | b | c |\n|:--|:-:|--:|\n| 1 | 2 | 3 |\n| 4 | 5 | 6 |\n");
        assert!(
            !b.iter().any(|blk| matches!(blk, MdBlock::Paragraph(runs)
                if runs_text(runs).contains('|'))),
            "no literal pipe text may remain, got {b:?}"
        );
        let (aligns, header, rows) = first_table(&b);
        assert_eq!(
            *aligns,
            vec![MdAlign::Left, MdAlign::Center, MdAlign::Right],
            "each delimiter form maps to its alignment"
        );
        let head: Vec<String> = header.iter().map(|c| runs_text(c)).collect();
        assert_eq!(head, vec!["a", "b", "c"]);
        let body: Vec<Vec<String>> = rows
            .iter()
            .map(|r| r.iter().map(|c| runs_text(c)).collect())
            .collect();
        assert_eq!(body, vec![vec!["1", "2", "3"], vec!["4", "5", "6"]]);
    }

    #[test]
    fn table_without_explicit_alignment_defaults_to_none() {
        let b = parse("| a |\n|---|\n| 1 |\n");
        let (aligns, _, _) = first_table(&b);
        assert_eq!(*aligns, vec![MdAlign::None]);
    }

    #[test]
    fn table_cells_keep_inline_styling_and_links() {
        // Cell content is a full inline stream, not a flat string — bold, code
        // and links must survive into the cell's runs.
        let b = parse("| **h** | x |\n|---|---|\n| `c` | [l](https://e.com) |\n");
        let (_, header, rows) = first_table(&b);
        assert!(
            header[0].iter().any(|r| r.bold && r.text == "h"),
            "bold header cell, got {header:?}"
        );
        assert!(
            rows[0][0].iter().any(|r| r.code && r.text == "c"),
            "inline code cell, got {rows:?}"
        );
        let linked = rows[0][1]
            .iter()
            .find(|r| r.link.is_some())
            .unwrap_or_else(|| panic!("expected a link cell, got {rows:?}"));
        assert_eq!(linked.text, "l");
        assert_eq!(linked.link.as_deref(), Some("https://e.com"));
    }

    #[test]
    fn a_ragged_table_row_is_reconciled_to_the_header_width_without_inventing_content() {
        // GFM: a short row is padded with an EMPTY cell and a long row is
        // truncated — `pulldown-cmark` does this itself. What matters here is
        // that the builder passes it through faithfully: every row lands at the
        // header width, the padding cell is genuinely empty, and no text is
        // duplicated into it.
        let b = parse("| a | b |\n|---|---|\n| 1 |\n| 2 | 3 | 4 |\n");
        let (_, header, rows) = first_table(&b);
        assert_eq!(header.len(), 2);
        assert_eq!(rows.len(), 2, "got {rows:?}");

        assert_eq!(rows[0].len(), 2, "short row padded, got {rows:?}");
        assert_eq!(runs_text(&rows[0][0]), "1");
        assert!(
            rows[0][1].is_empty(),
            "the padding cell is empty, not a copy of anything: {rows:?}"
        );

        assert_eq!(rows[1].len(), 2, "long row truncated, got {rows:?}");
        assert_eq!(runs_text(&rows[1][0]), "2");
        assert_eq!(runs_text(&rows[1][1]), "3");
    }

    #[test]
    fn render_table_pads_a_ragged_hand_built_table_instead_of_panicking() {
        // `parse` cannot produce a ragged table (see the test above), but
        // `MdBlock` is public, so the renderer must stay total for a table it
        // did not parse. Indexing instead of padding would panic here.
        use egui_kittest::kittest::Queryable as _;
        fn cell(text: &str) -> Vec<MdRun> {
            vec![MdRun {
                text: text.to_string(),
                ..MdRun::default()
            }]
        }
        let table = MdBlock::Table {
            // Three columns declared, but every row is short and the header
            // shorter still.
            aligns: vec![MdAlign::Left, MdAlign::Center, MdAlign::Right],
            header: vec![cell("onlyhead")],
            rows: vec![vec![cell("solo")], vec![]],
        };
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::Vec2::new(600.0, 400.0))
            .build_ui(move |ui| {
                let mut ctx = RenderCtx {
                    accent: Color32::WHITE,
                    muted: Color32::GRAY,
                    clicked: Vec::new(),
                    table_seq: 0,
                };
                render_blocks(ui, std::slice::from_ref(&table), &mut ctx, 0);
            });
        h.run();
        assert!(h.query_by_label("onlyhead").is_some(), "header cell renders");
        assert!(h.query_by_label("solo").is_some(), "body cell renders");
    }

    #[test]
    fn a_table_inside_a_blockquote_is_still_a_table() {
        let b = parse("> | a |\n> |---|\n> | 1 |\n");
        let (_, header, rows) = first_table(&b);
        assert_eq!(runs_text(&header[0]), "a");
        assert_eq!(runs_text(&rows[0][0]), "1");
    }

    // ---- Math ------------------------------------------------------------

    #[test]
    fn inline_math_is_captured_verbatim_as_its_own_run() {
        // Without ENABLE_MATH this was one literal Text run "Let $a_i$ and
        // $b_i$ be terms." with the `$` delimiters shown to the reader.
        let b = parse("Let $a_i$ and $b_i$ be terms.\n");
        let MdBlock::Paragraph(runs) = &b[0] else {
            panic!("expected a paragraph, got {b:?}")
        };
        let math: Vec<&str> = runs
            .iter()
            .filter(|r| r.kind == MdRunKind::InlineMath)
            .map(|r| r.text.as_str())
            .collect();
        assert_eq!(
            math,
            vec!["a_i", "b_i"],
            "TeX is kept VERBATIM (no delimiters, no transliteration), got {runs:?}"
        );
        assert!(
            !runs_text(runs).contains('$'),
            "delimiters must not reach the reader: {runs:?}"
        );
    }

    #[test]
    fn math_underscores_are_not_eaten_by_the_emphasis_parser() {
        // Two `_` across one expression is the classic corruption: CommonMark
        // reads them as emphasis and the subscripts vanish.
        let b = parse("$a_1 + b_2$\n");
        let MdBlock::Paragraph(runs) = &b[0] else {
            panic!("expected a paragraph, got {b:?}")
        };
        assert_eq!(runs.len(), 1, "one math run, got {runs:?}");
        assert_eq!(runs[0].kind, MdRunKind::InlineMath);
        assert_eq!(runs[0].text, "a_1 + b_2", "both underscores survive");
        assert!(
            !runs.iter().any(|r| r.italic),
            "nothing was reinterpreted as emphasis: {runs:?}"
        );
    }

    #[test]
    fn display_math_becomes_its_own_block_in_document_order() {
        let b = parse("before\n\n$$\nE = mc^2\n$$\n\nafter\n");
        let kinds: Vec<&str> = b
            .iter()
            .map(|blk| match blk {
                MdBlock::Paragraph(_) => "p",
                MdBlock::MathBlock { .. } => "math",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, vec!["p", "math", "p"], "got {b:?}");
        assert!(
            matches!(&b[1], MdBlock::MathBlock { tex } if tex.trim() == "E = mc^2"),
            "TeX kept verbatim, got {b:?}"
        );
    }

    #[test]
    fn display_math_mid_paragraph_does_not_reorder_the_text_around_it() {
        // `$$…$$` can occur inline. The runs collected BEFORE it must be
        // flushed first, or the leading prose would render after the formula.
        let b = parse("lead in $$q$$ trail out\n");
        let texts: Vec<String> = b
            .iter()
            .map(|blk| match blk {
                MdBlock::Paragraph(runs) => format!("p:{}", runs_text(runs).trim()),
                MdBlock::MathBlock { tex } => format!("math:{tex}"),
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(
            texts,
            vec!["p:lead in", "math:q", "p:trail out"],
            "document order preserved, got {b:?}"
        );
    }

    #[test]
    fn display_math_inside_a_list_item_stays_inline() {
        // A block cannot be emitted mid-item without breaking the list, so
        // display math degrades to an inline math run there.
        let b = parse("- item $$z$$ tail\n");
        let items: Vec<&Vec<MdRun>> = b
            .iter()
            .filter_map(|blk| match blk {
                MdBlock::ListItem { runs, .. } => Some(runs),
                _ => None,
            })
            .collect();
        assert_eq!(items.len(), 1, "still one list item, got {b:?}");
        assert!(
            items[0]
                .iter()
                .any(|r| r.kind == MdRunKind::InlineMath && r.text == "z"),
            "math kept inline in the item, got {items:?}"
        );
        assert!(
            !b.iter().any(|blk| matches!(blk, MdBlock::MathBlock { .. })),
            "no stray block was emitted mid-list, got {b:?}"
        );
    }

    // ---- Cache wiring ----------------------------------------------------

    #[test]
    fn show_goes_through_the_cache_and_does_not_reparse_unchanged_source() {
        // The preview pane redraws every frame. This asserts `show` reads its
        // blocks from the cache — calling `parse` directly instead would make
        // the count climb with each frame.
        let md = "# Cache wiring probe\n\nunique body for this test\n";
        let before = cache_parses();
        let mut h = harness_showing(md);
        let after_first = cache_parses();
        assert_eq!(
            after_first,
            before + 1,
            "showing a document parses it exactly once"
        );
        for _ in 0..5 {
            h.run();
        }
        assert_eq!(
            cache_parses(),
            after_first,
            "five more frames of the SAME source must not re-parse"
        );
    }

    // ---- Rendering -------------------------------------------------------

    #[test]
    fn show_renders_tables_footnotes_and_math_headlessly() {
        use egui_kittest::kittest::Queryable as _;
        let h = harness_showing(
            "| left | right |\n|:--|--:|\n| cellone | celltwo |\n\n\
             prose[^ref] here\n\n\
             $$\nE = mc^2\n$$\n\n\
             [^ref]: the note body\n",
        );
        // Table header + body cells reached the accessibility tree.
        assert!(h.query_by_label("left").is_some(), "table header cell");
        assert!(h.query_by_label("right").is_some(), "table header cell");
        assert!(h.query_by_label("cellone").is_some(), "table body cell");
        assert!(h.query_by_label("celltwo").is_some(), "table body cell");
        // The footnote marker renders as `[1]` at BOTH the reference and the
        // definition, and the note body is present.
        assert_eq!(
            h.query_all_by_label("[1]").count(),
            2,
            "the marker appears at the reference AND at the definition"
        );
        assert!(
            h.query_by_label_contains("the note body").is_some(),
            "footnote body"
        );
        // Display math renders transliterated, not as raw TeX.
        assert!(
            h.query_by_label_contains("E = mc²").is_some(),
            "display math rendered as Unicode"
        );
    }

    #[test]
    fn show_renders_inline_math_transliterated_not_as_raw_tex() {
        use egui_kittest::kittest::Queryable as _;
        let h = harness_showing("value $\\alpha_1$ ok\n");
        assert!(
            h.query_by_label("α₁").is_some(),
            "inline math is transliterated for display"
        );
        assert!(
            h.query_by_label_contains("\\alpha").is_none(),
            "raw TeX must not be shown as body text"
        );
    }

    #[test]
    fn render_blocks_stops_descending_past_the_depth_cap() {
        use egui_kittest::kittest::Queryable as _;
        // `parse` cannot currently build this (footnote definitions are always
        // routed to the top level), so the guard is exercised directly on a
        // hand-built tree — otherwise a future parse change could turn nesting
        // into an unbounded recursive walk.
        fn note(number: usize, text: &str, inner: Option<MdBlock>) -> MdBlock {
            let mut body = vec![
                MdBlock::Paragraph(vec![MdRun {
                    text: text.to_string(),
                    ..MdRun::default()
                }]),
                // A second block forces the nesting arm rather than the
                // single-paragraph shortcut.
                MdBlock::Rule,
            ];
            if let Some(i) = inner {
                body.push(i);
            }
            MdBlock::FootnoteDef {
                number,
                label: text.to_string(),
                body,
            }
        }
        // Depth 0 renders "lvl0", then each nested note one level deeper.
        let mut tree = note(99, "deepest", None);
        for level in (0..MAX_BLOCK_DEPTH + 2).rev() {
            tree = note(level as usize, &format!("lvl{level}"), Some(tree));
        }
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::Vec2::new(700.0, 900.0))
            .build_ui(move |ui| {
                let mut ctx = RenderCtx {
                    accent: Color32::WHITE,
                    muted: Color32::GRAY,
                    clicked: Vec::new(),
                    table_seq: 0,
                };
                render_blocks(ui, std::slice::from_ref(&tree), &mut ctx, 0);
            });
        h.run();
        assert!(
            h.query_by_label("lvl0").is_some(),
            "shallow content still renders"
        );
        assert!(
            h.query_by_label("deepest").is_none(),
            "content past the depth cap is not descended into"
        );
    }

    // ---- HTML export -----------------------------------------------------

    #[test]
    fn to_html_exports_tables_footnotes_and_math() {
        // The export used the DEFAULT option set, so none of these rendered —
        // the stylesheet even carried table CSS that nothing could ever match.
        let html = to_html(
            "| a | b |\n|---|--:|\n| 1 | 2 |\n\nnote[^k]\n\n$x^2$\n\n[^k]: body text\n",
        );
        assert!(html.contains("<table>"), "table element missing:\n{html}");
        assert!(html.contains("<th>a</th>"), "header cell missing:\n{html}");
        assert!(
            html.contains("text-align: right"),
            "column alignment missing:\n{html}"
        );
        assert!(
            html.contains("footnote-reference"),
            "footnote reference missing:\n{html}"
        );
        assert!(
            html.contains("footnote-definition"),
            "footnote definition missing:\n{html}"
        );
        assert!(html.contains("body text"), "footnote body missing:\n{html}");
        assert!(
            html.contains("math math-inline"),
            "inline math missing:\n{html}"
        );
        // The stylesheet gained matching rules for the newly-reachable elements.
        assert!(html.contains(".math{"), "math styling missing:\n{html}");
    }

    /// SEC-2 regression for the widened parser: tables and footnotes are NEW
    /// paths into the exported document, so the raw-HTML filter and the
    /// link-scheme allowlist must still hold on them. Against an unfiltered
    /// writer this export carries a live `<script>` in a table cell, another in
    /// the footnote body, and a `javascript:` href.
    #[test]
    fn to_html_keeps_the_new_table_and_footnote_paths_free_of_script_and_bad_schemes() {
        let md = "| <script>alert(1)</script> | <img src=x onerror=alert(1)> |\n\
                  |---|---|\n\
                  | [go](javascript:alert(1)) | ok |\n\n\
                  ref[^p]\n\n\
                  [^p]: <script>alert(2)</script> and [x](javascript:alert(3))\n";
        let html = to_html(md);
        assert!(
            !html.contains("<script>"),
            "a live <script> survived the widened parser:\n{html}"
        );
        assert!(
            !html.contains("onerror="),
            "an event handler survived:\n{html}"
        );
        assert!(
            !html.contains("javascript:"),
            "a javascript: URI survived:\n{html}"
        );
        // The safe structure around the stripped content is still exported.
        assert!(html.contains("<table>"), "table still rendered:\n{html}");
        assert!(html.contains("footnote-definition"), "note rendered:\n{html}");
        assert!(html.contains(">ok<"), "safe cell text kept:\n{html}");
    }

    #[test]
    fn compute_line_starts_points_one_byte_past_each_newline() {
        // Each line starts ONE byte after its '\n'. The `i + 1 -> i * 1` (= i)
        // mutant yields [0,1,4,8] instead. byte_to_line's saturating_sub masked it.
        assert_eq!(compute_line_starts("a\nbb\nccc\n"), vec![0, 2, 5, 9]);
    }

    #[test]
    fn a_second_top_level_list_starts_at_depth_zero() {
        // After the first list ends its level must be popped; a following sibling
        // list's items are depth 0. Deleting the List-end arm leaves the level on
        // the stack -> the second list's items wrongly compute depth 1. Kills 341:13.
        let b = parse("1. a\n2. b\n\n- c\n- d\n");
        let depths: Vec<u8> = b
            .iter()
            .filter_map(|blk| match blk {
                MdBlock::ListItem { depth, .. } => Some(*depth),
                _ => None,
            })
            .collect();
        assert_eq!(
            depths,
            vec![0, 0, 0, 0],
            "two sibling top-level lists are all depth 0, got {b:?}"
        );
    }

    #[test]
    fn strong_end_stops_bold_for_following_text() {
        // Deleting the Strong-End arm makes bold "stick" to following text. Kills 397:13.
        let b = parse("**bold** plain\n");
        let MdBlock::Paragraph(runs) = &b[0] else {
            panic!("expected paragraph")
        };
        assert!(runs.iter().any(|r| r.bold && r.text == "bold"));
        let plain = runs
            .iter()
            .find(|r| r.text.contains("plain"))
            .expect("plain run");
        assert!(!plain.bold, "text after **bold** is not bold");
    }

    #[test]
    fn emphasis_end_stops_italic_for_following_text() {
        // Deleting the Emphasis-End arm makes italic stick. Kills 399:13.
        let b = parse("*em* plain\n");
        let MdBlock::Paragraph(runs) = &b[0] else {
            panic!("expected paragraph")
        };
        assert!(runs.iter().any(|r| r.italic && r.text == "em"));
        let plain = runs
            .iter()
            .find(|r| r.text.contains("plain"))
            .expect("plain run");
        assert!(!plain.italic, "text after *em* is not italic");
    }

    #[test]
    fn link_end_clears_link_for_following_text() {
        // Deleting the Link-End arm makes the link stick to trailing text. Kills 401:13.
        let b = parse("[site](https://example.com) after\n");
        let MdBlock::Paragraph(runs) = &b[0] else {
            panic!("expected paragraph")
        };
        let after = runs
            .iter()
            .find(|r| r.text.contains("after"))
            .expect("trailing run");
        assert_eq!(
            after.link, None,
            "text after the closed link carries no link"
        );
    }

    #[test]
    fn parse_emits_no_trailing_empty_paragraph() {
        // For a well-formed doc runs is drained before the post-loop; the
        // `if !runs.is_empty()` -> `if runs.is_empty()` mutant appends a stray
        // empty Paragraph. Assert exact block count. Kills 428:8.
        let b = parse("# Only\n");
        assert_eq!(
            b.len(),
            1,
            "a single heading yields exactly one block, got {b:?}"
        );
        assert!(matches!(&b[0], MdBlock::Heading { .. }));
    }

    #[test]
    fn callout_split_rejects_a_non_alphanumeric_type() {
        // A `[!a-b]` (non-empty, non-alnum) reaches the type check: clean returns
        // None; the `|| -> &&` mutant accepts it as a callout. Kills 554:24.
        let runs = vec![MdRun {
            text: "[!a-b] body".into(),
            bold: false,
            italic: false,
            code: false,
            link: None,
            kind: MdRunKind::Text,
        }];
        assert!(
            callout_split(&runs).is_none(),
            "a hyphenated type is not a callout"
        );
        let empty = vec![MdRun {
            text: "[!] body".into(),
            bold: false,
            italic: false,
            code: false,
            link: None,
            kind: MdRunKind::Text,
        }];
        assert!(
            callout_split(&empty).is_none(),
            "an empty type is not a callout"
        );
    }

    #[test]
    fn callout_icon_maps_each_type_group_to_its_glyph() {
        // callout_split's title assert only checks the TYPE, never the icon, so
        // the whole-body / deleted-arm mutants survived. Pin each group. Kills
        // 574:5(x2), 576:9, 577:9, 578:9, 579:9.
        use egui_phosphor::thin as ph;
        assert_eq!(callout_icon("warning"), ph::WARNING);
        assert_eq!(callout_icon("caution"), ph::WARNING);
        assert_eq!(callout_icon("danger"), ph::WARNING);
        assert_eq!(callout_icon("tip"), ph::LIGHTBULB);
        assert_eq!(callout_icon("hint"), ph::LIGHTBULB);
        assert_eq!(callout_icon("important"), ph::LIGHTBULB);
        assert_eq!(callout_icon("todo"), ph::CHECK_SQUARE);
        assert_eq!(callout_icon("question"), ph::QUESTION);
        assert_eq!(callout_icon("faq"), ph::QUESTION);
        assert_eq!(callout_icon("unknown"), ph::NOTE);
        assert_eq!(callout_icon("WARNING"), ph::WARNING, "case-insensitive");
    }

    #[test]
    fn link_scheme_parses_plus_minus_dot_scheme_chars_then_rejects_unknown() {
        // '+','-','.' are valid RFC-3986 scheme chars: they must be consumed so an
        // unknown scheme is REJECTED. The `==`->`!=` and `||`->`&&` scheme-char
        // mutants break parsing at that char -> mis-classify as a relative link.
        // Kills 631:43, 631:50, 631:55, 631:62, 631:67.
        assert!(!is_safe_link_scheme("coap+ws://host"));
        assert!(!is_safe_link_scheme("view-source:http://x"));
        assert!(!is_safe_link_scheme("a.b://host"));
    }

    #[test]
    fn tag_spans_segment_boundaries_are_pinned_both_sides() {
        // The hashtag-only existing test missed the non-tag segment boundaries.
        // Kills 763:30(x3), 774:18(x3).
        assert_eq!(tag_spans("a #b"), vec![(0, 2, false), (2, 4, true)]);
        assert_eq!(tag_spans("#a b"), vec![(0, 2, true), (2, 4, false)]);
    }

    #[test]
    fn image_only_paragraph_emits_no_empty_block() {
        // An image with empty alt yields no inline runs, so Paragraph-end has empty
        // runs and the guard skips it. The `!runs.is_empty()` -> `true` mutant
        // pushes a stray empty Paragraph. Kills 290:46 (if pulldown nests as expected).
        let b = parse("![](http://x/y.png)\n\nreal text\n");
        assert_eq!(
            b.len(),
            1,
            "no empty block for the image-only paragraph, got {b:?}"
        );
        assert!(matches!(&b[0], MdBlock::Paragraph(runs) if runs_text(runs) == "real text"));
    }

    #[test]
    fn textless_parent_item_flushes_after_its_children_not_before() {
        // A textless parent item has EMPTY runs when the nested list starts; clean
        // does not early-flush (child first, empty parent last). The `&& -> ||`
        // mutant flushes the empty parent first -> items swap order. Kills 334:38.
        let b = parse("- \n    - child\n");
        let depths: Vec<u8> = b
            .iter()
            .filter_map(|blk| match blk {
                MdBlock::ListItem { depth, .. } => Some(*depth),
                _ => None,
            })
            .collect();
        assert_eq!(
            depths,
            vec![1, 0],
            "child (depth 1) before the textless parent (depth 0), got {b:?}"
        );
    }

    #[test]
    fn parses_task_items_with_state_and_source_line() {
        // Task boxes become TaskItem blocks carrying their checked state and the
        // 0-based source line of the box (for click-to-source editing).
        let src = "intro\n\n- [ ] unchecked\n- [x] checked\n- plain bullet\n";
        let b = parse(src);
        let tasks: Vec<(&bool, &usize)> = b
            .iter()
            .filter_map(|blk| match blk {
                MdBlock::TaskItem {
                    checked,
                    source_line,
                    ..
                } => Some((checked, source_line)),
                _ => None,
            })
            .collect();
        assert_eq!(tasks.len(), 2, "got {b:?}");
        assert_eq!((*tasks[0].0, *tasks[0].1), (false, 2));
        assert_eq!((*tasks[1].0, *tasks[1].1), (true, 3));
        // The plain bullet stays a ListItem (not a task).
        assert!(b
            .iter()
            .any(|blk| matches!(blk, MdBlock::ListItem { runs, .. }
            if runs_text(runs).contains("plain bullet"))));
    }

    #[test]
    fn callout_split_detects_admonition() {
        let runs = vec![MdRun {
            text: "[!warning] be careful".into(),
            bold: false,
            italic: false,
            code: false,
            link: None,
            kind: MdRunKind::Text,
        }];
        let (title, body) = callout_split(&runs).expect("callout");
        assert!(title.contains("WARNING"));
        assert_eq!(runs_text(&body), "be careful");
        // A plain quote is not a callout.
        let plain = vec![MdRun {
            text: "just a quote".into(),
            bold: false,
            italic: false,
            code: false,
            link: None,
            kind: MdRunKind::Text,
        }];
        assert!(callout_split(&plain).is_none());
    }

    #[test]
    fn tag_spans_isolates_hashtags() {
        let spans = tag_spans("see #idea and #to-do/now end");
        let tags: Vec<&str> = spans
            .iter()
            .filter(|(_, _, t)| *t)
            .map(|(s, e, _)| &"see #idea and #to-do/now end"[*s..*e])
            .collect();
        assert_eq!(tags, vec!["#idea", "#to-do/now"]);
        // A `#` not at a word boundary (e.g. a fragment) or a bare heading `# ` is
        // not a tag.
        assert!(!contains_tag("# heading"));
        assert!(!contains_tag("a#b"));
        assert!(contains_tag("#tag"));
    }

    #[test]
    fn byte_to_line_maps_offsets() {
        let starts = compute_line_starts("a\nbb\nccc\n");
        assert_eq!(byte_to_line(&starts, 0), 0);
        assert_eq!(byte_to_line(&starts, 2), 1); // start of "bb"
        assert_eq!(byte_to_line(&starts, 3), 1);
        assert_eq!(byte_to_line(&starts, 5), 2); // start of "ccc"
    }

    #[test]
    fn parses_heading_levels() {
        let b = parse("# One\n\n## Two\n\n### Three\n");
        assert!(matches!(&b[0], MdBlock::Heading { level: 1, text } if text == "One"));
        assert!(matches!(&b[1], MdBlock::Heading { level: 2, text } if text == "Two"));
        assert!(matches!(&b[2], MdBlock::Heading { level: 3, text } if text == "Three"));
    }

    #[test]
    fn parses_paragraph_with_emphasis() {
        let b = parse("Hello **bold** and *italic* text\n");
        match &b[0] {
            MdBlock::Paragraph(runs) => {
                assert_eq!(runs_text(runs), "Hello bold and italic text");
                assert!(runs.iter().any(|r| r.bold && r.text == "bold"));
                assert!(runs.iter().any(|r| r.italic && r.text == "italic"));
            }
            other => panic!("expected paragraph, got {other:?}"),
        }
    }

    #[test]
    fn parses_inline_and_fenced_code() {
        let b = parse("Use `cargo build` here.\n\n```rust\nlet x = 1;\nlet y = 2;\n```\n");
        // Inline code run.
        match &b[0] {
            MdBlock::Paragraph(runs) => {
                assert!(runs.iter().any(|r| r.code && r.text == "cargo build"));
            }
            other => panic!("expected paragraph, got {other:?}"),
        }
        // Fenced block: lang captured, both lines present, no trailing newline.
        match &b[1] {
            MdBlock::CodeBlock { lang, code } => {
                assert_eq!(lang.as_deref(), Some("rust"));
                assert!(code.contains("let x = 1;"));
                assert!(code.contains("let y = 2;"));
                assert!(!code.ends_with('\n'));
            }
            other => panic!("expected code block, got {other:?}"),
        }
    }

    #[test]
    fn parses_ordered_and_bullet_lists() {
        // Ordered list emits incrementing ordinals; bullet list emits "•".
        let b = parse("1. first\n2. second\n3. third\n\n- a\n- b\n");
        let markers: Vec<&str> = b
            .iter()
            .filter_map(|blk| match blk {
                MdBlock::ListItem { marker, .. } => Some(marker.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(markers, vec!["1.", "2.", "3.", "•", "•"]);
    }

    #[test]
    fn to_html_wraps_a_standalone_document() {
        let html = to_html("# Title\n\nSome **bold** text.\n");
        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(html.contains("<h1>Title</h1>"));
        assert!(html.contains("<strong>bold</strong>"));
        assert!(html.trim_end().ends_with("</html>"));
    }

    #[test]
    fn parses_nested_list_depth() {
        let b = parse("- outer\n    - inner\n");
        let depths: Vec<u8> = b
            .iter()
            .filter_map(|blk| match blk {
                MdBlock::ListItem { depth, .. } => Some(*depth),
                _ => None,
            })
            .collect();
        assert_eq!(depths, vec![0, 1]);
    }

    #[test]
    fn parses_link() {
        let b = parse("See [the site](https://example.com) now\n");
        match &b[0] {
            MdBlock::Paragraph(runs) => {
                let linked = runs
                    .iter()
                    .find(|r| r.link.is_some())
                    .expect("expected a linked run");
                assert_eq!(linked.text, "the site");
                assert_eq!(linked.link.as_deref(), Some("https://example.com"));
            }
            other => panic!("expected paragraph, got {other:?}"),
        }
    }

    #[test]
    fn parses_blockquote_and_rule() {
        let b = parse("> quoted line\n\n---\n");
        assert!(matches!(&b[0], MdBlock::Quote(runs) if runs_text(runs) == "quoted line"));
        assert!(matches!(&b[1], MdBlock::Rule));
    }

    #[test]
    fn malformed_input_does_not_panic() {
        // Unclosed emphasis, unterminated link, dangling fence — must not panic
        // and must still return some best-effort blocks.
        let _ = parse("**unclosed [link](http://\n\n```\nno close fence");
        let _ = parse("");
        let _ = parse("###### h6 only");
    }

    #[test]
    fn parses_all_six_heading_levels_to_u8() {
        // Covers the H4/H5/H6 arms of heading_to_u8 that the 1-3 test missed.
        let b = parse("#### Four\n\n##### Five\n\n###### Six\n");
        assert!(matches!(&b[0], MdBlock::Heading { level: 4, text } if text == "Four"));
        assert!(matches!(&b[1], MdBlock::Heading { level: 5, text } if text == "Five"));
        assert!(matches!(&b[2], MdBlock::Heading { level: 6, text } if text == "Six"));
    }

    #[test]
    fn heading_to_u8_maps_every_level() {
        assert_eq!(heading_to_u8(HeadingLevel::H1), 1);
        assert_eq!(heading_to_u8(HeadingLevel::H2), 2);
        assert_eq!(heading_to_u8(HeadingLevel::H3), 3);
        assert_eq!(heading_to_u8(HeadingLevel::H4), 4);
        assert_eq!(heading_to_u8(HeadingLevel::H5), 5);
        assert_eq!(heading_to_u8(HeadingLevel::H6), 6);
    }

    #[test]
    fn fenced_code_info_string_keeps_only_first_word() {
        // `rust ignore` is a real CommonMark info-string; only `rust` is the lang.
        let b = parse("```rust ignore\nlet x = 1;\n```\n");
        match &b[0] {
            MdBlock::CodeBlock { lang, code } => {
                assert_eq!(lang.as_deref(), Some("rust"));
                assert!(code.contains("let x = 1;"));
            }
            other => panic!("expected code block, got {other:?}"),
        }
    }

    #[test]
    fn fenced_code_without_lang_has_none() {
        // Bare fence: info-string empty => lang None (the `first.is_empty()` arm).
        let b = parse("```\nplain\n```\n");
        match &b[0] {
            MdBlock::CodeBlock { lang, code } => {
                assert_eq!(*lang, None);
                assert_eq!(code, "plain");
            }
            other => panic!("expected code block, got {other:?}"),
        }
    }

    #[test]
    fn indented_code_block_has_no_lang() {
        // A 4-space indented block is CodeBlockKind::Indented => lang None.
        let b = parse("    indented code line\n");
        let has_indented = b.iter().any(|blk| {
            matches!(blk, MdBlock::CodeBlock { lang, code }
                if lang.is_none() && code.contains("indented code line"))
        });
        assert!(has_indented, "expected an indented code block, got {b:?}");
    }

    #[test]
    fn code_block_preserves_internal_newlines_but_trims_trailing() {
        // Exercises the multi-line buffer + the single trailing-newline pop.
        let b = parse("```\na\nb\nc\n```\n");
        match &b[0] {
            MdBlock::CodeBlock { code, .. } => {
                assert_eq!(code, "a\nb\nc");
                assert!(!code.ends_with('\n'));
            }
            other => panic!("expected code block, got {other:?}"),
        }
    }

    #[test]
    fn nested_list_flushes_parent_item_before_children() {
        // The parent item's own text must be emitted BEFORE its nested children
        // (the `flushed` flag path in Start(Tag::List)). Parent appears first.
        let b = parse("- parent text\n    - child a\n    - child b\n");
        let items: Vec<(&u8, &str, String)> = b
            .iter()
            .filter_map(|blk| match blk {
                MdBlock::ListItem {
                    depth,
                    marker,
                    runs,
                } => Some((depth, marker.as_str(), runs_text(runs))),
                _ => None,
            })
            .collect();
        assert_eq!(items.len(), 3, "got {items:?}");
        // Parent (depth 0) emitted first, then the two depth-1 children.
        assert_eq!(*items[0].0, 0);
        assert_eq!(items[0].2, "parent text");
        assert_eq!(*items[1].0, 1);
        assert_eq!(items[1].2, "child a");
        assert_eq!(*items[2].0, 1);
        assert_eq!(items[2].2, "child b");
    }

    #[test]
    fn ordered_list_ordinals_increment_from_custom_start() {
        // pulldown-cmark passes the list start index; markers must reflect it.
        let b = parse("5. five\n6. six\n7. seven\n");
        let markers: Vec<&str> = b
            .iter()
            .filter_map(|blk| match blk {
                MdBlock::ListItem { marker, .. } => Some(marker.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(markers, vec!["5.", "6.", "7."]);
    }

    #[test]
    fn nested_blockquote_paragraph_is_a_quote() {
        // quote_depth > 0 at paragraph-end routes runs to MdBlock::Quote.
        let b = parse("> outer\n>\n> still quoted\n");
        assert!(
            b.iter().any(|blk| matches!(blk, MdBlock::Quote(_))),
            "expected a Quote block, got {b:?}"
        );
        // After the quote closes, a plain paragraph is NOT a quote.
        let b2 = parse("> quoted\n\nplain after\n");
        assert!(matches!(&b2[0], MdBlock::Quote(_)));
        assert!(
            b2.iter().any(|blk| matches!(blk, MdBlock::Paragraph(_))),
            "expected a trailing Paragraph, got {b2:?}"
        );
    }

    #[test]
    fn soft_break_joins_lines_with_space() {
        // A soft line break inside a paragraph becomes a single space run.
        let b = parse("line one\nline two\n");
        match &b[0] {
            MdBlock::Paragraph(runs) => {
                assert_eq!(runs_text(runs), "line one line two");
            }
            other => panic!("expected paragraph, got {other:?}"),
        }
    }

    #[test]
    fn hard_break_inside_code_block_keeps_newline() {
        // Inside a fence, a break is a literal newline, not a space.
        let b = parse("```\nfirst\nsecond\n```\n");
        match &b[0] {
            MdBlock::CodeBlock { code, .. } => assert_eq!(code, "first\nsecond"),
            other => panic!("expected code block, got {other:?}"),
        }
    }

    #[test]
    fn trailing_runs_from_truncated_paragraph_are_flushed() {
        // A document that ends mid-paragraph (no blank-line close) still emits
        // the dangling runs via the post-loop flush.
        let b = parse("dangling text with no trailing newline close");
        assert!(
            b.iter().any(|blk| matches!(blk, MdBlock::Paragraph(runs)
                if runs_text(runs).contains("dangling text"))),
            "expected the dangling paragraph to be flushed, got {b:?}"
        );
    }

    // --- SEC-2 (CWE-79): exported HTML must not carry executable content ---

    /// Red-first guard for SEC-2: the "Export as HTML" output is opened in a
    /// browser, so author-supplied raw HTML and dangerous-scheme links must be
    /// stripped/neutralised. Against the OLD unfiltered `push_html`, the export
    /// contained a live `<script>`, an `onerror=` attribute, and a `javascript:`
    /// href — this test asserts NONE of them survive after the fix.
    #[test]
    fn to_html_strips_raw_html_and_dangerous_schemes() {
        let md = "Intro text\n\n\
                  <script>alert(1)</script>\n\n\
                  An image: <img src=x onerror=alert(1)>\n\n\
                  A [click me](javascript:alert(1)) link\n\n\
                  A markdown ![pic](javascript:alert(1)) image\n\n\
                  A safe [home](https://example.com) link\n";
        let html = to_html(md);

        // No live <script> element survives (raw HTML dropped). pulldown-cmark
        // escapes any leftover angle brackets to entities, so the literal tag
        // must not appear.
        assert!(
            !html.contains("<script>"),
            "exported HTML still contains a live <script> tag:\n{html}"
        );
        // No event-handler attribute survives (the raw <img onerror=…> is gone).
        assert!(
            !html.contains("onerror="),
            "exported HTML still contains an onerror= handler:\n{html}"
        );
        // No javascript: URI survives in any href/src (link + image dests were
        // neutralised to '#').
        assert!(
            !html.contains("javascript:"),
            "exported HTML still contains a javascript: URI:\n{html}"
        );

        // The safe content is preserved: the body text and the allowlisted
        // https link must still be present + clickable.
        assert!(html.contains("Intro text"), "lost safe body text:\n{html}");
        assert!(
            html.contains("href=\"https://example.com\""),
            "safe https link was incorrectly stripped:\n{html}"
        );
    }

    #[test]
    fn to_html_renders_lists_and_code() {
        let html = to_html("- a\n- b\n\n```\ncode\n```\n");
        assert!(html.contains("<ul>"));
        assert!(html.contains("<li>a</li>"));
        assert!(html.contains("<pre><code>"));
        // The embedded stylesheet is always present.
        assert!(html.contains("font-family:ui-monospace"));
    }

    /// Render the full block model through the real egui `show` path so the
    /// per-variant widget arms (heading sizes, quote indent, code frame, list
    /// indent, rule, links, bold/italic/code runs) are all executed. Headless
    /// via egui_kittest — no GPU. We assert the rendered AccessKit tree exposes
    /// the heading + link text, proving `show`/`render_runs` ran end to end.
    #[test]
    fn show_renders_every_block_variant_headlessly() {
        use egui_kittest::kittest::Queryable as _;
        let md = "# Big Heading\n\n#### Small Heading\n\n\
                  Para with **bold** *italic* and `code` and [a link](https://e.com)\n\n\
                  > a quote\n\n\
                  ```rust\nfn main() {}\n```\n\n\
                  - bullet one\n    - nested two\n\n\
                  1. ordinal one\n\n\
                  ---\n";
        let accent = Color32::from_rgb(0x00, 0xd0, 0xa0);
        let muted = Color32::from_rgb(0x80, 0x80, 0x80);
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::Vec2::new(600.0, 800.0))
            .build_ui(move |ui| {
                show(ui, md, accent, muted);
            });
        h.run();
        // The heading text and the link text reached the accessibility tree.
        assert!(h.query_by_label("Big Heading").is_some());
        assert!(h.query_by_label("Small Heading").is_some());
        assert!(h.query_by_label("a link").is_some());
    }

    /// #D P2 — a BARE `http(s)://` URL in body text (not `[]()` nor `<>`) must
    /// render as a clickable hyperlink in the preview, matching the editor.
    /// `ui.hyperlink_to` exposes a `Link` role node in the AccessKit tree, while
    /// a plain `ui.label` would not — so a queryable Link node proves the fix.
    #[test]
    fn preview_makes_bare_autolinks_clickable() {
        // A bare `http(s)://` URL is split out of the surrounding paragraph text
        // into its OWN `hyperlink_to` node (proven by the URL becoming an
        // independently-labelled node). Without the D-P2 fix the whole line is a
        // single plain label and the URL sub-string is not a queryable node — so
        // this label-split is the discriminating signal that the fix is live.
        use egui_kittest::kittest::Queryable as _;
        let md = "see http://bare.example.com/x for details\n";
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::Vec2::new(600.0, 400.0))
            .build_ui(move |ui| {
                show(
                    ui,
                    md,
                    Color32::from_rgb(0, 0xd0, 0xa0),
                    Color32::from_rgb(0x80, 0x80, 0x80),
                );
            });
        h.run();
        assert!(
            h.query_by_label("http://bare.example.com/x").is_some(),
            "a bare URL must be split into its own clickable link node"
        );
        // The surrounding prose is present as its own (non-URL) node too.
        assert!(
            h.query_by_label_contains("for details").is_some(),
            "surrounding prose must still render"
        );
    }

    #[test]
    fn show_empty_source_renders_nothing_without_panic() {
        let mut h = egui_kittest::Harness::builder().build_ui(|ui| {
            show(ui, "", Color32::WHITE, Color32::GRAY);
        });
        h.run();
    }

    // --- S-05 (CWE-79 / CWE-939): markdown link scheme allowlist ---

    #[test]
    fn link_scheme_rejects_dangerous_schemes() {
        // The whole point of the fix: these must NEVER be made clickable.
        assert!(!is_safe_link_scheme("javascript:alert(1)"));
        assert!(!is_safe_link_scheme(
            "data:text/html,<script>alert(1)</script>"
        ));
        assert!(!is_safe_link_scheme("file:///etc/passwd"));
        assert!(!is_safe_link_scheme("vbscript:msgbox(1)"));
        // UNC-ish / other protocol handlers also rejected.
        assert!(!is_safe_link_scheme("ftp://host/x"));
        assert!(!is_safe_link_scheme("smb://attacker/share"));
    }

    #[test]
    fn link_scheme_allows_safe_schemes_and_relative() {
        assert!(is_safe_link_scheme("http://example.com"));
        assert!(is_safe_link_scheme("https://example.com/x?y=1#z"));
        assert!(is_safe_link_scheme("mailto:user@example.com"));
        // Relative / anchor links carry no scheme → always safe.
        assert!(is_safe_link_scheme("./page.md"));
        assert!(is_safe_link_scheme("../other/page.md"));
        assert!(is_safe_link_scheme("page.md"));
        assert!(is_safe_link_scheme("#section-2"));
        assert!(is_safe_link_scheme("path/page?x=1#y"));
        // A relative path that happens to contain a colon AFTER a slash is
        // still relative (no scheme delimiter before the first '/').
        assert!(is_safe_link_scheme("./a:b"));
    }

    #[test]
    fn link_scheme_is_case_insensitive_and_strips_obfuscation() {
        // Case-insensitive.
        assert!(!is_safe_link_scheme("JavaScript:alert(1)"));
        assert!(!is_safe_link_scheme("JAVASCRIPT:alert(1)"));
        assert!(is_safe_link_scheme("HTTPS://example.com"));
        assert!(is_safe_link_scheme("MailTo:user@example.com"));
        // Leading-whitespace trick.
        assert!(!is_safe_link_scheme("   JavaScript:alert(1)"));
        // Leading control bytes (TAB / NEWLINE / CR / NUL) are stripped before
        // scheme parsing — the classic "java\tscript:" smuggle.
        assert!(!is_safe_link_scheme("\tjavascript:alert(1)"));
        assert!(!is_safe_link_scheme("\n\r javascript:alert(1)"));
        assert!(!is_safe_link_scheme("\u{0000}javascript:alert(1)"));
        // Embedded control inside the scheme also fails closed.
        assert!(!is_safe_link_scheme("java\tscript:alert(1)"));
    }

    #[test]
    fn link_scheme_empty_and_degenerate_inputs() {
        // Empty / scheme-only-colon inputs are treated as relative (safe to
        // render — they cannot open a protocol handler).
        assert!(is_safe_link_scheme(""));
        assert!(is_safe_link_scheme(":"));
        // A leading-digit "scheme" is not a valid RFC-3986 scheme → reject so
        // "123:foo" can never be coerced into a handler.
        assert!(!is_safe_link_scheme("1http://x"));
    }
}
