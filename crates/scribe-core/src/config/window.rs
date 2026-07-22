//! Window appearance configuration: translucency mode, opacity, and tint
//! overlay ([`WindowMode`] + [`WindowConfig`]).

use serde::{Deserialize, Serialize};

/// Window translucency mode. `Opaque` is the default.
///
/// **Back-compat aliases — NO OS blur.** `Transparent`, `Glass`, `Mica`, and
/// `Vibrancy` are retained only so a config that stored one of them still
/// deserializes; NONE of them selects an OS backdrop material. There is no DWM
/// blur / acrylic / mica surface: applying a DWM material re-added the native
/// caption buttons over the custom titlebar (the "double caption" bug) AND the
/// materials were visually indistinguishable in practice, so every non-`Opaque`
/// variant collapses onto the single [`WindowConfig::transparency_enabled`]
/// toggle. The `mode` field is therefore VESTIGIAL —
/// [`WindowConfig::effective_translucent`] reads only `transparency_enabled` and
/// never consults `mode`. Selecting `Glass`/`Mica`/`Vibrancy` means exactly
/// "plain translucency once the master toggle is on"; it promises no OS effect
/// this app cannot deliver. That method is the single predicate every render
/// path consults.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum WindowMode {
    #[default]
    Opaque,
    Transparent,
    Glass,
    Mica,
    Vibrancy,
}

/// Window appearance: translucency mode, opacity, and a color tint overlay.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct WindowConfig {
    /// Master on/off switch for the whole transparency system. When `false`
    /// the window paints fully opaque regardless of `mode` (safe, fast, and
    /// avoids the layered-window ghost-on-close failure mode on Windows).
    /// Default OFF — translucency is opt-in.
    pub transparency_enabled: bool,
    pub mode: WindowMode,
    /// Surface opacity for translucent modes (0.0..=1.0; the 0.0 floor lets the
    /// window go fully transparent — the editor text is painted opaque on top,
    /// so it stays legible even at zero chrome alpha).
    pub opacity: f32,
    /// Master on/off switch for the window colour tint. When `false` no tint is
    /// applied regardless of `tint`/`tint_strength`, so the user can toggle the
    /// effect without losing their chosen colour + strength. Default ON (the
    /// tint only shows once `tint_strength` is raised above 0).
    pub tint_enabled: bool,
    /// Tint color (`#RRGGBB`) blended into the window background at `tint_strength`.
    pub tint: String,
    /// Tint strength (0.0 = none .. 1.0 = strong).
    pub tint_strength: f32,
    /// F-035 from docs/audits/overlooked-surfaces-2026-05-29.md: keep the
    /// SCR1B3 window on top of other windows. Default OFF.
    #[serde(default)]
    pub always_on_top: bool,
}

impl WindowConfig {
    /// Whether translucency should actually be rendered. Transparency is now a
    /// single enable/disable toggle: when on, the frameless transparent surface
    /// reveals the desktop through the translucent panels. There is no OS
    /// blur/backdrop mode — applying a DWM material (Mica/Acrylic/Tabbed) re-added
    /// the native caption buttons over the custom titlebar (the "double caption"
    /// bug) AND the materials were visually indistinguishable in practice, so the
    /// modes were collapsed to this toggle. The legacy `mode` field is retained
    /// only for config back-compat and no longer selects a surface. This is the
    /// single predicate every render path consults.
    pub fn effective_translucent(&self) -> bool {
        self.transparency_enabled
    }
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            transparency_enabled: false,
            mode: WindowMode::Opaque,
            // Fresh installs default to fully opaque. Inert unless the user
            // enables transparency (`transparency_enabled` defaults false).
            // Fresh-configs-only: an already-persisted config keeps whatever
            // opacity it stored (serde deserializes it untouched) — this default
            // only applies to a brand-new / never-set config.
            opacity: 1.0,
            tint_enabled: true,
            tint: "#08060d".to_string(),
            tint_strength: 0.0,
            always_on_top: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn transparency_off_by_default() {
        // Master toggle defaults OFF: a normal opaque window (no ghost-on-close
        // risk, no perf cost). See T19.1/T19.2.
        assert!(!Config::default().window.transparency_enabled);
        assert!(!Config::default().window.effective_translucent());
    }

    #[test]
    fn effective_translucent_tracks_the_single_toggle() {
        // Transparency is a single enable/disable toggle now — the legacy `mode`
        // field no longer participates (it is back-compat only). Off => opaque.
        let w = WindowConfig {
            transparency_enabled: false,
            ..Default::default()
        };
        assert!(!w.effective_translucent(), "toggle off stays opaque");
        // Toggle on => translucent, regardless of the vestigial mode value.
        let w = WindowConfig {
            transparency_enabled: true,
            ..Default::default()
        };
        assert!(w.effective_translucent());
        // The mode field does not change the outcome either way.
        let w = WindowConfig {
            mode: WindowMode::Opaque,
            transparency_enabled: true,
            ..Default::default()
        };
        assert!(w.effective_translucent());
    }

    /// The rustdoc promise, pinned behaviourally: `Glass`/`Mica`/`Vibrancy` (and
    /// `Transparent`) are back-compat ALIASES that carry NO OS blur and do NOT
    /// independently drive translucency — every mode collapses onto the single
    /// `transparency_enabled` toggle. This is the guard on "the doc no longer
    /// claims a capability that isn't there": if a future edit re-wired `mode`
    /// into `effective_translucent` (re-introducing the OS-blur claim the doc
    /// rejects), the master-OFF cases below would flip to `true` and fail.
    #[test]
    fn no_window_mode_independently_drives_translucency() {
        for mode in [
            WindowMode::Opaque,
            WindowMode::Transparent,
            WindowMode::Glass,
            WindowMode::Mica,
            WindowMode::Vibrancy,
        ] {
            // Master toggle OFF => opaque for EVERY mode, including the ones whose
            // names hint at an OS backdrop. `mode` is vestigial.
            let off = WindowConfig {
                transparency_enabled: false,
                mode,
                ..Default::default()
            };
            assert!(
                !off.effective_translucent(),
                "mode {mode:?} must NOT enable translucency while the master toggle is OFF — \
                 it is a back-compat alias, not an OS-blur lever"
            );
            // Master toggle ON => translucent for every mode; the single toggle is
            // the sole lever, exactly as the rustdoc states.
            let on = WindowConfig {
                transparency_enabled: true,
                mode,
                ..Default::default()
            };
            assert!(
                on.effective_translucent(),
                "the single transparency toggle drives translucency for every mode ({mode:?})"
            );
        }
    }

    /// F-035: always_on_top defaults OFF and round-trips through TOML.
    #[test]
    fn always_on_top_default_off_and_round_trips() {
        let c = Config::default();
        assert!(!c.window.always_on_top);
        let mut c2 = c.clone();
        c2.window.always_on_top = true;
        let s = c2.to_toml_string();
        let back: Config = toml::from_str(&s).expect("config TOML round-trip");
        assert!(back.window.always_on_top);
    }
}
