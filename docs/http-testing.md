# HTTP fixtures

Production code accepts `rustovate_http::Transport`. It takes a reqwest Request
constructed with `Request::new`, so building a request needs no Client. The
separate `rustovate-testkit` crate belongs only in consuming crates'
dev-dependencies. Replay has cassette data, match options and a consumption
cursor only. It has no reqwest Client or fallback transport. Its unmatched
request error names the redacted method/URL and the fields to check. A test
passes a panicking network factory to the default replay constructor, and uses
an unresolvable `.invalid` host to demonstrate this boundary.

JSON cassettes live under `fixtures/`, one per scenario, with schema version,
provenance, request and response. Bodies are parsed JSON (human-diffable and
object-order-independent) or UTF-8 text. Binary/streaming bodies fail explicitly.
Requests match method, sanitized URL, selected headers and the complete body,
including GraphQL query and variables. Defaults select Accept, Content-Type,
If-None-Match, If-Modified-Since and X-GitHub-Api-Version; callers can replace
the list. Repeated identical requests consume successive recorded interactions.
`Replay::assert_finished` detects unexpectedly unused interactions.

Replay is the default. `from_env` only calls the provided network factory when
`RUSTOVATE_RECORD=1`. Recording starts a fresh scenario and atomically persists
after each exchange; failures may leave a partial recording, so review the diff
and replay before committing. Ordinary tests and CI never opt into recording.

```console
mise run test
mise run fixtures:check
# For real GitHub recordings, provide GITHUB_TOKEN in the invoking environment.
# Never put its value in command arguments, fixtures, or logs.
mise run fixtures:record
mise run test
```

Redaction happens before writing: authorization and cookie headers; sensitive
JSON keys; URL credentials and token/key/signature query parameters; GitHub
`ghp_`, `ghs_`, `gho_`, `github_pat_` and `v1.` token values anywhere. Callers
can add literal secret values or regex patterns through `Redactor`. The scanner
also accepts comma-separated literal values in `RUSTOVATE_REDACT`. Redaction
does not change the in-memory response returned to a recording test. Scanning
checks raw and parsed JSON (including escaped tokens), all nested fields, and
URLs. Failures name the cassette path without printing matched values.
`mise run ci` runs the scan as well as the test suite.

Every real cassette must say it was recorded by rustovate from the endpoint
URLs. Simulated rate-limit and transport responses are explicitly labelled.
Never import another bot's fixtures, including Renovate's.

API references read for this implementation:
[reqwest](https://docs.rs/reqwest/0.13.5/reqwest/),
[Request](https://docs.rs/reqwest/0.13.5/reqwest/struct.Request.html),
[Response](https://docs.rs/reqwest/0.13.5/reqwest/struct.Response.html),
[Body](https://docs.rs/reqwest/0.13.5/reqwest/struct.Body.html),
[ClientBuilder](https://docs.rs/reqwest/0.13.5/reqwest/struct.ClientBuilder.html),
[serde](https://docs.rs/serde/latest/serde/),
[serde_json](https://docs.rs/serde_json/latest/serde_json/),
[regex](https://docs.rs/regex/latest/regex/),
[tempfile](https://docs.rs/tempfile/latest/tempfile/),
[thiserror](https://docs.rs/thiserror/latest/thiserror/).
