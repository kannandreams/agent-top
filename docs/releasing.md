# Releasing

A release is a git tag. `.github/workflows/release.yml` does the rest: it
builds four binaries, publishes them with checksums, rewrites the Homebrew
formula and pushes it to the tap.

## One-time setup

These three things are not automated because each is a deliberate, public act.

1. **Make the repository public.** Homebrew fetches release assets anonymously;
   a private repository means every `brew install` gets a 404. (The
   `agent-top-internal-docs` repository stays private — nothing in the release
   path reads it.)

2. **Create the tap.** A tap is an ordinary repository whose name starts with
   `homebrew-`:

   ```sh
   gh repo create kannandreams/homebrew-tap --public \
     --description "Homebrew formulae for kannandreams' tools"
   ```

   `kannandreams/tap` is how users will refer to it; `homebrew-` is implied.
   The workflow creates `Formula/agent-top.rb` on the first release, so the
   repository can start empty apart from a README.

3. **Add the tokens** as repository secrets on `agent-top`:

   | Secret | Needed for | Scope |
   |---|---|---|
   | `HOMEBREW_TAP_TOKEN` | pushing the formula to the tap | a fine-grained PAT with **Contents: read and write** on `kannandreams/homebrew-tap` only |
   | `CARGO_REGISTRY_TOKEN` | `cargo install agent-top` from crates.io | a crates.io API token with publish scope |

   Both are optional. If a secret is missing the workflow logs a warning and
   skips that step; the GitHub release and its binaries still happen.

## Cutting a release

```sh
# 1. Bump the workspace version. The workflow refuses to release a tag that
#    disagrees with Cargo.toml.
$EDITOR Cargo.toml            # [workspace.package] version = "0.2.0"
cargo build                   # refresh Cargo.lock

# 2. Move the CHANGELOG's [Unreleased] heading to "## [0.2.0] - YYYY-MM-DD".
#    The release notes are the first section of that file, verbatim.
$EDITOR CHANGELOG.md

# 3. Check it the way CI will.
mise run check

# 4. Tag and push.
git commit -am "release: v0.2.0"
git tag v0.2.0
git push && git push --tags
```

Then watch `gh run watch`, and verify the result on a clean machine:

```sh
brew install kannandreams/tap/agent-top
agent-top --version
```

## What gets built

| Runner | Target | Notes |
|---|---|---|
| `macos-14` | `aarch64-apple-darwin` | Apple silicon |
| `macos-13` | `x86_64-apple-darwin` | Intel Macs |
| `ubuntu-22.04` | `x86_64-unknown-linux-gnu` | glibc 2.35 floor |
| `ubuntu-22.04-arm` | `aarch64-unknown-linux-gnu` | glibc 2.35 floor |

Every target is built natively — no cross-compilation, no `cross`, no
emulation — and the workflow asserts that each runner's host triple is the one
it is packaging, so a runner image change cannot silently mislabel an asset.

Binaries link the build machine's glibc, which is why Linux is built on 22.04
rather than the newest image: a binary built against a newer glibc refuses to
start on older distributions. That sets the floor at Ubuntu 22.04, Debian 12
and Fedora 36. A musl build would remove the floor entirely and is the right
answer if anyone reports it.

## Homebrew core

`homebrew-core` accepts a formula only once the project is notable enough
(roughly 75 stars, forks or watchers) and has a stable, versioned release
history. Until then the tap is the distribution channel, and it is the one
users get from `brew install kannandreams/tap/agent-top`.
