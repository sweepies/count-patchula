---
title: "Terminal UI"
---
`patchula tui` runs a lookup and lets you browse the result: every update, the
releases held back by the release-age wait, every dependency found, template
updates, warnings and the run's stats. Run it in a checkout with no repository named and you
can pick updates and have the Count write them into its files.

```sh
patchula tui acme/my-app      # browse a GitHub repository
patchula tui                  # browse, and apply, the working tree here
patchula tui --once | less    # a plain-text snapshot instead
```

It takes the same flags as [`lookup`](/reference/cli/#patchula-lookup): `--cache`,
`--config`, `--concurrency` and the rest. It needs a terminal on both stdin and
stdout; `--once` prints a snapshot of the updates and exits, for pipes and CI
logs.

In a checkout with no `patchula.pkl`, the Count asks before he starts, with one
keypress: `d` (or Enter) uses his defaults for this run, `w` writes a starter
`patchula.pkl` and carries on, and `q` quits. `--default-config` answers `d`
up front, and is required when there is no terminal to ask on.

```text
 Count Patchula   1 repository · 2576 dependencies · 131 updates · 2 selected
 1 Updates │ 2 Held back │ 3 Dependencies │ 4 Templates │ 5 Warnings │ 6 Stats
 Updates (131 of 131, type: all) ───────────────────────────────────────────────────────────────────
      Dependency                               Current  Target   Type   Age    File
[x]   cloud.google.com/go/auth                 v0.23.3  v0.24.1  minor  12d    go/prometheus/go.mod
[x]   cloud.google.com/go/auth/oauth2adapt     v0.2.8   v0.3.0   minor  26d    go/prometheus/go.mod
[ ]   cloud.google.com/go/compute/metadata     v0.9.1   v0.10.0  minor  26d    go/prometheus/go.mod
[ ]   github.com/Azure/az…k-for-go/sdk/azcore  v1.14.1  v1.15.0  minor  6d     go/prometheus/go.mod
[ ]   github.com/AzureAD/…tion-library-for-go  v1.8.0   v1.10.1  minor  21d    go/prometheus/go.mod
[ ]   github.com/KimMachineGun/automemlimit    v0.7.5   v1.0.0   major  60d    go/prometheus/go.mod
[ ]   github.com/Microsoft/go-winio            v0.6.2   v0.6.4   patch  7d     go/prometheus/go.mod
[ ]   github.com/armon/go-metrics              v0.4.1   v0.4.2   patch  1230d  go/prometheus/go.mod
[ ]   github.com/armon/go-metrics              v0.4.1   v0.7.0   minor  34d    go/prometheus/go.mod
────────────────────────────────────────────────────────────────────────────────────────────────────
cloud.google.com/go/auth/oauth2adapt  go · go/prometheus/go.mod
v0.2.8 → v0.3.0  (minor)
version v0.3.0
published 2026-09-24T19:36:54Z

 q quit  ? help  / search  t type: all  space select  a all  w apply 2 selected
```

## Tabs

| Tab | Shows |
|---|---|
| 1 Updates | every proposed update, with the selected one's package, versions, digest and publication date below |
| 2 Held back | releases still inside the release-age wait, with when each becomes eligible |
| 3 Dependencies | everything the Count found, with current and resolved versions and how many updates each has |
| 4 Templates | template updates the Count reports but never edits, such as a [Copier](/guide/managers/#copier) template's new tag |
| 5 Warnings | lookup and extraction warnings |
| 6 Stats | counts per manager, requests, cache hits and the time each phase took |

## Keys

| Key | Action |
|---|---|
| `q`, `Esc`, `Ctrl-C` | quit |
| `?` | show the keys |
| `Tab`, `→`, `Shift-Tab`, `←` | next and previous tab |
| `1` to `6` | jump to a tab; each tab keeps its cursor while you are away |
| `j`, `k`, `↓`, `↑` | move; on the Warnings tab, scroll a line |
| `PgDn`, `PgUp`, `g`, `Home`, `G`, `End` | page down, page up, top, bottom |
| `/` | search names, packages and files; `Enter` or `Esc` to finish |
| `t` | on the Updates tab, cycle the update type filter: all, major, minor, patch, pin, digest |
| `space` | on the Updates tab, select or clear the update under the cursor |
| `a` | on the Updates tab, select every visible update, or clear them if all are selected |
| `w` | on the Updates tab, apply: write the selection into the working tree, after a `y` to confirm |
| `u` | on the Templates tab, run `copier update` for the template under the cursor, after a `y` to confirm |

The bar at the bottom lists the keys that work on the current tab, and wraps
rather than cutting off on a narrow terminal. When `w` or `u` cannot run, the
bar says why next to the key: the lookup ran on a server, the repositories come
from GitHub, nothing is selected yet, or, for copier, the tree has uncommitted
changes. A message from the last key appears above the bar until the next key.

Warnings are laid out like the end of a `lookup` report: grouped by
repository when there are several, failed lookups one row each with the
message under it, then the other warnings. They wrap to the width of the
terminal. When they fill more than the pane, the title shows which lines are
on screen, and `j`, `k`, `PgDn` and `PgUp` scroll through them, as the bar
says.

The Updates table sizes its columns by what is in them. The current version
takes up to 32 columns and the target up to 48, enough for a version with a
short digest; on a narrow terminal they give way last, after the dependency
name and the file. A long dependency name is cut in the middle, keeping its
start and its last segment.

## Applying updates

Select updates with `space` or `a`, press `w`, then `y` to write them; any
other key cancels. Applying only works on the working tree in the current
directory, with no repository named and not on a server. If this checkout is
set to run on a server, pass `--here` to look it up on this machine and apply.
The Count reads each selected file again,
finds the same declaration at the same place, and rewrites just the version,
the way he edits a branch. If a selected update cannot be written in place,
because its declaration changed since the lookup, the package is only in a
lockfile, or another selected update takes the same declaration, he refuses
with the reason rather than guess, and nothing is written. Lockfiles are not regenerated, so
run your package manager afterwards (`aube install`, `cargo update -w`,
`uv lock` and so on).

Template updates are never selected with the other updates, and `w` never
touches them: some managers only report a newer version. See
[Managers](/guide/managers/).

To write updates without picking them, in a script or a pre-commit hook, use
[`patchula apply`](/guide/apply/). It writes through the same edits, and
leaves alone, with a reason, what it cannot write in place.

For GitHub repositories the TUI is for looking. Let [`patchula run`](/guide/pull-requests/)
open the pull requests.
