---
title: "Applying updates locally"
---
`patchula apply` looks up the working tree in the current directory and writes
the updates into its files, with no branches and no pull requests. It is for
scripts, Makefiles and pre-commit hooks: pin and update a checkout's CI refs,
images and packages, then review and commit the change yourself.

```sh
patchula apply                                  # write every update
patchula apply --dry-run                        # print the diff instead
patchula apply --check                          # exit 3 if anything is waiting
patchula apply --manager github-actions --type pin --type digest
```

It reads the working tree the way [`lookup`](/reference/cli/#patchula-lookup)
does with no repository named: the `patchula.pkl` here, or `--config`,
uncommitted changes included. It takes the same cache, concurrency and HTTP
flags, and needs a [token](/guide/how-it-works/#tokens) for the lookups like
every other command. It always runs on this machine. Repository arguments,
`--all-repos` and `--remote` are refused, and a checkout you chose to run
on a server is never offered to it.

In a checkout with no `patchula.pkl`, it asks first, like the
[terminal UI](/guide/tui/): `d` (or Enter) uses the Count's defaults for this
run, `w` writes a starter `patchula.pkl` and carries on, and `q` quits.
Scripts and hooks have no terminal to ask on, so they pass `--default-config`
to use the defaults, or the command stops with exit 2 and says so.

The default cache, `.patchula-cache.sqlite`, is created in the current
directory. Ignore it in Git, or pass `--cache` with a path outside the tree.

## What gets written

The edits are the ones the [terminal UI](/guide/tui/#applying-updates) makes:
the Count reads each file again, finds the same declaration at the same place,
and rewrites only its version and digest. A digest pin, such as
`actions/checkout@v5` becoming the commit it points to, is an update like any
other.

When the lookup proposes several updates for one declaration, such as a patch
on the current line and a new minor, the largest is written: major, then
minor, patch, pin and digest. Filter with `--type` to stay on a smaller step.

Some updates cannot be written in place. He leaves them alone, says why, and
writes the rest:

- a package only a lockfile names, such as a vulnerable transitive crate in
  `Cargo.lock`, which only its package manager can update;
- an update the file already allows, such as a new release inside a range
  like `~> 6.0`, where only the lockfile would change;
- a declaration that changed between the lookup and the write.

Lockfiles are not regenerated. When an update's current version came from a
lockfile, the summary reminds you to refresh it with your package manager
(`aube install`, `cargo update -w`, `uv lock` and so on).

## Copier templates

A [Copier](/guide/managers/#copier) template's new tag is not an edit, so it
is never written like the other updates. The summary lists each one, and the
JSON result carries them as `notices`. Without `--copier` they never change the
exit code.

`--copier` asks the Count to run your own `copier update` for each template the
filters select (`--manager` and `--dependency` apply; any `--type` leaves templates
out), before he writes the other updates. It is the same run as the
[TUI's](/guide/tui/#templates): your `copier` from `PATH`, as you, with
`--vcs-ref <new tag> --skip-answered`, never `--trust`, and nothing committed.
The template's code runs on this machine, so read its compare link first.

- The tree must be clean, untracked files included. Copier leaves its changes
  behind, so with two templates the second is refused: commit, then run again.
- Copier asks new questions on the terminal. Without one, the template is
  refused unless you pass `--copier-defaults`, which runs `copier --defaults`.
- A template that needs `--trust` stops copier; review it and run
  `copier update --trust` yourself.
- The other updates are planned against the tree copier left, so a declaration
  copier changed is left alone with a reason.

A template copier did not update makes the command exit 3. `--copier` does not
combine with `--dry-run`, `--check` or `--json`.

## Choosing updates

| Flag | Selects |
|---|---|
| `--manager MANAGER` | updates from this [manager](/guide/managers/), such as `github-actions` or `dockerfile` |
| `--dependency PATTERN` | dependencies whose name as written, or registry package, matches; patterns take `glob:`, `regex:` and `!` as in [config selectors](/guide/configuration/#rules) |
| `--type TYPE` | `major`, `minor`, `patch`, `pin` or `digest` updates |

Each flag can be repeated, and repeats widen it: `--type pin --type digest`
selects both. Different flags narrow each other. The update policy in
`patchula.pkl` still applies first, so an update a rule turns off, or a
release still in its [release-age wait](/guide/lookup-policy/), is never
written.

## Output and exit codes

The summary goes to stderr: what was written, what was left alone and why,
and how many updates the filters left out. `--dry-run` prints a unified diff
of every file to stdout, so `patchula apply --dry-run > pin.patch` gives a
patch for `git apply`. With `--json`, stdout is one JSON object instead:
`updates`, `skipped` with each `reason`, `files` with each `diff`,
`written`, `filtered`, `warnings` and `exitCode`.

| Code | Meaning |
|---|---|
| 0 | nothing selected is left: everything was written, or there was nothing to write |
| 1 | runtime error, such as a failed lookup or an unwritable file |
| 2 | usage or configuration error |
| 3 | selected updates are still waiting: `--check` or `--dry-run` found some, or some could not be written in place |

`--check` and `--dry-run` never write. A real run that leaves something alone
also exits 3, so a script can tell "done" from "done, but look at the
summary".

A dependency whose lookup failed, for example on a registry error, has no
updates, so it cannot make the command exit 3. The failure is printed as a
warning, as `lookup` prints it, and `warnings` carries it in the JSON
result; a strict CI job can fail when that list is not empty.

## In hooks and scripts

A [pre-commit](https://pre-commit.com) hook that fails while the workflows
have refs to pin or update:

```yaml
# .pre-commit-config.yaml
repos:
  - repo: local
    hooks:
      - id: patchula
        name: Count Patchula
        entry: patchula apply --check --manager github-actions
        language: system
        pass_filenames: false
```

Drop `--check` to have the hook write the updates; pre-commit then fails
because files changed, and you stage them.

A Makefile target:

```make
pin:
	patchula apply --type pin --type digest
```

Committing, branching and pushing are left to you. For pull requests, use
[`patchula run`](/guide/pull-requests/).
