#!/usr/bin/env python3
"""Falsification suite for the public-repo content-safety audit.

Every leak class the audit claims to detect is fed to it here and asserted to
be CAUGHT, and every legitimate construct that a naive version of the same rule
would flag is asserted to stay CLEAN. A guard with no negative cases is a guard
nobody can safely tighten.

IMPORTANT - why every sample is assembled from fragments:
    This file is a tracked file, so the audit scans it like any other. A sample
    written as one literal would be a real leak sitting in the repository. Each
    sample is therefore split across a concatenation so that no leak-shaped
    string exists in the file's own bytes, while the value handed to the
    scanner at runtime is exactly the leak we mean to test. Keep this property
    when adding cases: assemble, never inline.

Run:  python scripts/test_content_safety_audit.py
      (or: python -m pytest scripts/test_content_safety_audit.py)
"""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location("csa", _HERE / "content_safety_audit.py")
assert _spec and _spec.loader
csa = importlib.util.module_from_spec(_spec)
sys.modules["csa"] = csa
_spec.loader.exec_module(csa)

# Fragments. Split points are chosen so this file's own text never matches.
_WIN = "C:/Users" + "/"
_WIN_BS = "C:\\Users" + "\\"
_HOME = "/home" + "/"
_MAC = "/Users" + "/"
_DOT = "."


# (description, sample) - each MUST produce at least one finding.
MUST_CATCH: list[tuple[str, str]] = [
    # Windows user profile, BOTH separator conventions. The forward-slash form
    # is the one an earlier revision of this audit could not see at all.
    ("windows path, forward slash", f'"{_WIN}a.dev_/Documents/n.md".to_string(),'),
    ("windows path, back slash", f'let p = "{_WIN_BS}a.dev_";'),
    # Personal mailboxes - a class with no pattern at all before.
    ("personal email, consumer domain", "author = someone@proton" + ".me"),
    ("personal email, second domain", "contact: a.person@pm" + ".me"),
    ("personal email, freemail", "reviewer <who@gmail" + ".com>"),
    # The opaque-URI exemption is an explicit scheme ALLOWLIST, not a generic
    # `word:` rule. A generic rule would exempt ordinary prose that happens to
    # carry a colon and turn a real leak into a pass — strictly worse than the
    # false positive it fixes. These pin that it stayed narrow.
    ("colon-prefixed prose is not a uri scheme", "Contact:who@gmail" + ".com"),
    ("author label is not a uri scheme", "Author:a.person@pm" + ".me"),
    # Home paths must not require a trailing slash.
    ("linux home, no trailing slash", "service runs as " + _HOME + "deploy"),
    ("linux home, real account", "cd " + _HOME + "j.smith/build"),
    ("macos home", "open " + _MAC + "jbloggs/dev/x"),
    # Internal tooling / monorepo / work-item tokens (hash-matched).
    ("tooling dir", "see " + _DOT + "s4f3-data/notes.md"),
    ("tooling dir, second", "path: " + _DOT + "claude/agents"),
    ("monorepo id embedded in a longer path", "C:/x/Itasha.Corp_S4F3-" + "R0UT3-4RB" + "1T3R/y"),
    ("work-item token", "<!-- bespoke instrument (plan-" + "611). -->"),
    # Secret shapes.
    ("private key block", "-----BEGIN OPENSSH PRIVATE " + "KEY-----"),
    ("aws access key", "AKIA" + "IOSFODNN7EXAMPLE"),
    ("github token", "ghp_" + "a" * 36),
    ("secret assignment", 'api_key = "' + "abcdefghijklmnopqrstuvwxyz0123" + '"'),
]

# (description, sample) - each MUST produce no finding at all.
MUST_NOT_FIRE: list[tuple[str, str]] = [
    # A name that shares a leading token-run with the internal monorepo id.
    # `token_probes` splits on `. _ -` and emits every contiguous run, so this
    # shape yields `itasha-corp-s4f3` (and `itasha`, `corp`, `itasha-corp`, …) —
    # the exact prefix of the internal monorepo identifier. These cases prove
    # the suppression digests stay pinned to the WHOLE identifier: the day
    # someone suppresses a prefix instead, this fires.
    #
    # This was the public repo's own name until it was renamed to `SCR1B3`.
    # It is deliberately KEPT rather than swapped for the new name: the new
    # name shares NO probe with the monorepo id, so swapping it in would delete
    # the collision coverage and leave a case that asserts nothing.
    ("prefix-collision with the monorepo id, url", "https://github.com/46b-ETYKiAL/Itasha.Corp_S4F3-SCR1B3/releases"),
    ("prefix-collision with the monorepo id, prose", "Itasha.Corp_S4F3-SCR1B3 is the repository"),
    # The public repo's current name.
    ("public repo url", "https://github.com/46b-ETYKiAL/SCR1B3/releases"),
    ("public repo name in prose", "SCR1B3 is the repository"),
    # The canonical publishing identity is not PII.
    ("canonical noreply identity", "133311911+46b-ETYKiAL@users.noreply.github.com"),
    # Documentation placeholders in test fixtures identify nobody.
    ("placeholder home, user", 'format_dropped_path("' + _HOME + 'user/file.txt")'),
    ("placeholder home, alice", "cwd=" + _HOME + "alice/proj"),
    ("placeholder home, op", 'insert(PaneId(0), "' + _HOME + 'op/work")'),
    # RFC 2606 / RFC 6761 reserved domains are documentation, not mailboxes.
    ("reserved domain, example.com", "maintainer@example.com"),
    ("reserved domain, .test", 'mailto_url("a@b.test", &title, &body)'),
    ("reserved domain, .example", "Maintainer: Corp <x@corp.example>"),
    # Ordinary English that merely shares a spelling with a suppressed token.
    ("word that shares a tooling name", "the claude model was used here"),
    ("word 'plan' without a number", "the plan is to ship; see plan B"),
    ("ordinary prose", "This terminal renders sixel images safely."),
    # `user@host` in a URL authority is not a mailbox. This shape is the whole
    # point of a URL-confinement test, so flagging it would discourage exactly
    # the security tests we want written.
    ("url userinfo in a confinement test", 'assert!(confined("https://api.github.com@evil.example.com/x").is_err());'),
    # …and the OPAQUE URI form, which has no `//` at all. The exemption used to
    # require `://`, so every `mailto:` fixture was reported as a personal
    # mailbox — `md_ops.rs` and `url_scan.rs` both tripped it. Rewriting those
    # fixtures to a reserved domain only hid it until the next `mailto:` fixture.
    # NOTE the concatenation is at RUNTIME (outside the quotes), so the sample
    # actually scanned is a COMPLETE `mailto:user@domain.tld`. Splitting inside
    # the literal would leave the domain TLD-less, the email pattern would not
    # match at all, and the case would pass for the wrong reason — a vacuous
    # test that says nothing about the exemption it claims to cover.
    # The domains are deliberately NON-reserved, so the ONLY thing that can
    # exempt them is the opaque-URI rule under test.
    ("mailto, opaque uri form", "mailto:a@b" + ".com"),
    ("mailto, inside a rust assert", 'is_clickable_url("mailto:who@gmail' + '.com")'),
    ("xmpp, opaque uri form", "xmpp:room@conference" + ".chat"),
    # A trailing file extension means a filename, not a domain.
    ("apple iconset member", 'cp "${D}/app-32.png" "${S}/icon_16x16@2x.png"'),
    # A single-character account name identifies nobody.
    ("single-letter account in a prompt fixture", r't.advance(b"line\r\nC:\Users\x>");'),
]


def test_catches_every_known_leak_class() -> None:
    for name, sample in MUST_CATCH:
        assert csa.scan_text(sample, "probe"), f"missed leak class: {name}"


def test_does_not_fire_on_legitimate_content() -> None:
    for name, sample in MUST_NOT_FIRE:
        assert not csa.scan_text(sample, "probe"), f"false positive on: {name}"


def test_third_party_attribution_is_exempt_from_the_email_rule_only() -> None:
    """Upstream authors' addresses in licence texts must be reproducible."""
    email = "Copyright (c) 2011, Someone (someone@upstream" + ".se)"
    assert csa.scan_text(email, "THIRD-PARTY-LICENSES.md")
    assert not csa.scan_text(email, "THIRD-PARTY-LICENSES.md", third_party=True)
    # ...but the exemption is email-only: a path still fires in the same file.
    assert csa.scan_text(_WIN + "a.dev_/x", "THIRD-PARTY-LICENSES.md", third_party=True)


def test_audit_does_not_exempt_itself() -> None:
    """The audit is scanned like any other tracked file."""
    src = (_HERE / "content_safety_audit.py").read_text(encoding="utf-8")
    assert "SKIP_FILES" not in src, "self-exemption must not be reintroduced"
    assert "content_safety_audit.py" in {p.name for p in csa.tracked_files()}


def test_this_suite_carries_no_literal_leak() -> None:
    """The corpus must be assembled, never inlined (see the module docstring)."""
    me = Path(__file__).resolve()
    assert not csa.scan_text(
        me.read_text(encoding="utf-8"), me.name
    ), "this suite leaks its own samples"


def _main() -> int:
    failures = 0
    for name, sample in MUST_CATCH:
        if not csa.scan_text(sample, "probe"):
            print(f"FAIL  expected a finding, got none: {name}")
            failures += 1
    for name, sample in MUST_NOT_FIRE:
        found = csa.scan_text(sample, "probe")
        if found:
            print(f"FAIL  unexpected finding for {name}: {found}")
            failures += 1
    for fn in (
        test_third_party_attribution_is_exempt_from_the_email_rule_only,
        test_audit_does_not_exempt_itself,
        test_this_suite_carries_no_literal_leak,
    ):
        try:
            fn()
        except AssertionError as e:
            print(f"FAIL  {fn.__name__}: {e}")
            failures += 1
    print(
        f"content-safety falsification: {len(MUST_CATCH)} catch-cases, "
        f"{len(MUST_NOT_FIRE)} clean-cases, {failures} failure(s)"
    )
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(_main())
