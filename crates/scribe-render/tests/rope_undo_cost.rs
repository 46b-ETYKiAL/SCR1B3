//! The COST of the rope editor's undo bookkeeping on the typing path.
//!
//! `apply_event` records a pre-edit snapshot before every mutation, and the
//! `History` COALESCES a run of consecutive inserts into one undo step — so
//! every keystroke of that run after the first hands over a snapshot that is
//! immediately discarded. Built eagerly, that snapshot is a full-buffer
//! `Rope::to_string()`: one whole-buffer copy per keypress, thrown away, and
//! worst exactly at the multi-MiB sizes the rope path exists to make fast.
//!
//! Correctness tests cannot see that — the text and the undo stack are identical
//! either way. So this file asserts the COST directly, by counting the bytes the
//! process actually allocates across a coalescing keystroke run and requiring
//! that it does NOT scale with the buffer.
//!
//! ## Why a global allocator rather than a call counter
//!
//! A `#[cfg(test)]` counter incremented next to the `to_string()` call would
//! measure the counter, not the copy: hoist the `to_string()` back out and leave
//! the counter behind and the test still reads green. Bytes allocated is the
//! thing we actually care about and cannot be faked by moving code around.
//!
//! The counter is THREAD-LOCAL, so the measurement is unaffected by whatever
//! other tests in this binary allocate in parallel. It is `const`-initialised
//! and destructor-free, so reading it inside the allocator cannot itself
//! allocate or recurse.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use ropey::Rope;
use scribe_render::{apply_event, RopeEditorState};

thread_local! {
    /// Bytes this thread has asked the allocator for since the process started.
    static ALLOCATED: Cell<usize> = const { Cell::new(0) };
}

fn add(bytes: usize) {
    // `try_with` so an allocation during thread teardown (after TLS is gone)
    // degrades to "not counted" instead of panicking inside the allocator.
    let _ = ALLOCATED.try_with(|c| c.set(c.get().saturating_add(bytes)));
}

struct CountingAlloc;

// SAFETY: every method forwards to `System`, which upholds the `GlobalAlloc`
// contract; the only added work is a thread-local counter bump that neither
// allocates nor deallocates.
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        add(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        add(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // Only GROWTH is new memory; a shrink costs nothing.
        add(new_size.saturating_sub(layout.size()));
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

/// Bytes allocated on this thread while `f` ran.
fn allocated_during<R>(f: impl FnOnce() -> R) -> (R, usize) {
    let before = ALLOCATED.with(Cell::get);
    let out = f();
    let after = ALLOCATED.with(Cell::get);
    (out, after.saturating_sub(before))
}

/// Type `keystrokes` characters into a `buf_bytes`-sized buffer and report the
/// bytes allocated by the COALESCING part of the run.
///
/// The first keystroke is excluded on purpose: it opens the undo group and its
/// snapshot is legitimately kept. Everything measured after it coalesces, so a
/// correct implementation stores nothing at all.
fn coalescing_run_alloc_bytes(buf_bytes: usize, keystrokes: usize) -> usize {
    // ASCII, so chars == bytes and the buffer size is exact.
    let mut rope = Rope::from_str(&"a".repeat(buf_bytes));
    let mut state = RopeEditorState::new();
    // Built once, outside the measured window: constructing the event allocates.
    let key = egui::Event::Text("x".to_string());

    // Keystroke 1 — opens the group, keeps its snapshot.
    apply_event(&mut rope, &mut state, &key);

    let (_, bytes) = allocated_during(|| {
        for _ in 0..keystrokes {
            apply_event(&mut rope, &mut state, &key);
        }
    });

    assert_eq!(
        rope.len_chars(),
        buf_bytes + keystrokes + 1,
        "sanity: every keystroke must actually have been inserted, or the \
         measurement is of a no-op"
    );
    bytes
}

/// Ctrl/Cmd(+Shift)+Z — undo, or redo when `shift`.
fn ctrl_z(shift: bool) -> egui::Event {
    egui::Event::Key {
        key: egui::Key::Z,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers {
            shift,
            command: true,
            ctrl: true,
            ..Default::default()
        },
    }
}

const KEYSTROKES: usize = 32;
const MIB: usize = 1024 * 1024;

/// A coalescing typing run must not cost a buffer copy per keypress.
///
/// Before the lazy-snapshot fix this allocated `KEYSTROKES` full copies of the
/// buffer — ~128 MiB for a 4 MiB buffer — and every one of them was discarded
/// unused by `History::record`'s coalesce.
#[test]
fn a_coalescing_typing_run_does_not_copy_the_buffer_per_keystroke() {
    let cost = coalescing_run_alloc_bytes(4 * MIB, KEYSTROKES);
    // Reported so `--nocapture` shows the real figure, not just pass/fail.
    eprintln!("[cost] {KEYSTROKES} keystrokes into 4 MiB: {cost} bytes allocated");

    assert!(
        cost < MIB,
        "{KEYSTROKES} coalescing keystrokes into a 4 MiB buffer allocated \
         {cost} bytes — that is not far off a whole-buffer copy, let alone the \
         near-nothing a discarded snapshot should cost"
    );
}

/// The cost of a typing run must not SCALE with the buffer.
///
/// This is the assertion a fixed absolute bound cannot make: quadrupling the
/// buffer must not quadruple (or even meaningfully move) the cost of typing into
/// it. A per-keystroke `to_string()` is O(buffer) and fails here by construction,
/// whatever constant a single-size test happened to be tuned to.
#[test]
fn typing_cost_does_not_scale_with_buffer_size() {
    let small = coalescing_run_alloc_bytes(MIB, KEYSTROKES);
    let large = coalescing_run_alloc_bytes(8 * MIB, KEYSTROKES);
    eprintln!("[cost] {KEYSTROKES} keystrokes: 1 MiB buffer -> {small} bytes, 8 MiB buffer -> {large} bytes");

    // 8x the buffer, and the run may allocate at most 64 KiB more. The eager
    // form's delta here is 32 x 7 MiB ≈ 224 MiB.
    let slack = 64 * 1024;
    assert!(
        large <= small.saturating_add(slack),
        "typing into an 8 MiB buffer allocated {large} bytes vs {small} for a \
         1 MiB buffer (slack {slack}) — the per-keystroke cost is scaling with \
         the buffer, which is the whole-buffer snapshot coming back"
    );
}

/// Cutting the cost must not have cut the behaviour: the run is still ONE undo
/// step, and undo/redo still round-trip.
///
/// Kept next to the cost assertions deliberately — "make it cheap" and "keep it
/// correct" are the two halves of the same change, and a cost test that passed
/// because snapshots stopped being recorded at all would be the obvious way to
/// get this wrong.
#[test]
fn the_cheap_path_still_coalesces_the_run_into_one_undo_step() {
    let mut rope = Rope::from_str("");
    let mut state = RopeEditorState::new();
    for ch in ["a", "b", "c", "d"] {
        apply_event(&mut rope, &mut state, &egui::Event::Text(ch.to_string()));
    }
    assert_eq!(rope.to_string(), "abcd");

    apply_event(&mut rope, &mut state, &ctrl_z(false));
    assert_eq!(
        rope.to_string(),
        "",
        "the whole typing run is one undo step, not four"
    );

    apply_event(&mut rope, &mut state, &ctrl_z(true));
    assert_eq!(rope.to_string(), "abcd", "and redo restores the whole run");
}
