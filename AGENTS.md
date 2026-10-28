# AGENTS.md

Count Patchula is a dependency update bot for GitHub, written in Rust. The
binary is `patchula`. Docs live in `docs/` and are the published site, built
with Astro Starlight; the pages are under `docs/src/content/docs/`.

## Clean room

Renovate is AGPL; Count Patchula is MIT. Read only Renovate's public
documentation, and treat Renovate itself as a black box you may run to compare
results. Its source code, preset data and test fixtures stay unread: before you
fetch anything from `renovatebot/*` on GitHub, confirm it is documentation.

When the Count behaves differently from Renovate on purpose, record the
departure and its reason in `docs/src/content/docs/renovate/departures.md`.
An unrecorded difference is a bug.

## Working

- Every tool comes from mise, and every repeatable action is a mise task
  (`mise tasks` lists them). Add a task for a new workflow rather than a script.
  A task that runs a command or two in order is an entry in `tasks.toml`,
  which `mise.toml` includes.
  Anything longer is a nu file task under `mise-tasks/` (`#!/usr/bin/env nu`,
  with `#MISE` and `#USAGE` headers) that works on nu's structured data instead
  of jq and string handling; never bash, and never shell inside a workflow.
  Only the `cc` and `ar` wrappers stay sh, because they run on every C compile.
  JavaScript packages go through aube.
- Changes are hard cutovers: rename, remove and migrate in one change, with no
  compatibility shims, aliases or deprecated paths left behind.
- Tests are offline and replay HTTP cassettes from `fixtures/`. Record new ones
  with `PATCHULA_RECORD=1` (see `mise run fixtures:record`); the fixtures scan
  rejects anything that looks like a secret.
- An intended change to extraction updates the corpus snapshot with
  `mise run corpus:snapshot`. Review the diff in `tests/corpus-extraction.tsv`
  as part of the change.
- A change to CLI flags regenerates `docs/src/content/docs/reference/cli.md`
  and `generated/` with `mise run docs:cli`.
- User-facing behaviour changes update the matching page under
  `docs/src/content/docs/guide/` in the same commit. The compatibility matrix
  is `docs/src/content/docs/renovate/compatibility.md`.

Architecture, caching and the HTTP test design are in
`docs/src/content/docs/internals/`. Read the relevant page before changing the
engine, cache or GitHub client.

## Voice

The Count is "he". Strings a user sees from the bot (PR bodies, dashboard,
CLI summaries) can be whimsical, in the Count's voice. Docs and code comments
are plain and direct.

## Done

A change is done when every CI check on GitHub is green after pushing. GitHub
runs the tasks in `mise run ci` as parallel jobs, so run only the checks for
what changed locally, such as one crate's tests. Commits use conventional commit
messages (`feat(manager): …`, `fix(lookup): …`).
