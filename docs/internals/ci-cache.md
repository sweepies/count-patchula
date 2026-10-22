# CI cache verification

Inspected run `38642667666`, attempts 1 and 2, with `mise exec -- gh run view
38642667666 -R sweepies/count-patchula --attempt 1 --log` (and attempt 2).
Attempt 1 saved target-cache ID `8963308758`; attempt 2 restored the exact
342 MB archive and skipped re-saving it. Thus the original archive transport
worked. mbx explicitly says its object counters exclude Cargo artifact reuse
and CI archive transfers. Those counters alone did not prove a cache failure.

The workflow selects `github-cache-mode: objects` and
`isolate-objects-cache: true` on the existing pinned action. It imports portable
objects and prediction/build-script metadata before builds, then exports the
deduplicated closure of every completed command using the action's automatic
`MBX_CACHE_EXPORT_GROUP`. An isolated `RUNNER_TEMP` store avoids unrelated local
state; the action disables automatic GC on hosted runners so imported objects
survive. No server URL, OIDC
permission or extra GitHub token scope is required for the GitHub backend.
The action controls default-branch publication; PR saving stays disabled.

## Export failures and storage policy

Runs `38654652071` and `38655546746` passed their code checks and failed during
cache export/save. In the latest run, the managed Cargo target occupied
4,434,399,232 allocated bytes before export, and export sampled only 5,001,216
free bytes. The workspace-state copy was omitted after ENOSPC. Benchmarks
finished first and published the same generated key; CI then failed to reserve
that immutable key. A successful benchmark upload was not a CI cache baseline.

The workflow now uses `v3-ci-objects` and `v3-benchmarks-objects`. Generation is
part of both primary keys and restore prefixes, so the two jobs cannot publish
the same key or restore each other's profile artifacts. These new generations
also avoid importing the oversized historical bundles. PRs remain restore-only.

Dev builds use `debug = "line-tables-only"` for workspace code and `debug = 0`
for external dependencies. Cargo's test profile inherits dev, so tests retain
workspace source locations without repeatedly linking full dependency debug
information. Release benchmark settings stay at Cargo's defaults. No task uses
`--all-features`: the existing feature selection already avoids optional Pkl
engines and extra HTTP/TLS backends. Conformance is a member of the root
workspace and builds in the same debug target. Tak consumes release binaries
in the Benchmarks workflow, which runs only on demand; it does not build
another profile inside the CI job.

`MBX_TARGET_LANES=0` makes the sequential Clippy/test/build commands share one
target tree instead of producing `target/check/debug` beside `target/debug`.
The pinned action's documented `clean-target-before-export: true` then runs
`mbx clean` in its post step, after every target consumer has finished. This
frees workspace and learned incremental state before exporting the portable
action/object closure; workspace target state is intentionally not transported.
The isolated store is still removed before `actions/cache` stages its archive.

The pinned action has no input that makes every cache restore/export/save
failure nonfatal. `continue-on-error: true` applies only to the cache action
and its diagnostic step. GitHub's runner uses the same action definition for
the post step, including its `continue-on-error` setting. Thus cache errors can
appear in annotations without failing CI; fixture checks, formatting, Clippy,
tests and builds remain required. Benchmark job failures were already report-only.

Both jobs log `du -sh target/.` and `df -h` before the post step, and the action
logs resource usage after target cleanup and during export/save. `target/.`
follows mbx's managed-target symlink; `du -sh target` alone measures the link.
No preinstalled runner software is deleted: the build changes and documented
target cleanup address the observed pressure without a runner-image-specific
cleanup script.

## Local measurements (2026-10-14/15)

The existing local target was 7.8 GiB from accumulated builds. For a comparable
measurement, both versions ran `mise run ci` with empty targets and fresh,
separate `MBX_CACHE_DIR` stores, `MBX_GC_AUTO=0` (as on hosted CI), and distinct
`MBX_CACHE_EXPORT_GROUP` receipts. The baseline used `edaa878`'s profiles and
default target lanes; the revised run used the settings above. Both passed.

`du -sh target/.` fell from **3.4 GiB to 1.7 GiB**, or
3,564,380 to 1,750,024 KiB with `du -sk` (51% less). The baseline included a
630 MiB `target/check/debug` lane; the revised target had only `debug/`.
The revised run passed 53 tests across 34 test/doc-test invocations, plus
fixture scanning, rustfmt, Clippy with warnings denied and the workspace build.

The baseline directory export occupied **4.0 GiB** (4,096,028 KiB). After the
revised build, `mbx clean` followed by `mbx cache export --group <group> --format
directory <bundle>` occupied **963 MiB** (985,132 KiB), 76% less. The export
reported 453 actions and 2,946 objects, with 950.4 MiB of object content; `du`
also includes metadata and filesystem allocation. The live revised store
shrunk from 2.9 GiB to 1.2 GiB during target cleanup. These are local allocated
sizes, not compressed GitHub upload sizes or measured hosted-runner headroom.

Importing the cleaned bundle into another empty store and running
`mise run build` passed in 50.21 seconds, with 158 hits, 36 misses and 43
bypasses. The portable closure remains usable after removing workspace state;
the restore does not promise that every compilation can be cached.

`actionlint` 1.7.12 validates the workflow, and `git diff --check` passes.

## Verify the next two runs

The first main push should miss both new generations, build, remove each
completed target, then export and save two distinct object bundles. Confirm
`Saved mbx objects bundle; actions/cache confirmed upload/finalization` in both
post logs, plus the free-space
samples; an export message alone does not prove publication. A cache failure
should leave the CI job successful when the required checks passed.

Re-running that commit should restore/import an exact hit per job and skip
re-saving the immutable entries. A second push at a new commit should restore
the matching job's compatible prefix and publish a new entry instead. Cargo
will reconstruct targets from portable objects; it cannot reuse a transported
workspace target, since that state was cleaned before export. Eligible
compilations should show object hits; unsupported crate types and rustdoc can
still bypass. Per-command remote byte counters remain zero because transport
happens in the action. Inspect both setup and post logs.

Local `mise run cache:doctor` passes with zero failures (warnings about archive
probing and the global Cargo shim are environment-specific; mise wraps Cargo).
An Actions upload/restore cannot be verified locally without its runtime cache
service; no push or workflow dispatch was performed.

Sources: [GitHub Action documentation](https://mr-boxington.jdx.dev/github-action),
[remote backends](https://mr-boxington.jdx.dev/remote-cache),
[pinned action README](https://github.com/jdx/mr-boxington-action/blob/47ce49b32025d93a7a2f4570815afaa2b7092240/README.md),
[managed target documentation](https://mr-boxington.jdx.dev/managed-targets),
[Cargo profiles](https://doc.rust-lang.org/cargo/reference/profiles.html),
[step failure handling](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax#jobsjob_idstepscontinue-on-error),
[runner post-action definition](https://github.com/actions/runner/blob/main/src/Runner.Worker/ActionRunner.cs).
