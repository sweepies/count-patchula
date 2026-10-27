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
5. **Publish.** The GitHub release is created from the build job's
   archives, checksums and usage spec, the same files the smoke tests ran,
   with its signed packslip manifest. communique then rewrites the notes
   (`mise run release:notes vX.Y.Z`).

A failed build pushes nothing, so fix the problem on `main` and run the
workflow again. If `main` moves while the run builds, the push to `main` is
refused and nothing is tagged, leaving only an untagged image; run the
workflow again from the new `main`. If a step after the push fails, re-run
the failed jobs: the land job sees the tag already on the bump commit and
carries on.

## Dry run

`dry_run: true` does the guard, the bump, the builds and the image checks,
and skips every push, the tag and the release. A dry run may start from any
branch whose head passed CI, which is how a change to the workflow is tested:

```sh
gh workflow run release.yml --ref my-branch -f version=0.5.2 -f dry_run=true
```

## GoReleaser

`.goreleaser.yaml` builds the four targets with `cargo zigbuild`, packs each
into `patchula-vX.Y.Z-PLATFORM.tar.gz` (`bin/patchula`, the man page, the
completions under `share/`, `LICENSE` and `README.md`) and writes
`patchula-vX.Y.Z-checksums.txt`. It publishes nothing: the workflow creates
the GitHub release from those files with `gh`, after the smoke tests and the
land job, so the published archives are the ones that ran. The usage spec, man
page and completions come from the build the host can run, through
`docs:cli`. GoReleaser and cargo-zigbuild are task tools of `release:dist`.

`mise run release:dist --snapshot` builds the archives locally, unversioned
and with no tag; it runs on Linux.

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
GitHub's generated notes. `mise run release:notes vX.Y.Z` also rewrites a
published release's notes locally, with `ANTHROPIC_API_KEY` set.
