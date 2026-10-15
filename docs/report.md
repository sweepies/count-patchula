# Batch report contract (version 2)

`rustovate-report` is the shared caller interface for `lookup --json`
and `run --json` results, and for CLI/server callers of the engine. The
existing config-check JSON is a different command result, not a batch report.

Deserialize untrusted input with `Report::from_json`, which checks `schemaVersion`
before interpreting the payload and validates identities. Unknown fields fail.
A future incompatible schema increments the version; consumers must reject it
rather than treating a new payload as an empty successful lookup.

```json
{
  "schemaVersion": 2,
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
  }]
}
```

All collection fields are required, including empty collections. Optional string
fields accept null or omission; producers should emit null consistently. Current
value means manifest text, version means resolved/extracted version, and digest
means the hash itself. Unknown versions stay null; consumers must not infer a
version from a range. `package` is the registry identity (for aliases it can differ
from `name`). Manager names use rustovate's configuration vocabulary. Update types
are `major`, `minor`, `patch`, `digest`, `pin`; comparison output also preserves
other upstream update types so unsupported maintenance is visible. `group` is
the human group identity, not a tool-specific branch slug. Update lists are sets,
not ordered preference lists; each proposal must have at least one target field.

Repository status is `complete`, `skipped`, or `error`. Skipped/error repositories
require a reason. An empty complete repository means a successful extraction with
no supported dependencies. A caller must never convert errors into that state.

Repository and file paths are unique. Within a file, `(manager, name, occurrence)`
identifies a dependency. `occurrence` is optional for singleton identities.
Producers merge identical package/current occurrences and their proposals and
locations. Different package/current identities receive explicit ordinal IDs
after sorting by package/current, independently of source order. Duplicate
unlabelled identities still fail validation. This additive v1 extension is also
implemented by the comparison adapter, so repeated action refs and multiple mise
tool versions survive comparison. Older strict readers must upgrade to consume
reports with these optional extensions; the original comparison fields remain.

Additional optional fields: repository `warnings`, `defaultBranch`, `commit`;
dependency `metadata` (`source`, `kinds`, byte/line/column/semantic `locations`);
update `policy` (`versions`, `waitHours`, `windows`, merge/labels/message settings);
dated updates' `releaseAge` (`publishedAt`, `evaluatedAt`); dependency metadata
`heldBack` entries summarize each update type: `version` is the newest blocked
release, `versions` contains every blocked release, and `count` counts them.
`publishedAt` and `eligibleAt` describe the earliest release that becomes eligible.
Optional `targetEligibleAt` gives the eligibility time of the displayed newest
`version`, which can differ from the class's earliest eligible release. The
dependency dashboard uses this exact target time.
Registry rate-limit fallback adds `metadata.stale`, mapping `catalog` and
`digest:<tag>` to the original cache fetch time (Unix seconds). These entries
mean the data is stale because the registry could not be revalidated; they do
not renew freshness. Repository warnings also identify each affected dependency.
`metadata.lookup` adds the fixed batch `evaluatedAt`, effective `disabledUpdates`
and `rejected` versions with deprecated/yanked reasons. Rejection evidence covers
versions newer than the known current version. `metadata.resolution` identifies
the origin and value of the reported current version: declared, lockfile,
pinned-commit, floating-ref, release-line or supported-range. Consumers should
use this provenance when comparing installed versions with resolved candidates.
For GitHub-tag actions, `metadata.tagDigests` maps proposed refs to independently
observed peeled commit hashes from the tag catalog. This lets consumers verify
that a pin matches a floating major/minor ref; the proposed digest alone cannot
establish that relationship. Missing entries provide no such evidence.
These entries also include `updateType`; batch
`stats` (requests, notModified, cacheHits, extractionHits, unique lookups and
phaseMicros). Unknown fields are still rejected. Timing windows are reported
without delaying lookup. Engine JSON errors remain error envelopes only for
batch-level failures; per-repository errors stay in the report.

The hand-written [sample](../conformance/tests/fixtures/report.json) exercises
aliases, ranges, grouping and digest policy. It is not a real lookup result.

## Branch actions (SWE-106)

Version 2 adds the optional top-level `actions` object for `run`. `lookup` omits
it. The batch version increments because previous readers reject unknown fields;
regenerate v1 lookup snapshots rather than relabeling them. Config-check/error
envelopes, HTTP cassettes and conformance triage output retain their separate v1
versions. The original extraction/proposal fields keep their meaning.

```json
"actions": {
  "schemaVersion": 1,
  "dryRun": true,
  "branches": [{
    "repository": "owner/repo",
    "branch": "rustovate/example-major-0123456789abcdef",
    "baseCommit": "default-branch-commit",
    "status": "planned",
    "reason": "would create branch",
    "message": "chore(deps): update example to 2.0.0",
    "previousCommit": null,
    "commit": null,
    "verified": null,
    "lockfilePending": false,
    "files": [{
      "path": "mise.toml",
      "blobSha": "extracted-git-blob-sha",
      "diff": "--- a/mise.toml\n+++ b/mise.toml\n@@ -1,1 +1,1 @@\n-example = '1.0.0'\n+example = '2.0.0'\n"
    }]
  }]
}
```

The inner schema is independently versioned and validated. `branches` is sorted
by repository/name; each identity and file path is unique. `files` describes
the changes against the extraction base even when an existing branch is already
up to date. Each diff is unified text; original bytes, comments and file modes
are retained outside the extracted scalar replacements. `blobSha` is the Git
blob SHA (including Git's object header), not a plain content hash.

Statuses are `planned`, `created`, `updated`, `rebased`, `unchanged`, `skipped`,
`protected`, `error`. Dry runs never contain a new `commit` or an applied status;
they do read GitHub to validate blobs and inspect existing branch ownership.
`previousCommit` records the observed branch head. Applied actions supply the
published commit and `verified: true`. An unsigned API commit is never published.
A failed atomic ref lease can leave unreachable Git objects, but does not
replace the branch. Applied actions in a partially failed batch remain reported.
Protected/error actions make their repository an error and the CLI exits 1.

Skipped/protected/error actions require `reason`; skipped plans may still have
files/diffs. Lockfile-bearing branches are skipped by default with
`lockfilePending: true`. `--allow-lockfile-pending` explicitly permits manifest-only
test branches, retaining that flag and a clear warning in the report and commit
message. No lockfiles, PRs, dashboards or lockfile tools are written in this wave.

Names use the exact `prs.branch` prefix plus a group/dependency slug, update class
and 16 hex digits of SHA-256 over the full typed identity. A dependency identity
includes its source, package and class; a group includes its exact name and class.
Targets, base commits and member ordering do not change the name. Distinct slugs,
sources and groups cannot silently overwrite each other: duplicate generated
names are rejected before repository writes, and existing commits must carry the
full identity marker. Major/minor/patch/pin/digest alternatives use separate tracks;
conflicting edits within one track skip that branch with an explanation.


`actions.schemaVersion: 1` now optionally includes `pullRequests`, each with
`repository`, `branch`, nullable `number`/`url`, `status` and nullable `reason`.
Dry-run PR actions are `planned` with no number or URL; authoritative closed
history and capacity checks happen on real runs only. PR statuses include
`created`, `updated`, `unchanged`, `closed`, `merged`, `skipped`, `protected` and `error`. Branch
cleanup uses `deleted`. Older strict readers must upgrade to accept these
additive fields/statuses. Failed PR actions make the repository status `error`,
even when its signed branch was successfully published. A later run reconciles
uncertain outcomes from GitHub rather than retrying writes blindly.

Fallback auto-merge reports `merged` on both the PR and branch action, with its
merge method and any deferred branch cleanup in `reason`. Check/policy holds
are `skipped` PR actions; ownership races are `protected`. A successful merge is
never relabelled as failed merely because its branch cleanup was refused.
The dashboard retains up to twenty fallback merge outcomes for the cached
actor, including cleanup diagnostics. Cache loss can lose this local history,
but never authorizes a merge without fresh write receipt evidence.
