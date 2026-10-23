//! The read-only API behind a web dashboard: what the Count last saw of each
//! public repository he runs. It needs no sign-in, so it never shows a private
//! repository or one the settings no longer allow. The server records a
//! repository after each run that writes, and discovery keeps the list and
//! each repository's visibility current.
use crate::settings::Reload;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use chrono::{DateTime, Utc};
use patchula_cache::{
    Cache,
    repositories::{Row, Run},
};
use patchula_github::Client;
use patchula_report::{ActionStatus, PackageFile, PullRequestAction, Report, Status};
use serde::Serialize;
use serde_json::{Value, json};

#[derive(Clone)]
pub(crate) struct Public {
    pub cache: Cache,
    pub settings: Reload,
}

pub(crate) fn routes(public: Public) -> Router {
    Router::new()
        .route("/api/v1/repositories", get(list))
        .route("/api/v1/repositories/{owner}/{repo}", get(show))
        // A dashboard on another origin reads this without credentials.
        .layer(axum::middleware::map_response(
            |mut response: Response| async {
                response.headers_mut().insert(
                    header::ACCESS_CONTROL_ALLOW_ORIGIN,
                    HeaderValue::from_static("*"),
                );
                response
            },
        ))
        .with_state(public)
}

/// What a run leaves of one repository. Branch diffs stay out: they can be
/// large, and the pull request shows them.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Snapshot<'a> {
    repository: &'a str,
    ran_at: String,
    status: Status,
    reason: Option<&'a str>,
    warnings: &'a [String],
    default_branch: Option<&'a str>,
    commit: Option<&'a str>,
    package_files: &'a [PackageFile],
    branches: Vec<Branch<'a>>,
    pull_requests: Vec<&'a PullRequestAction>,
    dashboard: Option<u64>,
    merged: Vec<Merged>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Branch<'a> {
    branch: &'a str,
    status: ActionStatus,
    reason: Option<&'a str>,
    message: &'a str,
    base_commit: &'a str,
    commit: Option<&'a str>,
    lockfile_pending: bool,
    files: Vec<&'a str>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Merged {
    number: u64,
    title: Option<String>,
    url: Option<String>,
    merged_at: Option<String>,
}
/// The counts the repository list shows.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Summary<'a> {
    status: Status,
    reason: Option<&'a str>,
    default_branch: Option<&'a str>,
    dependencies: usize,
    updates: usize,
    pull_requests: usize,
    dashboard: Option<u64>,
}

/// Records every repository in a writing run's report. A repository whose
/// visibility cannot be read is recorded as private, so it stays hidden.
pub(crate) async fn record(cache: &Cache, client: &Client, installation: u64, report: &Report) {
    let now = Utc::now();
    let empty = Vec::new();
    let actions = report.actions.as_ref();
    for repo in &report.repositories {
        let name = repo.repository.as_str();
        let private = match client.get(&format!("repos/{name}")).await {
            Ok(response) => serde_json::from_slice::<Value>(&response.body)
                .ok()
                .and_then(|value| value["private"].as_bool())
                .unwrap_or(true),
            Err(_) => true,
        };
        let pull_requests: Vec<_> = actions
            .map_or(&empty, |a| &a.pull_requests)
            .iter()
            .filter(|pr| pr.repository == name)
            .collect();
        let branches = actions
            .map(|a| a.branches.as_slice())
            .unwrap_or_default()
            .iter()
            .filter(|b| b.repository == name)
            .map(|b| Branch {
                branch: &b.branch,
                status: b.status,
                reason: b.reason.as_deref(),
                message: &b.message,
                base_commit: &b.base_commit,
                commit: b.commit.as_deref(),
                lockfile_pending: b.lockfile_pending,
                files: b.files.iter().map(|f| f.path.as_str()).collect(),
            })
            .collect();
        let dashboard = patchula_update::dashboard::issue_number(cache, name).await;
        let merged = patchula_update::automerge::merged(cache, name)
            .await
            .iter()
            .filter_map(|pr| {
                Some(Merged {
                    number: pr["number"].as_u64()?,
                    title: pr["title"].as_str().map(str::to_owned),
                    url: pr["html_url"].as_str().map(str::to_owned),
                    merged_at: pr["merged_at"].as_str().map(str::to_owned),
                })
            })
            .collect();
        let dependencies = repo.package_files.iter().flat_map(|f| &f.dependencies);
        let summary = Summary {
            status: repo.status,
            reason: repo.reason.as_deref(),
            default_branch: repo.default_branch.as_deref(),
            dependencies: dependencies.clone().count(),
            updates: dependencies.filter(|d| !d.updates.is_empty()).count(),
            pull_requests: pull_requests.iter().filter(|pr| open(pr)).count(),
            dashboard,
        };
        let snapshot = Snapshot {
            repository: name,
            ran_at: now.to_rfc3339(),
            status: repo.status,
            reason: repo.reason.as_deref(),
            warnings: &repo.warnings,
            default_branch: repo.default_branch.as_deref(),
            commit: repo.commit.as_deref(),
            package_files: &repo.package_files,
            branches,
            pull_requests,
            dashboard,
            merged,
        };
        let (Ok(summary), Ok(snapshot)) =
            (serde_json::to_vec(&summary), serde_json::to_vec(&snapshot))
        else {
            continue;
        };
        let run = Run {
            installation,
            repo: name.to_owned(),
            private,
            ran_at: now.timestamp(),
            summary,
            snapshot,
        };
        if let Err(error) = cache.repository_ran(run).await {
            tracing::warn!(target: "patchula_server", repository = name, error_kind = error.kind(),
                "cannot record the repository for the public API");
        }
    }
}

/// A PR the Count still has open after the run.
fn open(pr: &PullRequestAction) -> bool {
    pr.number.is_some()
        && !matches!(
            pr.status,
            ActionStatus::Closed | ActionStatus::Merged | ActionStatus::Deleted
        )
}

struct Reject(StatusCode, &'static str, &'static str);
impl IntoResponse for Reject {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"errorKind": self.1, "error": self.2}))).into_response()
    }
}
fn unknown() -> Reject {
    Reject(
        StatusCode::NOT_FOUND,
        "unknown-repository",
        "this server shows no such public repository",
    )
}
fn unavailable() -> Reject {
    Reject(
        StatusCode::SERVICE_UNAVAILABLE,
        "cache-unavailable",
        "the server cannot read its records right now",
    )
}

impl Public {
    fn enabled(&self) -> Result<crate::settings::Settings, Reject> {
        let settings = self.settings.current();
        if !settings.public_api {
            return Err(Reject(
                StatusCode::NOT_FOUND,
                "disabled",
                "this server does not publish repository state",
            ));
        }
        Ok(settings)
    }
}
fn shown(row: &Row, settings: &crate::settings::Settings) -> bool {
    !row.private && settings.allows(row.installation, &row.repo)
}
/// The fields every view of a repository starts with.
fn base(row: &Row) -> serde_json::Map<String, Value> {
    let mut fields = serde_json::Map::new();
    fields.insert("repository".into(), json!(row.repo));
    fields.insert(
        "ranAt".into(),
        json!(
            row.ran_at
                .and_then(|at| DateTime::from_timestamp(at, 0))
                .map(|at| at.to_rfc3339())
        ),
    );
    fields.insert(
        "queued".into(),
        match row
            .queued_at
            .and_then(DateTime::<Utc>::from_timestamp_millis)
        {
            Some(at) => json!({"at": at.to_rfc3339(), "running": row.running}),
            None => Value::Null,
        },
    );
    fields
}
fn merge(mut fields: serde_json::Map<String, Value>, json: Option<&[u8]>) -> Value {
    if let Some(Value::Object(stored)) = json.and_then(|j| serde_json::from_slice(j).ok()) {
        for (key, value) in stored {
            fields.entry(key).or_insert(value);
        }
    }
    Value::Object(fields)
}

async fn list(State(public): State<Public>) -> Result<Json<Value>, Reject> {
    let settings = public.enabled()?;
    let rows = public
        .cache
        .repositories()
        .await
        .map_err(|_| unavailable())?;
    let repositories: Vec<_> = rows
        .iter()
        .filter(|row| shown(row, &settings))
        .map(|row| merge(base(row), row.summary.as_deref()))
        .collect();
    Ok(Json(json!({"repositories": repositories})))
}

async fn show(
    State(public): State<Public>,
    Path((owner, repo)): Path<(String, String)>,
) -> Result<Json<Value>, Reject> {
    let settings = public.enabled()?;
    let name = format!("{owner}/{repo}").to_ascii_lowercase();
    let row = public
        .cache
        .repository(&name)
        .await
        .map_err(|_| unavailable())?
        .filter(|row| shown(row, &settings))
        .ok_or_else(unknown)?;
    Ok(Json(merge(base(&row), row.snapshot.as_deref())))
}

#[cfg(test)]
mod tests;
