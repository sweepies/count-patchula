---
title: "GitHub client"
description: "The patchula-github client: shared connection pool, GitHub App tokens, conditional reads, writes, rate limits and pagination."
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
requests never mint twice. When GitHub refuses a request with 401, the client
asks the credentials to forget the token it sent (`Credentials::invalidate`)
and sends the request once more with a fresh one; a token another request
minted in the meantime is kept. GitHub runs nothing it refuses with 401, so
the retry is safe for writes too, and it does not count against the
rate-limit retries. A second 401 is returned. Credentials that cannot mint
another token answer `None` and are not retried. App requests resolve against the configured API
root, so an Enterprise deployment's App endpoints work too. No credential
reaches SQLite.

Installation credentials can also mint a token scoped to one repository:
`Credentials::scoped_token("owner/name", permissions)` reads the repository's
numeric id with the installation token (`GET /repos/{owner}/{repo}`), then
posts `{"repository_ids": [<id>], "permissions": …}` to
`app/installations/{id}/access_tokens`. `Permissions::MetadataRead`
(`{"metadata": "read"}`) is what the actions.lock tool and mise's public
listings get: it reads public repositories, whose releases and tags any
token may list, and no private repository's contents, the minted one's
included. `Permissions::ContentsRead` (`{"contents": "read", "metadata":
"read"}`) is what mise gets for a listing kept for the repository alone,
enough to list the repository's own releases and tags.
`Credentials::repository_id` answers the numeric id the mint names, which
keys what such a listing serves. GitHub answers the token with the
`permissions`, `repositories` and `repository_selection` it minted. The full
installation mint stays body-less. Scoped tokens share the cache and the
single-flight lock, keyed by (installation, repository, permission set), so
two repositories mint separately and a fuller token never answers a narrower
mint. Only App installation credentials answer `scoped_token`,
`repository_id` and `mints_scoped_tokens`; every other credential returns
`None`, and the actions.lock tool and mise use the run's own token. A
real-installation proof lives ignored in `crates/github/tests/app.rs`: set
`PATCHULA_TEST_APP_ID`, `PATCHULA_TEST_APP_KEY_FILE`,
`PATCHULA_TEST_INSTALLATION`, `PATCHULA_TEST_REPO` and
`PATCHULA_TEST_OTHER_PRIVATE_REPO`, then run
`mise exec -- cargo test -p patchula-github --test app -- --ignored`.

## Token scope

The cache namespace of a client built from a token is a SHA-256 of the
token, which a token minted per run changes every run. `Client::identify`
replaces it, before the client is cloned, for an installation token (`ghs_`,
which includes an Action's `GITHUB_TOKEN`). It reads, live and never from the
cache:

- `GET /installation/repositories?per_page=100`, at most ten pages, for
  `repository_selection` and the repository ids;
- GraphQL `viewer { databaseId }`, the App's bot user;
- `GET /repos/{repository}/commits?per_page=1` on a repository in the set,
  a private one when there is one, which needs `contents: read`.

The scope is then `{api root}|app:{bot id}/repos:{sha256 of the sorted ids}`
whatever the selection, so a token narrowed to some repositories never shares
the full installation's entries even if GitHub called its selection `all`.
Any non-2xx answer, a missing or unknown `repository_selection` or one that
changes between pages, a listing past the page cap, empty, naming a
repository twice or whose count disagrees with `total_count`, a viewer
without an id, or a failed contents read keeps the token hash. An
installation with more than a thousand repositories therefore keeps it too.
Personal, OAuth and user tokens keep it without a request. The [threat
model](/internals/threat-model/#cache-scopes) says why the scope may leave
permissions out.

`unrevalidated::ENDPOINTS` lists every endpoint whose cached answer is
returned without asking GitHub: Git blobs, recursive trees and commits, under
`repos/{owner}/{repo}` or a renamed repository's `repositories/{id}`. A read
that skips revalidation on anything else fails with `Error::Unrevalidated`,
and a unit test checks the path of every `get_immutable` call in the
workspace against the list.

## Transport

reqwest 0.13 uses HTTP/2 negotiation, rustls, gzip, brotli and zstd
decompression, a 10-second connect, 30-second read and 60-second total request
timeout, and a 90-second idle pool timeout. User-Agent is
`patchula/<workspace version>`. Automatic retries are off so the governor owns
request accounting, and automatic redirects are off too: a REST GET follows
same-origin redirects itself, at most ten requests in all. Cross-origin URLs
and URLs with embedded credentials are rejected before the token is sent.
`api_url` can select another HTTPS API root, and GraphQL resolves as
`graphql` relative to it, so an Enterprise deployment whose GraphQL root is
elsewhere is not supported yet.

Authorization headers are marked sensitive, and Debug output, Display output
and errors omit tokens and bodies. Request diagnostics use an allowlist of
fields, so headers, bodies and query strings never reach the logger; the CLI
enables them with `-vv` or `PATCHULA_LOG=debug`.

## Conditional reads

Every REST GET looks up validators in SQLite and sends `If-None-Match` or
`If-Modified-Since` when it has them, even after expiry. The exception is a
read whose answer cannot change, a Git blob, commit or tree named by its SHA:
`get_immutable` returns a cached success without a request. `blobs` reads many at once: those
the cache holds need no request, the rest go through GraphQL fifty to a query,
and a blob counts from GraphQL only when its text hashes to the SHA asked for.
Binary, truncated or mismatched blobs come through REST instead. Either way the
blob is cached as its REST representation. A cached blob counts only when it
hashes to its SHA too; one that does not is dropped and read again. A 304 reuses the
cached status and body and merges headers, keeping a cached pagination `Link`
if GitHub omits it. `no-store`, `Vary: *` and `Vary: User-Agent` responses are
not kept. Any other `Vary` names a header the key holds (`Accept`, the API
version), one the client never sends, `Accept-Encoding` (bodies are stored
decoded) or authorization, which the key's credential namespace covers: a
refreshed token is the same installation. So `Vary` never splits the key. Keys include the normalised URL, `Accept`, the API
version and a SHA-256 namespace of the credential's scope (see [token
scope](#token-scope)), so data is shared only between credentials with the
same reach; tokens are never stored in keys. Cookies and
authorization are never cached.

## Writes

`post`, `patch` and `put` send JSON to the REST API, and `graphql` sends
queries and mutations. The Count uses them to create Git objects, publish refs
with `updateRefs`, and open, edit and close PRs, the dashboard issue and
auto-merge. Writes never enter the cache. Any response other than a 2xx is an
error, and only documented rate-limit refusals are retried: a transport or
server failure may already have written, so the next run reconciles instead.

Every REST write (POST, PATCH and PUT), and every GraphQL document
that does not parse as a query (comments and whitespace skipped, so a
mutation behind a leading comment counts; shorthand `{ … }` is a query),
runs one at a time across all clones of a client, paced by a token bucket of
`github.writeBurst` tokens (12 by default) refilling one a second — the
first writes of a run go out without a pause, and no minute holds more than
`writeBurst` + 60 (72 by default, at most 79) — and at most 500 an hour,
inside GitHub's
[best practices](https://docs.github.com/en/rest/using-the-rest-api/best-practices-for-using-the-rest-api)
and content-creation limit. Any secondary-rate-limit answer cuts the budget
to one token for the next hour, the one write a second GitHub asks for. A
mutation document must start with the `mutation` keyword, with no leading
comment, or it is paced as a read. Each installation's engine has its own
client, so the server's batches for different installations pace their
writes separately.

## Typed responses

`get_fresh`, `get_immutable`, `post`, `patch`, `put` and `graphql` are generic over the type
they decode into, like `rest_pages` already was. A response that does not
decode is an `Error::Decode`, which names the operation and the field path
only: serde's message is never kept, because a type-mismatch message can
quote response text that would reach dashboards and public pages.
`patchula_github::model` holds the REST resource types the lifecycle,
dashboard, batch, server and discovery listings read: deserialize only,
unknown fields ignored, and `Option` only where GitHub's OpenAPI description
documents null or a missing field must fail closed.
`model::Page<T>` decodes a list element by element: an element that fails
marks the page incomplete and stays out of `items`, so a partial read blocks
every action that depends on the whole set rather than silently acting on
the readable subset.

Static GraphQL documents — the inventory, status-check, ref-update,
auto-merge, repository-head and releases operations — live as `.graphql`
files beside the code that issues them. `graphql_client`'s `GraphQLQuery`
derive checks each against the vendored schema at
`crates/github/graphql/schema.docs.graphql` and generates the response types:
a schema-required field generates a plain field, a nullable one an `Option`,
and interface and union selections generate enums decoded on `__typename`,
which the document must therefore select. `mise run github:schema` refreshes
the vendored schema. Dynamic documents — the alias batches that read many
objects in one query — keep hand-written response types, since generated
code cannot name them.

## Rate limits

The library's default in-flight cap is **4**, shared by REST and GraphQL;
callers can set `concurrency=1` for serial requests, which GitHub recommends
to minimise secondary limits. A token run (the CLI) sizes it from the
`api.github.com` host limit when one is set, and otherwise from the default
host limit capped at 16 (see [host limits](/internals/cache/#host-limits)).
No cap guarantees against secondary throttling.

The governor tracks the `x-ratelimit-*` headers per resource, as
`x-ratelimit-resource` names it (core and graphql for the requests the Count
sends), reserves known remaining capacity for in-flight requests and waits
until an exhausted window resets, plus one second. A 304 refunds its
reservation. While a bucket is exhausted its reset only moves later, so a
response still in flight can't cut short a wait a rate-limit answer set. Once
the reset passes the bucket is unknown again and queued requests go ahead.

GitHub answers both GraphQL limits with a 200 and a `RATE_LIMITED` error. With
`x-ratelimit-remaining: 0` and no error message naming the secondary limit,
the answer is the primary limit: the graphql bucket waits until
`x-ratelimit-reset`, and writes are not throttled. Secondary 403 and 429
responses (`Retry-After`, documented secondary or abuse messages) and the
other GraphQL `RATE_LIMITED` answers set a cooldown shared by all tasks.
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
