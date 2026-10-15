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

## Compact catalogs and parallel cache readers (2026-10-15)

Baseline: `bd94910` (including the three previously unpushed commits);
implementation: `e0427bd`. This
investigation uses independent public registry metadata and the public workload
above. No Renovate implementation source or private renovate-runner data was
used. All builds use mise; every test/replay is offline. Live CLI samples use
`GH_TOKEN="$(gh auth token)"`, release builds, concurrency eight and `-vv`.

### npm format choice

The [npm registry metadata specification](https://github.com/npm/registry/blob/main/docs/responses/package-metadata.md)
documents per-version publication times in full metadata; abbreviated metadata
provides package-level `modified`. A live probe of vite, TypeScript, Renovate
and Effect confirmed that none of the abbreviated documents had a top-level
`time` map or per-version `time`. Their full documents supplied the timestamps.
The default 24-hour gate and publication-date report fields need those times;
a candidate-only abbreviated lookup cannot supply equivalent age evidence.

One sequential curl pass with `--compressed`, alternating full then abbreviated
per package, measured the following. Wire bytes are compressed; decoded sizes
are the saved JSON length. Network timings are single observations with CDN and
host variance, not a statistically controlled format comparison.

| Package | Versions | Full decoded bytes | Abbreviated decoded bytes | Full wire bytes / seconds | Abbreviated wire bytes / seconds |
|---|---:|---:|---:|---:|---:|
| vite | 760 | 38,988,699 | 2,333,244 | 4,625,639 / 1.306 | 380,228 / 0.232 |
| typescript | 3,853 | 15,796,668 | 8,733,038 | 2,020,054 / 0.403 | 1,647,254 / 2.051 |
| renovate | 5,723 | 72,292,488 | 35,630,263 | 3,308,199 / 7.227 | 2,601,256 / 6.063 |
| effect | 490 | 10,396,946 | 414,627 | 383,235 / 1.107 | 104,900 / 0.267 |

For this age-aware workload, abbreviated-first still requires the full request:
eight requests and 15,070,765 wire bytes versus four and 10,337,127. Its measured
sequential transfer sum would be 18.656 seconds versus 10.043 seconds for full
alone. The chosen path requests full metadata only on a cache miss or upstream
revalidation returning 200, skips unused fields during typed deserialization,
and stores only the compact catalog. Fresh hits and 304s reuse that catalog.
No arbitrary TTL extension or missing-date assumption was introduced. A separate
API for callers that deliberately omit all age/report evidence is not implemented.

Derivation runs in bounded blocking workers; network ingestion still buffers the
response. Compact npm catalogs retain version strings, publish dates, deprecated
flags, dist-tags and modified time. Integrity/shasum is unused by lookup and is
omitted. Cargo and PyPI documents and GitHub REST release heads also compact on
200. Full GraphQL release/tag history was already derived. Schema v3 retires raw
registry bodies and obsolete content-hashed npm extractions; format versions are
part of HTTP cache keys. Migration requires one unconditional registry fetch,
and reuses freed SQLite pages without automatically shrinking the file.

The read pool has four read-only WAL connections on dedicated workers. Access
updates are coalesced in writer transactions, keeping decoding independent of
both writes and async runtime scheduling. No version history is truncated.

### Live public comparison

The same shared host, repository commit and config from the earlier public
investigation were used: `sweepies/remote-agent` at
`74449eb704b8dcb6a1a05d11d22c533b8b884f19`, including the large TypeScript and
Effect catalogs. Both binaries were built first. With no compiler/CI process
running, baseline/implementation were alternated for cold, warm 1 and warm 2;
each variant began with its own absent cache. Each sample is a fresh CLI process.
These are six individual network observations, not medians. All six completed
with 16 dependencies and 16 proposals; their reports were identical after
removing statistics and the evaluation clock (`releaseAge.evaluatedAt`).

| Version | Cache | CLI wall seconds | Engine ms | Requests | 304s | Cache hits |
|---|---|---:|---:|---:|---:|---:|
| `bd94910` | Cold | 7.25 | 7,120.49 | 33 | 0 | 0 |
| `e0427bd` | Cold | 4.60 | 4,487.37 | 33 | 0 | 0 |
| `bd94910` | Warm 1 | 1.96 | 1,866.44 | 9 | 1 | 9 |
| `e0427bd` | Warm 1 | 1.47 | 1,396.80 | 9 | 1 | 9 |
| `bd94910` | Warm 2 | 2.18 | 2,068.30 | 9 | 9 | 17 |
| `e0427bd` | Warm 2 | 1.27 | 1,193.27 | 9 | 9 | 17 |

The warm-2 trace shows npm fresh hits returning 905,855 total decoded bytes,
down from 38,350,084. TypeScript changed from 15,796,668 bytes / 279.04 ms to
472,080 bytes / 4.20 ms. The final npm hit completed after 21.24 ms versus
279.04 ms before; source parsing/hashing in the baseline delayed the first
GitHub attempt until approximately 375 ms after engine start, versus 11 ms now
(inferred from trace completion timestamps minus `total_ms`). Cache and network
work now overlap. The remaining warm-2 prefetch took 1,185 ms: GitHub reads had
roughly 0.9–1.0 second durations, and the ninth request queued behind the cap of
eight (`cli/cli` release head: 864 ms queued, 1,154 ms total). The warm target
well below one second is **not established on this host**. The private
renovate-runner and a lower-latency production host were not remeasured; their
previous user-supplied timings are not used as this experiment's baseline.

Raw public artifacts: `benchmark-results/compact-catalogs/{before,after}-public-final/`
contain JSON reports, `-vv` logs and wall times. The reproducible public command
above is unchanged; use distinct cache paths/binaries to alternate variants.

### Large-document offline replay

The four exact full documents in the npm format probe total 137,474,801 decoded
bytes. The new example was copied unchanged into a detached `bd94910` checkout
and built through mise. Three pairs of baseline/implementation processes ran
alternately after all builds/CI finished, using a new SQLite cache for each.
Each process performed one cold and two warm passes, with four concurrent
lookups. Medians below use three observations per cell.

| Version | Cold ms | Warm 1 ms | Warm 2 ms |
|---|---:|---:|---:|
| `bd94910` | 4,022.399 | 1,030.077 | 1,136.962 |
| `e0427bd` | 307.117 | 28.660 | 13.331 |

Every pass in every process returned the identical normalized catalog SHA-256
`2832abe1ef352efb1270f7df4e1fa4725556979b6904a99dbf54a0d01737ca4c`.
There were four simulated 200 requests on cold and zero new transport calls on
both warm passes. This isolates the local cost and makes no live network claim.
The warm medians improve about 36× and 85×; cold local processing improves 13×.
Raw runs and databases are in
`benchmark-results/compact-catalogs/{before,after}-replay-{1,2,3}/`; the captured
public inputs and full/abbreviated transfer observations are in `npm-probe/`.

Existing release-mode cassette replay also passed via
`mise run benchmarks:lookup-release`. The small source replay changed from
23.14 / 10.09 ms cold/warm to 15.63 / 11.64 ms, with 19 cold requests, three warm
requests and two warm 304s in both versions. The multi-repo engine replay changed
from 85.90 / 20.99 ms to 37.52 / 6.77 ms, preserving 14 cold requests, six warm
requests, one warm 304, three unique lookups and two extraction hits. These are
single small-fixture observations, including scheduling variance, rather than
large-packument performance evidence. Logs are `benchmark-results/compact-replay-before.log`
and `compact-replay-after-final.log`.

### Cache footprint and validation

Sizes below were collected after warm 2 and a SQLite WAL checkpoint/truncate,
without VACUUM. HTTP bytes sum `length(body)` across the HTTP table; database
bytes include release/extraction data, indexes and allocated/free pages.

| Identical workload | Metric | Before bytes | After bytes | Reduction |
|---|---|---:|---:|---:|
| Public remote-agent | Database file | 6,225,920 | 921,600 | 85.2% |
| Public remote-agent | Compressed HTTP bodies | 3,310,845 | 251,641 | 92.4% |
| Four-packument replay | Database file (median of three) | 9,048,064 | 192,512 | 97.9% |
| Four-packument replay | Compressed HTTP bodies (all runs identical) | 5,466,252 | 131,618 | 97.6% |

Replay baseline database sizes varied from 8,822,784 to 9,068,544 bytes with
insertion order; implementation sizes were 192,512 bytes in all three runs.
Baseline replay also held 1,254,330 bytes of uncompressed catalog extractions and
10,826 duplicate release rows; both disappear with compact HTTP storage. Existing
migrated databases reuse freed pages and may retain their old physical file size.

`CARGO_NET_OFFLINE=true RUSTOVATE_RECORD=0 mise run ci` passed: 21-cassette secret
scan, formatting, Clippy `-D warnings`, workspace tests and build. Added coverage
checks parallel reader execution, schema upgrades/key versions, fresh reads after
reopen, compact 304 validator updates, changed/deprecated/unpublished versions,
invalid 200 preservation, no-store/unsupported Vary and PyPI upload/yanked flags.
Existing discovery tests caught and verified the fix for documents without a
`time` map: publication dates remain unknown, matching prior behavior. No test
constructs a live transport except the already-ignored, explicit recording paths.
All commits remain local; no push, PR or private fixture recording was performed.

### Reproduce the large offline replay

Capture public documents explicitly, outside tests (the four files are ignored
benchmark inputs, not committed cassettes):

```sh
mise exec -- bash -c '
  set -eu
  input=benchmark-results/npm-documents
  mkdir -p "$input"
  for package in vite typescript renovate effect; do
    curl --fail --silent --show-error --compressed -H "Accept: application/json" \
      "https://registry.npmjs.org/$package" -o "$input/$package-full.json"
  done
'
mise run benchmarks:catalogs benchmark-results/npm-documents benchmark-results/replay-new.sqlite
```

`catalog_bench` constructs only an in-memory replay transport. It reads the four
saved documents before timing, performs four concurrent lookups on a cold cache
and two fresh warm passes, asserts identical catalogs and exactly four total
transport calls, and prints a SHA-256 of the normalized catalogs. It rejects an
existing cache path. Its internal timer excludes Cargo/build/startup/input-file
reads and catalog sorting for comparison/fingerprinting. SQLite cache reads, compact/raw
parsing, sorting, storage and converting returned catalogs to JSON are included. Repeat with new cache paths; never
compare different captured input sets. The example uses only the public client
API and can be copied unchanged into a detached baseline checkout for A/B runs.
Tests never record or fetch these inputs.
