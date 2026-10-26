---
title: "CI cache"
---
CI builds through mr-boxington (`mbx`) with the pinned
`jdx/mr-boxington-action`, configured with `github-cache-mode: objects` and
`isolate-objects-cache: true`. Before the build, the action imports portable
objects and prediction and build-script metadata. After it, the action exports
the deduplicated closure of every completed command, using its automatic
`MBX_CACHE_EXPORT_GROUP`. The isolated store under `RUNNER_TEMP` keeps
unrelated local state out, and the action turns off automatic GC on hosted
runners so imported objects survive. The GitHub backend needs no server URL,
OIDC permission or extra token scope. See the action's
[documentation](https://mr-boxington.jdx.dev/github-action).

## Keys and publication

Each job has its own cache generation: `v3-ci-objects` for the CI job,
`v3-corpus-objects` for the corpus lookup and `v3-benchmarks-objects` for
benchmarks. The generation is part of both the primary key and the restore
prefix, so two jobs never publish the same key or restore each other's profile
artifacts. Benchmarks build release binaries and must not share the CI debug
bundle.

Pushes to the default branch publish in the action's post step. Pull requests
only restore (`save-on-pull-request: false`).

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
- `MBX_TARGET_LANES=0`, set in `mise.toml`, makes the sequential Clippy, test
  and build commands share one target tree instead of adding
  `target/check/debug` beside `target/debug`.
- `clean-target-before-export: true` runs `mbx clean` in the post step, after
  every target consumer has finished. Only the portable object closure is
  exported; workspace target state is never transported. The isolated store is
  removed before `actions/cache` stages its archive.

## Cache failures never fail CI

The pinned action has no input that makes restore, export or save failures
nonfatal, so the action step and its diagnostic step set
`continue-on-error: true`. GitHub applies the same setting to the post step.
Cache errors show up as annotations, while fixture checks, formatting, Clippy,
tests and builds still gate the job.

## Diagnosing storage

The CI and Benchmarks jobs log `du -sh target/.` and `df -h` before the post
step, and the action logs resource use after cleanup and during export and
save. Measure `target/.`, not `target`: mbx
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
