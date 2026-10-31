---
title: "JSON report"
---
`patchula lookup --json` and `patchula run --json` print one report for the
whole batch. This page describes schema version 3.

```json
{
  "schemaVersion": 4,
  "repositories": [{
    "repository": "owner/repo",
    "status": "complete",
    "reason": null,
    "packageFiles": [{
      "path": "Cargo.toml",
      "dependencies": [{
        "manager": "cargo",
        "name": "serde",
        "package": "serde",
        "current": {"value": "1", "version": "1.0.0", "digest": null},
        "updates": [{
          "newValue": "1", "newVersion": "1.1.0", "newDigest": null,
          "updateType": "minor", "group": null
        }]
      }]
    }]
  }],
  "stats": {"requests": 12, "notModified": 9, "cacheHits": 3, "extractionHits": 0, "lookups": 4, "phaseMicros": {}}
}
```

Readers should reject any other `schemaVersion`. Unknown fields are rejected
too, so a new field means a new version. Version 4 added the `reason` of a
notice that is not a template update; version 3 added an image rebuild's
`image` check.

## Repositories

| Field | |
|---|---|
| `repository` | `owner/name` |
| `status` | `complete`, `skipped` or `error` |
| `reason` | Why it was skipped or failed; null when complete |
| `packageFiles` | Files with dependencies, each with a `path` and `dependencies` |
| `warnings` | Optional. Files skipped, stale registry data and similar |
| `skipped` | Optional. Declarations that are not read as versions, such as images built from variables: `manager`, `path`, `line`, `value` and `reason` |
| `notices` | Optional. Newer versions that are reported and never proposed: a [Copier template](/guide/managers/#copier)'s tags, which only `copier update` applies, and a release whose declaration cannot be rewritten to allow it without guessing, such as a Composer `^1 \| ^2` needing 3.0. Each has `manager`, `path`, `line`, `name`, `package`, `current`, `available`, and `compareUrl` when the host has a compare page; the second kind also has a `reason` |
| `lookupFailures` | Optional. Dependencies whose lookup failed; see [below](#lookup-failures) |
| `tooling` | Optional. [Tooling warnings](/guide/auto-merge/#tooling-warnings) that hold on a live run, one per rule: `rule`, optional `detail` (`native` for `merge-without-ci`, `reviews-only` or `tagged-release`), the `branches` it applies to, and optional `silenced` with the reason from `warnings.quiet` |
| `defaultBranch`, `commit` | Optional. The branch and commit that were read |

A `complete` repository with no package files had nothing the Count supports.

## Lookup failures

Each entry in `lookupFailures` is a dependency whose registry lookup failed,
so its updates are unknown. A repository with lookup failures is still
`complete`, and the failure is not repeated in `warnings`.

```json
"lookupFailures": [{
  "manager": "npm",
  "path": "package.json",
  "name": "is-number",
  "package": "is-number",
  "datasource": "npm",
  "kind": "network",
  "message": "registry HTTP 503"
}]
```

| Field | |
|---|---|
| `manager`, `path`, `name`, `package` | The dependency, as in `packageFiles` |
| `datasource` | Where it was looked up, such as `npm`, `docker` or `github-tags` |
| `kind` | `network` (unreachable, or a 5xx answer), `auth` (401, 403 or 407), `not-found`, `rate-limit`, `parse` (an answer the Count could not read) or `other` |
| `message` | The error in words |

A failed digest lookup, such as an image tag whose digest could not be read,
is a lookup failure too. Declarations in `skipped` and dependencies a rule
ignores are never looked up, so they never fail. `lookup` and `apply` exit 4
when this list is not empty.

## Dependencies

| Field | |
|---|---|
| `manager` | The manager name used in configuration, such as `npm` or `cargo` |
| `name` | The name in the manifest |
| `package` | The registry identity; differs from `name` for aliases |
| `occurrence` | Optional. Tells apart the same name declared more than once in a file |
| `current.value` | The text in the manifest, such as `^1.2` |
| `current.version` | The resolved version, or null when unknown |
| `current.digest` | The pinned digest, or null |
| `updates` | Proposed updates, in no particular order |
| `metadata` | Optional. See below |

## Updates

| Field | |
|---|---|
| `newValue`, `newVersion`, `newDigest` | The target; at least one is set. A `null` `newValue` with a `newVersion` is a lockfile-only update: only a lockfile changes. For a direct dependency that is a release its declaration already allows; a transitive package, which has no declaration, moves the same way |
| `updateType` | `major`, `minor`, `patch`, `digest` or `pin` |
| `group` | The group name from configuration, or null |
| `policy` | Optional. The policy that applies: versions, wait, windows, merge, labels |
| `releaseAge` | Optional. `publishedAt` and the `evaluatedAt` it was judged against |
| `fixes` | Optional. IDs of the current version's advisories that the target no longer has |
| `image` | Optional. For a pinned image's new digest, the SBOM comparison: `state`, the compared `platforms`, the `sboms` read (`digest`, `source`, `generator`), `os`, `packages`, the `fixed` advisories with their `package`, `old` and `new` versions and `platforms`, `introduced` advisory IDs (most severe first), `introducedDetails` (OSV's records for up to five of them), `detail` and `silenced` |

## Dependency metadata

| Field | |
|---|---|
| `source`, `kinds` | Where the dependency came from and its kinds, such as `devDependencies`, or `transitive` for a vulnerable package only a lockfile records |
| `locations` | Byte offsets, line, column and a pointer into the file |
| `resolution` | How `current.version` was found: `declared`, `lockfile`, `pinned-commit`, `floating-ref`, `release-line` or `supported-range` |
| `heldBack` | Updates waiting on release age, per update type, with when they become eligible |
| `lookup` | When the lookup ran, disabled update types, and versions rejected as deprecated or yanked |
| `tagDigests` | For GitHub Actions, the commit each proposed tag points at |
| `image` | Optional. For a pinned image, the current digest's warning state, in the same shape as an update's `image` |
| `advisories` | Optional. OSV advisories affecting the current version: `id`, `aliases` (CVE IDs among them), `severity`, `summary`, `url` and `fixedIn`, the lowest fixed release above the current one |
| `stale` | Registry data used past its freshness because the registry was rate-limited, with the time it was fetched |

## Actions

`run --json` adds an `actions` object. `lookup` leaves it out.

```json
"actions": {
  "schemaVersion": 1,
  "dryRun": true,
  "branches": [{
    "repository": "owner/repo",
    "branch": "patchula/example-major-0123456789abcdef",
    "baseCommit": "…",
    "status": "planned",
    "reason": "would create branch",
    "message": "chore(deps): update example to 2.0.0",
    "previousCommit": null,
    "commit": null,
    "verified": null,
    "lockfilePending": false,
    "files": [{"path": "mise.toml", "blobSha": "…", "diff": "--- a/mise.toml\n+++ b/mise.toml\n…"}]
  }],
  "pullRequests": [{
    "repository": "owner/repo",
    "branch": "patchula/example-major-0123456789abcdef",
    "number": null,
    "url": null,
    "status": "planned",
    "reason": null
  }]
}
```

Branch and pull request statuses are `planned`, `created`, `updated`,
`rebased`, `unchanged`, `closed`, `merged`, `deleted`, `skipped`, `protected`
and `error`. A dry run never reports a write (`created`, `updated`, `rebased`,
`closed`, `merged` or `deleted`) and has no `commit`. A published branch has
its `commit` and `verified: true`.

`skipped`, `protected` and `error` always carry a `reason`. A `protected` or
`error` action makes its repository an `error`, and the CLI exits with 1.

`blobSha` is the Git blob SHA of the new file, and `diff` is a unified diff
against the base commit.
