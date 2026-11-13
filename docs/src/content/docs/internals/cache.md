---
title: "SQLite cache"
description: "How the SQLite cache is laid out: HTTP entries, releases and state, registry tokens, host limits and freshness floors."
---
`patchula-cache` uses rusqlite 0.41 with bundled SQLite, so the system
SQLite's version and features never matter. HTTP bodies are stored with Zstd
level 3.

## Layout

A cache is a directory. `Cache::open_dir` creates it and opens
`cache.sqlite` (with SQLite's `-wal` and `-shm` files) inside it. The partial
clones for lockfile tools go in `worktrees/` and the tool store in `tools/`,
unless `PATCHULA_TOOLS_DIR` moves it. The package caches the tools share
between runs go in `tool-caches/`, partitioned by the credentials a run
holds: `public` for a run without any (safe to share because repositories
that commit credentials are refused), `private-<hash of the entries'
scopes>` otherwise, with no read-only lower layer between them. Pruning is
throttled per cache root and takes `tool-caches/prune.lock` so concurrent
prunes never over-evict. The server also keeps `server.lock`
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
The conditional-GET protocol the registry cache and the GitHub client share —
keys, entries, validators, 304 merges, `Retry-After` and bounded decoding —
lives in `patchula_http::conditional`; each client keeps its own freshness
policy, sender and error mapping on top.
An entry stores the status, validators, selected response headers such as
`Link`, the compressed body, fetch time, expiry and Cache-Control hints.
Expiry keeps validators. An invalid compressed body is a miss, and its
validators are never sent.

Large registry bodies use a versioned local `representation` in the key. npm,
Cargo sparse indexes, PyPI and GitHub REST release heads store a derived
document with only the fields the Count reads, never the raw response.
Derivation runs on `spawn_blocking`; a successful derived response streams
straight into its derive worker, so the raw document is never held whole.
The request's host permit is held until its body is read, so in-flight bodies
stay inside the host limit. Reads that arrive already buffered derive under a
four-job limit. A 304 updates only metadata and reuses the compact body. A
derived representation is `name@v{VERSION}`, where the version belongs to the
transform's own code rather than the release: a new release keeps the cached
entries of every transform whose code did not change, and a change to one
transform misses only that representation. `tests/derived-versions.tsv`
guards the contract — it hashes the source files and locked dependency
versions that produce each representation's bytes, and the owning crate's
parity test fails when the code moves without a version bump; the
maintenance step is to bump the `Derived`'s version and update the row in
the same commit. A derive may only read code in its row's listed files, and
since whole files are hashed a formatting or comment edit in one forces a
bump too — batch cosmetic edits in those files. The parity test also scans
the crate's sources for `Derived::new("<name>"` (and the pull-token
representation constant), so a representation cannot exist without a row,
and a listed file that is renamed fails naming the row and the missing
file. Entries written
under the retired `name+build` scheme are never read again and are deleted
when the cache opens; raw and derived namespaces never share validators.

The default 256 MiB budget counts compressed HTTP body bytes. An access clock
approximates LRU and evicts whole HTTP rows, validators included, until the
cache fits, also on open after the budget is lowered. Release lists and
state values are outside the budget, so it does not cap the file size.

## Releases and state

Release order is the upstream's newest-first order, never inferred from semver
or dates. `merge_releases` starts at the head and stops at the first item
already stored. Later pages of the same scan pass the previous page's last
inserted version to `merge_release_page` as `after`, which tells a refresh
prefix apart from initial pagination. The newest item is also kept in
`release_heads`.

Typed values the Count keeps between runs (hints, extractions, catalog views,
receipts and the like) live in `state`; the server queue, installations and
repositories keep their own tables. A state row is one value, keyed by a
namespace and a canonical-JSON scope. A `State` implementation names its
`NAMESPACE` and `VERSION`, forming `name@v{VERSION}`, and declares the `Scope` struct
whose named fields say what the entry is about. A value that depends on this
build's decoding or extraction code sets `PER_BUILD`, and the build version
joins the namespace, so a release never reads another release's entries.
Opening the cache deletes per-build rows written by any other version.
Values are JSON. A stored value that fails to decode is a miss, logged at
debug level; database errors remain errors. Each call site owns its value and
scope types, so a change to a stored shape bumps `VERSION` or `NAMESPACE`
rather than silently reusing entries written for the old one. The exception
is a shape that reads both ways: auto-merge history rows went from whole REST
pull requests to the five fields its readers use, and each shape decodes as
the other, so they stay at version 1.

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

- `concurrency` in the [settings](/reference/settings/), or `--concurrency`,
  sets repository scan parallelism (default 8).
- Registry requests use per-host semaphores, `http.defaultHostConcurrency`
  (32) each, independent of repository work. `http.hostConcurrency` overrides
  single hosts, such as `["api.github.com"] = 8`. Limits must be 1–100.
- GitHub REST and GraphQL share one governor, sized by the `api.github.com`
  host limit. Without one, a CLI run takes the default host limit capped at
  16 and the server keeps 4. That is below GitHub's 100 simultaneous requests, but does not
  by itself avoid its point and CPU
  [secondary limits](https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api#about-secondary-rate-limits).
- The shared reqwest pool keeps up to 32 idle connections per host.
- A registry 429 sets a cooldown for that host and is retried at most three
  times, honouring `Retry-After` or backing off 1, 2 and 4 seconds. Waits over
  60 seconds and exhausted retries fail, and queued callers wait out the
  cooldown. Docker Hub pull-quota errors on manifest GETs are returned at once
  and hold back manifest GETs for the quota window (six hours when Docker Hub
  names none). HEAD and tag-list requests don't use the pull quota, so their 429
  cools the whole host only for a window the counter names; otherwise it is an
  ordinary throttle.
- Overlapping identical registry and derived reads share one request. Each
  shared answer lives only while a caller waits on it, so memory follows the
  reads in flight, not every URL read since the server started.
- Only successful responses are cached, so a rate-limited pagination attempt
  never becomes a complete catalog.

## Freshness floors

Three settings keep answers reusable past what upstream validators alone would
allow. The user-facing summary is in [sources](/guide/sources/#fetching-less).

- `http.registryFreshnessSecs` (3600) applies to OCI `tags/list` pages with
  neither ETag nor Last-Modified. After expiry every page is fetched again, so
  additions, deletions and retargeting anywhere are seen. Manifest and tag
  digests keep their upstream policy.
- `http.goFreshnessSecs` (3600) applies to public `proxy.golang.org` `@v/list`
  and `@latest`, even when the upstream max-age is shorter. `.info` documents
  keep the upstream policy.
- `http.advisoryFreshnessSecs` (3600) keeps the `(id, modified)` pairs OSV's
  `querybatch` lists for each `{ecosystem, package, version}` as `osv-listing`
  state. Inside the window the listing answers locally and only stale or
  missing queries are posted; a failed batch stores nothing, and a
  revalidation (an explicit recheck, dashboard tick or retry) always asks OSV.

Zero restores upstream freshness at once, also on an existing cache. Floors
count from the original fetch time and are never extended by reads. `no-cache`
and `no-store` still apply. No other source gets a floor. New versions,
deleted tags and new advisories can go unseen for up to the window.

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

## Registry credentials

A response read with a [private registry](/guide/sources/#private-registries)
entry's credentials is keyed by that entry's prefix and a one-way digest of its
credentials as well as the request, so a read without the entry, with another
one, or after its secret or account changes never sees it. The key never holds
the secret. Such responses follow the registry's `no-store` and `Vary: *`,
digest-addressed ones included. A response that varies on authorization is
kept only when the key names who authorized it: an operator entry's scope
does, a token the registry issued does not. Tokens a registry issues to an account
stay in memory and are never written to the cache.

## Manifest digests

Digest checks send HEAD with the same `Accept` negotiation as GET and their
own cache key. The top-level `Docker-Content-Digest` identifies the
multi-architecture index. A missing header or an unsupported HEAD (405 or 501)
falls back to GET, hashing the body if needed. Docker Hub does not count HEAD
checks against the pull quota.

## Write receipts

A successful branch and PR reconciliation stores a `verified-write-receipts`
state entry per repository and branch prefix: the commit and tree OIDs, the
base, the planned update and
the canonical PR state (a line-ending-normalised body hash, labels and the
auto-merge flag), never credentials. Warm runs read a fresh GraphQL inventory
of heads and open PRs, and an exact match skips history, tree and PR reads.
A stored receipt is proof every branch the Count wrote shared the prefix, so
the refs walk passes it as `refs(query:)`: GitHub's filter is a
case-insensitive substring match, and the client keeps only the names that
start with the prefix. The walk narrows only while a `receipts-prefix` state
entry, written beside every save, names the repository's current
`prs.branch`: a prefix flip away and back leaves the older prefix's receipts
in place but no longer proves the other branches, so the run walks every
branch once and the restore that verifies the receipts again marks them.
Every branch the receipts name must also appear in the answer — one that
does not, renamed outside the prefix while its pull request stayed open or
deleted, takes the full write path rather than settling around a branch it
cannot see. No receipt — a first run, a cold cache or a changed prefix —
walks every branch as before, and that run is a full write whose cleanup
lists every branch itself. One consequence remains: a copy of a Count branch
under a name outside the prefix has no receipt, so it is invisible to a warm
run and is deleted by the next full write instead.
Any mismatch, or unknown or incomplete inventory data, falls back to full
reconciliation. In behind mode a base move invalidates the receipt. At `-vv`,
diagnostics report save and restore counts and the first failed condition per
branch.

The inventory's issue walk filters `issues` by `filterBy.createdBy`, and the
login it asks for is remembered per credential scope as `inventory-actor`
state: the first page's `viewer.login` is stored as soon as it is read, even
when the walk goes on to fail. A read whose answer's `viewer.login` does not
match the stored hint retries unfiltered once, so a wrong or stale entry
costs one request and never narrows the issue set wrongly.

Branch cleanup keeps a per-build `not-ours` entry per repository and commit
OID whose message records no base: such a commit can never be the Count's,
so its branch is not read again while it points there. Nothing else about
ownership is cached; a compare, an author check or a signature check runs
on every full write.

## Catalog views and release age

Successful catalog views are stored as `catalog` state, scoped to the
package, whether the versions host may stand in and the credential scope. A
dependency with a positive effective `waitHours`
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

What `mise ls-remote --json` printed is stored as `mise-ls-remote` state, since
it has no validators. The scope holds the tool (and a version prefix such as
`lts`), the credential scope, whether mise may read the mise-versions mirror
and the pinned mise version. A listing younger than an hour is reused without
starting mise. A manual retry replaces it, and only successful lookups are
stored.
