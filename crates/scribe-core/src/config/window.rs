//! Window appearance configuration: translucency mode, opacity, and tint
//! overlay ([`WindowMode`] + [`WindowConfig`]).

use serde::{Deserialize, Deserializer, Serialize};

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
/// never consults `mode`. That method is the single predicate every render path
/// consults.
///
/// # The value is inert, and the user is TOLD so
///
/// `scr1b3.toml` is hand-editable and there is no Settings row for `mode`, so
/// the only way a user ever sets `mode = "glass"` is by typing it — precisely
/// because the name promises an OS glass surface. Accepting that silently and
/// doing nothing is the config lying to its user. The hand-written
/// [`Deserialize`] impl below therefore emits a `warn`-level record naming the
/// value, what it does NOT do, and the one knob that does work
/// (`transparency_enabled`). See [`WindowMode::inert_warning`].
///
/// The value is still ACCEPTED (not rejected) on purpose: rejecting would make
/// an existing config unparseable, and `Config::load_or_default` responds to an
/// unparseable file by backing it up and starting from defaults — punishing a
/// user for a setting an older build told them to write. Warn, keep, explain.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum WindowMode {
    #[default]
    Opaque,
    Transparent,
    Glass,
    Mica,
    Vibrancy,
}

impl WindowMode {
    /// Every accepted `[window] mode` spelling, in `scr1b3.toml` form.
    pub const CONFIG_SPELLINGS: &'static [&'static str] =
        &["opaque", "transparent", "glass", "mica", "vibrancy"];

    /// This mode's `scr1b3.toml` spelling (the `rename_all = "lowercase"` form).
    pub const fn as_config_str(self) -> &'static str {
        match self {
            WindowMode::Opaque => "opaque",
            WindowMode::Transparent => "transparent",
            WindowMode::Glass => "glass",
            WindowMode::Mica => "mica",
            WindowMode::Vibrancy => "vibrancy",
        }
    }

    /// Parse a `[window] mode` value; `None` for an unrecognised spelling.
    pub fn from_config_str(raw: &str) -> Option<Self> {
        match raw {
            "opaque" => Some(WindowMode::Opaque),
            "transparent" => Some(WindowMode::Transparent),
            "glass" => Some(WindowMode::Glass),
            "mica" => Some(WindowMode::Mica),
            "vibrancy" => Some(WindowMode::Vibrancy),
            _ => None,
        }
    }

    /// `true` when the value's NAME promises a window surface this app does not
    /// implement, so setting it changes nothing.
    ///
    /// Every non-`Opaque` value qualifies: `mode` is never read by
    /// [`WindowConfig::effective_translucent`], so `transparent`/`glass`/`mica`/
    /// `vibrancy` all describe an effect that will not appear.
    pub const fn is_inert_alias(self) -> bool {
        !matches!(self, WindowMode::Opaque)
    }

    /// The message a user must see when they hand-edit `[window] mode` to a
    /// value that cannot take effect. `None` for `Opaque`, which promises
    /// nothing and delivers exactly that.
    ///
    /// Returning the text (rather than logging inline) is what makes the warning
    /// a testable VALUE — see `an_inert_mode_warns_the_user_at_load`.
    pub fn inert_warning(self) -> Option<String> {
        if !self.is_inert_alias() {
            return None;
        }
        Some(format!(
            "[window] mode = \"{}\" has NO effect: SCR1B3 has no OS blur / acrylic / \
             mica surface, and `mode` is not read when deciding whether to render \
             translucency. Translucency is controlled solely by \
             `[window] transparency_enabled` (with `opacity` / `tint` / \
             `tint_strength`); set `transparency_enabled = true` to get a \
             translucent window. The value is kept for back-compat only.",
            self.as_config_str()
        ))
    }
}

/// Hand-written so a vestigial `mode` cannot be accepted SILENTLY.
///
/// The derived impl parsed `glass`/`mica`/`vibrancy` and dropped them into a
/// field nothing reads, which is indistinguishable — from the user's side — from
/// the app ignoring their config. This impl keeps the identical accept/reject
/// behaviour (same spellings, still an `unknown_variant` error for anything
/// else) and adds the one thing that was missing: it says so.
impl<'de> Deserialize<'de> for WindowMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        let mode = WindowMode::from_config_str(&raw)
            .ok_or_else(|| serde::de::Error::unknown_variant(&raw, WindowMode::CONFIG_SPELLINGS))?;
        if let Some(msg) = mode.inert_warning() {
            tracing::warn!(target: "scribe::config", "{msg}");
        }
        Ok(mode)
    }
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
    /// only for config back-compat and no longer selects a surface — a user who
    /// writes one is warned at load time via [`WindowMode::inert_warning`]. This
    /// is the single predicate every render path consults.
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
    use crate::test_log_capture::with_captured_logs;

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

    // ---- the vestigial `mode` is accepted, but never SILENTLY ----------------
    //
    // `no_window_mode_independently_drives_translucency` above pins that `mode`
    // does nothing. That is correct behaviour but a bad user experience on its
    // own: a hand-edited `mode = "glass"` used to parse, persist, and change
    // absolutely nothing with no signal anywhere. These tests pin the signal.

    /// Every non-`Opaque` value is an inert alias and carries a warning that
    /// names it AND names the knob that actually works.
    #[test]
    fn every_inert_mode_yields_an_actionable_warning() {
        for mode in [
            WindowMode::Transparent,
            WindowMode::Glass,
            WindowMode::Mica,
            WindowMode::Vibrancy,
        ] {
            assert!(
                mode.is_inert_alias(),
                "{mode:?} does nothing, so it is inert"
            );
            let msg = mode
                .inert_warning()
                .unwrap_or_else(|| panic!("{mode:?} is inert so it must warn"));
            assert!(
                msg.contains(mode.as_config_str()),
                "the warning must name the value the user wrote: {msg}"
            );
            assert!(
                msg.contains("transparency_enabled"),
                "the warning must name the knob that DOES work: {msg}"
            );
        }
        // `opaque` promises nothing and delivers exactly that — no warning.
        assert!(!WindowMode::Opaque.is_inert_alias());
        assert!(WindowMode::Opaque.inert_warning().is_none());
    }

    /// The end-to-end guarantee: deserializing a hand-edited `mode` that cannot
    /// take effect EMITS the warning. Without the hand-written `Deserialize`
    /// impl this capture is empty and the config lies silently.
    #[test]
    fn an_inert_mode_warns_the_user_at_load() {
        let logs_have = |toml: &str, needle: &str| {
            with_captured_logs(|logs| {
                let cfg: Config = toml::from_str(toml).expect("config parses");
                // The value is KEPT, not rejected or normalised away.
                assert_eq!(cfg.window.mode, WindowMode::Glass);
                logs.has(tracing::Level::WARN, needle)
            })
        };
        assert!(
            logs_have("[window]\nmode = \"glass\"\n", "has NO effect"),
            "a hand-edited inert `mode` must warn at WARN level"
        );
        assert!(
            logs_have("[window]\nmode = \"glass\"\n", "transparency_enabled"),
            "the emitted warning must point at the knob that works"
        );
    }

    /// The counter-case: the default/`opaque` load path stays quiet, so the
    /// warning is a real signal rather than noise on every start-up.
    #[test]
    fn an_opaque_mode_load_is_quiet() {
        with_captured_logs(|logs| {
            let cfg: Config = toml::from_str("[window]\nmode = \"opaque\"\n").expect("parses");
            assert_eq!(cfg.window.mode, WindowMode::Opaque);
            assert!(
                !logs.warn_plus_text().contains("has NO effect"),
                "`opaque` is not inert and must not warn: {}",
                logs.warn_plus_text()
            );
        });
    }

    /// Back-compat is intact: every legacy spelling still parses (rejecting one
    /// would make `load_or_default` back up the file and start from defaults),
    /// and an unknown spelling is still an error, exactly as before.
    #[test]
    fn every_legacy_spelling_still_deserializes_and_round_trips() {
        for spelling in WindowMode::CONFIG_SPELLINGS {
            let cfg: Config = toml::from_str(&format!("[window]\nmode = \"{spelling}\"\n"))
                .unwrap_or_else(|e| panic!("legacy spelling `{spelling}` must still parse: {e}"));
            assert_eq!(cfg.window.mode.as_config_str(), *spelling);
            // And it survives a save/load cycle unchanged — the warning explains
            // the value, it does not quietly rewrite the user's file.
            let back: Config =
                toml::from_str(&cfg.to_toml_string()).expect("config TOML round-trip");
            assert_eq!(back.window.mode, cfg.window.mode);
        }
        assert!(
            toml::from_str::<Config>("[window]\nmode = \"acrylic\"\n").is_err(),
            "an unrecognised mode must still be a parse error"
        );
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
