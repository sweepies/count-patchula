---
title: "HTTP fixtures"
---
Production code accepts a `patchula_http::Transport`, which takes a reqwest
`Request` built with `Request::new`, so building a request needs no client.
`patchula-testkit` belongs only in dev-dependencies. Replay holds only the
cassette data, match options and a consumption cursor, with no reqwest client
and no fallback transport. An unmatched request fails with the redacted method
and URL and the fields to check. A test passes a panicking network factory and
an unresolvable `.invalid` host to prove replay never reaches the network.

## Cassettes

JSON cassettes live under `fixtures/`, one per scenario, with a schema version,
provenance, requests and responses. Bodies are parsed JSON, so they diff well
and ignore key order, or UTF-8 text; binary and streaming bodies fail.
Requests match on the method, sanitised URL, selected headers and the complete
body, including GraphQL query and variables. The default headers are `Accept`,
`Content-Type`, `If-None-Match`, `If-Modified-Since` and
`X-GitHub-Api-Version`, and callers can replace the list. Repeated identical
requests consume successive interactions, and `Replay::assert_finished`
catches unused ones.

## Recording

Replay is the default. `from_env` calls the network factory only when
`PATCHULA_RECORD=1`, which ordinary tests and CI never set. Recording starts a
fresh scenario and saves atomically after each exchange, so a failure can leave
a partial recording: review the diff and replay before committing.

```console
mise run test
mise run fixtures:check
# For real GitHub recordings, provide GITHUB_TOKEN in the invoking environment.
# Never put its value in command arguments, fixtures, or logs.
mise run fixtures:record
mise run test
```

Every real cassette must say it was recorded by patchula, with the endpoint
URLs. Simulated rate-limit and transport responses are labelled as such.
Never import another bot's fixtures, including Renovate's.

## Redaction

Redaction happens before anything is written:

- authorization and cookie headers, and sensitive JSON keys;
- URL credentials and token, key and signature query parameters;
- GitHub tokens anywhere: `ghp_`, `ghs_`, `gho_`, `ghu_`, `ghr_` and
  `github_pat_` values, including dotted [stateless `ghs_`
  tokens](https://github.blog/changelog/2026-04-24-notice-about-upcoming-new-format-for-github-app-installation-tokens/);
- legacy `v1.` tokens, which need at least 20 characters after the prefix so
  version tags such as `v1.24.0` survive;
- the recording token itself, as a literal, and any literals or patterns a
  caller adds through `Redactor`.

The in-memory response a recording test sees is not redacted. The scanner
(`mise run fixtures:check`, also part of `mise run ci`) checks raw and parsed
JSON, escaped tokens, nested fields and URLs, and accepts extra literals as
comma-separated values in `PATCHULA_REDACT`. Failures name the cassette without
printing the match.

## mise

A subprocess has no HTTP cassette. mise lookups run through
`patchula_process::Executor`, and tests swap in `patchula_testkit::MiseReplay`,
which answers from `fixtures/mise/listings.json`: for each command after
`mise` (such as `ls-remote --json --prerelease -- jq`), the JSON mise printed
or the error it failed with. The replay keeps every command with its
environment and working directory, so tests can assert the sandbox. A lookup
missing from the fixture fails the test; replay never runs mise.

`mise run fixtures:record-mise` runs the pinned mise through `MiseRecorder` and
rewrites the file. It needs the network, a `mise` on `PATH` and `GH_TOKEN`,
which mise gets as it would in production and which is never written down.
`mise run fixtures:check` scans these files with the cassettes.

## Replaying a webhook to a local server

```sh
mise run server:replay /tmp/push.json push local-push-001
```

The `server:replay` task signs a recorded payload with your webhook secret and
posts it to a loopback listener. It prints only the HTTP status and refuses
non-loopback targets. Set only one of `PATCHULA_WEBHOOK_SECRET_FILE` and
`PATCHULA_WEBHOOK_SECRET` when you use it. A repeated delivery ID is accepted
but ignored, so use a new ID to trigger again.
