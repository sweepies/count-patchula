# GitHub App server

`patchula --server` keeps the Count up all night. It runs the same engine as
`patchula run`, but as a long-lived process: it takes GitHub webhooks, keeps one
deduplicated queue, sweeps every installed repository on a schedule, mints its
own installation tokens, and keeps every cache warm between batches.

There is no web UI. The only routes are `POST /webhook`, `GET /healthz` and
`GET /readyz`. It listens on `127.0.0.1:8080` by default, so put an HTTPS
reverse proxy in front for GitHub's deliveries.

## Create the GitHub App

The App needs:

- repository permissions: contents write, pull requests write, issues write
  (dashboard and labels), workflows write (workflow updates), and checks and
  commit statuses read (merge fallback);
- events: push, issues, pull request, installation, installation repositories,
  check suite and check run;
- a webhook secret.

A repository or organisation webhook can point at the server instead of the
App's own webhook URL, using the same secret.

## Settings

Operator settings are a Pkl file that amends the bundled `PatchulaServer.pkl`.
They are separate from repository configs, which still amend `Patchula.pkl`.

```pkl
// /etc/patchula/server.pkl
amends "PatchulaServer.pkl"

allowedInstallations { 123456 }
allowedRepositories { "sweepies/my-app" }
sweepCron = "0 0 * * * *"   // hourly; six fields, seconds first
reportDirectory = "/var/lib/patchula/reports"
```

```sh
patchula config server-schema           # print the schema
patchula config check-server /etc/patchula/server.pkl
```

Empty allowlists allow everything the App can see. The file is re-read about
once a second. An invalid edit keeps the last good settings, logs a sanitised
error and makes `/readyz` fail until it is fixed; a running batch keeps the
snapshot it started with. Every setting is in the [server settings
reference](/reference/server-settings).

## Credentials and startup

```sh
export PATCHULA_APP_ID=12345
export PATCHULA_APP_PRIVATE_KEY_FILE=/etc/patchula/app.pem
export PATCHULA_WEBHOOK_SECRET_FILE=/etc/patchula/webhook-secret
export PATCHULA_SERVER_SETTINGS=/etc/patchula/server.pkl
export PATCHULA_SERVER_CACHE=/var/lib/patchula/cache.sqlite
patchula --server
```

`PATCHULA_APP_PRIVATE_KEY` and `PATCHULA_WEBHOOK_SECRET` can hold the values
directly instead, but not both a file and a value for the same secret. Secrets
never appear in settings or command-line arguments, and no key, JWT, token,
signature or request body is ever logged. Installation tokens live in memory,
are renewed two minutes before they expire, and never touch SQLite. Keep the
host clock synchronised; JWTs are issued 60 seconds in the past.

`/healthz` is liveness. `/readyz` needs valid settings, a working cache and a
successful token probe for every allowed installation, and returns 503 while
draining. Logs are JSON on stderr; `PATCHULA_LOG=debug` adds request-level
detail.

## What starts a batch

- a push to a repository's default branch;
- a ticked box on the Count's dashboard, or the re-check, add-to-batch or
  remove-from-batch box on one of his PRs;
- one of his PRs being closed;
- the App being installed, or a repository being added;
- checks completing on one of his PRs;
- the sweep schedule, and startup.

Each trigger can be turned off under `triggers`. Events from the App itself,
disallowed repositories and unrelated events are acknowledged and ignored.

## Webhook handling

Signatures (`X-Hub-Signature-256`) are verified over the raw bytes before any
JSON is parsed. Bodies are limited to 2 MiB. A bad signature gets 401, a signed
but malformed delivery 400, and a durably queued one 202. If SQLite is busy or
the server is draining, the answer is 503 so GitHub redelivers. Delivery IDs are
deduplicated for seven days. Every rejection carries a JSON `error_kind`, such as
`bad-signature` or `draining`.

Repository and organisation webhooks can omit the installation; the server
looks it up with the App JWT and caches the answer for five minutes.

## The queue

One installation batch runs at a time, with up to `concurrency` repositories
scanned in parallel. The first event opens a coalescing window
(`coalescingMs`), and events that arrive during a batch fold into one follow-up
batch. Failures retry after 60 seconds. If the default branch moves while a
batch is writing, the stale publication is skipped and the repository is
requeued.

Queue rows survive restarts. SIGTERM stops new work, drains the current batch
within `--shutdown-timeout-secs` (60 by default), and requeues the rest. A lock
beside the cache allows one server per cache; give CLI jobs a separate cache.

## Replaying a webhook locally

```sh
mise run server:replay /tmp/push.json push local-push-001
```

The `server:replay` task signs a recorded payload with your webhook secret and
posts it to a loopback listener. It prints only the HTTP status and refuses
non-loopback targets. Set only one of `PATCHULA_WEBHOOK_SECRET_FILE` and
`PATCHULA_WEBHOOK_SECRET` when you use it. A repeated delivery ID is accepted
but ignored, so use a new ID to trigger again.

## systemd

The lockfile runner still calls mise, so install mise and the tool store for the
service user.

```ini
[Unit]
Description=Count Patchula GitHub webhook server
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=patchula
Group=patchula
StateDirectory=patchula
WorkingDirectory=/var/lib/patchula
EnvironmentFile=/etc/patchula/environment
LoadCredential=app.pem:/etc/patchula/app.pem
LoadCredential=webhook-secret:/etc/patchula/webhook-secret
ExecStart=/usr/local/bin/patchula --server --settings /etc/patchula/server.pkl --server-cache /var/lib/patchula/cache.sqlite --private-key-file %d/app.pem --webhook-secret-file %d/webhook-secret --shutdown-timeout-secs 60
Restart=on-failure
RestartSec=10
TimeoutStopSec=75
KillSignal=SIGTERM
UMask=0077
NoNewPrivileges=true

[Install]
WantedBy=multi-user.target
```

`/etc/patchula/environment` needs `PATCHULA_APP_ID` and a `PATH` that includes
mise. Keep the stop timeout longer than the drain deadline.
