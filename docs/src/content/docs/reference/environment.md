---
title: "Environment variables"
---
| Variable | Used by | Meaning |
|---|---|---|
| `PATCHULA_TOKEN` | `lookup`, `run` | GitHub token; falls back to `GITHUB_TOKEN`, then `GH_TOKEN` |
| `PATCHULA_LOG` | all | log level: `error`, `warn` (the CLI default), `info` (the server default), `debug` or `trace`; `-vv` means `debug` |
| `PATCHULA_RECHECK` | `run`, server | `now` or `next-run`: when a ticked checkbox says it takes effect |
| `PATCHULA_TOOLS_DIR` | `run`, `tools` | the lockfile tool store; default `<cache>.tools` |
| `PATCHULA_SERVER` | `lookup`, `run`, `tui`, `auth` | a [server](/guide/server/#run-from-your-machine) URL to run on instead of this machine |
| `XDG_CONFIG_HOME` | `--server` | where the server sign-ins are stored (`patchula/servers.json`); default `~/.config` |
| `PATCHULA_DOCKERHUB_USERNAME` | lookups | Docker Hub user for authenticated pulls; set with the token |
| `PATCHULA_DOCKERHUB_TOKEN` | lookups | Docker Hub access token |
| `PATCHULA_APP_ID` | server | GitHub App ID |
| `PATCHULA_APP_PRIVATE_KEY_FILE` | server | path to the App's PEM key |
| `PATCHULA_APP_PRIVATE_KEY` | server | the PEM itself, instead of a file |
| `PATCHULA_WEBHOOK_SECRET_FILE` | server | path to the webhook secret |
| `PATCHULA_WEBHOOK_SECRET` | server | the webhook secret itself, instead of a file |
| `PATCHULA_SERVER_SETTINGS` | server | the operator settings file |
| `PATCHULA_SERVER_CACHE` | server | the server's SQLite cache |
| `PATCHULA_LISTEN` | server | bind address; default `127.0.0.1:8080` |
| `PATCHULA_SHUTDOWN_TIMEOUT_SECS` | server | drain deadline on SIGTERM, 1 to 3600; default 60 |

Flags override their environment variables. For each secret, set either the
file or the value, not both.

Two more exist for development: `PATCHULA_RECORD=1` records HTTP fixtures
instead of replaying them, and `PATCHULA_REDACT` adds comma-separated literals for the fixture secret
scanner to reject. See [HTTP fixtures](/internals/http-testing/).
