//! Verify the raw, bounded body before any JSON decoding or delivery processing.
use crate::settings::{Reload, Settings};
use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
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
pub struct Webhook {
    pub cache: Cache,
    pub settings: Reload,
    pub bot_login: String,
    pub accepting: Arc<AtomicBool>,
    secret: Arc<Vec<u8>>,
}
impl Webhook {
    pub fn new(cache: Cache, settings: Reload, bot_login: String, secret: Vec<u8>) -> Self {
        Self {
            cache,
            settings,
            bot_login,
            secret: Arc::new(secret),
            accepting: Arc::new(AtomicBool::new(true)),
        }
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
pub async fn receive(State(state): State<Webhook>, headers: HeaderMap, body: Bytes) -> StatusCode {
    if !state.accepting.load(Ordering::Acquire) {
        return StatusCode::SERVICE_UNAVAILABLE;
    }
    if !header(&headers, "x-hub-signature-256")
        .is_some_and(|signature| verify(&state.secret, signature, &body))
    {
        return StatusCode::UNAUTHORIZED;
    }
    let Some(delivery) = header(&headers, "x-github-delivery").filter(|s| {
        !s.is_empty() && s.len() <= 128 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    }) else {
        return StatusCode::BAD_REQUEST;
    };
    let Some(event) = header(&headers, "x-github-event") else {
        return StatusCode::BAD_REQUEST;
    };
    let Ok(payload) = serde_json::from_slice::<Value>(&body) else {
        return StatusCode::BAD_REQUEST;
    };
    let settings = state.settings.current();
    let work = classify(event, &payload, &settings, &state.bot_login);
    match state
        .cache
        .enqueue(
            Some(delivery.into()),
            work,
            chrono::Utc::now().timestamp_millis(),
            settings.coalescing_ms as i64,
        )
        .await
    {
        Ok(_) => StatusCode::ACCEPTED,
        Err(_) => {
            tracing::error!(target: "rustovate_server", "webhook could not be persisted");
            StatusCode::SERVICE_UNAVAILABLE
        }
    }
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
        // Discovery is durable too; the webhook path never waits for GitHub API calls.
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
                    && rustovate_update::pull_requests::checked(pr)
                    && before.is_some_and(|before| {
                        !rustovate_update::pull_requests::checked(&json!({"body":before}))
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
