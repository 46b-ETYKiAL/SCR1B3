# Homebrew cask for SCR1B3. Install: brew install --cask scr1b3
# (self-update is disabled in cask installs; `brew upgrade` owns updates.)
#
# The URL filename must match what release.yml's `macos-installer` job actually
# uploads — `dist/scr1b3-${TAG}-${arch}.dmg`, i.e. `scr1b3-v0.4.62-aarch64.dmg`.
# It previously read `scr1b3-aarch64-apple-darwin.dmg`, the TARBALL's target
# triple, which no release has ever published: `brew install --cask scr1b3`
# 404'd on every version. Verified against
# `gh release view v0.4.62 --repo 46b-ETYKiAL/SCR1B3 --json assets`.
#
# `version` + `sha256` are pinned to a REAL release rather than a `0.1.0`
# placeholder with a zeroed hash, so this cask installs instead of failing its
# checksum. The hash is the one in the SIGNED release sidecar
# (scr1b3-v0.4.62-aarch64.dmg.sha256), which matches the GitHub asset digest.
cask "scr1b3" do
  version "0.4.62"
  sha256 "7879449ba8caccbdf0f9c8b05e65d217fe938fc16835c023220837ba53ce6ab1"

  url "https://github.com/46b-ETYKiAL/SCR1B3/releases/download/v#{version}/scr1b3-v#{version}-aarch64.dmg"
  name "SCR1B3"
  desc "Fast, telemetry-free, cross-platform code/text editor"
  homepage "https://github.com/46b-ETYKiAL/SCR1B3"

  # v0.4.62 published an aarch64 .dmg ONLY. Declaring the constraint makes the
  # cask refuse on Intel with an accurate message instead of downloading an
  # arm64 bundle onto a machine that cannot run it. The `macos-installer` job is
  # now matrixed over both arches, so this can become an on_arm/on_intel pair as
  # soon as a release publishes an x86_64 .dmg — not before.
  depends_on arch: :arm64

  app "SCR1B3.app"

  zap trash: [
    "~/Library/Application Support/com.itashacorp.scr1b3",
  ]
end
