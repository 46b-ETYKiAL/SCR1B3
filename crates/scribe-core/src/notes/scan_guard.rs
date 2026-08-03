//! Forward-progress guard for hand-written scan loops.
//!
//! The note parsers ([`super::tags`], [`super::wikilink`]) walk their input with
//! an explicit cursor and a `while` loop, because a plain `for` cannot express
//! their variable stride — a tag jumps the cursor past the whole tag body, a
//! wikilink jumps it past the closing `]]`. That shape buys the stride at the
//! cost of a real hazard: **every branch has to remember to advance the
//! cursor**, and nothing structurally enforces it. One branch that forgets is an
//! infinite loop, which in a text editor means the UI freezes on one specific
//! input.
//!
//! That hazard is invisible to the test suite. A test cannot observe a program
//! that never returns — it can only hang alongside it. Mutation testing made the
//! blind spot concrete: eight mutants that turn a cursor advance into a stall
//! (`i += 1` -> `i *= 1`) or a rewind (`+=` -> `-=`) were reported TIMEOUT rather
//! than caught, because the mutated build ran until the harness gave up. Verified
//! by hand: applying one of them and running `notes::tags` exits 124 (timed out),
//! never a failing assertion.
//!
//! [`assert_advanced`] converts that class from "hangs forever" into "fails
//! immediately, naming the loop". It is a `debug_assert!`, so it costs nothing in
//! a release build; tests and `cargo mutants` both run debug, which is exactly
//! where the hazard needs to be loud.
//!
//! The guard is a free function with its own direct tests rather than an inline
//! `debug_assert!` at each call site for a specific reason: an inline assertion's
//! own comparison could be weakened (`>` -> `>=`) with nothing to notice, trading
//! eight timeouts for a fresh crop of surviving mutants. Here, the
//! `#[should_panic]` tests below pin the comparison and the body, so weakening
//! either one fails a named test.

/// Assert a scan cursor moved strictly forward.
///
/// `prev` is the cursor at the top of the previous iteration, `next` at the top
/// of this one. Equal means the loop made no progress; smaller means it rewound.
/// Either way the loop cannot terminate, so this panics in debug rather than
/// letting the caller spin.
///
/// `what` names the loop so the panic points at the offending scanner instead of
/// at this shared helper.
/// Deliberately NOT `mutants::skip`ped: the tests below kill every mutant this
/// function can carry. Weakening `>` to `>=` fails `rejects_a_stall`, flipping it
/// to `<`/`<=`/`==` fails `accepts_forward_progress`, `!=` fails
/// `rejects_a_rewind`, and emptying the body fails all three `should_panic`s.
#[inline]
pub(super) fn assert_advanced(prev: usize, next: usize, what: &str) {
    debug_assert!(
        next > prev,
        "{what}: scan cursor did not advance ({prev} -> {next}); this loop would \
         never terminate. Every branch must move the cursor forward before it \
         `continue`s."
    );
}

#[cfg(test)]
mod tests {
    use super::assert_advanced;

    /// The ordinary case: a cursor that stepped forward is accepted silently.
    ///
    /// Without this, a mutant that makes the guard panic unconditionally (or
    /// inverts the comparison) would sail through the two `should_panic` tests
    /// below — they would still panic, just for the wrong reason.
    #[test]
    fn scan_guard_accepts_forward_progress() {
        assert_advanced(0, 1, "unit");
        assert_advanced(3, 4, "unit");
        // A stride jump, which is the whole reason these loops are hand-written.
        assert_advanced(3, 97, "unit");
    }

    /// A stalled cursor is the exact shape of the `i += 1` -> `i *= 1` mutant.
    #[test]
    #[should_panic(expected = "scan cursor did not advance")]
    fn scan_guard_rejects_a_stall() {
        assert_advanced(7, 7, "unit");
    }

    /// A rewound cursor is the `+=` -> `-=` mutant. It also catches a guard
    /// weakened from `>` to `>=`, which a stall alone would not.
    #[test]
    #[should_panic(expected = "scan cursor did not advance")]
    fn scan_guard_rejects_a_rewind() {
        assert_advanced(7, 6, "unit");
    }

    /// The panic names the caller's loop, not this helper — otherwise a failure
    /// in one of four scan loops would be indistinguishable from the others.
    #[test]
    #[should_panic(expected = "scan_line_tags")]
    fn scan_guard_names_the_calling_loop() {
        assert_advanced(2, 2, "scan_line_tags");
    }
}
