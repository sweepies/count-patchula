//! The API behind the CLI's `--remote` option. A GitHub user access token from
//! the App's device flow identifies the caller, and every repository in a
//! request must let that user push. Jobs live in memory only: a restart loses
//! them, and the CLI reports that.
pub mod protocol;

use crate::{
    reload::Reload,
    remote::protocol::{
        Accepted, AuthInfo, JOB_TTL_SECONDS, JobStatus, MAX_JOBS, MAX_REPOSITORIES,
        RUN_LIMIT_SECONDS, RejectBody, Start, USER_TTL_SECONDS,
    },
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::{FutureExt, future::BoxFuture};
use patchula_cache::Cache;
use patchula_config::settings::{Settings, valid_repo};
use patchula_github::{Client, app::App};
use patchula_http::Transport;
use patchula_report::Report;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::Mutex;

/// What a run request asks for; the wire shape is `protocol::Mode`.
pub use self::protocol::Mode;

/// Runs one request's repositories, which all belong to one installation.
pub(crate) trait Runner: Send + Sync + 'static {
    fn run<'a>(
        &'a self,
        installation: u64,
        repositories: Vec<String>,
        mode: Mode,
        login: &'a str,
        settings: &'a Settings,
    ) -> BoxFuture<'a, Result<Report, &'static str>>;
}

#[derive(Clone)]
pub(crate) struct Api(Arc<Inner>);
struct Inner {
    app: Arc<App>,
    client_id: Option<String>,
    bot: String,
    transport: Arc<dyn Transport>,
    cache: Cache,
    settings: Reload,
    accepting: Arc<AtomicBool>,
    runner: Arc<dyn Runner>,
    users: Mutex<BTreeMap<[u8; 32], (User, i64)>>,
    jobs: Mutex<BTreeMap<String, Job>>,
    tasks: Mutex<tokio::task::JoinSet<()>>,
    next: AtomicU64,
}
/// Who a token belongs to, and the installations of this App it reaches.
/// Only a user token this App issued can list them, so a personal token or
/// another App's token never passes.
#[derive(Clone)]
struct User {
    login: String,
    installations: Vec<u64>,
}
struct Job {
    login: String,
    state: JobState,
}
/// A job runs until its task ends, then holds its outcome and when it ended.
/// The task records an outcome on every exit, a panic included, so no job
/// stays running after its task is gone.
enum JobState {
    Running,
    Complete { report: Box<Report>, at: i64 },
    Failed { error: &'static str, at: i64 },
}
impl JobState {
    fn finished(&self) -> Option<i64> {
        match self {
            Self::Running => None,
            Self::Complete { at, .. } | Self::Failed { at, .. } => Some(*at),
        }
    }
}

pub(crate) struct Parts {
    pub app: Arc<App>,
    pub client_id: Option<String>,
    /// The App's bot login, which the CLI matches against the advertisement.
    pub bot: String,
    pub transport: Arc<dyn Transport>,
    pub cache: Cache,
    pub settings: Reload,
    pub accepting: Arc<AtomicBool>,
    pub runner: Arc<dyn Runner>,
}
impl Api {
    pub(crate) fn new(parts: Parts) -> Self {
        Self(Arc::new(Inner {
            app: parts.app,
            client_id: parts.client_id,
            bot: parts.bot,
            transport: parts.transport,
            cache: parts.cache,
            settings: parts.settings,
            accepting: parts.accepting,
            runner: parts.runner,
            users: Mutex::new(BTreeMap::new()),
            jobs: Mutex::new(BTreeMap::new()),
            tasks: Mutex::new(tokio::task::JoinSet::new()),
            next: AtomicU64::new(1),
        }))
    }
    /// Waits for every remote run in progress. Shutdown calls this once no
    /// new runs are accepted.
    pub(crate) async fn drain(&self) {
        let mut tasks = std::mem::take(&mut *self.0.tasks.lock().await);
        while tasks.join_next().await.is_some() {}
    }
}

pub(crate) fn routes(api: Api) -> Router {
    Router::new()
        .route("/api/v1/auth", get(auth))
        .route("/api/v1/runs", post(start))
        .route("/api/v1/runs/{id}", get(status))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .with_state(api)
}

struct Reject(StatusCode, &'static str, String);
impl IntoResponse for Reject {
    fn into_response(self) -> Response {
        tracing::warn!(target: "patchula_server", status = self.0.as_u16(), error_kind = self.1, "remote request rejected");
        (
            self.0,
            Json(RejectBody {
                error_kind: self.1.into(),
                error: self.2,
            }),
        )
            .into_response()
    }
}
fn reject(status: StatusCode, kind: &'static str, message: impl Into<String>) -> Reject {
    Reject(status, kind, message.into())
}
fn unauthorized() -> Reject {
    reject(
        StatusCode::UNAUTHORIZED,
        "unauthorized",
        "sign in with this server's GitHub App: patchula auth login --server <url>",
    )
}
fn github_unavailable() -> Reject {
    reject(
        StatusCode::BAD_GATEWAY,
        "github-unavailable",
        "GitHub did not answer as expected",
    )
}

async fn auth(State(api): State<Api>) -> Result<Json<AuthInfo>, Reject> {
    let client_id = api.0.client_id.as_deref().ok_or_else(|| {
        reject(
            StatusCode::SERVICE_UNAVAILABLE,
            "no-client-id",
            "the server could not read its GitHub App's client ID",
        )
    })?;
    Ok(Json(AuthInfo {
        client_id: client_id.to_owned(),
        bot: api.0.bot.clone(),
    }))
}

async fn start(
    State(api): State<Api>,
    headers: HeaderMap,
    body: Result<Json<Start>, axum::extract::rejection::JsonRejection>,
) -> Result<Response, Reject> {
    let user = api.user(&headers).await?;
    let login = user.login.clone();
    let Json(start) = body.map_err(|_| {
        reject(
            StatusCode::BAD_REQUEST,
            "invalid-request",
            "expected {\"mode\", \"repositories\"}",
        )
    })?;
    if !api.0.accepting.load(Ordering::Acquire) {
        return Err(reject(
            StatusCode::SERVICE_UNAVAILABLE,
            "shutting-down",
            "the server is shutting down",
        ));
    }
    if start.repositories.is_empty() || start.repositories.len() > MAX_REPOSITORIES {
        return Err(reject(
            StatusCode::BAD_REQUEST,
            "invalid-request",
            format!("name 1 to {MAX_REPOSITORIES} repositories"),
        ));
    }
    let mut repositories = Vec::new();
    for repo in &start.repositories {
        if !valid_repo(repo) {
            return Err(reject(
                StatusCode::BAD_REQUEST,
                "invalid-request",
                format!("{repo} is not owner/repo"),
            ));
        }
        let repo = repo.to_ascii_lowercase();
        if !repositories.contains(&repo) {
            repositories.push(repo);
        }
    }
    let settings = api.0.settings.current();
    let installation = api.installation(&repositories, &settings).await?;
    if !user.installations.contains(&installation) {
        return Err(reject(
            StatusCode::FORBIDDEN,
            "not-installed",
            "your sign-in does not reach this App's installation for these repositories",
        ));
    }
    api.authorize(installation, &repositories, &login).await?;
    // Held from before the job is recorded until its task is spawned, with no
    // await between the two: a request dropped at an await point either
    // recorded nothing or already has a task to finish the job.
    let mut tasks = api.0.tasks.lock().await;
    // Checked again under the lock: shutdown stops accepting before `drain`
    // takes the tasks, so a run spawned here is one drain waits for.
    if !api.0.accepting.load(Ordering::Acquire) {
        return Err(reject(
            StatusCode::SERVICE_UNAVAILABLE,
            "shutting-down",
            "the server is shutting down",
        ));
    }
    let id = {
        let now = chrono::Utc::now().timestamp();
        let mut jobs = api.0.jobs.lock().await;
        jobs.retain(|_, job| {
            job.state
                .finished()
                .is_none_or(|at| at + JOB_TTL_SECONDS > now)
        });
        if jobs
            .values()
            .any(|job| job.login == login && matches!(job.state, JobState::Running))
        {
            return Err(reject(
                StatusCode::TOO_MANY_REQUESTS,
                "job-running",
                "you already have a run in progress on this server",
            ));
        }
        if jobs.len() >= MAX_JOBS {
            return Err(reject(
                StatusCode::TOO_MANY_REQUESTS,
                "too-many-jobs",
                "the server is holding too many runs; try again shortly",
            ));
        }
        let id = format!("{now:x}-{:x}", api.0.next.fetch_add(1, Ordering::Relaxed));
        jobs.insert(
            id.clone(),
            Job {
                login: login.clone(),
                state: JobState::Running,
            },
        );
        id
    };
    tracing::info!(target: "patchula_server", installation, user = %login, mode = ?start.mode,
        repositories = repositories.len(), "remote run started");
    let job = api.clone();
    let job_id = id.clone();
    while tasks.try_join_next().is_some() {}
    tasks.spawn(async move {
        // The runner is called inside the caught future, so a panic while
        // building its future is caught too. RUN_LIMIT_SECONDS bounds the
        // run: a hung one fails, freeing the user for their next run.
        let result = tokio::time::timeout(
            Duration::from_secs(RUN_LIMIT_SECONDS as u64),
            std::panic::AssertUnwindSafe(async {
                job.0
                    .runner
                    .run(installation, repositories, start.mode, &login, &settings)
                    .await
            })
            .catch_unwind(),
        )
        .await;
        let result = match result {
            Ok(outcome) => outcome.unwrap_or(Err("the run stopped unexpectedly")),
            Err(_) => Err("the run exceeded the server's time limit"),
        };
        let success = result.is_ok();
        let at = chrono::Utc::now().timestamp();
        let mut jobs = job.0.jobs.lock().await;
        if let Some(entry) = jobs.get_mut(&job_id) {
            entry.state = match result {
                Ok(report) => JobState::Complete {
                    report: Box::new(report),
                    at,
                },
                Err(error) => JobState::Failed { error, at },
            };
        }
        tracing::info!(target: "patchula_server", installation, user = %login, success, "remote run finished");
    });
    drop(tasks);
    Ok((StatusCode::ACCEPTED, Json(Accepted { id })).into_response())
}

async fn status(
    State(api): State<Api>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<JobStatus>, Reject> {
    let login = api.user(&headers).await?.login;
    let jobs = api.0.jobs.lock().await;
    let job = jobs
        .get(&id)
        .filter(|job| job.login == login)
        .ok_or_else(|| {
            reject(
                StatusCode::NOT_FOUND,
                "unknown-run",
                "no such run; the server may have restarted",
            )
        })?;
    Ok(Json(match &job.state {
        JobState::Running => JobStatus::Running,
        JobState::Complete { report, .. } => JobStatus::Complete {
            report: report.as_ref().clone(),
        },
        JobState::Failed { error, .. } => JobStatus::Failed {
            error: (*error).to_owned(),
        },
    }))
}

impl Api {
    /// The user behind the request's token, which must be a user access
    /// token from this App's device flow.
    async fn user(&self, headers: &HeaderMap) -> Result<User, Reject> {
        let token = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .filter(|token| !token.is_empty())
            .ok_or_else(unauthorized)?;
        let key: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        let now = chrono::Utc::now().timestamp();
        {
            let mut users = self.0.users.lock().await;
            users.retain(|_, (_, expires)| *expires > now);
            if let Some((user, _)) = users.get(&key) {
                return Ok(user.clone());
            }
        }
        let login = self
            .github(token, "user")
            .await?
            .get("login")
            .and_then(|login| login.as_str())
            .filter(|login| !login.is_empty())
            .map(str::to_owned)
            .ok_or_else(github_unavailable)?;
        let mut installations = Vec::new();
        for page in 1..=10 {
            let value = self
                .github(
                    token,
                    &format!("user/installations?per_page=100&page={page}"),
                )
                .await?;
            let entries = value["installations"]
                .as_array()
                .ok_or_else(github_unavailable)?;
            installations.extend(entries.iter().filter_map(|entry| entry["id"].as_u64()));
            if entries.len() < 100 {
                break;
            }
        }
        let user = User {
            login,
            installations,
        };
        let mut users = self.0.users.lock().await;
        if users.len() < 10_000 {
            users.insert(key, (user.clone(), now + USER_TTL_SECONDS));
        }
        Ok(user)
    }

    /// GETs `path` from the GitHub API with the user's token.
    async fn github(&self, token: &str, path: &str) -> Result<serde_json::Value, Reject> {
        let mut request = reqwest::Request::new(
            reqwest::Method::GET,
            format!("https://api.github.com/{path}")
                .parse()
                .map_err(|_| github_unavailable())?,
        );
        let mut auth = header::HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| unauthorized())?;
        auth.set_sensitive(true);
        let headers = request.headers_mut();
        headers.insert(header::AUTHORIZATION, auth);
        headers.insert(
            header::ACCEPT,
            header::HeaderValue::from_static("application/vnd.github+json"),
        );
        headers.insert(
            "x-github-api-version",
            header::HeaderValue::from_static("2022-11-28"),
        );
        let response = self
            .0
            .transport
            .send(request)
            .await
            .map_err(|_| github_unavailable())?;
        // A rate limit is also a 403, but the token is fine: the CLI must
        // keep it, so it is not reported as unauthorized.
        let limited = response
            .headers
            .get("x-ratelimit-remaining")
            .is_some_and(|remaining| remaining == "0")
            || response.headers.contains_key(header::RETRY_AFTER);
        match response.status {
            status if status.is_success() => {
                serde_json::from_slice(&response.body).map_err(|_| github_unavailable())
            }
            StatusCode::FORBIDDEN | StatusCode::TOO_MANY_REQUESTS if limited => Err(reject(
                StatusCode::SERVICE_UNAVAILABLE,
                "github-rate-limited",
                "GitHub is rate limiting this sign-in; try again shortly",
            )),
            // GitHub answers 403 to a token that is not one of this App's user tokens.
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(unauthorized()),
            _ => Err(github_unavailable()),
        }
    }

    /// The one installation every repository belongs to, if the settings allow them.
    async fn installation(
        &self,
        repositories: &[String],
        settings: &Settings,
    ) -> Result<u64, Reject> {
        let mut found = None;
        for repo in repositories {
            let id = self
                .0
                .app
                .repository_installation(repo)
                .await
                .map_err(|_| {
                    reject(
                        StatusCode::BAD_GATEWAY,
                        "github-unavailable",
                        "cannot look up the App installation",
                    )
                })?
                .filter(|id| settings.allows(*id, repo))
                .ok_or_else(|| {
                    reject(
                        StatusCode::FORBIDDEN,
                        "not-installed",
                        format!("this server does not update {repo}"),
                    )
                })?;
            if found.is_some_and(|found| found != id) {
                return Err(reject(
                    StatusCode::BAD_REQUEST,
                    "mixed-installations",
                    "the repositories belong to different App installations; run them separately",
                ));
            }
            found = Some(id);
        }
        found.ok_or_else(|| {
            reject(
                StatusCode::BAD_REQUEST,
                "invalid-request",
                "no repositories",
            )
        })
    }

    /// Checks the user can push to every repository. Read access is not
    /// enough: on a public repository everyone has it.
    async fn authorize(
        &self,
        installation: u64,
        repositories: &[String],
        login: &str,
    ) -> Result<(), Reject> {
        let client = Client::with_credentials(
            self.0.app.installation(installation),
            self.0.cache.clone(),
            patchula_engine::settings::github_options(&self.0.settings.current()),
            self.0.transport.clone(),
        )
        .map_err(|_| {
            reject(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal-error",
                "cannot create installation client",
            )
        })?;
        for repo in repositories {
            let permission = match client
                .get_fresh::<serde_json::Value>(&format!(
                    "repos/{repo}/collaborators/{login}/permission"
                ))
                .await
            {
                Ok(value) => value["permission"].as_str().map(str::to_owned),
                Err(patchula_github::Error::Status(403 | 404)) => None,
                Err(_) => {
                    return Err(reject(
                        StatusCode::BAD_GATEWAY,
                        "github-unavailable",
                        "cannot check repository permission",
                    ));
                }
            };
            if !matches!(permission.as_deref(), Some("admin" | "write")) {
                return Err(reject(
                    StatusCode::FORBIDDEN,
                    "forbidden",
                    format!("{login} cannot push to {repo}"),
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
