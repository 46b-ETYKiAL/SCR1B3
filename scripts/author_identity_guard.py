#!/usr/bin/env python3
"""Reject commits whose author/committer identity is not an allowlisted one.

Commit metadata is exactly as public as the tree, and it is the vector that
actually leaks: a mailbox committed into `%ae`/`%ce` is published the moment
the branch is pushed, and it cannot be redacted afterwards without rewriting
history. A tree-content audit cannot see it at all.

Four design decisions are load-bearing - read them before editing:

1.  **Allowlist by SHAPE, never a denylist of addresses.** Listing the
    mailboxes we want to keep out would write those exact mailboxes into a
    public file - the guard would publish the very PII it exists to suppress.
    The rule is inverted instead: an identity must MATCH a permitted shape
    (a forge `noreply` address, or an explicitly configured domain) and
    everything else is a violation, whatever it happens to be. A new leak
    class therefore needs no new pattern; it is rejected by default.

2.  **Range-scoped, not history-wide.** The default range is the push or PR
    range. Historical commits already carry mailboxes that only a history
    rewrite - an owner decision - can clear, and a gate that is permanently
    red for something no ordinary PR can fix is a gate people learn to ignore.
    Pass `--all` to audit the whole history as a reporting run.

3.  **An unresolvable range fails CLOSED.** A base that cannot be resolved
    (a force-push, a shallow clone, a first push of a new branch) falls back
    to a bounded walk of the head commit's recent ancestry rather than
    silently checking nothing. An empty range is reported as such, and
    `--require-nonempty` turns "I checked zero commits" into a failure - the
    difference between a guard that passed and a guard that never ran.

4.  **Both identities, every commit.** `git commit --amend --author=...`
    changes `%ae` alone; a rebase changes `%ce` alone. Checking one of the two
    leaves the other wide open, so both are checked on every commit.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

# Identity shapes that expose no mailbox and are therefore permitted.
#
#   1. Forge-generated noreply identities - `<id>+<handle>@users.noreply.
#      github.com` and the bare `noreply@github.com` a web-UI commit carries.
#      These are the addresses the forge issues precisely so a real mailbox
#      never has to be published.
#   2. Bot identities, which are forge accounts and carry no personal mailbox.
#
# A contributor's NAME is deliberately not constrained - names are public
# attribution and are welcome. Only the mailbox is.
ALLOWED_IDENTITY_PATTERNS: list[tuple[str, re.Pattern[str]]] = [
    (
        "github noreply",
        re.compile(r"^(?:[0-9]+\+)?[A-Za-z0-9._%+\-]+@users\.noreply\.github\.com$", re.I),
    ),
    ("github web-ui noreply", re.compile(r"^noreply@github\.com$", re.I)),
    (
        "forge bot",
        re.compile(r"^[A-Za-z0-9._%+\-]+\[bot\]@users\.noreply\.github\.com$", re.I),
    ),
    (
        "dependabot",
        re.compile(r"^(?:support@dependabot\.com|49699333\+dependabot\[bot\]@users\.noreply\.github\.com)$", re.I),
    ),
]

# How many commits to walk when a range base cannot be resolved. Bounded so a
# first push of a long-lived branch cannot turn into a full-history scan, but
# large enough that a normal feature branch is covered end to end.
FALLBACK_DEPTH = 200

_ZERO_SHA = re.compile(r"^0{7,40}$")


def identity_is_allowed(addr: str) -> str | None:
    """Return the name of the shape that permits ``addr``, or ``None``."""
    a = addr.strip().lower()
    for name, pat in ALLOWED_IDENTITY_PATTERNS:
        if pat.match(a):
            return name
    return None


def redact(addr: str) -> str:
    """A violation message must not republish the mailbox it is reporting."""
    a = addr.strip()
    if "@" not in a:
        return f"{a[:2]}***" if a else "<empty>"
    local, _, domain = a.partition("@")
    dparts = domain.split(".")
    dmask = (dparts[0][:1] + "***") if dparts[0] else "***"
    tld = "." + ".".join(dparts[1:]) if len(dparts) > 1 else ""
    return f"{local[:2]}***@{dmask}{tld}"


def git(*args: str) -> str:
    return subprocess.run(
        ["git", *args], cwd=ROOT, capture_output=True, text=True,
        encoding="utf-8", errors="replace", check=True,
    ).stdout


def _rev_ok(rev: str) -> bool:
    if not rev or _ZERO_SHA.match(rev):
        return False
    try:
        git("rev-parse", "--verify", f"{rev}^{{commit}}")
        return True
    except subprocess.CalledProcessError:
        return False


def resolve_range(base: str, head: str) -> tuple[list[str], str]:
    """Commits to audit, plus a human description of how they were chosen."""
    head = head or "HEAD"
    if not _rev_ok(head):
        head = "HEAD"

    if _rev_ok(base):
        try:
            out = git("rev-list", f"{base}..{head}")
            return ([c for c in out.split() if c], f"{base[:12]}..{head[:12]}")
        except subprocess.CalledProcessError:
            pass

    # Base unresolvable (force-push, shallow clone, or a brand-new branch).
    # Fall back to a bounded ancestry walk rather than checking nothing.
    out = git("rev-list", f"-n{FALLBACK_DEPTH}", head)
    return (
        [c for c in out.split() if c],
        f"last {FALLBACK_DEPTH} commit(s) of {head[:12]} (base unresolvable)",
    )


def audit_commits(shas: list[str]) -> list[str]:
    """One violation line per (commit, offending role)."""
    if not shas:
        return []
    out: list[str] = []
    # Batch: one `git show` per commit would be O(n) processes.
    raw = git("show", "--no-patch", "--format=%H%x1f%ae%x1f%ce%x1e", *shas)
    for rec in raw.split("\x1e"):
        rec = rec.strip()
        if not rec:
            continue
        parts = rec.split("\x1f")
        if len(parts) != 3:
            continue
        sha, ae, ce = parts
        for role, addr in (("author", ae), ("committer", ce)):
            if not addr.strip():
                out.append(f"{sha[:12]}: empty {role} identity")
                continue
            if identity_is_allowed(addr) is None:
                out.append(
                    f"{sha[:12]}: {role} identity is not allowlisted "
                    f"({redact(addr)})"
                )
    return out


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description="Reject commits whose identity is not allowlisted."
    )
    ap.add_argument("--base", default="", help="range base (exclusive)")
    ap.add_argument("--head", default="HEAD", help="range head (inclusive)")
    ap.add_argument("--all", action="store_true", help="audit the whole history")
    ap.add_argument(
        "--require-nonempty",
        action="store_true",
        help="fail if the resolved range contains no commits",
    )
    args = ap.parse_args(argv)

    if args.all:
        shas = [c for c in git("rev-list", "--all").split() if c]
        how = "entire history"
    else:
        shas, how = resolve_range(args.base, args.head)

    print(f"author-identity: checking {len(shas)} commit(s) [{how}]")

    if not shas:
        if args.require_nonempty:
            print("\nFAIL - the resolved range is empty; nothing was checked.")
            return 1
        print("PASS - empty range, nothing to check.")
        return 0

    violations = audit_commits(shas)
    if violations:
        print(f"\nFAIL - {len(violations)} disallowed commit identity/identities:\n")
        for v in violations:
            print(f"  {v}")
        print(
            "\nCommit metadata is public and cannot be redacted after a push.\n"
            "Set a forge noreply address and re-author the offending commits:\n"
            "  git config user.email '<id>+<handle>@users.noreply.github.com'\n"
            "  git rebase -r --reset-author-date --exec "
            "'git commit --amend --no-edit --reset-author' <base>"
        )
        return 1

    print("PASS - every commit in range carries an allowlisted identity.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
