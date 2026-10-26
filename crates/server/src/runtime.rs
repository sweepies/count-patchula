use crate::{
    settings::{Reload, Settings},
    webhook::{BODY_LIMIT, Webhook},
};
use axum::{
    Router,
    extract::DefaultBodyLimit,
    http::StatusCode,
    routing::{get, post},
};
use chrono::Utc;
use futures_util::future::BoxFuture;
use patchula_cache::{
    Cache,
    queue::{Attribution, Work},
};
use patchula_engine::Engine;
use patchula_github::{Client, app::App};
use patchula_http::Transport;
use patchula_report::{Report, Repository, SCHEMA_VERSION, Status};
use std::{
    collections::BTreeMap,
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

/// Docker Hub credentials for authenticated pulls, as in the CLI.
const DOCKERHUB_USERNAME_VAR: &str = "PATCHULA_DOCKERHUB_USERNAME";
const DOCKERHUB_TOKEN_VAR: &str = "PATCHULA_DOCKERHUB_TOKEN";

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
    pub recheck: Option<String>,
    pub shutdown_timeout: Duration,
}

pub async fn serve(options: Options) -> miette::Result<()> {
    let settings = Reload::new(Settings::load(&options.settings)?);
    serve_with_settings(options, settings)
        .await
        .map_err(miette::Report::msg)
}

async fn serve_with_settings(options: Options, settings: Reload) -> Result<(), Error> {
    if options.webhook_secret.is_empty() {
        return Err(Error("webhook secret must be nonempty"));
    }
    if options
        .recheck
        .as_deref()
        .is_some_and(|r| !matches!(r, "now" | "next-run"))
    {
        return Err(Error("recheck must be now or next-run"));
    }
    if let Some(parent) = options.cache.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|_| Error("cannot create cache directory"))?;
    }
    // One queue owner per SQLite database. The OS releases this advisory lock on crash.
    let lock_path = options.cache.with_extension("server.lock");
    let lock = std::fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock_path)
        .map_err(|_| Error("cannot open server lock"))?;
    lock.try_lock()
        .map_err(|_| Error("another server owns this cache"))?;
    let cache = Cache::open(&options.cache, Default::default())
        .await
        .map_err(|_| Error("cannot open server cache"))?;
    cache
        .recover_work()
        .await
        .map_err(|_| Error("cannot recover pending work"))?;
    let transport: Arc<dyn Transport> =
        Arc::new(patchula_http::Network::new().map_err(|_| Error("cannot create HTTP transport"))?);
    let app = App::new(options.app_id, &options.private_key, transport.clone())
        .map_err(|_| Error("invalid GitHub App credentials"))?;
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
            .and_then(|url| crate::settings::from_webhook(&url)),
        None => None,
    };
    let current = settings.current();
    if client_id.is_some()
        && current.advertise
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
    requeue: bool,
}
impl BatchOutcome {
    fn report(report: &Report) -> Self {
        use patchula_report::ActionStatus;
        let mut outcome = Self::default();
        for repo in &report.repositories {
            if repo.status == Status::Error {
                *outcome.error_kinds.entry("repository-error").or_default() += 1;
            }
        }
        if let Some(actions) = &report.actions {
            for branch in &actions.branches {
                outcome.requeue |= patchula_update::needs_requeue(branch);
                let kind = match branch.status {
                    ActionStatus::Error => "branch-error",
                    ActionStatus::Protected => "branch-protected",
                    _ => continue,
                };
                *outcome.error_kinds.entry(kind).or_default() += 1;
            }
            for pr in &actions.pull_requests {
                let kind = match pr.status {
                    ActionStatus::Error => "pull-request-error",
                    ActionStatus::Protected => "pull-request-protected",
                    _ => continue,
                };
                *outcome.error_kinds.entry(kind).or_default() += 1;
            }
        }
        outcome
    }
    fn failed(&self) -> bool {
        !self.error_kinds.is_empty()
    }
    fn retry(&self) -> bool {
        self.requeue || self.failed()
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
struct Engines {
    app: Arc<App>,
    transport: Arc<dyn Transport>,
    cache: Cache,
    engines: Mutex<BTreeMap<u64, Engine>>,
    recheck: Option<String>,
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
            let credentials = self.app.installation(id);
            if work.iter().any(|w| w.repo.is_empty()) {
                let client = Client::with_credentials(
                    credentials.clone(),
                    self.cache.clone(),
                    patchula_github::Options::default(),
                    self.transport.clone(),
                )
                .map_err(|_| Error("cannot create installation client"))?;
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
                        settings.coalescing_ms as i64,
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
            let client = Client::with_credentials(
                credentials.clone(),
                self.cache.clone(),
                patchula_github::Options::default(),
                self.transport.clone(),
            )
            .map_err(|_| Error("cannot create check-event client"))?;
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
            let result = {
                let mut engines = self.engines.lock().await;
                let engine = self.engine(&mut engines, id, settings)?;
                engine
                    .run(
                        patchula_engine::Options {
                            repositories: repositories.clone(),
                            ..patchula_engine::Options::default()
                        },
                        false,
                        false,
                    )
                    .await
            };
            if let Ok(report) = &result {
                crate::public::record(
                    &self.cache,
                    &client,
                    &self.bot_login,
                    id,
                    report,
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
                            default_branch: None,
                            commit: None,
                        })
                        .collect(),
                    stats: None,
                    actions: None,
                },
            };
            output_report(&report, settings.report_directory.as_deref()).await?;
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
    /// The installation's engine, created on first use and set to the current settings.
    fn engine<'a>(
        &self,
        engines: &'a mut BTreeMap<u64, Engine>,
        id: u64,
        settings: &Settings,
    ) -> Result<&'a mut Engine, Error> {
        if let std::collections::btree_map::Entry::Vacant(entry) = engines.entry(id) {
            let mut engine = Engine::with_credentials(
                self.app.installation(id),
                self.cache.clone(),
                settings.concurrency,
                self.transport.clone(),
            )
            .map_err(|_| Error("cannot create batch engine"))?
            .with_advisories();
            match (
                std::env::var(DOCKERHUB_USERNAME_VAR).ok(),
                std::env::var(DOCKERHUB_TOKEN_VAR).ok(),
            ) {
                (None, None) => {}
                (Some(user), Some(token)) if !user.is_empty() && !token.is_empty() => {
                    engine = engine.with_dockerhub_credentials(user, token)
                }
                _ => return Err(Error("set both Docker Hub credentials to nonempty values")),
            }
            entry.insert(engine);
        }
        let engine = engines.get_mut(&id).expect("installation engine");
        engine
            .set_batch_options(
                settings.concurrency,
                settings.recheck(self.recheck.as_deref()),
            )
            .map_err(|_| Error("invalid batch options"))?;
        engine.set_advertised_server(
            settings
                .advertised(self.derived_url.as_deref())
                .filter(|_| self.signs_in),
        );
        Ok(engine)
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
            let report = {
                // The same lock as queued batches, so a remote run never races one.
                let mut engines = self.engines.lock().await;
                let engine = self
                    .engine(&mut engines, installation, settings)
                    .map_err(|e| e.0)?;
                let options = patchula_engine::Options {
                    repositories,
                    ..patchula_engine::Options::default()
                };
                match mode {
                    Mode::Lookup => engine.lookup(options).await,
                    Mode::DryRun => engine.run(options, true, false).await,
                    Mode::Run => engine.run(options, false, false).await,
                }
                .map_err(|_| "the run failed on the server")?
            };
            if mode == Mode::Run {
                if let Ok(client) = Client::with_credentials(
                    self.app.installation(installation),
                    self.cache.clone(),
                    patchula_github::Options::default(),
                    self.transport.clone(),
                ) {
                    crate::public::record(
                        &self.cache,
                        &client,
                        &self.bot_login,
                        installation,
                        &report,
                        started,
                        &self.member(installation),
                    )
                    .await;
                }
                tracing::info!(target: "patchula_server", installation, user = login, "remote run report");
                output_report(&report, settings.report_directory.as_deref())
                    .await
                    .map_err(|e| e.0)?;
            }
            Ok(report)
        })
    }
}
fn skip_report(reason: &str, discovery_only: bool) {
    tracing::info!(target: "patchula_server", reason, discovery_only, repository_runs = 0, "batch report skipped");
}
async fn output_report(report: &Report, directory: Option<&std::path::Path>) -> Result<(), Error> {
    if let Some(directory) = directory {
        let report = report.clone();
        let directory = directory.to_owned();
        tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&directory)
                .map_err(|_| Error("cannot create report directory"))?;
            let name = format!(
                "batch-{}-{}.json",
                Utc::now().format("%Y%m%dT%H%M%S%.9fZ"),
                std::process::id()
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
            .claim_batch(Utc::now().timestamp_millis(), settings.batch_size)
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
            Err(error) => (
                BatchOutcome {
                    error_kinds: BTreeMap::from([("batch-operation", 1)]),
                    requeue: false,
                },
                Some(error),
            ),
        };
        if outcome.failed() {
            tracing::error!(target: "patchula_server", error = error.as_ref().map(|e| e.0),
                error_kinds = ?outcome.error_kinds, error_count = outcome.error_kinds.values().sum::<usize>(),
                "batch failed; work will be retried");
        }
        if outcome.requeue {
            tracing::warn!(target: "patchula_server", reason = "default branch moved; requeued",
                installation = work[0].installation, "batch requeued");
        }
        tracing::info!(target: "patchula_server", installation = work[0].installation, entries = work.len(), success = !outcome.failed(), requeued = outcome.retry(), "batch finished");
        cache
            .finish_work(
                claims,
                !outcome.retry(),
                Utc::now().timestamp_millis(),
                if !outcome.retry() {
                    settings.coalescing_ms as i64
                } else {
                    60_000
                },
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
    let mut worker = tokio::spawn(worker(
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
    cache
        .recover_work()
        .await
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
        settings.current().sweep_cron,
        settings.current().sweep_timezone,
    );
    loop {
        if *stop.borrow() {
            break;
        }
        let current = settings.current();
        let new_schedule = (current.sweep_cron.clone(), current.sweep_timezone.clone());
        if new_schedule != schedule {
            next_sweep = sweep_after(&current, Utc::now());
            schedule = new_schedule;
        }
        let sweep_due = next_sweep.is_some_and(|next| next <= Utc::now());
        if sweep_due || tokio::time::Instant::now() >= probe_at {
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
                            current.coalescing_ms as i64,
                            Attribution::reason("sweep"),
                        )
                        .await
                        .map_err(|_| Error("cannot persist cron sweep"))?;
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
                next_sweep = if success {
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
    let schedule = patchula_config::schedule(settings.sweep_cron.as_deref()?, true).ok()?;
    let tz = chrono_tz::Tz::from_str(&settings.sweep_timezone).ok()?;
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
