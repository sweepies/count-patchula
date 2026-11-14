//! Verify the raw, bounded body before any JSON decoding or delivery processing.
use crate::reload::Reload;
use axum::{
    Json,
    body::Bytes,
    extract::{Request, State, rejection::BytesRejection},
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use hmac::{Hmac, KeyInit, Mac};
use patchula_cache::{
    Cache,
    queue::{Priority, Work},
};
use patchula_config::settings::{Settings, Triggers};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::Sha256;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

pub const BODY_LIMIT: usize = 2 * 1024 * 1024;

/// The fields of a webhook payload the server reads, typed. Everything else
/// is ignored, and a missing field reads as `None`, the same as the key
/// being absent.
#[derive(Deserialize)]
pub(crate) struct Payload {
    pub action: Option<String>,
    pub installation: Option<Installation>,
    pub repository: Option<Repository>,
    pub sender: Option<Actor>,
    pub organization: Option<Actor>,
    pub issue: Option<Issue>,
    /// `None` too when the object is not a whole pull request: such a PR
    /// is never the Count's.
    #[serde(default, deserialize_with = "pull_request")]
    pub pull_request: Option<patchula_github::model::PullRequest>,
    pub changes: Option<Changes>,
    pub check_suite: Option<Checks>,
    pub check_run: Option<Checks>,
    pub repositories: Option<Vec<Repository>>,
    pub repositories_added: Option<Vec<Repository>>,
    pub repositories_removed: Option<Vec<Repository>>,
    pub deleted: Option<bool>,
    #[serde(rename = "ref")]
    pub reference: Option<String>,
}
impl Payload {
    /// Whether the bot itself sent the event; logins compare without case.
    fn from(&self, bot: &str) -> bool {
        self.sender
            .as_ref()
            .and_then(|sender| sender.login.as_deref())
            .is_some_and(|login| login.eq_ignore_ascii_case(bot))
    }
    /// The event's installation, when the settings allow it.
    fn allowed_installation(&self, settings: &Settings) -> Option<u64> {
        self.installation
            .as_ref()
            .and_then(|installation| installation.id)
            .filter(|id| settings.installation_allowed(*id))
    }
}
/// Whether `event`'s `action` gives the App repositories: a new
/// installation, or repositories added to one.
fn adds_repositories(event: &str, action: &str) -> bool {
    (event == "installation" && action == "created")
        || (event == "installation_repositories" && action == "added")
}
/// A payload's `pull_request`, or `None` when it does not decode.
fn pull_request<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<patchula_github::model::PullRequest>, D::Error> {
    Ok(Option::<Value>::deserialize(deserializer)?.and_then(|pr| serde_json::from_value(pr).ok()))
}
#[derive(Deserialize)]
pub(crate) struct Installation {
    pub id: Option<u64>,
}
#[derive(Deserialize)]
pub(crate) struct Actor {
    pub login: Option<String>,
}
#[derive(Deserialize)]
pub(crate) struct Repository {
    pub full_name: Option<String>,
    pub private: Option<bool>,
    pub id: Option<u64>,
    pub default_branch: Option<String>,
}
#[derive(Deserialize)]
pub(crate) struct Issue {
    pub body: Option<String>,
    pub user: Option<Actor>,
}
#[derive(Deserialize)]
pub(crate) struct Changes {
    pub body: Option<BodyChange>,
    pub repository: Option<RepositoryChange>,
}
#[derive(Deserialize)]
pub(crate) struct BodyChange {
    pub from: Option<String>,
}
#[derive(Deserialize)]
pub(crate) struct RepositoryChange {
    pub name: Option<NameChange>,
}
#[derive(Deserialize)]
pub(crate) struct NameChange {
    pub from: Option<String>,
}
#[derive(Deserialize)]
pub(crate) struct Checks {
    pub pull_requests: Option<Vec<CheckPull>>,
}
#[derive(Deserialize)]
pub(crate) struct CheckPull {
    pub number: Option<u64>,
    pub head: Option<CheckSide>,
    pub base: Option<CheckSide>,
}
#[derive(Deserialize)]
pub(crate) struct CheckSide {
    pub repo: Option<CheckRepo>,
}
#[derive(Deserialize)]
pub(crate) struct CheckRepo {
    pub full_name: Option<String>,
    pub id: Option<u64>,
}

#[derive(Clone)]
struct Failure {
    kind: &'static str,
    event: Option<String>,
    delivery: Option<String>,
    installation: Option<u64>,
    repository: Option<String>,
}
impl Failure {
    fn new(kind: &'static str) -> Self {
        Self {
            kind,
            event: None,
            delivery: None,
            installation: None,
            repository: None,
        }
    }
    fn delivery(mut self, event: &str, delivery: &str, payload: &Payload) -> Self {
        self.event = Some(event.into());
        self.delivery = Some(delivery.into());
        self.installation = payload.installation.as_ref().and_then(|i| i.id);
        self.repository = payload
            .repository
            .as_ref()
            .and_then(|repo| repo.full_name.as_deref())
            .filter(|repo| patchula_config::settings::valid_repo(repo))
            .map(str::to_owned);
        self
    }
    fn response(self, status: StatusCode) -> Response {
        let mut response = (status, Json(json!({"error_kind": self.kind}))).into_response();
        response.extensions_mut().insert(self);
        response
    }
}

/// Observe the final response, including framework routing/rejection paths that
/// never reach `receive`. Only allowlisted diagnostics cross this boundary.
pub(crate) async fn observe(request: Request, next: Next) -> Response {
    let response = next.run(request).await;
    let status = response.status();
    if status.is_success() {
        return response;
    }
    let failure = response
        .extensions()
        .get::<Failure>()
        .cloned()
        .unwrap_or_else(|| {
            Failure::new(match status {
                StatusCode::NOT_FOUND => "route-not-found",
                StatusCode::METHOD_NOT_ALLOWED => "method-not-allowed",
                StatusCode::INTERNAL_SERVER_ERROR => "internal-error",
                StatusCode::SERVICE_UNAVAILABLE => "not-ready",
                _ => "http-rejection",
            })
        });
    tracing::warn!(target: "patchula_server", status = status.as_u16(), error_kind = failure.kind,
        event = failure.event.as_deref(), delivery_id = failure.delivery.as_deref(),
        installation = failure.installation, repository = failure.repository.as_deref(),
        outcome = "rejected", "request rejected");
    let mut rejected = failure.response(status);
    if let Some(allow) = response.headers().get(axum::http::header::ALLOW) {
        rejected
            .headers_mut()
            .insert(axum::http::header::ALLOW, allow.clone());
    }
    rejected
}
#[derive(Clone)]
pub struct Webhook {
    pub cache: Cache,
    pub settings: Reload,
    pub bot_login: String,
    pub accepting: Arc<AtomicBool>,
    secret: Arc<Vec<u8>>,
    resolver: Option<Arc<crate::installations::Resolver>>,
    pub(crate) api: Option<crate::remote::Api>,
}
impl Webhook {
    pub fn new(cache: Cache, settings: Reload, bot_login: String, secret: Vec<u8>) -> Self {
        Self {
            cache,
            settings,
            bot_login,
            secret: Arc::new(secret),
            accepting: Arc::new(AtomicBool::new(true)),
            resolver: None,
            api: None,
        }
    }
    pub(crate) fn with_api(mut self, api: crate::remote::Api) -> Self {
        self.api = Some(api);
        self
    }
    pub fn with_app(mut self, app: Arc<patchula_github::app::App>) -> Self {
        self.resolver = Some(Arc::new(crate::installations::Resolver::new(
            app,
            self.cache.clone(),
        )));
        self
    }
}
pub fn verify(secret: &[u8], signature: &str, body: &[u8]) -> bool {
    let Some(hex) = signature.strip_prefix("sha256=").filter(|s| s.len() == 64) else {
        return false;
    };
    let mut expected = [0u8; 32];
    for (index, pair) in hex.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        let digit = |b: u8| match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            _ => None,
        };
        let (Some(hi), Some(lo)) = (digit(pair[0]), digit(pair[1])) else {
            return false;
        };
        expected[index] = (hi << 4) | lo;
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC accepts any key size");
    mac.update(body);
    mac.verify_slice(&expected).is_ok()
}
fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?.to_str().ok()?;
    values.next().is_none().then_some(value)
}
pub async fn receive(
    State(state): State<Webhook>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let body = match body {
        Ok(body) => body,
        Err(error) => {
            return Failure::new(if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
                "body-too-large"
            } else {
                "body-read-failed"
            })
            .response(error.status());
        }
    };
    if !state.accepting.load(Ordering::Acquire) {
        return Failure::new("draining").response(StatusCode::SERVICE_UNAVAILABLE);
    }
    if !header(&headers, "x-hub-signature-256")
        .is_some_and(|signature| verify(&state.secret, signature, &body))
    {
        return Failure::new("bad-signature").response(StatusCode::UNAUTHORIZED);
    }
    let Some(delivery) = header(&headers, "x-github-delivery").filter(|s| {
        !s.is_empty() && s.len() <= 128 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    }) else {
        return Failure::new("invalid-delivery-id").response(StatusCode::BAD_REQUEST);
    };
    let Some(event) = header(&headers, "x-github-event").filter(|s| metadata_word(s)) else {
        return Failure::new("invalid-event").response(StatusCode::BAD_REQUEST);
    };
    let Ok(value) = serde_json::from_slice::<Value>(&body) else {
        return Failure::new("invalid-json").response(StatusCode::BAD_REQUEST);
    };
    if !value.is_object() {
        return Failure::new("invalid-payload").response(StatusCode::BAD_REQUEST);
    }
    let Ok(mut payload) = serde_json::from_value::<Payload>(value) else {
        return Failure::new("invalid-payload").response(StatusCode::BAD_REQUEST);
    };
    if event == "ping" {
        log_delivery(event, delivery, &payload, "accepted", "ping", None);
        return StatusCode::OK.into_response();
    }
    let mut resolution = None;
    if payload.installation.is_none() && !payload.from(&state.bot_login) {
        let resolved = match &state.resolver {
            Some(resolver) => resolver.resolve(&payload).await,
            None => Ok(None),
        };
        match resolved {
            Ok(Some(id)) => {
                payload.installation = Some(Installation { id: Some(id) });
                resolution = Some("installation-resolved");
            }
            Ok(None) => resolution = Some("installation-not-found"),
            Err(kind) => {
                return Failure::new(kind)
                    .delivery(event, delivery, &payload)
                    .response(StatusCode::SERVICE_UNAVAILABLE);
            }
        }
    }
    let settings = state.settings.current();
    let work = classify(event, &payload, &settings, &state.bot_login);
    let reason = if work.is_empty() {
        if resolution == Some("installation-not-found") {
            "installation-not-found"
        } else {
            ignored_reason(event, &payload, &settings, &state.bot_login)
        }
    } else if resolution.is_some() {
        "installation-resolved"
    } else if work.iter().any(|w| w.repo.is_empty()) {
        "installation discovery"
    } else {
        "repository trigger"
    };
    if let Err(error) = track(&state.cache, event, &payload, &settings).await {
        return Failure::new(error.kind())
            .delivery(event, delivery, &payload)
            .response(StatusCode::SERVICE_UNAVAILABLE);
    }
    let enqueued = !work.is_empty();
    let result = state
        .cache
        .enqueue(
            Some(delivery.into()),
            work,
            chrono::Utc::now().timestamp_millis(),
            settings.server.coalescing_ms as i64,
        )
        .await;
    let outcome = match result {
        Ok(false) => "duplicate delivery ignored",
        Ok(true) if enqueued => "accepted/enqueued",
        Ok(true) => "accepted/ignored",
        Err(error) => {
            return Failure::new(error.kind())
                .delivery(event, delivery, &payload)
                .response(StatusCode::SERVICE_UNAVAILABLE);
        }
    };
    log_delivery(event, delivery, &payload, outcome, reason, resolution);
    StatusCode::ACCEPTED.into_response()
}
/// Keeps the repositories behind the public API current, whether or not the
/// delivery starts a batch: installations add and remove them, and every
/// repository payload carries the repository's visibility.
pub(crate) async fn track(
    cache: &Cache,
    event: &str,
    payload: &Payload,
    settings: &Settings,
) -> patchula_cache::Result<()> {
    let Some(id) = payload.installation.as_ref().and_then(|i| i.id) else {
        return Ok(());
    };
    let name = |repo: &Repository| repo.full_name.as_deref().map(str::to_ascii_lowercase);
    let listed = |repos: &[Repository]| -> Vec<(String, bool)> {
        repos
            .iter()
            // Unknown visibility counts as private, which the public API hides.
            .filter_map(|repo| Some((name(repo)?, repo.private.unwrap_or(true))))
            .filter(|(repo, _)| settings.allows(id, repo))
            .collect()
    };
    let repository = payload.repository.as_ref();
    match (event, payload.action.as_deref().unwrap_or("")) {
        ("installation", "created") => {
            cache
                .repositories_added(id, listed(payload.repositories.as_deref().unwrap_or(&[])))
                .await?
        }
        ("installation_repositories", "added") => {
            cache
                .repositories_added(
                    id,
                    listed(payload.repositories_added.as_deref().unwrap_or(&[])),
                )
                .await?
        }
        ("installation_repositories", "removed") => {
            let removed = payload
                .repositories_removed
                .iter()
                .flatten()
                .filter_map(name)
                .collect();
            cache.repositories_removed(removed).await?
        }
        // The payload names the repository as it is now. A rename names the
        // old one too; after a transfer, discovery drops the old name.
        ("repository", action @ ("renamed" | "transferred")) => {
            let old = payload
                .changes
                .as_ref()
                .and_then(|changes| changes.repository.as_ref())
                .and_then(|change| change.name.as_ref())
                .and_then(|name| name.from.as_deref())
                .zip(repository.and_then(name))
                .filter(|_| action == "renamed")
                // A change of case only is the same repository.
                .filter(|(from, repo)| {
                    repo.split_once('/')
                        .is_none_or(|(_, now)| !now.eq_ignore_ascii_case(from))
                })
                .and_then(|(from, repo)| Some(format!("{}/{from}", repo.split_once('/')?.0)));
            if let Some(old) = old {
                cache
                    .repositories_removed(vec![old.to_ascii_lowercase()])
                    .await?;
            }
            if let Some(repo) = repository
                .and_then(name)
                .filter(|repo| settings.allows(id, repo))
            {
                let private = payload
                    .repository
                    .as_ref()
                    .and_then(|repo| repo.private)
                    .unwrap_or(true);
                cache.repositories_added(id, vec![(repo, private)]).await?;
            }
        }
        _ => {}
    }
    if let (Some(repo), Some(private)) = (
        repository.and_then(name),
        repository.and_then(|repo| repo.private),
    ) {
        cache.repository_visibility(&repo, private).await?;
    }
    Ok(())
}
fn log_delivery(
    event: &str,
    delivery: &str,
    payload: &Payload,
    outcome: &str,
    reason: &str,
    resolution: Option<&str>,
) {
    tracing::info!(target: "patchula_server", event, action = payload.action.as_deref().filter(|s| metadata_word(s)).unwrap_or(""), delivery_id = delivery, installation = payload.installation.as_ref().and_then(|i| i.id), repository = payload.repository.as_ref().and_then(|repo| repo.full_name.as_deref()).filter(|s| patchula_config::settings::valid_repo(s)), outcome, reason, installation_resolution = resolution, "webhook received");
}
fn metadata_word(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
}
/// The configured switch a delivery's event and action answer to.
/// `classify` and `ignored_reason` share this table, so a trigger added for
/// one cannot be missing from the other.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Trigger {
    Push,
    Dashboard,
    PullRequestEdited,
    PullRequestClosed,
    Installation,
    Checks,
}

impl Trigger {
    /// The switch `event`/`action` belongs to, or `None` for a delivery no
    /// trigger claims. The mapping is deliberately coarse: `classify` still
    /// checks the finer conditions — a completed check, an edited issue, an
    /// owned pull request — before it enqueues work.
    fn of(event: &str, action: &str) -> Option<Self> {
        Some(match (event, action) {
            ("push", _) => Self::Push,
            ("issues", _) => Self::Dashboard,
            ("pull_request", "closed") => Self::PullRequestClosed,
            ("pull_request", _) => Self::PullRequestEdited,
            ("installation", _) | ("installation_repositories", _) => Self::Installation,
            ("check_suite", _) | ("check_run", _) => Self::Checks,
            _ => return None,
        })
    }

    fn enabled(self, triggers: &Triggers) -> bool {
        match self {
            Self::Push => triggers.push,
            Self::Dashboard => triggers.dashboard,
            Self::PullRequestEdited => triggers.pull_request_edited,
            Self::PullRequestClosed => triggers.pull_request_closed,
            Self::Installation => triggers.installation,
            Self::Checks => triggers.checks,
        }
    }
}

pub(crate) fn ignored_reason(
    event: &str,
    payload: &Payload,
    settings: &Settings,
    bot: &str,
) -> &'static str {
    if payload.from(bot) {
        return "own-bot ignored";
    }
    let Some(id) = payload.allowed_installation(settings) else {
        return "not allowlisted";
    };
    let Some(trigger) = Trigger::of(event, payload.action.as_deref().unwrap_or("")) else {
        return "unrelated event or action";
    };
    if !trigger.enabled(&settings.server.triggers) {
        return "trigger disabled";
    }
    if matches!(event, "installation" | "installation_repositories") {
        return if adds_repositories(event, payload.action.as_deref().unwrap_or("")) {
            "not allowlisted"
        } else {
            "unrelated event or action"
        };
    }
    if !payload
        .repository
        .as_ref()
        .and_then(|repo| repo.full_name.as_deref())
        .is_some_and(|repo| settings.allows(id, repo))
    {
        return "not allowlisted";
    }
    "unrelated event or action"
}
pub(crate) fn classify(
    event: &str,
    payload: &Payload,
    settings: &Settings,
    bot: &str,
) -> Vec<Work> {
    if payload.from(bot) {
        return Vec::new();
    }
    let Some(id) = payload.allowed_installation(settings) else {
        return Vec::new();
    };
    let action = payload.action.as_deref().unwrap_or("");
    if Trigger::of(event, action) == Some(Trigger::Installation)
        && Trigger::Installation.enabled(&settings.server.triggers)
        && adds_repositories(event, action)
    {
        let repos = if event == "installation" {
            &payload.repositories
        } else {
            &payload.repositories_added
        };
        if let Some(repos) = repos.as_ref().filter(|r| !r.is_empty()) {
            return repos
                .iter()
                .filter_map(|r| r.full_name.as_deref())
                .filter(|repo| settings.allows(id, repo))
                .map(|repo| Work::repository(id, repo.to_ascii_lowercase()))
                .collect();
        }
        // Discovery is durable too; repository enumeration runs in the worker.
        return vec![Work::repository(id, String::new())];
    }
    let repository = payload.repository.as_ref();
    let Some(repo) = repository
        .and_then(|repo| repo.full_name.as_deref())
        .filter(|repo| settings.allows(id, repo))
    else {
        return Vec::new();
    };
    if Trigger::of(event, action) == Some(Trigger::Checks)
        && Trigger::Checks.enabled(&settings.server.triggers)
        && action == "completed"
    {
        let checks = if event == "check_suite" {
            &payload.check_suite
        } else {
            &payload.check_run
        };
        // Abbreviated GitHub check records omit author/body and may use a custom
        // configured branch prefix. Persist candidates; ownership is checked by
        // the worker without delaying the delivery response.
        let same = |side: Option<&CheckSide>| {
            side.and_then(|side| side.repo.as_ref())
                .is_some_and(|side| {
                    side.full_name
                        .as_deref()
                        .is_some_and(|r| patchula_update::pull_requests::same_repository(r, repo))
                        || (side.id.is_some() && side.id == repository.and_then(|repo| repo.id))
                })
        };
        let checks = checks
            .as_ref()
            .and_then(|checks| checks.pull_requests.as_deref())
            .unwrap_or(&[])
            .iter()
            .filter(|pr| same(pr.head.as_ref()) && same(pr.base.as_ref()))
            .filter_map(|pr| pr.number.filter(|number| *number > 0))
            .collect::<std::collections::BTreeSet<_>>();
        return if checks.is_empty() {
            Vec::new()
        } else {
            vec![Work {
                installation: id,
                repo: repo.to_ascii_lowercase(),
                check_prs: Some(checks),
                priority: Priority::Check,
            }]
        };
    }
    let before = payload
        .changes
        .as_ref()
        .and_then(|changes| changes.body.as_ref())
        .and_then(|body| body.from.as_deref());
    let issue = payload.issue.as_ref();
    let pr = payload.pull_request.as_ref();
    let owned = pr.is_some_and(|pr| patchula_update::pull_requests::owned_pr(pr, bot, repo));
    let trigger = match Trigger::of(event, action) {
        Some(t) if t.enabled(&settings.server.triggers) => match t {
            Trigger::Push => {
                let branch = repository.and_then(|repo| repo.default_branch.as_deref());
                !payload.deleted.unwrap_or(false)
                    && branch.is_some_and(|b| {
                        payload.reference.as_deref() == Some(format!("refs/heads/{b}").as_str())
                    })
            }
            Trigger::Dashboard => {
                let after = issue.and_then(|issue| issue.body.as_deref()).unwrap_or("");
                action == "edited"
                    && issue
                        .and_then(|issue| issue.user.as_ref())
                        .and_then(|user| user.login.as_deref())
                        .is_some_and(|login| login.eq_ignore_ascii_case(bot))
                    && patchula_update::markers::dashboard(after)
                    && before.is_some_and(|before| {
                        patchula_update::dashboard::Controls::checkbox_edited(before, after)
                    })
            }
            Trigger::PullRequestClosed => owned && action == "closed",
            Trigger::PullRequestEdited => {
                owned
                    && action == "edited"
                    && before.is_some_and(|before| {
                        patchula_update::pull_requests::controls_edited(
                            before,
                            pr.and_then(|pr| pr.body.as_deref()).unwrap_or(""),
                        )
                    })
            }
            _ => false,
        },
        _ => false,
    };
    if trigger {
        vec![Work::person(id, repo.to_ascii_lowercase())]
    } else {
        Vec::new()
    }
}
