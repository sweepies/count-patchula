# Phase 2, wave 1 evidence

## SWE-104: intermittent local test failure

The original failure did not reproduce. At unmodified `09d8172`, an isolated
checkout passed 36 full offline workspace test runs (12 each with 1, 2 and 16
test threads), 50 focused engine replay runs, and a full `mise run ci`, including
lint and build. The original aggregate of 114 passed / 1 failed points to
`patchula-engine::public_multi_repo_report_cold_and_warm`: the baseline reaches
114 passes immediately before that one-test integration binary. This identifies
a suspect, not a confirmed root cause. No speculative behavior fix is claimed.

Following the owner's fallback instruction, CI now retains each stage's complete
stdout/stderr in a unique directory under `/tmp` (override with
`PATCHULA_CI_LOG_DIR`). A failure prints the stage, failing test names and log
path on a `test result:` line, surviving the filter that lost the original
evidence. `mise run test` uses `--no-fail-fast`. The suspected replay logs its
evaluation instant and cold/warm counters before comparing results.

`mise run test:stress` repeats the suite with varied thread counts and keeps full
logs outside the checkout. `PATCHULA_STRESS_RUNS`, `PATCHULA_STRESS_LOG_DIR` and
`PATCHULA_STRESS_FILTER` bound and focus reproduction. Recording is disabled.
Tests use isolated caches and no shared environment mutations, ports or fixed
temporary directories. A recurrence should be investigated using its retained
log rather than inferred from an aggregate count.

## SWE-105: shared config evaluation

Local checks/overrides and API-supplied modules now use the same evaluator and
bundled-schema fallback. Regression tests compare nested `prs` and `defaults`
amendments with and without vendoring, plus vendored-schema precedence and CLI
directory discovery. `config check` on a local checkout of the sandbox passes without a
local `Patchula.pkl`. No files in the sandbox were changed.

## SWE-106: branches only

The updater plans surgical manifest edits with extracted locations and blob
identities, then uses Git Data blobs/trees/commits and leased GraphQL Git Data refs.
It preserves formatting and file modes, refuses stale content and protects human
or uninspectable branch histories. Updates/rebases replace one commit with the
default branch as its sole parent. Only GitHub-verified commits are published.
`run --dry-run` performs read-only validation and prints branch/file plans with
unified diffs. `run --json` emits batch schema v2 and actions schema v1.

Offline tests cover creation, replacement, unchanged reruns, both rebase policies,
conflicting moved-base files, human commits at/below the branch tip, foreign
committers, stale blobs, moved bases, truncated trees, unsigned commits and failed
ref leases. Manager tests cover JSON aliases/key order/indentation, action pin
comments/subpaths/CRLF, TOML forms/inheritance, Docker variants, Compose YAML and
Go require blocks/directives. A token-only synthetic engine test exercises in-repo
Pkl discovery on both cold and cached extraction, dry/real runs, and lockfile gates.
No private responses or credentials were recorded.

## Live sandbox verification

From the repository root, with an installation token already exported as
`PATCHULA_TOKEN` (no other credentials/config flags are needed):

```sh
mise run run --dry-run OWNER/SANDBOX
mise run run OWNER/SANDBOX
# A second real run should report unchanged eligible branches:
mise run run --json OWNER/SANDBOX > /tmp/patchula-sandbox-applied.json
```

For JSON review before publishing, add `--json` to the first command and redirect
stdout to `/tmp/patchula-sandbox-plan.json`. Diagnostics/summaries stay on stderr.
The token needs Contents write, and the installation must allow workflow updates
for `.github/workflows/*`. No App ID, private key, checkout or local schema is used.

Expected default outcome: only `patchula/` branches for eligible Actions
(`actions-checkout-*`, `jdx-mise-action-*` and hosted-runner `ubuntu-*`), mise
(`node-*`, `oven-sh-bun-*`, `jqlang-jq-*`) and container (`node-*`, `nginx-*`,
`redis-*`, `postgres-*`) tracks. Each name ends in its class and stable 16-digit
identity hash. Docker Node and mise Node have different source identities/hashes.
Available patch/minor/major/pin/digest tracks depend on the catalogs returned at
run time; the dry-run report is the authoritative exact branch list. No PRs or
dashboard issue are created and the default branch remains unchanged.

npm/bun package, Cargo and Go plans are skipped with explicit `lockfile pending`
reasons because this sandbox has `bun.lock`, `Cargo.lock` and `go.sum`. To test
those manifest-only branches deliberately, use the same flag for both steps:

```sh
mise run run --dry-run --allow-lockfile-pending OWNER/SANDBOX
mise run run --allow-lockfile-pending OWNER/SANDBOX
```

Those branches retain `lockfilePending: true` and a commit warning. Keep this
flag out of ordinary runs until SWE-109 can regenerate and validate lockfiles.

Open live check: verify the App's Git Data commits return `verification.verified`
as true. The offline server tests both outcomes; an unsigned commit leaves the
branch untouched and returns an action error. GitHub explicitly documents
automatic signing for
[`createCommitOnBranch`](https://docs.github.com/en/graphql/reference/commits#createcommitonbranch),
whereas the requested REST Git Data route exposes a verification result. If the
App's Git Data route is unsigned, use the signed GraphQL authoring route in a
follow-up rather than weakening verification or inventing an App identity.
That change needs to preserve the one-commit/base and atomic human-work protection
guarantees; it cannot simply append commits to an existing update branch.

Final offline validation: `mise run ci` passes (216 tests across 55 suites,
zero failures; strict Clippy, formatting and workspace build). Its fixture stage
checked all 22 cassettes for possible secrets. `mise run docs` regenerated the
CLI reference, usage definition, man page and completions. No live sandbox write
was attempted and nothing was pushed.
