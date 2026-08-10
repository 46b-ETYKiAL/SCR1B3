#!/usr/bin/env python3
"""Falsification harness for `packaging/install.sh`.

`install.sh` is the one-line installer users pipe into `sh`. It is the single
place where a compromised or corrupted download becomes an executed binary, so
every one of its abort paths has to be *demonstrated*, not asserted in a commit
message. `sh -n` proves the script parses; it proves nothing about whether the
signature check can actually reject anything.

This harness runs the REAL `packaging/install.sh`, unmodified except for the
embedded public key (we do not hold the release private key), against generated
fixtures, under a PATH containing only stubs we control. The stubs implement
REAL minisign semantics on top of Ed25519 -- key-id match, signature over the
BLAKE2b-512 prehash, and the global signature over the trusted comment -- so a
tampered artifact genuinely fails verification rather than being assumed to.

Why a stub at all: `minisign` and `rsign` are frequently absent from CI images
and dev machines. A harness that shells out to a missing binary gets rc=127 on
every case and reports "every tamper was rejected -- perfect". That is the
failure mode this file is built to avoid, so:

  * a MUST-PASS control case ("good download installs") runs first and the whole
    run is declared INVALID if it fails -- a suite where nothing can succeed
    cannot be evidence that failures are being caught;
  * the stub verifier is unit-checked (accepts a good signature, rejects a
    tampered one, rejects a foreign key) BEFORE any install case runs;
  * rc=127 from any case is treated as a harness error, never as a pass.

Run:  python packaging/verify_install_sh.py [-v]
Exit: 0 all cases behaved as specified; 1 otherwise.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import os
import shutil
import subprocess
import sys
import tarfile
import tempfile
from dataclasses import dataclass, field
from pathlib import Path

try:
    from cryptography.exceptions import InvalidSignature
    from cryptography.hazmat.primitives.asymmetric import ed25519
except ImportError:  # pragma: no cover - environment gate
    sys.stderr.write(
        "error: this harness needs `cryptography` for real Ed25519 semantics.\n"
        "       install it, or run the harness on a host that has it. It is NOT\n"
        "       valid to skip these cases -- a skipped signature test is\n"
        "       indistinguishable from a passing one.\n"
    )
    raise SystemExit(2)

REPO = "46b-ETYKiAL/SCR1B3"
BASE_URL = f"https://github.com/{REPO}/releases/latest/download"
SIG_ALG_PREHASHED = b"ED"
SIG_ALG_LEGACY = b"Ed"


# --------------------------------------------------------------------------
# minisign reference implementation (sign + verify), used for BOTH the fixture
# producer and the stub verifier. Format per jedisct1/minisign:
#
#   pubkey file : line 1 comment; line 2 base64(alg[2] || key_id[8] || pk[32])
#   sig file    : line 1 untrusted comment
#                 line 2 base64(alg[2] || key_id[8] || sig[64])
#                 line 3 "trusted comment: <tc>"
#                 line 4 base64(global_sig[64])
#
#   sig        = Ed25519(sk, alg=="ED" ? blake2b512(content) : content)
#   global_sig = Ed25519(sk, sig || tc)
# --------------------------------------------------------------------------


class MinisignKey:
    """An Ed25519 keypair in minisign's on-disk encoding."""

    def __init__(self, seed: bytes) -> None:
        self.sk = ed25519.Ed25519PrivateKey.from_private_bytes(seed)
        self.pk_bytes = self.sk.public_key().public_bytes_raw()
        # minisign's key id is 8 arbitrary bytes; derive deterministically so
        # fixtures are reproducible across runs.
        self.key_id = hashlib.sha256(self.pk_bytes).digest()[:8]

    @property
    def key_id_display(self) -> str:
        """minisign prints the key id byte-reversed relative to the wire order."""
        return self.key_id[::-1].hex().upper()

    @property
    def pubkey_b64(self) -> str:
        return base64.b64encode(
            SIG_ALG_PREHASHED + self.key_id + self.pk_bytes
        ).decode()

    def pubkey_file(self) -> str:
        return (
            f"untrusted comment: minisign public key: {self.key_id_display}\n"
            f"{self.pubkey_b64}\n"
        )

    def sign(self, data: bytes, trusted_comment: str = "test fixture") -> str:
        msg = hashlib.blake2b(data, digest_size=64).digest()
        sig = self.sk.sign(msg)
        blob = SIG_ALG_PREHASHED + self.key_id + sig
        global_sig = self.sk.sign(sig + trusted_comment.encode())
        return (
            "untrusted comment: signature from minisign secret key\n"
            f"{base64.b64encode(blob).decode()}\n"
            f"trusted comment: {trusted_comment}\n"
            f"{base64.b64encode(global_sig).decode()}\n"
        )


def minisign_verify(pubkey_text: str, sig_text: str, data: bytes) -> bool:
    """Verify a detached minisign signature. Returns True only on full success."""
    try:
        pub_lines = [ln for ln in pubkey_text.splitlines() if ln.strip()]
        pub_blob = base64.b64decode(pub_lines[1])
        if len(pub_blob) != 42:
            return False
        pub_key_id, pk_bytes = pub_blob[2:10], pub_blob[10:]

        sig_lines = sig_text.splitlines()
        if len(sig_lines) < 4:
            return False
        sig_blob = base64.b64decode(sig_lines[1])
        if len(sig_blob) != 74:
            return False
        alg, sig_key_id, sig = sig_blob[:2], sig_blob[2:10], sig_blob[10:]

        # An attacker's signature made with a DIFFERENT key must be rejected
        # here even though it is internally self-consistent.
        if sig_key_id != pub_key_id:
            return False

        tc_line = sig_lines[2]
        prefix = "trusted comment: "
        if not tc_line.startswith(prefix):
            return False
        trusted_comment = tc_line[len(prefix) :]
        global_sig = base64.b64decode(sig_lines[3])

        if alg == SIG_ALG_PREHASHED:
            msg = hashlib.blake2b(data, digest_size=64).digest()
        elif alg == SIG_ALG_LEGACY:
            msg = data
        else:
            return False

        pk = ed25519.Ed25519PublicKey.from_public_bytes(pk_bytes)
        pk.verify(sig, msg)
        pk.verify(global_sig, sig + trusted_comment.encode())
        return True
    except (InvalidSignature, ValueError, IndexError, TypeError):
        return False


# --------------------------------------------------------------------------
# Stub entry points (this same file is re-invoked as `minisign` / `rsign` /
# `curl` / `uname` by the generated PATH shims).
# --------------------------------------------------------------------------


def stub_minisign(argv: list[str]) -> int:
    """`minisign -V -p <pub> -x <sig> -m <file>`."""
    pub = sig = target = None
    i = 0
    while i < len(argv):
        if argv[i] == "-p":
            pub = argv[i + 1]
            i += 2
        elif argv[i] == "-x":
            sig = argv[i + 1]
            i += 2
        elif argv[i] == "-m":
            target = argv[i + 1]
            i += 2
        else:
            i += 1
    if not (pub and sig and target):
        sys.stderr.write("stub-minisign: missing -p/-x/-m\n")
        return 2
    ok = minisign_verify(
        Path(pub).read_text(), Path(sig).read_text(), Path(target).read_bytes()
    )
    if not ok:
        sys.stderr.write("Signature verification failed\n")
        return 1
    return 0


def stub_rsign(argv: list[str]) -> int:
    """`rsign verify -p <pub> -x <sig> <file>`."""
    if not argv or argv[0] != "verify":
        return 2
    argv = argv[1:]
    pub = sig = None
    positional: list[str] = []
    i = 0
    while i < len(argv):
        if argv[i] == "-p":
            pub = argv[i + 1]
            i += 2
        elif argv[i] == "-x":
            sig = argv[i + 1]
            i += 2
        else:
            positional.append(argv[i])
            i += 1
    if not (pub and sig and positional):
        return 2
    ok = minisign_verify(
        Path(pub).read_text(), Path(sig).read_text(), Path(positional[0]).read_bytes()
    )
    if not ok:
        sys.stderr.write("Signature verification failed\n")
        return 1
    return 0


def stub_curl(argv: list[str]) -> int:
    """`curl -fsSL <url> -o <dest>` served from $HARNESS_RELEASE_DIR.

    Mimics `curl -f`: any URL that does not resolve to a published asset exits
    22 and writes nothing. The expected URL prefix is asserted, so reverting the
    repo coordinate (or the branch token) produces a 404 here exactly as it
    would in production.
    """
    url = dest = None
    i = 0
    while i < len(argv):
        if argv[i] == "-o":
            dest = argv[i + 1]
            i += 2
        elif argv[i].startswith("-"):
            i += 1
        else:
            url = argv[i]
            i += 1
    if not url or not dest:
        return 2

    expected = os.environ.get("HARNESS_BASE_URL", BASE_URL)
    if not url.startswith(expected + "/"):
        sys.stderr.write(
            f"stub-curl: 404 (url is not under the published release base)\n"
            f"  got:      {url}\n  expected: {expected}/...\n"
        )
        return 22

    name = url[len(expected) + 1 :]
    src = Path(os.environ["HARNESS_RELEASE_DIR"]) / name
    if not src.is_file():
        sys.stderr.write(f"stub-curl: 404 {name}\n")
        return 22
    shutil.copyfile(src, dest)
    return 0


def stub_uname(argv: list[str]) -> int:
    if "-s" in argv:
        sys.stdout.write(os.environ.get("HARNESS_UNAME_S", "Linux") + "\n")
    elif "-m" in argv:
        sys.stdout.write(os.environ.get("HARNESS_UNAME_M", "x86_64") + "\n")
    else:
        sys.stdout.write(os.environ.get("HARNESS_UNAME_S", "Linux") + "\n")
    return 0


STUBS = {
    "--stub-minisign": stub_minisign,
    "--stub-rsign": stub_rsign,
    "--stub-curl": stub_curl,
    "--stub-uname": stub_uname,
}


# --------------------------------------------------------------------------
# Fixture + environment construction
# --------------------------------------------------------------------------

# `gzip` is here because GNU tar forks it for `-z`; without it the control fails
# at extraction AFTER a fully successful verification, which would read as
# "verification blocked a good download".
PASSTHROUGH_TOOLS = (
    "mktemp",
    "awk",
    "tar",
    "gzip",
    "install",
    "mkdir",
    "cp",
    "rm",
    "sed",
)
# Tools whose PRESENCE is what a case is testing -- created only on request.
OPTIONAL_TOOLS = ("sha256sum", "shasum", "minisign", "rsign")


def sha256_hex(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def build_artifact(dest: Path, marker: bytes) -> None:
    """A tarball shaped like the real release: binary at the archive root."""
    stage = dest.parent / "stage"
    shutil.rmtree(stage, ignore_errors=True)
    (stage / "licenses").mkdir(parents=True)
    (stage / "scr1b3").write_bytes(b"#!/bin/sh\necho scr1b3\n" + marker)
    (stage / "LICENSE-MIT").write_text("MIT\n")
    (stage / "THIRD-PARTY-LICENSES.md").write_text("# third party\n")
    (stage / "licenses" / "OFL-1.1.txt").write_text("OFL\n")
    with tarfile.open(dest, "w:gz") as tf:
        for member in ("scr1b3", "LICENSE-MIT", "THIRD-PARTY-LICENSES.md", "licenses"):
            tf.add(stage / member, arcname=member)
    shutil.rmtree(stage, ignore_errors=True)


@dataclass
class Release:
    """A generated release directory the stub `curl` serves from."""

    root: Path
    key: MinisignKey
    asset: str

    def build(
        self,
        *,
        artifact_marker: bytes = b"genuine",
        sign_key: MinisignKey | None = None,
        manifest_key: MinisignKey | None = None,
        manifest_matches_tampered: bool = False,
        omit: tuple[str, ...] = (),
        empty: tuple[str, ...] = (),
        corrupt_manifest_digest: bool = False,
        manifest_asset_name: str | None = None,
    ) -> None:
        """Stage a release, optionally tampered.

        The genuine artifact is built and signed FIRST, then optionally
        overwritten with tampered bytes. That ordering matters: a gzip stream
        embeds an mtime, so rebuilding a byte-identical "pristine" copy to sign
        after the fact does not reproduce the original digest and every case
        would fail the artifact signature -- including the control. Signing the
        real bytes once, up front, is what makes the tampered signature
        genuinely stale rather than merely different.
        """
        self.root.mkdir(parents=True, exist_ok=True)
        sign_key = sign_key or self.key
        manifest_key = manifest_key or self.key

        art = self.root / self.asset
        build_artifact(art, b"genuine")
        genuine_digest = sha256_hex(art)

        # Signed over the GENUINE bytes -- so a later tamper leaves this stale.
        (self.root / f"{self.asset}.minisig").write_text(
            sign_key.sign(art.read_bytes()), newline="\n"
        )

        tampered = artifact_marker != b"genuine"
        if tampered:
            build_artifact(art, artifact_marker)

        # Default: the manifest holds the GENUINE digest, so a tamper trips the
        # checksum. `manifest_matches_tampered` instead points the manifest at
        # the tampered bytes, letting the checksum pass so the per-artifact
        # signature check is the thing under test.
        listed = manifest_asset_name or self.asset
        digest = sha256_hex(art) if manifest_matches_tampered else genuine_digest
        if corrupt_manifest_digest:
            digest = "0" * 64
        manifest = f"{digest}  {listed}\n"
        (self.root / "SHA256SUMS").write_text(manifest, newline="\n")
        (self.root / "SHA256SUMS.minisig").write_text(
            manifest_key.sign(manifest.encode()), newline="\n"
        )

        for name in omit:
            (self.root / name).unlink(missing_ok=True)
        for name in empty:
            (self.root / name).write_bytes(b"")


def which(tool: str) -> str | None:
    """`shutil.which` plus the dirs a native Python misses on a Git-Bash host.

    `shasum` lives in `/usr/bin/core_perl` under Git for Windows, which is not on
    the PATH a Windows-native Python inherits. Missing it would silently drop the
    macOS control case -- and a control that never runs is the failure this
    harness is built to make impossible, so resolve it explicitly.
    """
    found = shutil.which(tool)
    if found:
        return found
    extra = [
        Path(r"C:\Program Files\Git\usr\bin"),
        Path(r"C:\Program Files\Git\usr\bin\core_perl"),
        Path("/usr/bin"),
        Path("/usr/bin/core_perl"),
        Path("/bin"),
    ]
    for d in extra:
        for cand in (d / tool, d / f"{tool}.exe"):
            if cand.is_file():
                return str(cand)
    return None


def shpath(p: str) -> str:
    """A path a POSIX `sh` can exec, even when Python hands us a Windows one."""
    return str(p).replace("\\", "/")


def find_sh() -> str:
    found = shutil.which("sh") or shutil.which("bash")
    if found:
        return found
    for cand in (
        r"C:\Program Files\Git\usr\bin\sh.exe",
        r"C:\Program Files\Git\bin\sh.exe",
        "/bin/sh",
        "/usr/bin/sh",
    ):
        if Path(cand).is_file():
            return cand
    raise RuntimeError("no POSIX sh found to run install.sh under")


def write_shim(path: Path, body: str) -> None:
    path.write_text(body, newline="\n")
    path.chmod(0o755)


def build_fakebin(
    bindir: Path, python: str, harness: str, tools: tuple[str, ...]
) -> None:
    """A PATH containing ONLY what a case declares, so absence is real absence."""
    bindir.mkdir(parents=True, exist_ok=True)
    for tool in PASSTHROUGH_TOOLS:
        real = which(tool)
        if real:
            write_shim(bindir / tool, f'#!/bin/sh\nexec "{shpath(real)}" "$@"\n')
    for tool in ("sha256sum", "shasum"):
        if tool in tools:
            real = which(tool)
            if not real:
                raise RuntimeError(f"host lacks {tool}; cannot build this case")
            write_shim(bindir / tool, f'#!/bin/sh\nexec "{shpath(real)}" "$@"\n')
    for tool, flag in (
        ("minisign", "--stub-minisign"),
        ("rsign", "--stub-rsign"),
        ("curl", "--stub-curl"),
        ("uname", "--stub-uname"),
    ):
        if tool in tools:
            write_shim(
                bindir / tool,
                f'#!/bin/sh\nexec "{shpath(python)}" "{shpath(harness)}" {flag} "$@"\n',
            )


def patch_pubkey(src: Path, dest: Path, key: MinisignKey) -> None:
    """Swap ONLY the embedded key material; assert nothing else changed.

    We do not hold the release private key, so the harness must substitute a
    test key. That substitution is the one and only edit -- if it silently
    failed we would be verifying against the production key with test fixtures
    and every case would 'correctly' abort, which is the same fake-green shape
    this harness exists to prevent. So both lines are asserted to have changed.
    """
    out, id_hits, key_hits = [], 0, 0
    for line in src.read_text().splitlines():
        if line.startswith("PUBKEY_ID="):
            out.append(f'PUBKEY_ID="{key.key_id_display}"')
            id_hits += 1
        elif line.startswith("PUBKEY="):
            out.append(f'PUBKEY="{key.pubkey_b64}"')
            key_hits += 1
        else:
            out.append(line)
    if id_hits != 1 or key_hits != 1:
        raise RuntimeError(
            f"key substitution did not apply (PUBKEY_ID x{id_hits}, PUBKEY x{key_hits})"
            " -- install.sh's key lines changed shape; the harness would otherwise"
            " report vacuous passes."
        )
    dest.write_text("\n".join(out) + "\n", newline="\n")


@dataclass
class Case:
    name: str
    why: str
    expect_install: bool
    expect_text: str = ""
    tools: tuple[str, ...] = ("minisign", "sha256sum", "curl", "uname")
    uname_s: str = "Linux"
    uname_m: str = "x86_64"
    build: dict = field(default_factory=dict)
    mutate_script: tuple[str, str] | None = None
    asset: str = "scr1b3-x86_64-unknown-linux-gnu.tar.gz"


def run_case(case: Case, install_sh: Path, python: str, harness: str, verbose: bool):
    sandbox = Path(tempfile.mkdtemp(prefix=f"instv-{case.name}-"))
    try:
        key = MinisignKey(hashlib.sha256(b"scr1b3-release-test-key").digest())
        attacker = MinisignKey(hashlib.sha256(b"attacker-key").digest())

        build_kwargs = dict(case.build)
        for slot in ("sign_key", "manifest_key"):
            if build_kwargs.get(slot) == "attacker":
                build_kwargs[slot] = attacker

        release = Release(sandbox / "release", key, case.asset)
        release.build(**build_kwargs)

        script = sandbox / "install.sh"
        patch_pubkey(install_sh, script, key)
        if case.mutate_script:
            old, new = case.mutate_script
            text = script.read_text()
            if old not in text:
                return None, f"mutation target {old!r} not found in install.sh"
            script.write_text(text.replace(old, new, 1), newline="\n")

        bindir = sandbox / "bin"
        build_fakebin(bindir, python, harness, case.tools)

        home = sandbox / "home"
        home.mkdir()
        env = {
            "PATH": str(bindir),
            "HOME": str(home),
            "HARNESS_RELEASE_DIR": str(release.root),
            "HARNESS_BASE_URL": BASE_URL,
            "HARNESS_UNAME_S": case.uname_s,
            "HARNESS_UNAME_M": case.uname_m,
            "TMPDIR": str(sandbox / "tmp"),
            "SYSTEMROOT": os.environ.get("SYSTEMROOT", ""),
        }
        (sandbox / "tmp").mkdir()

        proc = subprocess.run(
            [find_sh(), shpath(str(script))],
            env=env,
            capture_output=True,
            text=True,
            timeout=120,
        )
        installed = (home / ".local" / "bin" / "scr1b3").is_file()
        combined = proc.stdout + proc.stderr

        if verbose:
            print(f"    rc={proc.returncode} installed={installed}")
            for line in combined.splitlines():
                print(f"    | {line}")

        # rc 127 means a stub was never found. Treat as a harness fault, never a
        # pass -- this is the exact trap that makes an absent tool look like a
        # perfect blocker.
        if proc.returncode == 127:
            return None, "rc=127 (command not found) -- stub PATH is broken"

        if case.expect_install:
            if not installed:
                return False, f"expected an install, got rc={proc.returncode}"
            if proc.returncode != 0:
                return False, f"binary installed but rc={proc.returncode}"
        else:
            if installed:
                return False, "UNVERIFIED BINARY WAS INSTALLED"
            if proc.returncode == 0:
                return False, "aborted-case exited 0"
            if case.expect_text and case.expect_text.lower() not in combined.lower():
                return False, f"missing expected message {case.expect_text!r}"
            if not combined.strip():
                return False, "aborted silently with no message"
        return True, ""
    finally:
        shutil.rmtree(sandbox, ignore_errors=True)


CASES = [
    # ---- MUST-PASS controls -------------------------------------------------
    Case(
        "control_good_install",
        "CONTROL: a genuine signed release installs. If this fails the suite is "
        "INVALID -- nothing below proves rejection.",
        expect_install=True,
    ),
    Case(
        "control_macos_shasum_only",
        "CONTROL: macOS ships shasum, not sha256sum. Verification must still run.",
        expect_install=True,
        tools=("minisign", "shasum", "curl", "uname"),
    ),
    Case(
        "control_darwin_target",
        "CONTROL: the Darwin/arm64 leg resolves and installs.",
        expect_install=True,
        uname_s="Darwin",
        uname_m="arm64",
        asset="scr1b3-aarch64-apple-darwin.tar.gz",
        tools=("minisign", "shasum", "curl", "uname"),
    ),
    Case(
        "control_rsign_verifier",
        "CONTROL: the rsign fallback branch verifies too.",
        expect_install=True,
        tools=("rsign", "sha256sum", "curl", "uname"),
    ),
    # ---- tamper -------------------------------------------------------------
    Case(
        "tampered_artifact",
        "A modified tarball must fail the signed digest.",
        expect_install=False,
        expect_text="checksum mismatch",
        build={"artifact_marker": b"MALICIOUS"},
    ),
    Case(
        "tampered_artifact_matching_manifest",
        "Tamper WITH a matching manifest: isolates the per-artifact signature, "
        "proving that check is load-bearing and not shadowed by the checksum.",
        expect_install=False,
        expect_text="signature verification failed",
        build={"artifact_marker": b"MALICIOUS", "manifest_matches_tampered": True},
    ),
    Case(
        "tampered_manifest_digest",
        "An edited SHA256SUMS with a stale signature must fail authenticity.",
        expect_install=False,
        expect_text="sha256sums signature verification failed",
        build={"corrupt_manifest_digest": True},
    ),
    Case(
        "foreign_signing_key",
        "A wholly valid release signed by an ATTACKER key must be rejected -- "
        "proves the embedded PUBKEY is actually consulted.",
        expect_install=False,
        expect_text="signature verification failed",
        build={"sign_key": "attacker", "manifest_key": "attacker"},
    ),
    # ---- missing inputs -----------------------------------------------------
    Case(
        "missing_artifact_signature",
        "No <asset>.minisig must abort, not skip.",
        expect_install=False,
        expect_text="no .minisig published",
        build={"omit": ("scr1b3-x86_64-unknown-linux-gnu.tar.gz.minisig",)},
    ),
    Case(
        "missing_sha256sums",
        "No SHA256SUMS must abort (the fail-closed cost of the manifest choice).",
        expect_install=False,
        expect_text="no sha256sums published",
        build={"omit": ("SHA256SUMS",)},
    ),
    Case(
        "missing_sha256sums_signature",
        "An unsigned checksum manifest must not be trusted.",
        expect_install=False,
        expect_text="no sha256sums.minisig published",
        build={"omit": ("SHA256SUMS.minisig",)},
    ),
    Case(
        "empty_sha256sums",
        "A zero-byte manifest satisfies `[ -f ]`; the `[ -s ]` guard must catch it.",
        expect_install=False,
        expect_text="empty",
        build={"empty": ("SHA256SUMS",)},
    ),
    Case(
        "asset_absent_from_manifest",
        "A manifest with no line for this asset must abort.",
        expect_install=False,
        expect_text="no entry for",
        build={"manifest_asset_name": "scr1b3-some-other-target.tar.gz"},
    ),
    Case(
        "artifact_404",
        "A missing artifact must abort with a message naming the URL.",
        expect_install=False,
        expect_text="download failed",
        build={"omit": ("scr1b3-x86_64-unknown-linux-gnu.tar.gz",)},
    ),
    # ---- missing tools ------------------------------------------------------
    Case(
        "no_signature_verifier",
        "No minisign AND no rsign must abort with instructions, never degrade.",
        expect_install=False,
        expect_text="no signature verifier",
        tools=("sha256sum", "curl", "uname"),
    ),
    Case(
        "no_sha256_tool",
        "Neither sha256sum nor shasum must abort, not silently skip (the exact "
        "macOS defect on origin/master).",
        expect_install=False,
        expect_text="no sha256 tool",
        tools=("minisign", "curl", "uname"),
    ),
    # ---- mutants: prove the URL coordinates are load-bearing -----------------
    Case(
        "mutant_stale_repo_name",
        "MUTANT: reverting to the pre-rename repo must 404, proving the repo "
        "coordinate is exercised rather than cosmetic.",
        expect_install=False,
        expect_text="download failed",
        mutate_script=('REPO="46b-ETYKiAL/SCR1B3"', 'REPO="46b-ETYKiAL/OLD-NAME"'),
    ),
]


def unit_check_stub() -> list[str]:
    """Prove the stub verifier can both accept and reject before it is trusted."""
    problems = []
    key = MinisignKey(hashlib.sha256(b"unit").digest())
    other = MinisignKey(hashlib.sha256(b"unit-other").digest())
    data = b"payload bytes"
    sig = key.sign(data)

    if not minisign_verify(key.pubkey_file(), sig, data):
        problems.append("stub rejected a GOOD signature (it can never pass)")
    if minisign_verify(key.pubkey_file(), sig, b"payload bytez"):
        problems.append("stub accepted TAMPERED content (it can never fail)")
    if minisign_verify(other.pubkey_file(), sig, data):
        problems.append("stub accepted a FOREIGN key (key id not enforced)")
    bad = sig.replace("trusted comment: test fixture", "trusted comment: forged")
    if minisign_verify(key.pubkey_file(), bad, data):
        problems.append("stub accepted a forged trusted comment (global sig unchecked)")
    return problems


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("-v", "--verbose", action="store_true")
    ap.add_argument("--only", default=None, help="run a single case by name")
    args, _ = ap.parse_known_args()

    harness = str(Path(__file__).resolve())
    install_sh = Path(__file__).resolve().parent / "install.sh"
    if not install_sh.is_file():
        print(f"error: {install_sh} not found", file=sys.stderr)
        return 2
    python = sys.executable

    print("== stub self-check (must both accept and reject) ==")
    problems = unit_check_stub()
    for p in problems:
        print(f"  FAIL {p}")
    if problems:
        print("\nINVALID: the stub verifier cannot discriminate. No case below is evidence.")
        return 1
    print("  ok  accepts good / rejects tampered / rejects foreign key / rejects forged comment\n")

    print("== install.sh cases ==")
    failures, errors, control_failed = [], [], False
    selected = [c for c in CASES if not args.only or c.name == args.only]
    for case in selected:
        ok, detail = run_case(case, install_sh, python, harness, args.verbose)
        if ok is None:
            print(f"  ERROR {case.name}: {detail}")
            errors.append(case.name)
        elif ok:
            print(f"  PASS  {case.name}")
        else:
            print(f"  FAIL  {case.name}: {detail}")
            failures.append(case.name)
            if case.name.startswith("control_"):
                control_failed = True

    print()
    if control_failed:
        print(
            "INVALID: a MUST-PASS control failed. A suite in which nothing can "
            "install cannot be evidence that tampering is rejected."
        )
        return 1
    if errors:
        print(f"HARNESS ERROR in {len(errors)} case(s): {', '.join(errors)}")
        return 1
    if failures:
        print(f"FAILED {len(failures)}/{len(selected)}: {', '.join(failures)}")
        return 1
    print(f"OK {len(selected)}/{len(selected)} cases behaved as specified.")
    return 0


if __name__ == "__main__":
    for flag, fn in STUBS.items():
        if len(sys.argv) > 1 and sys.argv[1] == flag:
            raise SystemExit(fn(sys.argv[2:]))
    raise SystemExit(main())
