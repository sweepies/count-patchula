# Benchmarks (SWE-88, report-only)

[Tak](https://github.com/jdx/tak) is pinned to 0.0.15 in mise. Its README calls it
pre-v1 and warns that its interfaces can change. These measurements help
investigate performance; they never gate a change. Configuration follows the
[benchmark docs](https://tak.jdx.dev/guide/configuration), verified against the
pinned executable's CLI help.

## Running and recording

```sh
mise install
mise run benchmarks:run
mise exec -- cat benchmark-results/summary.txt

# Explicit wall-time-only run (also works without Valgrind):
mise run benchmarks:run --no-counters

# Narrow investigation; arguments are passed to tak run:
mise run benchmarks:run --bench config-fnox --runs 20

# Record against local HEAD in refs/notes/tak, without pushing anything:
mise run benchmarks:record --no-counters
mise run benchmarks:export
```

`benchmarks:build` builds optimized CLI and replay-example binaries through
Cargo/mbx before measuring. `benchmarks:run` and `benchmarks:record` depend on it;
compilation and mise startup are outside measured commands. Run from the repo
root with Cargo's default target directory (`target/`). Results go under ignored
`benchmark-results/`: `summary.txt` is tak's human report, `results.json` contains
samples and metadata, and `tak-artifact.json` exports recorded local HEAD notes.
Recording notes does not commit source or publish to a remote. Record again
**after** committing if the measurement should be associated with that commit.
Ordinary measurement can be done on an uncommitted working tree.

The default is ten timed runs and two warmups for each of fourteen benchmarks:

- CLI startup: the release `rustovate --version` invocation.
- Config evaluation: `rustovate config check` for each of the twelve examples,
  including file reads, Pkl evaluation and validation. Each sample starts a fresh
  process; warmups only warm host/filesystem state.
- GitHub/cache replay: 25 simulated 200/304 pairs through the real GitHub client
  and a shared SQLite cache, then 1,000 warm reads of a compressed 4 KiB entry.
  The example uses rustovate's existing conditional cassette and the fail-closed
  Replay transport. It constructs no live HTTP transport and needs no token.
  Synthetic credential namespaces keep each pair's initial GET cold. Every
  response body, 304 count, and cassette consumption is checked. End-to-end
  wall time includes process/runtime, temporary DB and replay-fixture setup;
  this is a micro workload, not a network latency or batch throughput claim.

## Instruction counts and CI

Tak optionally uses Valgrind for instruction counts. Valgrind is absent on the
current local host and has no entry in the checked mise registry. Local
verification therefore uses wall time only; no instruction-count numbers are
claimed. Do not install system packages locally for this workflow. The separate
CI benchmark job installs Valgrind via apt on Ubuntu, since mise cannot provide
it, and asks tak to record counters where supported. If installation fails,
tak falls back to timings. The replay benchmark explicitly disables counters
because cache-worker scheduling and SQLite I/O are variable.

The CI job is `continue-on-error: true`, has a 20-minute timeout, calls no gate
or `tak compare`, and never pushes notes. It records locally, exports a portable
artifact, appends its text summary to the job summary and uploads the result
directory (including any partial JSON after failure). The regular `ci` job
still enforces lint/tests/build. No trusted publisher or benchmark comments are
configured; results are Actions artifacts only. A first hosted execution and
Valgrind compatibility with the pinned toolchain remain unverified. Inspect
stderr and `results.json` to see which counters were actually available.

Local validation on 2026-10-14 completed all 14 subjects with
`mise run benchmarks:run --no-counters` and exported JSON plus text. The shared
Linux host produced substantial timing variance and tak outlier warnings; these
samples verify the workflow and are not performance targets or batch baselines.

## Real batch timings

No rustovate batch timing exists yet: lookup/run remain Phase 1 stubs. Record
results here only after a real batch works, including commit, hardware, repo
count, dry-run/write mode, cache state and repetition count. Startup/config
microbenchmarks are not substitutes for these measurements.

| Date | Commit | Host | Repos | Mode | Cache | Runs | Median | Notes |
|---|---|---|---:|---|---|---:|---|---|

Historical Renovate baselines from [design.md](design.md), not remeasured here:
Renovate 44.155.6, warm caches, one EC2 c7i.large.

| Workload | Baseline |
|---|---:|
| Batch of 12 repos, before the release-notes patch | 6.7 min |
| fnox alone, after the patch | 44 s |
| renovate-runner alone, after the patch | 13 s |

The design targets (unconfirmed) are under 5 seconds for an unchanged warm batch
and 10–30 seconds with updates, bounded by lockfile tools and GitHub writes.
