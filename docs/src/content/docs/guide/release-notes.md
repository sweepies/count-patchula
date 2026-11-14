---
title: "Release notes"
description: "How update PRs gather release notes and changelogs for every version between the current and proposed one."
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
| Docker and OCI images | the image's `org.opencontainers.image.source` label, then a small table for official images, then the same key in the manifest's annotations |
| Helm charts, from an OCI registry or an `index.yaml` | the chart version's `sources`: the first on a supported host, else the first |
| mise | the release URL mise lists for the version; a table for core tools; the npm, PyPI or crates.io metadata when that is the backend mise picks for the tool |

GitHub, GitLab (gitlab.com and hosts with `gitlab` in the name), Codeberg,
gitea.com and hosts named `gitea` or `forgejo` are supported. Other hosts are
reported as "source found, unsupported host" and never probed.

A GitHub URL of the form `/tree/<branch>/<directory>` also names the package's
directory, as charts kept together in one repository usually do. The branch
must be a single path segment, such as `main`.

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

The mise-versions lists only cover public repositories, so a private GitHub
repository gives its notes through a changelog, which the Count reads when its
GitHub access covers that repository.

Plain and `v`-prefixed tags, monorepo namespaces (`@scope/pkg@1.2.3`,
chart-releaser's `my-chart-1.2.3`), and the
spellings used by Go, jq, Bun, nginx, Redis and PostgreSQL are understood.
Comparison links use the exact tags. If a tag had to be inferred from the
upstream's naming pattern, the link says so.

## Helm charts

A chart can list its own changes for each version in Artifact Hub's
[`artifacthub.io/changes`](https://artifacthub.io/docs/topics/annotations/helm/)
annotation in `Chart.yaml`. When any version in the range has it, those lists
are the notes, and the source repository is not read for text:

```yaml
annotations:
  artifacthub.io/changes: |
    - kind: added
      description: Support for custom ingress annotations
    - kind: fixed
      description: Probe timeouts were ignored
      links:
        - name: PR #42
          url: https://github.com/acme/charts/pull/42
```

Plain entries (`- Fixed probe timeouts`) are listed as they are; structured
ones are grouped by kind. A version in the range without the annotation says
the chart lists no changes for it. Charts in an OCI registry are read one
version at a time, at most 32 per range; an `index.yaml` holds every version
already.

Without the annotation, the notes come from the repository the chart's
`sources` names, as for any other package. That is often the packaged
application's repository rather than the chart's, so these PRs say where the
notes came from. A changelog packaged inside the chart is not read.

## Fetching once

Notes are only read when a PR is being written or rewritten. A steady run makes
no release-note requests at all, and a rebase reuses the notes already in the
PR body. Within a batch, release lists, bodies and changelog reads are shared
across every PR: mise, Docker and GitHub Actions updates of Node read Node's
releases once. Bodies are kept in SQLite with their `updatedAt`, so later runs
only ask for timestamps. Changelogs are parsed once per immutable blob.

## How notes are rendered

The body stays within 32,000 bytes. Each dependency gets at most 12,000 bytes
and 32 releases. On every forge the releases in range are taken newest version
first, whatever order the forge lists them in; once the budget is spent, older
releases are left out and the notes say they are incomplete.

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
list items or headings, and not inside a link, a code span or a code block; a
fence or `<details>` that upstream left open is closed. When a single paragraph
is bigger than the room left, the cut falls at the last line end that leaves no
code span or link label open, and when not even that fits the release is left
out. The package then gets one notice, linking to upstream's release history:
either "the Count left some release notes out", or, when upstream's own history
couldn't be read in full, "these release notes may be incomplete".

When fetching a package's notes fails, the Count settles the body only if the
failure would repeat every run: an unsafe package name, a registry or upstream
that does not know the package or release, or an upstream answer the notes
cannot use. If the source repository was never found, the section reads
"release notes unavailable: source repository could not be resolved"; if it was
found but its notes could not be read, the section says so and links the
source. Any other failure (the network, a timeout, a server error, a rate limit,
a refused credential, the Count's own cache) may pass, so the section says the
Count could not read the notes this run. The PR body is then not treated as
settled: the run keeps no write receipt for that branch, so the next run renders
the body again, fetches the notes again and rewrites it. A host that no longer
resolves counts as a network failure, so such a PR retries its notes every run.

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
