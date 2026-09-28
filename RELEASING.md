# Release guide

This guide describes the manual publication flow. Do not publish credentials,
SSH keys, device addresses, or private build artifacts.

The supported host environment is macOS 26 Tahoe or later on Apple Silicon.
Do not describe Intel Macs or earlier macOS releases as supported in release
notes or the Homebrew formula.

## macOS signing policy

The initial Homebrew source-build release does not require macOS code signing
or notarization. If this project later distributes prebuilt macOS binaries,
each distributed binary must be signed with a Developer ID and notarized by
Apple before publication.

## Initial repository publication

Create `karad/yuge-hasi-dev` on GitHub, then run these commands locally after
reviewing the working tree:

```sh
git add -A
git commit -m "Prepare Yuge Hasi Devkit for public release"
git branch -M main
git remote add origin https://github.com/karad/yuge-hasi-dev.git
git push -u origin main
```

## First source release

1. Update the workspace version and the `Unreleased` section in `CHANGELOG.md`.
   Follow Semantic Versioning: increment MAJOR for incompatible command or
   behavior changes, MINOR for backward-compatible features, and PATCH for
   backward-compatible fixes.
2. Run `sh scripts/build-devkit.sh`.
3. Commit the version change, create an annotated tag such as `v0.1.0`, and
   push the commit and tag.

The initial release does not publish a GitHub Release page or prebuilt macOS
binaries. The public tag is sufficient: GitHub provides its source archive, and
the Homebrew formula below fetches that archive and builds the CLI locally.

## Initial release flow

```text
Publish karad/yuge-hasi-dev with a clean initial commit
    -> Push the v0.1.0 tag
    -> Download that tag's source archive and calculate its SHA-256
    -> Commit Formula/yuge-hasi.rb to karad/homebrew-yuge-hasi
    -> Run brew audit and verify brew install in a clean environment
```

The source archive checksum belongs in the Homebrew formula. A checksum for a
distributed macOS binary is not needed until this project publishes prebuilt
binaries or bottles.

## Homebrew tap

Create the separate repository `karad/homebrew-yuge-hasi`. Its first commit
contains `Formula/yuge-hasi.rb`. Replace `VERSION` and `SHA256` after
the source release is published:

```ruby
class YugeHasi < Formula
  desc "Deploy game builds from macOS to SteamOS developer devices"
  homepage "https://github.com/karad/yuge-hasi-dev"
  url "https://github.com/karad/yuge-hasi-dev/archive/refs/tags/vVERSION.tar.gz"
  sha256 "SHA256"
  license "MIT"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args(path: "crates/devkit")
  end

  test do
    assert_match "SteamOS development", shell_output("#{bin}/yuge-hasi devkit --help")
  end
end
```

Calculate the archive checksum with `shasum -a 256 <downloaded-archive>`, then
run `brew audit --strict --online karad/yuge-hasi` and install
it in a clean environment before publishing the tap commit.

## Codex Plugin

After the tag is public, users add the marketplace with:

```sh
codex plugin marketplace add karad/yuge-hasi-dev --ref vVERSION --sparse .agents/plugins
```

They then choose **Yuge Hasi Devkit** in the Codex Plugins screen and select
**Install**. Test this process from a clean Codex profile before announcing the
release.
