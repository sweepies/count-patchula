use crate::{Error, Response, Result, text};
use serde::Serialize;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::{Mutex, MutexGuard, Semaphore, SemaphorePermit},
    time::Instant,
};

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct PrimaryLimit {
    pub limit: Option<u64>,
    pub remaining: Option<u64>,
    pub used: Option<u64>,
    pub reset_at: Option<u64>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Stats {
    pub requests: u64,
    pub not_modified: u64,
    pub retries: u64,
    /// Summed across tasks: semaphore queueing plus primary/secondary waits.
    pub wait_millis: u64,
    pub primary: BTreeMap<String, PrimaryLimit>,
}

#[derive(Default)]
struct Bucket {
    report: PrimaryLimit,
    available: Option<u64>,
    reset: Option<Instant>,
}

#[derive(Default)]
struct State {
    primary: BTreeMap<String, Bucket>,
    secondary_until: Option<Instant>,
}

#[derive(Default)]
struct Content {
    next: Option<Instant>,
    hour: VecDeque<Instant>,
}
pub(super) struct ContentPermit<'a>(MutexGuard<'a, Content>);
impl Drop for ContentPermit<'_> {
    fn drop(&mut self) {
        self.0.next = Some(Instant::now() + Duration::from_secs(1));
    }
}

pub(super) struct Governor {
    semaphore: Semaphore,
    content: Mutex<Content>,
    state: Mutex<State>,
    max_wait: Duration,
    requests: AtomicU64,
    not_modified: AtomicU64,
    retries: AtomicU64,
    wait_millis: AtomicU64,
}

impl Governor {
    pub fn new(concurrency: usize, max_wait: Duration) -> Self {
        Self {
            semaphore: Semaphore::new(concurrency),
            content: Mutex::new(Content::default()),
            state: Mutex::new(State::default()),
            max_wait,
            requests: AtomicU64::new(0),
            not_modified: AtomicU64::new(0),
            retries: AtomicU64::new(0),
            wait_millis: AtomicU64::new(0),
        }
    }

    /// Serialize REST mutations and GraphQL mutations across all client clones.
    /// GitHub recommends a one-second pause after mutations; also reserve the
    /// documented 500 content requests/hour budget (one-second spacing <80/min).
    pub async fn content(&self) -> Result<ContentPermit<'_>> {
        let started = Instant::now();
        let mut state = self.content.lock().await;
        let now = Instant::now();
        while state
            .hour
            .front()
            .is_some_and(|t| now.duration_since(*t) >= Duration::from_secs(3600))
        {
            state.hour.pop_front();
        }
        let until = state
            .next
            .into_iter()
            .chain(
                (state.hour.len() >= 500)
                    .then(|| *state.hour.front().expect("full hour") + Duration::from_secs(3600)),
            )
            .max();
        if let Some(until) = until {
            if until.saturating_duration_since(now) > self.max_wait {
                return Err(Error::RateLimit);
            }
            tokio::time::sleep_until(until).await;
        }
        state.hour.push_back(Instant::now());
        self.wait_millis.fetch_add(
            started.elapsed().as_millis().min(u64::MAX as u128) as u64,
            Ordering::Relaxed,
        );
        Ok(ContentPermit(state))
    }

    pub fn request(&self) {
        self.requests.fetch_add(1, Ordering::Relaxed);
    }
    pub fn not_modified(&self) {
        self.not_modified.fetch_add(1, Ordering::Relaxed);
    }
    pub fn retry(&self) {
        self.retries.fetch_add(1, Ordering::Relaxed);
    }

    pub async fn stats(&self) -> Stats {
        Stats {
            requests: self.requests.load(Ordering::Relaxed),
            not_modified: self.not_modified.load(Ordering::Relaxed),
            retries: self.retries.load(Ordering::Relaxed),
            wait_millis: self.wait_millis.load(Ordering::Relaxed),
            primary: self
                .state
                .lock()
                .await
                .primary
                .iter()
                .map(|(name, bucket)| (name.clone(), bucket.report.clone()))
                .collect(),
        }
    }

    pub async fn acquire(&self, resource: &str) -> Result<SemaphorePermit<'_>> {
        let started = Instant::now();
        loop {
            let permit = self
                .semaphore
                .acquire()
                .await
                .map_err(|_| Error::Configuration)?;
            let mut state = self.state.lock().await;
            let now = Instant::now();
            let mut until = state.secondary_until.filter(|until| *until > now);
            if let Some(bucket) = state.primary.get_mut(resource) {
                if bucket.reset.is_some_and(|reset| reset <= now) {
                    bucket.available = None;
                    bucket.reset = None;
                }
                if bucket.available == Some(0) {
                    // Missing reset header: wait conservatively, then probe once.
                    let reset = *bucket.reset.get_or_insert(now + Duration::from_secs(60));
                    until = Some(until.map_or(reset, |until| until.max(reset)));
                }
            }
            if let Some(until) = until {
                let delay = until.saturating_duration_since(now);
                if delay > self.max_wait {
                    return Err(Error::RateLimit);
                }
                drop(state);
                drop(permit);
                tokio::time::sleep_until(until).await;
                continue;
            }
            if let Some(available) = state
                .primary
                .get_mut(resource)
                .and_then(|bucket| bucket.available.as_mut())
            {
                *available = available.saturating_sub(1);
            }
            let waited = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
            self.wait_millis.fetch_add(waited, Ordering::Relaxed);
            return Ok(permit);
        }
    }

    pub async fn observe(&self, resource: &str, response: &Response) {
        let reported = PrimaryLimit {
            limit: number(response, "x-ratelimit-limit"),
            remaining: number(response, "x-ratelimit-remaining"),
            used: number(response, "x-ratelimit-used"),
            reset_at: number(response, "x-ratelimit-reset"),
        };
        if reported == PrimaryLimit::default() {
            return;
        }
        let resource = text(response, "x-ratelimit-resource").unwrap_or(resource);
        let mut state = self.state.lock().await;
        let bucket = state.primary.entry(resource.to_owned()).or_default();
        let same_window =
            reported.reset_at.is_some() && bucket.report.reset_at == reported.reset_at;
        let old_remaining = bucket.available.map(|remaining| {
            if response.status == reqwest::StatusCode::NOT_MODIFIED {
                remaining.saturating_add(1)
            } else {
                remaining
            }
        });
        if let Some(reset) = reported.reset_at {
            let delay =
                Duration::from_secs(reset.saturating_sub(epoch_seconds()).saturating_add(1));
            bucket.reset = Instant::now().checked_add(delay);
        }
        // Out-of-order in-flight responses must not replenish this window's reservations.
        bucket.available = match (same_window, old_remaining, reported.remaining) {
            (true, Some(old), Some(new)) => Some(old.min(new)),
            (_, _, Some(new)) => Some(new),
            (_, old, None) => old,
        };
        bucket.report = PrimaryLimit {
            limit: reported.limit.or(bucket.report.limit),
            remaining: reported.remaining.or(bucket.report.remaining),
            used: reported.used.or(bucket.report.used),
            reset_at: reported.reset_at.or(bucket.report.reset_at),
        };
    }

    pub async fn rate_limited(
        &self,
        resource: &str,
        response: &Response,
        attempt: u32,
    ) -> Result<bool> {
        let status = response.status.as_u16();
        let rate_status = matches!(status, 403 | 429);
        let retry_after = text(response, "retry-after").and_then(|value| {
            value
                .parse::<u64>()
                .ok()
                .map(Duration::from_secs)
                .or_else(|| {
                    httpdate::parse_http_date(value)
                        .ok()
                        .map(|date| date.duration_since(SystemTime::now()).unwrap_or_default())
                })
        });
        let exhausted = rate_status && number(response, "x-ratelimit-remaining") == Some(0);
        // Inspect only to classify; never attach this data to an error or log.
        let json: serde_json::Value = serde_json::from_slice(&response.body).unwrap_or_default();
        let message = json
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_ascii_lowercase();
        let documented_secondary = message.contains("secondary rate limit")
            || message.contains("abuse detection")
            || json
                .get("documentation_url")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|url| url.contains("secondary-rate-limits"));
        let graphql_limit = status == 200
            && json
                .get("errors")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|errors| {
                    errors.iter().any(|error| {
                        error.get("type").and_then(serde_json::Value::as_str)
                            == Some("RATE_LIMITED")
                            || error
                                .get("message")
                                .and_then(serde_json::Value::as_str)
                                .is_some_and(|message| {
                                    message
                                        .to_ascii_lowercase()
                                        .contains("secondary rate limit")
                                })
                    })
                });
        let secondary = rate_status
            && (status == 429 || retry_after.is_some() || documented_secondary)
            || graphql_limit;
        if !exhausted && !secondary {
            return Ok(false);
        }
        let primary_delay = exhausted
            .then(|| {
                number(response, "x-ratelimit-reset").map(|reset| {
                    Duration::from_secs(reset.saturating_sub(epoch_seconds()).saturating_add(1))
                })
            })
            .flatten();
        let fallback = Duration::from_secs(60 * (1_u64 << attempt.min(10)));
        let delay = retry_after
            .into_iter()
            .chain(primary_delay)
            .max()
            .unwrap_or(fallback);
        let until = Instant::now().checked_add(delay).ok_or(Error::RateLimit)?;
        let mut state = self.state.lock().await;
        if secondary {
            state.secondary_until = Some(state.secondary_until.map_or(until, |old| old.max(until)));
        } else {
            let resource = text(response, "x-ratelimit-resource").unwrap_or(resource);
            let bucket = state.primary.entry(resource.into()).or_default();
            bucket.available = Some(0);
            bucket.reset = Some(bucket.reset.map_or(until, |old| old.max(until)));
        }
        if delay > self.max_wait {
            return Err(Error::RateLimit);
        }
        Ok(true)
    }
}

fn number(response: &Response, name: &str) -> Option<u64> {
    text(response, name)?.parse().ok()
}
fn epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
