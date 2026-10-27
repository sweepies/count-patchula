---
title: "CI jobs and caches"
---
## Jobs

`.github/workflows/ci.yml` runs each check as its own job, and every job runs
one or two mise tasks, so `mise run ci` runs the same checks locally, one after
another. The workflow holds no shell beyond single task calls; anything longer
is a file task under `mise-tasks/`.

| Job | Tasks | Runs when |
| --- | --- | --- |
| Changed paths | `ci:changes` | always |
| Lint: rustfmt, clippy | `lint:rust` | Rust can be affected |
| Lint: workflows | `lint:actions` | Rust or a workflow can be affected |
| Build: workspace, fixture secret scan | `build`, `fixtures:check` | Rust can be affected |
| Tests: unit (update) | `test:unit --part update` | Rust can be affected |
| Tests: unit (others) | `test:unit --part others` | Rust can be affected |
| Tests: real lockfile tools | `test:tools` | Rust can be affected |
| Docs: build | `docs:build` | the docs site, `mise.toml` or `ci.yml` changed |
| Corpus: lookup | `corpus:report` | Rust can be affected |

`ci:changes` reads the changed paths from the GitHub API. A docs-only change
runs only the docs build; files that no job reads, such as `README.md`, run
nothing. Unknown or unlisted paths run every Rust job.

`patchula-update`'s suite takes about a minute on its own, so it runs beside
the other crates' tests. `test:tools` runs only the `lockfiles::tests::real_`
tests, the ones that drive the real lockfile engines, and `test:unit` skips
them, so only one job installs the task-scoped engines.

There is no aggregate gate job; every check is reported on its own.

## mbx objects

The Rust jobs build through mr-boxington (`mbx`) with the pinned
`jdx/mr-boxington-action`, configured with `github-cache-mode: objects` and
`isolate-objects-cache: true`. Before the build, the action imports portable
objects and prediction and build-script metadata. After it, the action exports
the deduplicated closure of every completed command, using its automatic
`MBX_CACHE_EXPORT_GROUP`. The isolated store under `RUNNER_TEMP` keeps
unrelated local state out, and the action turns off automatic GC on hosted
runners so imported objects survive. The GitHub backend needs no server URL,
OIDC permission or extra token scope. See the action's
[documentation](https://mr-boxington.jdx.dev/github-action).

Each job has its own cache generation: `v4-lint-objects`,
`v4-build-objects`, `v4-test-update-objects`, `v4-test-others-objects`,
`v4-tools-objects`, `v3-corpus-objects`, and `v3-benchmarks-objects` for
the Benchmarks workflow. The generation is part of both the primary key and the
restore prefix, so two jobs never publish the same key or restore each other's
profile artifacts. Clippy's check metadata, the test builds and the release
builds share almost no objects, so one combined bundle would only make every
job download what it does not use.

Pushes to the default branch publish in the action's post step. Pull requests
only restore (`save-on-pull-request: false`).

## Downloads

Two `actions/cache` entries hold downloads that mbx does not:

- **Cargo downloads**, `cargo-downloads-v1-<Cargo.lock hash>`:
  `~/.cargo/registry/index`, `~/.cargo/registry/cache` and
  `~/.cargo/git/db`, about 60 MB. Every Rust job restores it, with the
  previous lockfile's entry as a fallback. Only the lint job saves it, from
  `main`, because Clippy over all targets fetches every crate the other jobs
  build. The extracted sources in `~/.cargo/registry/src` are not cached;
  Cargo unpacks them from the cached archives.
- **Lockfile engines**, `lockfile-tools-v1-<hash of mise-tasks/test/tools>`:
  the mise installs of the task-scoped engines `test:tools` declares, about
  415 MB. The real-tool job restores it before the task and saves it on a miss
  from `main`. It replaces about 20 seconds of downloads from several hosts
  with a 9-second restore.

`jdx/mise-action` caches the rest of the mise installs under its default key,
which hashes `mise.toml`. Every job restores the same entry, about 305 MB, and
the first job to miss saves it. The Rust toolchain comes from rustup, outside
the mise directory, and installs in about 10 seconds.

The lockfile engine tests are offline, so the engines' own package caches
(NuGet, pip and uv, gems, Composer, npm) are never filled and are not cached.
The docs build installs its packages with aube in a few seconds, so its store
is not cached either.

The corpus job keeps registry validators and catalogs in
`corpus-v2-<run id>`, restored by prefix like a real deployment's cache.
Only `main` saves it.

## Footprint

A push to `main` publishes about 1.6 GB of mbx bundles (one per job and
commit), plus the corpus cache when it changes. The mise, Cargo download and
lockfile engine entries change only with their keys. GitHub evicts the least
recently used entries past the repository's 10 GB limit, and every job restores
the newest entry for its own prefix, so eviction only removes bundles nothing
restores any more.

## Build settings

- Dev builds use `debug = "line-tables-only"` for workspace code and
  `debug = 0` for dependencies. The test profile inherits dev, so tests keep
  workspace source locations without linking full dependency debug info.
  Release settings stay at Cargo's defaults.
- No task uses `--all-features`, which keeps optional Pkl engines and extra
  HTTP and TLS backends out of the build.
- Conformance is a member of the root workspace and builds in the same debug
  target. tak uses release binaries only in the Benchmarks workflow, which runs
  on demand.
- `MBX_TARGET_LANES=0`, set in `mise.toml`, makes Clippy, tests and builds
  share one target tree instead of adding `target/check/debug` beside
  `target/debug`.
- `clean-target-before-export: true` runs `mbx clean` in the post step, after
  every target consumer has finished. Only the portable object closure is
  exported; workspace target state is never transported. The isolated store is
  removed before `actions/cache` stages its archive.
- Native C code, such as `aws-lc-sys`, compiles in each Rust job, because mbx
  bypasses native links and build-script runs. A compiler cache in front of
  `zig cc` saved only a few seconds locally, so there is none.

## Cache failures never fail CI

The pinned mbx action has no input that makes restore, export or save failures
nonfatal, so the action step sets `continue-on-error: true`. GitHub applies
the same setting to the post step. The `actions/cache` save steps set it too.
Cache errors show up as annotations, while the checks still gate their jobs.

## Diagnosing storage

Each Rust job runs `mise run cache:usage`, which logs `du -sh target/.` and
`df -h` before the post step, and the action logs resource use after cleanup
and during export and save. Measure `target/.`, not `target`: mbx
[manages the target](https://mr-boxington.jdx.dev/managed-targets) through a
symlink, and `du -sh target` measures only the link. `mise run cache:doctor`
checks the local setup.

A run that publishes logs `Saved mbx objects bundle; actions/cache confirmed
upload/finalization` in its post step; an export message alone does not prove
publication. A rerun of the same commit restores an exact hit and skips saving
the immutable entry. Cargo rebuilds targets from portable objects, so eligible
compilations show object hits, while unsupported crate types and rustdoc can
still bypass. mbx's per-command remote byte counters stay at zero because the
action does the transport.
