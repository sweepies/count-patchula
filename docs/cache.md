# SQLite cache

`rustovate-cache` uses rusqlite 0.40 with bundled SQLite. rusqlite is a mature,
actively maintained synchronous wrapper with transactions and explicit error
codes; bundling avoids system SQLite version/feature drift. Zstd level 3 stores
HTTP bodies with a small CPU cost and reuses the HTTP stack's compression
dependency. The pinned Zig mise tasks build the native dependencies.

Open one Cache per batch and clone its handle. One dedicated OS thread owns
writes, migrations and corruption recovery, using a bounded queue of 128 jobs.
Four additional OS threads each own a read-only WAL connection and a bounded
32-job queue. Round-robin dispatch runs SQLite reads and Zstd decoding in parallel,
without blocking async workers. Release and extraction reads use the same pool.
The in-memory/rebuild fallback uses the writer connection. Reader corruption
disables the pool for that handle and retries through the recovery coordinator.
Each reader keeps a short SQLite statement lifetime, so decoding holds no WAL
read transaction. Canceled callers may still complete submitted writes. Dropping
the last handle closes the queues; worker threads drain and close asynchronously.

Read access updates are coalesced and flushed together in a transaction before
writer jobs, including budget eviction, and on worker shutdown. Reads therefore
do not acquire SQLite's write lock. Access within one coalesced group has
approximate recency. Body inserts and release merges are also transactional.

The database uses WAL, a five-second busy timeout and synchronous=NORMAL (cache
data is reproducible). The usual WAL sidecars are transient; all tables live
in one database file. One batch should own a cache path during corruption
recovery; coordinating quarantine across independent OS processes is outside
this API's contract.

HTTP keys normalize method case, URL authority/default ports/path/fragment and
header names/outer whitespace. Query order is retained because repeated query
parameters may be order-sensitive. Callers select vary-relevant headers; include
a credential namespace digest for authenticated data, never a raw token.
HTTP entries store status, validators, response headers (e.g. pagination Link),
compressed body, fetched-at, expiry and Cache-Control hints. Expiry does not
discard validators. Invalid compressed bodies are misses and are replaced by
the next successful fetch; their validators are never sent.

Large registry bodies use an explicit, versioned local `representation` in the
HTTP key, independent of upstream Accept/Vary. npm, Cargo sparse indexes, PyPI
and GitHub REST release heads store derived documents, never the raw response.
npm retains every version's publication time/deprecation flag, dist-tags and
modified time; unused integrity/shasum and other package metadata are discarded.
GitHub release heads omit assets/counters; complete GraphQL release/tag histories
already use release rows/derived extraction records. Node's small index and
repository/ref/Go metadata still use raw bodies under the same size budget.

On a 200, typed serde deserialization skips unused fields without materializing
the complete JSON tree. Registry derivation runs on `spawn_blocking` with a
four-job limit; HTTP transfer remains independently concurrent. The transport
still buffers one decoded response, so this is bounded parallel derivation,
not streaming network ingestion. A 304 updates only HTTP metadata and reuses the
compact body. Fresh hits neither hash nor parse the original packument. Uncacheable
responses remove both compact bodies and validators. A format change requires a
new representation version; raw and derived namespaces cannot share validators.

The default 256 MiB budget counts compressed HTTP body bytes. A monotonic access
clock approximates LRU and evicts entire HTTP rows until within budget, including
on open after a budget reduction. Removing validators along with evicted bodies
prevents unusable 304 responses. Release lists and extraction data are retained.
This is not a hard cap on database/WAL filesystem size: SQLite reuses freed
pages, and release/extraction tables are outside this budget.

Release order is upstream newest-first, never inferred from semver or dates.
`merge_releases` starts at the head; it returns true and stops processing at the
first item that was already stored. The known item's date/digest can refresh.
For subsequent pages of the same scan, `merge_release_page` takes the preceding
page's last inserted version as `after`. This explicit anchor distinguishes a
disjoint refresh prefix from initial history pagination, which cannot be inferred
from a page alone. The newest item is stored separately in `release_heads`.
Merges are transactional. Extraction keys include repo, default-branch commit,
and resolved config hash, and store caller-serialized bytes.

Schema migrations are transactional, using SQLite user_version: v1 contains the
three cache domains; v2 adds the HTTP access clock/LRU index; v3 introduces the
versioned compact representation keys and retires raw registry/release-head
bodies, obsolete npm content-hashed extractions, and duplicate npm/Cargo release
rows. Repository extractions and other HTTP/release data survive. Migrated
registries need one unconditional fetch; SQLite reuses freed pages without an
automatic VACUUM. Tests cover populated v1/v2 upgrades, version namespace isolation,
concurrent decoding, conditional compact refreshes and cache-control behavior.
A future schema is rejected without modifying or quarantining its database.

Opening runs quick_check. SQLITE_CORRUPT/NOTADB and failed integrity checks close
the connection, quarantine the file and any remaining WAL/SHM sidecars under a
unique `.corrupt-<timestamp>` name, warn through tracing, and rebuild. Operational
corruption is handled on the worker too: rebuild and retry the operation once.
If quarantine/rebuild fails, warn and use an in-memory cache to keep the batch
running. Permission, busy and future-schema errors are not misclassified as
corruption. The application must initialize tracing to display the warnings.

API references read:
[rusqlite](https://docs.rs/rusqlite/0.40.2/rusqlite/),
[rusqlite build features](https://github.com/rusqlite/rusqlite#optional-features),
[Connection](https://docs.rs/rusqlite/0.40.2/rusqlite/struct.Connection.html),
[zstd README](https://github.com/gyscos/zstd-rs#usage),
[Tokio mpsc](https://docs.rs/tokio/latest/tokio/sync/mpsc/),
[oneshot](https://docs.rs/tokio/latest/tokio/sync/oneshot/),
[tracing](https://docs.rs/tracing/latest/tracing/),
[url](https://docs.rs/url/latest/url/),
[SQLite WAL](https://www.sqlite.org/wal.html).

## Local freshness and host limits

`--concurrency` controls repository scan parallelism (default eight). Registry
HTTP requests use separate per-host semaphores, default 16 each, independent of
repository work. GitHub REST and GraphQL share their existing governor with a
CLI default of 16; primary limits, secondary cooldowns, Retry-After and bounded
retries remain enforced. This is below GitHub's documented 100 simultaneous
REST/GraphQL requests, but concurrency alone does not guarantee avoiding its
point/CPU secondary limits. `--host-concurrency api.github.com=8` overrides one
host; repeat the flag for others. Limits must be 1–100. Catalog and digest work
can start across hosts without waiting for a repository slot. The shared
reqwest pool retains up to 32 idle connections per host. Registry 429s publish
a shared host cooldown and retry at most three times. Numeric/date Retry-After
is honored; absent headers use 1/2/4-second backoff. Waits above 60 seconds and
exhausted retries fail explicitly, and queued callers do not bypass long
cooldowns. Other hosts remain available. Docker Hub pull-quota errors are returned immediately, rather than retried on
a short backoff. Only successful responses enter the
cache, so a rate-limited pagination attempt cannot become a complete catalog.

Two explicit freshness departures avoid redownloading validator-free catalogs:

- `--registry-freshness-secs 3600`: minimum freshness for OCI `tags/list` pages
  only when both ETag and Last-Modified are absent. Compact `oci-tags-v1` bodies
  contain only tags; pagination Link headers survive. After expiry, every lexical
  page is fetched again, detecting additions, deletions and retargeting anywhere.
  Manifest/tag digests keep their upstream policy and revalidate independently.
- `--go-freshness-secs 3600`: minimum freshness for public `proxy.golang.org`
  `@v/list` and `@latest`, even when their upstream max-age is shorter. Version
  `.info` documents retain upstream policy.

Both flags accept zero to restore upstream freshness immediately, including on
an existing cache. Floors are computed from original fetched-at, never extended
by reads. `no-cache` still requires revalidation and `no-store` still removes
entries. npm, Cargo, PyPI and other sources receive no freshness floor. New
versions and deleted tags can remain unseen for up to the configured window.
No release dates are invented for OCI tags, so their age evidence stays unknown.

Anonymous registry pull tokens are keyed by registry and image in a separate
`anonymous-registry-pull-token-v1` representation under the HTTP body budget.
Only the bearer is persisted; the key includes neither credentials nor challenge
query data. Expiry respects `issued_at`, `expires_in`, HTTP Age and a ten-second
margin, capped at one hour; missing expires_in uses 60 seconds. `no-store`
responses are memory-only. A 401 evicts the token and permits a single refresh;
concurrent callers share a refresh. Token values never enter tracing, errors or
fixtures. These are public, anonymous, pull-only credentials obtained without
GitHub/private registry authentication. SQLite stores them unencrypted, so keep
the cache under the same access controls as other batch cache data. Disk cache
reopen avoids both the token request and the initial registry 401 until expiry.

Protocol references: [GitHub secondary limits](https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api#about-secondary-rate-limits),
[Go version queries](https://go.dev/ref/mod#version-queries),
[registry authentication](https://docs.docker.com/reference/api/registry/auth/).


Manifest digest checks use HEAD with the same Accept negotiation as GET and an
independent method-aware cache key. The top-level Docker-Content-Digest header
identifies the multi-architecture index. A missing digest header or unsupported
HEAD (405/501) falls back to GET and, if necessary, hashing its body. Conditional
HEAD 304s preserve the previous digest header. This avoids downloading unused
manifest bodies and Docker Hub documents that HEAD version checks do not consume
pull quota. It does not cache mutable tags beyond upstream freshness.

Successful branch and PR reconciliation stores a verified-write receipt per
branch in the extraction cache. It contains the commit/tree OIDs, base, planned
update identity and canonical PR state (including a CRLF/LF-normalised body hash,
labels and native auto-merge flag), never credentials. Warm runs read a fresh paginated GraphQL
inventory of heads and associated open PRs. An exact head/intent/PR match skips
history, tree and PR reads; any mismatch falls back to normal reconciliation.
Behind-mode base moves invalidate the receipt; conflicts mode retains the
existing behavior when planned file contents are unchanged. Unknown, obsolete
or incomplete inventory data never authorizes a write. Remaining Git Data
reads use conditional GETs and publication retains atomic head/base leases.
GraphQL PR authors are converted to REST bot logins only when their Actor type
is `Bot`; ordinary users with the same name remain distinct. Receipts are saved
per verified branch even when unrelated actions fail or are skipped. At `-vv`,
receipt diagnostics report save/restore counts and the first failed condition
for each branch requiring verification, without PR bodies or credentials.

# Catalog views and release-age policy (departure 50)

Successful catalog views are stored separately from HTTP bodies, scoped to the
credential identity, source mode and mise registry mapping. A dependency with
positive effective `waitHours` reuses a catalog younger than
`clamp(waitHours - 1, 1, 24)` hours: 23 hours for the default 24-hour wait.
Zero keeps the existing HTTP/validator freshness rules. The shortest active
update-class wait is used; dependencies sharing a catalog use the shortest wait
among them. Expiry is strict, and timestamps from the future are revalidated.

Only the catalog is cached. Policy evaluation always uses the current run time,
so an already known held-back release can become eligible during the reuse
window. The versioned report records `metadata.lookup.catalog.fetchedAt` and
`reused` next to the current `evaluatedAt` and held-back evidence. This describes
the cached snapshot, not unseen upstream releases. Fetch time is the client's
successful retrieval/validation time; publication times remain source evidence.
Held-back dashboard rows also show the fetch time, retained across reuse.
Aggregate cache-hit statistics include catalog-view hits.

Repository rechecks, checked PR boxes and selected dashboard controls bypass
reuse. Saved control-to-dependency identities let a checked PR refresh its own
catalogs while unrelated dependencies retain their views. Missing control
history conservatively revalidates the repository's catalogs. Failed lookups are
not cached as views, and registry quota fallbacks do not reset an older
snapshot's age. Catalog prefetch waits for current policy and checkbox state;
speculative digest reads may still overlap extraction.

Mutable Git tags and image digests keep their independent freshness. Reused tag
catalogs never freeze target commits: bounded GraphQL reads resolve only the
currently needed refs, including annotated tags, without revalidating version
lists. Missing objects fall back to the existing per-ref lookup.

Offline regressions cover reopen, expiry/clamping, zero-age rules, credential
isolation, eligibility transitions, targeted PR/dashboard controls, failed
refreshes, and mutable lightweight/annotated tags. Legacy conditional catalog
replay explicitly uses `waitHours = 0`.
