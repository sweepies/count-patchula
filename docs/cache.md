# SQLite cache

`rustovate-cache` uses rusqlite 0.40 with bundled SQLite. rusqlite is a mature,
actively maintained synchronous wrapper with transactions and explicit error
codes; bundling avoids system SQLite version/feature drift. Zstd level 3 stores
HTTP bodies with a small CPU cost and reuses the HTTP stack's compression
dependency. The pinned Zig mise tasks build the native dependencies.

Open one Cache per batch and clone its handle. A dedicated OS thread owns the
connection. A bounded Tokio mpsc queue (128 jobs) and per-job oneshot reply
serialize database access and compression without blocking async workers or
holding an async mutex across database I/O. This is simpler than a pool for the
small batch and SQLite's single writer; reads are also queued. If profiling
finds that reads dominate, a read pool can be added behind the same API. Dropping
the last handle closes the queue; the thread drains queued work and closes the
connection. Cancelled callers may still complete their submitted cache writes.

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
compressed body, fetched-at, expiry and raw Cache-Control hints. Expiry does not
discard validators. Bodies that cannot be decompressed are evicted as misses.

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
three cache domains; v2 adds the HTTP access clock/LRU index. Tests create a
populated v1 database and verify that v2 preserves its entries. A future schema
is rejected without modifying or quarantining its database.

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
