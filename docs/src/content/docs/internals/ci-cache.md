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
| Corpus: lookup | `corpus:report`, after the build job | Rust can be affected |
| Cache: prune superseded entries | `cache:prune` | pushes to `main` |

`ci:changes` reads the changed paths from the GitHub API. A docs-only change
runs only the docs build; files that no job reads, such as `README.md`, run
nothing. Unknown or unlisted paths run every Rust job.

`patchula-update`'s suite takes about a minute on its own, so it runs beside
the other crates' tests. `test:tools` runs only the `lockfiles::tests::real_`
tests, the ones that drive the real lockfile engines, and `test:unit` skips
them, so only one job installs the task-scoped engines.

The corpus lookup is mostly network time, so it runs the build job's dev
binary, passed as an artifact and named by `PATCHULA_BIN`, instead of a
release build of its own. A release build took about two minutes and kept a
separate 250 MB objects cache that shared nothing with the debug bundles; the
dev binary plans the corpus in under a minute.

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

Each job that compiles has its own cache generation: `v4-lint-objects`,
`v4-build-objects`, `v4-test-update-objects`, `v4-test-others-objects`,
`v4-tools-objects`, and `v3-benchmarks-objects` for the Benchmarks
workflow, which never saves. The generation is part of both the primary key and the
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
  414 MB. The real-tool job restores it before the task and saves it on a miss
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

## Pruning

Every save creates a new immutable entry: mbx keys carry the commit, corpus
keys the run, and the mise, Cargo download and lockfile engine keys a hash of
their inputs. GitHub only evicts past the repository's 10 GB limit, so the
cache used to sit at that limit. Two jobs keep it small:

- On every push to `main`, the last CI job, Cache: prune superseded entries,
  runs `mise run cache:prune --drop-others <prefix>...`. It keeps the newest
  `main` entry of each listed prefix and deletes the older ones, and
  `--drop-others` deletes `main` entries that match no prefix, such as retired
  mbx generations. Every cache `main` should keep must be in that list. It
  runs after the saving jobs finish, even when some fail; a failed job saves
  nothing, so its family keeps its previous entry.
- When a pull request closes, `cache-cleanup.yml` runs
  `mise run cache:prune --all --ref refs/pull/<n>/merge` from the default
  branch and deletes every cache the pull request saved.

Both use the built-in token with `actions: write` on that job alone, and run
the task file directly, without installing mise.

## Footprint

After pruning, `main` holds one entry per family, about 2.1 GB in total:

| Family | Size | New entry |
| --- | --- | --- |
| mbx `v4-lint-objects` | 90 MB | every push that runs Rust jobs |
| mbx `v4-build-objects` | 306 MB | every push that runs Rust jobs |
| mbx `v4-test-update-objects` | 206 MB | every push that runs Rust jobs |
| mbx `v4-test-others-objects` | 249 MB | every push that runs Rust jobs |
| mbx `v4-tools-objects` | 202 MB | every push that runs Rust jobs |
| `corpus-v2` | about 250 MB | every push that runs Rust jobs |
| `mise-v1` | 305 MB | `mise.toml` changes |
| `lockfile-tools-v1` | 414 MB | `mise-tasks/test/tools` changes |
| `cargo-downloads-v1` | 58 MB | `Cargo.lock` changes |

A push that runs the Rust jobs saves about 1.3 GB and the prune job deletes
about as much. Pull requests save only a mise entry when they change
`mise.toml`, and lose it when they close.

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
