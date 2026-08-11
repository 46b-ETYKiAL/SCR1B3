//! Note-template placeholder substitution.
//!
//! A note template is CONTENT, not code. Substitution is therefore a closed,
//! total, declarative set of `{{placeholder}}` names — deliberately NOT a
//! scripting pass.
//!
//! # Why not `rhai`?
//!
//! The crate already embeds a sandboxed `rhai` for the plugin "easy mode", so
//! wiring it here would be cheap in lines and expensive in everything else:
//!
//! * **Trust class.** Plugins are governed — discovered from a plugins dir,
//!   manifest-declared, minisign-verified against pinned keys, and
//!   enable/disable-able by the user. A template is a `.md` file the user (or a
//!   synced vault, or a downloaded starter pack) drops in a folder. Running it
//!   through a script engine would give ordinary content the reach of a plugin
//!   with none of the plugin subsystem's signing or consent gates.
//! * **Totality.** Every name below is resolvable for every well-formed
//!   timestamp, so rendering cannot fail at the moment the user asks for a new
//!   note. A script can fail, and its failure mode is a broken note.
//! * **Scope honesty.** Doing `rhai` templating *properly* needs template
//!   discovery, an eval budget, error surfacing, and a governance story. A
//!   half-wired script hook is worse than a complete declarative one.
//!
//! Unknown placeholders are left **verbatim**, braces and all. A silently
//! blanked `{{dat}}` typo is indistinguishable from a working placeholder;
//! leaving it visible makes the mistake self-reporting.

/// The values a template may interpolate. Built once per render so every
/// placeholder in one note sees the same instant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateContext {
    /// `YYYY-MM-DD` (UTC).
    pub date: String,
    /// `HH:MM` (UTC).
    pub time: String,
    /// The full `YYYY-MM-DDTHH:MM:SSZ` stamp (UTC).
    pub datetime: String,
    /// `YYYY`.
    pub year: String,
    /// `MM`, zero-padded.
    pub month: String,
    /// `DD`, zero-padded.
    pub day: String,
    /// The note's title (its file stem, or a caller-chosen heading).
    pub title: String,
}

/// Every placeholder name this module substitutes. Anything not in this list is
/// left verbatim in the rendered output.
pub const PLACEHOLDERS: &[&str] = &["date", "time", "datetime", "year", "month", "day", "title"];

impl TemplateContext {
    /// Build a context from an ISO-8601 UTC stamp of the exact shape
    /// `YYYY-MM-DDTHH:MM:SSZ` (what the editor's own clock formatter emits).
    ///
    /// Returns `None` for any other shape rather than slicing blindly — a
    /// mis-shaped stamp would otherwise panic on a non-char-boundary slice or,
    /// worse, silently produce a wrong date. The caller degrades to an
    /// unsubstituted template, which is visible, instead of a wrong one.
    #[must_use]
    pub fn from_iso8601_utc(stamp: &str, title: &str) -> Option<Self> {
        let b = stamp.as_bytes();
        if b.len() != 20 {
            return None;
        }
        // Fixed layout: 0123-56-89T12:45:78Z
        const DIGITS: [usize; 14] = [0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15, 17, 18];
        if !DIGITS.iter().all(|&i| b[i].is_ascii_digit()) {
            return None;
        }
        if b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':' {
            return None;
        }
        if b[19] != b'Z' {
            return None;
        }
        Some(Self {
            date: stamp[0..10].to_string(),
            time: stamp[11..16].to_string(),
            datetime: stamp.to_string(),
            year: stamp[0..4].to_string(),
            month: stamp[5..7].to_string(),
            day: stamp[8..10].to_string(),
            title: title.to_string(),
        })
    }

    /// The substitution for `name`, or `None` when the name is not one of
    /// [`PLACEHOLDERS`].
    #[must_use]
    pub fn lookup(&self, name: &str) -> Option<&str> {
        match name {
            "date" => Some(&self.date),
            "time" => Some(&self.time),
            "datetime" => Some(&self.datetime),
            "year" => Some(&self.year),
            "month" => Some(&self.month),
            "day" => Some(&self.day),
            "title" => Some(&self.title),
            _ => None,
        }
    }
}

/// Render `template`, replacing every `{{name}}` whose `name` is a known
/// placeholder. Whitespace inside the braces is ignored (`{{ date }}` works).
/// An unknown name, or an unterminated `{{`, is copied through unchanged.
#[must_use]
pub fn render(template: &str, ctx: &TemplateContext) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find("{{") {
        out.push_str(&rest[..open]);
        let after = &rest[open + 2..];
        let Some(close) = after.find("}}") else {
            // Unterminated — the remainder is literal text.
            out.push_str(&rest[open..]);
            return out;
        };
        let name = after[..close].trim();
        match ctx.lookup(name) {
            Some(v) => out.push_str(v),
            // Verbatim, braces included: an unknown name must stay visible.
            None => out.push_str(&rest[open..open + 2 + close + 2]),
        }
        rest = &after[close + 2..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAMP: &str = "2026-08-04T09:07:05Z";

    fn ctx() -> TemplateContext {
        TemplateContext::from_iso8601_utc(STAMP, "Notes").expect("well-formed stamp parses")
    }

    #[test]
    fn parses_each_field_out_of_the_stamp() {
        let c = ctx();
        assert_eq!(c.date, "2026-08-04");
        assert_eq!(c.time, "09:07");
        assert_eq!(c.datetime, STAMP);
        assert_eq!(c.year, "2026");
        assert_eq!(c.month, "08");
        assert_eq!(c.day, "04");
        assert_eq!(c.title, "Notes");
    }

    #[test]
    fn every_placeholder_renders_its_own_distinct_value() {
        // A `_ =>` arm that echoed the input, or two arms returning the same
        // field, would pass a per-name "not the raw braces" check. Assert the
        // exact value AND that all seven differ, so no arm can stand in for
        // another.
        let c = ctx();
        let rendered: Vec<String> = PLACEHOLDERS
            .iter()
            .map(|p| render(&format!("{{{{{p}}}}}"), &c))
            .collect();
        assert_eq!(
            rendered,
            vec![
                "2026-08-04",
                "09:07",
                "2026-08-04T09:07:05Z",
                "2026",
                "08",
                "04",
                "Notes",
            ]
        );
        let mut uniq = rendered.clone();
        uniq.sort();
        uniq.dedup();
        assert_eq!(uniq.len(), rendered.len(), "no two placeholders collide");
    }

    #[test]
    fn unknown_placeholder_is_left_verbatim() {
        // NOT blanked: a typo must stay visible in the note.
        assert_eq!(render("{{dat}}", &ctx()), "{{dat}}");
        assert_eq!(render("x {{nope}} y", &ctx()), "x {{nope}} y");
    }

    #[test]
    fn substitutes_inside_surrounding_prose() {
        assert_eq!(
            render("# {{date}}\n\n- logged at {{time}}\n", &ctx()),
            "# 2026-08-04\n\n- logged at 09:07\n"
        );
    }

    #[test]
    fn whitespace_inside_braces_is_tolerated() {
        assert_eq!(render("{{ date }}", &ctx()), "2026-08-04");
    }

    #[test]
    fn unterminated_and_stray_braces_are_literal() {
        assert_eq!(render("{{date", &ctx()), "{{date");
        assert_eq!(render("a { b } c", &ctx()), "a { b } c");
        assert_eq!(render("}}{{", &ctx()), "}}{{");
    }

    #[test]
    fn template_without_placeholders_is_unchanged() {
        let t = "# Checklist\n\n- [ ] \n";
        assert_eq!(render(t, &ctx()), t);
    }

    #[test]
    fn adjacent_placeholders_both_render() {
        assert_eq!(render("{{year}}{{month}}{{day}}", &ctx()), "20260804");
    }

    #[test]
    fn malformed_stamps_are_rejected_rather_than_sliced() {
        for bad in [
            "",
            "2026-08-04",
            "2026-08-04T09:07:05",    // no Z
            "2026-08-04 09:07:05Z",   // space, not T
            "2026/08/04T09:07:05Z",   // wrong separators
            "20X6-08-04T09:07:05Z",   // non-digit
            "2026-08-04T09:07:05Zz",  // too long
            "日本語-08-04T09:07:05Z", // multi-byte, would panic a blind slice
        ] {
            assert!(
                TemplateContext::from_iso8601_utc(bad, "t").is_none(),
                "{bad:?} must be rejected"
            );
        }
    }

    #[test]
    fn lookup_rejects_names_outside_the_closed_set() {
        let c = ctx();
        for p in PLACEHOLDERS {
            assert!(c.lookup(p).is_some(), "{p} must resolve");
        }
        for other in ["", "Date", "now", "author", "date ", "titles"] {
            assert!(c.lookup(other).is_none(), "{other:?} must not resolve");
        }
    }

    /// Every separator position in the fixed ISO-8601 layout is checked by one
    /// `||` chain, so a single corrupted separator must be enough to reject the
    /// stamp. Testing only a well-formed stamp (and only a wholesale-garbage
    /// one) leaves the chain free to be rewritten as `&&`, which would demand
    /// that ALL separators be wrong before rejecting — i.e. accept a stamp with
    /// one bad separator and slice fields out of the wrong offsets.
    #[test]
    fn each_separator_position_alone_rejects_the_stamp() {
        const GOOD: &str = "2026-08-10T12:34:56Z";
        assert!(
            TemplateContext::from_iso8601_utc(GOOD, "t").is_some(),
            "the control stamp must parse, or this test proves nothing"
        );

        // index -> the separator that belongs there
        for (idx, sep) in [(4usize, '-'), (7, '-'), (10, 'T'), (13, ':'), (16, ':')] {
            let mut bad: Vec<char> = GOOD.chars().collect();
            assert_eq!(bad[idx], sep, "layout drifted at index {idx}");
            // Swap in a character that is neither the right separator nor a
            // digit, so only the separator check can reject it.
            bad[idx] = '/';
            let bad: String = bad.into_iter().collect();
            assert!(
                TemplateContext::from_iso8601_utc(&bad, "t").is_none(),
                "a wrong separator at index {idx} must reject: {bad}"
            );
        }
    }
}
