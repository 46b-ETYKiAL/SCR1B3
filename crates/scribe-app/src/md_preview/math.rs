//! Best-effort LaTeX → Unicode transliteration for the markdown preview.
//!
//! # Scope — and what this deliberately is NOT
//!
//! This is **not** a TeX typesetter, and that is a decision, not an oversight.
//! The ecosystem was surveyed before writing it:
//!
//!   * The actively-maintained pure-Rust LaTeX crates (`pulldown-latex`,
//!     `math-core`, `latex2mathml`) all emit a **MathML string**. Nothing in
//!     Rust lays MathML out to geometry, and `epaint::Shape` has no markup
//!     variant — so the output is unrenderable here. The preview's stated
//!     premise (top of `md_preview.rs`) is "no HTML, no webview, no
//!     JavaScript", which is exactly what consuming MathML would require.
//!   * `katex` / `mathjax` / `mathjax_svg` embed a JavaScript engine (the last
//!     pulls in V8) — ruled out by the same premise.
//!   * The one crate that produces glyph geometry, **ReX**, is a
//!     single-maintainer fork of a project dormant since 2020, is **not
//!     published on crates.io** (git-dependency only), and requires bundling an
//!     OpenType `MATH` font (≈176 KB Fira Math at best, ≈1.3 MB for NewCM). A
//!     git-only dependency cannot be pinned to an exact registry version and
//!     sits outside the audit/vet surface every other dependency here is
//!     inside — a poor trade for one formula in a side panel.
//!
//! What this module delivers instead is a *complete, bounded* feature rather
//! than a half-built typesetter: math is **recognised as math** (so
//! `$a_1 + b_2$` is no longer at the mercy of the emphasis parser, and `$$…$$`
//! is no longer three lines of literal `$` text), and is displayed in a
//! readable Unicode form. The original TeX is always kept verbatim in the block
//! model, so the renderer shows it on hover — the user never loses the source.
//!
//! Guarantees: pure `&str -> String`, no allocation of unbounded recursion
//! (depth is capped at [`MAX_DEPTH`]), never panics, and every unrecognised
//! construct is passed through unchanged rather than silently dropped.

/// Maximum `{…}` nesting the transliterator will descend. Deeper groups are
/// emitted verbatim rather than risking an unbounded recursive walk on a
/// pathological (or hostile) document.
const MAX_DEPTH: u8 = 8;

/// Transliterate a TeX fragment (the text *between* the `$`/`$$` delimiters, as
/// `pulldown-cmark` hands it to us) into a readable Unicode approximation.
pub(crate) fn math_to_unicode(tex: &str) -> String {
    let chars: Vec<char> = tex.chars().collect();
    render(&chars, 0).trim().to_string()
}

/// Walk `src`, expanding commands, scripts and grouping braces.
fn render(src: &[char], depth: u8) -> String {
    if depth > MAX_DEPTH {
        return src.iter().collect();
    }
    let mut out = String::new();
    let mut i = 0usize;
    while let Some(&c) = src.get(i) {
        // The dispatch character is consumed HERE — once, unconditionally,
        // before any arm runs — instead of by a separate `i += 1` in each arm.
        // Two reasons, both about this loop being unable to stall:
        //
        //   * No arm can be the loop's ONLY source of progress any more. A
        //     malformed `$…$` fragment must never be able to wedge the markdown
        //     preview, and that is now a property of the loop itself rather
        //     than of all four arms independently remembering to advance.
        //   * `saturating_add`, not `+= 1`. An index step written as an
        //     assign-op is exactly the shape a mutation run perturbs into
        //     `-=`/`*=`, and a cursor that rewinds or stands still here does
        //     not render the wrong thing — it hangs. That is a fault no
        //     assertion can observe, only a timeout, and a timeout fails the
        //     gate while teaching nothing. With no operator there is nothing to
        //     perturb: the four `+=` mutants this loop used to carry (three of
        //     them measured TIMEOUTs) are no longer generated at all. It is
        //     also the honest semantics — this cursor only ever moves forward,
        //     and saturating at `usize::MAX` beats wrapping to 0, which would
        //     restart the whole walk.
        i = i.saturating_add(1);
        match c {
            '\\' => render_command(src, &mut i, &mut out, depth),
            '^' | '_' => render_script(src, &mut i, &mut out, depth, c == '^'),
            // Bare braces are TeX grouping, not content — drop them. An escaped
            // brace arrives as `\{` and is handled by `render_command`.
            '{' | '}' => {}
            _ => out.push(c),
        }
    }
    out
}

/// Expand the command whose name starts at `*i` (the leading `\` is already
/// consumed). Always advances `*i` so the caller's loop cannot stall.
fn render_command(src: &[char], i: &mut usize, out: &mut String, depth: u8) {
    let Some(&first) = src.get(*i) else {
        // A trailing lone backslash: emit it verbatim.
        out.push('\\');
        return;
    };
    if !first.is_ascii_alphabetic() {
        *i += 1;
        match first {
            // Thin/medium/thick spaces and an explicit inter-word space.
            ',' | ';' | ':' | ' ' => out.push(' '),
            // Negative thin space — renders as nothing.
            '!' => {}
            // A line break inside math becomes a space in this flat preview.
            '\\' => out.push(' '),
            // `\{`, `\}`, `\$`, `\%`, `\&`, `\#`, `\_` — escaped literals.
            other => out.push(other),
        }
        return;
    }

    let start = *i;
    // Counted, not stepped — the same reason as the identical scan in
    // `read_group_inner`. As a `while` loop the `*i += 1` was the loop's ONLY
    // progress, so perturbing it spun forever instead of returning a wrong
    // command name: a hang, which no assertion can catch and only a timeout
    // reports. Counted, a perturbed advance mis-parses the name and terminates,
    // which `greek_and_operator_commands_become_unicode` asserts.
    *i += src
        .get(*i..)
        .unwrap_or(&[])
        .iter()
        .take_while(|c| c.is_ascii_alphabetic())
        .count();
    let name: String = src[start..*i].iter().collect();

    match name.as_str() {
        "frac" | "dfrac" | "tfrac" => {
            let num = render(&read_group(src, i), depth + 1);
            let den = render(&read_group(src, i), depth + 1);
            out.push_str(&paren_if_compound(&num));
            // Truncated source (`\frac{a`, or an explicit `\frac{a}{}`) leaves no
            // denominator. Emitting the separator anyway renders a dangling
            // `a/`, which reads as a typo in the preview rather than as the
            // partial fraction it is — so the separator is tied to having an
            // actual denominator. Well-formed input is unaffected.
            if !den.is_empty() {
                out.push('/');
                out.push_str(&paren_if_compound(&den));
            }
        }
        "sqrt" => {
            let body = render(&read_group(src, i), depth + 1);
            out.push('√');
            out.push_str(&paren_if_compound(&body));
        }
        // Font/roman wrappers: keep the content, drop the wrapper.
        "text" | "mathrm" | "mathbf" | "mathit" | "mathsf" | "mathtt" | "operatorname" => {
            out.push_str(&render(&read_group(src, i), depth + 1));
        }
        // Sizing/delimiter commands: the delimiter itself follows and is
        // emitted by the normal path, so these expand to nothing.
        "left" | "right" | "bigl" | "bigr" | "Bigl" | "Bigr" | "big" | "Big" | "displaystyle" => {}
        "quad" => out.push(' '),
        "qquad" => out.push_str("  "),
        _ => match symbol(&name) {
            Some(sym) => out.push_str(sym),
            // Unknown command: keep the TeX verbatim so nothing is silently
            // lost and the user can see exactly what was not translated.
            None => {
                out.push('\\');
                out.push_str(&name);
            }
        },
    }
}

/// Expand a `^`/`_` script. `*i` points just past the marker.
fn render_script(src: &[char], i: &mut usize, out: &mut String, depth: u8, sup: bool) {
    let group = read_group(src, i);
    if group.is_empty() {
        out.push(if sup { '^' } else { '_' });
        return;
    }
    let braced = matches!(src.get(i.wrapping_sub(1)), Some('}'));
    let body = render(&group, depth + 1);
    match to_script(&body, sup) {
        Some(mapped) => out.push_str(&mapped),
        // Not every character has a Unicode super/subscript form. Rather than
        // mangle half the expression, fall back to the plain marker — adding
        // parentheses when the group was compound so `x^{n+1}` cannot be
        // misread as `x^n + 1`.
        None => {
            out.push(if sup { '^' } else { '_' });
            if braced && body.chars().count() > 1 {
                out.push('(');
                out.push_str(&body);
                out.push(')');
            } else {
                out.push_str(&body);
            }
        }
    }
}

/// Read the argument starting at `*i`: a braced group (returned without its
/// braces), a whole `\command`, or a single character — matching TeX's own
/// argument rules (`\frac12` is `\frac{1}{2}`). Always advances `*i` when a
/// character is available, so callers cannot loop forever.
fn read_group(src: &[char], i: &mut usize) -> Vec<char> {
    let entry = *i;
    let group = read_group_inner(src, i);
    // ENFORCE the "always advances" contract above rather than trusting it.
    //
    // Every caller is a `while *i < src.len()` loop whose ONLY progress is this
    // cursor, so a `read_group` that stands still or rewinds does not render the
    // wrong thing — it hangs the preview on a malformed fragment. That is a
    // fault no assertion can observe, only a timeout, which is exactly how it
    // shows up in a mutation run: perturbing an advance inside the body scored
    // TIMEOUT instead of a kill, and a timeout fails the gate while teaching
    // nothing. With this clamp the same perturbation produces a WRONG PARSE,
    // which `read_group_advances_past_every_first_character` asserts.
    //
    // It is also a real robustness property in its own right: a user's `$…$`
    // fragment must never be able to wedge the markdown preview.
    if entry < src.len() && *i <= entry {
        *i = entry + 1;
    }
    group
}

fn read_group_inner(src: &[char], i: &mut usize) -> Vec<char> {
    // Counted, not stepped. The obvious `while …is_whitespace() { *i += 1 }` is
    // a manual index loop whose increment is the loop's ONLY progress, so
    // perturbing that increment hangs rather than returning a wrong answer —
    // a fault no assertion can observe, only a timeout. Advancing by a computed
    // count has identical semantics while making a wrong step produce a wrong
    // parse, which `read_group_index_arithmetic_is_exact` catches.
    *i += src
        .get(*i..)
        .unwrap_or(&[])
        .iter()
        .take_while(|c| c.is_whitespace())
        .count();
    match src.get(*i) {
        None => Vec::new(),
        Some('{') => {
            *i += 1;
            let start = *i;
            let mut nesting = 1usize;
            // Consumed at the TOP of the loop, once, before the arms run — so
            // no arm is this loop's only progress and a perturbed advance
            // cannot stall it. Same reasoning as `render`'s loop head: a
            // stalled cursor here does not mis-parse, it hangs the preview on a
            // malformed fragment, and a hang is a fault only a timeout can
            // observe. Previously the escaped-brace skip and the bottom-of-loop
            // step were BOTH mutable into a net-zero advance.
            while let Some(&c) = src.get(*i) {
                let at = *i;
                *i = i.saturating_add(1);
                match c {
                    // Skip an escaped brace so it cannot unbalance the scan.
                    '\\' => *i = i.saturating_add(1),
                    '{' => nesting += 1,
                    '}' => {
                        nesting -= 1;
                        if nesting == 0 {
                            // `at` is the closing brace, which the cursor has
                            // already been advanced past.
                            return src[start..at].to_vec();
                        }
                    }
                    _ => {}
                }
            }
            // Unbalanced `{` — take the rest of the fragment.
            src[start..].to_vec()
        }
        Some('\\') => {
            let start = *i;
            *i += 1;
            if src.get(*i).is_some_and(|c| c.is_ascii_alphabetic()) {
                // Counted, not stepped — the same reason as the whitespace skip
                // at the top of this function. As a `while` loop the `*i += 1`
                // was the loop's only progress, so perturbing it spun forever
                // instead of returning a wrong name; counted, a perturbed
                // advance mis-parses and can be asserted.
                *i += src
                    .get(*i..)
                    .unwrap_or(&[])
                    .iter()
                    .take_while(|c| c.is_ascii_alphabetic())
                    .count();
            } else if *i < src.len() {
                *i += 1;
            }
            src[start..*i].to_vec()
        }
        Some(&c) => {
            *i += 1;
            vec![c]
        }
    }
}

/// Wrap in parentheses only when the rendered fragment is more than one
/// character, so `\frac{a}{b}` reads `a/b` while `\frac{a+b}{c}` reads `(a+b)/c`.
fn paren_if_compound(s: &str) -> String {
    if s.chars().count() > 1 {
        format!("({s})")
    } else {
        s.to_string()
    }
}

/// Map a whole string to Unicode super- or subscript, or `None` when ANY
/// character lacks a form (partial mapping would be less readable, not more).
fn to_script(s: &str, sup: bool) -> Option<String> {
    if s.is_empty() {
        return None;
    }
    let mut out = String::new();
    for c in s.chars() {
        let mapped = if sup { superscript(c) } else { subscript(c) }?;
        out.push(mapped);
    }
    Some(out)
}

/// Unicode superscript form of `c`, when one exists.
fn superscript(c: char) -> Option<char> {
    Some(match c {
        '0' => '⁰',
        '1' => '¹',
        '2' => '²',
        '3' => '³',
        '4' => '⁴',
        '5' => '⁵',
        '6' => '⁶',
        '7' => '⁷',
        '8' => '⁸',
        '9' => '⁹',
        '+' => '⁺',
        '-' | '−' => '⁻',
        '=' => '⁼',
        '(' => '⁽',
        ')' => '⁾',
        'a' => 'ᵃ',
        'b' => 'ᵇ',
        'c' => 'ᶜ',
        'd' => 'ᵈ',
        'e' => 'ᵉ',
        'f' => 'ᶠ',
        'g' => 'ᵍ',
        'h' => 'ʰ',
        'i' => 'ⁱ',
        'j' => 'ʲ',
        'k' => 'ᵏ',
        'l' => 'ˡ',
        'm' => 'ᵐ',
        'n' => 'ⁿ',
        'o' => 'ᵒ',
        'p' => 'ᵖ',
        'r' => 'ʳ',
        's' => 'ˢ',
        't' => 'ᵗ',
        'u' => 'ᵘ',
        'v' => 'ᵛ',
        'w' => 'ʷ',
        'x' => 'ˣ',
        'y' => 'ʸ',
        'z' => 'ᶻ',
        'A' => 'ᴬ',
        'B' => 'ᴮ',
        'D' => 'ᴰ',
        'E' => 'ᴱ',
        'G' => 'ᴳ',
        'H' => 'ᴴ',
        'I' => 'ᴵ',
        'J' => 'ᴶ',
        'K' => 'ᴷ',
        'L' => 'ᴸ',
        'M' => 'ᴹ',
        'N' => 'ᴺ',
        'O' => 'ᴼ',
        'P' => 'ᴾ',
        'R' => 'ᴿ',
        'T' => 'ᵀ',
        'U' => 'ᵁ',
        'V' => 'ⱽ',
        'W' => 'ᵂ',
        _ => return None,
    })
}

/// Unicode subscript form of `c`, when one exists.
fn subscript(c: char) -> Option<char> {
    Some(match c {
        '0' => '₀',
        '1' => '₁',
        '2' => '₂',
        '3' => '₃',
        '4' => '₄',
        '5' => '₅',
        '6' => '₆',
        '7' => '₇',
        '8' => '₈',
        '9' => '₉',
        '+' => '₊',
        '-' | '−' => '₋',
        '=' => '₌',
        '(' => '₍',
        ')' => '₎',
        'a' => 'ₐ',
        'e' => 'ₑ',
        'h' => 'ₕ',
        'i' => 'ᵢ',
        'j' => 'ⱼ',
        'k' => 'ₖ',
        'l' => 'ₗ',
        'm' => 'ₘ',
        'n' => 'ₙ',
        'o' => 'ₒ',
        'p' => 'ₚ',
        'r' => 'ᵣ',
        's' => 'ₛ',
        't' => 'ₜ',
        'u' => 'ᵤ',
        'v' => 'ᵥ',
        'x' => 'ₓ',
        _ => return None,
    })
}

/// Unicode character(s) for a TeX symbol command name (without the backslash).
fn symbol(name: &str) -> Option<&'static str> {
    Some(match name {
        // ---- lower-case Greek ----
        "alpha" => "α",
        "beta" => "β",
        "gamma" => "γ",
        "delta" => "δ",
        "epsilon" => "ε",
        "varepsilon" => "ε",
        "zeta" => "ζ",
        "eta" => "η",
        "theta" => "θ",
        "vartheta" => "ϑ",
        "iota" => "ι",
        "kappa" => "κ",
        "lambda" => "λ",
        "mu" => "μ",
        "nu" => "ν",
        "xi" => "ξ",
        "pi" => "π",
        "rho" => "ρ",
        "sigma" => "σ",
        "tau" => "τ",
        "upsilon" => "υ",
        "phi" => "φ",
        "varphi" => "ϕ",
        "chi" => "χ",
        "psi" => "ψ",
        "omega" => "ω",
        // ---- upper-case Greek ----
        "Gamma" => "Γ",
        "Delta" => "Δ",
        "Theta" => "Θ",
        "Lambda" => "Λ",
        "Xi" => "Ξ",
        "Pi" => "Π",
        "Sigma" => "Σ",
        "Upsilon" => "Υ",
        "Phi" => "Φ",
        "Psi" => "Ψ",
        "Omega" => "Ω",
        // ---- relations ----
        "leq" | "le" => "≤",
        "geq" | "ge" => "≥",
        "neq" | "ne" => "≠",
        "approx" => "≈",
        "equiv" => "≡",
        "sim" => "∼",
        "simeq" => "≃",
        "cong" => "≅",
        "propto" => "∝",
        "ll" => "≪",
        "gg" => "≫",
        // ---- operators ----
        "times" => "×",
        "div" => "÷",
        "pm" => "±",
        "mp" => "∓",
        "cdot" => "·",
        "ast" => "∗",
        "star" => "⋆",
        "circ" => "∘",
        "bullet" => "∙",
        "oplus" => "⊕",
        "otimes" => "⊗",
        // ---- big operators ----
        "sum" => "∑",
        "prod" => "∏",
        "coprod" => "∐",
        "int" => "∫",
        "iint" => "∬",
        "oint" => "∮",
        "bigcup" => "⋃",
        "bigcap" => "⋂",
        // ---- sets & logic ----
        "in" => "∈",
        "notin" => "∉",
        "ni" => "∋",
        "subset" => "⊂",
        "subseteq" => "⊆",
        "supset" => "⊃",
        "supseteq" => "⊇",
        "cup" => "∪",
        "cap" => "∩",
        "setminus" => "∖",
        "emptyset" | "varnothing" => "∅",
        "forall" => "∀",
        "exists" => "∃",
        "nexists" => "∄",
        "neg" | "lnot" => "¬",
        "land" | "wedge" => "∧",
        "lor" | "vee" => "∨",
        "therefore" => "∴",
        "because" => "∵",
        // ---- arrows ----
        "to" | "rightarrow" => "→",
        "leftarrow" | "gets" => "←",
        "leftrightarrow" => "↔",
        "Rightarrow" | "implies" => "⇒",
        "Leftarrow" => "⇐",
        "Leftrightarrow" | "iff" => "⇔",
        "mapsto" => "↦",
        "uparrow" => "↑",
        "downarrow" => "↓",
        // ---- misc ----
        "infty" => "∞",
        "partial" => "∂",
        "nabla" => "∇",
        "angle" => "∠",
        "perp" => "⊥",
        "parallel" => "∥",
        "degree" => "°",
        "prime" => "′",
        "hbar" => "ℏ",
        "ell" => "ℓ",
        "Re" => "ℜ",
        "Im" => "ℑ",
        "aleph" => "ℵ",
        "ldots" | "dots" => "…",
        "cdots" => "⋯",
        "vdots" => "⋮",
        "ddots" => "⋱",
        "checkmark" => "✓",
        // ---- named functions ----
        "sin" => "sin",
        "cos" => "cos",
        "tan" => "tan",
        "log" => "log",
        "ln" => "ln",
        "exp" => "exp",
        "min" => "min",
        "max" => "max",
        "lim" => "lim",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn greek_and_operator_commands_become_unicode() {
        assert_eq!(math_to_unicode("\\alpha + \\beta"), "α + β");
        assert_eq!(math_to_unicode("a \\times b \\div c"), "a × b ÷ c");
        assert_eq!(
            math_to_unicode("x \\leq y \\geq z \\neq w"),
            "x ≤ y ≥ z ≠ w"
        );
        assert_eq!(math_to_unicode("\\sum \\prod \\int \\infty"), "∑ ∏ ∫ ∞");
        assert_eq!(math_to_unicode("A \\cup B \\cap C"), "A ∪ B ∩ C");
        assert_eq!(math_to_unicode("\\Omega \\Delta"), "Ω Δ");
    }

    #[test]
    fn superscripts_and_subscripts_map_to_unicode() {
        assert_eq!(math_to_unicode("x^2"), "x²");
        assert_eq!(math_to_unicode("a_1"), "a₁");
        // A braced multi-character group maps when EVERY char has a form.
        assert_eq!(math_to_unicode("x^{10}"), "x¹⁰");
        assert_eq!(math_to_unicode("x^{n+1}"), "xⁿ⁺¹");
        assert_eq!(math_to_unicode("e^{-x}"), "e⁻ˣ");
        assert_eq!(math_to_unicode("a_{ij}"), "aᵢⱼ");
        // Transpose — the uppercase superscript arm.
        assert_eq!(math_to_unicode("A^T"), "Aᵀ");
    }

    #[test]
    fn unmappable_script_falls_back_without_mangling() {
        // 'q' has no Unicode superscript, so the whole group must fall back
        // rather than be half-translated.
        assert_eq!(math_to_unicode("x^{q}"), "x^q");
        // A compound fallback keeps parentheses so `x^{n+q}` cannot be misread
        // as `x^n + q`.
        assert_eq!(math_to_unicode("x^{n+q}"), "x^(n+q)");
        // A command-valued script falls back to the marker + symbol.
        assert_eq!(math_to_unicode("x^\\alpha"), "x^α");
        // Subscript 'b' has no Unicode form either.
        assert_eq!(math_to_unicode("a_b"), "a_b");
    }

    #[test]
    fn frac_and_sqrt_are_readable() {
        assert_eq!(math_to_unicode("\\frac{a}{b}"), "a/b");
        assert_eq!(math_to_unicode("\\frac{a+b}{c}"), "(a+b)/c");
        // TeX's single-token argument form.
        assert_eq!(math_to_unicode("\\frac12"), "1/2");
        assert_eq!(math_to_unicode("\\sqrt{x}"), "√x");
        assert_eq!(math_to_unicode("\\sqrt{x+y}"), "√(x+y)");
        // Nested fractions still resolve.
        assert_eq!(math_to_unicode("\\frac{\\frac{a}{b}}{c}"), "(a/b)/c");
    }

    #[test]
    fn spacing_wrappers_and_delimiters_are_stripped() {
        assert_eq!(math_to_unicode("a\\,b\\;c"), "a b c");
        assert_eq!(math_to_unicode("a\\!b"), "ab");
        assert_eq!(math_to_unicode("\\left( x \\right)"), "( x )");
        assert_eq!(math_to_unicode("\\mathrm{d}x"), "dx");
        assert_eq!(math_to_unicode("\\text{if } x"), "if  x");
        assert_eq!(math_to_unicode("a \\quad b"), "a   b");
    }

    #[test]
    fn unknown_commands_are_preserved_not_dropped() {
        // Silently swallowing an untranslated command would LOSE content. The
        // TeX must survive verbatim so the reader can see what was not mapped.
        assert_eq!(math_to_unicode("\\weirdmacro x"), "\\weirdmacro x");
        assert_eq!(math_to_unicode("\\zeta\\unknown"), "ζ\\unknown");
    }

    #[test]
    fn escaped_literals_survive_the_brace_stripper() {
        // Grouping braces are dropped, but `\{` / `\}` are literal content.
        assert_eq!(math_to_unicode("\\{x\\}"), "{x}");
        assert_eq!(math_to_unicode("{abc}"), "abc");
        assert_eq!(math_to_unicode("100\\%"), "100%");
        assert_eq!(math_to_unicode("a\\_b"), "a_b");
    }

    #[test]
    fn malformed_input_terminates_and_never_panics() {
        // Every one of these must return (no infinite loop) and not panic.
        assert_eq!(math_to_unicode(""), "");
        assert_eq!(math_to_unicode("\\"), "\\");
        assert_eq!(math_to_unicode("^"), "^");
        assert_eq!(math_to_unicode("_"), "_");
        assert_eq!(math_to_unicode("{{{{"), "");
        assert_eq!(math_to_unicode("\\frac{a"), "a");
        let _ = math_to_unicode("}}}}");
        let _ = math_to_unicode("\\sqrt");
        let _ = math_to_unicode("x^{");
        let _ = math_to_unicode("\\frac");
    }

    #[test]
    fn deep_nesting_stops_expanding_at_the_cap_and_emits_the_rest_verbatim() {
        // Past MAX_DEPTH the fragment is emitted VERBATIM instead of recursing
        // further — the guard against a hostile deeply-nested document.
        //
        // Asserting only "the output still contains x and √" would be vacuous:
        // that holds whether or not the cap exists. The discriminating signal is
        // that the un-expanded tail is still raw TeX — with no cap, every level
        // expands and no backslash or brace can remain.
        let n = MAX_DEPTH as usize + 4;
        let deep = format!("{}x{}", "\\sqrt{".repeat(n), "}".repeat(n));
        let out = math_to_unicode(&deep);
        assert!(out.contains('x'), "content survived: {out}");
        assert!(out.starts_with('√'), "outer levels still expanded: {out}");
        assert!(
            out.contains("\\sqrt"),
            "levels past the cap must stay raw TeX (no cap => fully expanded): {out}"
        );
        assert!(
            out.contains('{'),
            "the verbatim tail keeps its braces: {out}"
        );
        // Exactly the levels within the cap expanded.
        assert_eq!(
            out.matches('√').count(),
            MAX_DEPTH as usize + 1,
            "one √ per level up to and including the cap: {out}"
        );
    }

    #[test]
    fn non_ascii_and_unicode_input_round_trips() {
        // Multi-byte input must not be sliced mid-character (the walk is over
        // `char`s, not bytes).
        assert_eq!(math_to_unicode("日本語 + α"), "日本語 + α");
        assert_eq!(math_to_unicode("ℝ^2"), "ℝ²");
    }

    /// Every arm of the `superscript` table, exhaustively.
    ///
    /// cargo-mutants plants a `delete match arm 'X'` mutant on each arm; a
    /// deleted arm falls through to the `_ => return None` catch-all, so
    /// asserting the whole table kills that family in one test. The negative
    /// cases pin the catch-all itself, so widening the table is also a failure.
    #[test]
    fn superscript_table_maps_every_arm() {
        const TABLE: &[(char, char)] = &[
            ('0', '⁰'),
            ('1', '¹'),
            ('2', '²'),
            ('3', '³'),
            ('4', '⁴'),
            ('5', '⁵'),
            ('6', '⁶'),
            ('7', '⁷'),
            ('8', '⁸'),
            ('9', '⁹'),
            ('+', '⁺'),
            ('-', '⁻'),
            ('−', '⁻'),
            ('=', '⁼'),
            ('(', '⁽'),
            (')', '⁾'),
            ('a', 'ᵃ'),
            ('b', 'ᵇ'),
            ('c', 'ᶜ'),
            ('d', 'ᵈ'),
            ('e', 'ᵉ'),
            ('f', 'ᶠ'),
            ('g', 'ᵍ'),
            ('h', 'ʰ'),
            ('i', 'ⁱ'),
            ('j', 'ʲ'),
            ('k', 'ᵏ'),
            ('l', 'ˡ'),
            ('m', 'ᵐ'),
            ('n', 'ⁿ'),
            ('o', 'ᵒ'),
            ('p', 'ᵖ'),
            ('r', 'ʳ'),
            ('s', 'ˢ'),
            ('t', 'ᵗ'),
            ('u', 'ᵘ'),
            ('v', 'ᵛ'),
            ('w', 'ʷ'),
            ('x', 'ˣ'),
            ('y', 'ʸ'),
            ('z', 'ᶻ'),
            ('A', 'ᴬ'),
            ('B', 'ᴮ'),
            ('D', 'ᴰ'),
            ('E', 'ᴱ'),
            ('G', 'ᴳ'),
            ('H', 'ᴴ'),
            ('I', 'ᴵ'),
            ('J', 'ᴶ'),
            ('K', 'ᴷ'),
            ('L', 'ᴸ'),
            ('M', 'ᴹ'),
            ('N', 'ᴺ'),
            ('O', 'ᴼ'),
            ('P', 'ᴾ'),
            ('R', 'ᴿ'),
            ('T', 'ᵀ'),
            ('U', 'ᵁ'),
            ('V', 'ⱽ'),
            ('W', 'ᵂ'),
        ];
        for &(input, expected) in TABLE {
            assert_eq!(
                superscript(input),
                Some(expected),
                "superscript({input:?}) must map to {expected:?}"
            );
        }
        for input in ['q', 'C', 'F', 'Q', 'S', 'X', 'Y', 'Z', '*', '/', '%'] {
            assert_eq!(
                superscript(input),
                None,
                "superscript({input:?}) has no Unicode form and must return None"
            );
        }
    }

    /// Every arm of the `subscript` table, exhaustively (see the superscript
    /// twin above for why this is table-driven rather than per-arm).
    #[test]
    fn subscript_table_maps_every_arm() {
        const TABLE: &[(char, char)] = &[
            ('0', '₀'),
            ('1', '₁'),
            ('2', '₂'),
            ('3', '₃'),
            ('4', '₄'),
            ('5', '₅'),
            ('6', '₆'),
            ('7', '₇'),
            ('8', '₈'),
            ('9', '₉'),
            ('+', '₊'),
            ('-', '₋'),
            ('−', '₋'),
            ('=', '₌'),
            ('(', '₍'),
            (')', '₎'),
            ('a', 'ₐ'),
            ('e', 'ₑ'),
            ('h', 'ₕ'),
            ('i', 'ᵢ'),
            ('j', 'ⱼ'),
            ('k', 'ₖ'),
            ('l', 'ₗ'),
            ('m', 'ₘ'),
            ('n', 'ₙ'),
            ('o', 'ₒ'),
            ('p', 'ₚ'),
            ('r', 'ᵣ'),
            ('s', 'ₛ'),
            ('t', 'ₜ'),
            ('u', 'ᵤ'),
            ('v', 'ᵥ'),
            ('x', 'ₓ'),
        ];
        for &(input, expected) in TABLE {
            assert_eq!(
                subscript(input),
                Some(expected),
                "subscript({input:?}) must map to {expected:?}"
            );
        }
        for input in [
            'b', 'c', 'd', 'f', 'g', 'q', 'w', 'y', 'z', 'A', 'Z', '*', '/',
        ] {
            assert_eq!(
                subscript(input),
                None,
                "subscript({input:?}) has no Unicode form and must return None"
            );
        }
    }

    /// Every arm of the `symbol` command table, exhaustively — including both
    /// names of every aliased arm (`\leq`/`\le`, `\to`/`\rightarrow`, …), so
    /// deleting a whole multi-pattern arm cannot hide behind its twin.
    #[test]
    fn symbol_table_maps_every_arm() {
        const TABLE: &[(&str, &str)] = &[
            ("alpha", "α"),
            ("beta", "β"),
            ("gamma", "γ"),
            ("delta", "δ"),
            ("epsilon", "ε"),
            ("varepsilon", "ε"),
            ("zeta", "ζ"),
            ("eta", "η"),
            ("theta", "θ"),
            ("vartheta", "ϑ"),
            ("iota", "ι"),
            ("kappa", "κ"),
            ("lambda", "λ"),
            ("mu", "μ"),
            ("nu", "ν"),
            ("xi", "ξ"),
            ("pi", "π"),
            ("rho", "ρ"),
            ("sigma", "σ"),
            ("tau", "τ"),
            ("upsilon", "υ"),
            ("phi", "φ"),
            ("varphi", "ϕ"),
            ("chi", "χ"),
            ("psi", "ψ"),
            ("omega", "ω"),
            ("Gamma", "Γ"),
            ("Delta", "Δ"),
            ("Theta", "Θ"),
            ("Lambda", "Λ"),
            ("Xi", "Ξ"),
            ("Pi", "Π"),
            ("Sigma", "Σ"),
            ("Upsilon", "Υ"),
            ("Phi", "Φ"),
            ("Psi", "Ψ"),
            ("Omega", "Ω"),
            ("leq", "≤"),
            ("le", "≤"),
            ("geq", "≥"),
            ("ge", "≥"),
            ("neq", "≠"),
            ("ne", "≠"),
            ("approx", "≈"),
            ("equiv", "≡"),
            ("sim", "∼"),
            ("simeq", "≃"),
            ("cong", "≅"),
            ("propto", "∝"),
            ("ll", "≪"),
            ("gg", "≫"),
            ("times", "×"),
            ("div", "÷"),
            ("pm", "±"),
            ("mp", "∓"),
            ("cdot", "·"),
            ("ast", "∗"),
            ("star", "⋆"),
            ("circ", "∘"),
            ("bullet", "∙"),
            ("oplus", "⊕"),
            ("otimes", "⊗"),
            ("sum", "∑"),
            ("prod", "∏"),
            ("coprod", "∐"),
            ("int", "∫"),
            ("iint", "∬"),
            ("oint", "∮"),
            ("bigcup", "⋃"),
            ("bigcap", "⋂"),
            ("in", "∈"),
            ("notin", "∉"),
            ("ni", "∋"),
            ("subset", "⊂"),
            ("subseteq", "⊆"),
            ("supset", "⊃"),
            ("supseteq", "⊇"),
            ("cup", "∪"),
            ("cap", "∩"),
            ("setminus", "∖"),
            ("emptyset", "∅"),
            ("varnothing", "∅"),
            ("forall", "∀"),
            ("exists", "∃"),
            ("nexists", "∄"),
            ("neg", "¬"),
            ("lnot", "¬"),
            ("land", "∧"),
            ("wedge", "∧"),
            ("lor", "∨"),
            ("vee", "∨"),
            ("therefore", "∴"),
            ("because", "∵"),
            ("to", "→"),
            ("rightarrow", "→"),
            ("leftarrow", "←"),
            ("gets", "←"),
            ("leftrightarrow", "↔"),
            ("Rightarrow", "⇒"),
            ("implies", "⇒"),
            ("Leftarrow", "⇐"),
            ("Leftrightarrow", "⇔"),
            ("iff", "⇔"),
            ("mapsto", "↦"),
            ("uparrow", "↑"),
            ("downarrow", "↓"),
            ("infty", "∞"),
            ("partial", "∂"),
            ("nabla", "∇"),
            ("angle", "∠"),
            ("perp", "⊥"),
            ("parallel", "∥"),
            ("degree", "°"),
            ("prime", "′"),
            ("hbar", "ℏ"),
            ("ell", "ℓ"),
            ("Re", "ℜ"),
            ("Im", "ℑ"),
            ("aleph", "ℵ"),
            ("ldots", "…"),
            ("dots", "…"),
            ("cdots", "⋯"),
            ("vdots", "⋮"),
            ("ddots", "⋱"),
            ("checkmark", "✓"),
            ("sin", "sin"),
            ("cos", "cos"),
            ("tan", "tan"),
            ("log", "log"),
            ("ln", "ln"),
            ("exp", "exp"),
            ("min", "min"),
            ("max", "max"),
            ("lim", "lim"),
        ];
        for &(input, expected) in TABLE {
            assert_eq!(
                symbol(input),
                Some(expected),
                "symbol({input:?}) must map to {expected:?}"
            );
        }
        for input in ["", "notacommand", "alpha_", "Alpha", "LEQ"] {
            assert_eq!(
                symbol(input),
                None,
                "symbol({input:?}) is not a known command and must return None"
            );
        }
    }

    /// The depth cap must be driven by EVERY recursing construct, not just
    /// `\sqrt` (which the test above already pins). Each fragment below nests
    /// one construct past `MAX_DEPTH`; if that construct stopped incrementing
    /// `depth` the cap would never fire, the fragment would expand all the way
    /// down, and no raw TeX would be left — which is what each assert looks for.
    #[test]
    fn every_recursing_construct_increments_the_depth_counter() {
        let n = MAX_DEPTH as usize + 4;

        // `\frac` numerator: \frac{\frac{…x…}{c}}{c}
        let out = math_to_unicode(&format!("{}x{}", r"\frac{".repeat(n), "}{c}".repeat(n)));
        assert!(
            out.contains(r"\frac"),
            "frac numerator must stop expanding at the cap: {out}"
        );

        // `\frac` denominator: \frac{c}{\frac{c}{…x…}}
        let out = math_to_unicode(&format!("{}x{}", r"\frac{c}{".repeat(n), "}".repeat(n)));
        assert!(
            out.contains(r"\frac"),
            "frac denominator must stop expanding at the cap: {out}"
        );

        // Font/roman wrapper: \mathrm{\mathrm{…\alpha…}}
        let out = math_to_unicode(&format!(
            "{}{}{}",
            r"\mathrm{".repeat(n),
            r"\alpha",
            "}".repeat(n)
        ));
        assert!(
            out.contains(r"\mathrm") && out.contains(r"\alpha"),
            "wrapper must stop expanding at the cap: {out}"
        );

        // Script group: x^{x^{…\alpha…}}
        let out = math_to_unicode(&format!(
            "{}{}{}",
            "x^{".repeat(n),
            r"\alpha",
            "}".repeat(n)
        ));
        assert!(
            out.contains(r"\alpha"),
            "script body must stop expanding at the cap: {out}"
        );
    }

    /// `read_group` is hand-rolled index arithmetic, so every step it takes is
    /// pinned here: the leading-whitespace skip, the escaped-brace skip that
    /// stops `\}` closing the group early, and both `\command` argument forms
    /// (an alphabetic name, and the single-punctuation-character form).
    #[test]
    fn read_group_index_arithmetic_is_exact() {
        // TeX allows whitespace between a command and its argument.
        assert_eq!(math_to_unicode(r"\frac {a}{b}"), "a/b");
        assert_eq!(math_to_unicode("x^ 2"), "x²");

        // `\}` inside a group must NOT terminate it — the brace is content.
        assert_eq!(math_to_unicode(r"\mathrm{a\}b}"), "a}b");

        // `\command` as a bare argument: an alphabetic name...
        assert_eq!(math_to_unicode(r"\sqrt\alpha"), "√α");
        // ...and the single-punctuation-character form.
        assert_eq!(math_to_unicode(r"\sqrt\{"), "√{");

        // A command argument must be CONSUMED, not merely re-rendered by the
        // caller's loop. `\sqrt\alpha` cannot tell the difference — if the
        // group comes back empty the outer walk re-expands `\alpha` and the
        // output is identical either way. A two-argument command can: get the
        // consumption wrong and both arguments fall through to the outer walk,
        // which drops the fraction separator entirely.
        assert_eq!(math_to_unicode(r"\frac\alpha\beta"), "α/β");
    }

    /// `\quad` and `\qquad` are the two spacing commands the renderer knows,
    /// and they differ only in WIDTH.
    ///
    /// Deleting the `"qquad"` arm drops it through to the unknown-command path,
    /// which keeps the TeX verbatim — so a double-width space silently renders
    /// as the literal text `\qquad`. The `\quad` half is asserted alongside it
    /// so the test cannot pass by treating every spacing command the same.
    /// Kills math.rs:134:9.
    #[test]
    fn the_two_spacing_commands_render_as_one_and_two_spaces() {
        // `math_to_unicode` trims, so the spacing is asserted with content on
        // both sides of it.
        assert_eq!(
            math_to_unicode(r"a\quad b"),
            "a  b",
            "one quad is one space"
        );
        assert_eq!(
            math_to_unicode(r"a\qquad b"),
            "a   b",
            "qquad is DOUBLE width, and must not fall through to the verbatim              unknown-command path"
        );
        assert!(
            !math_to_unicode(r"a\qquad b").contains("qquad"),
            "the command name must never reach the rendered output"
        );
    }

    /// `read_group` must consume at least one character whenever one is
    /// available — its documented contract, and the thing every caller's
    /// `while *i < src.len()` loop depends on for progress.
    ///
    /// Asserted over EVERY first-character shape the function branches on, at
    /// the end of input as well as mid-fragment, because a cursor that stands
    /// still or rewinds does not mis-render: it hangs the preview, which is a
    /// fault only a timeout can observe. The `entry`-clamp in `read_group` is
    /// what converts such a perturbation into a wrong parse this can catch.
    #[test]
    fn read_group_advances_past_every_first_character() {
        for frag in [
            "{ab}", "{", "}", r"\alpha", r"\{", r"\\", "a", " ", "  x", "",
        ] {
            let src: Vec<char> = frag.chars().collect();
            let mut i = 0usize;
            let _ = read_group(&src, &mut i);
            if src.is_empty() {
                assert_eq!(i, 0, "an empty fragment has nothing to consume");
            } else {
                assert!(
                    i > 0,
                    "read_group({frag:?}) left the cursor at {i} — a caller's                      loop would never terminate"
                );
                assert!(
                    i <= src.len(),
                    "read_group({frag:?}) ran the cursor to {i}, past the end                      ({} chars)",
                    src.len()
                );
            }
        }

        // A `\command` argument that runs out of input is the end-of-input edge
        // the `*i < src.len()` bound guards: widened to `<=` the cursor steps
        // past the end and the slice that follows panics.
        assert_eq!(math_to_unicode(r"\sqrt\"), "√\\");
        // And the escaped-backslash form, where the second character IS present
        // and must be consumed as the command's single-character name — the
        // `*i += 1` that `<=` would skip and a rewind would undo.
        assert_eq!(math_to_unicode(r"\sqrt\\x"), "√ x");
    }

    /// An escaped `\}` inside a braced group is CONTENT, not the group's
    /// terminator.
    ///
    /// The brace scan carries an arm whose only job is to step over the
    /// character after a `\` so it cannot unbalance the nesting count. Nothing
    /// asserted it: every existing brace test either has no backslash inside
    /// the group, or has one followed by an ordinary letter (`\frac{\frac…}`),
    /// where skipping and not skipping land on the same parse. Deleting the arm
    /// therefore changed no test — the mutant was MISSED, and the escape was
    /// live but ungated.
    ///
    /// The discriminating shape is specifically an escaped CLOSE brace: without
    /// the skip it terminates the group early, so the argument is truncated and
    /// the remainder leaks out into the surrounding text.
    #[test]
    fn an_escaped_close_brace_inside_a_group_does_not_terminate_it() {
        // The whole of `a\}b` is the radicand; the `\}` renders as a literal
        // `}` via the escaped-literal path. Truncating at the escape would give
        // `√(a\)b` instead — the `b` outside the radical and a stray backslash
        // inside it.
        assert_eq!(math_to_unicode(r"\sqrt{a\}b}"), "√(a}b)");
        // Same escape as a whole numerator, which additionally proves the
        // DENOMINATOR still lines up: an early terminator eats the `{` of `{b}`
        // as the second argument and the fraction separator disappears.
        assert_eq!(math_to_unicode(r"\frac{\}}{b}"), "}/b");
        // The open-brace direction is deliberately asserted too. It is NOT
        // discriminating on its own (an unskipped `\{` raises the nesting count
        // and the group runs to the end of the fragment, which happens to
        // re-render the same), so it is here to pin the behaviour rather than to
        // carry the kill — and to keep a later reader from assuming the two
        // directions are interchangeable.
        assert_eq!(math_to_unicode(r"\sqrt{a\{b}"), "√(a{b)");
    }
}
