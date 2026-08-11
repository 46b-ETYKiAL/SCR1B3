# ADR 0005 — Theming and Customization

**Status:** Accepted

## Context

Deep customization is a first-class pillar — but it must not become bloat. Users want full control over colors, fonts, effects, and behavior without recompiling, without a heavy plugin marketplace, and without a brittle theme format that can blank the editor when it has an error.

## Decision

Customization is **config-driven and theming is a Helix-style three-namespace TOML schema**.

- **Themes are TOML** with three orthogonal tables: `[palette]` (named base colors), `[ui]` (chrome), and `[syntax]` (token scopes). Values are either `#`-hex literals (`#RGB` / `#RRGGBB` / `#RRGGBBAA`) or references to a palette name — define a color once, reference it everywhere.
- **UI-toolkit-agnostic colors.** The engine stores colors as RGBA; the render layer maps them onto egui. The engine carries no UI dependency.
- **Longest-matching-scope-wins** syntax resolution: `function.builtin.static` falls back to `function.builtin`, then `function`, then a default. Themes can be broad or finely refined.
- **Never blanks the editor.** A compiled-in fallback theme (`wired-noir`) is always available. A broken or malformed user theme surfaces an error and keeps the default rather than rendering an unusable blank screen. Missing UI keys fall back to defaults, so a theme can be small.
- **Effects are separate from themes.** The CRT/retro post-process (scanline, phosphor glow, bloom, vignette, curvature, chromatic aberration) was envisioned as a config-driven layer in `[effects]`, orthogonal to color themes so any theme could run flat or under CRT.

> **Update (not shipped):** the `[effects]` scaffold was **removed rather than shipped as dead toggles**. No GPU/WGSL post-process shader was implemented, so there is no `[effects]` config table or `EffectsConfig` in `scribe-core` — a user's `[effects]` keys would have been silently ignored. The retro aesthetic is instead carried by the color themes themselves (e.g. `phosphor-amber`, `terminal-lock`). The only motion-related config that ships is `[motion]` (`MotionConfig`), which scales egui's native animation time and is OFF by default. (This mirrors the same decision recorded in `crates/scribe-core/src/config/mod.rs` for the per-effect motion catalog: features without a renderer implementation were dropped, not shipped as no-op toggles.) If a post-process pass lands later it will be re-introduced as a real, documented feature.

> **Update 2 (2026-07-27) — correcting Update 1.** Update 1 above is now partly
> inaccurate and is superseded on two points; it is kept for the record.
>
> 1. **`[motion]` is not just an animation-time scale.** It grew a real CRT/retro
>    effect catalog that DOES ship: `crt_scanlines` (+ `scanline_darkness`),
>    `flicker` (+ strength/speed), `vhs_tracking` (+ speed) and `wired_ambient`
>    (+ mesh density/brightness/drift/color). These are drawn as
>    `ctx.layer_painter()` overlays compositing over the finished frame — so the
>    "no GPU/WGSL post-process shader" half of Update 1 remains TRUE, and the
>    shader-only effects (phosphor glow, bloom, curvature, chromatic aberration)
>    are still unimplemented. What changed is that the scanline/flicker/VHS subset
>    was reachable without a shader and shipped as painter overlays.
> 2. **The `[motion]` master is ON by default** (`MotionConfig::enabled = true`),
>    not off — subtle motion is part of the intended feel. It is the *individual
>    CRT/VHS effects* that default to OFF.
>
> The `[effects]` table itself was still never created, so Update 1's core
> decision stands: effects config lives under `[motion]`, and no key is shipped
> as a dead no-op toggle. Additionally, motion is now suppressed when the OS
> reports a reduced-motion preference (WCAG 2.3.3), via
> `MotionConfig::effective_enabled`. See
> [THEMING.md](../../THEMING.md#crt-effects-shipped-as-painter-overlays-not-a-gpu-shader).
- **Appearance and behavior** (fonts, ligatures, line height, tab width, minimap, word wrap, session restore, frameless titlebar) are plain config keys — no code, no plugins required.
- **The default theme is `itasha-corp`** — the brand palette of neutral dark (`#121212`) with two primaries: `.Corp` signal green (`#00ff90`, the live voice) and Itasha structural purple (`#7700ff`), with Akira-red (`#ff3b5c`) reserved for alarms. (`wired-noir` remains the compiled-in fallback if a configured theme name cannot be resolved.)

Code-loading extensibility (the user plugin/mod system) is handled separately and is capability-sandboxed; it is not required for deep customization. This keeps the not-bloated promise: most customization needs no code at all.

## Consequences

- Users theme and reconfigure live, without recompiling and without a marketplace.
- A bad theme degrades gracefully to the default; the editor is always usable.
- Themes authored for other Helix-style editors are easy to port.
- The retro aesthetic is carried by opt-in color themes plus the opt-in `[motion]` painter overlays (the `[effects]` GPU post-process pass was not shipped — see Update 2), so it is never imposed and stays accessibility-aware.
