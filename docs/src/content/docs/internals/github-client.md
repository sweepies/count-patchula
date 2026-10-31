---
title: "GitHub client"
---
Create one `patchula_github::Client` per batch and clone it for repository
tasks; clones share the token, connection pool, governor and stats.
`Client::new` takes a token and builds one shared reqwest pool through
patchula-http, and `with_transport` injects replay without a network client.
`with_credentials` takes refreshable credentials, such as an App installation,
and keeps the pool, governor and cache namespace across token renewals. The
client never reads the environment; the CLI and server supply the credentials.

## App tokens

`app::App` holds a GitHub App's ID and private key. It signs RS256 JWTs,
backdated 60 seconds and valid for ten minutes, and uses them to look up a
repository's or organisation's installation, list installations and read the
App's bot login, client ID and webhook URL. `App::installation` returns
credentials that mint installation access tokens on demand. A token is reused
until two minutes before it expires, and one lock per App means simultaneous
requests never mint twice. App requests always go to `api.github.com`. No
credential reaches SQLite.

## Transport

reqwest 0.13 uses HTTP/2 negotiation, rustls, gzip, brotli and zstd
decompression, a 10-second connect, 30-second read and 60-second total request
timeout, and a 90-second idle pool timeout. User-Agent is
`patchula/<workspace version>`. Automatic retries are off so the governor owns
request accounting, and automatic redirects are off too: a REST GET follows
redirects itself, up to five, HTTPS only. A redirect to another origin has its
credentials stripped, and one with embedded credentials or a loop is refused.
`api_url` can select another HTTPS API root, and GraphQL resolves as
`graphql` relative to it, so an Enterprise deployment whose GraphQL root is
elsewhere is not supported yet.

Authorization headers are marked sensitive, and Debug output, Display output
and errors omit tokens and bodies. Request diagnostics use an allowlist of
fields, so headers, bodies and query strings never reach the logger; the CLI
enables them with `-vv` or `PATCHULA_LOG=debug`.

## Conditional reads

Every REST GET looks up validators in SQLite and sends `If-None-Match` or
`If-Modified-Since` when it has them, even after expiry. A 304 reuses the
cached status and body and merges headers, keeping a cached pagination `Link`
if GitHub omits it. `no-store` and `Vary: *` responses are not kept. Keys
include the normalised URL, `Accept`, the API version, `Vary` headers and a
SHA-256 namespace of the credential, so data is never shared between
credentials; tokens are never stored in keys. Cookies and authorization are
never cached.

## Writes

`post`, `patch` and `put` send JSON to the REST API, and `graphql` sends
queries and mutations. The Count uses them to create Git objects, publish refs
with `updateRefs`, and open, edit and close PRs, the dashboard issue and
auto-merge. Writes never enter the cache. Any response other than a 2xx is an
error, and only documented rate-limit refusals are retried: a transport or
server failure may already have written, so the next run reconciles instead.

Every REST request other than GET, HEAD and OPTIONS, and every GraphQL
document whose text starts with `mutation` (or whose body cannot be parsed),
runs one at a time across all clones, with a one-second pause after
each and at most 500 an hour, following GitHub's
[best practices](https://docs.github.com/en/rest/using-the-rest-api/best-practices-for-using-the-rest-api)
and content-creation limit. A mutation document must start with the
`mutation` keyword, with no leading comment, or it is paced as a read.

## Rate limits

The library's default in-flight cap is **4**, shared by REST and GraphQL;
callers can set `concurrency=1` for serial requests, which GitHub recommends
to minimise secondary limits. The engine sizes it from the `api.github.com`
host limit, 16 by default (see [host limits](/internals/cache/#host-limits)).
No cap guarantees against secondary throttling.

The governor tracks the `x-ratelimit-*` headers per resource (core, search and
graphql), reserves known remaining capacity for in-flight requests and waits
until an exhausted window resets, plus one second. A 304 refunds its
reservation.

Secondary 403 and 429 responses (`Retry-After`, documented secondary or abuse
messages, or GraphQL `RATE_LIMITED` errors) set a cooldown shared by all tasks.
`Retry-After` seconds or dates are honoured; otherwise the delay starts at 60
seconds and doubles. When a primary reset and `Retry-After` both apply, the
longer wait wins. By default a request is retried three times, and a requested
wait over one hour fails rather than retrying early. Ordinary permission errors
are not retried. Requests already in flight may finish while a new cooldown
blocks later ones.

## Stats and request lines

`stats()` returns transport attempts, 304s, retries, summed queue and rate
wait, and the primary limit snapshots. Debug request lines show the method,
host, sanitised path, status, cache outcome, body bytes, wait and total time,
and the negotiated protocol. reqwest does not expose connection or TLS reuse or
timings, so those fields say `unavailable` rather than being guessed.

## Pagination

REST pagination follows `Link` `rel=next` URLs as given, never inventing page
numbers. GraphQL pagination puts the returned `endCursor` into `$cursor`, and
`GraphqlPage::from_connection` reads nodes and `pageInfo` at a JSON pointer.
Both call a stop predicate on each page before fetching the next and return the
stopping page, so a cache merge can take its new prefix and ignore the known
rest. Repeated pages or cursors fail instead of looping, and the default budget
is 1,000 pages.

## Fixtures

`fixtures/github/public-{releases,tags,graphql}.json` were recorded from public
GitHub endpoints; releases include a real 200 and 304 exchange. Simulated
cassettes cover two-page pagination that stops at known history, rate limits,
permission errors, redirects and cache policy. Tests use Tokio's paused clock
for minute-scale backoff. [HTTP fixtures](/internals/http-testing/) covers
recording.
