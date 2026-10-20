//! Verify the raw, bounded body before any JSON decoding or delivery processing.
use crate::settings::{Reload, Settings};
use axum::{
    Json,
    body::Bytes,
    extract::{Request, State, rejection::BytesRejection},
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use hmac::{Hmac, KeyInit, Mac};
use rustovate_cache::{Cache, queue::Work};
use serde_json::{Value, json};
use sha2::Sha256;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

pub const BODY_LIMIT: usize = 2 * 1024 * 1024;

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
    fn delivery(mut self, event: &str, delivery: &str, payload: &Value) -> Self {
        self.event = Some(event.into());
        self.delivery = Some(delivery.into());
        self.installation = payload["installation"]["id"].as_u64();
        self.repository = payload["repository"]["full_name"]
            .as_str()
            .filter(|repo| crate::settings::valid_repo(repo))
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
    tracing::warn!(target: "rustovate_server", status = status.as_u16(), error_kind = failure.kind,
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
        }
    }
    pub fn with_app(mut self, app: Arc<rustovate_github::app::App>) -> Self {
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
    let Ok(mut payload) = serde_json::from_slice::<Value>(&body) else {
        return Failure::new("invalid-json").response(StatusCode::BAD_REQUEST);
    };
    if !payload.is_object() {
        return Failure::new("invalid-payload").response(StatusCode::BAD_REQUEST);
    }
    if event == "ping" {
        log_delivery(event, delivery, &payload, "accepted", "ping", None);
        return StatusCode::OK.into_response();
    }
    let mut resolution = None;
    if payload["installation"].is_null()
        && !payload["sender"]["login"]
            .as_str()
            .is_some_and(|login| login.eq_ignore_ascii_case(&state.bot_login))
    {
        let resolved = match &state.resolver {
            Some(resolver) => resolver.resolve(&payload).await,
            None => Ok(None),
        };
        match resolved {
            Ok(Some(id)) => {
                payload["installation"] = json!({"id":id});
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
    let enqueued = !work.is_empty();
    let result = state
        .cache
        .enqueue(
            Some(delivery.into()),
            work,
            chrono::Utc::now().timestamp_millis(),
            settings.coalescing_ms as i64,
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
fn log_delivery(
    event: &str,
    delivery: &str,
    payload: &Value,
    outcome: &str,
    reason: &str,
    resolution: Option<&str>,
) {
    tracing::info!(target: "rustovate_server", event, action = payload["action"].as_str().filter(|s| metadata_word(s)).unwrap_or(""), delivery_id = delivery, installation = payload["installation"]["id"].as_u64(), repository = payload["repository"]["full_name"].as_str().filter(|s| crate::settings::valid_repo(s)), outcome, reason, installation_resolution = resolution, "webhook received");
}
fn metadata_word(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
}
pub(crate) fn ignored_reason(
    event: &str,
    payload: &Value,
    settings: &Settings,
    bot: &str,
) -> &'static str {
    if payload["sender"]["login"]
        .as_str()
        .is_some_and(|s| s.eq_ignore_ascii_case(bot))
    {
        return "own-bot ignored";
    }
    let Some(id) = payload["installation"]["id"]
        .as_u64()
        .filter(|id| settings.installation_allowed(*id))
    else {
        return "not allowlisted";
    };
    let enabled = match event {
        "push" => settings.triggers.push,
        "issues" => settings.triggers.dashboard,
        "pull_request" if payload["action"] == "closed" => settings.triggers.pull_request_closed,
        "pull_request" => settings.triggers.pull_request_edited,
        "installation" | "installation_repositories" => settings.triggers.installation,
        "check_suite" | "check_run" => settings.triggers.checks,
        _ => return "unrelated event or action",
    };
    if !enabled {
        return "trigger disabled";
    }
    if matches!(event, "installation" | "installation_repositories") {
        return if (event == "installation" && payload["action"] == "created")
            || (event == "installation_repositories" && payload["action"] == "added")
        {
            "not allowlisted"
        } else {
            "unrelated event or action"
        };
    }
    if !payload["repository"]["full_name"]
        .as_str()
        .is_some_and(|repo| settings.allows(id, repo))
    {
        return "not allowlisted";
    }
    "unrelated event or action"
}
pub fn classify(event: &str, payload: &Value, settings: &Settings, bot: &str) -> Vec<Work> {
    if payload["sender"]["login"]
        .as_str()
        .is_some_and(|s| s.eq_ignore_ascii_case(bot))
    {
        return Vec::new();
    }
    let Some(id) = payload["installation"]["id"]
        .as_u64()
        .filter(|id| settings.installation_allowed(*id))
    else {
        return Vec::new();
    };
    let action = payload["action"].as_str().unwrap_or("");
    if settings.triggers.installation
        && ((event == "installation" && action == "created")
            || (event == "installation_repositories" && action == "added"))
    {
        let key = if event == "installation" {
            "repositories"
        } else {
            "repositories_added"
        };
        if let Some(repos) = payload[key].as_array().filter(|r| !r.is_empty()) {
            return repos
                .iter()
                .filter_map(|r| r["full_name"].as_str())
                .filter(|repo| settings.allows(id, repo))
                .map(|repo| Work::repository(id, repo.to_ascii_lowercase()))
                .collect();
        }
        // Discovery is durable too; repository enumeration runs in the worker.
        return vec![Work::repository(id, String::new())];
    }
    let Some(repo) = payload["repository"]["full_name"]
        .as_str()
        .filter(|repo| settings.allows(id, repo))
    else {
        return Vec::new();
    };
    if matches!(event, "check_suite" | "check_run")
        && settings.triggers.checks
        && action == "completed"
    {
        // Abbreviated GitHub check records omit author/body and may use a custom
        // configured branch prefix. Persist candidates; ownership is checked by
        // the worker without delaying the delivery response.
        let checks = payload[event]["pull_requests"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|pr| {
                let same = |side: &str| {
                    pr[side]["repo"]["full_name"]
                        .as_str()
                        .is_some_and(|r| r.eq_ignore_ascii_case(repo))
                        || (pr[side]["repo"]["id"].as_u64().is_some()
                            && pr[side]["repo"]["id"] == payload["repository"]["id"])
                };
                same("head") && same("base")
            })
            .filter_map(|pr| pr["number"].as_u64().filter(|number| *number > 0))
            .collect::<std::collections::BTreeSet<_>>();
        return if checks.is_empty() {
            Vec::new()
        } else {
            vec![Work {
                installation: id,
                repo: repo.to_ascii_lowercase(),
                check_prs: Some(checks),
            }]
        };
    }
    let before = payload["changes"]["body"]["from"].as_str();
    let issue = &payload["issue"];
    let pr = &payload["pull_request"];
    let owned = rustovate_update::pull_requests::owned_pr(pr, bot, repo);
    let trigger = match event {
        "push" if settings.triggers.push => {
            let branch = payload["repository"]["default_branch"].as_str();
            !payload["deleted"].as_bool().unwrap_or(false)
                && branch.is_some_and(|b| payload["ref"] == format!("refs/heads/{b}"))
        }
        "issues" if settings.triggers.dashboard && action == "edited" => {
            let after = issue["body"].as_str().unwrap_or("");
            issue["user"]["login"] == bot
                && after.contains(rustovate_update::dashboard::MARKER)
                && before.is_some_and(|before| {
                    rustovate_update::dashboard::Controls::checkbox_edited(before, after)
                })
        }
        "pull_request" if owned => {
            (settings.triggers.pull_request_closed && action == "closed")
                || (settings.triggers.pull_request_edited
                    && action == "edited"
                    && before.is_some_and(|before| {
                        rustovate_update::pull_requests::controls_edited(
                            before,
                            pr["body"].as_str().unwrap_or(""),
                        )
                    }))
        }
        _ => false,
    };
    if trigger {
        vec![Work::repository(id, repo.to_ascii_lowercase())]
    } else {
        Vec::new()
    }
}
