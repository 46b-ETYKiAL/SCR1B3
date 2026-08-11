//! Note-capture commands: paste an image from the clipboard into the vault's
//! attachments folder, rename a note and rewrite every inbound wiki-link, and
//! create a real dated daily note.
//!
//! # The one security invariant
//!
//! Every filesystem target produced here — the attachment file, the daily note,
//! the rename destination — goes through
//! [`scribe_core::notes::vault_path::resolve_in_vault`], the single
//! traversal-safety chokepoint. A link target, a note title, and the two folder
//! names in `[notes]` are all UNTRUSTED input; nothing this module writes may
//! land outside the configured vault.
//!
//! The OS clipboard is reached through [`read_clipboard_image`], which is
//! headless under `cfg(test)` for the same reason `dialogs.rs` is: a test must
//! not depend on (or clobber) the host clipboard. Only the OS read is stubbed —
//! the PNG encode, the vault resolution, the de-duplication, the file write and
//! the link text around it are the real code under test.

#![allow(clippy::wildcard_imports)]

use super::*;
use scribe_core::notes::{relink, vault_path};
use std::path::{Path, PathBuf};

/// Hard cap on de-duplication suffixes for one attachment name. Reaching it
/// means something is badly wrong (a thousand pastes in the same second), and
/// looping forever would hang the UI thread.
const MAX_NAME_ATTEMPTS: u32 = 1000;

/// A raw RGBA8 image lifted off the clipboard.
pub(super) struct ClipboardImage {
    pub width: usize,
    pub height: usize,
    /// `width * height * 4` bytes, RGBA8.
    pub rgba: Vec<u8>,
}

/// Read an image off the OS clipboard.
///
/// Headless under `cfg(test)`: returns whatever [`test_hooks::set_next_image`]
/// injected, or the same "no image" error the OS returns when the clipboard
/// holds text — which is the branch every call site must already handle.
pub(super) fn read_clipboard_image() -> Result<ClipboardImage, String> {
    #[cfg(test)]
    {
        test_hooks::take_next_image().ok_or_else(|| "no image on the clipboard".to_string())
    }
    #[cfg(not(test))]
    {
        let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
        let img = cb.get_image().map_err(|e| e.to_string())?;
        Ok(ClipboardImage {
            width: img.width,
            height: img.height,
            rgba: img.bytes.into_owned(),
        })
    }
}

/// Encode RGBA8 pixels as a PNG.
///
/// Validates the buffer length against the declared dimensions FIRST: a short
/// or over-long buffer is a corrupt clipboard payload, and handing it to the
/// encoder is either a panic or a silently truncated image.
pub(super) fn encode_png(img: &ClipboardImage) -> Result<Vec<u8>, String> {
    if img.width == 0 || img.height == 0 {
        return Err("the clipboard image has no pixels".to_string());
    }
    let expected = img
        .width
        .checked_mul(img.height)
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| "the clipboard image is implausibly large".to_string())?;
    if img.rgba.len() != expected {
        return Err(format!(
            "clipboard image is {} bytes but {}x{} RGBA needs {expected}",
            img.rgba.len(),
            img.width,
            img.height
        ));
    }
    let w = u32::try_from(img.width).map_err(|_| "image width out of range".to_string())?;
    let h = u32::try_from(img.height).map_err(|_| "image height out of range".to_string())?;
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().map_err(|e| e.to_string())?;
        writer
            .write_image_data(&img.rgba)
            .map_err(|e| e.to_string())?;
        writer.finish().map_err(|e| e.to_string())?;
    }
    Ok(out)
}

/// The base file stem for an attachment pasted at `stamp` (an ISO-8601 UTC
/// timestamp): `pasted-YYYYMMDD-HHMMSS`. Falls back to `pasted` when the stamp
/// is not the expected shape, so the name is always usable.
#[must_use]
pub(super) fn attachment_stem(stamp: &str) -> String {
    let b = stamp.as_bytes();
    let shaped = b.len() == 20 && b[10] == b'T' && b[19] == b'Z';
    if !shaped {
        return "pasted".to_string();
    }
    let date: String = stamp[0..10].chars().filter(char::is_ascii_digit).collect();
    let time: String = stamp[11..19].chars().filter(char::is_ascii_digit).collect();
    if date.len() != 8 || time.len() != 6 {
        return "pasted".to_string();
    }
    format!("pasted-{date}-{time}")
}

/// Pick a file name of the form `<stem>.<ext>`, appending `-2`, `-3`, … until
/// `exists` says the name is free. Returns `None` if every candidate up to
/// [`MAX_NAME_ATTEMPTS`] is taken (rather than looping forever).
#[must_use]
pub(super) fn unique_file_name(
    stem: &str,
    ext: &str,
    exists: impl Fn(&str) -> bool,
) -> Option<String> {
    let first = format!("{stem}.{ext}");
    if !exists(&first) {
        return Some(first);
    }
    (2..=MAX_NAME_ATTEMPTS)
        .map(|n| format!("{stem}-{n}.{ext}"))
        .find(|c| !exists(c))
}

/// The `../` prefix that walks from the folder holding the vault-relative note
/// `note_rel` back up to the vault root. `"a.md"` → `""`, `"p/a.md"` → `"../"`,
/// `"p/q/a.md"` → `"../../"`.
///
/// Without this a note in a subfolder gets an attachment link that resolves
/// relative to ITS folder and silently renders as a broken image.
#[must_use]
pub(super) fn ascent_prefix(note_rel: &str) -> String {
    let depth = note_rel
        .replace('\\', "/")
        .split('/')
        .filter(|s| !s.is_empty())
        .count()
        .saturating_sub(1);
    "../".repeat(depth)
}

/// The markdown an image paste inserts. Alt text is a real description rather
/// than `![]` so the note is not born with an inaccessible image.
#[must_use]
pub(super) fn attachment_markdown(link: &str) -> String {
    format!("![pasted image]({link})")
}

/// Canonicalise `path` as far as the filesystem allows: the path itself when it
/// exists, otherwise its (existing) parent rejoined with the file name,
/// otherwise the path unchanged.
///
/// A rename destination does not exist yet, so plain `canonicalize` fails on it
/// and leaves a `C:\…` path being compared against a `\\?\C:\…` vault — which
/// reads as "outside the vault" and would refuse every legitimate rename.
#[must_use]
fn canonical_ish(path: &Path) -> PathBuf {
    if let Ok(c) = std::fs::canonicalize(path) {
        return c;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => match std::fs::canonicalize(parent) {
            Ok(c) => c.join(name),
            Err(_) => path.to_path_buf(),
        },
        _ => path.to_path_buf(),
    }
}

/// `path` expressed relative to `vault` with `/` separators, or `None` when it
/// is not inside the vault at all.
#[must_use]
pub(super) fn vault_relative(vault: &Path, path: &Path) -> Option<String> {
    let v = canonical_ish(vault);
    let p = canonical_ish(path);
    let rel = p.strip_prefix(&v).ok()?;
    let s = rel.to_string_lossy().replace('\\', "/");
    if s.is_empty() {
        return None;
    }
    Some(s)
}

/// The wiki-link target that names `path` inside `vault` — the vault-relative
/// path with a `.md` extension dropped (`projects/Roadmap.md` → `projects/Roadmap`).
///
/// The stripped form is only used when it ROUND-TRIPS: `resolve_in_vault` must
/// map it back to exactly `path`. A stem containing a dot (`v1.2 notes.md`)
/// does not round-trip — `Path::extension` sees `2 notes`, so no `.md` is
/// re-appended — and for those the full file name is used instead. Emitting a
/// target that does not resolve back would replace working links with broken
/// ones, which is worse than the rename we are fixing.
#[must_use]
pub(super) fn link_target_for(vault: &Path, path: &Path) -> Option<String> {
    let rel = vault_relative(vault, path)?;
    let is_md = Path::new(&rel)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("md"));
    if is_md {
        let stripped = &rel[..rel.len() - 3];
        if !stripped.is_empty() && resolves_to(vault, stripped, path) {
            return Some(stripped.to_string());
        }
    }
    Some(rel)
}

/// Whether the untrusted wiki-link `target` resolves — through the vault-safety
/// chokepoint — to exactly `path`.
#[must_use]
pub(super) fn resolves_to(vault: &Path, target: &str, path: &Path) -> bool {
    if target.trim().is_empty() {
        return false;
    }
    let Ok(resolved) = vault_path::resolve_in_vault(vault, target, Some("md")) else {
        return false;
    };
    scribe_core::path_norm::paths_equal_for_compare(&canonical_ish(&resolved), &canonical_ish(path))
}

/// One file the rename must rewrite, with its full new content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RenameEdit {
    pub path: PathBuf,
    pub content: String,
    pub links: usize,
}

/// Everything a rename will do, computed BEFORE anything is touched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RenamePlan {
    pub old_path: PathBuf,
    pub new_path: PathBuf,
    /// The wiki-link target inbound links are retargeted to.
    pub new_target: String,
    pub edits: Vec<RenameEdit>,
}

impl RenamePlan {
    /// Total links retargeted across every edited note.
    #[must_use]
    pub fn links(&self) -> usize {
        self.edits.iter().map(|e| e.links).sum()
    }
}

/// True when a rename completed only PARTIALLY — some notes, or some links,
/// were not written.
///
/// Extracted from `rename_note_active` so the four-way comparison is assertable
/// without a live vault, a tab set, and a filesystem that can be made to fail
/// halfway. Writing MORE than planned is not a failure.
#[must_use]
pub(super) fn rename_incomplete(
    written: usize,
    planned_notes: usize,
    links: usize,
    planned_links: usize,
) -> bool {
    written < planned_notes || links < planned_links
}

/// Compute the rewrite plan for renaming `old_path` to `new_path` inside
/// `vault`.
///
/// Note contents are re-read from disk here rather than taken from the note
/// index: the index caps each body at 256 KiB for search, and rewriting from a
/// capped body would TRUNCATE the file on save.
#[must_use]
pub(super) fn plan_rename(vault: &Path, old_path: &Path, new_path: &Path) -> Option<RenamePlan> {
    let new_target = link_target_for(vault, new_path)?;
    let mut edits = Vec::new();
    for doc in super::notes_ui::scan_vault(vault) {
        let Ok(content) = std::fs::read_to_string(&doc.path) else {
            continue;
        };
        if let Some(r) =
            relink::retarget_wikilinks(&content, &new_target, |t| resolves_to(vault, t, old_path))
        {
            edits.push(RenameEdit {
                path: doc.path,
                content: r.text,
                links: r.count,
            });
        }
    }
    Some(RenamePlan {
        old_path: old_path.to_path_buf(),
        new_path: new_path.to_path_buf(),
        new_target,
        edits,
    })
}

/// Carry out a [`RenamePlan`]: move the file first, then write every rewritten
/// note.
///
/// Order matters. If the move fails, nothing has been rewritten and the vault
/// is untouched. Doing it the other way round would leave every note pointing
/// at a name that does not exist.
///
/// The renamed note may itself contain links to itself; its edit is keyed on the
/// OLD path, so it is redirected to the new one instead of resurrecting a file
/// at the old name.
///
/// Returns `(notes_written, links_rewritten)` or the move error.
pub(super) fn apply_rename(plan: &RenamePlan) -> Result<(usize, usize), String> {
    if let Some(parent) = plan.new_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::rename(&plan.old_path, &plan.new_path).map_err(|e| e.to_string())?;
    let mut written = 0usize;
    let mut links = 0usize;
    for edit in &plan.edits {
        let target = if scribe_core::path_norm::paths_equal_for_compare(&edit.path, &plan.old_path)
        {
            &plan.new_path
        } else {
            &edit.path
        };
        match std::fs::write(target, &edit.content) {
            Ok(()) => {
                written += 1;
                links += edit.links;
            }
            Err(e) => {
                tracing::warn!("rename relink write failed for {}: {e}", target.display());
            }
        }
    }
    Ok((written, links))
}

impl ScribeApp {
    /// Paste an image from the clipboard into the vault's attachments folder and
    /// queue a markdown link to it for insertion at the caret.
    ///
    /// The file write is done here and now; only the caret insertion is deferred
    /// (the caret lives in egui state this call cannot reach), draining on the
    /// next frame through `drain_pending_editor_action`.
    pub(super) fn paste_image_attachment(&mut self) {
        let Some(vault) = self.config.notes.vault_dir.clone() else {
            self.toast = Some("Choose a notes folder first (the Notes pane).".into());
            return;
        };
        let img = match read_clipboard_image() {
            Ok(i) => i,
            Err(e) => {
                tracing::warn!("clipboard image read failed: {e}");
                self.toast = Some(
                    "There's no image on the clipboard. Copy an image, then try again.".into(),
                );
                return;
            }
        };
        let bytes = match encode_png(&img) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("png encode failed: {e}");
                self.toast = Some("Couldn't read that clipboard image.".into());
                return;
            }
        };

        let folder = self.config.notes.attachments_folder().to_string();
        let stem = attachment_stem(&crate::datetime::now_iso8601_utc());
        // Every candidate goes through the vault chokepoint, so a hostile or
        // mistyped `attachments_folder` cannot place the file outside the vault.
        let name = unique_file_name(&stem, "png", |candidate| {
            vault_path::resolve_in_vault(&vault, &format!("{folder}/{candidate}"), None)
                .map(|p| p.exists())
                // An unresolvable target is "taken" so the search does not
                // settle on a name we are about to refuse anyway.
                .unwrap_or(true)
        });
        let Some(name) = name else {
            self.toast = Some("Couldn't find a free name for the pasted image.".into());
            return;
        };
        let rel = format!("{folder}/{name}");
        let target = match vault_path::resolve_in_vault(&vault, &rel, None) {
            Ok(p) => p,
            Err(e) => {
                self.toast = Some(format!(
                    "Can't save the image there — {} Check `attachments_folder` in your settings.",
                    e.message()
                ));
                return;
            }
        };
        if let Some(parent) = target.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::warn!("create attachments dir failed: {e}");
                self.toast = Some("Couldn't create the attachments folder.".into());
                return;
            }
        }
        if let Err(e) = std::fs::write(&target, &bytes) {
            tracing::warn!("attachment write failed: {e}");
            self.toast = Some("Couldn't save the pasted image.".into());
            return;
        }

        // Link relative to the NOTE, not the vault, so a note in a subfolder
        // still renders the image.
        let note_rel = self
            .tabs
            .get(self.active)
            .and_then(|t| t.doc.path())
            .and_then(|p| vault_relative(&vault, p));
        let link = match note_rel {
            Some(nr) => format!("{}{rel}", ascent_prefix(&nr)),
            None => rel.clone(),
        };
        self.pending_insert_text = Some(attachment_markdown(&link));
        self.notes_refresh_index();
        self.status = format!("saved {rel} ({} bytes)", bytes.len());
    }

    /// Rename the active note and retarget every `[[wiki-link]]` in the vault
    /// that pointed at it.
    ///
    /// Refuses rather than half-doing the job: no vault, an unsaved buffer, a
    /// note outside the vault, a destination outside the vault, or a
    /// destination that already exists each stop the whole operation with a
    /// specific explanation. A rename that broke every backlink would be worse
    /// than no rename at all.
    pub(super) fn rename_note_active(&mut self) {
        let Some(vault) = self.config.notes.vault_dir.clone() else {
            self.toast = Some("Choose a notes folder first (the Notes pane).".into());
            return;
        };
        let active = self.active.min(self.tabs.len().saturating_sub(1));
        let Some(old_path) = self
            .tabs
            .get(active)
            .and_then(|t| t.doc.path())
            .map(Path::to_path_buf)
        else {
            self.toast = Some("Save this note first, then rename it.".into());
            return;
        };
        if self.tabs[active].doc.is_dirty() {
            self.toast = Some("Save this note before renaming it.".into());
            return;
        }
        if vault_relative(&vault, &old_path).is_none() {
            self.toast =
                Some("Only notes inside your vault can be renamed with link updates.".into());
            return;
        }
        let suggested = old_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("note.md")
            .to_string();
        let Some(picked) = super::dialogs::save_file(
            &suggested,
            &[("Markdown", "md"), ("Text", "txt"), ("All files", "*")],
        ) else {
            return; // cancelled
        };
        let Some(new_rel) = vault_relative(&vault, &picked) else {
            self.toast = Some("Pick a name inside your vault so links can be updated.".into());
            return;
        };
        // Defence in depth: the picked path already tested as vault-relative,
        // but the chokepoint is what the rest of the app trusts, so re-derive
        // the destination through it rather than using the dialog's path.
        let new_path = match vault_path::resolve_in_vault(&vault, &new_rel, Some("md")) {
            Ok(p) => p,
            Err(e) => {
                self.toast = Some(format!("Can't rename to that name — {}", e.message()));
                return;
            }
        };
        if scribe_core::path_norm::paths_equal_for_compare(&new_path, &old_path) {
            self.status = "name unchanged".to_string();
            return;
        }
        if new_path.exists() {
            self.toast = Some("A note with that name already exists.".into());
            return;
        }
        let Some(plan) = plan_rename(&vault, &old_path, &new_path) else {
            self.toast = Some("Couldn't work out the new note's link name.".into());
            return;
        };
        let planned_links = plan.links();
        let planned_notes = plan.edits.len();
        let (written, links) = match apply_rename(&plan) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("rename failed: {e}");
                self.toast = Some("Couldn't rename the note. Nothing was changed.".into());
                return;
            }
        };
        // Re-open the moved file in place: the tab's document still points at a
        // path that no longer exists, and the external-change poll cannot help
        // it (there is nothing to stat).
        match EditorTab::from_path(new_path.clone()) {
            Ok(mut t) => {
                t.doc_id = self.tabs[active].doc_id;
                t.pinned = self.tabs[active].pinned;
                self.tabs[active] = t;
            }
            Err(e) => {
                tracing::warn!("reopen after rename failed: {e}");
                self.toast = Some("Renamed, but couldn't reopen the note. Open it again.".into());
            }
        }
        // Other open tabs whose files were rewritten are picked up by the
        // existing external-change poll (silent reload when clean, banner when
        // dirty) — no second reload path to keep in sync.
        self.notes_refresh_index();
        let name = new_path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        self.status = format!(
            "renamed to {name}; updated {links} link{} in {written} note{}",
            if links == 1 { "" } else { "s" },
            if written == 1 { "" } else { "s" }
        );
        if rename_incomplete(written, planned_notes, links, planned_links) {
            self.toast = Some(
                "Renamed, but some notes couldn't be updated. Check that the vault is writable."
                    .into(),
            );
        }
    }

    /// Open today's daily note, creating it from the daily template the first
    /// time it is asked for.
    ///
    /// With a vault this is a real dated file (`<daily_folder>/YYYY-MM-DD.md`),
    /// so asking again later the same day reopens the SAME note instead of
    /// seeding a second unsaved buffer. Without a vault there is nowhere to put
    /// it, so it falls back to the in-memory seeded buffer — still with the
    /// date substituted.
    pub(super) fn new_daily_note(&mut self) {
        let Some(vault) = self.config.notes.vault_dir.clone() else {
            self.new_note_from_template(super::text_ops_methods::NoteTemplate::Daily);
            self.toast =
                Some("Choose a notes folder to keep dated daily notes on disk.".to_string());
            return;
        };
        let stamp = crate::datetime::now_iso8601_utc();
        let Some(ctx) = scribe_core::notes::template::TemplateContext::from_iso8601_utc(
            &stamp,
            // The daily note's title IS its date.
            stamp.get(0..10).unwrap_or("daily"),
        ) else {
            self.toast = Some("Couldn't read the system clock for today's date.".into());
            return;
        };
        let rel = format!("{}/{}", self.config.notes.daily_folder(), ctx.date);
        let path = match vault_path::resolve_in_vault(&vault, &rel, Some("md")) {
            Ok(p) => p,
            Err(e) => {
                self.toast = Some(format!(
                    "Can't create today's note there — {} Check `daily_folder` in your settings.",
                    e.message()
                ));
                return;
            }
        };
        if !path.exists() {
            if let Some(parent) = path.parent() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    tracing::warn!("create daily folder failed: {e}");
                    self.toast = Some("Couldn't create the daily-notes folder.".into());
                    return;
                }
            }
            let body = super::text_ops_methods::NoteTemplate::Daily.render_with(&ctx);
            if let Err(e) = std::fs::write(&path, body) {
                tracing::warn!("create daily note failed: {e}");
                self.toast = Some("Couldn't create today's note.".into());
                return;
            }
            self.notes_refresh_index();
        }
        self.open_path(path);
        self.status = format!("daily note: {}", ctx.date);
    }
}

/// Inject what the OS clipboard "returns", so the code AROUND it is testable —
/// the same discipline `dialogs::test_hooks` applies to the file pickers.
#[cfg(test)]
pub(crate) mod test_hooks {
    use super::ClipboardImage;
    use std::cell::RefCell;

    thread_local! {
        static NEXT_IMAGE: RefCell<Option<ClipboardImage>> = const { RefCell::new(None) };
    }

    /// The next [`super::read_clipboard_image`] returns this image. Consumed
    /// once; a second call reads as "no image on the clipboard".
    pub(crate) fn set_next_image(img: ClipboardImage) {
        NEXT_IMAGE.with(|c| *c.borrow_mut() = Some(img));
    }

    pub(super) fn take_next_image() -> Option<ClipboardImage> {
        NEXT_IMAGE.with(|c| c.borrow_mut().take())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scribe_core::config::Config;
    use std::path::PathBuf;

    // ---- pure helpers ----

    #[test]
    fn attachment_stem_strips_the_iso_punctuation() {
        assert_eq!(
            attachment_stem("2026-08-04T09:07:05Z"),
            "pasted-20260804-090705"
        );
    }

    #[test]
    fn attachment_stem_falls_back_on_a_malformed_stamp() {
        for bad in [
            "",
            "2026-08-04",
            "2026-08-04T09:07:05",
            "not a timestamp!!!!",
        ] {
            assert_eq!(attachment_stem(bad), "pasted", "{bad:?}");
        }
    }

    #[test]
    fn unique_file_name_uses_the_bare_name_when_free() {
        assert_eq!(
            unique_file_name("pasted-1", "png", |_| false).unwrap(),
            "pasted-1.png"
        );
    }

    #[test]
    fn unique_file_name_walks_past_taken_names() {
        let taken = ["pasted.png", "pasted-2.png", "pasted-3.png"];
        assert_eq!(
            unique_file_name("pasted", "png", |c| taken.contains(&c)).unwrap(),
            "pasted-4.png"
        );
    }

    #[test]
    fn unique_file_name_gives_up_instead_of_looping_forever() {
        assert!(unique_file_name("pasted", "png", |_| true).is_none());
    }

    #[test]
    fn ascent_prefix_matches_the_notes_folder_depth() {
        assert_eq!(ascent_prefix("a.md"), "");
        assert_eq!(ascent_prefix("p/a.md"), "../");
        assert_eq!(ascent_prefix("p/q/a.md"), "../../");
        assert_eq!(ascent_prefix("p\\q\\a.md"), "../../");
    }

    #[test]
    fn attachment_markdown_carries_real_alt_text() {
        let md = attachment_markdown("attachments/x.png");
        assert_eq!(md, "![pasted image](attachments/x.png)");
        assert!(
            !md.starts_with("![]"),
            "an image must not ship empty alt text"
        );
    }

    #[test]
    fn png_encode_rejects_a_mismatched_buffer_rather_than_truncating() {
        let bad = ClipboardImage {
            width: 2,
            height: 2,
            rgba: vec![0u8; 8], // needs 16
        };
        let err = encode_png(&bad).unwrap_err();
        assert!(err.contains("needs 16"), "{err}");
        assert!(encode_png(&ClipboardImage {
            width: 0,
            height: 4,
            rgba: Vec::new()
        })
        .is_err());
    }

    #[test]
    fn png_encode_emits_a_real_png_signature_and_dimensions() {
        let img = ClipboardImage {
            width: 3,
            height: 2,
            rgba: vec![0x7fu8; 3 * 2 * 4],
        };
        let bytes = encode_png(&img).unwrap();
        assert_eq!(
            &bytes[..8],
            &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a],
            "PNG magic"
        );
        // IHDR width/height are big-endian u32 at bytes 16..24.
        assert_eq!(&bytes[16..20], &3u32.to_be_bytes());
        assert_eq!(&bytes[20..24], &2u32.to_be_bytes());
        assert!(
            bytes.ends_with(&[b'I', b'E', b'N', b'D', 0xae, 0x42, 0x60, 0x82]),
            "the stream must be finished, not left half-written"
        );
    }

    // ---- vault-relative helpers (real filesystem) ----

    /// A disposable vault that is a SUBDIRECTORY of the temp dir.
    ///
    /// The nesting is load-bearing for the traversal tests: they assert that
    /// nothing appeared at `<vault>/../escaped`. If the vault were the tempdir
    /// itself, that parent would be the machine-wide temp folder — so one
    /// escape (during a mutation run, say) leaves a real `%TEMP%\escaped`
    /// behind and every later run of those tests fails for a reason that has
    /// nothing to do with the code under test. Here `..` lands inside the
    /// unique, auto-deleted tempdir instead.
    struct Vault {
        _root: tempfile::TempDir,
        path: PathBuf,
    }

    impl Vault {
        fn path(&self) -> &Path {
            &self.path
        }
        /// The directory the vault sits in — where an escape WOULD land.
        fn outside(&self) -> &Path {
            self._root.path()
        }
    }

    fn vault() -> Vault {
        let root = tempfile::tempdir().expect("temp root");
        let path = root.path().join("vault");
        std::fs::create_dir_all(&path).expect("vault dir");
        Vault { _root: root, path }
    }

    #[test]
    fn vault_relative_reports_none_for_a_path_outside_the_vault() {
        let v = vault();
        let outside = v.outside().join("elsewhere.md");
        assert!(vault_relative(v.path(), &outside).is_none());
    }

    #[test]
    fn vault_relative_works_for_a_file_that_does_not_exist_yet() {
        // A rename destination never exists at check time; a plain canonicalize
        // fails on it and would read as "outside the vault".
        let v = vault();
        assert_eq!(
            vault_relative(v.path(), &v.path().join("New.md")).as_deref(),
            Some("New.md")
        );
    }

    #[test]
    fn link_target_drops_a_md_extension_but_keeps_others() {
        let v = vault();
        std::fs::write(v.path().join("Home.md"), "x").unwrap();
        std::fs::write(v.path().join("Log.txt"), "x").unwrap();
        std::fs::create_dir_all(v.path().join("p")).unwrap();
        std::fs::write(v.path().join("p").join("Deep.md"), "x").unwrap();
        assert_eq!(
            link_target_for(v.path(), &v.path().join("Home.md")).as_deref(),
            Some("Home")
        );
        assert_eq!(
            link_target_for(v.path(), &v.path().join("Log.txt")).as_deref(),
            Some("Log.txt")
        );
        assert_eq!(
            link_target_for(v.path(), &v.path().join("p").join("Deep.md")).as_deref(),
            Some("p/Deep")
        );
    }

    #[test]
    fn link_target_keeps_the_full_name_when_stripping_would_not_round_trip() {
        // `v1.2 notes` has extension "2 notes", so resolve_in_vault appends no
        // `.md` and the stripped target resolves to a file that is not there.
        let v = vault();
        let p = v.path().join("v1.2 notes.md");
        std::fs::write(&p, "x").unwrap();
        assert_eq!(
            link_target_for(v.path(), &p).as_deref(),
            Some("v1.2 notes.md")
        );
        assert!(
            resolves_to(v.path(), "v1.2 notes.md", &p),
            "the chosen target must resolve back to the file"
        );
    }

    #[test]
    fn resolves_to_rejects_traversal_and_empty_targets() {
        let v = vault();
        let p = v.path().join("Home.md");
        std::fs::write(&p, "x").unwrap();
        assert!(resolves_to(v.path(), "Home", &p));
        assert!(!resolves_to(v.path(), "", &p));
        assert!(!resolves_to(v.path(), "   ", &p));
        assert!(!resolves_to(v.path(), "../Home", &p));
        assert!(!resolves_to(v.path(), "/etc/passwd", &p));
        assert!(!resolves_to(v.path(), "Other", &p));
    }

    // ---- rename + relink, end to end on disk ----

    fn app_with_vault(v: &Path) -> ScribeApp {
        let mut cfg = Config::default();
        cfg.editor.first_run_completed = true;
        cfg.notes.vault_dir = Some(v.to_path_buf());
        ScribeApp::new_test(cfg)
    }

    #[test]
    fn rename_moves_the_file_and_rewrites_every_inbound_link_shape() {
        let v = vault();
        std::fs::write(v.path().join("Old.md"), "# Old\n\nself [[Old]]\n").unwrap();
        std::fs::write(
            v.path().join("A.md"),
            "plain [[Old]]\nalias [[Old|The Old]]\nhead [[Old#Sec]]\nembed ![[Old]]\n",
        )
        .unwrap();
        std::fs::write(v.path().join("B.md"), "unrelated [[Other]]\n").unwrap();
        let b_before = std::fs::read_to_string(v.path().join("B.md")).unwrap();

        let mut app = app_with_vault(v.path());
        app.open_path(v.path().join("Old.md"));
        super::super::dialogs::test_hooks::set_next_save_path(v.path().join("New.md"));
        app.rename_note_active();

        // The move actually happened.
        assert!(!v.path().join("Old.md").exists(), "old file is gone");
        let moved = std::fs::read_to_string(v.path().join("New.md")).unwrap();
        // …and the renamed note's own self-link landed in the NEW file, not a
        // resurrected old one.
        assert!(moved.contains("self [[New]]"), "{moved:?}");

        let a = std::fs::read_to_string(v.path().join("A.md")).unwrap();
        assert_eq!(
            a,
            "plain [[New]]\nalias [[New|The Old]]\nhead [[New#Sec]]\nembed ![[New]]\n"
        );
        // A note with no matching link is byte-identical — never rewritten.
        assert_eq!(
            std::fs::read_to_string(v.path().join("B.md")).unwrap(),
            b_before
        );
        assert!(app.status.contains("renamed to New.md"), "{}", app.status);
        assert!(app.status.contains("5 links"), "{}", app.status);
        assert!(app.status.contains("2 notes"), "{}", app.status);
    }

    #[test]
    fn rename_retargets_a_link_written_as_a_subfolder_path() {
        let v = vault();
        std::fs::create_dir_all(v.path().join("p")).unwrap();
        std::fs::write(v.path().join("p").join("Old.md"), "body\n").unwrap();
        std::fs::write(v.path().join("A.md"), "see [[p/Old]]\n").unwrap();

        let mut app = app_with_vault(v.path());
        app.open_path(v.path().join("p").join("Old.md"));
        super::super::dialogs::test_hooks::set_next_save_path(v.path().join("p").join("New.md"));
        app.rename_note_active();

        assert_eq!(
            std::fs::read_to_string(v.path().join("A.md")).unwrap(),
            "see [[p/New]]\n"
        );
    }

    #[test]
    fn rename_refuses_a_destination_outside_the_vault_and_touches_nothing() {
        let v = vault();
        std::fs::write(v.path().join("Old.md"), "body\n").unwrap();
        std::fs::write(v.path().join("A.md"), "[[Old]]\n").unwrap();
        let outside = v.outside().join("escaped.md");

        let mut app = app_with_vault(v.path());
        app.open_path(v.path().join("Old.md"));
        super::super::dialogs::test_hooks::set_next_save_path(outside.clone());
        app.rename_note_active();

        assert!(
            !outside.exists(),
            "nothing may be written outside the vault"
        );
        assert!(v.path().join("Old.md").exists(), "the note stays put");
        assert_eq!(
            std::fs::read_to_string(v.path().join("A.md")).unwrap(),
            "[[Old]]\n",
            "no links are rewritten when the rename is refused"
        );
        assert!(
            app.toast
                .as_deref()
                .unwrap_or("")
                .contains("inside your vault"),
            "{:?}",
            app.toast
        );
    }

    #[test]
    fn rename_refuses_to_clobber_an_existing_note() {
        let v = vault();
        std::fs::write(v.path().join("Old.md"), "old body\n").unwrap();
        std::fs::write(v.path().join("Taken.md"), "keep me\n").unwrap();

        let mut app = app_with_vault(v.path());
        app.open_path(v.path().join("Old.md"));
        super::super::dialogs::test_hooks::set_next_save_path(v.path().join("Taken.md"));
        app.rename_note_active();

        assert_eq!(
            std::fs::read_to_string(v.path().join("Taken.md")).unwrap(),
            "keep me\n"
        );
        assert!(v.path().join("Old.md").exists());
        assert!(
            app.toast
                .as_deref()
                .unwrap_or("")
                .contains("already exists"),
            "{:?}",
            app.toast
        );
    }

    #[test]
    fn rename_refuses_while_the_buffer_has_unsaved_edits() {
        let v = vault();
        std::fs::write(v.path().join("Old.md"), "body\n").unwrap();
        let mut app = app_with_vault(v.path());
        app.open_path(v.path().join("Old.md"));
        let a = app.active;
        app.tabs[a].doc.mark_dirty();
        super::super::dialogs::test_hooks::set_next_save_path(v.path().join("New.md"));
        app.rename_note_active();

        assert!(v.path().join("Old.md").exists(), "nothing moved");
        assert!(!v.path().join("New.md").exists());
        assert!(
            app.toast
                .as_deref()
                .unwrap_or("")
                .contains("Save this note before"),
            "{:?}",
            app.toast
        );
    }

    #[test]
    fn rename_without_a_vault_explains_itself_and_does_nothing() {
        let v = vault();
        std::fs::write(v.path().join("Old.md"), "body\n").unwrap();
        let mut cfg = Config::default();
        cfg.editor.first_run_completed = true;
        let mut app = ScribeApp::new_test(cfg);
        app.open_path(v.path().join("Old.md"));
        super::super::dialogs::test_hooks::set_next_save_path(v.path().join("New.md"));
        app.rename_note_active();
        assert!(v.path().join("Old.md").exists());
        assert!(
            app.toast.as_deref().unwrap_or("").contains("notes folder"),
            "{:?}",
            app.toast
        );
    }

    #[test]
    fn rename_reopens_the_moved_file_in_the_same_tab() {
        let v = vault();
        std::fs::write(v.path().join("Old.md"), "unique-body-marker\n").unwrap();
        let mut app = app_with_vault(v.path());
        app.open_path(v.path().join("Old.md"));
        let tabs_before = app.tabs.len();
        let a = app.active;
        let doc_id_before = app.tabs[a].doc_id;
        super::super::dialogs::test_hooks::set_next_save_path(v.path().join("New.md"));
        app.rename_note_active();

        assert_eq!(app.tabs.len(), tabs_before, "no extra tab is opened");
        assert_eq!(
            app.tabs[a].doc.path().map(Path::to_path_buf),
            Some(v.path().join("New.md")),
            "the tab follows the file"
        );
        assert!(app.tabs[a].text.contains("unique-body-marker"));
        assert_eq!(
            app.tabs[a].doc_id, doc_id_before,
            "grid panes keep pointing at it"
        );
    }

    #[test]
    fn cancelling_the_rename_dialog_changes_nothing() {
        let v = vault();
        std::fs::write(v.path().join("Old.md"), "body\n").unwrap();
        let mut app = app_with_vault(v.path());
        app.open_path(v.path().join("Old.md"));
        // No injected path == the user pressed Cancel.
        app.rename_note_active();
        assert!(v.path().join("Old.md").exists());
        assert_eq!(
            app.tabs[app.active].doc.path(),
            Some(v.path().join("Old.md")).as_deref()
        );
    }

    // ---- image paste ----

    fn solid_image(w: usize, h: usize) -> ClipboardImage {
        ClipboardImage {
            width: w,
            height: h,
            rgba: vec![0x40u8; w * h * 4],
        }
    }

    #[test]
    fn pasting_an_image_writes_a_real_png_into_the_attachments_folder() {
        let v = vault();
        std::fs::write(v.path().join("Note.md"), "body\n").unwrap();
        let mut app = app_with_vault(v.path());
        app.open_path(v.path().join("Note.md"));
        test_hooks::set_next_image(solid_image(4, 3));
        app.paste_image_attachment();

        let dir = v.path().join("attachments");
        let files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .expect("attachments folder was created")
            .flatten()
            .map(|e| e.path())
            .collect();
        assert_eq!(files.len(), 1, "exactly one attachment: {files:?}");
        let bytes = std::fs::read(&files[0]).unwrap();
        assert_eq!(
            &bytes[..8],
            &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a],
            "the bytes on disk are a PNG"
        );
        assert_eq!(
            &bytes[16..20],
            &4u32.to_be_bytes(),
            "declared width survives"
        );

        let name = files[0].file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("pasted-"), "{name}");
        assert!(name.ends_with(".png"), "{name}");
        assert_eq!(
            app.pending_insert_text.as_deref(),
            Some(format!("![pasted image](attachments/{name})").as_str()),
            "the markdown queued for the caret points at the file that was written"
        );
    }

    #[test]
    fn a_second_paste_does_not_overwrite_the_first() {
        let v = vault();
        std::fs::write(v.path().join("Note.md"), "body\n").unwrap();
        let mut app = app_with_vault(v.path());
        app.open_path(v.path().join("Note.md"));
        test_hooks::set_next_image(solid_image(2, 2));
        app.paste_image_attachment();
        test_hooks::set_next_image(solid_image(5, 5));
        app.paste_image_attachment();

        let mut files: Vec<PathBuf> = std::fs::read_dir(v.path().join("attachments"))
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .collect();
        files.sort();
        assert_eq!(files.len(), 2, "both pastes survive: {files:?}");
        // The two images differ in size, so a clobber would show as equal bytes.
        let a = std::fs::read(&files[0]).unwrap();
        let b = std::fs::read(&files[1]).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn a_note_in_a_subfolder_gets_a_link_that_walks_back_up() {
        let v = vault();
        std::fs::create_dir_all(v.path().join("p").join("q")).unwrap();
        let note = v.path().join("p").join("q").join("Deep.md");
        std::fs::write(&note, "body\n").unwrap();
        let mut app = app_with_vault(v.path());
        app.open_path(note);
        test_hooks::set_next_image(solid_image(2, 2));
        app.paste_image_attachment();

        let link = app.pending_insert_text.clone().expect("markdown queued");
        assert!(
            link.contains("](../../attachments/pasted-"),
            "a two-deep note needs two ascents, got {link:?}"
        );
    }

    #[test]
    fn pasting_with_no_image_on_the_clipboard_writes_nothing() {
        let v = vault();
        std::fs::write(v.path().join("Note.md"), "body\n").unwrap();
        let mut app = app_with_vault(v.path());
        app.open_path(v.path().join("Note.md"));
        // No injected image == the clipboard holds text.
        app.paste_image_attachment();
        assert!(!v.path().join("attachments").exists());
        assert!(app.pending_insert_text.is_none());
        assert!(
            app.toast.as_deref().unwrap_or("").contains("no image"),
            "{:?}",
            app.toast
        );
    }

    #[test]
    fn pasting_without_a_vault_writes_nothing() {
        let mut cfg = Config::default();
        cfg.editor.first_run_completed = true;
        let mut app = ScribeApp::new_test(cfg);
        test_hooks::set_next_image(solid_image(2, 2));
        app.paste_image_attachment();
        assert!(app.pending_insert_text.is_none());
        assert!(
            app.toast.as_deref().unwrap_or("").contains("notes folder"),
            "{:?}",
            app.toast
        );
    }

    #[test]
    fn a_traversing_attachments_folder_is_refused_not_followed() {
        let v = vault();
        std::fs::write(v.path().join("Note.md"), "body\n").unwrap();
        let mut cfg = Config::default();
        cfg.editor.first_run_completed = true;
        cfg.notes.vault_dir = Some(v.path().to_path_buf());
        cfg.notes.attachments_folder = "../escaped".to_string();
        let mut app = ScribeApp::new_test(cfg);
        app.open_path(v.path().join("Note.md"));
        test_hooks::set_next_image(solid_image(2, 2));
        app.paste_image_attachment();

        assert!(
            !v.outside().join("escaped").exists(),
            "an attachments_folder that escapes the vault must never be created"
        );
        assert!(app.pending_insert_text.is_none());
    }

    // ---- daily notes ----

    #[test]
    fn the_daily_note_is_a_dated_file_in_the_configured_folder() {
        let v = vault();
        let mut app = app_with_vault(v.path());
        app.new_daily_note();

        let today = crate::datetime::now_iso8601_utc()[0..10].to_string();
        let path = v.path().join("daily").join(format!("{today}.md"));
        assert!(path.exists(), "expected {}", path.display());
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(
            body.contains(&format!("# {today}")),
            "the date must be substituted, not left as a placeholder: {body:?}"
        );
        assert!(!body.contains("{{"), "no placeholder may survive: {body:?}");
        assert_eq!(app.tabs[app.active].doc.path(), Some(path.as_path()));
    }

    #[test]
    fn asking_twice_in_a_day_reopens_the_same_note_instead_of_seeding_a_new_one() {
        let v = vault();
        let mut app = app_with_vault(v.path());
        app.new_daily_note();
        let today = crate::datetime::now_iso8601_utc()[0..10].to_string();
        let path = v.path().join("daily").join(format!("{today}.md"));
        std::fs::write(&path, "# edited by hand\n").unwrap();
        let tabs = app.tabs.len();

        app.new_daily_note();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "# edited by hand\n",
            "an existing daily note is never re-seeded"
        );
        assert_eq!(
            app.tabs.len(),
            tabs,
            "the same note is re-focused, not reopened"
        );
    }

    #[test]
    fn the_daily_folder_is_configurable() {
        let v = vault();
        let mut cfg = Config::default();
        cfg.editor.first_run_completed = true;
        cfg.notes.vault_dir = Some(v.path().to_path_buf());
        cfg.notes.daily_folder = "journal/2026".to_string();
        let mut app = ScribeApp::new_test(cfg);
        app.new_daily_note();

        let today = crate::datetime::now_iso8601_utc()[0..10].to_string();
        assert!(v
            .path()
            .join("journal")
            .join("2026")
            .join(format!("{today}.md"))
            .exists());
        assert!(
            !v.path().join("daily").exists(),
            "the default folder is not used"
        );
    }

    #[test]
    fn a_traversing_daily_folder_is_refused_not_followed() {
        let v = vault();
        let mut cfg = Config::default();
        cfg.editor.first_run_completed = true;
        cfg.notes.vault_dir = Some(v.path().to_path_buf());
        cfg.notes.daily_folder = "../escaped".to_string();
        let mut app = ScribeApp::new_test(cfg);
        app.new_daily_note();
        assert!(!v.outside().join("escaped").exists());
        assert!(
            app.toast.as_deref().unwrap_or("").contains("daily_folder"),
            "{:?}",
            app.toast
        );
    }

    // ---- palette wiring: the command reaches the effect, end to end ----

    #[test]
    fn the_palette_entry_for_each_new_command_reaches_its_effect() {
        // Routing asserted by OBSERVABLE OUTCOME (a file appears / moves), not
        // by "a pending flag is set" — a flag assertion would still pass with
        // the match arm pointing at the wrong method.
        let v = vault();
        std::fs::write(v.path().join("Note.md"), "body\n").unwrap();
        let mut app = app_with_vault(v.path());
        app.open_path(v.path().join("Note.md"));

        test_hooks::set_next_image(solid_image(2, 2));
        app.execute_builtin(BuiltinCommand::PasteImageAttachment);
        assert!(
            v.path().join("attachments").is_dir(),
            "PasteImageAttachment must reach paste_image_attachment"
        );

        app.execute_builtin(BuiltinCommand::NewDailyNote);
        let today = crate::datetime::now_iso8601_utc()[0..10].to_string();
        assert!(
            v.path().join("daily").join(format!("{today}.md")).exists(),
            "NewDailyNote must reach new_daily_note, not the scratch-buffer seeder"
        );

        // Rename acts on whatever tab is active; point it back at Note.md.
        let idx = app
            .tabs
            .iter()
            .position(|t| t.doc.path() == Some(v.path().join("Note.md").as_path()))
            .expect("Note.md is still open");
        app.active = idx;
        super::super::dialogs::test_hooks::set_next_save_path(v.path().join("Renamed.md"));
        app.execute_builtin(BuiltinCommand::RenameNote);
        assert!(
            v.path().join("Renamed.md").exists() && !v.path().join("Note.md").exists(),
            "RenameNote must reach rename_note_active"
        );
    }

    #[test]
    fn the_queued_attachment_link_actually_lands_in_the_buffer() {
        // The end of the wire: `paste_image_attachment` queues the markdown,
        // `drain_pending_editor_action` delivers it, and the REAL editor
        // inserts it. Asserting only `pending_insert_text.is_some()` would pass
        // with the drain deleted.
        let v = vault();
        std::fs::write(v.path().join("Note.md"), "before\n").unwrap();
        let mut cfg = Config::default();
        cfg.editor.first_run_completed = true;
        cfg.editor.experimental_rope_editor = true;
        cfg.notes.vault_dir = Some(v.path().to_path_buf());
        let mut app = ScribeApp::new_test(cfg);
        app.open_path(v.path().join("Note.md"));

        let d = super::super::e2e::Driver::new();
        d.idle(&mut app);
        d.idle(&mut app);
        let active = app.active;
        assert!(
            app.active_editor_is_rope(),
            "precondition: the rope surface is rendering this tab"
        );

        test_hooks::set_next_image(solid_image(2, 2));
        app.execute_builtin(BuiltinCommand::PasteImageAttachment);
        // Drain + apply: one frame delivers the event, the next settles it.
        d.idle(&mut app);
        d.idle(&mut app);

        let text = app.tabs[active].text.to_string();
        assert!(
            text.contains("![pasted image](attachments/pasted-"),
            "the markdown must be inserted into the buffer, got {text:?}"
        );
        assert!(
            text.contains("before"),
            "existing content survives: {text:?}"
        );
        assert!(
            app.pending_insert_text.is_none(),
            "the queue must be drained, not latched"
        );
        // …and the link that landed names a file that is really there.
        let start = text.find("](").unwrap() + 2;
        let end = text[start..].find(')').unwrap() + start;
        let rel = &text[start..end];
        assert!(
            v.path()
                .join(rel.replace('/', std::path::MAIN_SEPARATOR_STR))
                .exists(),
            "the inserted link must resolve to the written file: {rel}"
        );
    }

    #[test]
    fn without_a_vault_the_daily_note_still_substitutes_the_date_in_memory() {
        let mut cfg = Config::default();
        cfg.editor.first_run_completed = true;
        let mut app = ScribeApp::new_test(cfg);
        app.new_daily_note();
        let text = app.tabs[app.active].text.to_string();
        let today = crate::datetime::now_iso8601_utc()[0..10].to_string();
        assert!(text.contains(&today), "{text:?}");
        assert!(!text.contains("{{"), "{text:?}");
        assert!(
            app.tabs[app.active].doc.path().is_none(),
            "nothing is written"
        );
    }

    #[test]
    fn rename_incomplete_flags_exactly_the_partial_writes() {
        // Each row is chosen to break one mutation of the two comparisons and
        // the `||`: row 1 is the all-clear (so `==` / `<=` on either side turns
        // it into a false alarm), rows 2 and 3 fail on exactly ONE side (so `&&`
        // and a flipped `>` both stop reporting them).
        // (written, planned_notes, links, planned_links, want)
        for (written, planned_notes, links, planned_links, want) in [
            (3usize, 3usize, 7usize, 7usize, false), // everything landed
            (2, 3, 7, 7, true),                      // a note could not be written
            (3, 3, 6, 7, true),                      // a link could not be rewritten
            (2, 3, 6, 7, true),                      // both fell short
            (4, 3, 8, 7, false),                     // more than planned is not a failure
        ] {
            assert_eq!(
                rename_incomplete(written, planned_notes, links, planned_links),
                want,
                "written={written}/{planned_notes} links={links}/{planned_links}"
            );
        }
    }

    #[test]
    fn canonical_ish_resolves_a_not_yet_existing_destination_through_its_parent() {
        // `vault_relative_works_for_a_file_that_does_not_exist_yet` also covers
        // this arm, but it compares a tempdir path against its own canonical
        // form — which on LINUX (the mutation runner) are the same bytes, so the
        // arm can be deleted and that test still passes. Route through a `..`
        // component instead: canonicalisation normalises it away on every
        // platform, so the canonical and raw forms differ everywhere.
        let root = tempfile::tempdir().expect("temp root");
        std::fs::create_dir(root.path().join("sub")).expect("sub dir");
        let canonical_root = std::fs::canonicalize(root.path()).expect("canonicalize root");

        let dest = root.path().join("sub").join("..").join("New.md");
        assert!(
            !dest.exists(),
            "the fixture must be a destination that does NOT exist"
        );

        // A rename destination cannot be `canonicalize`d directly, so the parent
        // arm is the only thing that can produce a comparable path. Without it
        // the raw `…/sub/../New.md` is compared against a canonical vault and
        // reads as "outside the vault" — refusing every legitimate rename.
        assert_eq!(canonical_ish(&dest), canonical_root.join("New.md"));
    }

    #[test]
    fn rename_plan_links_sums_the_link_count_of_every_edit() {
        // The status line reports this number to the user ("updated N links in M
        // notes") and `rename_incomplete` compares against it, so a stubbed
        // constant is a silently wrong report AND a silently wrong toast.
        let plan = RenamePlan {
            old_path: PathBuf::from("/v/Old.md"),
            new_path: PathBuf::from("/v/New.md"),
            new_target: "New".into(),
            edits: vec![
                RenameEdit {
                    path: PathBuf::from("/v/a.md"),
                    content: String::new(),
                    links: 2,
                },
                RenameEdit {
                    path: PathBuf::from("/v/b.md"),
                    content: String::new(),
                    links: 1,
                },
            ],
        };
        // 3 = 2 + 1: a value neither a `-> 0` nor a `-> 1` stub can produce, and
        // one that a per-edit max (rather than a sum) would report as 2.
        assert_eq!(plan.links(), 3);
        assert_eq!(
            RenamePlan {
                edits: Vec::new(),
                ..plan
            }
            .links(),
            0,
            "no edits is genuinely zero links"
        );
    }

    /// A zero in EITHER dimension is a corrupt payload on its own. Asserted
    /// only with both dimensions zero, the `||` could become `&&` and a
    /// 0 x N (or N x 0) image would sail past the guard into the encoder.
    #[test]
    fn either_zero_dimension_alone_is_rejected() {
        for (w, h) in [(0usize, 4usize), (4, 0), (0, 0)] {
            let img = ClipboardImage {
                width: w,
                height: h,
                rgba: Vec::new(),
            };
            let err = encode_png(&img).expect_err("{w}x{h} must not encode");
            assert!(
                err.contains("no pixels"),
                "a zero dimension must be rejected as having no pixels, got: {err}"
            );
        }
    }

    /// The date and time digit-count checks are one `||` chain, so EITHER being
    /// wrong must fall back to the bare stem. Asserted only with both wrong,
    /// the chain could become `&&` and a half-malformed stamp would produce a
    /// truncated name like `pasted-20260810-` instead of `pasted`.
    #[test]
    fn a_stamp_wrong_in_only_one_half_still_falls_back() {
        assert_eq!(
            attachment_stem("2026-08-10T12:34:56Z"),
            "pasted-20260810-123456",
            "the control stamp must format, or this test proves nothing"
        );
        // Right shape (20 chars, `T` at 10, `Z` at 19) and a well-formed date,
        // but a time half carrying no digits at all.
        assert_eq!(
            attachment_stem("2026-08-10Tab:cd:efZ"),
            "pasted",
            "a bad time half alone falls back"
        );
        // ...and the mirror: a good time half with a date carrying no digits.
        assert_eq!(
            attachment_stem("abcd-ef-ghT12:34:56Z"),
            "pasted",
            "a bad date half alone falls back"
        );
    }
}
