//! The read-only API behind a web dashboard: what the Count last saw of each
//! public repository he runs. It needs no sign-in, so it never shows a private
//! repository or one the settings no longer allow. The server records a
//! repository after each run that writes, and discovery keeps the list and
//! each repository's visibility current.
use crate::reload::Reload;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use chrono::{DateTime, Utc};
use futures_util::{StreamExt, stream::FuturesUnordered};
use patchula_cache::{
    Cache,
    repositories::{Row, Run},
};
use patchula_github::Client;
use patchula_report::{
    BranchStatus, LookupFailure, PackageFile, PullRequestAction, Report, Status,
};
use serde::{Deserialize, Serialize};
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
    lookup_failures: &'a [LookupFailure],
    default_branch: Option<&'a str>,
    commit: Option<&'a str>,
    package_files: &'a [PackageFile],
    branches: Vec<Branch<'a>>,
    /// null when GitHub would not list them.
    pull_requests: Option<Vec<PullRequest<'a>>>,
    dashboard: Option<u64>,
    merged: Vec<Merged>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Branch<'a> {
    branch: &'a str,
    status: BranchStatus,
    reason: Option<&'a str>,
    message: &'a str,
    base_commit: &'a str,
    commit: Option<&'a str>,
    lockfile_pending: bool,
    files: Vec<&'a str>,
}
/// One of the Count's open PRs as GitHub showed it after the run.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PullRequest<'a> {
    number: u64,
    url: Option<String>,
    branch: Option<String>,
    title: Option<String>,
    body: Option<String>,
    draft: bool,
    labels: Vec<String>,
    head_sha: Option<String>,
    base: Option<String>,
    created_at: Option<String>,
    updated_at: Option<String>,
    /// GitHub computes these in the background, so either may be null.
    mergeable: Option<bool>,
    mergeable_state: Option<String>,
    auto_merge: bool,
    /// What the run did to the PR, when it acted on it.
    action: Option<Action<'a>>,
}
#[derive(Serialize)]
struct Action<'a> {
    status: patchula_report::PullRequestStatus,
    reason: Option<&'a str>,
}
/// The pull-request fields the public API publishes, as GitHub sent them.
#[derive(Deserialize)]
struct GithubPull {
    number: Option<u64>,
    html_url: Option<String>,
    title: Option<String>,
    body: Option<String>,
    draft: Option<bool>,
    labels: Option<Vec<GithubLabel>>,
    head: Option<GithubSide>,
    base: Option<GithubSide>,
    created_at: Option<String>,
    updated_at: Option<String>,
    mergeable: Option<bool>,
    mergeable_state: Option<String>,
    auto_merge: Option<Value>,
}
#[derive(Deserialize)]
struct GithubLabel {
    name: Option<String>,
}
#[derive(Deserialize)]
struct GithubSide {
    #[serde(rename = "ref")]
    reference: Option<String>,
    sha: Option<String>,
}
/// The one repository field record() reads.
#[derive(Deserialize)]
struct RepoVisibility {
    private: Option<bool>,
}
impl<'a> PullRequest<'a> {
    fn new(pr: &Value, actions: &[&'a PullRequestAction]) -> Option<Self> {
        let pr = GithubPull::deserialize(pr).ok()?;
        let number = pr.number?;
        Some(Self {
            number,
            url: pr.html_url,
            branch: pr.head.as_ref().and_then(|head| head.reference.clone()),
            title: pr.title,
            body: pr.body,
            draft: pr.draft.unwrap_or(false),
            labels: pr
                .labels
                .iter()
                .flatten()
                .filter_map(|label| label.name.clone())
                .collect(),
            head_sha: pr.head.as_ref().and_then(|head| head.sha.clone()),
            base: pr.base.as_ref().and_then(|base| base.reference.clone()),
            created_at: pr.created_at,
            updated_at: pr.updated_at,
            mergeable: pr.mergeable,
            mergeable_state: pr.mergeable_state,
            auto_merge: pr.auto_merge.is_some(),
            action: actions
                .iter()
                .find(|action| action.number == Some(number))
                .map(|action| Action {
                    status: action.status,
                    reason: action.reason.as_deref(),
                }),
        })
    }
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
    pull_requests: Option<usize>,
    dashboard: Option<u64>,
}

/// Whether GitHub says a repository is in the run's installation.
pub(crate) type Member =
    dyn Fn(String) -> futures_util::future::BoxFuture<'static, bool> + Send + Sync;

/// A run's report for the public API, and the same without what lookups made
/// with registry credentials found. A repository that is private when it runs
/// is stored without them, so it never shows them if it later becomes public;
/// a public one shows them, as its dependency dashboard does.
pub(crate) struct Recorded<'a> {
    pub report: &'a Report,
    pub withheld: Report,
}

/// `report` without what lookups made with registry credentials found. Such a
/// dependency keeps what the repository declares, but loses its updates, the
/// version its range resolved to, held-back releases, lookup evidence, tag
/// digests and image checks. Whether
/// a lookup carried credentials was recorded as its requests were made.
pub(crate) fn withheld(report: &Report) -> Report {
    let mut report = report.clone();
    for repo in &mut report.repositories {
        // Warnings and lookup failures about a dependency found with
        // credentials can quote what the registry answered.
        let names: Vec<String> = repo
            .package_files
            .iter()
            .flat_map(|file| &file.dependencies)
            .filter(|dep| dep.metadata.as_ref().is_some_and(|meta| meta.credentialed))
            .flat_map(|dep| [dep.name.clone(), dep.package.clone()])
            .collect();
        repo.warnings
            .retain(|warning| !names.iter().any(|name| warning.contains(name.as_str())));
        repo.lookup_failures
            .retain(|failure| !names.contains(&failure.package) && !names.contains(&failure.name));
    }
    for dep in report
        .repositories
        .iter_mut()
        .flat_map(|repo| &mut repo.package_files)
        .flat_map(|file| &mut file.dependencies)
    {
        let Some(meta) = dep.metadata.as_mut() else {
            continue;
        };
        if !meta.credentialed {
            continue;
        }
        // Rebuild the metadata field by field, every field named: a field a
        // credentialed lookup could have filled must be classified here —
        // kept as declared, or dropped — or this stops compiling.
        let patchula_report::DependencyMetadata {
            tag_digests: _,
            source,
            kinds,
            locations,
            held_back: _,
            stale: _,
            lookup: _,
            resolution: _,
            advisories: _,
            image: _,
            credentialed,
        } = meta.clone();
        // Image advisories came from SBOMs read with the same credentials.
        *meta = patchula_report::DependencyMetadata {
            tag_digests: Default::default(),
            source,
            kinds,
            locations,
            held_back: Vec::new(),
            stale: Default::default(),
            lookup: None,
            resolution: None,
            advisories: Vec::new(),
            image: None,
            credentialed,
        };
        dep.updates.clear();
        // The version a range resolves to came from the lookup too; every
        // field of the current declaration is classified the same way.
        let patchula_report::Current {
            value,
            version: _,
            digest,
        } = dep.current.clone();
        dep.current = patchula_report::Current {
            value,
            version: None,
            digest,
        };
    }
    report
}

/// Most open PRs read for one repository.
const PULL_PAGES: u32 = 10;
/// Repositories recorded at once, and each repository's pull request reads
/// in flight at once: the record only reads GitHub, so it fans out within
/// bounds.
const RECORD_FANOUT: usize = 4;

/// Records every repository in a writing run's report, for a run that
/// started at `started`. A repository whose visibility cannot be read is
/// recorded as private, so it stays hidden. One the server has not listed
/// yet is added if `member` confirms it belongs to the installation, so a run
/// before the first discovery still shows.
pub(crate) async fn record(
    cache: &Cache,
    client: &Client,
    bot: &str,
    installation: u64,
    recorded: &Recorded<'_>,
    started: DateTime<Utc>,
    member: &Member,
) {
    // `Recorded` borrows the report, so a mapping closure cannot name its
    // lifetimes; the futures go straight into the set instead.
    let mut recording = FuturesUnordered::new();
    for (index, repo) in recorded.report.repositories.iter().enumerate() {
        while recording.len() == RECORD_FANOUT {
            recording.next().await;
        }
        recording.push(record_repository(
            cache,
            client,
            bot,
            installation,
            recorded,
            index,
            repo,
            started,
            member,
        ));
    }
    while recording.next().await.is_some() {}
}

/// Records one repository of a writing run's report.
#[allow(clippy::too_many_arguments)]
async fn record_repository(
    cache: &Cache,
    client: &Client,
    bot: &str,
    installation: u64,
    recorded: &Recorded<'_>,
    index: usize,
    repo: &patchula_report::Repository,
    started: DateTime<Utc>,
    member: &Member,
) {
    let report = recorded.report;
    let actions = report.actions.as_ref();
    let name = repo.repository.as_str();
    let private = match client.get(&format!("repos/{name}")).await {
        Ok(response) => serde_json::from_slice::<RepoVisibility>(&response.body)
            .ok()
            .and_then(|repo| repo.private)
            .unwrap_or(true),
        Err(_) => true,
    };
    let pull_actions: Vec<_> = actions
        .map(|a| a.pull_requests.as_slice())
        .unwrap_or_default()
        .iter()
        .filter(|pr| pr.repository == name)
        .collect();
    let pulls = open_pulls(client, bot, name).await;
    let pull_requests: Option<Vec<_>> = pulls.as_ref().map(|pulls| {
        pulls
            .iter()
            .filter_map(|pr| PullRequest::new(pr, &pull_actions))
            .collect()
    });
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
    let merged = patchula_update::automerge::merged(cache, name, bot)
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(repository = name, %error, "cannot read the auto-merge history");
            Vec::new()
        })
        .iter()
        .filter_map(|pr| {
            Some(Merged {
                number: pr.number?,
                title: pr.title.clone(),
                url: pr.html_url.clone(),
                // A merge the Count made himself has no recorded time.
                merged_at: pr
                    .merged_at
                    .clone()
                    .filter(|at| DateTime::parse_from_rfc3339(at).is_ok()),
            })
        })
        .collect();
    // A private repository's run is stored without what credentials
    // found, its warnings and lookup failures included.
    let shown = if private {
        &recorded.withheld.repositories[index]
    } else {
        repo
    };
    let package_files = &shown.package_files;
    let dependencies = package_files.iter().flat_map(|f| &f.dependencies);
    let summary = Summary {
        status: repo.status,
        reason: repo.reason.as_deref(),
        default_branch: repo.default_branch.as_deref(),
        dependencies: dependencies.clone().count(),
        updates: dependencies.filter(|d| !d.updates.is_empty()).count(),
        pull_requests: pull_requests.as_ref().map(Vec::len),
        dashboard,
    };
    let snapshot = Snapshot {
        repository: name,
        ran_at: started.to_rfc3339(),
        status: repo.status,
        reason: repo.reason.as_deref(),
        warnings: &shown.warnings,
        lookup_failures: &shown.lookup_failures,
        default_branch: repo.default_branch.as_deref(),
        commit: repo.commit.as_deref(),
        package_files,
        branches,
        pull_requests,
        dashboard,
        merged,
    };
    let (Ok(summary), Ok(snapshot)) = (serde_json::to_vec(&summary), serde_json::to_vec(&snapshot))
    else {
        return;
    };
    let run = Run {
        installation,
        repo: name.to_owned(),
        private,
        ran_at: started.timestamp(),
        summary,
        snapshot,
    };
    let known = match cache.repository(name).await {
        Ok(row) => row.is_some_and(|row| row.installation == installation),
        Err(_) => false,
    };
    if !known
        && member(name.to_owned()).await
        && let Err(error) = cache
            .repositories_added(installation, vec![(name.to_owned(), private)])
            .await
    {
        tracing::warn!(target: "patchula_server", repository = name, error_kind = error.kind(),
            "cannot add the repository to the public API");
    }
    if let Err(error) = cache.repository_ran(run).await {
        tracing::warn!(target: "patchula_server", repository = name, error_kind = error.kind(),
            "cannot record the repository for the public API");
    }
}

/// The Count's open PRs in `repo`, each read on its own for its merge state.
/// None when GitHub would not list them.
async fn open_pulls(client: &Client, bot: &str, repo: &str) -> Option<Vec<Value>> {
    let mut owned = Vec::new();
    for page in 1..=PULL_PAGES {
        let response = client
            .get(&format!(
                "repos/{repo}/pulls?state=open&per_page=100&page={page}"
            ))
            .await
            .ok()?;
        let pulls: Vec<Value> = serde_json::from_slice(&response.body).ok()?;
        let full = pulls.len() == 100;
        // Each owned PR, with its number for the read of its own.
        owned.extend(pulls.into_iter().filter_map(|pr| {
            let pull = patchula_github::model::PullRequest::deserialize(&pr).ok()?;
            patchula_update::pull_requests::owned_pr(&pull, bot, repo).then_some((pull.number, pr))
        }));
        if !full {
            break;
        }
    }
    let owned = futures_util::stream::iter(owned)
        .map(|(number, pr)| async move {
            client
                .get(&format!("repos/{repo}/pulls/{number}"))
                .await
                .ok()
                .and_then(|response| serde_json::from_slice::<Value>(&response.body).ok())
                .unwrap_or(pr)
        })
        .buffered(RECORD_FANOUT)
        .collect::<Vec<_>>()
        .await;
    Some(owned)
}

struct Reject(StatusCode, &'static str, &'static str);
impl IntoResponse for Reject {
    fn into_response(self) -> Response {
        (
            self.0,
            Json(crate::remote::protocol::RejectBody {
                error_kind: self.1.into(),
                error: self.2.into(),
            }),
        )
            .into_response()
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
    fn enabled(&self) -> Result<patchula_config::settings::Settings, Reject> {
        let settings = self.settings.current();
        if !settings.server.public_api {
            return Err(Reject(
                StatusCode::NOT_FOUND,
                "disabled",
                "this server does not publish repository state",
            ));
        }
        Ok(settings)
    }
}
fn shown(row: &Row, settings: &patchula_config::settings::Settings) -> bool {
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
