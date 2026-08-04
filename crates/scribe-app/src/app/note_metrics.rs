//! One-entry cache for the markdown preview header's note metrics.
//!
//! The preview header shows "~N min · H headings · ☑ done/total". Producing
//! those three numbers is three full-document scans —
//! [`scribe_core::md_ops::tasks_progress`], a `split_whitespace().count()`, and
//! [`scribe_core::md_ops::heading_outline`] — and they ran **uncached on every
//! egui frame the preview pane was open**. For a note the user is typing into
//! that is the same three passes thousands of times a minute for a result that
//! changes only when a key is pressed. With the markdown parse itself now
//! cached (`md_preview::cache`), these scans were the dominant remaining
//! per-frame cost for a large note.
//!
//! The cache is the same shape as [`crate::md_preview::cache::PreviewCache`],
//! deliberately: exactly one entry, keyed on the **full source text**. Keying on
//! the whole source (rather than a hash) makes a stale-content hit structurally
//! impossible — there is no collision to reason about — and the comparison is a
//! `memcmp` that short-circuits on length, orders of magnitude cheaper than
//! three O(n) scans. One entry is the right size: the preview renders one
//! document at a time.
//!
//! Unlike the parse cache there are no option bits in the key: all three scans
//! are pure functions of the source text alone, with no configuration input.

use std::cell::RefCell;

/// The three numbers the preview header shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct NoteMetrics {
    /// Completed task checkboxes.
    pub done: usize,
    /// Total task checkboxes.
    pub total: usize,
    /// Estimated reading time in minutes.
    pub minutes: usize,
    /// Number of ATX headings outside fenced code blocks.
    pub headings: usize,
}

/// Run the three full-document scans. The cache-miss body, factored out so the
/// cached and uncached results are provably the same computation.
fn scan(src: &str) -> NoteMetrics {
    let (done, total) = scribe_core::md_ops::tasks_progress(src);
    let words = src.split_whitespace().count();
    NoteMetrics {
        done,
        total,
        minutes: scribe_core::md_ops::reading_time_minutes(words),
        headings: scribe_core::md_ops::heading_outline(src).len(),
    }
}

/// A single-entry, content-keyed cache over [`scan`].
#[derive(Default)]
pub(crate) struct NoteMetricsCache {
    /// The source the cached metrics were produced from.
    key: Option<String>,
    value: NoteMetrics,
    /// Number of real [`scan`] calls made. Test-only instrumentation: the whole
    /// point of this type is that an idle frame does NOT re-scan, which cannot
    /// be observed from the returned value (a re-scan returns the same numbers).
    #[cfg(test)]
    scans: u64,
}

impl NoteMetricsCache {
    /// An empty cache. `const` so the thread-local holding it needs no lazy
    /// initialisation on first access.
    pub(crate) const fn new() -> Self {
        Self {
            key: None,
            value: NoteMetrics {
                done: 0,
                total: 0,
                minutes: 0,
                headings: 0,
            },
            #[cfg(test)]
            scans: 0,
        }
    }

    /// The metrics for `src`, reusing the cached result when the source has not
    /// changed since the last call.
    pub(crate) fn metrics(&mut self, src: &str) -> NoteMetrics {
        let hit = matches!(&self.key, Some(cached) if cached == src);
        if !hit {
            self.value = scan(src);
            self.key = Some(src.to_owned());
            #[cfg(test)]
            {
                self.scans += 1;
            }
        }
        self.value
    }

    /// How many times this cache actually scanned. Test-only.
    #[cfg(test)]
    pub(crate) fn scans(&self) -> u64 {
        self.scans
    }
}

thread_local! {
    /// The live cache behind [`metrics_for`]. Thread-local because the egui
    /// frame thread is the only caller and it keeps the cache out of the app
    /// struct, exactly as `md_preview`'s parse cache does.
    static NOTE_METRICS: RefCell<NoteMetricsCache> = const { RefCell::new(NoteMetricsCache::new()) };
}

/// Cached note metrics for `src` — the live entry point used by the preview
/// header.
pub(crate) fn metrics_for(src: &str) -> NoteMetrics {
    NOTE_METRICS.with(|c| c.borrow_mut().metrics(src))
}

/// How many real scans the LIVE cache has run. Test-only: this is what a
/// wiring test watches while driving real frames, since a re-scan and a cache
/// hit return the same numbers and are otherwise indistinguishable.
#[cfg(test)]
pub(crate) fn live_scan_count() -> u64 {
    NOTE_METRICS.with(|c| c.borrow().scans())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTE: &str = "\
# Title

Some prose with several words in it.

## Second heading

- [x] done one
- [ ] not done
- [x] done two

```
# not a heading, it is inside a fence
```
";

    #[test]
    fn the_metrics_are_the_three_scans_the_header_shows() {
        // Pin the VALUES first: a cache that returns wrong numbers fast is worse
        // than no cache. Two headings (the fenced `#` is not one), 3 tasks of
        // which 2 are done, and a non-zero reading time.
        let m = NoteMetricsCache::new().metrics(NOTE);
        assert_eq!((m.done, m.total), (2, 3));
        assert_eq!(
            m.headings, 2,
            "the `#` inside the fence is not a heading — heading_outline's rule"
        );
        assert_eq!(m.minutes, 1);
    }

    #[test]
    fn repeated_identical_source_scans_exactly_once() {
        // The reason this type exists: the preview header runs it every frame.
        // Asserting only the returned VALUE would pass with no cache at all —
        // the scan counter is what distinguishes a hit from a re-scan.
        let mut c = NoteMetricsCache::new();
        let first = c.metrics(NOTE);
        assert_eq!(c.scans(), 1, "the first call is a miss");
        for _ in 0..100 {
            assert_eq!(c.metrics(NOTE), first, "cached result is identical");
        }
        assert_eq!(
            c.scans(),
            1,
            "100 idle frames must re-scan ZERO times — the counter is FLAT"
        );
    }

    #[test]
    fn changed_source_rescans_and_returns_the_new_numbers() {
        // A stale hit would show the OLD note's counts — the failure this cache
        // must never produce.
        let mut c = NoteMetricsCache::new();
        assert_eq!(c.metrics("# One\n").headings, 1);
        assert_eq!(c.scans(), 1);
        let after = c.metrics("# One\n## Two\n");
        assert_eq!(after.headings, 2, "the NEW source's numbers, not the old");
        assert_eq!(c.scans(), 2, "a changed source is a miss");
    }

    #[test]
    fn same_length_different_content_is_not_a_hit() {
        // A length-only, prefix-only, or count-only key would collide here: both
        // sources are 14 bytes and both have one heading, but the task counts
        // differ. The key is the full text, so it cannot.
        let a = "# H\n- [x] ab\n";
        let b = "# H\n- [ ] ab\n";
        assert_eq!(a.len(), b.len(), "precondition: equal length");
        let mut c = NoteMetricsCache::new();
        assert_eq!(c.metrics(a).done, 1);
        let out = c.metrics(b);
        assert_eq!(c.scans(), 2, "equal-length different content must re-scan");
        assert_eq!(
            out.done, 0,
            "and it must return the SECOND source's numbers, got {out:?}"
        );
    }

    #[test]
    fn a_one_character_edit_invalidates_the_entry() {
        // A keystroke is the realistic invalidation, and the one a prefix- or
        // suffix-only key would miss.
        let mut c = NoteMetricsCache::new();
        let long = format!("# H\n{}\n- [ ] task\n", "word ".repeat(500));
        c.metrics(&long);
        assert_eq!(c.scans(), 1);
        let edited = format!("{long}x");
        let m = c.metrics(&edited);
        assert_eq!(c.scans(), 2, "a trailing keystroke must invalidate");
        assert_eq!(m.total, 1);
    }

    #[test]
    fn empty_source_is_cached_like_any_other_and_moving_off_it_rescans() {
        let mut c = NoteMetricsCache::new();
        assert_eq!(c.metrics(""), NoteMetrics::default());
        assert_eq!(c.metrics(""), NoteMetrics::default());
        assert_eq!(c.scans(), 1, "empty is a real entry, not a permanent miss");
        assert_eq!(c.metrics("# x\n").headings, 1);
        assert_eq!(c.scans(), 2);
    }

    #[test]
    fn the_cached_value_equals_the_uncached_scan_for_every_field() {
        // The cache must not quietly compute something different from the scan
        // it replaced — asserted field-by-field over a document that exercises
        // all three scans.
        let mut c = NoteMetricsCache::new();
        assert_eq!(
            c.metrics(NOTE),
            scan(NOTE),
            "cached metrics must equal a fresh scan"
        );
    }

    #[test]
    fn the_live_thread_local_entry_point_caches_too() {
        // `metrics_for` is what the preview header calls. It must go through the
        // cache — a version that called `scan` directly would be correct and
        // useless, and no value assertion could tell the difference.
        let unique = format!("# live-entry-point {:?}\n- [x] a\n", std::time::Instant::now());
        let first = metrics_for(&unique);
        assert_eq!((first.headings, first.done, first.total), (1, 1, 1));
        let before = NOTE_METRICS.with(|c| c.borrow().scans());
        for _ in 0..50 {
            assert_eq!(metrics_for(&unique), first);
        }
        assert_eq!(
            NOTE_METRICS.with(|c| c.borrow().scans()),
            before,
            "50 identical calls through the LIVE entry point re-scan zero times"
        );
    }
}
