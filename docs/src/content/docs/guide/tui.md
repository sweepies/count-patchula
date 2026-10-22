---
title: "Terminal UI"
---
`patchula tui` runs a lookup and lets you browse the result: every update, the
releases held back by the release-age wait, every dependency found, warnings
and the run's stats. Point it at a directory with `--local` and you can pick
updates and have the Count write them into your files.

```sh
patchula tui acme/my-app            # browse a GitHub repository
patchula tui --local .                  # browse, and apply, the current directory
patchula tui --once --local . | less    # a plain-text snapshot instead
```

It takes the same flags as [`lookup`](/reference/cli/#patchula-lookup): `--cache`,
`--config`, `--concurrency` and the rest. It needs a terminal on both stdin and
stdout; `--once` prints a snapshot of the updates and exits, for pipes and CI
logs.

```text
 Count Patchula   1 repository · 2576 dependencies · 131 updates · 2 selected
 1 Updates │ 2 Held back │ 3 Dependencies │ 4 Warnings │ 5 Stats
 Updates (131 of 131, type: all) ───────────────────────────────────────────────────────────────────
     Dependency                  Current        Target               Type   Age   File
[x]  cloud.google.com/go/auth    v0.23.3        v0.24.1              minor  12d   go/prometheus/go.m
[x]  cloud.google.com/go/auth/oa v0.2.8         v0.3.0               minor  26d   go/prometheus/go.m
[ ]  cloud.google.com/go/compute v0.9.1         v0.10.0              minor  26d   go/prometheus/go.m
[ ]  github.com/Azure/azure-sdk- v1.14.1        v1.15.0              minor  6d    go/prometheus/go.m
[ ]  github.com/AzureAD/microsof v1.8.0         v1.10.1              minor  21d   go/prometheus/go.m
[ ]  github.com/KimMachineGun/au v0.7.5         v1.0.0               major  60d   go/prometheus/go.m
[ ]  github.com/Microsoft/go-win v0.6.2         v0.6.4               patch  7d    go/prometheus/go.m
[ ]  github.com/armon/go-metrics v0.4.1         v0.4.2               patch  1230d go/prometheus/go.m
[ ]  github.com/armon/go-metrics v0.4.1         v0.7.0               minor  34d   go/prometheus/go.m
────────────────────────────────────────────────────────────────────────────────────────────────────
cloud.google.com/go/auth/oauth2adapt  go · go/prometheus/go.mod
v0.2.8 → v0.3.0  (minor)
version v0.3.0
published 2026-09-24T19:36:54Z

 q quit · ? help · tab switch · j/k move · / search · t type: all
```

## Tabs

| Tab | Shows |
|---|---|
| 1 Updates | every proposed update, with the selected one's package, versions, digest and publication date below |
| 2 Held back | releases still inside the release-age wait, with when each becomes eligible |
| 3 Dependencies | everything the Count found, with current and resolved versions and how many updates each has |
| 4 Warnings | lookup and extraction warnings |
| 5 Stats | counts per manager, requests, cache hits and the time each phase took |

## Keys

| Key | Action |
|---|---|
| `q`, `Esc`, `Ctrl-C` | quit |
| `Tab`, `→`, `Shift-Tab`, `←` | next and previous tab |
| `1` to `5` | jump to a tab |
| `j`, `k`, `↓`, `↑` | move |
| `PgDn`, `PgUp`, `g`, `G` | page down, page up, top, bottom |
| `/` | search names, packages and files; `Enter` or `Esc` to finish |
| `t` | cycle the update type filter: all, major, minor, patch, pin, digest |
| `space` | select the update under the cursor |
| `a` | select every visible update, or clear them if all are selected |
| `A` | apply the selection (local directories only), after a `y` to confirm |
| `?` | help |

## Applying updates

Applying only works with `--local`. The Count reads each selected file again,
finds the same declaration at the same place, and rewrites just the version,
the way he edits a branch. If a file changed since the lookup he refuses
rather than guess, and nothing is written. Lockfiles are not regenerated, so
run your package manager afterwards (`aube install`, `cargo update -w`,
`uv lock` and so on).

For GitHub repositories the TUI is for looking. Let [`patchula run`](/guide/pull-requests/)
open the pull requests.
