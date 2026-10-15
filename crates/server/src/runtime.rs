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
use rustovate_cache::{
    Cache,
    queue::{Attribution, Work},
};
use rustovate_engine::Engine;
use rustovate_github::{Client, app::App};
use rustovate_http::Transport;
use rustovate_report::{Report, Repository, SCHEMA_VERSION, Status};
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
    let transport: Arc<dyn Transport> = Arc::new(
        rustovate_http::Network::new().map_err(|_| Error("cannot create HTTP transport"))?,
    );
    let app = App::new(options.app_id, &options.private_key, transport.clone())
        .map_err(|_| Error("invalid GitHub App credentials"))?;
    // The canonical slug is fetched, never guessed from repository names or senders.
    let bot = app
        .bot_login()
        .await
        .map_err(|_| Error("GitHub App authentication failed"))?;
    let runner = Arc::new(Engines {
        app: app.clone(),
        transport,
        cache: cache.clone(),
        engines: Mutex::new(BTreeMap::new()),
        recheck: options.recheck,
        bot_login: bot.clone(),
    });
    let webhook = Webhook::new(cache.clone(), settings.clone(), bot, options.webhook_secret)
        .with_app(app.clone());
    let ready = Arc::new(AtomicBool::new(false));
    let listener = TcpListener::bind(options.listen)
        .await
        .map_err(|_| Error("cannot bind server listener"))?;
    tracing::info!(target: "rustovate_server", address = %listener.local_addr().map_err(|_| Error("cannot inspect listener"))?, "server listening");
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
    drop(lock);
    result
}

fn router(webhook: Webhook, ready: Arc<AtomicBool>) -> Router {
    let settings = webhook.settings.clone();
    let accepting = webhook.accepting.clone();
    let cache = webhook.cache.clone();
    Router::new()
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
        .with_state(webhook)
}

trait Batch: Send + Sync + 'static {
    fn run<'a>(
        &'a self,
        work: &'a [Work],
        settings: &'a Settings,
        attribution: &'a Attribution,
    ) -> BoxFuture<'a, Result<(), Error>>;
}
struct Engines {
    app: Arc<App>,
    transport: Arc<dyn Transport>,
    cache: Cache,
    engines: Mutex<BTreeMap<u64, Engine>>,
    recheck: Option<String>,
    bot_login: String,
}
impl Batch for Engines {
    fn run<'a>(
        &'a self,
        work: &'a [Work],
        settings: &'a Settings,
        attribution: &'a Attribution,
    ) -> BoxFuture<'a, Result<(), Error>> {
        Box::pin(async move {
            let id = work.first().ok_or(Error("empty batch"))?.installation;
            if !settings.installation_allowed(id) {
                skip_report("installation not allowlisted", false);
                return Ok(());
            }
            let credentials = self.app.installation(id);
            if work.iter().any(|w| w.repo.is_empty()) {
                let client = Client::with_credentials(
                    credentials.clone(),
                    self.cache.clone(),
                    Default::default(),
                    self.transport.clone(),
                )
                .map_err(|_| Error("cannot create installation client"))?;
                let mut repos = Vec::new();
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
                            repos.push(Work::repository(id, repo.to_ascii_lowercase()));
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
            }
            let client = Client::with_credentials(
                credentials.clone(),
                self.cache.clone(),
                Default::default(),
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
                return Ok(());
            }
            let mut engines = self.engines.lock().await;
            if let std::collections::btree_map::Entry::Vacant(entry) = engines.entry(id) {
                let mut engine = Engine::with_credentials(
                    credentials,
                    self.cache.clone(),
                    settings.concurrency,
                    self.transport.clone(),
                )
                .map_err(|_| Error("cannot create batch engine"))?;
                match (
                    std::env::var("RUSTOVATE_DOCKERHUB_USERNAME").ok(),
                    std::env::var("RUSTOVATE_DOCKERHUB_TOKEN").ok(),
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
            let result = engine
                .run(
                    rustovate_engine::Options {
                        repositories: repositories.clone(),
                        ..Default::default()
                    },
                    false,
                    false,
                )
                .await;
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
                            default_branch: None,
                            commit: None,
                        })
                        .collect(),
                    stats: None,
                    actions: None,
                },
            };
            output_report(&report, settings.report_directory.as_deref()).await?;
            if result.is_err()
                || report
                    .repositories
                    .iter()
                    .any(|r| r.status == Status::Error)
                || report.actions.as_ref().is_some_and(|a| {
                    a.branches
                        .iter()
                        .any(|b| b.status == rustovate_report::ActionStatus::Error)
                        || a.pull_requests
                            .iter()
                            .any(|pr| pr.status == rustovate_report::ActionStatus::Error)
                })
            {
                return Err(Error("batch had errors; work will be retried"));
            }
            Ok(())
        })
    }
}
fn skip_report(reason: &str, discovery_only: bool) {
    tracing::info!(target: "rustovate_server", reason, discovery_only, repository_runs = 0, "batch report skipped");
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
            tracing::info!(target: "rustovate_server", path = %path.display(), repository_runs = report.repositories.len(), "batch report written");
            Ok::<(), Error>(())
        })
        .await
        .map_err(|_| Error("report writer failed"))??;
    } else {
        let json =
            serde_json::to_string(report).map_err(|_| Error("cannot encode batch report"))?;
        tracing::info!(target: "rustovate_server", report = %json, "batch report");
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
        tracing::info!(target: "rustovate_server", installation = work[0].installation, entries = work.len(), reasons = ?attribution.reasons, delivery_ids = ?attribution.deliveries, "batch started");
        let result = runner.run(&work, &settings, &attribution).await;
        if let Err(error) = &result {
            tracing::error!(target: "rustovate_server", %error, "batch failed");
        }
        tracing::info!(target: "rustovate_server", installation = work[0].installation, entries = work.len(), success = result.is_ok(), "batch finished");
        cache
            .finish_work(
                claims,
                result.is_ok(),
                Utc::now().timestamp_millis(),
                if result.is_ok() {
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
    tracing::info!(target: "rustovate_server", "shutdown started; finishing current batch");
    let drained = tokio::time::timeout(timeout, async {
        if !worker.is_finished() {
            let _ = (&mut worker).await;
        }
        if !server.is_finished() {
            let _ = (&mut server).await;
        }
    })
    .await
    .is_ok();
    worker.abort();
    server.abort();
    if !drained {
        tracing::warn!(target: "rustovate_server", "shutdown deadline reached; unfinished work remains queued");
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
    let mut next_sweep = Some(Utc::now()); // Recover the installation inventory on startup.
    let mut startup = true;
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
                                .map(|id| Work::repository(id, String::new()))
                                .collect(),
                            Utc::now().timestamp_millis(),
                            current.coalescing_ms as i64,
                            Attribution::reason(if startup {
                                "startup discovery"
                            } else {
                                "sweep"
                            }),
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
                tracing::error!(target: "rustovate_server", %error, "readiness/sweep probe failed");
            }
            probe_at = tokio::time::Instant::now() + Duration::from_secs(30);
            if sweep_due {
                next_sweep = if success {
                    startup = false;
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
            Err(rustovate_github::Error::Status(404)) => continue,
            Err(_) => return Err(Error("check-event PR ownership lookup failed")),
        };
        if rustovate_update::pull_requests::owned_pr(&pr, bot, &work.repo) {
            return Ok(true);
        }
    }
    Ok(false)
}
fn sweep_after(settings: &Settings, now: chrono::DateTime<Utc>) -> Option<chrono::DateTime<Utc>> {
    let schedule = cron::Schedule::from_str(settings.sweep_cron.as_deref()?).ok()?;
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
