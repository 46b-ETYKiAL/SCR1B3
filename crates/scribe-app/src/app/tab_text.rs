//! The tab's editable text, sealed behind a private `String`.
//!
//! # Why this module exists
//!
//! `EditorTab::text` used to be an unqualified-private `String` field. In Rust
//! that means "visible to the defining module AND every descendant of it" —
//! and every editor surface (`frame_tick`, `grid_methods`, `text_ops_methods`,
//! `editor_overlays`, `multi_cursor_glue`, …) is a descendant of `app`. So the
//! field was, in practice, writable from anywhere in the editor.
//!
//! That mattered because writing `text` is never the whole job. Several caches
//! are DERIVED from it, and a writer that skips their invalidation causes
//! silent data loss:
//!
//! * `rope_buf` — the persistent rope. Left stale, the next content edit on the
//!   rope path writes it back over `text` and destroys the write outright.
//! * `rope_state` — the rope editor's carets + undo history. Left stale, one
//!   Ctrl+Z restores a buffer the user never had, and a caret can index past
//!   the end.
//! * `edit_gen` — keys the minimap / spellcheck / symbol-scope / change-bar
//!   caches. Left unbumped, every one of them serves pre-edit content.
//! * `textedit_undo_stale` — the proxy for the egui `TextEdit` undoer, which
//!   lives in egui's own memory rather than on this struct.
//!
//! The invalidation was documented ("EVERY external mutation of `text` MUST go
//! through here") and the only thing enforcing it was a source-text scan whose
//! own docstring called itself "the ONLY police on that arm". A grep is not an
//! invariant: it cannot see a writer added in a file it does not scan, and it
//! cannot see one added tomorrow.
//!
//! # What this module changes
//!
//! [`TabText`] owns the `String` **and every cache derived from it**, and the
//! `String` is private to this module. Nothing outside `tab_text` can name it,
//! so nothing outside can write it. The only ways in are the seams below, and
//! every one of them performs the invalidation itself — there is no ordering
//! for a caller to get wrong and no call for a caller to forget.
//!
//! The enforcement is therefore the type system, not a scan: a new write path
//! that skips the funnel does not lint, it does not warn, it does not fail a
//! guard test — it fails to compile, because the field it would have to assign
//! is not nameable from where it is written.
//!
//! # The two-seam polarity is preserved
//!
//! [`TabText::set_text`] and [`TabText::set_text_keep_undo`] keep the exact
//! polarity the undo-invalidation fix established, and this module deliberately
//! does NOT collapse them into one entry point:
//!
//! * `set_text` is the SAFE DEFAULT. It additionally flags the egui `TextEdit`
//!   undo history stale, so a Ctrl+Z after the replacement cannot restore the
//!   PREVIOUS document over content that came from outside the buffer.
//! * `set_text_keep_undo` is the explicit opt-out, for a user-issued in-buffer
//!   command whose result the undoer SHOULD be able to revert.
//!
//! Misclassifying a call site toward `set_text` costs one undo step.
//! Misclassifying it toward `set_text_keep_undo` costs the user their document.
//! Every seam here that leaves the undo history intact says so in its name, so
//! the cheap mistake is the one you make by not thinking about it.
//!
//! # What is NOT sealed
//!
//! `EditorTab` lives in the parent module, so `tab.text = TabText::opened(s)` —
//! replacing the whole field — is still expressible inside `app`. That is not
//! the data-loss shape this module exists to prevent: it replaces the text and
//! every derived cache together, so no cache can survive stale across it. It is
//! also not silent — it has to name this type. Sealing it as well would mean
//! moving `EditorTab` and its other fifteen fields in here too, which buys
//! nothing against the defect family and costs an accessor for every one of
//! them.

use std::fmt;
use std::ops::Deref;

use scribe_core::buffer::Buffer;
use scribe_render::RopeEditorState;

/// The editable text mirror of one open document, plus every cache derived
/// from it. See the module docs for why they live together.
pub(super) struct TabText {
    /// PRIVATE, and the reason this type exists. Never widen this; the seal is
    /// the invariant.
    text: String,
    /// KEYSTONE perf — the persistent rope buffer for the experimental owned
    /// editor. Built once from `text` (O(n)) on first use, then mutated in
    /// place each frame; `text` is re-synced from it ONLY when an edit actually
    /// changes content. Dropped by every replacement, so the next frame
    /// rebuilds it rather than writing pre-replacement content back.
    rope_buf: Option<Buffer>,
    /// KEYSTONE — per-tab editing state (caret/selection + undo history) for
    /// the experimental owned rope editor. Lazily created on first use;
    /// `None` while the egui `TextEdit` path owns this tab.
    rope_state: Option<RopeEditorState>,
    /// Wave-3 perf: monotonic per-tab edit generation, bumped on EVERY
    /// mutation. The minimap + spellcheck + change-bar caches key off this
    /// `u64` instead of re-hashing the whole buffer every frame.
    edit_gen: u64,
    /// The ONE text-derived cache that is not a field here: the egui `TextEdit`
    /// path's undo history, an `Undoer<(CCursorRange, String)>` inside egui's
    /// own memory. No seam here has a `&egui::Context`, so none can clear it
    /// directly; this flag is its proxy, raised by [`TabText::set_text`] and
    /// consumed by each render path immediately before its `TextEdit` renders.
    textedit_undo_stale: bool,
}

/// A widget's report of whether it wrote to the buffer it was handed.
///
/// [`TabText::edit_with_widget`] is the only way to obtain the `&mut String` an
/// egui text widget needs, and it decides the invalidation from this trait
/// rather than from the caller — so a widget writer cannot forget to invalidate
/// and cannot lie about it by omission.
pub(super) trait WroteText {
    /// True exactly on the frame the widget mutated the buffer.
    fn wrote_text(&self) -> bool;
}

impl WroteText for egui::text_edit::TextEditOutput {
    fn wrote_text(&self) -> bool {
        self.response.changed()
    }
}

impl TabText {
    // ---- construction -----------------------------------------------------

    /// A freshly-opened (or freshly-restored) tab's buffer: no rope, no editing
    /// state, generation zero.
    ///
    /// This is a CONSTRUCTOR, not a setter. To change the text of a tab that
    /// already exists, use [`TabText::set_text`] or one of the other seams —
    /// they carry the generation forward, which is what keeps every gen-keyed
    /// cache honest across the write.
    pub(super) fn opened(text: String) -> Self {
        Self {
            text,
            rope_buf: None,
            rope_state: None,
            edit_gen: 0,
            textedit_undo_stale: false,
        }
    }

    // ---- reads ------------------------------------------------------------

    /// The current edit generation — the key every text-derived cache stores.
    pub(super) fn edit_gen(&self) -> u64 {
        self.edit_gen
    }

    /// The persistent rope, if one has been built.
    pub(super) fn rope_buf(&self) -> Option<&Buffer> {
        self.rope_buf.as_ref()
    }

    /// The rope editor's per-tab editing state, if it has claimed this tab.
    pub(super) fn rope_state(&self) -> Option<&RopeEditorState> {
        self.rope_state.as_ref()
    }

    /// The rope editor's editing state, created empty if the rope path has not
    /// claimed this tab yet — for the command dispatcher, which injects a
    /// synthesised event into the editor's queue before the first rope frame
    /// has necessarily run.
    pub(super) fn rope_state_or_new(&mut self) -> &mut RopeEditorState {
        self.rope_state.get_or_insert_with(RopeEditorState::new)
    }

    /// Install (or clear) the rope editor's editing state — the session-restore
    /// path, which reinstates a saved caret. Carets and history are not derived
    /// from `text` in the direction that loses data, so this is not a write.
    pub(super) fn set_rope_state(&mut self, state: Option<RopeEditorState>) {
        self.rope_state = state;
    }

    /// Read AND clear the stale-undo flag — what a render path calls just
    /// before its `TextEdit` renders, so the undoer is cleared exactly once per
    /// replacement.
    pub(super) fn take_textedit_undo_stale(&mut self) -> bool {
        std::mem::take(&mut self.textedit_undo_stale)
    }

    /// Bump the edit generation WITHOUT touching the text.
    ///
    /// For the callers that must invalidate the gen-keyed caches because
    /// something else about the tab changed (a reload that produced identical
    /// bytes, a test forcing a recompute). This is not a text write and
    /// deliberately does not drop the rope: dropping it here would throw away a
    /// live buffer that still matches `text`.
    pub(super) fn bump_edit_gen(&mut self) {
        self.edit_gen = self.edit_gen.wrapping_add(1);
    }

    // ---- writes -----------------------------------------------------------
    //
    // Every one of these performs the invalidation itself. That is the whole
    // design: there is no way to reach the `String` without going through one,
    // so there is nothing for a caller to forget.

    /// Replace the buffer from an EXTERNAL source — a disk reload, a session
    /// restore, a plugin transform, a find-replace, a sort — and invalidate
    /// every cache derived from the old content.
    ///
    /// This is the SAFE DEFAULT of the two replacement seams. On top of the
    /// shared invalidation it flags the egui `TextEdit` undo history stale, so
    /// a Ctrl+Z after the replacement cannot `replace_with` the PREVIOUS
    /// document over the new content.
    ///
    /// Use it for every replacement whose content did not come from the current
    /// buffer, and for anything you have not classified. A user-issued in-buffer
    /// editing command that SHOULD stay revertible calls
    /// [`TabText::set_text_keep_undo`] instead.
    ///
    /// The polarity is deliberate: forgetting to classify a new call site here
    /// costs the user one undo step; forgetting it on the other seam costs the
    /// user their document. The default must be the one that cannot lose data.
    pub(super) fn set_text(&mut self, new: String) {
        self.set_text_keep_undo(new);
        self.textedit_undo_stale = true;
    }

    /// [`TabText::set_text`] MINUS the egui-undo invalidation: the buffer is
    /// replaced and every on-struct derived cache is invalidated exactly as
    /// `set_text` does, but the `TextEdit` undo history is left intact so
    /// Ctrl+Z still reverts the replacement.
    ///
    /// PRECONDITION: the new text was produced by a USER-ISSUED in-buffer
    /// editing command from the CURRENT buffer contents (sort lines, toggle
    /// comment, replace-all, case transform, table format, auto-pair, …). Under
    /// that precondition the snapshot the undoer holds is a state the user
    /// genuinely had a moment ago, so restoring it destroys nothing they have
    /// not seen — it is what undo is FOR.
    ///
    /// It is NOT for content arriving from outside the buffer. A disk reload, a
    /// session restore, or a plugin that can read anywhere must use
    /// [`TabText::set_text`], or Ctrl+Z resurrects a document the user no
    /// longer has over content they never saw.
    pub(super) fn set_text_keep_undo(&mut self, new: String) {
        self.text = new;
        self.note_text_mutated();
    }

    /// Splice the buffer IN PLACE, for the callers that cannot hand a seam an
    /// owned `String` without cloning the whole buffer — `accept_completion`'s
    /// `replace_range`, the multi-cursor replay's `apply_edit` loop.
    ///
    /// The raw `&mut String` exists only for the duration of `f`, and the
    /// invalidation runs here afterwards, so an in-place splicer owes the
    /// buffer nothing it can forget.
    ///
    /// `_keep_undo` for the same reason `set_text_keep_undo` carries it: both
    /// splicers are user-issued in-buffer edits, and the egui undoer is
    /// supposed to be able to revert them. Content from OUTSIDE the buffer must
    /// not be introduced through here — use [`TabText::set_text`].
    pub(super) fn splice_in_place_keep_undo<R>(&mut self, f: impl FnOnce(&mut String) -> R) -> R {
        let out = f(&mut self.text);
        self.note_text_mutated();
        out
    }

    /// Render an egui text widget over the buffer.
    ///
    /// This is the ONLY way to obtain the `&mut String` an egui text widget
    /// needs, and the borrow does not outlive `f`. Whether the widget wrote is
    /// read back off its own output via [`WroteText`], and the invalidation
    /// happens here — so the "the `.changed()` arm must remember to invalidate"
    /// duty, which used to be policed by a source-text scan, is now discharged
    /// by construction.
    ///
    /// Keeps the undo history: an edit the user typed into the widget is
    /// exactly what the widget's undoer is for.
    pub(super) fn edit_with_widget<R: WroteText>(&mut self, f: impl FnOnce(&mut String) -> R) -> R {
        let out = f(&mut self.text);
        if out.wrote_text() {
            self.note_text_mutated();
        }
        out
    }

    /// Publish the rope back into `text` after the rope editor reported a real
    /// content edit, and bump the generation.
    ///
    /// The rope editor OWNS the buffer on its path, so this deliberately does
    /// NOT drop `rope_buf` or `rope_state` the way a replacement does — doing
    /// so would discard the live buffer the edit just landed in. Returns
    /// whether the text was actually refreshed (false when no rope is built,
    /// where the generation bump alone is the historical behaviour).
    pub(super) fn sync_from_rope(&mut self) -> bool {
        let refreshed = match self.rope_buf.as_ref().and_then(Buffer::as_rope) {
            Some(rope) => {
                self.text = rope.to_string();
                true
            }
            None => false,
        };
        self.bump_edit_gen();
        refreshed
    }

    /// The rope and the editing state the rope editor needs, building both if
    /// this is the first frame on the rope path.
    ///
    /// Returned together from one call because they are two disjoint fields of
    /// one private struct: handing them out separately would need two `&mut`
    /// borrows the caller cannot take at once.
    pub(super) fn ensure_rope_parts_mut(&mut self) -> (&mut Buffer, &mut RopeEditorState) {
        if self.rope_buf.is_none() {
            self.rope_buf = Some(Buffer::from_text(&self.text));
        }
        let buf = self.rope_buf.as_mut().expect("rope_buf set above");
        let state = self.rope_state.get_or_insert_with(RopeEditorState::new);
        (buf, state)
    }

    // ---- shared invalidation ----------------------------------------------

    /// The invalidation every write owes the buffer: drop the stale rope,
    /// invalidate the rope editor's state, bump the generation.
    ///
    /// Private on purpose. It used to be reachable from the whole `app` subtree,
    /// which is what let a writer perform a SUBSET of the invalidation by hand —
    /// and two hand-maintained invalidation lists drift, silently. Now the only
    /// callers are the seams above, so there is one implementation and no way to
    /// invoke a fraction of it.
    fn note_text_mutated(&mut self) {
        self.rope_buf = None;
        self.invalidate_rope_state();
        self.bump_edit_gen();
    }

    /// Invalidate the rope editor's per-tab editing state after `text` was
    /// replaced.
    ///
    /// `rope_state` is derived from the buffer: its `History` holds snapshots of
    /// the PREVIOUS content and its caret/selection are offsets into it.
    /// Clearing `rope_buf` alone left that state behind, so the first Undo after
    /// a command-palette or find-replace edit restored a buffer the user never
    /// had — silent data loss — and a caret past the new end pointed out of
    /// range.
    ///
    /// The history is dropped (those snapshots describe content that no longer
    /// exists, so a no-op Undo is the only honest outcome) along with any
    /// secondary carets, whose offsets a wholesale replacement invalidates. The
    /// primary caret is kept, clamped into the new text, so an in-place command
    /// (comment-toggle, move/duplicate/join line) does not throw the user back
    /// to the top of the file.
    ///
    /// A tab whose `rope_state` is still `None` is left alone — the rope editor
    /// has not claimed it yet, and the next frame creates the state fresh from
    /// the new content.
    fn invalidate_rope_state(&mut self) {
        let Some(prev) = self.rope_state.as_ref() else {
            return;
        };
        let cursor = prev.edit.cursor;
        // `chars().count()` is O(n); skip it for the common caret-at-origin case
        // so a large-buffer replacement pays nothing extra.
        let clamped = if cursor == 0 {
            0
        } else {
            cursor.min(self.text.chars().count())
        };
        let mut fresh = RopeEditorState::new();
        fresh.edit = scribe_core::editing::EditState::at(clamped);
        self.rope_state = Some(fresh);
    }
}

/// Observers the invariant tests need and production does not.
///
/// They are `cfg(test)` rather than plain `pub(super)` so the shipping surface
/// stays exactly the seams the editor actually uses — a read accessor nobody
/// calls is dead weight that reads like a supported way in.
#[cfg(test)]
impl TabText {
    /// The buffer as a string slice. `TabText` also derefs to `str`, so most
    /// read sites need no accessor at all.
    pub(super) fn as_str(&self) -> &str {
        &self.text
    }

    /// Whether a persistent rope has been built for this buffer.
    pub(super) fn has_rope_buf(&self) -> bool {
        self.rope_buf.is_some()
    }

    /// The rope editor's per-tab editing state, mutably — for the test harness
    /// that parks a caret before driving a real rope event.
    pub(super) fn rope_state_mut(&mut self) -> Option<&mut RopeEditorState> {
        self.rope_state.as_mut()
    }

    /// Whether the egui `TextEdit` undo history is currently flagged stale.
    /// Reading it WITHOUT clearing it is a test-only need; the render paths use
    /// [`TabText::take_textedit_undo_stale`], which is a one-shot.
    pub(super) fn textedit_undo_stale(&self) -> bool {
        self.textedit_undo_stale
    }
}

impl Deref for TabText {
    type Target = str;

    fn deref(&self) -> &str {
        &self.text
    }
}

impl fmt::Display for TabText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

impl fmt::Debug for TabText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.text, f)
    }
}

// Comparisons against the plain string types, so the many read sites that
// assert on buffer content keep reading as they did. Deliberately NOT
// `PartialEq<TabText>` — comparing two buffers is not a thing the editor does,
// and leaving it out keeps `==` from quietly meaning "same caches too".
impl PartialEq<str> for TabText {
    fn eq(&self, other: &str) -> bool {
        self.text == other
    }
}

impl PartialEq<&str> for TabText {
    fn eq(&self, other: &&str) -> bool {
        self.text == *other
    }
}

impl PartialEq<String> for TabText {
    fn eq(&self, other: &String) -> bool {
        self.text == *other
    }
}

impl PartialEq<TabText> for str {
    fn eq(&self, other: &TabText) -> bool {
        *self == *other.text
    }
}

impl PartialEq<TabText> for &str {
    fn eq(&self, other: &TabText) -> bool {
        **self == *other.text
    }
}

impl PartialEq<TabText> for String {
    fn eq(&self, other: &TabText) -> bool {
        *self == other.text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A buffer with every derived cache warm, so a replacement has something
    /// real to invalidate. The rope is built from the text; the editing state
    /// is created alongside it.
    fn warmed(text: &str) -> TabText {
        let mut t = TabText::opened(text.to_string());
        let _ = t.ensure_rope_parts_mut();
        t.bump_edit_gen();
        t
    }

    /// EVERY field of `TabText` must be accounted for by a replacement.
    ///
    /// The exhaustive destructuring below is the load-bearing part: adding a
    /// field to `TabText` makes this test fail TO COMPILE, forcing whoever adds
    /// it to decide whether a replacement must invalidate it. Never replace it
    /// with `..` — the missing-field compile error IS the assertion.
    ///
    /// It is the companion to `set_text_invalidates_every_text_derived_cache`
    /// in `text_ops_methods`, which does the same for `EditorTab`'s remaining
    /// fields. Neither struct can name the other's fields, so both are needed.
    #[test]
    fn a_replacement_invalidates_every_field() {
        let mut t = warmed("before\n");
        let gen_before = t.edit_gen;
        assert!(t.rope_buf.is_some(), "precondition — the rope is warm");
        assert!(
            t.rope_state.is_some(),
            "precondition — editing state is warm"
        );

        t.set_text("after\n".to_string());

        let TabText {
            text,
            rope_buf,
            rope_state,
            edit_gen,
            textedit_undo_stale,
        } = &t;

        assert_eq!(text, "after\n", "the new content is in place");
        assert!(
            rope_buf.is_none(),
            "the rope caches the OLD content — left alive, the next content \
             edit writes it back over `text` and destroys the replacement"
        );
        assert!(
            rope_state
                .as_ref()
                .is_some_and(|s| s.history.retained_bytes() == 0),
            "the editing history describes content that no longer exists — a \
             no-op undo is the only honest outcome"
        );
        assert_ne!(
            *edit_gen, gen_before,
            "`edit_gen` keys the minimap / spellcheck / change-bar caches — it \
             must move or every one of them serves stale derived data"
        );
        assert!(
            *textedit_undo_stale,
            "`set_text` is the SAFE DEFAULT: it must flag the egui undoer stale \
             so Ctrl+Z cannot restore the previous document over content that \
             came from outside the buffer"
        );
    }

    /// The polarity, stated as a test rather than as prose: the two seams must
    /// differ in EXACTLY one observable, and it must be the undo flag.
    ///
    /// Collapsing them into one entry point is the change this asserts against.
    /// If `set_text_keep_undo` ever starts flagging the undoer, every
    /// user-issued in-buffer command silently loses its undo step; if
    /// `set_text` ever stops, an external replacement can be undone back over
    /// content the user never saw.
    #[test]
    fn the_two_seams_differ_only_in_the_undo_flag() {
        let mut safe = warmed("before\n");
        let mut opt_out = warmed("before\n");
        safe.set_text("after\n".to_string());
        opt_out.set_text_keep_undo("after\n".to_string());

        assert_eq!(safe.text, opt_out.text, "both replace the buffer");
        assert_eq!(safe.edit_gen, opt_out.edit_gen, "both bump the generation");
        assert!(
            safe.rope_buf.is_none() && opt_out.rope_buf.is_none(),
            "both drop the stale rope"
        );

        assert!(
            safe.textedit_undo_stale,
            "the SAFE DEFAULT flags the egui undo history stale"
        );
        assert!(
            !opt_out.textedit_undo_stale,
            "the explicit opt-out leaves it intact — that is the whole reason \
             it exists, and the reason it is the one you have to ask for"
        );
    }

    /// The in-place splicer owes the buffer the same invalidation as a
    /// wholesale replacement, and keeps the undo history for the same reason
    /// `set_text_keep_undo` does.
    #[test]
    fn an_in_place_splice_invalidates_the_derived_caches() {
        let mut t = warmed("hello\n");
        let gen_before = t.edit_gen;

        t.splice_in_place_keep_undo(|s| s.replace_range(0..5, "goodbye"));

        assert_eq!(t.text, "goodbye\n");
        assert!(t.rope_buf.is_none(), "the pre-splice rope must be dropped");
        assert_ne!(t.edit_gen, gen_before, "the generation must move");
        assert!(
            !t.textedit_undo_stale,
            "an in-place splice is user-issued in-buffer work the undoer is \
             supposed to be able to revert"
        );
    }

    /// A widget that did NOT write must not cost an invalidation — otherwise
    /// every idle frame drops the rope and the editor is O(n) per frame again.
    #[test]
    fn a_widget_that_did_not_write_invalidates_nothing() {
        struct Quiet(bool);
        impl WroteText for Quiet {
            fn wrote_text(&self) -> bool {
                self.0
            }
        }

        let mut t = warmed("hello\n");
        let gen_before = t.edit_gen;

        t.edit_with_widget(|_buf| Quiet(false));
        assert!(t.rope_buf.is_some(), "an idle frame must keep the rope");
        assert_eq!(
            t.edit_gen, gen_before,
            "an idle frame must not bump the gen"
        );

        t.edit_with_widget(|buf| {
            buf.push_str("typed");
            Quiet(true)
        });
        assert!(t.rope_buf.is_none(), "a writing frame drops the stale rope");
        assert_ne!(t.edit_gen, gen_before, "a writing frame bumps the gen");
        assert!(
            !t.textedit_undo_stale,
            "the widget's own edit is exactly what its undoer is for"
        );
    }

    /// The rope path OWNS the buffer, so publishing from it must NOT drop the
    /// live rope the edit just landed in — the one write that deliberately
    /// invalidates less than a replacement does.
    #[test]
    fn publishing_from_the_rope_keeps_the_live_rope() {
        let mut t = warmed("hello\n");
        let gen_before = t.edit_gen;

        assert!(t.sync_from_rope(), "a built rope republishes its content");

        assert!(
            t.rope_buf.is_some(),
            "dropping the rope here would discard the buffer the edit landed in"
        );
        assert_ne!(t.edit_gen, gen_before, "the generation still moves");
    }

    /// With no rope built there is nothing to publish, but the generation still
    /// moves — the historical behaviour of the `content_changed` arm.
    #[test]
    fn publishing_without_a_rope_still_bumps_the_generation() {
        let mut t = TabText::opened("hello\n".to_string());
        let gen_before = t.edit_gen;

        assert!(!t.sync_from_rope(), "there is no rope to publish from");
        assert_ne!(t.edit_gen, gen_before, "the generation moves regardless");
        assert_eq!(t.text, "hello\n", "and the buffer is untouched");
    }

    /// The caret survives a replacement, clamped — an in-place command must not
    /// throw the user back to the top of a large file, nor leave a caret
    /// indexing past the end.
    #[test]
    fn a_replacement_clamps_rather_than_discards_the_caret() {
        let mut t = TabText::opened("0123456789".to_string());
        let mut st = RopeEditorState::new();
        st.edit = scribe_core::editing::EditState::at(9);
        t.set_rope_state(Some(st));

        t.set_text("abc".to_string());
        assert_eq!(
            t.rope_state().map(|s| s.edit.cursor),
            Some(3),
            "an out-of-range caret clamps to the new end, never points past it"
        );

        let mut st = RopeEditorState::new();
        st.edit = scribe_core::editing::EditState::at(2);
        t.set_rope_state(Some(st));
        t.set_text("abcdefghij".to_string());
        assert_eq!(
            t.rope_state().map(|s| s.edit.cursor),
            Some(2),
            "an in-range caret is preserved so the view does not jump"
        );
    }

    /// A tab the rope editor has not claimed keeps no editing state — creating
    /// one would be pointless work on every write for every TextEdit-path tab.
    #[test]
    fn a_replacement_leaves_an_unclaimed_tab_without_editing_state() {
        let mut t = TabText::opened("before".to_string());
        assert!(t.rope_state().is_none());
        t.set_text("after".to_string());
        assert!(
            t.rope_state().is_none(),
            "a replacement must not fabricate editing state for a tab the rope \
             editor never claimed"
        );
    }

    /// The stale-undo flag is a one-shot: the render path takes it, and a
    /// second take must not re-clear an undoer that is already fresh.
    #[test]
    fn the_stale_undo_flag_is_taken_once() {
        let mut t = TabText::opened("a".to_string());
        t.set_text("b".to_string());
        assert!(t.take_textedit_undo_stale(), "the render path consumes it");
        assert!(
            !t.take_textedit_undo_stale(),
            "a second take must report nothing to do, or every later frame \
             would clear an undo history the user is still building"
        );
    }
}
