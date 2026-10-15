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
