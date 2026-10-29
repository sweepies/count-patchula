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
patchula apply --check                          # exit 3 if anything is waiting, 4 if a lookup failed
patchula apply --manager github-actions --type pin --type digest
```

It reads the working tree the way [`lookup`](/reference/cli/#patchula-lookup)
does with no repository named: the `patchula.pkl` here, or `--config`,
uncommitted changes included. It takes the same cache, concurrency and HTTP
flags, and needs a [token](/guide/how-it-works/#tokens) for the lookups like
every other command. It always runs on this machine. Repository arguments,
`--all-repos` and `--remote` are refused, and a checkout you chose to run
on a server is never offered to it.

In a checkout with no `patchula.pkl` it [asks
first](/guide/configuration/#where-the-config-lives). Scripts and hooks pass
`--default-config`, or the command exits 2.

Nothing is written to the checkout except the updates. The cache lives in your
user cache directory, shared by every checkout; see
[Installation](/installation/#what-the-runner-needs) for where, and `--cache DIR` to
move it.

## What gets written

The Count reads each file again, finds the same declaration at the same place,
and rewrites only its version and digest; the [terminal UI](/guide/tui/) makes
the same edits. A digest pin, such as
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

Some managers only report a newer version and never edit it, such as a
template's new tag. The summary lists those, and the JSON result carries them
as `notices`; see [Managers](/guide/managers/).

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

The summary goes to stderr. It opens with what was written, or would be, and
lists those updates; then what was left alone and why, template updates,
failed lookups and warnings; and last how many updates the filters left out. `--dry-run` prints a unified diff
of every file to stdout, so `patchula apply --dry-run > pin.patch` gives a
patch for `git apply`. With `--json`, stdout is one JSON object instead:
`updates`, `skipped` with each `reason`, `files` with each `diff`,
`written`, `filtered`, `warnings`, `lookupFailures` and `exitCode`.
`lookupFailures` lists each failed lookup as the
[report](/reference/report/#lookup-failures) does.

| Code | Meaning |
|---|---|
| 0 | nothing selected is left: everything was written, or there was nothing to write |
| 1 | runtime error, such as an unreadable working tree or an unwritable file |
| 2 | usage or configuration error |
| 3 | selected updates are still waiting: `--check` or `--dry-run` found some, or some could not be written in place |
| 4 | a dependency's lookup failed, so its updates are unknown |

`--check` and `--dry-run` never write. A real run that leaves something alone
also exits 3, so a script can tell "done" from "done, but look at the
summary".

Exit 4 wins over 3, since a failed lookup means the waiting list may be
incomplete; without `--check` or `--dry-run`, what could be looked up is still
written. Each failure is listed under "Lookup failures" with the dependency,
its file, its datasource, the kind of error and the registry's message. The
[CLI reference](/reference/cli/) has the exit codes of the other commands.

## Copier templates

The Count runs copier only when you ask, on your own working tree: `u` on the
[TUI](/guide/tui/)'s Templates tab updates one template after a `y`, and
`patchula apply --copier` updates every template the filters select
(`--manager` and `--dependency` apply; any `--type` leaves templates out) before
writing the other updates, which are then planned against the tree copier
left. It is your own `copier` from `PATH`, run as you with
`copier update --vcs-ref <new tag> --skip-answered`, never `--trust`, and
nothing is committed. The template's new, unreviewed code runs with your
permissions, so read the compare link first.

- The tree must be clean, untracked files included. With `apply` and two
  templates, the second is refused: commit, then run again.
- Copier asks the template's new questions on the terminal. Without one,
  `apply` refuses the template unless you pass `--copier-defaults`.
- A template that needs `--trust` stops copier; review it and run
  `copier update --trust` yourself.
- Conflicts stay as copier leaves them, and the Count lists the files it
  changed and the ones with conflicts.
- With `apply`, a template copier did not update exits 3, and `--copier`
  does not combine with `--dry-run`, `--check` or `--json`.

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
