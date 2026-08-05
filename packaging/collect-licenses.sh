#!/bin/sh
# collect-licenses.sh — stage every license text that MUST accompany a
# distributed copy of SCR1B3.
#
# Usage:
#   packaging/collect-licenses.sh <repo-root> <dest-dir>
#
# Why this exists
# ---------------
# SCR1B3 `include_bytes!`-embeds 22 third-party typefaces directly into the
# shipped executable (crates/scribe-app/src/app/render_support.rs). Twenty-one
# are licensed under the SIL Open Font License 1.1 and one (Syncopate) under
# Apache-2.0. It also `include_str!`-embeds a third-party English word list
# (crates/scribe-core/assets/dict/en_US.txt) as the built-in spell-check
# dictionary.
#
#   OFL-1.1 section 2 requires that the license text accompany EVERY copy of the
#   Font Software, including when it is bundled inside a larger software package.
#
# The release `.tar.gz` previously contained ONLY the binary — not even
# LICENSE-MIT. Shipping the fonts compiled in but the license text nowhere is a
# license breach, not a paperwork nit. Every packaging path — .tar.gz, .deb,
# .AppImage, .dmg/.app, the native Windows installer — must call this script so
# the obligation travels with the artifact.
#
# Layout produced under <dest-dir>:
#   LICENSE-MIT
#   LICENSE-APACHE            (when present)
#   THIRD-PARTY-LICENSES.md
#   licenses/fonts/<FontName>/<OFL.txt|LICENSE.txt>
#   licenses/dictionaries/en_US-PROVENANCE.txt
#
# FAILS CLOSED: if a bundled font directory has no license file, or a required
# top-level license document is missing, this script exits non-zero and the
# release build fails. A silently-incomplete license set is exactly the
# regression this guards.
set -eu

ROOT="${1:?usage: collect-licenses.sh <repo-root> <dest-dir>}"
DEST="${2:?usage: collect-licenses.sh <repo-root> <dest-dir>}"

# SCR1B3's fonts live at the REPO ROOT (unlike the sibling C0PL4ND, whose fonts
# sit under its app crate). That difference is why a packaging line that copies
# repo-root `assets/` happens to catch the fonts here and catches nothing there.
FONT_ROOT="${ROOT}/assets/fonts"
DICT="${ROOT}/crates/scribe-core/assets/dict/en_US.txt"

REQUIRED="LICENSE-MIT THIRD-PARTY-LICENSES.md"
OPTIONAL="LICENSE-APACHE README.md"

mkdir -p "${DEST}"

# --- top-level license documents -------------------------------------------
for f in ${REQUIRED}; do
	if [ ! -f "${ROOT}/${f}" ]; then
		echo "collect-licenses: FATAL: missing required license document: ${f}" >&2
		exit 1
	fi
	cp "${ROOT}/${f}" "${DEST}/${f}"
done

for f in ${OPTIONAL}; do
	[ -f "${ROOT}/${f}" ] && cp "${ROOT}/${f}" "${DEST}/${f}"
done

# --- per-font license texts -------------------------------------------------
if [ ! -d "${FONT_ROOT}" ]; then
	echo "collect-licenses: FATAL: font directory not found: ${FONT_ROOT}" >&2
	exit 1
fi

mkdir -p "${DEST}/licenses/fonts"

count=0
missing=""
for dir in "${FONT_ROOT}"/*/; do
	[ -d "${dir}" ] || continue
	name="$(basename "${dir}")"

	# Only fonts that are actually shipped need their license shipped.
	has_font=0
	for ext in ttf otf ttc woff2; do
		for candidate in "${dir}"*."${ext}"; do
			[ -f "${candidate}" ] && has_font=1 && break
		done
		[ "${has_font}" -eq 1 ] && break
	done
	[ "${has_font}" -eq 1 ] || continue

	# Accept either upstream naming convention.
	lic=""
	for cand in "${dir}OFL.txt" "${dir}LICENSE.txt" "${dir}LICENSE" "${dir}UFL.txt"; do
		[ -f "${cand}" ] && lic="${cand}" && break
	done

	if [ -z "${lic}" ]; then
		missing="${missing} ${name}"
		continue
	fi

	mkdir -p "${DEST}/licenses/fonts/${name}"
	cp "${lic}" "${DEST}/licenses/fonts/${name}/$(basename "${lic}")"
	count=$((count + 1))
done

if [ -n "${missing}" ]; then
	echo "collect-licenses: FATAL: bundled font(s) with no license text:${missing}" >&2
	echo "collect-licenses: OFL-1.1 s2 requires the license to accompany every copy." >&2
	exit 1
fi

if [ "${count}" -eq 0 ]; then
	echo "collect-licenses: FATAL: no font licenses collected — refusing to" >&2
	echo "collect-licenses: produce an artifact that claims complete licensing." >&2
	exit 1
fi

# --- bundled dictionary provenance ------------------------------------------
# The word list ships no separate license file; its provenance lives in the
# file's own header comment. Extract that verbatim rather than restating it, so
# the shipped attribution cannot drift from the shipped data.
if [ ! -f "${DICT}" ]; then
	echo "collect-licenses: FATAL: bundled dictionary not found: ${DICT}" >&2
	exit 1
fi

mkdir -p "${DEST}/licenses/dictionaries"
{
	echo "Provenance of the built-in en_US spell-check dictionary bundled in SCR1B3."
	echo "Compiled into the binary via include_str! (crates/scribe-core/src/spell.rs)."
	echo "Header reproduced verbatim from crates/scribe-core/assets/dict/en_US.txt:"
	echo
	sed -n '1,10p' "${DICT}" | grep '^#' || true
} > "${DEST}/licenses/dictionaries/en_US-PROVENANCE.txt"

# A short pointer so a user unpacking the archive knows what they are looking at.
cat > "${DEST}/licenses/README.txt" <<'EOF'
Third-party license texts for components embedded in the SCR1B3 binary.

fonts/         Per-typeface license text. SCR1B3 compiles these typefaces into
               the executable, so the SIL Open Font License 1.1 (section 2)
               requires this text to accompany every copy of the software.

dictionaries/  Provenance of the built-in spell-check word list, which is also
               compiled into the executable.

See THIRD-PARTY-LICENSES.md in the parent directory for the full index,
including each font's upstream source and copyright line.
EOF

echo "collect-licenses: staged ${count} font license(s) + dictionary provenance + ${DEST}/THIRD-PARTY-LICENSES.md"
