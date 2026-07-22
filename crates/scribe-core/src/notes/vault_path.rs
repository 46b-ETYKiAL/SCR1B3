//! Vault-relative path safety.
//!
//! Note content is UNTRUSTED input: a `[[wiki-link]]` target, a `#tag`, or an
//! attachment name arrives from a file the user did not necessarily author. A
//! target such as `../../etc/passwd`, `/etc/shadow`, `C:\Windows\...`, or one
//! carrying a NUL byte must NEVER resolve to a path outside the configured
//! vault. Every wiki-link / attachment path that will touch the filesystem
//! MUST pass through [`resolve_in_vault`] (or at least [`sanitize_relative`]).
//!
//! The check is purely lexical + a defence-in-depth canonical containment
//! check, so it works even for a target whose file does not exist yet
//! (create-on-click): a lexical reject of `..` / absolute / drive-letter
//! components is the primary gate, and — when the parent exists — a
//! canonicalised-prefix check is the backstop against symlink escape.

use std::path::{Component, Path, PathBuf};

/// Reason a target was rejected (surfaced to the user / logs, never silently
/// swallowed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathReject {
    /// The target was empty after trimming.
    Empty,
    /// The target was an absolute path or carried a drive prefix (`C:`), a UNC
    /// root, or a leading `/` / `\`.
    Absolute,
    /// The target contained a `..` parent component (traversal).
    Traversal,
    /// The target contained a NUL or other control byte illegal in a path.
    IllegalChar,
    /// The resolved path escaped the vault root (symlink / canonical mismatch).
    Escapes,
}

impl PathReject {
    /// A short, user-facing explanation.
    #[must_use]
    pub fn message(&self) -> &'static str {
        match self {
            PathReject::Empty => "the link target is empty",
            PathReject::Absolute => {
                "the link target is an absolute path (must be inside the vault)"
            }
            PathReject::Traversal => "the link target uses `..` and would escape the vault",
            PathReject::IllegalChar => "the link target contains an illegal character",
            PathReject::Escapes => "the link target resolves outside the vault",
        }
    }
}

/// Validate a vault-relative target string lexically and return the cleaned
/// relative `PathBuf` (forward or back slashes normalised to native separators,
/// `.` components dropped). Rejects absolute paths, drive prefixes, `..`
/// traversal, and control characters. Does NOT touch the filesystem.
///
/// This is the primary gate: it is deliberately conservative so a link to a
/// not-yet-created note is still safely constrained.
pub fn sanitize_relative(target: &str) -> Result<PathBuf, PathReject> {
    let trimmed = target.trim();
    if trimmed.is_empty() {
        return Err(PathReject::Empty);
    }
    // Reject NUL and other C0 control bytes outright — no legitimate note path
    // contains them, and they are a classic path-truncation smuggling vector.
    if trimmed.chars().any(|c| c.is_control()) {
        return Err(PathReject::IllegalChar);
    }
    // Windows drive-absolute (`C:\`, `C:/`) or `C:relative` — reject anything
    // with a `:` in the first component (drive letter / ADS stream name).
    // A `:` never appears in a legitimate vault-relative note name.
    let first_seg = trimmed
        .split(['/', '\\'])
        .find(|s| !s.is_empty())
        .unwrap_or("");
    if first_seg.contains(':') {
        return Err(PathReject::Absolute);
    }
    // Leading slash / backslash → absolute-from-root — but ONLY when there is
    // real content after the separators. A string that is ENTIRELY separators
    // (`///`) has no target at all and is Empty, not Absolute: it normalises to
    // nothing in the component walk below. Without this guard `///` short-circuits
    // here as Absolute before ever reaching the empty check.
    let all_separators = trimmed.chars().all(|c| c == '/' || c == '\\');
    if !all_separators && (trimmed.starts_with('/') || trimmed.starts_with('\\')) {
        return Err(PathReject::Absolute);
    }

    // Normalise separators and walk components, rejecting traversal / absolute
    // components and dropping `.` and empty segments.
    let unified = trimmed.replace('\\', "/");
    let mut out = PathBuf::new();
    for seg in unified.split('/') {
        if seg.is_empty() || seg == "." {
            continue;
        }
        if seg == ".." {
            return Err(PathReject::Traversal);
        }
        // A component that std would treat as a root/prefix is absolute.
        let comp_path = Path::new(seg);
        for comp in comp_path.components() {
            match comp {
                Component::Normal(_) | Component::CurDir => {}
                Component::ParentDir => return Err(PathReject::Traversal),
                Component::RootDir | Component::Prefix(_) => return Err(PathReject::Absolute),
            }
        }
        out.push(seg);
    }
    if out.as_os_str().is_empty() {
        return Err(PathReject::Empty);
    }
    Ok(out)
}

/// Resolve a vault-relative target to an absolute path INSIDE `vault`, applying
/// [`sanitize_relative`] first and then a canonical-containment backstop.
///
/// `default_ext` (e.g. `Some("md")`) is appended when the sanitised target has
/// no extension — so `[[Home]]` resolves to `<vault>/Home.md`. Pass `None` to
/// leave the name untouched (attachments already carry an extension).
///
/// Returns the joined absolute path on success. The returned path may not yet
/// exist (create-on-click); its PARENT, if it exists, is canonicalised and
/// checked to still sit within the canonicalised vault, defeating a symlinked
/// subfolder that would otherwise escape.
pub fn resolve_in_vault(
    vault: &Path,
    target: &str,
    default_ext: Option<&str>,
) -> Result<PathBuf, PathReject> {
    let mut rel = sanitize_relative(target)?;
    if let Some(ext) = default_ext {
        if rel.extension().is_none() {
            rel.set_extension(ext);
        }
    }
    let joined = vault.join(&rel);

    // Defence-in-depth: if the parent directory exists, canonicalise it and the
    // vault and confirm containment. This catches a symlinked subdirectory that
    // lexical checks alone cannot see. When the parent does not exist yet (a new
    // note in a new subfolder), the lexical gate above is authoritative.
    if let Some(parent) = joined.parent() {
        if parent.exists() {
            let canon_parent = parent.canonicalize().map_err(|_| PathReject::Escapes)?;
            let canon_vault = vault.canonicalize().map_err(|_| PathReject::Escapes)?;
            if !canon_parent.starts_with(&canon_vault) {
                return Err(PathReject::Escapes);
            }
        }
    }
    Ok(joined)
}

/// Turn an arbitrary title / heading into a filesystem-safe note stem (for
/// create-on-click of `[[New Note]]` and for attachment naming). Illegal path
/// characters are replaced with `-`; the result is a single path component
/// (never contains a separator) and is non-empty.
#[must_use]
pub fn safe_stem(title: &str) -> String {
    // A title made ONLY of path separators, Windows-illegal chars, control chars,
    // dots and whitespace has no real name — it becomes "untitled" rather than a
    // run of dashes. A title with any meaningful character keeps it, with illegal
    // chars mapped to '-' (so "a/b:c*d?" stays "a-b-c-d-", trailing dash and all).
    // Without this pre-check, "///" mapped each '/' to '-' and returned "---".
    let has_meaningful = title.chars().any(|c| {
        !matches!(
            c,
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '.'
        ) && !c.is_control()
            && !c.is_whitespace()
    });
    if !has_meaningful {
        return "untitled".to_string();
    }
    let s: String = title
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            c if c.is_control() => '-',
            c => c,
        })
        .collect();
    let s = s.trim().trim_matches('.').trim().to_string();
    if s.is_empty() {
        "untitled".to_string()
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn plain_name_ok() {
        assert_eq!(sanitize_relative("Home").unwrap(), PathBuf::from("Home"));
    }

    #[test]
    fn subfolder_ok() {
        assert_eq!(
            sanitize_relative("projects/roadmap").unwrap(),
            PathBuf::from("projects").join("roadmap")
        );
    }

    #[test]
    fn backslash_normalised() {
        assert_eq!(
            sanitize_relative("a\\b\\c").unwrap(),
            PathBuf::from("a").join("b").join("c")
        );
    }

    #[test]
    fn dot_segments_dropped() {
        assert_eq!(
            sanitize_relative("./a/./b").unwrap(),
            PathBuf::from("a").join("b")
        );
    }

    #[test]
    fn parent_traversal_rejected() {
        assert_eq!(
            sanitize_relative("../secrets").unwrap_err(),
            PathReject::Traversal
        );
        assert_eq!(
            sanitize_relative("a/../../b").unwrap_err(),
            PathReject::Traversal
        );
        assert_eq!(
            sanitize_relative("notes/../../etc/passwd").unwrap_err(),
            PathReject::Traversal
        );
    }

    #[test]
    fn absolute_rejected() {
        assert_eq!(
            sanitize_relative("/etc/passwd").unwrap_err(),
            PathReject::Absolute
        );
        assert_eq!(
            sanitize_relative("\\\\server\\share").unwrap_err(),
            PathReject::Absolute
        );
    }

    #[test]
    fn windows_drive_rejected() {
        assert_eq!(
            sanitize_relative("C:\\Windows").unwrap_err(),
            PathReject::Absolute
        );
        assert_eq!(
            sanitize_relative("C:/Windows").unwrap_err(),
            PathReject::Absolute
        );
        // Drive-relative `C:foo` (no slash) is still absolute-ish and rejected.
        assert_eq!(
            sanitize_relative("C:foo").unwrap_err(),
            PathReject::Absolute
        );
    }

    #[test]
    fn nul_and_control_rejected() {
        assert_eq!(
            sanitize_relative("a\0b").unwrap_err(),
            PathReject::IllegalChar
        );
        assert_eq!(
            sanitize_relative("a\nb").unwrap_err(),
            PathReject::IllegalChar
        );
    }

    #[test]
    fn empty_rejected() {
        assert_eq!(sanitize_relative("   ").unwrap_err(), PathReject::Empty);
        assert_eq!(sanitize_relative("///").unwrap_err(), PathReject::Empty);
    }

    #[test]
    fn resolve_appends_default_ext() {
        let vault = std::env::temp_dir();
        let p = resolve_in_vault(&vault, "Home", Some("md")).unwrap();
        assert_eq!(p, vault.join("Home.md"));
    }

    #[test]
    fn resolve_keeps_existing_ext() {
        let vault = std::env::temp_dir();
        let p = resolve_in_vault(&vault, "photo.png", None).unwrap();
        assert_eq!(p, vault.join("photo.png"));
        // With a default ext but an existing ext, the existing one wins.
        let p2 = resolve_in_vault(&vault, "note.txt", Some("md")).unwrap();
        assert_eq!(p2, vault.join("note.txt"));
    }

    #[test]
    fn resolve_rejects_traversal_before_touching_fs() {
        let vault = PathBuf::from("/nonexistent/vault");
        assert_eq!(
            resolve_in_vault(&vault, "../../etc/passwd", Some("md")).unwrap_err(),
            PathReject::Traversal
        );
    }

    #[test]
    fn resolve_stays_within_real_vault() {
        // A real temp vault with a real subfolder — the resolved path is inside.
        let vault = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(vault.path().join("projects")).unwrap();
        let p = resolve_in_vault(vault.path(), "projects/roadmap", Some("md")).unwrap();
        assert!(p.starts_with(vault.path()));
        assert!(p.ends_with("roadmap.md"));
    }

    #[test]
    fn safe_stem_replaces_illegal_chars() {
        assert_eq!(safe_stem("a/b:c*d?"), "a-b-c-d-");
        assert_eq!(safe_stem("  trailing.  "), "trailing");
        assert_eq!(safe_stem("///"), "untitled");
        assert_eq!(safe_stem("Normal Title"), "Normal Title");
    }

    #[test]
    fn safe_stem_never_contains_separator() {
        assert!(!safe_stem("a/b\\c").contains('/'));
        assert!(!safe_stem("a/b\\c").contains('\\'));
    }
}
