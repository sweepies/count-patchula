---
title: "CI on boat"
---
The Rust jobs of the CI workflow, `ci` and `corpus`, run in
[boat](https://docs.boat.dev/ci) sandboxes. Each job starts one sandbox from a
saved snapshot that already holds the toolchains and a warm build cache, runs
its checks there and deletes the sandbox. The **CI result** check is unchanged.

## How a job runs

The `boat` job (shown as **Boat snapshot**) reads the boat API key and looks up
the newest ready snapshot with `mise run boat:current`. When it finds both, the
Rust jobs run `mise run boat:run`, which:

1. creates a `large` sandbox from the snapshot with `--no-env`, so it gets none
   of the boat account's secrets, and `--no-snapshots`, so a sandbox whose
   delete never ran is erased when it auto-stops after an hour;
2. copies the whole checkout in, `.git` included, so Git-aware code sees the
   same tree as on a runner;
3. copies the variables named with `--pass` in through a file, never a command
   line. The corpus job passes its read-only `github.token`; nothing else
   leaves the runner;
4. runs the command with no time limit and keeps its exit code;
5. copies the `--fetch` paths back, also after a failure, and deletes the
   sandbox.

The corpus job keeps its registry cache with `actions/cache` on the runner and
carries it into the sandbox and back in `.corpus-cache` at the repository root.

When the key or a snapshot is missing, the Rust jobs run on GitHub runners as
before, and the **Boat snapshot** job's summary says why. This is how fork pull
requests run, since GitHub gives them no OIDC token, and how CI runs before the
first snapshot exists.

## Caching

A saved snapshot keeps `~/.local` and `~/.cargo` but not `~/.cache`, so the
sandbox commands set `XDG_CACHE_HOME=~/.boat-cache`. The snapshot then holds
the mise installs, including the task-scoped lockfile toolchains, the Cargo
registry and the mbx object store and managed target from a full `mise run ci`
and a release build of `patchula-cli`. That replaces both the mise-action cache
and the [mbx transport](/internals/ci-cache/) for jobs on boat: nothing is
restored or published from GitHub's cache.

The snapshot is a fixed starting point, and a stale one stays correct. A
dependency or toolchain added after it was built is fetched and compiled in each
run, so refresh it after such changes. The runner path keeps the mbx action,
but its cache is published only by runs on main that took the runner path.

## The snapshot

Saved snapshots of this repository are named
`count-patchula-ci-<UTC timestamp>-<12-character commit>`. The boat tasks only
touch names that match `boat_snapshot_pattern` in `mise.toml`; every other
saved snapshot and sandbox in the account is left alone.

`mise run boat:snapshot` builds one from HEAD:

1. creates a builder sandbox with `--no-env`, installs a pinned, checksummed
   mise and copies the checkout in;
2. runs `mise run ci` and the release build, then removes the checkout, so no
   file from this commit reaches a later run;
3. saves the sandbox under a new name and waits until boat reports it `ready`;
4. deploys it once in a probe sandbox;
5. only then deletes every other saved snapshot of this repository.

A failure before step 5 saves or deletes nothing that already works, and the new
snapshot is never deleted. A run that fails in step 5 leaves the new snapshot in
place and is safe to repeat. CI always picks the newest ready snapshot, so an
extra one only costs storage until the next run. `--dry-run` lists the
current snapshots and what a real run would create and delete.

## Refresh the snapshot

Run the **Boat snapshot** workflow from the Actions tab on `main`, optionally
with **dry-run** first. It refuses other refs. Its concurrency group lets one
run proceed at a time and never cancels one halfway through a save. A full
build takes about as long as a cold CI run.

Refresh it after a change to the tools in `mise.toml` or to `Cargo.lock` that
makes CI on boat noticeably slower.

## The key

The boat key is stored in Infisical as `boat-api-key`, in the project named by
the `INFISICAL_PROJECT_ID` repository variable, environment `prod`. The
workflows read it with `Infisical/secrets-action` through GitHub OIDC, as the
identity in `INFISICAL_BOAT_IDENTITY_ID`, or `INFISICAL_IDENTITY_ID` when the
first is unset. The identity must accept these subjects:

| Workflow | Subject |
| --- | --- |
| Boat snapshot (`workflow_dispatch` on main) and CI on pushes to main | `repo:sweepies@7191851/count-patchula@1419868512:ref:refs/heads/main` |
| CI on pull requests | `repo:sweepies@7191851/count-patchula@1419868512:pull_request` |

The key needs to create, use (exec and SSH, which `boat scp` uses) and delete
its own sandboxes, and `snapshot.read` and `snapshot.write` to list, save and
remove saved snapshots. boat's docs do not list the exact actions of the `ci`
preset. If a call fails with `api_key_action_forbidden`, create the key with
explicit actions instead:

```sh
boat api-key create count-patchula-ci --ttl 90d \
  --actions sandbox.create,sandbox.read,sandbox.delete-own,exec,file.read,file.write,ssh,snapshot.read,snapshot.write
```

Keys expire, 90 days by default; create a new one and update `boat-api-key`
before then.

## Limits and cost

A CI run starts two sandboxes and a snapshot build two. Starts count against the
boat plan's limits (12 a minute on the $20 plan). A `large` sandbox costs
$0.072 an hour while it runs. Each saved snapshot above the plan's included
count costs $0.056 a day, which is why the task keeps one.
