# Contributing to SCR1B3

Thanks for your interest in contributing. SCR1B3 is a fast, telemetry-free, cross-platform editor — contributions that keep it that way (small, native, privacy-respecting, not bloated) are very welcome.

## Project layout

SCR1B3 is a Cargo workspace:

```
scr1b3/
├── crates/
│   ├── scribe-core         # engine: rope buffer, file I/O + mmap, encoding/EOL,
│   │                       #         config, theme, syntax highlighting
│   │                       #         (tree-sitter primary, syntect fallback),
│   │                       #         search, update logic. No UI dependency.
│   ├── scribe-render       # maps the engine Theme onto egui Visuals; CRT params.
│   ├── scribe-app          # the binary: egui/eframe shell, tabs, find bar,
│   │                       #             status bar, frameless titlebar.
│   └── scribe-win32-chrome # Win32 FFI for the frameless titlebar (Windows).
│                           # The main unsafe crate; scribe-core additionally
│                           # permits a single documented mmap unsafe (deny + one
│                           # allow), while scribe-render and scribe-app forbid unsafe.
├── assets/            # SVG identity, bundled themes, fonts, media
├── docs/adr/          # architecture decision records
└── packaging/         # per-OS install recipes
```

The **core / render / app** seam is deliberate: `scribe-core` is the replaceable engine with a clean public API and no UI dependency. Keep UI-specific code out of `scribe-core`.

## Prerequisites

- A [Rust toolchain](https://rustup.rs/). The exact version is pinned in `rust-toolchain.toml` and will be installed automatically by `rustup`.
- Recommended dev tools:
  ```bash
  cargo install cargo-nextest   # fast test runner
  cargo install cargo-audit     # advisory scanning
  cargo install cargo-deny      # license + advisory gate
  ```

## Before your first push: install the git hooks

```bash
sh scripts/install-git-hooks.sh
```

This repository is public, so anything you push is published immediately.
Commit **metadata** is the part that cannot be taken back: the author and
committer addresses on a pushed commit are visible the moment the push lands,
and clearing them afterwards requires rewriting history for everyone. The
pre-push hook is what prevents that; the CI job is the backstop for a push made
without it.

The hook refuses a push when:

- either guard fails its own falsification suite (checked **first**, so a guard
  that has silently stopped detecting anything cannot let the rest report a
  clean pass);
- a tracked file carries an absolute home path, a personal mailbox, an internal
  tooling reference, or a secret-shaped string;
- any commit in the range being pushed carries an identity that is not on the
  allowlist, or a **name field** that carries something the content-safety
  audit refuses anywhere else.

Set a publishing identity before you commit:

```bash
git config user.email '<id>+<handle>@users.noreply.github.com'
git config user.name  '<handle>'
```

GitHub issues that address under **Settings → Emails → Keep my email address
private**. Your **name** is welcome in commits and in the contributor list —
real names, handles and pseudonyms all pass, and it is the mailbox that must
stay out. Contributions from a bot or forge noreply address are equally fine.

The one thing a name field must not carry is content that is not attribution
at all: a **workstation account name**, a home path, or an internal token.
`git` fills `user.name` from your OS account by default, so this is easy to
publish by accident and the address rules cannot see it — set `user.name`
explicitly and it never arises.

Vendor- and forge-issued **no-reply** addresses (`noreply@github.com`,
`<handle>@users.noreply.github.com`, `noreply@anthropic.com`) are not personal
mailboxes and are not findings; the audit reports them as their own class with
a count. An ordinary local part at one of those domains *is* a real inbox and
is still a finding.

Run the checks yourself at any time:

```bash
python3 scripts/test_content_safety_audit.py   # falsify the audit
python3 scripts/test_author_identity_guard.py  # falsify the identity guard
python3 scripts/content_safety_audit.py        # audit tracked files
python3 scripts/content_safety_audit.py --history   # + commit metadata (reporting)
```

`--no-verify` bypasses the hook. Use it only when you have confirmed by other
means that the push carries nothing personal — not to skip a finding you have
not read, since the audit prints the file and line for every one.

## Build

```bash
cargo build              # debug
cargo build --release    # optimized, stripped binary
cargo run -- path/to/file.txt   # run, optionally opening a file
```

## Test

The test runner is [`cargo-nextest`](https://nexte.st/):

```bash
cargo nextest run        # whole workspace
```

Plain `cargo test` also works if you don't have nextest installed. Every new behavior should ship with a test. The engine crates favor pure, offline-testable logic (for example, update version-comparison is tested without touching the network).

## Format & lint

These must pass before a PR is merged:

```bash
cargo fmt --check          # formatting
cargo clippy -- -D warnings   # lint; warnings are errors
```

Do not silence clippy with `#[allow(...)]` without a one-line justification comment explaining why.

## Security & dependencies

```bash
cargo audit     # known-vulnerability scan
cargo deny check   # license + advisory policy
```

- All dependencies are pinned via `Cargo.lock`; commit lock changes alongside `Cargo.toml` changes.
- Prefer the standard library and small, well-maintained crates. SCR1B3 has no embedded webview and no paid/cloud dependencies, and that is a design constraint, not an accident.
- The only permitted network surface is the telemetry-free update check. Do not add code that transmits file contents, usage data, or PII. See [SECURITY.md](SECURITY.md).

## Pull request conventions

1. **Branch** from the default branch; use a short descriptive name (`feat/...`, `fix/...`, `docs/...`).
2. **Commits** follow [Conventional Commits](https://www.conventionalcommits.org/): `feat:`, `fix:`, `docs:`, `refactor:`, `test:`, `chore:`.
3. **Keep PRs focused** — one logical change per PR. Files stay small and single-purpose where practical.
4. **Tests + green checks** — `cargo nextest run`, `cargo fmt --check`, `cargo clippy -D warnings`, `cargo audit`, and `cargo deny check` all pass.
5. **Document decisions** — significant architectural changes get an ADR under `docs/adr/` (see the existing records for the format).
6. **No telemetry, no bloat** — changes that add tracking, a system webview, or a heavy runtime will be declined.

## Architecture decisions

Read the [ADRs](docs/adr/) before proposing structural changes — they explain why the stack is Rust + egui/wgpu, why the config is TOML, why both syntax engines ship in v1 (tree-sitter primary where a native grammar is wired, syntect as the pure-Rust fallback), and how the telemetry-free auto-update is designed.

## Code of conduct

Be respectful, assume good faith, and keep discussion technical. Harassment or discrimination is not tolerated. By participating you agree to uphold a welcoming, inclusive environment for everyone. Report conduct concerns to the maintainers via a private channel (security/abuse contact in [SECURITY.md](SECURITY.md)).

## License

By contributing, you agree that your contributions are dual-licensed under **MIT OR Apache-2.0**, matching the project license, with no additional terms.
