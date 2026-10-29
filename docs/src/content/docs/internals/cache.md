---
title: "SQLite cache"
---
`patchula-cache` uses rusqlite 0.40 with bundled SQLite, so the system
SQLite's version and features never matter. HTTP bodies are stored with Zstd
level 3.

## Layout

A cache is a directory. `Cache::open_dir` creates it and opens
`cache.sqlite` (with SQLite's `-wal` and `-shm` files) inside it. The partial
clones for lockfile tools go in `worktrees/` and the tool store in `tools/`,
unless `PATCHULA_TOOLS_DIR` moves it. The server also keeps `server.lock`
there. The CLI's default directory is shared by every checkout on the machine,
so concurrent local runs use one database through WAL and the busy timeout.
Opening takes an exclusive lock on `cache.lock` while it checks and migrates
the database, and migrations read the schema version inside an immediate
transaction. In the tool store, each tool run holds `patchula-store.lock`
shared, and removing versions (clearing an unproven install, `tools prune`)
holds it exclusive, so no run loses a tool it is using.

## Threads and transactions

Open one `Cache` per batch and clone its handle. One OS thread owns writes,
migrations and corruption recovery, with a queue of 128 jobs. Four more each
own a read-only WAL connection and a 32-job queue; round-robin dispatch runs
SQLite reads and Zstd decoding in parallel without blocking async workers.
Readers keep statements short, so decoding never holds a WAL read transaction.
Reader corruption disables the pool for that handle and retries through the
recovery coordinator.

Read access updates are coalesced and flushed in one transaction before writer
jobs and on shutdown, so reads never take SQLite's write lock. Body inserts and
release merges are transactional. The database uses WAL, a five-second busy
timeout and `synchronous=NORMAL`, since cache data is reproducible. Only one
batch should own a cache path during corruption recovery: quarantine is not
coordinated across processes.

## HTTP entries

Keys normalise method case, the URL (authority, default port, path, fragment)
and header names. Query order is kept, because repeated parameters can be
order-sensitive. Callers choose the vary-relevant headers; for authenticated
data they include a digest of the credential namespace, never a raw token.
An entry stores the status, validators, selected response headers such as
`Link`, the compressed body, fetch time, expiry and Cache-Control hints.
Expiry keeps validators. An invalid compressed body is a miss, and its
validators are never sent.

Large registry bodies use a versioned local `representation` in the key. npm,
Cargo sparse indexes, PyPI and GitHub REST release heads store a derived
document with only the fields the Count reads, never the raw response.
Derivation runs on `spawn_blocking` with a four-job limit. A 304 updates only
metadata and reuses the compact body. A format change needs a new
representation version, so raw and derived namespaces never share validators.

The default 256 MiB budget counts compressed HTTP body bytes. An access clock
approximates LRU and evicts whole HTTP rows, validators included, until the
cache fits, also on open after the budget is lowered. Release lists and
extraction data are outside the budget, so it does not cap the file size.

## Releases and extractions

Release order is the upstream's newest-first order, never inferred from semver
or dates. `merge_releases` starts at the head and stops at the first item
already stored. Later pages of the same scan pass the previous page's last
inserted version to `merge_release_page` as `after`, which tells a refresh
prefix apart from initial pagination. The newest item is also kept in
`release_heads`. Extraction keys include the repository, the default-branch
commit and the resolved config hash.

## Schema and corruption

Migrations are transactional and tracked with SQLite's `user_version`. A
migration that retires a representation costs one unconditional fetch per
affected registry. A database from a future schema is rejected untouched.

Opening runs `quick_check`. `SQLITE_CORRUPT`, `SQLITE_NOTADB` or a failed
check closes the connection, moves the file and its WAL and SHM sidecars to a
unique `.corrupt-<timestamp>` name, warns through tracing and rebuilds.
Corruption during an operation rebuilds and retries it once. If that fails, the
cache warns and falls back to memory so the batch keeps running. Permission,
busy and future-schema errors are not treated as corruption. The application
must initialise tracing for the warnings to show.

## Host limits

- `--concurrency` sets repository scan parallelism (default 8).
- Registry requests use per-host semaphores, 16 each by default, independent of
  repository work. `--host-concurrency api.github.com=8` overrides one host;
  repeat it for others. Limits must be 1–100.
- GitHub REST and GraphQL share one governor, sized by the `api.github.com`
  host limit. That is below GitHub's 100 simultaneous requests, but does not
  by itself avoid its point and CPU
  [secondary limits](https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api#about-secondary-rate-limits).
- The shared reqwest pool keeps up to 32 idle connections per host.
- A registry 429 sets a cooldown for that host and is retried at most three
  times, honouring `Retry-After` or backing off 1, 2 and 4 seconds. Waits over
  60 seconds and exhausted retries fail, and queued callers wait out the
  cooldown. Docker Hub pull-quota errors are returned at once.
- Only successful responses are cached, so a rate-limited pagination attempt
  never becomes a complete catalog.

## Freshness floors

Two flags keep validator-free catalogs from being downloaded on every run. The
user-facing summary is in [sources](/guide/sources/#fetching-less).

- `--registry-freshness-secs 3600` applies to OCI `tags/list` pages with
  neither ETag nor Last-Modified. After expiry every page is fetched again, so
  additions, deletions and retargeting anywhere are seen. Manifest and tag
  digests keep their upstream policy.
- `--go-freshness-secs 3600` applies to public `proxy.golang.org` `@v/list`
  and `@latest`, even when the upstream max-age is shorter. `.info` documents
  keep the upstream policy.

Zero restores upstream freshness at once, also on an existing cache. Floors
count from the original fetch time and are never extended by reads. `no-cache`
and `no-store` still apply. No other source gets a floor. New versions and
deleted tags can go unseen for up to the window.

## Registry pull tokens

Anonymous registry pull tokens are keyed by registry and image, under the HTTP
body budget. Only the bearer is stored. Expiry follows `issued_at`,
`expires_in` and HTTP `Age`, less a ten-second margin and capped at one hour;
a missing `expires_in` means 60 seconds. A 401 evicts the token and allows one
shared refresh.

:::caution
These tokens are public, anonymous and pull-only, and never enter tracing,
errors or fixtures, but SQLite stores them unencrypted. Keep the cache under
the same access controls as other batch cache data.
:::

## Manifest digests

Digest checks send HEAD with the same `Accept` negotiation as GET and their
own cache key. The top-level `Docker-Content-Digest` identifies the
multi-architecture index. A missing header or an unsupported HEAD (405 or 501)
falls back to GET, hashing the body if needed. Docker Hub does not count HEAD
checks against the pull quota.

## Write receipts

A successful branch and PR reconciliation stores a receipt per branch in the
extraction cache: the commit and tree OIDs, the base, the planned update and
the canonical PR state (a line-ending-normalised body hash, labels and the
auto-merge flag), never credentials. Warm runs read a fresh GraphQL inventory
of heads and open PRs, and an exact match skips history, tree and PR reads.
Any mismatch, or unknown or incomplete inventory data, falls back to full
reconciliation. In behind mode a base move invalidates the receipt. At `-vv`,
diagnostics report save and restore counts and the first failed condition per
branch.

## Catalog views and release age

Successful catalog views are stored apart from HTTP bodies, scoped to the
credential and source mode. A dependency with a positive effective `waitHours`
reuses a catalog younger than `clamp(waitHours - 1, 1, 24)` hours, 23 hours
for the default 24-hour wait. Zero keeps the usual HTTP freshness. Dependencies
sharing a catalog use the shortest wait among them. A release that must wait a
day does not need a fresh catalog on every run.

Only the catalog is cached; policy always uses the current time, so a known
held-back release can become eligible during reuse. The report records
`metadata.lookup.catalog.fetchedAt` and `reused` next to `evaluatedAt`.
Repository rechecks, checked PR boxes and selected dashboard controls bypass
reuse for the dependencies they name. Failed lookups are never cached as
views. Mutable Git tags and image digests keep their own freshness: bounded
GraphQL reads resolve the refs currently needed without revalidating the lists.

## mise listings

What `mise ls-remote --json` printed is stored in the extraction table, since
it has no validators. The key holds the tool (and a version prefix such as
`lts`), the credential scope, whether mise may read the mise-versions mirror
and the pinned mise version. A listing younger than an hour is reused without
starting mise. A manual retry replaces it, and only successful lookups are
stored.
