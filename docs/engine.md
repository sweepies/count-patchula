# Read-only engine (SWE-95)

`rustovate-engine::Engine` accepts a token, shared SQLite cache, concurrency cap
and transport. The CLI constructs one network pool; tests inject cassette replay.
Its lookup method accepts repositories or discovers all accessible repositories,
using user/repos or the installation repository endpoint for App tokens.

Repositories run concurrently behind one GitHub governor. The default branch
commit and tree are read through conditional API requests. Only selected manager
files and required local config modules are fetched as blobs. Truncated trees
and invalid selected files produce per-repo errors. Config precedence is
.github/rustovate.pkl then rustovate.pkl; absence is skipped. Local --config
applies to every selected repository and is evaluated before network access.

Repository configs evaluate with an isolated supplied-file capability. Local
amends/import dependencies are fetched, with the bundled Rustovate.pkl available
when not vendored. Remote and escaping imports/resources are rejected. Literal
imports are supported; dynamic import targets require future discovery support.
An unchanged commit/config fingerprint reuses extraction without tree/blob calls.
Catalog and digest/ref requests are deduplicated across the whole batch. Opaque
action branches skip release history lookups, retaining their ref when pinned.

Report metadata includes branch/commit, warnings, source/kinds/locations, per-update
policy and request/cache counters plus phase timings. Unsupported managers and
sources are explicit warnings; API/config/extraction failures are errors. The CLI
writes only the report to stdout with --json and always summarizes on stderr.
It exits 1 for batch failures or any per-repo error, 2 for invalid config/options.
`run` remains unimplemented. No writes to GitHub occur.

Validation includes a public two-repo replay using jdx/usage and jdx/mr-boxington,
plus independently simulated discovery/config-precedence/skips/errors tests.
Public fixture file selection and ignore exceptions bound recording size while
retaining all extracted dependencies. Source tests separately cover npm, Cargo,
Go, GitHub releases and tags, moving refs and incremental pagination.

```sh
mise run benchmarks:lookup
GH_TOKEN=$(mise exec -- gh auth token) mise run fixtures:record-wave1
mise run fixtures:check
mise run ci
```

The benchmarks are debug engine runs against recorded responses, not a claim
about the twelve-repo production batch. Recording timings include network and
cassette redaction/persistence overhead. Persistent cache rows include repository
identities and selected manifest contents; keep private run caches/reports outside
this source repository.

Final wave-1 measurements (`mise run benchmarks:lookup`, debug, offline replay):
source fixtures: 54.11 ms cold / 28.15 ms warm (20 / 5 requests); two-repo batch:
227.42 ms cold / 42.56 ms warm (19 / 9 requests, three unique catalogs, two warm
extraction hits). Every warm network response is 304. The source fixture also
reuses two fresh validator-free Go responses without network access.
Public recording runs measured 3.29 s / 1.36 s for sources and 7.94 s / 4.64 s
for the batch, including recording overhead. These are individual observations,
not statistical benchmarks or live twelve-repo/App-token measurements.
