---
title: "Release notes"
---
Every update PR carries the release notes for each version between what you
have and what it proposes, newest first. `defaults { releaseNotes = false }`
turns them off; pin-only rows never fetch any.

## Finding the upstream

The source repository comes from each ecosystem's own metadata, never from
running package code:

| Source | Where the repository comes from |
|---|---|
| npm | the target version's `repository`, then `homepage` |
| crates.io | the crate's `repository` |
| PyPI | project URLs named Source, Source Code, Repository, Code, GitHub or Changelog |
| Go | the module path; vanity paths through `?go-get=1`; `golang.org/x` through its GitHub mirrors |
| Docker and OCI | the image's `org.opencontainers.image.source` label, then a small table for official images |
| mise | the release URL mise lists for the version; a table for core tools; the npm, PyPI or crates.io metadata when that is the backend mise picks for the tool |

GitHub, GitLab (gitlab.com and hosts with `gitlab` in the name), Codeberg,
gitea.com and hosts named `gitea` or `forgejo` are supported. Other hosts are
reported as "source found, unsupported host" and never probed.

## Where the text comes from

1. GitHub releases in the range, selected through the public
   [mise-versions](https://github.com/jdx/mise-versions) lists and fetched in
   one aliased GraphQL query per upstream. Drafts are skipped; prereleases only
   appear for a prerelease target.
2. When releases are empty, a changelog: `CHANGELOG`, `CHANGES`, `HISTORY`,
   `RELEASES` or `NEWS` in the package's directory or the root, with the usual
   extensions, including directory layouts like Node's
   `doc/changelogs/CHANGELOG_V22.md`. Package-local changelogs win.
3. For upstreams that publish notes elsewhere, a dedicated adapter: PostgreSQL's
   SGML release notes on the target's branch, and Go's release history data.

Plain and `v`-prefixed tags, monorepo namespaces (`@scope/pkg@1.2.3`), and the
spellings used by Go, jq, Bun, nginx, Redis and PostgreSQL are understood.
Comparison links use the exact tags. If a tag had to be inferred from the
upstream's naming pattern, the link says so.

## Fetching once

Notes are only read when a PR is being written or rewritten. A steady run makes
no release-note requests at all, and a rebase reuses the notes already in the
PR body. Within a batch, release lists, bodies and changelog reads are shared
across every PR: mise, Docker and GitHub Actions updates of Node read Node's
releases once. Bodies are kept in SQLite with their `updatedAt`, so later runs
only ask for timestamps. Changelogs are parsed once per immutable blob.

## How notes are rendered

The body stays within 32,000 bytes. Each dependency gets at most 12,000 bytes
and 32 releases.

Under the dependency's name and its Source and Compare links, the notes sit in
one collapsed block whose summary says what is inside, such as
`Release notes: 4.151.3 → 4.156.0 (5 releases)`, or `3 of 5 releases` when some
were left out. Each release keeps its linked version heading. Upstream
headings (`### Minor Changes`) move down to sit below the version heading, so
they never look like the next dependency. A single release with at most 8
lines and 600 bytes stays expanded, since a fold would be longer than the
notes.

The table, any `bodyNotes` and the checkboxes stay outside the fold.

A package that runs out of room is cut where a block ends, between paragraphs,
list items or headings, and never inside a link, a code span or a code block;
a fence or `<details>` that upstream left open is closed. The package then gets
one notice, linking to upstream's release history: either "the Count left some
release notes out", or, when upstream's own history couldn't be read in full,
"these release notes may be incomplete".

Two edits keep upstream text from making noise:

- `@mentions` in upstream prose get a zero-width joiner after the `@`, so
  copying release notes into a PR does not notify half of GitHub. Code, URLs,
  email addresses, `name@version`, scoped packages and `@sha256:` are left
  intact.
- Links to issues, pull requests, discussions and commits go through
  `redirect.github.com`, which GitHub documents for [avoiding
  backlinks](https://docs.github.com/en/get-started/writing-on-github/working-with-advanced-formatting/autolinked-references-and-urls#avoiding-backlinks-to-linked-references),
  so upstream timelines are not filled with "mentioned this" events. Bare
  `#123` references become links to the upstream's issue.

HTML comment delimiters in upstream text are escaped, so release notes cannot
forge the Count's hidden markers.
