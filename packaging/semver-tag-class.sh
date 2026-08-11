#!/bin/sh
# semver-tag-class.sh — classify the ref being released. ONE definition of
# "prerelease", shared by every consumer.
#
# Usage:
#   class=$(sh packaging/semver-tag-class.sh)          # reads GITHUB_REF_*
#   class=$(sh packaging/semver-tag-class.sh v1.2.3)   # or an explicit ref name
#
# Prints exactly one word on stdout, and exits 0:
#
#   stable      a tag with NO SemVer prerelease segment (v0.4.63).
#               The release real users auto-update to.
#   prerelease  a tag WITH a well-formed SemVer prerelease segment
#               (v0.4.63-rc.1, v1.2.3-alpha.2+build.5).
#   non-tag     any other ref (a workflow_dispatch run on a branch).
#
# Why this is its own file
# ------------------------
# Two places need this answer and they MUST agree:
#
#   1. packaging/require-signing-key.sh — decides whether an ABSENT signing key
#      may ship. It deliberately TOLERATES an unsigned prerelease.
#   2. release.yml's "Create GitHub Release" step — decides whether the release
#      is marked `--prerelease` or promoted with `--latest`.
#
# While (2) had no notion of a prerelease at all, it forced
# `gh release edit --draft=false --latest` unconditionally. Combined with (1),
# cutting `v0.5.0-rc.1` with no signing key produced UNSIGNED artifacts
# published as the repo's current release — served to every download link and
# offered by the updater, which is built to reject exactly those bytes. Each
# behaviour was defensible alone; the hole was in the disagreement between them.
#
# So the classification lives here, in one file, rather than being restated in
# each consumer. Two copies of a predicate are two things that can drift, and a
# drift between these two is precisely the bug: an artifact simultaneously
# "unsigned because prerelease" and "published because stable".
#
# The invariant the consumers are pinned to:
#
#     stable  <=>  an unsigned build is FORBIDDEN  <=>  gets `--latest`
#
# so the set of refs allowed to ship unsigned is exactly the set of refs that
# must never be promoted to latest.
#
# Note the predicate is a real SemVer test, NOT "contains a hyphen". The
# workflow triggers on `v*`, so a hyphen test hands the tolerated-unsigned path
# to `v1.0-final`, `v0.5-hotfix` and a date tag like `v2026-08-10` — none of
# which is a SemVer prerelease (`1.0-final` has no PATCH field; `2026-08-10` is
# not a version triple). `v0.5.0-hotfix` DOES classify as a prerelease, because
# `0.5.0-hotfix` is a well-formed SemVer prerelease of 0.5.0; that is the
# correct reading of the tag, not a hole.
set -eu

REF_TYPE="${GITHUB_REF_TYPE:-}"
REF_NAME="${1:-${GITHUB_REF_NAME:-}}"

# An explicit ref-name argument means "classify this tag", so the ambient
# GITHUB_REF_TYPE (often empty off-CI) must not veto it.
if [ "$#" -eq 0 ] && [ "${REF_TYPE}" != "tag" ]; then
	echo "non-tag"
	exit 0
fi

# optional leading `v`, MAJOR.MINOR.PATCH, then `-<identifiers>` and optionally
# `+<build metadata>`.
if printf '%s' "${REF_NAME}" | grep -qE '^v?[0-9]+\.[0-9]+\.[0-9]+-[0-9A-Za-z.-]+(\+[0-9A-Za-z.-]+)?$'; then
	echo "prerelease"
	exit 0
fi

echo "stable"
