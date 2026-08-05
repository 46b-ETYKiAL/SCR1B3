#!/bin/sh
# SCR1B3 one-line installer (Linux/macOS).
#   curl -fsSL https://raw.githubusercontent.com/46b-ETYKiAL/SCR1B3/master/packaging/install.sh | sh
#
# Downloads the release artifact matching this OS/arch from GitHub Releases,
# verifies its SHA-256 AND its Ed25519 (minisign) signature, and installs the
# `scr1b3` binary to a bin dir on PATH. POSIX sh, shellcheck-clean. No
# telemetry. No data leaves your machine beyond the GitHub download itself.
#
# --- Why a signature and not just a checksum -------------------------------
# A SHA-256 sidecar fetched from the SAME host as the artifact is a corruption
# check, not a security control: whoever can serve you a malicious tarball can
# serve the matching `.sha256` alongside it. The authenticity control is the
# signature checked against PUBKEY below, which is embedded in this script and
# therefore shares the trust root you accepted by choosing to run this script.
#
# This script FAILS CLOSED. Previously the checksum was entirely optional --
# `curl ... || true` plus `if [ -f sum ] && command -v sha256sum` -- so a
# missing sidecar or a machine without sha256sum installed silently skipped
# verification and installed whatever had been downloaded. There is now no path
# that installs an unverified binary, and deliberately no env var to skip it.
set -eu

# Canonical public release repository, confirmed via
# `gh api repos/46b-ETYKiAL/SCR1B3 --jq .full_name`. Note the branch is
# `master`; the previous usage header said `main`, which does not exist.
REPO="46b-ETYKiAL/SCR1B3"
BIN="scr1b3"

# Minisign public key for release artifacts (packaging/minisign.pub).
PUBKEY_ID="BD4ADF9145E13B17"
PUBKEY="RWQXO+FFkd9Kvdw2hUrWtt5Eoebj41ckYRPGs7tTH+zym1moqwXT5D7N"

die() { echo "error: $1" >&2; exit 1; }

# --- locate a signature verifier (FAIL CLOSED) -----------------------------
if command -v minisign >/dev/null 2>&1; then
  VERIFIER=minisign
elif command -v rsign >/dev/null 2>&1; then
  VERIFIER=rsign
else
  echo "error: no signature verifier found — refusing to install." >&2
  echo "" >&2
  echo "SCR1B3 release artifacts are Ed25519-signed. This installer will not" >&2
  echo "install a binary it cannot verify. Install one of:" >&2
  echo "  minisign  (apt/dnf/pacman/brew install minisign)" >&2
  echo "  rsign2    (cargo install rsign2)" >&2
  echo "then re-run. Or download the archive + .minisig from" >&2
  echo "  https://github.com/${REPO}/releases" >&2
  echo "and verify manually against public key ${PUBKEY_ID}." >&2
  exit 1
fi

# A checksum tool is likewise required, not optional.
if command -v sha256sum >/dev/null 2>&1; then
  SHACMD="sha256sum"
elif command -v shasum >/dev/null 2>&1; then
  SHACMD="shasum -a 256"
else
  die "no sha256 tool found (need sha256sum or shasum)"
fi

os=$(uname -s)
arch=$(uname -m)

case "$os" in
  Linux)  target_os="unknown-linux-gnu" ;;
  Darwin) target_os="apple-darwin" ;;
  *) echo "unsupported OS: $os (use the Windows installer / winget)" >&2; exit 1 ;;
esac

case "$arch" in
  x86_64|amd64) target_arch="x86_64" ;;
  arm64|aarch64) target_arch="aarch64" ;;
  *) echo "unsupported arch: $arch" >&2; exit 1 ;;
esac

target="${target_arch}-${target_os}"
asset="${BIN}-${target}.tar.gz"
base="https://github.com/${REPO}/releases/latest/download"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

echo "downloading ${asset} ..."
curl -fsSL "${base}/${asset}" -o "${tmp}/${asset}" \
  || die "download failed: ${base}/${asset}"

# Both sidecars are REQUIRED. A missing one aborts rather than skipping the
# check that a missing sidecar would otherwise silently disable.
curl -fsSL "${base}/${asset}.sha256" -o "${tmp}/sum" \
  || die "no .sha256 published for ${asset} — refusing to install unverified"
curl -fsSL "${base}/${asset}.minisig" -o "${tmp}/sig" \
  || die "no .minisig published for ${asset} — refusing to install unverified"
[ -s "${tmp}/sum" ] || die "checksum file is empty"
[ -s "${tmp}/sig" ] || die "signature file is empty"

echo "verifying checksum ..."
expected=$(awk '{print $1}' "${tmp}/sum")
actual=$(${SHACMD} "${tmp}/${asset}" | awk '{print $1}')
[ -n "$expected" ] || die "checksum file is malformed"
[ "$expected" = "$actual" ] || die "checksum mismatch — aborting"

echo "verifying signature ..."
printf 'untrusted comment: minisign public key: %s\n%s\n' \
  "${PUBKEY_ID}" "${PUBKEY}" > "${tmp}/minisign.pub"
if [ "$VERIFIER" = "minisign" ]; then
  minisign -V -p "${tmp}/minisign.pub" -x "${tmp}/sig" -m "${tmp}/${asset}" \
    >/dev/null 2>&1 || die "SIGNATURE VERIFICATION FAILED — not authentic. Aborting."
else
  rsign verify -p "${tmp}/minisign.pub" -x "${tmp}/sig" "${tmp}/${asset}" \
    >/dev/null 2>&1 || die "SIGNATURE VERIFICATION FAILED — not authentic. Aborting."
fi
echo "signature verified (${VERIFIER}, key ${PUBKEY_ID})"

tar -xzf "${tmp}/${asset}" -C "$tmp"

# Pick a writable bin dir on PATH.
if [ -w "/usr/local/bin" ]; then
  dest="/usr/local/bin"
else
  dest="${HOME}/.local/bin"
  mkdir -p "$dest"
fi

install -m 0755 "${tmp}/${BIN}" "${dest}/${BIN}"
echo "installed ${BIN} to ${dest}"

# The archive carries the license texts that must accompany every copy of the
# embedded fonts (OFL-1.1 s2). Keep them alongside the installed binary.
if [ -d "${tmp}/licenses" ]; then
  licdir="${dest}/../share/scr1b3/licenses"
  if mkdir -p "$licdir" 2>/dev/null && cp -R "${tmp}/licenses/." "$licdir/" 2>/dev/null; then
    [ -f "${tmp}/THIRD-PARTY-LICENSES.md" ] && \
      cp "${tmp}/THIRD-PARTY-LICENSES.md" "$licdir/../" 2>/dev/null || true
    echo "license texts installed to $licdir"
  fi
fi
case ":${PATH}:" in
  *":${dest}:"*) ;;
  *) echo "note: add ${dest} to your PATH" ;;
esac
echo "run: ${BIN}"
