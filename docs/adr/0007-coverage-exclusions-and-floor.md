# ADR 0007 — Coverage Exclusions and the CI Coverage Floor

**Status:** Accepted

## Context

The SCR1B3 coverage initiative (work-units WU-1 .. WU-7) raised workspace line
coverage from ~73.9% to **82.77%** by testing the genuinely-testable surface
(engine logic, e2e/kittest GUI drive-throughs, payload assembly, parsers,
state reducers). After those WUs landed, the remaining uncovered surface is no
longer "we have not written the test yet" — a material fraction of it is
**structurally uncoverable in a headless CI runner** and will never be reached
by any honest test that runs without a GPU adapter, a real OS window, a live
subprocess, or a multi-gigabyte file.

A coverage gate that counts those lines against the percentage punishes the
project for code that *cannot* be exercised in CI, and it pressures authors
toward the worst possible response: writing fake tests that call into
GPU/FFI/boot code purely to mark the line "executed" without asserting
anything. That is a soft-completion leak (a test that exists to move a number,
not to catch a bug).

This ADR records (a) the measured testable-coverage ceiling, (b) each excluded
file/region and *why* it is structurally uncoverable headless, (c) the rule
that these surfaces get **exclusions, never faked tests**, and (d) the CI
floor that locks the achieved level in.

Source assessment: the per-file coverage gap map produced at the start of the
coverage initiative (the WU planning artifact), measured with cargo-llvm-cov +
nextest under `--test-threads=1`.

## Decision

### 1. Whole-file exclusions (purely uncoverable headless)

Four files are excluded from the coverage denominator via cargo-llvm-cov's
`--ignore-filename-regex` in `.github/workflows/coverage.yml`. Each is
**0%-or-near-0% coverable in CI by construction** — not because tests are
missing:

| Excluded file | CI line cov | Why structurally uncoverable headless |
|---|---|---|
| `scribe-app/src/app/visual_qa.rs` | 0.00% | The GPU render-QA harness itself. Every scene is `#[ignore]`-gated and only runs when a real **wgpu adapter** resolves. CI has no GPU; the scenes never execute. This is the visual-QA discipline working as designed — the harness is the thing that needs a GPU, so it cannot self-cover headless. |
| `scribe-app/src/app/effects.rs` | 0.00% | Pure `ctx.layer_painter().rect_filled(...)` CRT/motion overlays (tint, scanline, flicker, VHS, wired-mesh, caret-trail, boot-glitch). Their entire output is **GPU pixels**; there is no return value or observable state to assert. The only non-painter logic (`Rgba::parse_hex` clamp guards) is already covered where it lives in `scribe-render`. |
| `scribe-app/src/main.rs` | 0.00% | The `eframe::run_native` process/window **entry point**. `main()` owns the OS event loop and never returns under a test harness; the env-filter init and `ExitCode` plumbing run only in a real launched process. A test cannot enter `main()` without launching the actual windowed app. |
| `scribe-win32-chrome/src/lib.rs` | 31.96% | Live Win32 FFI — `SetWindowLongPtrW` / `SetWindowPos` / `EnumWindows` / the `nc_subclass_proc` window-procedure — needs a **real HWND + DWM**. The FFI bodies are `#[cfg(windows)]`-gated, so on the Linux CI runner they are **not even compiled**, and on a Windows runner they would still need a live composited window. The pure style-math helpers (`caption_button_styles_present`, `style_without_caption_buttons`, inset/clamp math) ARE tested; whole-file exclusion is chosen because cargo-llvm-cov cannot region-exclude the FFI bodies cleanly and the file's coverable surface is dominated by `cfg(windows)` dead code on the CI target. |

The exclusion regex (cross-platform — `[/\\]` separator classes so it matches
both the Windows backslash path form and the Linux forward-slash form CI uses):

```
scribe-app[/\\]src[/\\]app[/\\](visual_qa|effects)\.rs$|scribe-app[/\\]src[/\\]main\.rs$|scribe-win32-chrome[/\\]src[/\\]lib\.rs$
```

### 2. Region-level uncoverable arms (inside otherwise-covered files)

These files are **kept in the denominator** (their testable surface is large
and well-covered), but they each carry a residue of headless-uncoverable
arms. cargo-llvm-cov 0.8.7 has **no stable inline-comment region-exclusion**
mechanism (that is a grcov-only feature), so per the WU-0 contract these arms
are **left in place and accounted for here — never faked**:

These arms fall into a small number of **categories**, each defined by the
host facility it hands off to:

| Category | Why it cannot be entered headless |
|---|---|
| Native OS dialogs | Blocking open/save pickers have no headless return value. |
| OS launch glue | Handing a URL to the default browser / mail client leaves the process. The URL-assembly logic around the hand-off *is* covered. |
| Background threads | Work moved onto a spawned thread is not entered by a single-threaded instrumented run. The pure state-transition reducers driving them *are* covered. |
| Child processes | Driving a real child over stdio needs a live process; the pure protocol/message framing around it *is* covered. |
| Network transport arms | Error/redirect branches need a live or mock endpoint. Happy-path parsing *is* covered. |
| Very-large-file paths | Guarded by a non-injectable size `const`; materialising a fixture past the threshold is not reasonable in CI. |
| Platform-gated arms | `#[cfg]`-gated code does not compile on the CI target, so it never enters the CI denominator. |
| GPU render/diff gates | Need a real graphics adapter; these lanes are capability-gated and honestly **skip** headless — they never pass falsely. |
| Per-frame render loop | Residue fragmented across many small event-handler bodies and paint geometry, each reachable only by a real layout pass. Ordinary (expensive) backlog, not an exclusion. |

> Per-file coverage numbers are deliberately **not** enumerated here: they are a
> measurement, not a decision, and a stale table is worse than no table. Read the
> current values from the coverage run itself (`cargo llvm-cov --workspace`)
> rather than from this ADR. Note that `cfg`-gated code compiles locally and not
> on the CI target, so local and CI numbers differ by design.

### 3. The rule

**Structurally-uncoverable-headless surfaces get exclusions or are accounted
for in this ADR — they are NEVER given fake tests.** A test that enters
GPU/FFI/boot/subprocess/network/large-file code solely to mark a line
"executed" without a meaningful assertion is forbidden (it is a
soft-completion leak — `thorough-completion.md` / `no-future-work.md`). The
correct CI path for these surfaces is:

- GPU render-QA → the `#[ignore]`'d `visual_qa.rs` harness, run on demand with
  a real wgpu adapter (optional local lane), not in the headless gate.
- FFI / OS-process / OS-dialog / OS-launch → exercised by the developer on the
  real OS, not asserted in the headless gate.
- eframe boot → validated by actually launching the app, not by a unit test.

### 4. Verification correction (gap-map claim refuted)

The gap-map hypothesised that *"the tree-sitter-rust native grammar fails to
build under the llvm-cov instrumentation profile, so `syntax.rs`'s tree-sitter
arms fall back to syntect and are engine-uncoverable in CI."* **This was
measured to be false.** Under the instrumented profile, `scribe-core/src/syntax.rs`
measures **92.09% line coverage**, and the tree-sitter assertions
(`tree_sitter_rust_grammar_is_wired` asserting `tree_sitter_language_count() == 1`,
and `rust_uses_tree_sitter_and_colors_keywords`) **pass under coverage**. The
grammar DOES build and execute under instrumentation. Therefore `syntax.rs` is
**not excluded** — its residual ~63 missed lines are ordinary
less-common-token-rule backlog, not a structural exclusion.

### 5. Amendment — the frame-independent-surface WU (floor 85 → 90)

This ADR anticipated "RAISE this floor as future testing WUs reach the
`app/mod.rs` backlog". That WU has now landed, and the floor moves **85 → 90**.

The backlog it drained shared one root cause, and it was *not* "nobody wrote the
test yet": these methods read their caret/selection from live
`egui::TextEdit` state, so under a plain unit test `load_state` returns `None`
and every method early-returns having done nothing. A test could call them and
assert nothing had happened — and pass. Storing a real `TextEditState` at the id
the method loads is the whole unlock; no render loop is required. The same shape
applied to the deferred-action dispatcher and the keyboard handlers, which were
reachable only through a live frame.

| File | Before | After |
|---|---|---|
| `scribe-app/src/app/text_ops_methods.rs` | 33.07% | **94.98%** |
| `scribe-app/src/app/session_io.rs` | 45.80% | **75.57%** |
| `scribe-app/src/app/deferred_actions.rs` | 52.53% | **77.89%** |
| `scribe-app/src/app/keyboard_input.rs` | 72.47% | **90%+** |
| **workspace total** | 89.88% | **91.28%** |

Two new exclusions are recorded under §2 rather than tested
(`save_as_active` / `act.open` / `act.open_folder` are `rfd::FileDialog` calls
that block on a human; `integration/windows.rs` mutates the live registry and is
not compiled on CI at all). Both follow §3: accounted for, never faked.

**This WU also found a real user-facing bug**, which is the argument for doing it
at all rather than treating the number as the goal: `Ctrl+Shift+[` (fold-all)
called the brace-only `fold_regions`, so in a markdown note it found zero regions
— the shortcut switched the user into fold view with nothing folded and reported
"folded 0 region(s)", while the palette's `BuiltinCommand::FoldAll` and the fold
gutter's button (both language-aware) folded the same buffer correctly. The
handler had been moved verbatim out of `frame_tick` before `fold_regions_for`
existed and was never updated with its siblings. Nothing caught it because
nothing had ever driven that dispatcher.

## Consequences

- **Measured testable line coverage rises from 82.77% → 84.61%** once the four
  whole-file structural exclusions are applied (the 674 excluded lines, of which
  604 were uncovered, no longer drag the denominator).
- **The CI floor (`--fail-under-lines`) was raised 74 → 85**, and then **85 → 90**
  by the frame-independent-surface WU (§5), locking in the achieved level with a
  safety margin for normal fluctuation.
  RAISE this floor further as the `frame_tick` / `settings.rs` click-handler
  backlog lands; never lower it to make a red build pass.
- **100% is not the target and is not reachable** on this codebase: the residue
  is GPU pixels, native dialogs, FFI, subprocesses, and a 256 MiB fixture. §3 is
  the governing rule — the honest ceiling is "no *testable* line is untested",
  and chasing the last few points with tests that assert nothing would make the
  number go up and the suite worth less.
- The reported percentage now reflects the **testable** surface. The excluded
  surface (GPU / FFI / OS-process / boot) is documented here, not faked.
- The exclusion list is narrow and full-path-anchored, so it cannot silently
  swallow a future newly-added file.

## References

- The coverage initiative's per-file gap-map assessment (WU planning artifact).
- `.github/workflows/coverage.yml` — the gate (exclusion regex + floor).
- ADR 0006 (`0006-syntax-engine.md`) — the tree-sitter-primary / syntect-fallback engine
  decision referenced by the verification correction above.
