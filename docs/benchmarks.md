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

Read-only lookup is implemented; writes remain a Phase 1 stub. Record live
results here with commit, hardware, repo
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

## Live network investigation (2026-10-15)

This investigation builds on `ca4bcdd` and uses only public repositories and
registries. The primary workload is `sweepies/remote-agent` at
`74449eb704b8dcb6a1a05d11d22c533b8b884f19`, with
[`examples/benchmark-public.pkl`](../examples/benchmark-public.pkl). It selects
package.json, GitHub workflows, mise tools, and a regex extraction of
`min_version` in mise.toml as a jdx/mise dependency: 16 dependencies and 15 unique
source/package catalogs. The existing opaque workflow SHA has no version comment;
its warning is expected. Nothing from the repository is executed.

Host: shared Linux 7.0.14-23-pve x86_64, Intel Core i5-6500T at 2.50 GHz, mise Rust
1.99.0, release build, concurrency 8, local SQLite, fresh CLI process per sample.
There is no CPU isolation and the host's location is not established. Builds are
outside the measured commands. These are individual live observations, not
medians or a North America production baseline. Raw reports, request logs and
wall times are kept locally under ignored `benchmark-results/`.

### Measurements

Selected comparison: `ca4bcdd` versus implementation `f09bc51` plus the held-back
report normalization in `4df528e`. Both release binaries were built first, then
run alternately before/after for cold, warm 1 and warm 2. Each variant had its own
initially empty SQLite cache. No compiler or CI process ran during these samples.
All six reports completed with 16 proposals and the same public repository commit.

| Version | Cache | Wall seconds | Requests | 304s | Cache hits | Extraction hits |
|---|---|---:|---:|---:|---:|---:|
| `ca4bcdd` | Cold | 9.07 | 45 | 0 | 0 | 0 |
| Selected implementation | Cold | 4.98 | 33 | 0 | 0 | 0 |
| `ca4bcdd` | Warm 1 | 1.49 | 10 | 8 | 16 | 1 |
| Selected implementation | Warm 1 | 1.38 | 9 | 1 | 9 | 1 |
| `ca4bcdd` | Warm 2 | 1.20 | 9 | 9 | 17 | 1 |
| Selected implementation | Warm 2 | 1.39 | 9 | 9 | 17 | 1 |

Cold engine time fell from 8,985.87 to 4,886.79 ms; extraction from 1,863.30 to
1,519.79 ms, lookup from 7,119.92 to 3,363.70 ms. Selected warm lookup times were
1,332.90/1,352.72 ms and extraction 941.97/953.71 ms; those phases now overlap and
must not be summed. Raw artifacts: `benchmark-results/live-selected/`.

Cold improved in this close comparison; warm latency did **not** improve reliably.
The first selected warm run primes repository/release REST validators after cold
GraphQL discovery (eight 200s, one 304). The next uses nine 304s. Request counts
remain nine; no history queries or tree/blob reads occur on either warm run.

The initial baseline earlier in the session was 8.96/2.31/2.68 s (cold/warm 1/
warm 2), with 45/10/11 requests and 0/10/9 304s. Network variance is material.
An intermediate GraphQL-only warm variant used three requests but measured
1.58/1.54 s against 1.69/1.27 s before; it was rejected in favor of conditional
REST. Other intermediate variants spanned 5.67–11.55 s cold and 1.22–2.51 s warm;
these are not measurements of the selected implementation.

A separate TTL diagnostic about seven minutes after an intermediate variant's
warm run changed from three requests/one 304 to ten/eight, taking 2.58 s.
All seven added requests were expired npm entries, revalidated in parallel, with
no release-history refresh. That variant used GraphQL warm heads; its request
totals are diagnostic evidence for registry expiry, not the selected warm totals.

### Causes established from live requests

- The shared pool negotiated HTTP/2. Independent requests overlapped; ordinary
  governor acquisition was near zero, with queueing when the eight-request cap
  was occupied. There was no semaphore-of-one or mutex held over network I/O.
- Repository discovery had a metadata/default-branch dependency on cold runs;
  source lookup then waited for the entire extraction phase, even on warm runs.
  One GraphQL repository snapshot now supplies default branch, commit and tree
  on cold scans. Warm scans use parallel conditional metadata/branch reads,
  which outperformed GraphQL head snapshots in a close A/B run.
  Credential/config-scoped lookup hints start warm source/ref reads alongside
  repository validation. The newly validated extraction remains authoritative.
- GitHub tag history walked cursors serially. Matching-refs now enumerates the
  complete namespace, then bounded parallel queries hydrate unique immutable
  objects under one repository. Global Ref-node hydration was also tested:
  one 100-target probe took 1.82 s versus 1.12 s for repository object queries.
  This is a single diagnostic comparison, not a statistical speedup claim.
- The old release-head fingerprint hashed the complete REST response. Public
  cache comparisons proved identical tag/publication identities while download
  counts changed on 18 jdx/mise assets and four age assets. Those changes caused
  200s and unnecessary GraphQL history refreshes. Cold sources now share their
  first GraphQL history pages in groups of sixteen. Warm sources conditionally
  probe REST heads in parallel and compare only semantic release fields. Even
  a counter-driven 200 performs no history fetch if the release is unchanged.
  Warm GraphQL head batches were measured and rejected: fewer requests did not
  improve latency. The chosen warm path retains parallel conditional GETs.
- Current/proposed action tags previously caused extra commit requests, including
  a wave after policy evaluation. The revalidated tag catalog now supplies their
  peeled digests; opaque branches still revalidate separately.
- npm freshness is upstream-controlled, not re-keying: observed remaining TTLs
  ranged from 39 to 292 seconds in one warm run, reflecting max-age minus Age.
  After those TTLs expired, seven npm conditional GETs overlapped. Traces report
  `freshness=expired` and negative remaining TTLs. No arbitrary local grace period
  was added. Immediate warm request counts are stable; delayed counts can rise
  at legitimate expiry boundaries without history rehydration.

The exact earlier production result of 19 requests/5 304s cannot be reconstructed
without its request trace. Both mechanisms that explain that pattern were
reproduced here: expired registry entries add requests, and volatile release
documents previously added both 200s and history queries. The new diagnostics
distinguish them. A 304 count alone is no longer a useful warm efficiency metric:
the first warm run also primes REST validators after cold GraphQL discovery,
while later release probes may be 200 solely because counters changed.

### Diagnostics and limitations

`-vv` (including `-vvv`) or `RUSTOVATE_LOG=debug` enables one stderr line per HTTP
attempt and fresh-cache read, plus start/end phase spans. `RUSTOVATE_LOG` accepts
`off`, `error`, `warn`, `info`, `debug` or `trace`. Logs contain method, host,
sanitized path, status, cache outcome (`fresh-hit`, `304`, `200`, `miss`), decoded
body bytes, governor queue/rate-wait ms, total ms, freshness/TTL, and the negotiated
HTTP protocol when a network response is available. Query strings, userinfo,
headers, tokens and bodies are excluded; recognizable token paths are redacted,
including percent-encoded credentials. Dependency HTTP debug logs are filtered
out even at trace level. An offline test exercises secret headers, bodies,
userinfo, paths and query parameters and checks the emitted lines.

reqwest does not expose connection/TLS reuse or connect/TLS durations through
this transport API; those fields explicitly say `unavailable`. A separate public
curl diagnostic observed DNS 36 ms, connect 114 ms, TLS complete 201 ms and total
503 ms over HTTP/2. These values are not attributed to reqwest requests.

The sub-second warm and couple-second cold targets are **not established on this
host**. Full cold release history still follows each repository's cursor chain;
different sources run concurrently. Cold tag hydration and upstream GraphQL
service latency remain material. No history was truncated to manufacture a fast
result. Private renovate-runner and a North America host were not benchmarked.

### Reproduce public and renovate-runner runs

The following commands keep builds outside measurement and use a new cache for
cold, then the same cache for two warm samples. Each process authenticates using
the existing caller's token; all API operations are reads. No credential value
is printed.

```sh
mise exec -- cargo build --release --locked -p rustovate-cli
mise exec -- bash -c '
  set -eu
  export GH_TOKEN="$(gh auth token)"
  mkdir -p benchmark-results
  run_dir=$(mktemp -d benchmark-results/public-live.XXXXXX)
  for run in cold warm1 warm2; do
    /usr/bin/time -f "wall=%e" -o "$run_dir/$run.time" \
      target/release/rustovate -vv lookup \
      --config examples/benchmark-public.pkl --cache "$run_dir/cache.sqlite" \
      --concurrency 8 --json sweepies/remote-agent \
      >"$run_dir/$run.json" 2>"$run_dir/$run.log"
  done
  echo "$run_dir"
'
```

For renovate-runner, use the same loop with
`--config examples/renovate-runner.pkl` and `sweepies/renovate-runner`. Store that
private workload's cache/reports/logs in a private directory outside this repo.
Keep the same cache for later samples a few minutes apart; TTL diagnostics show
which registry responses expired. stdout stays JSON and all tracing stays on
stderr. At first upgrade, new cache namespaces may require one-time hydration;
subsequent unchanged runs reuse the derived catalogs.

Normal validation is offline:

```sh
mise exec -- env CARGO_NET_OFFLINE=true mise run ci
```

This includes fixture secret scanning, formatting, Clippy with `-D warnings`,
workspace tests and build. Public recordings were refreshed explicitly with the
existing opt-in recording workflow; normal replay constructs no live transport.
