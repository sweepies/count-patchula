use crate::{
    reload::Reload,
    webhook::{BODY_LIMIT, Webhook},
};
use axum::{
    Router,
    extract::DefaultBodyLimit,
    http::StatusCode,
    routing::{get, post},
};
use chrono::Utc;
use futures_util::{FutureExt, future::BoxFuture};
use patchula_cache::{
    Cache,
    queue::{Attribution, Finish, Work},
};
use patchula_config::settings::Settings;
use patchula_engine::Engine;
use patchula_github::{Client, app::App};
use patchula_http::Transport;
use patchula_report::{Report, Repository, SCHEMA_VERSION, Status};
use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    path::PathBuf,
    str::FromStr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    net::TcpListener,
    sync::{Mutex, watch},
};

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct Error(&'static str);
pub struct Options {
    pub settings: PathBuf,
    pub cache: PathBuf,
    pub listen: std::net::SocketAddr,
    pub app_id: u64,
    pub private_key: Vec<u8>,
    pub webhook_secret: Vec<u8>,
    pub recheck: Option<patchula_words::Recheck>,
    pub shutdown_timeout: Duration,
}

pub async fn serve(options: Options) -> miette::Result<()> {
    let settings = Settings::load(&options.settings)?;
    patchula_engine::settings::check(&settings).map_err(|(field, message)| {
        patchula_config::settings_problem(Some(&options.settings), field, message)
    })?;
    let settings = Reload::new(settings);
    serve_with_settings(options, settings)
        .await
        .map_err(miette::Report::msg)
}

async fn serve_with_settings(options: Options, settings: Reload) -> Result<(), Error> {
    if options.webhook_secret.is_empty() {
        return Err(Error("webhook secret must be nonempty"));
    }
    std::fs::create_dir_all(&options.cache).map_err(|_| Error("cannot create cache directory"))?;
    // One queue owner per cache directory. The OS releases this advisory lock on crash.
    let lock_path = options.cache.join("server.lock");
    let lock = std::fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock_path)
        .map_err(|_| Error("cannot open server lock"))?;
    lock.try_lock()
        .map_err(|_| Error("another server owns this cache"))?;
    let cache = Cache::open_dir(&options.cache, Default::default())
        .await
        .map_err(|_| Error("cannot open server cache"))?;
    cache
        .recover_work(Utc::now().timestamp_millis())
        .await
        .map_err(|_| Error("cannot recover pending work"))?;
    let transport: Arc<dyn Transport> =
        Arc::new(patchula_http::Network::new().map_err(|_| Error("cannot create HTTP transport"))?);
    let app = App::new(
        options.app_id,
        &options.private_key,
        transport.clone(),
        patchula_engine::settings::github_options(&settings.current()).api_url,
    )
    .map_err(|_| Error("invalid GitHub App credentials"))?;
    // The image has no mise of its own: the first start installs the pinned
    // one into the tool store, and later starts find it there.
    if let Some(workspace) = cache.workspace_dir() {
        let tools = patchula_update::lockfiles::tools_dir(workspace);
        match patchula_update::lockfiles::bootstrap_mise(&tools).await {
            Ok(Some(mise)) => {
                tracing::info!(target: "patchula_server", mise = %mise.display(), "lockfile tools run with this mise");
            }
            Ok(None) => {
                tracing::warn!(target: "patchula_server", "no mise on PATH and no packslip to install it; mise tools and lockfiles will fail");
            }
            Err(error) => {
                tracing::error!(target: "patchula_server", %error, "cannot install mise; mise tools and lockfiles will fail until the next start");
            }
        }
    }
    // The canonical slug is fetched, never guessed from repository names or senders.
    let bot = app
        .bot_login()
        .await
        .map_err(|_| Error("GitHub App authentication failed"))?;
    // Without a client ID the CLI cannot sign in, but webhooks still work.
    let client_id = app.client_id().await.ok();
    if client_id.is_none() {
        tracing::warn!(target: "patchula_server", "cannot read the App's client ID; --remote runs are unavailable");
    }
    // Advertising a server the CLI cannot sign in to would only send users to an error.
    let derived_url = match &client_id {
        Some(_) => app
            .webhook_url()
            .await
            .ok()
            .and_then(|url| patchula_config::settings::from_webhook(&url)),
        None => None,
    };
    let current = settings.current();
    if client_id.is_some()
        && current.server.advertise
        && current.advertised(derived_url.as_deref()).is_none()
    {
        tracing::warn!(target: "patchula_server", "the App's webhook URL does not end in /webhook; set publicUrl to advertise this server");
    }
    let runner = Arc::new(Engines {
        app: app.clone(),
        transport: transport.clone(),
        cache: cache.clone(),
        engines: Mutex::new(BTreeMap::new()),
        recheck: options.recheck,
        bot_login: bot.clone(),
        derived_url,
        signs_in: client_id.is_some(),
    });
    let webhook = Webhook::new(
        cache.clone(),
        settings.clone(),
        bot.clone(),
        options.webhook_secret,
    )
    .with_app(app.clone());
    let api = crate::remote::Api::new(crate::remote::Parts {
        app: app.clone(),
        client_id,
        bot: bot.clone(),
        transport,
        cache: cache.clone(),
        settings: settings.clone(),
        accepting: webhook.accepting.clone(),
        runner: runner.clone(),
    });
    let webhook = webhook.with_api(api);
    let ready = Arc::new(AtomicBool::new(false));
    let listener = TcpListener::bind(options.listen)
        .await
        .map_err(|_| Error("cannot bind server listener"))?;
    tracing::info!(target: "patchula_server", address = %listener.local_addr().map_err(|_| Error("cannot inspect listener"))?, "server listening");
    let (stop, stopped) = watch::channel(false);
    let reload_settings = settings.clone();
    let settings_path = options.settings;
    let mut reload_stop = stopped.clone();
    let reload = tokio::spawn(async move {
        loop {
            if *reload_stop.borrow() {
                break;
            }
            let settings = reload_settings.clone();
            let path = settings_path.clone();
            let _ = tokio::task::spawn_blocking(move || settings.refresh(&path)).await;
            tokio::select! { _ = tokio::time::sleep(Duration::from_secs(1)) => {}, _ = reload_stop.changed() => {} }
        }
    });
    let maintenance = tokio::spawn(maintenance(
        app,
        cache,
        settings,
        ready.clone(),
        stopped.clone(),
    ));
    let result = run_until(
        listener,
        webhook,
        ready,
        runner,
        signal(),
        options.shutdown_timeout,
        stop,
        stopped,
    )
    .await;
    maintenance.abort();
    reload.abort();
    let _ = maintenance.await;
    let _ = reload.await;
    // Closing the file would release it too; this says so.
    let _ = lock.unlock();
    result
}

fn router(webhook: Webhook, ready: Arc<AtomicBool>) -> Router {
    let api = webhook.api.clone();
    let settings = webhook.settings.clone();
    let accepting = webhook.accepting.clone();
    let cache = webhook.cache.clone();
    let (public_cache, public_settings) = (webhook.cache.clone(), webhook.settings.clone());
    let router = Router::new()
        .route("/webhook", post(crate::webhook::receive))
        .route("/healthz", get(|| async { StatusCode::OK }))
        .route(
            "/readyz",
            get(move || {
                let (settings, accepting, cache, ready) = (
                    settings.clone(),
                    accepting.clone(),
                    cache.clone(),
                    ready.clone(),
                );
                // Ready is every condition at once. Webhooks and remote runs
                // check only `accepting`: while unready they are still queued,
                // since a rejected delivery would be lost.
                async move {
                    if accepting.load(Ordering::Acquire)
                        && settings.valid()
                        && ready.load(Ordering::Acquire)
                        && cache.queue_len().await.is_ok()
                    {
                        StatusCode::OK
                    } else {
                        StatusCode::SERVICE_UNAVAILABLE
                    }
                }
            }),
        )
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .layer(axum::middleware::from_fn(crate::webhook::observe))
        .with_state(webhook);
    // The APIs answer in their own JSON, outside the webhook observer.
    let router = router.merge(crate::public::routes(crate::public::Public {
        cache: public_cache,
        settings: public_settings,
    }));
    match api {
        Some(api) => router.merge(crate::remote::routes(api)),
        None => router,
    }
}

#[derive(Default, Debug)]
struct BatchOutcome {
    error_kinds: BTreeMap<&'static str, usize>,
    /// Repositories, lowercased, whose run failed and retries after a backoff.
    failed: BTreeSet<String>,
    /// Repositories, lowercased, to run again because their default branch
    /// moved while the run was writing.
    requeued: BTreeSet<String>,
    /// The batch failed before its repositories had results; every claim retries.
    whole: bool,
}
impl BatchOutcome {
    fn report(report: &Report) -> Self {
        use patchula_report::BranchStatus;
        let mut outcome = Self::default();
        for repo in &report.repositories {
            if repo.status == Status::Error {
                outcome.fail("repository-error", &repo.repository);
            }
        }
        if let Some(actions) = &report.actions {
            for branch in &actions.branches {
                if patchula_update::needs_requeue(branch) {
                    outcome
                        .requeued
                        .insert(branch.repository.to_ascii_lowercase());
                }
                let kind = match branch.status {
                    BranchStatus::Error => "branch-error",
                    BranchStatus::Protected => "branch-protected",
                    _ => continue,
                };
                outcome.fail(kind, &branch.repository);
            }
            for pr in &actions.pull_requests {
                let kind = match pr.status {
                    patchula_report::PullRequestStatus::Error => "pull-request-error",
                    patchula_report::PullRequestStatus::Protected => "pull-request-protected",
                    _ => continue,
                };
                outcome.fail(kind, &pr.repository);
            }
        }
        outcome
    }
    /// Every claim of the batch retries.
    fn whole(kind: &'static str) -> Self {
        Self {
            error_kinds: BTreeMap::from([(kind, 1)]),
            whole: true,
            ..Self::default()
        }
    }
    fn fail(&mut self, kind: &'static str, repo: &str) {
        *self.error_kinds.entry(kind).or_default() += 1;
        self.failed.insert(repo.to_ascii_lowercase());
    }
    fn failed(&self) -> bool {
        !self.error_kinds.is_empty()
    }
    /// How a claimed repository's run ended. Discovery and repositories the
    /// batch skipped are done.
    fn finish(&self, work: &Work) -> Finish {
        let repo = work.repo.to_ascii_lowercase();
        if self.whole || self.failed.contains(&repo) {
            Finish::Failed
        } else if self.requeued.contains(&repo) {
            Finish::Requeue
        } else {
            Finish::Done
        }
    }
}

trait Batch: Send + Sync + 'static {
    fn run<'a>(
        &'a self,
        work: &'a [Work],
        settings: &'a Settings,
        attribution: &'a Attribution,
    ) -> BoxFuture<'a, Result<BatchOutcome, Error>>;
}
/// What an installation engine was built from: the `http` settings and its
/// registry credentials.
type EngineKey = (
    patchula_config::settings::Http,
    Vec<patchula_http::Credential>,
);
/// An installation's engine and the settings it was built from, once built.
type Slot = Arc<Mutex<Option<(EngineKey, Engine)>>>;

struct Engines {
    app: Arc<App>,
    transport: Arc<dyn Transport>,
    cache: Cache,
    /// Each installation's engine, behind its own lock: batches of different
    /// installations run at once, and a remote run waits only for its own.
    engines: Mutex<BTreeMap<u64, Slot>>,
    recheck: Option<patchula_words::Recheck>,
    bot_login: String,
    /// The server URL from the App's webhook URL, for `advertise`.
    derived_url: Option<String>,
    /// Whether the CLI can sign in here; nothing is advertised otherwise.
    signs_in: bool,
}
impl Batch for Engines {
    fn run<'a>(
        &'a self,
        work: &'a [Work],
        settings: &'a Settings,
        attribution: &'a Attribution,
    ) -> BoxFuture<'a, Result<BatchOutcome, Error>> {
        Box::pin(async move {
            let id = work.first().ok_or(Error("empty batch"))?.installation;
            if !settings.installation_allowed(id) {
                skip_report("installation not allowlisted", false);
                return Ok(BatchOutcome::default());
            }
            // One client serves discovery, check candidates and the public
            // record: every batch past the allowlist check needs it anyway.
            let client = Client::with_credentials(
                self.app.installation(id),
                self.cache.clone(),
                patchula_engine::settings::github_options(settings),
                self.transport.clone(),
            )
            .map_err(|_| Error("cannot create installation client"))?;
            if work.iter().any(|w| w.repo.is_empty()) {
                let mut repos = Vec::new();
                let mut listed = Vec::new();
                for page in 1..=1000 {
                    let value = client
                        .get(&format!(
                            "installation/repositories?per_page=100&page={page}"
                        ))
                        .await
                        .map_err(|_| Error("installation discovery failed"))?;
                    let value: serde_json::Value = serde_json::from_slice(&value.body)
                        .map_err(|_| Error("invalid installation repository JSON"))?;
                    let entries = value["repositories"]
                        .as_array()
                        .ok_or(Error("invalid installation repositories"))?;
                    for entry in entries {
                        let repo = entry["full_name"]
                            .as_str()
                            .ok_or(Error("invalid installation repository"))?;
                        if settings.allows(id, repo) {
                            let repo = repo.to_ascii_lowercase();
                            // Unknown visibility counts as private, which the public API hides.
                            let private = entry["private"].as_bool().unwrap_or(true);
                            listed.push((repo.clone(), private));
                            repos.push(Work::repository(id, repo));
                        }
                    }
                    if entries.len() < 100 {
                        break;
                    }
                    if page == 1000 {
                        return Err(Error("installation repository page budget exceeded"));
                    }
                }
                self.cache
                    .enqueue_attributed(
                        None,
                        repos,
                        Utc::now().timestamp_millis(),
                        settings.server.coalescing_ms as i64,
                        attribution.clone(),
                    )
                    .await
                    .map_err(|_| Error("cannot persist discovered repositories"))?;
                self.cache
                    .repositories_listed(id, listed)
                    .await
                    .map_err(|_| Error("cannot record discovered repositories"))?;
                tracing::info!(target: "patchula_server", installation = id,
                    "discovery completed; repository work queued for a separate batch");
            }
            let mut repositories = Vec::new();
            for item in work
                .iter()
                .filter(|w| !w.repo.is_empty() && settings.allows(id, &w.repo))
            {
                if check_candidate(&client, item, &self.bot_login).await? {
                    repositories.push(item.repo.clone());
                }
            }
            // Empty repositories would mean discovery to the engine. Never pass that
            // through after filtering a queue batch: it could escape the allowlist.
            if repositories.is_empty() {
                skip_report(
                    if work.iter().all(|w| w.repo.is_empty()) {
                        "discovery-only batch; no repository runs"
                    } else {
                        "no eligible repository runs"
                    },
                    work.iter().all(|w| w.repo.is_empty()),
                );
                return Ok(BatchOutcome::default());
            }
            let started = Utc::now();
            let (result, withheld) = {
                let mut slot = self.engine(id, settings).await?;
                let outcome = std::panic::AssertUnwindSafe(async {
                    let (_, engine) = slot.as_mut().ok_or(Error("missing batch engine"))?;
                    let result = engine
                        .run(
                            patchula_engine::Options {
                                repositories: repositories.clone(),
                                ..patchula_engine::Options::default()
                            },
                            false,
                            false,
                        )
                        .await;
                    let withheld = result.as_ref().ok().map(crate::public::withheld);
                    Ok::<_, Error>((result, withheld))
                })
                .catch_unwind()
                .await;
                discard_on_panic(&mut slot, outcome)?
            };
            if let (Ok(report), Some(withheld)) = (&result, withheld) {
                crate::public::record(
                    &self.cache,
                    &client,
                    &self.bot_login,
                    id,
                    &crate::public::Recorded { report, withheld },
                    started,
                    &self.member(id),
                )
                .await;
            }
            let report = match &result {
                Ok(report) => report.clone(),
                Err(_) => Report {
                    schema_version: SCHEMA_VERSION,
                    repositories: repositories
                        .iter()
                        .map(|repo| Repository {
                            repository: repo.clone(),
                            status: Status::Error,
                            reason: Some("batch engine failed; work will be retried".into()),
                            package_files: Vec::new(),
                            warnings: Vec::new(),
                            skipped: Vec::new(),
                            notices: Vec::new(),
                            lookup_failures: Vec::new(),
                            tooling: Vec::new(),
                            default_branch: None,
                            commit: None,
                        })
                        .collect(),
                    stats: None,
                    actions: None,
                },
            };
            output_report(&report, &settings.server).await?;
            let mut outcome = BatchOutcome::report(&report);
            if result.is_err() {
                outcome.error_kinds.insert("engine-error", 1);
            }
            Ok(outcome)
        })
    }
}
impl Engines {
    /// Asks GitHub whether a repository is in this App's installation `id`.
    fn member(&self, id: u64) -> impl Fn(String) -> BoxFuture<'static, bool> + Send + Sync + use<> {
        let app = self.app.clone();
        move |repo| {
            let app = app.clone();
            Box::pin(
                async move { app.repository_installation(&repo).await.ok().flatten() == Some(id) },
            )
        }
    }
    /// The installation's engine, created on first use and set to the current
    /// settings. Registry secrets are read again for every batch, so a rotated
    /// secret applies without a restart. A change to the `http` settings or to
    /// the installation's credentials replaces the engine, since its clients
    /// are built from them.
    async fn engine(
        &self,
        id: u64,
        settings: &Settings,
    ) -> Result<tokio::sync::OwnedMutexGuard<Option<(EngineKey, Engine)>>, Error> {
        let (credentials, failed) = patchula_engine::settings::credentials(settings, Some(id));
        for (prefix, reason) in &failed {
            tracing::error!(target: "patchula_server", installation = id, registry = %prefix, reason = %reason,
                "registry credentials unavailable; its lookups run without them");
        }
        let key = (settings.http.clone(), credentials);
        let slot = self.engines.lock().await.entry(id).or_default().clone();
        let mut slot = slot.lock_owned().await;
        if slot.as_ref().is_none_or(|(built, _)| *built != key) {
            let engine = Engine::from_settings(
                settings,
                key.1.clone(),
                patchula_engine::Auth::Credentials(self.app.installation(id)),
                settings.concurrency,
                settings.recheck(self.recheck),
                self.cache.clone(),
                self.transport.clone(),
            )
            .map_err(|_| Error("cannot create batch engine"))?;
            *slot = Some((key, engine));
        }
        let (_, engine) = slot.as_mut().ok_or(Error("missing batch engine"))?;
        engine
            .set_batch_options(settings.concurrency, settings.recheck(self.recheck))
            .map_err(|_| Error("invalid batch options"))?;
        engine.set_lockfile_cache_limit(settings.lockfiles.cache_gib);
        engine.set_advertised_server(
            settings
                .advertised(self.derived_url.as_deref())
                .filter(|_| self.signs_in),
        );
        Ok(slot)
    }
}
/// The installation's engine slot while a remote run holds it. A run ends
/// three ways: it returns, it panics, or its future is dropped — the remote
/// time limit cancels a hung run mid-await. Only a returned run leaves the
/// engine in place: a panic or a dropped run may have left half-done state,
/// so the slot is emptied however the guard drops.
struct SlotGuard {
    slot: tokio::sync::OwnedMutexGuard<Option<(EngineKey, Engine)>>,
    armed: bool,
}
impl SlotGuard {
    fn engine(&mut self) -> Result<&mut Engine, &'static str> {
        self.slot
            .as_mut()
            .map(|(_, engine)| engine)
            .ok_or("missing batch engine")
    }
    /// The run returned; the engine it used is whole, so keep it.
    fn finish(mut self) {
        self.armed = false;
    }
}
impl Drop for SlotGuard {
    fn drop(&mut self) {
        if self.armed {
            *self.slot = None;
        }
    }
}
impl crate::remote::Runner for Engines {
    fn run<'a>(
        &'a self,
        installation: u64,
        repositories: Vec<String>,
        mode: crate::remote::Mode,
        login: &'a str,
        settings: &'a Settings,
    ) -> BoxFuture<'a, Result<Report, &'static str>> {
        use crate::remote::Mode;
        Box::pin(async move {
            let started = Utc::now();
            let (report, withheld) = {
                // The installation's own lock, so a remote run never races its
                // queued batches.
                let slot = self.engine(installation, settings).await.map_err(|e| e.0)?;
                let mut slot = SlotGuard { slot, armed: true };
                let options = patchula_engine::Options {
                    repositories,
                    ..patchula_engine::Options::default()
                };
                let result = match mode {
                    Mode::Lookup => slot.engine()?.lookup(options).await,
                    Mode::DryRun => slot.engine()?.run(options, true, false).await,
                    Mode::Run => slot.engine()?.run(options, false, false).await,
                }
                .map_err(|_| "the run failed on the server");
                slot.finish();
                let report = result?;
                let withheld = crate::public::withheld(&report);
                (report, withheld)
            };
            if mode == Mode::Run {
                if let Ok(client) = Client::with_credentials(
                    self.app.installation(installation),
                    self.cache.clone(),
                    patchula_engine::settings::github_options(settings),
                    self.transport.clone(),
                ) {
                    crate::public::record(
                        &self.cache,
                        &client,
                        &self.bot_login,
                        installation,
                        &crate::public::Recorded {
                            report: &report,
                            withheld,
                        },
                        started,
                        &self.member(installation),
                    )
                    .await;
                }
                tracing::info!(target: "patchula_server", installation, user = login, "remote run report");
                output_report(&report, &settings.server)
                    .await
                    .map_err(|e| e.0)?;
            }
            Ok(report)
        })
    }
}
/// The batch report file grammar, `batch-<ts>-<pid>-<counter>.json`, in one
/// place: `name` builds it for a write and `report` recognises it for
/// pruning. Anything else in the directory belongs to someone else and is
/// never counted or deleted.
mod report_name {
    use chrono::{DateTime, Utc};

    /// `batch-YYYYMMDDTHHMMSS.NNNNNNNNNZ-<pid>-<counter>.json`.
    pub fn name(at: DateTime<Utc>, pid: u32, counter: u64) -> String {
        format!(
            "batch-{}-{}-{}.json",
            at.format("%Y%m%dT%H%M%S%.9fZ"),
            pid,
            counter
        )
    }

    /// Whether `name` is a batch report exactly as [`name`] writes one.
    pub fn report(name: &str) -> bool {
        let digits = |text: &str, len: Option<usize>| {
            !text.is_empty()
                && len.is_none_or(|len| text.len() == len)
                && text.bytes().all(|b| b.is_ascii_digit())
        };
        let Some(rest) = name
            .strip_prefix("batch-")
            .and_then(|rest| rest.strip_suffix(".json"))
        else {
            return false;
        };
        let Some((time, rest)) = rest.split_once("Z-") else {
            return false;
        };
        let (Some((date, clock)), Some((pid, counter))) =
            (time.split_once('T'), rest.split_once('-'))
        else {
            return false;
        };
        let Some((seconds, fraction)) = clock.split_once('.') else {
            return false;
        };
        digits(date, Some(8))
            && digits(seconds, Some(6))
            && digits(fraction, Some(9))
            && digits(pid, None)
            && digits(counter, None)
    }
}
/// How old a report's temporary file must be before it counts as abandoned by
/// a writer that crashed, rather than one still being written.
const ABANDONED_REPORT: std::time::Duration = std::time::Duration::from_secs(24 * 3600);
/// Deletes the oldest batch reports beyond `kept`, and temporary files that
/// crashed writers left behind. Names start with a fixed-width UTC time, so
/// their order is their age, as long as the clock does not step backwards.
fn prune_reports(directory: &std::path::Path, kept: usize) {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) => {
            tracing::warn!(target: "patchula_server", directory = %directory.display(), %error, "cannot list the report directory; old reports are not deleted");
            return;
        }
    };
    let mut reports = Vec::new();
    let mut abandoned = Vec::new();
    for entry in entries.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if report_name::report(&name) {
            reports.push(name);
        } else if name
            .strip_prefix('.')
            .and_then(|name| name.strip_suffix(".tmp"))
            .is_some_and(report_name::report)
            && entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|age| age > ABANDONED_REPORT)
        {
            abandoned.push(name);
        }
    }
    reports.sort_unstable();
    let excess = reports.len().saturating_sub(kept);
    let mut removed = 0;
    for name in reports[..excess].iter().chain(&abandoned) {
        match std::fs::remove_file(directory.join(name)) {
            Ok(()) => removed += 1,
            // Another worker deleted it first.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(target: "patchula_server", report = %name, %error, "cannot delete an old batch report");
            }
        }
    }
    if removed > 0 {
        tracing::info!(target: "patchula_server", removed, kept, "old batch reports deleted");
    }
}
fn skip_report(reason: &str, discovery_only: bool) {
    tracing::info!(target: "patchula_server", reason, discovery_only, repository_runs = 0, "batch report skipped");
}
async fn output_report(
    report: &Report,
    server: &patchula_config::settings::Server,
) -> Result<(), Error> {
    if let Some(directory) = &server.report_directory {
        let report = report.clone();
        let directory = directory.to_owned();
        let kept = server.reports_kept;
        tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&directory)
                .map_err(|_| Error("cannot create report directory"))?;
            // Batches finish side by side, so the counter keeps two in the
            // same instant apart.
            static REPORTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let name = report_name::name(
                Utc::now(),
                std::process::id(),
                REPORTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            );
            let temporary = directory.join(format!(".{name}.tmp"));
            let file = std::fs::File::options()
                .write(true)
                .create_new(true)
                .open(&temporary)
                .map_err(|_| Error("cannot create batch report"))?;
            serde_json::to_writer(&file, &report)
                .map_err(|_| Error("cannot write batch report"))?;
            file.sync_all()
                .map_err(|_| Error("cannot sync batch report"))?;
            let path = directory.join(name);
            std::fs::rename(temporary, &path)
                .map_err(|_| Error("cannot publish batch report"))?;
            tracing::info!(target: "patchula_server", path = %path.display(), repository_runs = report.repositories.len(), "batch report written");
            prune_reports(&directory, kept);
            Ok::<(), Error>(())
        })
        .await
        .map_err(|_| Error("report writer failed"))??;
    } else {
        let json =
            serde_json::to_string(report).map_err(|_| Error("cannot encode batch report"))?;
        tracing::info!(target: "patchula_server", report = %json, "batch report");
    }
    Ok(())
}

/// `server.workers` queue workers, each claiming one installation's batch at
/// a time. A stop, or the first worker to fail, halts the others once their
/// current batches finish, and the first failure is returned.
async fn workers(
    cache: Cache,
    settings: Reload,
    runner: Arc<dyn Batch>,
    mut stop: watch::Receiver<bool>,
) -> Result<(), Error> {
    let (halt, halted) = watch::channel(*stop.borrow());
    let mut running = tokio::task::JoinSet::new();
    for _ in 0..settings.current().server.workers {
        running.spawn(worker(
            cache.clone(),
            settings.clone(),
            runner.clone(),
            halted.clone(),
        ));
    }
    let mut failure = None;
    let mut listening = true;
    loop {
        tokio::select! {
            joined = running.join_next() => {
                let Some(joined) = joined else { break };
                let result = joined
                    .map_err(|_| Error("queue worker stopped unexpectedly"))
                    .and_then(|result| result);
                if let Err(error) = result
                    && failure.is_none()
                {
                    failure = Some(error);
                    let _ = halt.send(true);
                }
            }
            changed = stop.changed(), if listening => {
                if changed.is_err() || *stop.borrow() {
                    let _ = halt.send(true);
                    listening = false;
                }
            }
        }
    }
    failure.map_or(Ok(()), Err)
}

/// Repository names a batch log line lists at most.
const LOGGED_REPOSITORIES: usize = 20;

async fn worker(
    cache: Cache,
    settings: Reload,
    runner: Arc<dyn Batch>,
    mut stop: watch::Receiver<bool>,
) -> Result<(), Error> {
    loop {
        if *stop.borrow() {
            return Ok(());
        }
        let settings = settings.current();
        let claims = cache
            .claim_batch(Utc::now().timestamp_millis(), settings.server.batch_size)
            .await
            .map_err(|_| Error("queue claim failed"))?;
        if claims.is_empty() {
            tokio::select! { _ = tokio::time::sleep(Duration::from_millis(100)) => {}, _ = stop.changed() => {} }
            continue;
        }
        let work: Vec<_> = claims.iter().map(|c| c.work.clone()).collect();
        let mut attribution = Attribution::default();
        for claim in &claims {
            attribution.merge(&claim.attribution);
        }
        tracing::info!(target: "patchula_server", installation = work[0].installation, entries = work.len(), reasons = ?attribution.reasons, delivery_ids = ?attribution.deliveries, "batch started");
        let result = runner.run(&work, &settings, &attribution).await;
        let (outcome, error) = match result {
            Ok(outcome) => (outcome, None),
            Err(error) => (BatchOutcome::whole("batch-operation"), Some(error)),
        };
        let finished: Vec<_> = claims
            .into_iter()
            .map(|claim| {
                let finish = outcome.finish(&claim.work);
                (claim, finish)
            })
            .collect();
        let named = |wanted: Finish| -> Vec<&str> {
            finished
                .iter()
                .filter(|(claim, finish)| *finish == wanted && !claim.work.repo.is_empty())
                .map(|(claim, _)| claim.work.repo.as_str())
                .collect()
        };
        let (retried, requeued) = (named(Finish::Failed), named(Finish::Requeue));
        // Names of a large batch are capped; `batch finished` has the counts.
        let listed = |names: &[&str]| -> Vec<String> {
            let mut listed: Vec<_> = names
                .iter()
                .take(LOGGED_REPOSITORIES)
                .map(|n| (*n).to_owned())
                .collect();
            if names.len() > LOGGED_REPOSITORIES {
                listed.push(format!("… {} more", names.len() - LOGGED_REPOSITORIES));
            }
            listed
        };
        if outcome.failed() {
            tracing::error!(target: "patchula_server", error = error.as_ref().map(|e| e.0),
                error_kinds = ?outcome.error_kinds, error_count = outcome.error_kinds.values().sum::<usize>(),
                retried = ?listed(&retried), "batch failed; failed repositories will be retried with backoff");
        }
        if !requeued.is_empty() {
            tracing::warn!(target: "patchula_server", reason = "default branch moved; requeued",
                installation = work[0].installation, requeued = ?listed(&requeued), "batch requeued");
        }
        tracing::info!(target: "patchula_server", installation = work[0].installation, entries = work.len(),
            success = !outcome.failed(), retried = retried.len(), requeued = requeued.len(), "batch finished");
        cache
            .finish_work(
                finished,
                Utc::now().timestamp_millis(),
                settings.server.coalescing_ms as i64,
            )
            .await
            .map_err(|_| Error("queue completion failed"))?;
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_until(
    listener: TcpListener,
    webhook: Webhook,
    ready: Arc<AtomicBool>,
    runner: Arc<dyn Batch>,
    shutdown: impl Future<Output = ()>,
    timeout: Duration,
    stop: watch::Sender<bool>,
    stopped: watch::Receiver<bool>,
) -> Result<(), Error> {
    let cache = webhook.cache.clone();
    let mut worker = tokio::spawn(workers(
        cache.clone(),
        webhook.settings.clone(),
        runner,
        stopped,
    ));
    let accepting = webhook.accepting.clone();
    let api = webhook.api.clone();
    let (http_stop, http_stopped) = tokio::sync::oneshot::channel();
    let mut server = tokio::spawn(async move {
        axum::serve(listener, router(webhook, ready))
            .with_graceful_shutdown(async {
                let _ = http_stopped.await;
            })
            .await
    });
    let mut failure = None;
    tokio::select! {
        _ = shutdown => {},
        result = &mut worker => { failure = Some(result.ok().and_then(Result::err).unwrap_or(Error("queue worker stopped unexpectedly"))); },
        _ = &mut server => { failure = Some(Error("HTTP server stopped unexpectedly")); },
    }
    accepting.store(false, Ordering::Release);
    let _ = stop.send(true);
    let _ = http_stop.send(());
    tracing::info!(target: "patchula_server", "shutdown started; finishing current batch");
    let drained = tokio::time::timeout(timeout, async {
        if !worker.is_finished() {
            let _ = (&mut worker).await;
        }
        if !server.is_finished() {
            let _ = (&mut server).await;
        }
        // Remote runs write branches too; let them finish like a batch.
        if let Some(api) = &api {
            api.drain().await;
        }
    })
    .await
    .is_ok();
    worker.abort();
    server.abort();
    if !drained {
        tracing::warn!(target: "patchula_server", "shutdown deadline reached; unfinished work remains queued");
    }
    // Aborting is cancellation-safe: claimed rows are never removed on claim.
    // Wait for the worker to release engine/DB handles before resetting claims.
    if !worker.is_finished() {
        let _ = worker.await;
    }
    // A clean shutdown releases its runs; one on a failure, such as a
    // panicking batch, counts them as failed so a run that takes the server
    // down backs off instead of looping at restart rate.
    if failure.is_some() {
        cache.recover_work(Utc::now().timestamp_millis()).await
    } else {
        cache.release_work().await
    }
    .map_err(|_| Error("cannot requeue interrupted batch"))?;
    failure.map_or(Ok(()), Err)
}

async fn maintenance(
    app: Arc<App>,
    cache: Cache,
    settings: Reload,
    ready: Arc<AtomicBool>,
    mut stop: watch::Receiver<bool>,
) {
    let mut probe_at = tokio::time::Instant::now();
    // The first sweep is the next one the schedule names, never one at startup:
    // queued work survives restarts, and the readiness probe below checks the
    // installations straight away.
    let mut next_sweep = sweep_after(&settings.current(), Utc::now());
    let mut schedule = (
        settings.current().server.sweep_cron,
        settings.current().server.sweep_timezone,
    );
    loop {
        if *stop.borrow() {
            break;
        }
        let current = settings.current();
        let new_schedule = (
            current.server.sweep_cron.clone(),
            current.server.sweep_timezone.clone(),
        );
        if new_schedule != schedule {
            next_sweep = sweep_after(&current, Utc::now());
            schedule = new_schedule;
        }
        let sweep_due = next_sweep.is_some_and(|next| next <= Utc::now());
        if sweep_due || tokio::time::Instant::now() >= probe_at {
            // A sweep is done once it is queued; a token probe failing after
            // that marks the server unready but does not queue it again.
            let mut swept = false;
            let result = async {
                app.bot_login()
                    .await
                    .map_err(|_| Error("App authentication probe failed"))?;
                let installations = app
                    .installations()
                    .await
                    .map_err(|_| Error("installation listing failed"))?;
                let ids: Vec<_> = installations
                    .into_iter()
                    .filter(|id| current.installation_allowed(*id))
                    .collect();
                // Repositories of removed or disallowed installations leave the public API.
                cache
                    .repositories_retain(ids.clone())
                    .await
                    .map_err(|_| Error("cannot prune recorded repositories"))?;
                if ids.is_empty() {
                    return Err(Error(
                        "no allowed App installations available for token probe",
                    ));
                }
                if sweep_due {
                    cache
                        .enqueue_attributed(
                            None,
                            ids.iter()
                                .copied()
                                .map(|id| Work::repository(id, String::default()))
                                .collect(),
                            Utc::now().timestamp_millis(),
                            current.server.coalescing_ms as i64,
                            Attribution::reason("sweep"),
                        )
                        .await
                        .map_err(|_| Error("cannot persist cron sweep"))?;
                    swept = true;
                }
                for id in &ids {
                    app.installation(*id)
                        .token()
                        .await
                        .map_err(|_| Error("installation token probe failed"))?;
                }
                Ok(())
            }
            .await;
            let success = result.is_ok();
            ready.store(success, Ordering::Release);
            if let Err(error) = result {
                tracing::error!(target: "patchula_server", %error, "readiness/sweep probe failed");
            }
            probe_at = tokio::time::Instant::now() + Duration::from_secs(30);
            if sweep_due {
                next_sweep = if swept {
                    sweep_after(&current, Utc::now())
                } else {
                    Some(Utc::now() + chrono::Duration::seconds(60))
                };
            }
        }
        tokio::select! { _ = tokio::time::sleep(Duration::from_secs(1)) => {}, _ = stop.changed() => {} }
    }
}
async fn check_candidate(client: &Client, work: &Work, bot: &str) -> Result<bool, Error> {
    let Some(numbers) = &work.check_prs else {
        return Ok(true);
    };
    for number in numbers {
        let pr = match client
            .get_fresh(&format!("repos/{}/pulls/{number}", work.repo))
            .await
        {
            Ok(pr) => pr,
            Err(patchula_github::Error::Status(404)) => continue,
            Err(_) => return Err(Error("check-event PR ownership lookup failed")),
        };
        if patchula_update::pull_requests::owned_pr(&pr, bot, &work.repo) {
            return Ok(true);
        }
    }
    Ok(false)
}
fn sweep_after(settings: &Settings, now: chrono::DateTime<Utc>) -> Option<chrono::DateTime<Utc>> {
    let schedule = patchula_config::schedule(settings.server.sweep_cron.as_deref()?, true).ok()?;
    let tz = chrono_tz::Tz::from_str(&settings.server.sweep_timezone).ok()?;
    schedule
        .after(&now.with_timezone(&tz))
        .next()
        .map(|next| next.with_timezone(&Utc))
}
async fn signal() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM handler");
        tokio::select! { _ = term.recv() => {}, _ = tokio::signal::ctrl_c() => {} }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
mod tests;

/// The outcome of work on a slot's engine. An engine that panicked mid-run
/// may hold half-done state, so its slot is emptied, the next run builds a
/// fresh one, and the panic carries on to whoever recovers it.
fn discard_on_panic<T>(
    slot: &mut tokio::sync::OwnedMutexGuard<Option<(EngineKey, Engine)>>,
    outcome: std::thread::Result<T>,
) -> T {
    outcome.unwrap_or_else(|panic| {
        **slot = None;
        std::panic::resume_unwind(panic)
    })
}
