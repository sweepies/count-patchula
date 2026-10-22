//! The API behind the CLI's `--server` option. A GitHub user access token from
//! the App's device flow identifies the caller, and every repository in a
//! request must let that user push. Jobs live in memory only: a restart loses
//! them, and the CLI reports that.
use crate::settings::{Reload, Settings, valid_repo};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::future::BoxFuture;
use patchula_cache::Cache;
use patchula_github::{Client, app::App};
use patchula_http::Transport;
use patchula_report::Report;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use tokio::sync::Mutex;

/// Repositories one request may name.
const MAX_REPOSITORIES: usize = 20;
/// How long a verified user token is trusted before GitHub is asked again.
const USER_TTL_SECONDS: i64 = 300;
/// How long a finished job's report stays available to poll.
const JOB_TTL_SECONDS: i64 = 900;
const MAX_JOBS: usize = 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    Lookup,
    DryRun,
    Run,
}

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
    transport: Arc<dyn Transport>,
    cache: Cache,
    settings: Reload,
    accepting: Arc<AtomicBool>,
    runner: Arc<dyn Runner>,
    users: Mutex<BTreeMap<[u8; 32], (String, i64)>>,
    jobs: Mutex<BTreeMap<String, Job>>,
    next: AtomicU64,
}
struct Job {
    login: String,
    state: JobState,
    finished: Option<i64>,
}
enum JobState {
    Running,
    Complete(Box<Report>),
    Failed(&'static str),
}

pub(crate) struct Parts {
    pub app: Arc<App>,
    pub client_id: Option<String>,
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
            transport: parts.transport,
            cache: parts.cache,
            settings: parts.settings,
            accepting: parts.accepting,
            runner: parts.runner,
            users: Mutex::new(BTreeMap::new()),
            jobs: Mutex::new(BTreeMap::new()),
            next: AtomicU64::new(1),
        }))
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
        (self.0, Json(json!({"errorKind": self.1, "error": self.2}))).into_response()
    }
}
fn reject(status: StatusCode, kind: &'static str, message: impl Into<String>) -> Reject {
    Reject(status, kind, message.into())
}

async fn auth(State(api): State<Api>) -> Result<Json<serde_json::Value>, Reject> {
    let client_id = api.0.client_id.as_deref().ok_or_else(|| {
        reject(
            StatusCode::SERVICE_UNAVAILABLE,
            "no-client-id",
            "the server could not read its GitHub App's client ID",
        )
    })?;
    Ok(Json(json!({"clientId": client_id})))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Start {
    mode: Mode,
    repositories: Vec<String>,
}

async fn start(
    State(api): State<Api>,
    headers: HeaderMap,
    body: Result<Json<Start>, axum::extract::rejection::JsonRejection>,
) -> Result<Response, Reject> {
    let login = api.user(&headers).await?;
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
    api.authorize(installation, &repositories, &login).await?;
    let id = {
        let now = chrono::Utc::now().timestamp();
        let mut jobs = api.0.jobs.lock().await;
        jobs.retain(|_, job| job.finished.is_none_or(|at| at + JOB_TTL_SECONDS > now));
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
                finished: None,
            },
        );
        id
    };
    tracing::info!(target: "patchula_server", installation, user = %login, mode = ?start.mode,
        repositories = repositories.len(), "remote run started");
    let job = api.clone();
    let job_id = id.clone();
    tokio::spawn(async move {
        let result = job
            .0
            .runner
            .run(installation, repositories, start.mode, &login, &settings)
            .await;
        let success = result.is_ok();
        let mut jobs = job.0.jobs.lock().await;
        if let Some(entry) = jobs.get_mut(&job_id) {
            entry.state = match result {
                Ok(report) => JobState::Complete(Box::new(report)),
                Err(error) => JobState::Failed(error),
            };
            entry.finished = Some(chrono::Utc::now().timestamp());
        }
        tracing::info!(target: "patchula_server", installation, user = %login, success, "remote run finished");
    });
    Ok((StatusCode::ACCEPTED, Json(json!({"id": id}))).into_response())
}

async fn status(
    State(api): State<Api>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, Reject> {
    let login = api.user(&headers).await?;
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
        JobState::Running => json!({"status": "running"}),
        JobState::Complete(report) => json!({"status": "complete", "report": report}),
        JobState::Failed(error) => json!({"status": "failed", "error": error}),
    }))
}

impl Api {
    /// The GitHub login behind the request's user access token.
    async fn user(&self, headers: &HeaderMap) -> Result<String, Reject> {
        let unauthorized = || {
            reject(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "send a GitHub user access token for this server's App",
            )
        };
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
            if let Some((login, _)) = users.get(&key) {
                return Ok(login.clone());
            }
        }
        let mut request = reqwest::Request::new(
            reqwest::Method::GET,
            "https://api.github.com/user"
                .parse()
                .expect("static GitHub URL"),
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
        let response = self.0.transport.send(request).await.map_err(|_| {
            reject(
                StatusCode::BAD_GATEWAY,
                "github-unavailable",
                "GitHub did not answer",
            )
        })?;
        if response.status == StatusCode::UNAUTHORIZED {
            return Err(unauthorized());
        }
        let login = (response.status.is_success())
            .then(|| serde_json::from_slice::<serde_json::Value>(&response.body).ok())
            .flatten()
            .and_then(|user| user["login"].as_str().map(str::to_owned))
            .filter(|login| !login.is_empty())
            .ok_or_else(|| {
                reject(
                    StatusCode::BAD_GATEWAY,
                    "github-unavailable",
                    "GitHub did not return the user",
                )
            })?;
        let mut users = self.0.users.lock().await;
        if users.len() < 10_000 {
            users.insert(key, (login.clone(), now + USER_TTL_SECONDS));
        }
        Ok(login)
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
            Default::default(),
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
                .get_fresh(&format!("repos/{repo}/collaborators/{login}/permission"))
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
