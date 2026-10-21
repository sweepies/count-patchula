# GitHub client

Create one `patchula_github::Client` per batch and clone it for repository
tasks. `Client::new` builds one shared reqwest pool through patchula-http;
`with_transport` accepts replay without constructing a network Client. The
caller supplies an existing token (for example, read GITHUB_TOKEN in the batch
runner); this crate does not mint tokens or inspect the environment.

reqwest 0.13 uses HTTP/2 negotiation, rustls, gzip/brotli/zstd decompression,
10-second connect, 30-second read, 60-second total request timeouts, and a
90-second idle pool timeout. User-Agent is `patchula/<workspace version>`.
The transport disables automatic protocol retries so the governor owns request
accounting. It disables automatic redirects; REST GET follows up to ten
same-origin redirects through the governor. Cross-origin URLs and embedded URL
credentials are rejected before sending the caller's token. `api_url` can
select an alternate HTTPS API root; GraphQL is resolved as `graphql` relative
to that root (Enterprise deployments with distinct REST/GraphQL roots will need
an explicit second endpoint option before use).

Authorization headers are marked sensitive. Client Debug/Display, Response
Debug and errors omit tokens and bodies. Network failures discard reqwest's
URL-bearing error details. Replay errors retain the testkit's sanitized
method/URL diagnostic. Optional request diagnostics use an explicit allowlist of
fields; headers, bodies and query strings never reach the logger. The CLI enables
them with `-vv` or `PATCHULA_LOG=debug`, filtering out dependency HTTP logs even
at trace level. Tests verify
credential/body confidentiality, including rejected requests and 403 responses.

Every REST GET checks SQLite for validators and sends If-None-Match and/or
If-Modified-Since when available, even after expiry. A 304 reuses the cached
status/body and merges headers, preserving a cached pagination Link if GitHub
omits it. Updated cache hints and validators are persisted. No-store and Vary:*
responses are not retained. Keys include normalized URL, Accept, API version,
Vary headers and a SHA-256 token namespace, so data from different credentials
cannot be reused. Tokens are never stored in keys. Since request headers are
fixed by this API, the base representation also supplies Vary discovery metadata;
a distinct Vary key may duplicate a body and both copies count toward eviction.
Only needed response metadata is cached, excluding cookies and authorization.

The default in-flight cap is **4**, shared across REST and GraphQL. It balances
read throughput for the small batch with headroom below GitHub's shared limit of
100; GitHub recommends serial requests to minimize secondary limits, so callers
can set `concurrency=1` if their workload needs that. It is not a guarantee against
secondary throttling. Mutating requests are not exposed by this Phase 0 client.

The governor records x-ratelimit-limit/remaining/used/reset by resource (core,
search and graphql), reserves known remaining capacity for in-flight requests,
and waits until exhausted windows reset. Same-window out-of-order responses
cannot replenish reservations. A 304 refunds its reservation. Reset waits add a
one-second boundary margin. Secondary 403/429 responses (Retry-After, documented
secondary/abuse messages, or GraphQL RATE_LIMITED errors) impose a cooldown shared
by all tasks. Retry-After seconds or HTTP dates are honored; otherwise delays
start at 60 seconds and double. Primary reset and Retry-After use the longer
required wait. The default is three retries after the original request and a
one-hour per-wait ceiling: longer requested waits fail, never retry early.
Ordinary permission errors are not retried. Already in-flight requests may
finish while a newly observed secondary cooldown blocks subsequent attempts.

`stats()` returns serializable request, 304 and retry counts, summed task wait
milliseconds (queueing plus rate waits), and primary header snapshots. Requests
count actual transport attempts, not cache operations. Tests use Tokio's paused
clock to verify minute-scale backoff without wall-clock sleeps.

Request lines include method, host, sanitized path, status, cache outcome,
decoded body bytes, governor queue/rate-wait milliseconds and total milliseconds.
Fresh registry hits are explicitly marked. Freshness reasons and remaining TTL
explain transitions to conditional requests. The network transport records the
negotiated HTTP protocol. reqwest does not expose per-request connection/TLS reuse
or connect/TLS durations here; these fields say `unavailable`. No values are
inferred from response latency.

REST pagination follows Link rel=next, retaining query URLs instead of inventing
page numbers. Multiple Link fields, commas in URI/quoted parameters, relative
links, repeated pages and page budgets are handled. GraphQL pagination inserts
the returned endCursor into `$cursor`; callers provide a query, variables and
connection extractor. `GraphqlPage::from_connection` parses nodes/pageInfo at a
JSON pointer. Both helpers call a stop predicate on each page before fetching
another; they return the stopping page too, so a cache merge can consume its
new prefix and ignore its known suffix. Repeated/missing cursors fail instead
of looping. The default safety budget is 1,000 pages.

`fixtures/github/public-{releases,tags,graphql}.json` were recorded by patchula
from public GitHub endpoints using an existing caller token. Releases includes
a real 200/304 exchange. Tags and GraphQL stop on the first nonempty page;
simulated two-page cassettes prove that pagination stops at known history even
when GitHub advertises another page. Other simulations cover rate limits,
permission errors and cache policy. All replay by default in CI. See
[recording instructions](http-testing.md); use `mise run fixtures:record` with
GITHUB_TOKEN or GH_TOKEN already in the environment, then `mise run test` to
replay and `mise run fixtures:check` to scan. Recording replaces scenarios, so
inspect the fixture diff and provenance before committing.

References read:
[GitHub best practices](https://docs.github.com/en/rest/using-the-rest-api/best-practices-for-using-the-rest-api),
[REST limits](https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api),
[REST pagination](https://docs.github.com/en/rest/using-the-rest-api/using-pagination-in-the-rest-api),
[GraphQL pagination](https://docs.github.com/en/graphql/guides/using-pagination-in-the-graphql-api),
[GraphQL limits](https://docs.github.com/en/graphql/overview/rate-limits-and-query-limits-for-the-graphql-api),
[reqwest builder](https://docs.rs/reqwest/0.13.5/reqwest/struct.ClientBuilder.html),
[Tokio Semaphore](https://docs.rs/tokio/latest/tokio/sync/struct.Semaphore.html),
[paused test time](https://docs.rs/tokio/latest/tokio/time/fn.pause.html),
[SHA-2](https://docs.rs/sha2/latest/sha2/),
[httpdate](https://docs.rs/httpdate/latest/httpdate/).
