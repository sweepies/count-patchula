---
title: Releasing
description: How a release is made, from one workflow run.
---

A release is one run of the **Release** workflow
(`.github/workflows/release.yml`). Run it from the Actions tab, or:

```sh
gh workflow run release.yml -f version=0.5.2
```

Nobody bumps versions in a pull request or pushes a tag by hand. The workflow
uses only the built-in `GITHUB_TOKEN`.

## What the run does

1. **Guard.** The run refuses unless it runs on `main`, the version is
   later than the latest `v*` tag and not tagged yet, and the commit it runs
   on has a successful **CI result** check from GitHub Actions. That check is
   the only verification of the code: the commit already passed CI.
   Versions are `X.Y.Z`; prereleases are not supported.
2. **Bump.** `mise run release:bump <version>` sets the workspace version
   and the internal path-dependency versions, refreshes the workspace entries
   in `Cargo.lock`, moves the docs' `@vX.Y.Z` and `@X.Y.Z` pins, and
   regenerates the CLI reference (`docs:cli`) and the help snapshot
   (`help:snapshot`). The run commits the result as `chore: X.Y.Z` and
   fails if the commit touches anything else.
3. **Build.** One Linux runner builds the static archives for Linux and
   macOS with GoReleaser (`mise run release:dist`), against a local
   `vX.Y.Z` tag on that commit. Each archive is run on its own platform,
   and the server image is built from the Linux archives and run.
4. **Land.** Only when all of that passes, the image is pushed to GHCR by
   digest, the commit is pushed to `main` and tagged `vX.Y.Z`, and the
   image gets its `X.Y.Z` and `latest` tags.
5. **Publish.** GoReleaser builds the archives again from the tag, checks
   that they match the smoke-tested ones, signs the packslip manifest and
   publishes the GitHub release. communique then rewrites the notes
   (`mise run release:notes vX.Y.Z`).

A failed build pushes nothing, so fix the problem on `main` and run the
workflow again. If `main` moves while the run builds, the push to `main` is
refused and nothing is tagged, leaving only an untagged image; run the
workflow again from the new `main`. If a step after the push fails, re-run
the failed jobs: the land job sees the tag already on the bump commit and
carries on.

## Dry run

`dry_run: true` does the guard, the bump, the builds, the image checks and
the publish job's rebuild, comparison and packslip signature, and skips every
push, the tag, the provenance attestations and the release. A dry run may start from any
branch whose head passed CI, which is how a change to the workflow is tested:

```sh
gh workflow run release.yml --ref my-branch -f version=0.5.2 -f dry_run=true
```

## GoReleaser

`.goreleaser.yaml` builds the four targets with `cargo zigbuild`, packs each
into `patchula-vX.Y.Z-PLATFORM.tar.gz` (`bin/patchula`, the man page, the
completions under `share/`, `LICENSE` and `README.md`), writes
`patchula-vX.Y.Z-checksums.txt` and creates the GitHub release with those,
`patchula.usage.kdl` and `packslip.sigstore.json`. The release body starts
as GoReleaser's list of commits since the previous tag. The usage spec, man page and completions come
from the build the host can run, through `docs:cli`. GoReleaser, cargo-zigbuild
and packslip are task tools of `release:dist` and `release:sign`.

`mise run release:dist --snapshot` builds the archives locally, unversioned,
with no tag and no signature; it runs on Linux.

### macOS on Linux

Zig compiles the C dependencies (aws-lc, SQLite, zstd) for macOS with its own
headers and links against its own `libSystem`, but it ships no Apple
frameworks. rustls-platform-verifier needs Security and CoreFoundation for the
system trust store, so `release/macos-sdk` holds text stubs (`.tbd`) of the
two frameworks that list only the symbols the binary imports, and
`release:dist` points `SDKROOT` at them. The linker records the frameworks'
real install names, and macOS loads the real frameworks at run time. No Xcode
SDK is downloaded.

A dependency update that imports a new symbol from either framework fails the
link with `undefined symbol: _Name`. Add the name to the stub's `symbols` list.

### One build, published twice

GoReleaser without Pro cannot publish files it did not build, so the release
is built twice: once before landing, for the smoke tests, and again from the
tag in the publish job. Both builds use the same toolchains, paths and commit
timestamps, so the archives are byte-identical. The macOS binaries are
linked with `-x`, which leaves local symbols out of the symbol table: Zig's
Mach-O linker names anonymous ones after the thread that read their object
file, which made the symbol table, the UUID and the code signature differ
between two builds of the same commit. GoReleaser runs
`release:sign` as its signing step, after the checksums and before
publishing. It compares the checksums file with the smoke-tested one and fails
on any difference, so a release that does not match what was tested is never
signed or published. The provenance attestations are made for the
smoke-tested archives, which by then have the same digests.

The server image is not built by GoReleaser: its rebuild would install
Debian packages again, so the pushed image would not be the one that ran. The
image job builds, runs and pushes it as before.

## No CI on the bump commit

`main`'s ruleset requires a successful **CI result** from GitHub Actions on
every commit pushed to it. The bump commit's parent passed CI, and the commit
changes only versions and the files generated from them, which the run has
just built and tested. So the land job posts the **CI result** check run on
the commit itself, as GitHub Actions through the built-in token, and does not
run CI again. A check run needs a commit GitHub already has, so the commit
goes to a short-lived `release/vX.Y.Z` branch first, which the job deletes
afterwards. Pushes made with the built-in token start no workflows, so CI
doesn't run on the branch or on `main` either.

## Signatures

The packslip manifest and the build attestations are signed with the run's
OIDC identity, `.github/workflows/release.yml@refs/heads/main`. Releases up to
0.5.1 were signed from their tag ref. mise's signer continuity compares the
workflow path without the ref, so installs carry on. A mise configuration that
pins `workflow = "release.yml"` accepts only tag refs, so it needs
`identity_prefix = "https://github.com/sweepies/count-patchula/.github/workflows/release.yml@"`
with GitHub's issuer instead.

## Release notes

communique reads the Anthropic key from Infisical through GitHub OIDC. The
repository uses GitHub's immutable OIDC subject format, and the Infisical
machine identity trusts
`repo:sweepies@7191851/*:ref:refs/{tags/v*,heads/main}`, which covers a run
dispatched on `main`. Until the `INFISICAL_IDENTITY_ID` and
`INFISICAL_PROJECT_ID` repository variables are set, the release keeps
GoReleaser's commit list. `mise run release:notes vX.Y.Z` also rewrites a
published release's notes locally, with `ANTHROPIC_API_KEY` set.
