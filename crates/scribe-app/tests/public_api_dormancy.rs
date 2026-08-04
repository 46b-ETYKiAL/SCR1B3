//! The enforced ledger of public APIs with **no production caller**.
//!
//! A public function whose only call sites are tests is not "covered" — it is
//! shipped surface that no user can reach. That is fine when it is a deliberate,
//! stated position and a defect when it is an accident nobody wrote down. The
//! difference is whether anything checks. This file is the thing that checks.
//!
//! For every [`DORMANT`] entry the guard asserts the stated status is still TRUE:
//! the symbol has zero call sites in production code (comments, string literals,
//! `#[cfg(test)]` items and whole test-only files all stripped by `common`). If
//! someone wires one up, the guard FAILS and demands the entry be removed — so
//! the in-code status can never quietly rot into a lie.
//!
//! [`the_detector_sees_a_real_production_caller`] is the anti-vacuity control: it
//! runs the same probe over APIs that ARE called and requires hits. Without it, a
//! corpus that had silently become empty would report every entry as dormant and
//! the whole file would pass while proving nothing.

mod common;

use common::{contains_token, production_source, CorpusOptions};

/// One public API with no production caller.
struct Dormant {
    /// The call-site spelling to search for. Chosen so the DEFINITION does not
    /// match it (`Buffer::open` never appears at `pub fn open`), or paired with
    /// `defined_in` below when it does.
    symbol: &'static str,
    /// File name to drop from the corpus because it DEFINES the symbol and would
    /// otherwise match the probe. Empty when the probe cannot match its own
    /// definition.
    defined_in: &'static str,
    /// Why this is a stated position rather than an accident.
    why: &'static str,
}

/// The ledger. Every entry is a deliberate, documented position — see the
/// matching rustdoc on the item itself, which points back here.
const DORMANT: &[Dormant] = &[
    Dormant {
        symbol: "Buffer::open",
        defined_in: "",
        why: "scribe-core::buffer::Buffer::open — the lazy-mmap large-file loader. \
              NOT YET WIRED, and deliberately recorded as such rather than left \
              silently dormant. The app's open path is ScribeApp::open_path -> \
              Document::open, and a tab materialises its content as `tab.text: \
              String`; the rope editor then builds Buffer::from_text(&tab.text) \
              from that already-decoded string. Buffer::open's whole point is to \
              STAY in the mmap representation until the first edit, which cannot \
              happen while the tab model decodes the file to a String at open \
              time. Wiring it means changing the tab model in \
              scribe-app/src/app/mod.rs, scribe-core/src/document.rs and \
              scribe-app/src/app/frame_tick.rs together — see the rustdoc on \
              Buffer::open for the full contract that has to move with it.",
    },
    Dormant {
        symbol: "replace_all",
        defined_in: "search.rs",
        why: "scribe-core::search::replace_all — the unbounded half of the \
              replace_all / replace_n pair, mirroring the regex crate's own \
              replace_all / replacen API shape. The editor's single replace call \
              site (scribe-app/src/app/find_replace.rs) passes a dynamic \
              Option<usize> limit, so it always calls replace_n directly; \
              replace_all is exercised by the crate's criterion bench \
              (scribe-core/benches/search.rs) and by the proptest / miri / \
              correctness integration suites. It is workspace-internal — \
              scribe-core is not published and the Rhai plugin surface exposes no \
              search API — so nothing outside this repo depends on it.",
    },
];

/// APIs that ARE called from production. These are the anti-vacuity controls:
/// the probe must find them, or the ledger above proves nothing.
const LIVE_CONTROLS: &[(&str, &str)] = &[
    ("find_all", "search.rs"),
    ("replace_n", "search.rs"),
    ("Buffer::from_text", ""),
    ("Document::open", ""),
];

fn corpus(defined_in: &str) -> String {
    production_source(&CorpusOptions {
        skip_config_dir: false,
        skip_file_names: if defined_in.is_empty() {
            Vec::new()
        } else {
            vec![defined_in.to_string()]
        },
    })
}

#[test]
fn every_dormant_entry_still_has_no_production_caller() {
    for entry in DORMANT {
        let src = corpus(entry.defined_in);
        assert!(
            !contains_token(&src, entry.symbol),
            "`{}` now HAS a production caller. That is good news — delete its \
             entry from DORMANT and update the rustdoc that points here, so the \
             recorded status stops contradicting the code.\n\nRecorded status: {}",
            entry.symbol,
            entry.why,
        );
    }
}

/// Anti-vacuity: the same probe, over APIs that really are called, must find
/// them. If this goes red the corpus is broken and
/// `every_dormant_entry_still_has_no_production_caller` is passing for the wrong
/// reason.
#[test]
fn the_detector_sees_a_real_production_caller() {
    for (symbol, defined_in) in LIVE_CONTROLS {
        let src = corpus(defined_in);
        assert!(
            contains_token(&src, symbol),
            "`{symbol}` is called from production code, so the probe MUST find it. \
             It did not — the production corpus is broken, and every DORMANT entry \
             is currently passing vacuously.",
        );
    }
}

/// The ledger is a record of a stated position, so every entry must actually
/// state it. An empty `why` is an accident wearing the ledger's clothes.
#[test]
fn every_dormant_entry_explains_itself() {
    for entry in DORMANT {
        assert!(
            entry.why.len() > 120,
            "`{}` needs a real explanation of why it has no production caller, not \
             a label",
            entry.symbol,
        );
    }
}
