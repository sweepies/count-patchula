---
title: "Threat model"
description: "Who the Count trusts, what a repository may cause, and which guards follow from that."
---
Read this before changing credentials, lookups, caches shared between
repositories, or anything that runs a tool. Every guard in those areas should
follow from the rules below. A guard that doesn't follow from them is noise,
and it breaks legitimate setups.

## Who is trusted

- **The operator** runs the Count and writes its settings: the operator
  settings file, flags and environment. The operator is fully trusted.
- **Every repository** the Count reads is treated the same way. Its writers
  may be hostile toward the Count and toward other repositories. There is no
  setting that trusts a repository.

Someone who can change what the Count reads in a repository — its
configuration, manifests or lockfiles on the branch he reads — already has
write access to that repository. Nothing the Count does can take that access
away, and nothing needs to protect the repository from its own writers.

## What a repository may cause

It may:

- point the Count at any host, registry or package its configuration names.
  The Count follows repository-declared sources, such as a registry, a mise
  `api_url` or tool options. He never falls back to a public package of the
  same name when the declared source fails (dependency confusion).
- receive a credential whose reach is no wider than that repository's own,
  such as a token minted for that repository. Sending that credential to a
  host of its choosing gains the writer nothing.

It may not:

- run code on the Count's machine. Code the repository chose includes the
  code of every package it depends on, directly or through other packages:
  the repository picks the packages, and may pick the registry they come
  from. That covers plugin backends (asdf, vfox), lifecycle scripts, hooks,
  build logic such as PEP 517 backends, gemspecs and MSBuild targets, the
  preparation of git dependencies, credential helpers and anything else that
  executes something the repository or a package chose. Resolving a lock
  reads package metadata; it never executes anything fetched. The Count runs
  as one user, with his credentials, his cache and, under an App, the private
  key, so code running there reaches all of them. A sandbox would not make
  that code acceptable; the rule is that it never runs.
- obtain a credential wider than its own reach: the installation token, the
  App key, another repository's token, an operator registry credential meant
  for other repositories, or the machine's own credentials. Cloud
  instance-metadata endpoints (link-local addresses such as `169.254.169.254`)
  hand those out, so a repository-chosen fetch target may not be one.
- see data that only another repository's credentials could fetch. Results
  shared between repositories, such as listings and catalogs, are shared only
  when they came from public data or from a credential the reader may hold.

## Consequences

- **Follow, don't refuse.** If the Count can reproduce what a repository
  declares, he does it. He refuses only what would run code chosen by the
  repository or its packages, or what he cannot reproduce faithfully.
- **Scope credentials, don't strip them.** Give each lookup the narrowest
  credential its repository is entitled to. Don't withhold that credential
  because a repository-chosen host could see it.
- **Never run code a repository chose.** Plugin backends, lifecycle scripts,
  hooks, build backends and the like stay off for every repository. When a
  lock cannot be regenerated without running such code, the update that
  needs it is skipped, and lockfile maintenance with it, with a reason that
  stands on the dashboard and in the report until the repository changes. No
  pull request is opened or updated with a stale lock instead.
- **Keep shared results honest.** A shared cache key includes everything that
  changes the answer: the options, the source, and the reach of the
  credential that fetched it.
- **Argument hygiene is separate from trust.** Rejecting values that a tool
  would read as flags, selectors or extra arguments is about parsing
  correctly, and applies to every repository.

## How each tool keeps to it

Every lockfile tool runs with an allowlisted environment, its own HOME and no
inherited configuration, and the Count's own checkout runs apart from it.
On top of that, each tool keeps the rule like this:

- **mise** (lookups and `mise lock`): plugin backends are disabled, and every
  tool name and option passes the lookup's checks before mise starts.
- **npm** (`lockfiles.npmEngine = "npm"`, and format v1 locks):
  `--ignore-scripts`. npm prepares a git dependency by packing it, which runs
  its scripts whatever `--ignore-scripts` says, before pacote 22, and the
  pinned npm ships an older pacote. So the update is refused when any
  `package.json` in the tree, its `overrides`, or the lock names a source
  outside the allowlist of registry versions, local paths and plain tarballs
  (git URLs, forge shorthands such as `github:` or `owner/repo`, forge archive
  URLs). The run has no Git transport (`GIT_ALLOW_PROTOCOL` names none), so a
  git dependency of a registry package fails to fetch, and a guard the Count
  loads into every Node process of the run stops pacote's preparation process
  before it starts, for a GitHub dependency pinned to a commit, which arrives
  over HTTPS. A regenerated lock that records a git source is thrown away and
  the update refused.
- **Yarn Berry**: the Count's pinned release, every `.yarnrc.yml` up to the
  repository root replaced for the run by a copy without `yarnPath`,
  `plugins` or `networkSettings`, and `enableScripts` off. Yarn packs git and
  GitHub dependencies whatever `enableScripts` says, and runs `exec:`
  generators, so the same refusal covers manifests, `resolutions`, catalogs,
  `packageExtensions`, `yarn.lock` and `exec:`. The run has no Git transport,
  and the GitHub hosts Yarn's GitHub fetcher downloads from are closed in the
  run's user configuration. A regenerated lock that records a git source is
  thrown away.
- **aube** (pnpm, npm and Yarn Classic locks) and **Bun**:
  `--ignore-scripts`. Neither runs a git dependency's scripts, directly or
  through a registry package; the canary tests run both with Git reachable to
  prove it.
- **uv**: `uv lock --no-build` with `UV_NO_BUILD=1` and the interpreter
  named by version (`UV_PYTHON`), so every dependency comes as a wheel or
  not at all; one only an sdist provides, or a path dependency with
  non-static metadata, ends in uv's "building is disabled" answer, which is
  a refusal. `--no-build` does not stop uv building the workspace's own
  packages, which it does whenever their metadata is dynamic (a dynamic
  version alone included), so before uv runs the Count refuses the update
  when any `pyproject.toml` of the workspace (the project's own, and every
  one below a uv workspace root above it) declares `dynamic` metadata, or a
  workspace `members` or `exclude` glob leaves the workspace root.
- **Poetry**: Poetry builds packages to read metadata that is not static,
  and no setting stops it while locking. It runs only under a Count-owned
  launcher (`python -I poetry_guard.py`) that replaces every build entry
  point (the PEP 517 hook call, pypa/build's builder, Poetry's isolated
  builder and metadata build, the installer's Chef) and plugin loading with
  an immediate exit, and that refuses to start when the installed Poetry is
  not the version the Count asked for or an entry point it replaces is
  missing. The Count runs it only on Poetry 2.5.1, the default its canary
  test covers; a repository that pins another Poetry is refused, and so is
  a lock the launcher stopped.
- **Bundler**: a Gemfile is Ruby, so Bundler never reads the repository's.
  The Count renders a Gemfile from his own parsed model and runs Bundler on
  it, beside a copy of `Gemfile.lock`, in a scratch directory outside the
  checkout, with `BUNDLE_VERSION=system` (never a Bundler the lock names,
  which it would download and run), `BUNDLE_APP_CONFIG` pointed away from
  any repository `.bundle/config`, and no Git transport; only
  `Gemfile.lock` comes back.
  `gemspec`, `path:`, `git:`, `github:` and other forge keys, `git_source`,
  `plugin`, `eval_gemfile`, `install_if`, any line the Count does not read,
  and a lock with `GIT`, `PATH` or `PLUGIN SOURCE` sections are refused.
- **dotnet**: `packages.lock.json` is not regenerated at all. `dotnet
  restore` evaluates MSBuild projects, props and targets from the repository
  and from every package it restores, so every update that would move one is
  refused.
- **Composer**: `--no-scripts --no-plugins --no-install`.
- **Cargo**, **Go**, **Terraform**, **APM** and **gh-actions-lock** resolve
  from metadata and checksums and run nothing they fetch. **nix** locks
  inputs without evaluating the flake's outputs, with `max-jobs = 0` and the
  flake's `nixConfig` ignored. The Gradle wrapper is checked against Gradle's
  published checksum, not run.

Each refusal and Git guard has a canary test that runs the real tool over a
dependency whose scripts, backend or gemspec write a marker file, reached
directly and through a registry package, and checks that the marker never
appears.

## Cache scopes

Results the Count keeps from GitHub (Git objects, HTTP validators, catalogs,
listings, hints) are keyed by a credential scope that names the credential's
reach, never its secret:

- an App installation by the App and the installation;
- an installation token (`ghs_`: an Action's App token or `GITHUB_TOKEN`) by
  the API root, its App's bot user and either the whole account it is
  installed on or the exact set of repositories it reaches, as GitHub
  reports them for that token in the run. Anything unexpected in that
  report keeps the token hash;
- any other token (personal, OAuth or user tokens) by a hash of the token,
  since GitHub does not report what it reaches.

Two credentials share entries only when their reach is the same. A token
narrowed to some repositories shares nothing with the full installation.

Permissions are not part of the scope. Everything the cache answers without
asking GitHub needs at most `contents: read` on a repository in the reach,
and an installation token has the same permissions on every repository it
reaches. So a token run uses its reach as the scope only after the token has
read a repository's contents in that run, a private one when the reach has
any. Anything that needs more is asked of GitHub every time.

The scope is not a secret, so the cache file is trusted like the operator's
settings: whoever can write the file the Count opens can plant entries in
it. On a runner that is whoever can write the Actions cache the workflow
restores, which is the operator, or the repository's own writers when the
workflow lives in the repository it updates. A Git blob read from the cache
must hash to its SHA, so a planted blob is read from GitHub again. Trees,
commits, catalogs, listings and hints cannot be checked that way and stay
plantable by that writer, as public registry responses always were.

## Out of scope

- Protecting a repository from its own writers.
- A compromised operator, or a compromised host the operator configured.
