# API server (Phase 2b / SWE-90)

`rustovate --server` runs the same write engine as `rustovate run`. There is no
web UI or settings API. The only routes are `POST /webhook`, `GET /healthz`, and
`GET /readyz`. Bind defaults to `127.0.0.1:8080`; use an HTTPS reverse proxy for
GitHub deliveries. rustovate is independent of renovate-runner.

## Operator settings

Create `/etc/rustovate/server.pkl` (the schema is bundled; no adjacent schema
file is needed). The repository-root `RustovateServer.pkl` is the published
operator schema; repository dependency policies still amend `Rustovate.pkl`.
`mise run server:schema` prints the bundled schema.
`mise exec -- cargo run --locked -p rustovate-cli -- config check-server /etc/rustovate/server.pkl`
validates operator settings offline, without credentials.

```pkl
amends "RustovateServer.pkl"

allowedInstallations = new Listing { 123456 }
allowedRepositories = new Listing { "sweepies/rustovate-sandbox" }
reportDirectory = "/var/lib/rustovate/reports"
```

Empty allowlists permit all repositories/installations accessible to this App;
both nonempty lists must match. Repository matching is case-insensitive. Work
is filtered both at acceptance and immediately before execution. An installation
discovery job is filtered again when its repository list is retrieved.

Additional settings and defaults:

- `triggers { push = true; dashboard = true; pullRequestEdited = true;
  pullRequestClosed = true; installation = true; checks = true }` (put each
  property on its own line in Pkl).
- `sweepCron = "0 0 * * * *"`, `sweepTimezone = "UTC"`: hourly, six cron fields
  including seconds (optional seventh year field). Named IANA timezones are
  supported. `sweepCron = null` disables recurring sweeps. Startup always
  discovers installed repositories to recover installation inventory.
- `concurrency = 8` (1–100 repository scans), `batchSize = 32` (1–1000 queue
  entries), `coalescingMs = 1000` (0–60000).
- `recheck = null`: server defaults to `now`. `"next-run"` overrides wording.
- `reportDirectory = null`: log each version 2 report as JSON in the structured
  event's `report` string field. Set a directory for atomic, unique `batch-*.json`
  files instead. Keep INFO logging enabled when using report logs. Directory
  errors cause the batch to be retried. Manage retention externally.

Settings and local Pkl imports are reevaluated periodically (normally once per
second). Relative and absolute local imports work; remote imports, environment reads and resource reads
are disabled. Invalid edits keep the last good snapshot, emit a sanitized error,
and make readiness fail until repaired. Edits apply between batches; an existing
batch retains its snapshot. An invalid initial file prevents startup. Settings
are operator-owned files; rustovate never rewrites them.
`config check-server` and startup errors include the cause, file and available
source span; string contents are masked in diagnostic excerpts. Reload errors
direct operators to `config check-server` without logging source excerpts.

## Credentials and startup

Install the GitHub App on the sandbox. It needs the same permissions as CLI write
mode: contents write, pull requests write, issues write for the dashboard/labels,
workflows write for workflow updates, and checks/commit statuses read for merge
fallback. Repository metadata read is implicit. Subscribe to push, issues, pull
request, installation, installation repositories, check suite and check run
events, and configure a webhook secret. A repository or organization webhook may
also point at this server, using its configured webhook secret; the App's own
webhook URL can remain with another system. Keep branch protection and the existing
[merge fallback gates](auto-merge.md) in place.

Set environment variables (or the equivalent flags):

```sh
export RUSTOVATE_APP_ID=12345
export RUSTOVATE_APP_PRIVATE_KEY_FILE=/etc/rustovate/app.pem
export RUSTOVATE_WEBHOOK_SECRET_FILE=/etc/rustovate/webhook-secret
export RUSTOVATE_SERVER_SETTINGS=/etc/rustovate/server.pkl
export RUSTOVATE_SERVER_CACHE=/var/lib/rustovate/cache.sqlite
mise run server
```

`--settings`, `--server-cache`, `--listen`, `--app-id`, `--private-key-file`,
`--webhook-secret-file`, and `--shutdown-timeout-secs` override their environment
counterparts. `RUSTOVATE_LISTEN` defaults to `127.0.0.1:8080` and
`RUSTOVATE_SHUTDOWN_TIMEOUT_SECS` defaults to 60 (1–3600).
`--recheck now|next-run` overrides `RUSTOVATE_RECHECK`, which overrides Pkl.
The default wording is “Created by rustovate. Click to re-check now.” CLI mode
continues to default to “on the next run”. Hidden markers identify checkboxes.

Instead of files, `RUSTOVATE_APP_PRIVATE_KEY` may contain the PEM and
`RUSTOVATE_WEBHOOK_SECRET` may contain the secret. Choose one source per secret;
supplying both a file and an environment value is an error. File contents are
read at startup with trailing CR/LF removed. Rotate secrets by restarting the
service. Secrets cannot be supplied in settings or command-line values. No key,
JWT, installation token, signature, request body or webhook secret is logged.
Signing keys and tokens stay in memory; installation tokens never enter SQLite.
Protect credential files, the cache, reports and logs with operator filesystem
permissions. Caller-token environment variables are used only by CLI mode.

The App signs RS256 JWTs with `iat` 60 seconds in the past and a nine-minute
future expiry. Installation tokens are cached separately per installation and
renewed two minutes before expiration. Every GitHub API request and lockfile Git
operation resolves credentials through that cache. Engine instances, HTTP pool,
registry state, SQLite connection workers and write receipts stay warm across
batches and token renewals. Synchronize the host clock. See GitHub's
[JWT requirements](https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/generating-a-json-web-token-jwt-for-a-github-app).

`/healthz` reports process liveness. `/readyz` requires a valid settings snapshot,
a working cache, and a successful App/token probe. Probes run at startup and at
least every 30 seconds while GitHub is responding; every allowed installation
must be mintable. With no allowed installations readiness stays 503. `/readyz`
does no remote I/O and returns 503 while draining. The App identity is checked
before opening the listener. Logs are JSON on stderr with sanitized fields;
`RUSTOVATE_LOG=debug` adds the existing request/phase diagnostics.

## Delivery and queue behavior

The webhook route limits bodies to 2 MiB. It verifies `X-Hub-Signature-256` using
HMAC-SHA256 and a constant-time MAC comparison on the raw bytes **before** parsing
JSON. Missing/invalid signatures return 401; signed invalid JSON or missing
event/delivery headers return 400; oversized bodies return 413. A signed `ping`
returns 200 without installation lookup or enqueue. Durable acceptance returns
202; SQLite failure or draining returns 503, allowing redelivery.
Every non-2xx application response has a JSON `error_kind` and a structured
`request rejected` log with the same kind and HTTP `status`. This boundary also
covers routing errors and body-read failures before webhook processing. Examples
include `bad-signature`, `body-too-large`, `body-read-failed`, `cache-busy`,
`cache-worker-unavailable`, `installation-lookup-failed`,
`installation-resolution-timeout` and `draining`. Error bodies and logs never
include upstream response bodies, raw request bodies or headers. Failures after
signature/metadata validation include the validated event and delivery ID.
Responses generated by a reverse proxy before reaching the server cannot appear
in the server's request logs; inspect proxy diagnostics for those responses.

Repository and organization webhook payloads can omit `installation`. The server
resolves the repository with the [App JWT installation endpoint](https://docs.github.com/en/rest/apps/apps#get-a-repository-installation-for-the-authenticated-app),
or resolves `organization.login` when there is no repository. It never falls back
to the organization endpoint when a repository lookup fails. Successful mappings
are cached in memory and SQLite for five minutes, scoped to the App ID; cache
hits do not extend expiry. A lookup returning 404 removes the mapping and accepts
the delivery without enqueue; absence is not cached. Other lookup failures or a
five-second resolution timeout return 503 so GitHub can redeliver. Allowlists
apply to the resolved installation and repositories before enqueue.

`X-GitHub-Delivery` is deduplicated transactionally with enqueue and retained for
seven days. Ignored events also return 202. Ignore rules cover this App's sender,
disabled triggers, disallowed installations/repositories and unrelated events.
Each delivery emits one INFO decision with event, action, delivery ID,
installation, repository and outcome/reason. Invalid signatures emit only the
decision, without delivery metadata.
Resolved deliveries use reason `installation-resolved`, or
`installation-not-found` when no installation exists. The
`installation_resolution` field retains the lookup outcome when another rule,
such as `not allowlisted`, explains why the delivery was ignored.
Triggers are:

- A non-deletion push to the repository's default branch.
- Newly checked marker actions on an App-owned, marked dashboard issue.
- Newly checked recheck marker on an App-owned, marked, same-repository PR body.
- Closure of an App-owned, marked, same-repository PR.
- Installation creation or repository addition (including durable discovery
  when GitHub omits the repository array).
- Completed check suite/run on an App-owned, marked, same-repository PR.
  GitHub's check payload has abbreviated PR records without body/author. PR
  numbers are persisted with the repository queue entry and verified against
  fresh GitHub PR details asynchronously, before invoking the engine. Custom
  branch prefixes work. Unrelated/fork PRs do not start engine batches.

One queue drains one installation batch at a time; an engine batch shares source
lookups and bounds repository scans by `concurrency`. The first queued event
starts the coalescing window; additional events do not postpone it indefinitely.
Events during a batch advance its generation and coalesce into one follow-up
batch after completion. Failures retry after 60 seconds. A default branch that
moves or changes after extraction skips stale branch publication and requeues
the work after 60 seconds. It logs WARN `default branch moved; requeued` and
finishes with `success=true`, `requeued=true` when there are no other errors.
Actual failures still log ERROR with `error_kinds` and `error_count`, counting
failed/protected report entries by repository, branch and pull request scope
(repository summaries can also describe errors counted in actions).
At-least-once execution
is intentional: engine ownership checks and verified receipts reconcile writes.
Batch start logs list their persisted reasons (startup discovery, sweep, webhook
trigger or retry) and coalesced delivery IDs. Discovery passes that attribution
to the repository work it enqueues. Every batch that runs repositories emits a
report; discovery-only batches and batches with no eligible repository runs log
an explicit report skip with the reason.
Startup therefore normally has two passes per installation: a discovery-only
batch enumerates repositories and durably enqueues them, then a repository batch
processes the inventory after the coalescing window. These are separate queue
claims so discovery can complete independently and repository work survives a
restart. Only the second pass produces a repository report. Cache writes use one
writer and short transactions; engine batches do not hold a SQLite transaction
while awaiting GitHub. A competing external writer can exhaust the five-second
SQLite busy timeout; its delivery receives a retryable rejection without
consuming the deduplication ID.

Queue rows stay persisted during execution. SIGTERM/Ctrl-C stops acceptance and
new claims, drains the current batch within the deadline, then requeues unfinished
work. Restart also recovers in-flight rows and retains delivery IDs. A process
lock beside the cache enforces one server owner. This is a single-process queue,
not a multi-instance scheduler. Keep its SQLite volume persistent and use a
separate cache from concurrently running CLI jobs.

## Sandbox verification and local replay

Use the App installed on `sweepies/rustovate-sandbox`, its installation ID, and
the allowlists in the minimal example. The sandbox must have its own
`.github/rustovate.pkl` (or `rustovate.pkl`) and the required lockfile tools
available through mise. No local repository policy override is needed. Run a CLI
dry run with a caller-provided installation token first if reviewing proposed
writes; server batches apply repository policy immediately, including merge
policy. This implementation was validated offline; no live sandbox writes were
performed as part of implementation.

Save a recorded GitHub payload as `/tmp/push.json`, or create this minimal
synthetic push using the actual installation ID:

```json
{
  "installation": { "id": 123456 },
  "sender": { "login": "sandbox-operator" },
  "repository": {
    "id": 123,
    "full_name": "sweepies/rustovate-sandbox",
    "default_branch": "main"
  },
  "ref": "refs/heads/main",
  "deleted": false
}
```

With the same secret file/env exported in the replay shell:

Set **only one** of `RUSTOVATE_WEBHOOK_SECRET_FILE` and
`RUSTOVATE_WEBHOOK_SECRET` for `mise run server:replay`. Unset the other variable;
the task refuses to run when both are set, even if they contain the same secret.

```sh
mise run server:replay /tmp/push.json push local-push-001
# Same delivery ID: accepted but ignored; use a new ID to trigger again.
mise run server:replay /tmp/push.json push local-push-001
# Optional fourth argument selects another loopback listener.
mise run server:replay /tmp/push.json push local-push-002 http://127.0.0.1:8080/webhook
```

The mise task uses pinned Bun's built-in HMAC and fetch; no npm or standalone
script is needed. It computes the signature over exactly the recorded bytes,
prints only HTTP status, refuses non-loopback targets and never prints the
secret/signature. Replay other recorded events by changing the event argument.
Check batch reports, tick a sandbox dashboard/PR checkbox, close a sandbox PR,
and complete checks to exercise the live triggers. Edit the Pkl concurrency,
then make an invalid edit and check `/readyz` (503); repair it (200 after reload).
Send SIGTERM during a batch, restart with the same cache, and confirm unfinished
repositories are retried. Ordinary tests cover these cases without remote I/O.

## systemd example

Install the built binary and pinned mise/tool environment for the service user;
the lockfile runner still invokes mise. Put only non-secret environment selectors
in `/etc/rustovate/environment` and use systemd credentials for secret files:

```ini
[Unit]
Description=rustovate GitHub webhook server
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=rustovate
Group=rustovate
StateDirectory=rustovate
WorkingDirectory=/var/lib/rustovate
EnvironmentFile=/etc/rustovate/environment
LoadCredential=app.pem:/etc/rustovate/app.pem
LoadCredential=webhook-secret:/etc/rustovate/webhook-secret
ExecStart=/usr/local/bin/rustovate --server --settings /etc/rustovate/server.pkl --server-cache /var/lib/rustovate/cache.sqlite --private-key-file %d/app.pem --webhook-secret-file %d/webhook-secret --shutdown-timeout-secs 60
Restart=on-failure
RestartSec=10
TimeoutStopSec=75
KillSignal=SIGTERM
UMask=0077
NoNewPrivileges=true

[Install]
WantedBy=multi-user.target
```

The environment file needs `RUSTOVATE_APP_ID`, a PATH including mise, and the
service user's mise/tool locations if nonstandard. Keep the systemd stop timeout
longer than the server drain deadline. Route only the three API endpoints through
the reverse proxy; use the same secret in the GitHub App webhook configuration.
