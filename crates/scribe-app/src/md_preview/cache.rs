//! One-entry parse cache for the markdown preview pane.
//!
//! The preview pane is redrawn on **every** egui frame it is open, and before
//! this cache existed each of those frames re-ran the full `pulldown-cmark`
//! parse and rebuilt the whole [`MdBlock`] tree — for a document the user is
//! typing into, that is the same work thousands of times per minute for a
//! result that changes only when a key is pressed.
//!
//! The cache holds exactly one entry, keyed on the **full source text** plus the
//! **parser-option bits** in effect:
//!
//!   * Keying on the whole source (rather than a hash) makes a stale-content hit
//!     structurally impossible — there is no collision to reason about. The
//!     comparison is a `memcmp` that short-circuits on length, which is orders
//!     of magnitude cheaper than a parse.
//!   * Keying on the option bits means that if [`super::PARSER_OPTIONS`] is ever
//!     changed, an entry built under the old flag set is invalidated instead of
//!     being silently served with the wrong extensions enabled.
//!
//! One entry is the right size: the pane renders one document at a time, so a
//! larger cache would only add eviction policy for no hit-rate gain.

use super::{parse, MdBlock};

/// A single-entry, content-keyed cache over [`parse`].
pub(crate) struct PreviewCache {
    /// `(source, parser-option bits)` the cached blocks were produced from.
    key: Option<(String, u32)>,
    blocks: Vec<MdBlock>,
    /// Number of real [`parse`] calls made. Test-only instrumentation: the
    /// wiring test asserts that re-showing unchanged source does NOT re-parse,
    /// which is the whole point of this type.
    #[cfg(test)]
    parses: u64,
}

impl PreviewCache {
    /// An empty cache. `const` so the thread-local holding it needs no lazy
    /// initialisation on first access.
    pub(crate) const fn new() -> Self {
        Self {
            key: None,
            blocks: Vec::new(),
            #[cfg(test)]
            parses: 0,
        }
    }

    /// The parsed blocks for `src`, reusing the cached result when neither the
    /// source nor the active parser options have changed since the last call.
    pub(crate) fn blocks(&mut self, src: &str) -> &[MdBlock] {
        self.blocks_for(src, super::PARSER_OPTIONS.bits())
    }

    /// [`blocks`](Self::blocks) with the option bits supplied explicitly, so the
    /// option half of the cache key is directly testable (the live caller always
    /// passes the one real value).
    fn blocks_for(&mut self, src: &str, opt_bits: u32) -> &[MdBlock] {
        let hit = matches!(&self.key, Some((cached, bits)) if *bits == opt_bits && cached == src);
        if !hit {
            self.blocks = parse(src);
            self.key = Some((src.to_owned(), opt_bits));
            #[cfg(test)]
            {
                self.parses += 1;
            }
        }
        &self.blocks
    }

    /// How many times this cache actually parsed. Test-only.
    #[cfg(test)]
    pub(crate) fn parses(&self) -> u64 {
        self.parses
    }
}

impl Default for PreviewCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BITS: u32 = 0;

    #[test]
    fn repeated_identical_source_parses_exactly_once() {
        // The reason this type exists: the preview pane calls it every frame.
        let mut c = PreviewCache::new();
        let src = "# Title\n\nbody **text**\n";
        for _ in 0..25 {
            let blocks = c.blocks_for(src, BITS);
            assert!(!blocks.is_empty());
        }
        assert_eq!(c.parses(), 1, "25 identical frames must parse once");
    }

    #[test]
    fn changed_source_reparses_and_returns_the_new_blocks() {
        // A stale hit would render the OLD document — the failure this cache
        // must never produce.
        let mut c = PreviewCache::new();
        assert_eq!(c.blocks_for("# One\n", BITS).len(), 1);
        assert!(matches!(&c.blocks_for("# One\n", BITS)[0],
            MdBlock::Heading { text, .. } if text == "One"));
        let second = c.blocks_for("# Two\n", BITS).to_vec();
        assert!(
            matches!(&second[0], MdBlock::Heading { text, .. } if text == "Two"),
            "changed source must yield the NEW blocks, got {second:?}"
        );
        assert_eq!(c.parses(), 2);
    }

    #[test]
    fn same_length_different_content_is_not_a_hit() {
        // A length-only or prefix-only key would collide here. The key is the
        // full text, so it cannot.
        let mut c = PreviewCache::new();
        assert!(matches!(&c.blocks_for("# aaa\n", BITS)[0],
            MdBlock::Heading { text, .. } if text == "aaa"));
        let out = c.blocks_for("# bbb\n", BITS).to_vec();
        assert!(
            matches!(&out[0], MdBlock::Heading { text, .. } if text == "bbb"),
            "equal-length different content must re-parse, got {out:?}"
        );
        assert_eq!(c.parses(), 2);
    }

    #[test]
    fn changed_option_bits_invalidate_an_otherwise_identical_entry() {
        // Guards the second half of the key: if PARSER_OPTIONS is ever changed,
        // an entry parsed under the old flags must not be served.
        let mut c = PreviewCache::new();
        let src = "| a |\n|---|\n| 1 |\n";
        c.blocks_for(src, 1);
        assert_eq!(c.parses(), 1);
        c.blocks_for(src, 1);
        assert_eq!(c.parses(), 1, "same bits + same source is a hit");
        c.blocks_for(src, 2);
        assert_eq!(c.parses(), 2, "different option bits must invalidate");
    }

    #[test]
    fn empty_source_is_cached_like_any_other() {
        let mut c = PreviewCache::new();
        assert!(c.blocks_for("", BITS).is_empty());
        assert!(c.blocks_for("", BITS).is_empty());
        assert_eq!(c.parses(), 1);
        // ...and moving off empty still re-parses.
        assert!(!c.blocks_for("x\n", BITS).is_empty());
        assert_eq!(c.parses(), 2);
    }

    #[test]
    fn the_live_entry_point_uses_the_real_parser_options() {
        // `blocks()` must key on PARSER_OPTIONS — not on 0 or a stale constant —
        // so a table (a PARSER_OPTIONS-gated extension) actually parses.
        let mut c = PreviewCache::new();
        let blocks = c.blocks("| a | b |\n|---|---|\n| 1 | 2 |\n").to_vec();
        assert!(
            blocks.iter().any(|b| matches!(b, MdBlock::Table { .. })),
            "blocks() must parse with PARSER_OPTIONS, got {blocks:?}"
        );
        assert_eq!(c.parses(), 1);
        c.blocks("| a | b |\n|---|---|\n| 1 | 2 |\n");
        assert_eq!(c.parses(), 1, "and it must still hit the cache");
    }
}
