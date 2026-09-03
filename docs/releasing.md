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
   | `CARGO_REGISTRY_TOKEN` | `cargo install agent-top` from crates.io | a crates.io API token scoped to `publish-new` + `publish-update`, crates `agent-top*` |

   Both are optional. If a secret is missing the workflow logs a warning and
   skips that step; the GitHub release and its binaries still happen.

## Checking the credentials before you tag

```sh
gh workflow run release.yml && gh run watch
```

A manual run of the release workflow publishes nothing. It only checks that
`HOMEBREW_TAP_TOKEN` exists and can actually write to the tap, and reports
whether `CARGO_REGISTRY_TOKEN` is set. Worth doing after rotating a token,
because the alternative is finding out from a red job on a tag whose binaries
are already in front of users.

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

The release workflow runs the full check suite on macOS and Linux first, and
builds nothing if it fails, so a tag cannot publish what CI would have
rejected. That gate was added after v0.2.0 shipped to crates.io and the tap
while a Linux-only test failure sat on `main`.

Then watch `gh run watch`, and verify the result on a clean machine:

```sh
brew install kannandreams/tap/agent-top
agent-top --version
```

## What gets built

| Runner | Target | Notes |
|---|---|---|
| `macos-14` | `aarch64-apple-darwin` | Apple silicon, native |
| `macos-14` | `x86_64-apple-darwin` | Intel Macs, cross-compiled |
| `ubuntu-22.04` | `x86_64-unknown-linux-gnu` | native, glibc 2.35 floor |
| `ubuntu-22.04-arm` | `aarch64-unknown-linux-gnu` | native, glibc 2.35 floor |

Intel macOS is cross-compiled from the arm64 runner rather than built on
`macos-13`. The Apple SDK ships both slices, so no extra linker or `cross` is
involved, and it removes the dependency on an Intel runner image that GitHub
has already put on its retirement path. The workflow runs `file` on each
binary and fails if the architecture is not the one the asset name claims, so
a runner image change cannot silently mislabel a download. The smoke test runs
only on the natively-built targets, since a runner cannot be relied on to
execute a foreign architecture.

`rustup target add` names the toolchain read out of `mise.toml`, so the target
cannot be installed into the runner's own default Rust while `cargo` is using
mise's pinned one.

Binaries link the build machine's glibc, which is why Linux is built on 22.04
rather than the newest image: a binary built against a newer glibc refuses to
start on older distributions. That sets the floor at Ubuntu 22.04, Debian 12
and Fedora 36. A musl build would remove the floor entirely and is the right
answer if anyone reports it.

## crates.io

The token wants exactly two endpoint scopes and nothing else:

| Scope | Why |
|---|---|
| `publish-new` | the first release of each crate name; `agent-top` and `agent-top-core` do not exist on crates.io yet |
| `publish-update` | every release after that |

Leave `yank` and `change-owners` unticked: the workflow never needs them, and
a leaked token that can only add versions is far less damaging than one that
can remove them or hand the crate to someone else. Under **Crates**, scope the
token to the pattern `agent-top*` so it covers both crates and any future
workspace member but nothing else you own.

Once both crates exist, a replacement token needs only `publish-update`.

Publishing is irreversible — a version can be yanked but never deleted, and
the name is reserved for good. The workflow publishes `agent-top-core` first
because `agent-top` depends on it by version, and skips any version already on
crates.io so a retried job does not fail on what already landed.

## Homebrew core

`homebrew-core` accepts a formula only once the project is notable enough
(roughly 75 stars, forks or watchers) and has a stable, versioned release
history. Until then the tap is the distribution channel, and it is the one
users get from `brew install kannandreams/tap/agent-top`.
