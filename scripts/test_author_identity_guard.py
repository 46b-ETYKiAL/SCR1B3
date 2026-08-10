#!/usr/bin/env python3
"""Falsification suite for the commit author/committer identity guard.

Every identity class the guard claims to reject is fed to it here and asserted
to be REJECTED, and every identity that must keep working is asserted to be
ACCEPTED. A guard with no negative cases is a guard nobody can safely tighten,
and an allowlist with no rejection cases is indistinguishable from one that
accepts everything.

IMPORTANT - why the rejected samples are assembled from fragments:
    This file is a tracked file and the content-safety audit scans it like any
    other. A real-looking mailbox written as one literal would be an actual
    leak sitting in the repository - the guard would be publishing the very
    PII it exists to suppress. Each sample is therefore split across a
    concatenation so no mailbox-shaped string exists in this file's own bytes,
    while the value handed to the guard at runtime is exactly the identity we
    mean to test. Keep this property when adding cases: assemble, never inline.

    The domains used below are ordinary consumer-mail domains chosen because
    they are NOT reserved: a reserved domain (example.com) would be exempt
    under the content-safety audit's documentation-domain rule, and the case
    would then pass for the wrong reason.

Run:  python scripts/test_author_identity_guard.py
      (or: python -m pytest scripts/test_author_identity_guard.py)
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

_HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(_HERE))

import author_identity_guard as aig  # noqa: E402

_AT = "@"

# (description, identity) - each MUST be rejected by the allowlist.
MUST_REJECT: list[tuple[str, str]] = [
    # The class that actually leaked: a personal mailbox on a consumer host.
    ("personal mailbox, privacy host", "someone" + _AT + "proton" + ".me"),
    ("personal mailbox, short privacy host", "a.person" + _AT + "pm" + ".me"),
    ("personal mailbox, freemail", "who" + _AT + "gmail" + ".com"),
    ("personal mailbox, freemail 2", "who" + _AT + "outlook" + ".com"),
    # A corporate mailbox is still a mailbox: allowlisted DOMAINS are not the
    # rule, allowlisted SHAPES are, and a real inbox is a real inbox.
    ("company mailbox", "first.last" + _AT + "acme-corp" + ".io"),
    # Near-misses on the noreply shape. Each is one character/segment away from
    # being permitted, which is exactly where a sloppy regex silently opens up.
    ("noreply lookalike, wrong host", "user" + _AT + "users.noreply.github" + ".io"),
    ("noreply lookalike, missing subdomain", "user" + _AT + "noreply.github" + ".com"),
    ("noreply as a subdomain of an attacker host", "user" + _AT + "users.noreply.github.com" + ".evil" + ".net"),
    ("noreply string only in the local part", "users.noreply.github.com" + _AT + "gmail" + ".com"),
    # Structurally broken identities must not slip through as "not matching a
    # violation pattern" - the allowlist is positive, so these fail closed.
    ("empty identity", ""),
    ("whitespace identity", "   "),
    ("no at-sign", "not-an-email"),
    ("local part only", "someone" + _AT),
]

# (description, identity) - each MUST be accepted.
MUST_ACCEPT: list[tuple[str, str]] = [
    ("canonical numeric-prefixed noreply", "133311911+46b-ETYKiAL@users.noreply.github.com"),
    ("bare handle noreply", "46b-ETYKiAL@users.noreply.github.com"),
    ("noreply, different contributor", "12345+someone-else@users.noreply.github.com"),
    ("noreply, mixed case host", "someone@Users.NoReply.GitHub.Com"),
    ("web-ui commit identity", "noreply@github.com"),
    ("forge bot", "github-actions[bot]@users.noreply.github.com"),
    ("dependabot noreply", "49699333+dependabot[bot]@users.noreply.github.com"),
]


def test_rejects_every_non_allowlisted_identity() -> None:
    for name, ident in MUST_REJECT:
        assert aig.identity_is_allowed(ident) is None, f"wrongly allowed: {name}"


def test_accepts_every_publishable_identity() -> None:
    for name, ident in MUST_ACCEPT:
        assert aig.identity_is_allowed(ident) is not None, f"wrongly rejected: {name}"


def test_violation_messages_do_not_republish_the_mailbox() -> None:
    """A guard that prints the PII it caught has leaked it into the CI log."""
    for _name, ident in MUST_REJECT:
        red = aig.redact(ident)
        if not ident.strip():
            # A blank identity carries nothing to redact; it is reported as
            # such. Asserting a mask here would be asserting on nothing.
            assert red == "<empty>", f"blank identity mis-rendered: {red}"
            continue
        if "@" in ident and len(ident.strip()) > 4:
            local, _, domain = ident.partition("@")
            assert local not in red or len(local) <= 2, f"local part republished: {red}"
            assert domain.split(".")[0] not in red or len(domain.split(".")[0]) <= 1, (
                f"domain republished: {red}"
            )
        assert "***" in red, f"redaction produced no mask: {red}"


def test_this_suite_carries_no_literal_leak() -> None:
    """The corpus must be assembled, never inlined (see the module docstring).

    Enforced against the content-safety audit itself, so this suite cannot
    become the leak. Skipped only if the audit is absent - never silently.
    """
    audit = _HERE / "content_safety_audit.py"
    assert audit.is_file(), "content_safety_audit.py must sit beside this suite"
    sys.path.insert(0, str(_HERE))
    import content_safety_audit as csa

    me = Path(__file__).resolve()
    findings = csa.scan_text(me.read_text(encoding="utf-8"), me.name)
    assert not findings, f"this suite leaks its own samples: {findings}"


def test_empty_range_can_be_made_a_failure() -> None:
    """"I checked zero commits" must be distinguishable from "I passed".

    Without `--require-nonempty` a misconfigured range would report PASS while
    checking nothing - a gate that cannot fail because it never ran.
    """
    rc_lenient = aig.main(["--base", "HEAD", "--head", "HEAD"])
    rc_strict = aig.main(["--base", "HEAD", "--head", "HEAD", "--require-nonempty"])
    assert rc_lenient == 0, "an empty range should pass by default"
    assert rc_strict == 1, "--require-nonempty must fail on an empty range"


def test_guard_actually_fails_on_a_real_bad_commit() -> None:
    """End-to-end proof the guard exits non-zero on a genuinely bad commit.

    The allowlist unit-cases above prove the predicate; this proves the whole
    program - range resolution, batching, exit code - goes RED for a commit
    that really carries a non-allowlisted identity. A disposable throwaway repo
    is used so nothing touches the real one.
    """
    import tempfile

    bad = "throwaway" + _AT + "not-allowlisted" + ".test"
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)

        def g(*a: str) -> None:
            subprocess.run(["git", *a], cwd=tmp, check=True,
                           capture_output=True, text=True)

        g("init", "-q", "-b", "main")
        g("config", "user.name", "Throwaway")
        g("config", "user.email", bad)
        g("commit", "-q", "--allow-empty", "-m", "base")
        base = subprocess.run(["git", "rev-parse", "HEAD"], cwd=tmp, check=True,
                              capture_output=True, text=True).stdout.strip()
        g("commit", "-q", "--allow-empty", "-m", "bad identity")

        real_root = aig.ROOT
        try:
            aig.ROOT = tmp
            rc = aig.main(["--base", base, "--head", "HEAD"])
        finally:
            aig.ROOT = real_root
    assert rc == 1, "the guard passed a commit carrying a non-allowlisted identity"


def _main() -> int:
    failures = 0
    for name, ident in MUST_REJECT:
        if aig.identity_is_allowed(ident) is not None:
            print(f"FAIL  expected rejection, got acceptance: {name}")
            failures += 1
    for name, ident in MUST_ACCEPT:
        if aig.identity_is_allowed(ident) is None:
            print(f"FAIL  expected acceptance, got rejection: {name}")
            failures += 1
    for fn in (
        test_violation_messages_do_not_republish_the_mailbox,
        test_this_suite_carries_no_literal_leak,
        test_empty_range_can_be_made_a_failure,
        test_guard_actually_fails_on_a_real_bad_commit,
    ):
        try:
            fn()
        except AssertionError as e:
            print(f"FAIL  {fn.__name__}: {e}")
            failures += 1
    print(
        f"author-identity falsification: {len(MUST_REJECT)} reject-cases, "
        f"{len(MUST_ACCEPT)} accept-cases, {failures} failure(s)"
    )
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(_main())
