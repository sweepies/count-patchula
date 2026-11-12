---
title: "HTTP fixtures"
description: "How Count Patchula's tests replay recorded HTTP cassettes offline, and how to record and check new fixtures."
---
Production code accepts a `patchula_http::Transport`, which takes a reqwest
`Request` built with `Request::new`, so building a request needs no client.
`patchula-testkit` belongs only in dev-dependencies. Replay holds only the
cassette data, match options and a consumption cursor, with no reqwest client
and no fallback transport. An unmatched request fails as an ordinary transport
error, and `assert_finished` catches interactions no request consumed. A test
passes a panicking network factory and an unresolvable `.invalid` host to
prove replay never reaches the network.

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

## Stateful GitHub tests

Tests that simulate GitHub repository state — branches, commits, pulls,
discovery listings — share one in-memory fake,
`patchula_testkit::github::FakeGitHub`, instead of per-test transport
scripts. The fake answers the REST and GraphQL routes the client calls,
applies writes to its state, and scripts faults through `on(Matcher)`.
Its pulls listing honours `state`, `sort=created`, `direction`,
`per_page` and `page` with GitHub's documented defaults and panics on
parameters it does not model, and `repository.pullRequests` resolves
`headRefName`/`states`/`orderBy` filtering with `first`/`after` paging.
`Hosts` fronts it for a test's other endpoints — OSV, npm, registries —
so one transport covers a whole run.

Every request the fake sees feeds a per-test transcript: the request
multiset is normalised, hashed and compared against `tests/transcripts.tsv`
beside the suite. `mise run transcripts:snapshot` rewrites the snapshots;
a request change that leaves the snapshot stale fails the test, so a port
that alters what a suite asks is a deliberate, reviewed diff.

`settings.latency` sleeps a fixed time on Tokio's paused clock before the
fake answers, and `Hosts::set_latency` charges the same latency on the
scripted routes, so one knob times a whole run. Virtual elapsed time
divided by that latency is the run's depth — the round trips on its
critical path. Depth pins live beside the transcripts in `tests/depth.tsv`
(a warm lookup, a no-op real run, and full writes of one and three plans),
and each pin repeats its scenario, serialized against the other pins so a
sibling's real work cannot steal a wave boundary: the file holds the
depth a repetition reaches most often. A deterministic extra request
moves every repetition off the pin and fails it the way a changed
request fails a transcript; so does an improvement. `mise run
transcripts:snapshot` rewrites both files.

A repetition's elapsed time should divide exactly by the latency — the
fake sleeps whole waves and the governor paces whole seconds — so a
change costing less than a wave cannot hide inside the division. It does
not always: a deadline now and then reaches the paused clock off the
wave grid (a timer armed from real time slips past `latency` and
resolves at an instant no boundary produced), which leaves that
repetition's depth unreadable as whole waves. Such a sample is left out
of the distribution, and more than a quarter of them fails too — a
systematic sub-latency change rags most samples, which is what a strict
assert would have caught. What remains is the pin's blind spot, bounded
rather than removed: a send racing a wave's boundary can land on an
earlier request's completion or co-fire with a sibling read and wander
a wave, and under suite load a paced write can slip a second wave when
a read escapes its pacing window — a rep further out fails outright —
and a request that is only sometimes sequential can hide behind that
slack, so fewer than a quarter of the repetitions left on the pin fails
as well. And a write
pin's depth is mostly the governor's one-second write pacing, not round
trips — a read that fits inside a pacing window adds no wave. What depth
never changes is the transcript's job: the request multiset still pins
exactly which requests each test makes.

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
