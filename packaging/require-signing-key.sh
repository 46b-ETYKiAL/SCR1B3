#!/bin/sh
# require-signing-key.sh — decide whether an ABSENT signing key may ship.
#
# Usage (from release.yml's signing step, when MINISIGN_SECRET_KEY is empty):
#   sh packaging/require-signing-key.sh
#
# Exit status
# -----------
#   1  the ref is a STABLE tag  -> a stable release MUST be signed; fail loudly
#   0  anything else            -> unsigned artifacts are tolerated, with a
#                                  ::warning:: on the log
#
# Why this exists
# ---------------
# release.yml's signing step used to open with
#
#     if [ -z "${MINISIGN_SECRET_KEY:-}" ]; then
#       echo "::warning::… shipping UNSIGNED artifacts"
#       exit 0
#     fi
#
# and that `exit 0` skipped the signing AND the step's own fail-closed
# self-verify gate in one move: with the secret unprovisioned the whole gate was
# unreachable, so CI went green and PUBLISHED artifacts the in-app updater is
# built to REJECT. A stable release that no deployed client can auto-update to
# is a release-readiness landmine that looks like a success.
#
# The rule is therefore ref-sensitive, and deliberately narrow:
#
#   * STABLE TAG (no SemVer prerelease segment, e.g. v0.4.63) — the release real
#     users auto-update to. Unsigned is a HARD FAILURE. Provision the key (see
#     packaging/signing.md) or cut a prerelease tag instead.
#   * PRERELEASE TAG (v0.4.63-rc.1, v0.4.63-pre) — rc/pre builds are opt-in
#     downloads, not auto-update targets. Warn and continue.
#   * NON-TAG ref (a workflow_dispatch run on a branch) — behaviour deliberately
#     UNCHANGED: warn and continue. Whether a dispatch run should be allowed to
#     produce unsigned artifacts at all is an open owner decision, and this
#     script does not pre-empt it.
#
# Ported from the sibling C0PL4ND repo's release.yml, which already fails closed
# on a stable tag. It lives in a script rather than inline in the YAML for one
# reason: a gate that cannot be EXECUTED cannot be shown to fail, and an
# unfalsified gate is exactly the defect being fixed here. See
# `the_signing_gate_fails_on_a_stable_tag_with_no_key` in
# crates/scribe-app/src/integration/mod.rs, which runs this file for real.
set -eu

REF_TYPE="${GITHUB_REF_TYPE:-}"
REF_NAME="${GITHUB_REF_NAME:-}"

if [ "${REF_TYPE}" != "tag" ]; then
	echo "::warning::MINISIGN_SECRET_KEY not set on a non-tag ref (${REF_TYPE:-?}/${REF_NAME:-?}) — shipping checksummed but UNSIGNED artifacts (auto-update will reject them). See packaging/signing.md."
	exit 0
fi

# A tag is a PRERELEASE only when it carries a real SemVer prerelease segment:
# an optional leading `v`, then MAJOR.MINOR.PATCH, then `-<identifiers>` (and
# optionally `+<build metadata>`).
#
# This was `case "${REF_NAME}" in *-*)` — "contains a hyphen ANYWHERE". The
# workflow triggers on `v*`, so that handed the unsigned-is-fine path to every
# stable tag that merely happened to contain a hyphen: `v1.0-final`,
# `v0.5-hotfix` and a date tag like `v2026-08-10` would each have PUBLISHED
# UNSIGNED and green, and every deployed client would reject the release. None
# of those is a SemVer prerelease — `1.0-final` has no PATCH field, and
# `2026-08-10` is not a version triple at all — so they now fail closed with the
# rest of the stable tags.
#
# `v0.5.0-hotfix` DOES stay a prerelease: `0.5.0-hotfix` is a well-formed SemVer
# prerelease of 0.5.0, so treating it as one is the correct reading of the tag,
# not a hole. Cut `v0.5.1` (or provision the key) to ship it signed.
#
# Pinned in BOTH directions by `a_hyphenated_non_semver_tag_is_stable_and_must_
# fail_unsigned` and `a_wellformed_semver_prerelease_is_still_tolerated`.
if printf '%s' "${REF_NAME}" | grep -qE '^v?[0-9]+\.[0-9]+\.[0-9]+-[0-9A-Za-z.-]+(\+[0-9A-Za-z.-]+)?$'; then
	echo "::warning::MINISIGN_SECRET_KEY not set — shipping checksummed but UNSIGNED prerelease artifacts (auto-update will reject them; acceptable for an rc/pre tag ${REF_NAME})."
	exit 0
fi

echo "::error::MINISIGN_SECRET_KEY not set on a STABLE tag (${REF_NAME:-?}). A stable release MUST be signed — the fail-closed in-app updater verifies a minisign signature before installing, so every deployed client would REJECT this release and auto-update would silently stop working. Provision the signing key (packaging/signing.md) or cut a prerelease (-rc/-pre) tag instead. Failing the release."
exit 1
