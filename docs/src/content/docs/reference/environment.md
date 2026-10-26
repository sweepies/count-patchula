---
title: "Environment variables"
---
| Variable | Used by | Meaning |
|---|---|---|
| `PATCHULA_TOKEN` | `lookup`, `run` | GitHub token; falls back to `GITHUB_TOKEN`, then `GH_TOKEN`, then `gh auth token --hostname github.com` |
| `PATCHULA_LOG` | all | log level: `error`, `warn` (the CLI default), `info` (the server default), `debug` or `trace`; `-vv` means `debug` |
| `PATCHULA_RECHECK` | `run`, server | `now` or `next-run`: when a ticked checkbox says it takes effect |
| `PATCHULA_TOOLS_DIR` | `run`, `tools` | the tool store, holding the lockfile tools and the pinned mise that lists `mise` tools; default `tools/` in the cache directory |
| `XDG_CACHE_HOME` | local commands, server | the user cache directory on Linux and other non-macOS systems, holding `patchula/` (the server's default is `patchula/server`); default `~/.cache`. macOS uses `~/Library/Caches` |
| `PATCHULA_SERVER` | `lookup`, `run`, `tui`, `auth` | the [server](/guide/server/#run-from-your-machine) URL for `--remote`, and for `auth` without `--server` |
| `XDG_CONFIG_HOME` | `lookup`, `run`, `tui`, `auth` | where the server sign-ins (`patchula/servers.json`) and per-repository server choices (`patchula/repositories.json`) are stored; default `~/.config` |
| `PATCHULA_DOCKERHUB_USERNAME` | lookups | Docker Hub user for authenticated pulls; set with the token |
| `PATCHULA_DOCKERHUB_TOKEN` | lookups | Docker Hub access token |
| `PATCHULA_APP_ID` | server | GitHub App ID |
| `PATCHULA_APP_PRIVATE_KEY_FILE` | server | path to the App's PEM key |
| `PATCHULA_APP_PRIVATE_KEY` | server | the PEM itself, instead of a file |
| `PATCHULA_WEBHOOK_SECRET_FILE` | server | path to the webhook secret |
| `PATCHULA_WEBHOOK_SECRET` | server | the webhook secret itself, instead of a file |
| `PATCHULA_SERVER_SETTINGS` | server | the operator settings file |
| `PATCHULA_SERVER_CACHE` | server | the server's cache and queue directory |
| `PATCHULA_LISTEN` | server | bind address; default `127.0.0.1:8080` |
| `PATCHULA_SHUTDOWN_TIMEOUT_SECS` | server | drain deadline on SIGTERM, 1 to 3600; default 60 |
| `NO_COLOR` | all | turns colour off: any value for `--help`, which prints as when it is piped; any non-empty value for reports, the spinner and errors, unless `--color always` forces it |
| `CLICOLOR_FORCE` | `--help` | any value but `0` colours help that is piped; `NO_COLOR` still wins |
| `COLUMNS` | `--help` | the width help wraps to; default the terminal's, else 80 |
| `COLORTERM` | all | `truecolor` or `24bit` draws the Count's colours exactly |
| `TERM` | all | without `COLORTERM`, a value containing `256color` uses the nearest of 256 colours, and anything else the 16 ANSI colours; `dumb` hides the spinner |

Flags override their environment variables. For each secret, set either the
file or the value, not both.

Two more exist for development: `PATCHULA_RECORD=1` records HTTP fixtures
instead of replaying them, and `PATCHULA_REDACT` adds comma-separated literals for the fixture secret
scanner to reject. See [HTTP fixtures](/internals/http-testing/).
