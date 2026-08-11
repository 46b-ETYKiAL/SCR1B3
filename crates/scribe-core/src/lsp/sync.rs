//! `textDocument/didChange` document synchronisation.
//!
//! Three pure pieces, all testable without a child process:
//!
//!   * [`TextDocumentSyncKind`] — what the server asked for in its `initialize`
//!     result. A server that declared `Incremental` is sent a ranged edit; one
//!     that declared `Full` is sent the whole document; one that declared `None`
//!     is sent nothing at all.
//!   * [`content_changes`] — turns an (old, new) text pair into the
//!     `contentChanges` array for the declared kind. The incremental form is a
//!     common-prefix/common-suffix diff, so a one-character keystroke sends a
//!     one-character edit instead of the whole buffer.
//!   * [`ChangeDebouncer`] — coalesces a burst of keystrokes into ONE send. The
//!     clock is a parameter, never read internally, so the debounce window is
//!     asserted directly rather than slept through.
//!
//! ## Positions are UTF-16 code units
//!
//! LSP `Position.character` counts UTF-16 code units, not bytes and not chars
//! (the default `PositionEncodingKind`). `é` is one unit, `😀` is two, and a
//! byte offset is neither. [`utf16_position`] and [`byte_offset_of_position`]
//! are the conversion pair — used on the way out for `didChange` ranges and on
//! the way in for painting a diagnostic's range over the editor's text.

use serde_json::{json, Value};
use std::time::{Duration, Instant};

/// How the server wants document changes delivered (LSP `TextDocumentSyncKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextDocumentSyncKind {
    /// `0` — the server does not want change notifications at all.
    NoSync,
    /// `1` — send the full document text on every change.
    Full,
    /// `2` — send only the changed range.
    Incremental,
}

impl TextDocumentSyncKind {
    /// Decode the LSP wire number. Anything else is `None` (unknown servers are
    /// handled by the caller's default, never guessed at here).
    pub fn from_wire(n: i64) -> Option<Self> {
        match n {
            0 => Some(Self::NoSync),
            1 => Some(Self::Full),
            2 => Some(Self::Incremental),
            _ => None,
        }
    }

    /// The wire number, for round-tripping through an [`std::sync::atomic::AtomicU8`].
    pub fn to_wire(self) -> u8 {
        match self {
            Self::NoSync => 0,
            Self::Full => 1,
            Self::Incremental => 2,
        }
    }

    /// Read the declared sync kind out of an `initialize` RESULT message.
    ///
    /// `capabilities.textDocumentSync` has two legal shapes in the spec: a bare
    /// number (`2`), or a `TextDocumentSyncOptions` object whose `change` field
    /// carries the number (`{ "openClose": true, "change": 2 }`). Real servers
    /// ship both — rust-analyzer sends the object, several smaller servers send
    /// the number — so both are decoded here. Returns `None` when the server
    /// declared nothing (the spec's "unspecified" case).
    pub fn from_initialize_result(msg: &Value) -> Option<Self> {
        let sync = msg
            .get("result")?
            .get("capabilities")?
            .get("textDocumentSync")?;
        if let Some(n) = sync.as_i64() {
            return Self::from_wire(n);
        }
        Self::from_wire(sync.get("change")?.as_i64()?)
    }
}

/// A single `TextDocumentContentChangeEvent`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentChange {
    /// `(start_line, start_char, end_line, end_char)` in the PRE-change
    /// document, UTF-16 code units. `None` means "this text is the whole
    /// document" (the full-sync form).
    pub range: Option<(u32, u32, u32, u32)>,
    pub text: String,
}

impl ContentChange {
    /// The JSON shape LSP expects inside `contentChanges`.
    pub fn to_json(&self) -> Value {
        match self.range {
            Some((sl, sc, el, ec)) => json!({
                "range": {
                    "start": { "line": sl, "character": sc },
                    "end":   { "line": el, "character": ec },
                },
                "text": self.text,
            }),
            None => json!({ "text": self.text }),
        }
    }
}

/// The `contentChanges` for a document that went from `old` to `new`.
///
/// * [`TextDocumentSyncKind::NoSync`] — empty: the server asked for nothing.
/// * [`TextDocumentSyncKind::Full`] — one rangeless change carrying `new`.
/// * [`TextDocumentSyncKind::Incremental`] — one ranged change covering exactly
///   the span between the common prefix and the common suffix.
///
/// An unchanged document yields an empty vec in every mode: sending a no-op
/// change would bump the document version for nothing and re-trigger the
/// server's whole analysis pass.
pub fn content_changes(old: &str, new: &str, kind: TextDocumentSyncKind) -> Vec<ContentChange> {
    if old == new {
        return Vec::new();
    }
    match kind {
        TextDocumentSyncKind::NoSync => Vec::new(),
        TextDocumentSyncKind::Full => vec![ContentChange {
            range: None,
            text: new.to_string(),
        }],
        TextDocumentSyncKind::Incremental => {
            let prefix = common_prefix_bytes(old, new);
            let suffix = common_suffix_bytes(old, new, prefix);
            let old_end = old.len() - suffix;
            let new_end = new.len() - suffix;
            vec![ContentChange {
                range: Some({
                    let (sl, sc) = utf16_position(old, prefix);
                    let (el, ec) = utf16_position(old, old_end);
                    (sl, sc, el, ec)
                }),
                text: new[prefix..new_end].to_string(),
            }]
        }
    }
}

/// Byte length of the longest common prefix, always on a char boundary.
fn common_prefix_bytes(a: &str, b: &str) -> usize {
    let mut n = 0;
    for ((ai, ac), (_, bc)) in a.char_indices().zip(b.char_indices()) {
        if ac != bc {
            break;
        }
        n = ai + ac.len_utf8();
    }
    n
}

/// Byte length of the longest common suffix that does not run back past
/// `prefix` in EITHER string (so the two spans can never overlap).
fn common_suffix_bytes(a: &str, b: &str, prefix: usize) -> usize {
    let mut n = 0;
    let mut ai = a.char_indices().rev();
    let mut bi = b.char_indices().rev();
    loop {
        match (ai.next(), bi.next()) {
            (Some((ao, ac)), Some((bo, bc))) if ac == bc && ao >= prefix && bo >= prefix => {
                n += ac.len_utf8();
            }
            _ => return n,
        }
    }
}

/// `(line, utf16 character)` for a byte offset into `text`.
///
/// A `\r\n` pair is ONE line terminator: the `\r` contributes no column, so a
/// position at the end of a CRLF line matches what the server computed from the
/// same bytes. A lone `\r` (classic-Mac EOL) is still a line break.
pub fn utf16_position(text: &str, byte: usize) -> (u32, u32) {
    let byte = byte.min(text.len());
    let mut line = 0u32;
    let mut col = 0u32;
    let mut chars = text.char_indices().peekable();
    while let Some((i, ch)) = chars.next() {
        if i >= byte {
            break;
        }
        match ch {
            '\n' => {
                line += 1;
                col = 0;
            }
            '\r' => {
                if matches!(chars.peek(), Some((_, '\n'))) {
                    // The `\n` on the next iteration owns the line break.
                } else {
                    line += 1;
                    col = 0;
                }
            }
            _ => col += ch.len_utf16() as u32,
        }
    }
    (line, col)
}

/// Byte offsets of every line start in a document.
///
/// Built in ONE pass so a batch of positions (a whole file's diagnostics, each
/// with a start and an end) costs `O(text)` once instead of `O(text)` per
/// lookup. [`byte_offset_of_position`] is the one-shot form built on top of it.
#[derive(Debug, Clone)]
pub struct LineIndex {
    starts: Vec<usize>,
}

impl LineIndex {
    /// Index `text`. `\r\n` is one break; a lone `\r` is also a break.
    pub fn new(text: &str) -> Self {
        let mut starts = vec![0usize];
        let mut chars = text.char_indices().peekable();
        while let Some((i, ch)) = chars.next() {
            let is_break = ch == '\n' || (ch == '\r' && !matches!(chars.peek(), Some((_, '\n'))));
            if is_break {
                starts.push(i + ch.len_utf8());
            }
        }
        Self { starts }
    }

    /// Number of lines (always at least 1).
    pub fn line_count(&self) -> usize {
        self.starts.len()
    }

    /// Byte offset of a `(line, utf16 character)` position.
    ///
    /// Out-of-range positions clamp instead of panicking: a server may publish
    /// a diagnostic against a document version we have already edited past, and
    /// a stale range must degrade to a harmless in-bounds offset rather than
    /// take the editor down. A character offset past the end of its line clamps
    /// to that line's end, NOT into the next line — a squiggle that ran on into
    /// the following line would underline unrelated code.
    pub fn offset_of(&self, text: &str, line: u32, character: u32) -> usize {
        let Some(&line_start) = self.starts.get(line as usize) else {
            return text.len();
        };
        if line_start >= text.len() {
            return text.len();
        }
        let mut col = 0u32;
        for (i, ch) in text[line_start..].char_indices() {
            if ch == '\n' || ch == '\r' {
                return line_start + i;
            }
            if col >= character {
                return line_start + i;
            }
            col += ch.len_utf16() as u32;
        }
        text.len()
    }
}

/// Byte offset of a `(line, utf16 character)` position in `text` — the inverse
/// of [`utf16_position`]. See [`LineIndex::offset_of`] for the clamping rules;
/// use [`LineIndex`] directly when converting more than one position.
pub fn byte_offset_of_position(text: &str, line: u32, character: u32) -> usize {
    LineIndex::new(text).offset_of(text, line, character)
}

/// How long the buffer must be quiet before a `didChange` is sent.
///
/// Long enough that a burst of typing coalesces into one message (a language
/// server re-analyses the whole file on each change), short enough that
/// diagnostics feel live after a pause. 250 ms is the interval VS Code's own
/// clients settle on for the same trade-off.
pub const DEBOUNCE: Duration = Duration::from_millis(250);

/// Coalesces a burst of edits into a single send.
///
/// The clock is passed in, never read internally: the window is asserted by
/// handing it two instants, not by sleeping. The debouncer holds the LATEST
/// text seen, so a burst always sends the final state — never an intermediate
/// keystroke.
#[derive(Debug, Default)]
pub struct ChangeDebouncer {
    pending: Option<String>,
    last_change: Option<Instant>,
    idle: Option<Duration>,
}

impl ChangeDebouncer {
    /// A debouncer using the default [`DEBOUNCE`] window.
    pub fn new() -> Self {
        Self::default()
    }

    /// A debouncer with an explicit idle window (used by the timing tests).
    pub fn with_idle(idle: Duration) -> Self {
        Self {
            idle: Some(idle),
            ..Self::default()
        }
    }

    fn idle(&self) -> Duration {
        self.idle.unwrap_or(DEBOUNCE)
    }

    /// Record the buffer's current text. A text DIFFERENT from the pending one
    /// replaces it and restarts the quiet window — that is what makes continuous
    /// typing send nothing until the user pauses.
    ///
    /// Re-noting the SAME text is a no-op, deliberately. The caller is an egui
    /// frame loop that hands over the buffer's current state ~60 times a second
    /// whether or not anything was typed; if each of those restarted the window,
    /// the window would never elapse and NOTHING WOULD EVER BE SENT — the
    /// feature would be dead in exactly the way it looks alive. "Note" means
    /// "note a change", and an identical text is not one.
    pub fn note(&mut self, text: &str, now: Instant) {
        if self.pending.as_deref() == Some(text) {
            return;
        }
        self.pending = Some(text.to_string());
        self.last_change = Some(now);
    }

    /// Take the pending text once the buffer has been quiet for the idle
    /// window. `None` while nothing is pending or the window has not elapsed.
    pub fn take_due(&mut self, now: Instant) -> Option<String> {
        let last = self.last_change?;
        if now.duration_since(last) < self.idle() {
            return None;
        }
        self.last_change = None;
        self.pending.take()
    }

    /// True while an edit is waiting for the quiet window to elapse.
    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- sync-kind decoding ----

    #[test]
    fn sync_kind_decodes_the_bare_number_form() {
        let msg = json!({ "id": 1, "result": { "capabilities": { "textDocumentSync": 2 } } });
        assert_eq!(
            TextDocumentSyncKind::from_initialize_result(&msg),
            Some(TextDocumentSyncKind::Incremental)
        );
    }

    #[test]
    fn sync_kind_decodes_the_options_object_form() {
        // rust-analyzer's actual shape.
        let msg = json!({ "id": 1, "result": { "capabilities": {
            "textDocumentSync": { "openClose": true, "change": 2, "save": {} } } } });
        assert_eq!(
            TextDocumentSyncKind::from_initialize_result(&msg),
            Some(TextDocumentSyncKind::Incremental)
        );
        let full = json!({ "id": 1, "result": { "capabilities": {
            "textDocumentSync": { "openClose": true, "change": 1 } } } });
        assert_eq!(
            TextDocumentSyncKind::from_initialize_result(&full),
            Some(TextDocumentSyncKind::Full)
        );
    }

    #[test]
    fn sync_kind_is_unknown_when_the_server_declared_nothing() {
        assert_eq!(
            TextDocumentSyncKind::from_initialize_result(&json!({"id": 1, "result": {}})),
            None
        );
        // A publishDiagnostics notification is not an initialize result.
        assert_eq!(
            TextDocumentSyncKind::from_initialize_result(
                &json!({"method": "textDocument/publishDiagnostics"})
            ),
            None
        );
        // An out-of-range number is not silently mapped onto a real kind.
        assert_eq!(TextDocumentSyncKind::from_wire(7), None);
        assert_eq!(TextDocumentSyncKind::from_wire(-1), None);
    }

    #[test]
    fn sync_kind_wire_numbers_are_the_spec_values() {
        // Pinned against the LSP spec: 0 None, 1 Full, 2 Incremental. A swapped
        // pair here would send a full document to a server expecting a range.
        assert_eq!(TextDocumentSyncKind::NoSync.to_wire(), 0);
        assert_eq!(TextDocumentSyncKind::Full.to_wire(), 1);
        assert_eq!(TextDocumentSyncKind::Incremental.to_wire(), 2);
        for k in [
            TextDocumentSyncKind::NoSync,
            TextDocumentSyncKind::Full,
            TextDocumentSyncKind::Incremental,
        ] {
            assert_eq!(
                TextDocumentSyncKind::from_wire(i64::from(k.to_wire())),
                Some(k)
            );
        }
    }

    // ---- content changes ----

    #[test]
    fn full_sync_sends_the_whole_new_document_with_no_range() {
        let c = content_changes("a\n", "ab\n", TextDocumentSyncKind::Full);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].range, None);
        assert_eq!(c[0].text, "ab\n");
        assert_eq!(c[0].to_json(), json!({ "text": "ab\n" }));
    }

    #[test]
    fn no_sync_sends_nothing_even_for_a_real_edit() {
        assert!(content_changes("a", "b", TextDocumentSyncKind::NoSync).is_empty());
    }

    #[test]
    fn an_unchanged_document_sends_nothing_in_any_mode() {
        for k in [
            TextDocumentSyncKind::Full,
            TextDocumentSyncKind::Incremental,
            TextDocumentSyncKind::NoSync,
        ] {
            assert!(
                content_changes("same\n", "same\n", k).is_empty(),
                "a no-op edit must not bump the document version ({k:?})"
            );
        }
    }

    #[test]
    fn incremental_sends_only_the_typed_character() {
        // The whole point: one keystroke in the middle of a big file must not
        // ship the file. `fn mai|n` -> `fn main`, an insert of "n" at 0:6.
        let old = "fn mai() {}\nlet x = 1;\n";
        let new = "fn main() {}\nlet x = 1;\n";
        let c = content_changes(old, new, TextDocumentSyncKind::Incremental);
        assert_eq!(c.len(), 1);
        assert_eq!(
            c[0].text, "n",
            "only the inserted character travels, not the document"
        );
        assert_eq!(
            c[0].range,
            Some((0, 6, 0, 6)),
            "an insertion is an empty range at the insertion point"
        );
    }

    #[test]
    fn incremental_deletion_is_a_range_replaced_by_empty_text() {
        let old = "hello world\n";
        let new = "hello\n";
        let c = content_changes(old, new, TextDocumentSyncKind::Incremental);
        assert_eq!(c[0].text, "");
        assert_eq!(
            c[0].range,
            Some((0, 5, 0, 11)),
            "the deleted span is named in PRE-change coordinates"
        );
    }

    #[test]
    fn incremental_range_spans_lines_when_the_edit_does() {
        let old = "one\ntwo\nthree\n";
        let new = "one\nXXX\n";
        let c = content_changes(old, new, TextDocumentSyncKind::Incremental);
        assert_eq!(
            c[0].range,
            Some((1, 0, 2, 5)),
            "replacing two lines with one names a multi-line range"
        );
        assert_eq!(c[0].text, "XXX");
    }

    #[test]
    fn incremental_positions_are_utf16_units_not_bytes_or_chars() {
        // `é` is 2 bytes / 1 char / 1 UTF-16 unit; `😀` is 4 bytes / 1 char /
        // 2 UTF-16 units. A byte-counting or char-counting implementation puts
        // the range in the wrong place — the server then edits the wrong text.
        let old = "é😀x\n";
        let new = "é😀xy\n";
        let c = content_changes(old, new, TextDocumentSyncKind::Incremental);
        assert_eq!(c[0].text, "y");
        assert_eq!(
            c[0].range,
            Some((0, 4, 0, 4)),
            "é(1) + 😀(2) + x(1) = 4 UTF-16 units, not 7 bytes and not 3 chars"
        );
    }

    #[test]
    fn incremental_json_carries_the_range_and_the_text() {
        let c = content_changes("ab\n", "aXb\n", TextDocumentSyncKind::Incremental);
        assert_eq!(
            c[0].to_json(),
            json!({
                "range": { "start": {"line": 0, "character": 1},
                           "end":   {"line": 0, "character": 1} },
                "text": "X"
            })
        );
    }

    #[test]
    fn incremental_handles_a_document_emptied_and_a_document_created() {
        let cleared = content_changes("abc\n", "", TextDocumentSyncKind::Incremental);
        assert_eq!(cleared[0].text, "");
        assert_eq!(cleared[0].range, Some((0, 0, 1, 0)));
        let created = content_changes("", "abc\n", TextDocumentSyncKind::Incremental);
        assert_eq!(created[0].text, "abc\n");
        assert_eq!(created[0].range, Some((0, 0, 0, 0)));
    }

    #[test]
    fn incremental_prefix_and_suffix_never_overlap() {
        // "aaa" -> "aa": prefix and suffix both want the same `a`s. An
        // unclamped suffix walk would produce a negative-width range (and, in
        // the byte arithmetic, an underflow panic).
        let c = content_changes("aaa", "aa", TextDocumentSyncKind::Incremental);
        assert_eq!(c[0].text, "");
        let (sl, sc, el, ec) = c[0].range.unwrap();
        assert!(
            (sl, sc) <= (el, ec),
            "the range must be well-ordered, got {:?}",
            c[0].range
        );
        assert_eq!(c[0].range, Some((0, 2, 0, 3)));
    }

    // ---- position conversion ----

    #[test]
    fn position_conversion_round_trips_across_lines_and_wide_chars() {
        let text = "fn main() {\n    let s = \"é😀\";\n}\n";
        for (byte, _) in text.char_indices() {
            let (l, c) = utf16_position(text, byte);
            assert_eq!(
                byte_offset_of_position(text, l, c),
                byte,
                "byte {byte} -> ({l},{c}) -> back must be the same byte"
            );
        }
    }

    #[test]
    fn a_crlf_pair_is_one_line_break_and_the_cr_has_no_column() {
        let text = "ab\r\ncd";
        // End of line 0 is byte 2 (before the \r).
        assert_eq!(utf16_position(text, 2), (0, 2));
        // The \n at byte 3 still reports as line 0 col 2 — the \r took no column.
        assert_eq!(utf16_position(text, 3), (0, 2));
        // First byte of line 1.
        assert_eq!(utf16_position(text, 4), (1, 0));
        assert_eq!(byte_offset_of_position(text, 1, 0), 4);
    }

    #[test]
    fn a_lone_cr_is_still_a_line_break() {
        let text = "ab\rcd";
        assert_eq!(utf16_position(text, 3), (1, 0));
        assert_eq!(byte_offset_of_position(text, 1, 1), 4);
    }

    #[test]
    fn a_stale_out_of_range_position_clamps_instead_of_panicking() {
        // A server can publish a diagnostic against a version we edited past.
        let text = "short\n";
        assert_eq!(
            byte_offset_of_position(text, 99, 0),
            text.len(),
            "a line past the end clamps to the end of the document"
        );
        assert_eq!(
            byte_offset_of_position(text, 0, 999),
            5,
            "a character past the end of its line clamps to that line's end, \
             NOT into the next line"
        );
        assert_eq!(utf16_position(text, 10_000), (1, 0));
    }

    #[test]
    fn the_line_index_agrees_with_the_one_shot_lookup_everywhere() {
        // The batch path (a whole file's diagnostics) and the one-shot path must
        // never disagree — a divergence would paint squiggles at offsets the
        // rest of the editor does not believe in.
        let text = "one\r\ntwo\rthree\n\nfive é😀\n";
        let idx = LineIndex::new(text);
        assert_eq!(
            idx.line_count(),
            6,
            "\\r\\n, lone \\r, blank line, trailing \\n"
        );
        for line in 0..8u32 {
            for ch in [0u32, 1, 3, 99] {
                assert_eq!(
                    idx.offset_of(text, line, ch),
                    byte_offset_of_position(text, line, ch),
                    "batch and one-shot disagree at ({line},{ch})"
                );
            }
        }
    }

    #[test]
    fn a_character_offset_never_leaks_into_the_following_line() {
        // The clamp that matters for painting: a squiggle whose end ran into the
        // next line would underline unrelated code.
        let text = "ab\ncdef\n";
        assert_eq!(byte_offset_of_position(text, 0, 50), 2, "end of line 0");
        assert_eq!(byte_offset_of_position(text, 1, 50), 7, "end of line 1");
    }

    // ---- debounce ----

    #[test]
    fn nothing_is_due_before_the_window_elapses() {
        let t0 = Instant::now();
        let mut d = ChangeDebouncer::with_idle(Duration::from_millis(100));
        d.note("a", t0);
        assert!(
            d.take_due(t0).is_none(),
            "due immediately would be no debounce"
        );
        assert!(d.take_due(t0 + Duration::from_millis(99)).is_none());
        assert!(d.is_pending(), "the edit is still waiting, not dropped");
    }

    #[test]
    fn the_text_is_due_at_exactly_the_window() {
        let t0 = Instant::now();
        let mut d = ChangeDebouncer::with_idle(Duration::from_millis(100));
        d.note("a", t0);
        assert_eq!(
            d.take_due(t0 + Duration::from_millis(100)).as_deref(),
            Some("a"),
            "the boundary is inclusive (`< idle` holds it, `>= idle` releases)"
        );
    }

    #[test]
    fn a_burst_of_keystrokes_sends_once_and_sends_the_last_state() {
        // The reason this type exists: typing "hello" is five edits and ONE
        // message, carrying "hello" — never "h", and never five messages.
        let t0 = Instant::now();
        let mut d = ChangeDebouncer::with_idle(Duration::from_millis(100));
        let mut sent = Vec::new();
        for (i, s) in ["h", "he", "hel", "hell", "hello"].iter().enumerate() {
            let now = t0 + Duration::from_millis(20 * i as u64);
            d.note(s, now);
            if let Some(text) = d.take_due(now) {
                sent.push(text);
            }
        }
        assert!(
            sent.is_empty(),
            "continuous typing inside the window sends nothing, got {sent:?}"
        );
        let after_pause = t0 + Duration::from_millis(20 * 4 + 100);
        assert_eq!(
            d.take_due(after_pause).as_deref(),
            Some("hello"),
            "the pause releases exactly one send carrying the FINAL text"
        );
        assert!(
            d.take_due(after_pause + Duration::from_secs(10)).is_none(),
            "and nothing more is due afterwards"
        );
    }

    #[test]
    fn re_noting_the_same_text_does_not_restart_the_window() {
        // The caller is a 60fps frame loop that hands over the buffer every
        // frame. If an unchanged text restarted the window, the window would
        // never elapse and nothing would EVER be sent — the whole feature would
        // be dead while every other test stayed green. (This is not
        // hypothetical: it is the defect the frame-loop wiring test caught.)
        let t0 = Instant::now();
        let mut d = ChangeDebouncer::with_idle(Duration::from_millis(100));
        d.note("edited", t0);
        // 60 idle frames re-noting the same text, spread over well past the
        // window.
        for i in 0..60u64 {
            d.note("edited", t0 + Duration::from_millis(i * 5));
        }
        assert_eq!(
            d.take_due(t0 + Duration::from_millis(100)).as_deref(),
            Some("edited"),
            "the window is measured from the last REAL change, not from the \
             last time the frame loop mentioned the text"
        );
    }

    #[test]
    fn a_fresh_edit_restarts_the_quiet_window() {
        let t0 = Instant::now();
        let mut d = ChangeDebouncer::with_idle(Duration::from_millis(100));
        d.note("a", t0);
        d.note("ab", t0 + Duration::from_millis(90));
        assert!(
            d.take_due(t0 + Duration::from_millis(150)).is_none(),
            "150ms after the FIRST edit is only 60ms after the second — not due"
        );
        assert_eq!(
            d.take_due(t0 + Duration::from_millis(190)).as_deref(),
            Some("ab")
        );
    }

    #[test]
    fn an_untouched_debouncer_is_never_due() {
        let mut d = ChangeDebouncer::new();
        assert!(!d.is_pending());
        assert!(d
            .take_due(Instant::now() + Duration::from_secs(3600))
            .is_none());
    }

    #[test]
    fn the_default_window_coalesces_typing_without_feeling_dead() {
        // Guard the cadence constant itself: below ~100ms a fast typist still
        // spams the server; above ~1s diagnostics stop feeling live.
        const {
            assert!(DEBOUNCE.as_millis() >= 100, "too short to coalesce a burst");
            assert!(DEBOUNCE.as_millis() <= 1000, "too long to feel live");
        }
        let t0 = Instant::now();
        let mut d = ChangeDebouncer::new();
        d.note("x", t0);
        assert!(d
            .take_due(t0 + DEBOUNCE - Duration::from_millis(1))
            .is_none());
        assert!(d.take_due(t0 + DEBOUNCE).is_some());
    }
}
