# Benchmarks (SWE-88, report-only)

## Mise-versions catalogs (2026-10-15, departure 49)

Release CLI, concurrency 8, identical per-repository example overrides and an
independent new SQLite cache for each workload/mode. Builds and GitHub CLI
authentication are outside wall time. `useVersionsHost` is the only config
difference. No remote writes or fixture recording were performed. Requests below
count `api.github.com` trace completions, including extraction and digest reads.

- Twelve repositories, direct sources: cold **39.28 s / 590 GitHub requests**;
  warm **5.29 s / 126 requests**, with **345 proposals** on both runs.
- Twelve repositories, primary host: cold **49.36 s / 499 GitHub requests**;
  first warm **87.83 s / 72 requests**; settled warm **3.28 s / 71 requests**.
  There were 38 mise-versions requests per run. The cold run had **343 proposals**,
  while both warm runs had **345**. GHCR throttled the large Immich machine-learning
  catalog and a tool file temporarily answered 404; the first warm run completed
  the missing registry history and incurred throttling delays. The 404 was
  reported rather than retried on GitHub. An earlier cold run with an extraction
  HTTP 500 was excluded, together with its partially cold warm run.
- Sandbox, direct sources: cold **6.33 s / 27 GitHub requests**; warm
  **1.78 s / 10 requests**. Primary host: cold **5.66 s / 17 requests**; warm
  **1.53 s / 6 requests**, plus six mise-versions requests per run. All four
  observations retained **30 proposals**.

These are sequential single observations on changing, shared services, not
controlled medians. The lower GitHub request count does not imply a guaranteed
wall-time improvement during registry throttling. Raw reports, traces and
summaries are under `/tmp/rustovate-followup-bench/`: `item2-results.json`,
`item2-final-results.json` and `item2-steady-results.json`. The following
release-age catalog reuse measurement builds on these baseline paths.

## Phase 1 final live check (2026-10-15)

The supplied final twelve-repository batch at the Phase 1 implementation took
**37.4 s cold / 4.1 s warm / 3.8 s warm2**, producing **343 proposals on every
run**. The fresh Renovate dry run took **261 s** and started about four minutes
before rustovate's cold run. These are caller-supplied single observations,
with builds outside the measured runs; they are not controlled medians.
The observed cold/warm/warm2 speed ratios are approximately **7.0× / 63.7× /
68.7×** relative to that Renovate run.

Cold report: `/root/rustovate-conf/r12-cold.json`; fresh baseline:
`/root/rustovate-conf/renovate12.jsonl`. Input triage was **0 missing, 0 extra,
6 field-mismatch, 275 departures**. Final strict replay at
`/root/rustovate-conf/out12-closed/triage.{md,json}` is **0 missing, 0 extra,
4 field-mismatch, 277 departures**. Independent cached floating-tag evidence
accounts for the two action fields; the four unocss fields remain because
the log's actual evaluation timestamps are already beyond 72 hours. See
[the timestamp evidence and report provenance](phase1-triage.md#final-live-check-2026-10-15).

All twelve repositories ran through rustovate; eleven have a comparable
Renovate baseline. remote-agent's `renovate.json` is invalid, so Renovate
supplies no baseline for it. rustovate's own configuration for it validates.
Proposal-count stability and timings do not establish parity for that gap.

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

### Phase 1 exit round 4 input (2026-10-15)

Caller-supplied release observations at `1826e32`, twelve test repositories in
one batch, with an initially fresh SQLite cache:

| Cache pass | Seconds | Proposals |
|---|---:|---:|
| Cold | 33.5 | 344 |
| Warm | 2.9 | 344 |
| Warm2 | 3.5 | 344 |

The fresh Renovate dry run of the same repositories took **296 s**, captured
within about ten minutes. The observed ratios are about **8.8× cold** and
**85–102× warm**; these are single-run comparisons, not medians or guarantees.
Host/hardware were not supplied. rustovate's proposal count is identical across
all three captures; cold/warm differences caused by the old local pull cooldown
are absent in this batch. Both captures retain upstream lookup/coverage limits.

Inputs are `/root/rustovate-conf/r12-{cold,warm,warm2}.json` with their `.err`
traces and the **fresh** `/root/rustovate-conf/renovate12.jsonl`; the earlier
baseline is now `renovate12-old.jsonl`. Fresh triage started at **2 missing,
6 extra, 13 field-mismatch, 264 departures**. The final round 4 offline replay
of the unchanged cold report is **0 missing, 1 extra, 7 field-mismatch,
277 departures**, under `/root/rustovate-conf/out12-r4-final/`. Its eight
remaining rows map to fixed tag-date, peer, Cargo-floor and provider behaviors
that require new live output; they are not accepted departures. Eleven repos
have a comparison baseline; remote-agent is a configuration-validation coverage
gap. Full row accounting is in [phase 1 triage](phase1-triage.md).

These timings precede the round 4 changes. No lookup credential is configured
in this implementation process, so new cold/warm timings and proposal sets are
still required for implementation `9271512` (local; not pushed). Additional tag
release-history and Node LTS metadata reads mean
the earlier timings must not be attributed to this implementation. Phase 1 exit
requires a live rerun confirming the fixes, deterministic proposals and zero
unexplained differences.

### Phase 1 exit round 3 input (2026-10-15)

Caller-supplied live observations at `1fa827c`, release build, all twelve
comparison repositories in one batch with an initially fresh SQLite cache:

| Cache pass | Seconds | Requests | 304s | Proposals |
|---|---:|---:|---:|---:|
| Cold | 36.9 | 1,750 | 0 | 331 |
| Warm | 4.2 | 163 | 76 | 331 |
| Warm2 | 4.1 | 151 | 122 | 334 |

Each is one sample, not a median; host/hardware were not supplied for these
captures. The baseline lookup log predates evaluation by about one hour.
Artifacts are `/root/rustovate-conf/r12-{cold,warm,warm2}.{json,err}` and
`renovate12.jsonl`, outside the repository. Warm2 has 27 warnings, reviewed in
[phase1-triage.md](phase1-triage.md). The 12 repositories are external test
inputs; rustovate is developed independently of any runner in that set.

The three extra proposals are two vaultwarden/server and one hashicorp/vault
digest pins. Failed HEADs in cold/warm versus successful HEADs in warm2 account
for all three; this is not an age crossing. A successful zero-pull response set
an overly broad local cooldown, making later HEAD availability depend on order.
Round 3 separates consuming manifest GET quota from HEAD/tag-list access and
tests both orders across cold/warm caches. Equal-precedence version candidates
also have a deterministic source-record tie-breaker, tested at a fixed clock.

The final offline adapter replay of the unchanged warm2 report is **8 missing,
6 extra, 21 field mismatches and 263 departures**, with one missing baseline.
Eight digest differences carry rate-limit evidence. These counts include the
old report's unfixed lookup/metadata outputs; they are not a live measurement
of round 3. No new timing/proposal-count improvement is claimed. A new release
build and contemporaneous baseline should verify lookup completeness, stable
proposal sets for the same inputs/evaluation time and remaining source errors.
Implementation `af88256` passed `CARGO_NET_OFFLINE=true mise run ci`.

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


## Twelve-repository host queues and registry freshness (2026-10-15)

Baseline: `4182a1f`; implementation: `acf42c2`, `832936f` and `ecec03b`. This work uses
independent protocol documentation and public manifests, never Renovate
implementation source. Builds and tests run through mise; tests are offline.
Release binaries are built before measurements, and each sample is a fresh
process with `GH_TOKEN="$(gh auth token)"`, concurrency eight and `-vv`. Own
build/CI jobs finished before live samples; the host is shared. Measurements are
individual observations, not medians or a controlled latency guarantee.

### Confirmed causes and selected behavior

The registry client had one semaphore across every registry host. GitHub already
had a separate governor: it did not literally wait behind GHCR's semaphore, but
its cap was also tied to repository concurrency. The CLI now defaults to 16
requests per host, independent of its eight repository scans, with repeatable
`--host-concurrency HOST=LIMIT` overrides. Catalog groups, file reads and digest
work use HTTP admission instead of repository slots. GitHub's existing primary
reservations, secondary cooldowns and retry budgets remain intact.

The supplied successful original warm-2 trace contains 260 GHCR 200s, 56 Go
list/latest 200s and 52 anonymous token calls (36 GHCR, 16 Docker Hub). Registry
validators cannot eliminate those downloads. OCI tag pages now store only tag
strings and use a one-hour freshness floor when validators are absent. Go
list/latest has the same configurable minimum; tagged Go lists skip @latest,
while empty lists retain its pseudo-version fallback. Both floors are derived
from fetched-at and can be disabled immediately with their `--*-freshness-secs 0`
flags. `no-cache`/`no-store`, other registries and mutable manifest digests retain
their upstream rules. Anonymous image-scoped pull tokens are reused only within
their expiry minus a margin; private/GitHub credentials are never stored as tokens.
See [cache policy](cache.md#local-freshness-and-host-limits) and
[departure 21](departures.md#departure-21). New versions/deletions can be delayed
within the chosen freshness window; OCI publication dates remain unknown.

A live GitHub Packages versions probe returned 403: the supplied OAuth token
lacks `read:packages`, as required by the
[Packages API](https://docs.github.com/en/rest/packages/packages#list-package-versions-for-a-package-owned-by-an-organization).
Docker Hub's dated newest-first API worked (the postgres probe reported 1,427
tags). It does not make stopping at a known mutable tag sufficient to reconcile
older retargeting/deletion. Neither API replaced portable OCI enumeration; no
unsafe lexical `?last=` early stop was introduced. Every expired catalog still
walks the complete page chain.

Initial live attempts also exposed intermittent GHCR 429s partway through Immich
catalogs. Extraction status remained Complete, but ten original-workload proposals
were missing (432 versus 442). Those short timings were rejected. Registry reads
now share a host cooldown and retry at most three times, honoring numeric/date
Retry-After up to 60 seconds or backing off 1/2/4 seconds without it. Longer waits
and exhaustion remain explicit failures; 429s never enter the cache. Tests cover
recovery, exhaustion and isolating a throttled host. Final runs below are checked
for full enumeration, stable proposal counts and warnings, not status alone.


### Final public twelve-repository comparison

The committed `examples/benchmark-batch-public.pkl` reads actual public workflow,
Dockerfile, Kubernetes/Kustomize and Go declarations. It restricts Go extraction
to six common `golang.org/x` modules; every selected source still enumerates its
full catalog. The twelve repositories are listed in the reproduction command
below. This is a different workload from the supplied twelve sweepies repos:
72 unique catalogs, 203 proposals and twelve Complete repositories in every run.

For this benchmark only, non-GHCR image digest checks are explicitly disabled;
Docker Hub tag catalogs and GHCR digest checks remain enabled. The shared outbound
IP exhausted Docker Hub's anonymous pull allowance during earlier repetitions.
A direct public probe returned 429 with `ratelimit-remaining: 0;w=3600` and the
unauthenticated pull-limit classification. HEAD is documented not to consume
pull quota, but a live HEAD was also rejected once the quota was already zero.
The production image-digest default remains enabled. This benchmark therefore
cannot establish successful Docker Hub digest checks on the exhausted IP.
See [Docker pull limits](https://docs.docker.com/docker-hub/usage/pulls/) and
[its distinct quota/abuse responses](https://docs.docker.com/docker-hub/troubleshoot/).

Both release binaries used this exact same benchmark configuration. Their six
samples alternated before/after for cold, warm 1 and warm 2, with separate absent
SQLite caches for cold and retained caches for warm. There were **zero 429s** in
all six final samples. After removing only `stats` and `releaseAge.evaluatedAt`,
all six complete JSON reports are identical, including the same six pre-existing
warnings (five SHA pins without version comments and one setup-go digest 422).
Normalized SHA-256:
`8c54d1d8fe004ab9a505aa2e17e5933cdf66c829f155c981dfe4cd20ab2b19e4`.

| Version | Sample | Wall s | Engine s | Requests | 304s | Proposals |
|---|---|---:|---:|---:|---:|---:|
| `4182a1f` | cold | 44.30 | 44.226 | 837 | 0 | 203 |
| `4182a1f` | warm1 | 30.62 | 30.572 | 380 | 68 | 203 |
| `4182a1f` | warm2 | 33.83 | 33.700 | 371 | 92 | 203 |
| `ecec03b` | cold | 36.31 | 36.262 | 831 | 0 | 203 |
| `ecec03b` | warm1 | 3.68 | 3.533 | 104 | 68 | 203 |
| `ecec03b` | warm2 | 3.40 | 3.339 | 93 | 92 | 203 |

Warm wall time improves 8.3× and 9.9× in these observations; warm-2 requests fall
75%. Cold still walks 248 authenticated GHCR tag pages, including the very large
Immich machine-learning catalog. Cold requests drop by six because tagged Go
modules no longer query @latest. No history or lexical page chain is truncated.

Warm 2 makes zero GHCR tag-list requests, zero Go list/latest requests and zero
token requests. All 248 GHCR tag pages, nine Docker Hub tag pages and six Go
lists are fresh reads. The remaining 93 requests are 87 GitHub reads and six
conditional GHCR manifest HEADs. Warm 1 has five token refreshes and six initial
HEAD 401s: intervening baseline scans let the short anonymous tokens expire;
persisting tokens does not extend their server lifetime. The remaining wall
latency is predominantly GitHub validation/head reads and their bounded queue.

Representative warm-2 host aggregates follow. Durations sum across concurrent
requests and must not be added to estimate wall time. `miss` includes challenges
and first uncached reads; fresh hits are included explicitly. All cold/warm-1/
warm-2 aggregates are retained next to the raw logs as `*.hosts.json`.

| Version | Host | Outcome | Count | Summed total ms | Summed queued ms |
|---|---|---|---:|---:|---:|
| `4182a1f` | api.github.com | 304 | 86 | 143856.7 | 116657.4 |
| `4182a1f` | api.github.com | miss | 1 | 264.1 | 0.0 |
| `4182a1f` | auth.docker.io | miss | 4 | 812.7 | 31.4 |
| `4182a1f` | ghcr.io | miss | 10 | 3816.7 | 1101.3 |
| `4182a1f` | ghcr.io | 200 | 248 | 36039.2 | 85.2 |
| `4182a1f` | ghcr.io | 304 | 6 | 585.1 | 0.0 |
| `4182a1f` | proxy.golang.org | fresh-hit | 9 | 21.4 | 0.0 |
| `4182a1f` | proxy.golang.org | 200 | 3 | 2048.5 | 1608.8 |
| `4182a1f` | registry-1.docker.io | miss | 4 | 2171.8 | 0.0 |
| `4182a1f` | registry-1.docker.io | 200 | 9 | 1408.6 | 25.8 |
| `ecec03b` | api.github.com | 304 | 86 | 96779.1 | 65618.2 |
| `ecec03b` | api.github.com | miss | 1 | 230.4 | 0.0 |
| `ecec03b` | ghcr.io | fresh-hit | 248 | 416.8 | 0.0 |
| `ecec03b` | ghcr.io | 304 | 6 | 1836.2 | 0.0 |
| `ecec03b` | proxy.golang.org | fresh-hit | 6 | 22.9 | 0.0 |
| `ecec03b` | registry-1.docker.io | fresh-hit | 9 | 30.6 | 0.0 |

After WAL checkpoint/truncate without VACUUM, baseline/implementation database
sizes are 7,401,472 / 7,434,240 bytes and compressed HTTP bodies are 3,631,228 /
3,624,231 bytes. Compact OCI pages retain all tag strings, so this work mainly
removes repeat network downloads rather than shrinking an already small page
format. Anonymous token rows and SQLite allocation order can increase file size.

Raw public artifacts are under `benchmark-results/batch-live/verified-{before,complete}/`;
`verified-summary.json` includes all samples, host aggregates, cache sizes and
normalized report hashes. Earlier `public-before`, `public-after`,
`public-final` and original-workload attempts are diagnostic samples, not part
of this final A/B comparison.

### Original workload and limits of verification

The supplied successful baseline remains 99.6 / 73.4 / 69.6 seconds with 442
proposals and about 1,815 / 630 / 632 requests. The exact supplied warm-2 trace
confirms 260 GHCR 200s and 56 Go list/latest 200s. Its GitHub 304s alone sum
120,970 ms total / 87,915 ms queued; npm 304s sum 22,700 / 20,353 ms. These are
actual queue costs, not connection-setup estimates.

The original 324-catalog workload was also run with the new caches before the
HEAD change. Warm lookup phase time was 2.584 / 3.396 seconds, with zero Go
list/latest requests and no successful GHCR tag-page downloads (two inaccessible
private-image challenges remained). However, Docker Hub manifest quota failures
caused roughly 41 seconds of digest retry time and reduced proposal counts to
434 / 429 / 429 across cold/warm samples. Those **are not successful end-to-end
speedup results** and are excluded above. They motivated both HEAD checks and
fail-fast classification of persistent pull-quota failures.

The original full 442-proposal workload has **not** been validated after the
quota was exhausted. It needs an available Docker Hub quota or authorized Docker
registry credentials; neither is fabricated here. No new private registry
credential support, Docker Hub dated-tag API or GitHub Packages fallback was
implemented. The new public benchmark proves the catalog/host changes on twelve
repositories, not an identical replacement for the supplied private workload.
Private workload reports/caches/logs remain outside this repository at
`/root/rustovate-conf/performance-{after,final}/`; no private fixtures were added.

### Validation and reproduction

`CARGO_NET_OFFLINE=true RUSTOVATE_RECORD=0 mise run ci` passed on the final code:
22-cassette secret scan, formatting, Clippy `-D warnings`, workspace tests and
build. Meaningful new coverage includes independent host admission, freshness
expiry/disable/no-cache behavior, compact OCI reopen, anonymous-token persistence,
expiry/rejection/no-store, stable/prerelease/pseudo Go versions, bounded 429
recovery/exhaustion/host cooldowns, pull-quota classification, HEAD/GET validator
isolation, HEAD 304 reuse and unsupported/missing-header fallback. The three
manifest fixture interactions are explicitly identified as independently
simulated HEAD responses derived from recorded GET headers and OCI semantics;
no tests performed network requests. Log: `benchmark-results/batch-complete-ci.log`.

Build first, then reproduce the final public policy with a new cache:

```sh
mise exec -- cargo build --release --locked -p rustovate-cli
mise exec -- bash -c '
  set -eu
  export GH_TOKEN="$(gh auth token)"
  unset RUSTOVATE_TOKEN GITHUB_TOKEN
  batch_root=benchmark-results/batch-live
  batch_repos=(
    sweepies/remote-agent sweepies/immich-go stefanprodan/podinfo
    fluxcd/flux2 fluxcd/source-controller fluxcd/helm-controller
    fluxcd/kustomize-controller fluxcd/notification-controller
    fluxcd/image-reflector-controller fluxcd/image-automation-controller
    containrrr/watchtower immich-app/immich
  )
  for batch_repo in "${batch_repos[@]}"; do
    mkdir -p "$batch_root/config/${batch_repo%/*}"
    printf "amends \"../../../../examples/benchmark-batch-public.pkl\"\n" \
      > "$batch_root/config/$batch_repo.pkl"
  done
  batch_run=$(mktemp -d "$batch_root/reproduce.XXXXXX")
  for sample in cold warm1 warm2; do
    /usr/bin/time -f "%e" -o "$batch_run/$sample.time" \
      target/release/rustovate lookup --config-dir "$batch_root/config" \
      --cache "$batch_run/cache.sqlite" --concurrency 8 -vv --json \
      "${batch_repos[@]}" > "$batch_run/$sample.json" 2> "$batch_run/$sample.log"
  done
  echo "$batch_run"
'
```

Use an independently built `4182a1f` binary and its own absent cache for the
baseline, keeping the same final config. Alternate variants as above; do not
compare the earlier invalid-rate-limit samples. To restore upstream catalog
freshness for a diagnostic run, pass both `--registry-freshness-secs 0` and
`--go-freshness-secs 0`; doing so deliberately restores repeated downloads.
All implementation and documentation commits remain local; nothing was pushed.
