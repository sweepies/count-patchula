use crate::{Error, Response, Result, text};
use patchula_http::conditional;
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
    /// Write capacity is one token until this instant, for the hour after a
    /// secondary-rate-limit answer. It lives here rather than in `Content`
    /// because `rate_limited` reports such an answer while the write it
    /// answered still holds the content permit — nothing locks `state` and
    /// then `content`.
    content_throttled_until: Option<Instant>,
}

/// The write budget: `burst` tokens refilling at one a second, one token per
/// write taken when the write starts.
struct Content {
    tokens: f64,
    refilled_at: Instant,
    burst: usize,
    hour: VecDeque<Instant>,
}

impl Content {
    fn new(burst: usize) -> Self {
        Self {
            tokens: burst as f64,
            refilled_at: Instant::now(),
            burst,
            hour: VecDeque::new(),
        }
    }
    /// Top the bucket up for the time passed, capped at `capacity`; a smaller
    /// capacity also drains what exceeds it.
    fn refill(&mut self, capacity: usize, now: Instant) {
        let accrued = now
            .saturating_duration_since(self.refilled_at)
            .as_secs_f64();
        self.tokens = (self.tokens + accrued).min(capacity as f64);
        self.refilled_at = now;
    }
}
/// Held for the drop: the write stays in flight until the permit drops.
pub(super) struct ContentPermit<'a> {
    _guard: MutexGuard<'a, Content>,
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

/// The window GitHub's hourly write limit counts in.
const HOUR: Duration = Duration::from_secs(3600);
/// GitHub's documented content writes an hour.
const WRITES_PER_HOUR: usize = 500;

impl Governor {
    pub fn new(concurrency: usize, max_wait: Duration, write_burst: usize) -> Self {
        Self {
            semaphore: Semaphore::new(concurrency),
            content: Mutex::new(Content::new(write_burst)),
            state: Mutex::new(State::default()),
            max_wait,
            requests: AtomicU64::new(0),
            not_modified: AtomicU64::new(0),
            retries: AtomicU64::new(0),
            wait_millis: AtomicU64::new(0),
        }
    }

    /// Serialize REST mutations and GraphQL mutations across all client clones,
    /// paced by a token bucket: `burst` tokens refilling one a second, one
    /// token a write counted from the write's start — under the documented 80
    /// content requests a minute — inside the documented [`WRITES_PER_HOUR`]. A
    /// secondary-rate-limit answer cuts the budget to one token for an hour.
    pub async fn content(&self) -> Result<ContentPermit<'_>> {
        let started = Instant::now();
        let mut content = self.content.lock().await;
        let mut now = Instant::now();
        while content
            .hour
            .front()
            .is_some_and(|t| now.duration_since(*t) >= HOUR)
        {
            content.hour.pop_front();
        }
        let mut capacity = self.write_capacity(&content, now).await;
        content.refill(capacity, now);
        let until = (content.tokens < 1.0)
            .then(|| now + Duration::from_secs_f64(1.0 - content.tokens))
            .into_iter()
            .chain(
                (content.hour.len() >= WRITES_PER_HOUR)
                    .then(|| *content.hour.front().expect("full hour") + HOUR),
            )
            .max();
        if let Some(until) = until {
            if until.saturating_duration_since(now) > self.max_wait {
                return Err(Error::RateLimit);
            }
            tokio::time::sleep_until(until).await;
            now = Instant::now();
            capacity = self.write_capacity(&content, now).await;
            content.refill(capacity, now);
        }
        content.tokens = (content.tokens - 1.0).max(0.0);
        content.hour.push_back(now);
        self.add_wait(started);
        Ok(ContentPermit { _guard: content })
    }

    /// Count the time since `started` as waiting.
    fn add_wait(&self, started: Instant) {
        let waited = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.wait_millis.fetch_add(waited, Ordering::Relaxed);
    }

    /// One token while a secondary-rate-limit answer's hour runs, else `burst`.
    async fn write_capacity(&self, content: &Content, now: Instant) -> usize {
        if self
            .state
            .lock()
            .await
            .content_throttled_until
            .is_some_and(|until| until > now)
        {
            1
        } else {
            content.burst
        }
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
                .expect("the governor never closes its semaphore");
            let mut state = self.state.lock().await;
            let now = Instant::now();
            let mut until = state.secondary_until.filter(|until| *until > now);
            if let Some(bucket) = state.primary.get_mut(resource) {
                if bucket.reset.is_some_and(|reset| reset <= now) {
                    bucket.available = None;
                    bucket.reset = None;
                }
                if bucket.available == Some(0) {
                    // Missing reset header: wait a minute. When it passes the
                    // bucket is unknown again and queued requests go ahead; a
                    // still-exhausted answer closes it again.
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
            self.add_wait(started);
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
        // Out-of-order in-flight responses must not replenish this window's reservations.
        bucket.available = match (same_window, old_remaining, reported.remaining) {
            (true, Some(old), Some(new)) => Some(old.min(new)),
            (_, _, Some(new)) => Some(new),
            (_, old, None) => old,
        };
        if let Some(reset) = reported.reset_at {
            let delay =
                Duration::from_secs(reset.saturating_sub(epoch_seconds()).saturating_add(1));
            let next = Instant::now().checked_add(delay);
            // While the bucket is spent its reset only moves later: an earlier
            // header from a response in flight must not cut short a wait a
            // rate-limit answer set.
            bucket.reset = if bucket.available == Some(0) {
                bucket.reset.max(next)
            } else {
                next
            };
        }
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
        let retry_after = conditional::retry_after(response);
        let spent = number(response, "x-ratelimit-remaining") == Some(0);
        let graphql_answer = resource == "graphql" && status == 200;
        // Only a rate-limit status or a GraphQL answer can carry a body that
        // classifies; any other, which may be megabytes, is never parsed. A
        // GraphQL 200 classifies on its top-level `errors` alone, so one
        // whose bytes cannot spell that key is not parsed either. The call
        // runs under the in-flight permit, and a 200 is usually a large
        // blob batch the caller parses anyway.
        // Inspect only to classify; never attach this data to an error or log.
        let parse = rate_status || graphql_answer && may_name_errors(&response.body);
        let body = if parse {
            Classified::parse(&response.body)
        } else {
            Classified::default()
        };
        let message = body
            .message
            .as_ref()
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_ascii_lowercase();
        let documented_secondary = message.contains("secondary rate limit")
            || message.contains("abuse detection")
            || body
                .documentation_url
                .as_ref()
                .and_then(serde_json::Value::as_str)
                .is_some_and(|url| url.contains("secondary-rate-limits"));
        let graphql = GraphqlLimit::classify(graphql_answer, body.errors.as_ref());
        // GitHub answers both GraphQL limits with a 200 and `RATE_LIMITED`.
        // A spent `x-ratelimit-remaining` names the primary window, which
        // ends at `x-ratelimit-reset`; without it, or with an error message
        // naming the secondary limit, the answer is a secondary one.
        let graphql_primary = graphql.limited && spent && !graphql.names_secondary;
        let exhausted = rate_status && spent || graphql_primary;
        let secondary = rate_status
            && (status == 429 || retry_after.is_some() || documented_secondary)
            || graphql.limited && !graphql_primary;
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
            state.content_throttled_until = Some(Instant::now() + HOUR);
            tracing::warn!(
                target: "patchula_github",
                "secondary rate limit: writes pace at one a second for the next hour"
            );
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

/// What a GraphQL 200's `errors` say about rate limits.
struct GraphqlLimit {
    /// Some error is `RATE_LIMITED` or names the secondary limit.
    limited: bool,
    /// Some error's message names the secondary limit.
    names_secondary: bool,
}

impl GraphqlLimit {
    fn classify(graphql_answer: bool, errors: Option<&serde_json::Value>) -> Self {
        let errors = errors
            .and_then(serde_json::Value::as_array)
            .filter(|_| graphql_answer)
            .map_or(&[][..], Vec::as_slice);
        let names_secondary = errors.iter().any(|error| {
            error
                .get("message")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|message| {
                    message
                        .to_ascii_lowercase()
                        .contains("secondary rate limit")
                })
        });
        let typed = errors.iter().any(|error| {
            error.get("type").and_then(serde_json::Value::as_str) == Some("RATE_LIMITED")
        });
        Self {
            limited: typed || names_secondary,
            names_secondary,
        }
    }
}

/// Whether a JSON body could hold an `errors` key. JSON spells a key either
/// literally or with a `\u` escape, so a body with neither has none.
fn may_name_errors(body: &[u8]) -> bool {
    memchr::memmem::find(body, b"errors").is_some() || memchr::memmem::find(body, b"\\u").is_some()
}

/// The top-level fields of a JSON body that classify a rate limit. Every
/// other value is skipped without being built, so a large answer costs a
/// scan rather than a document. A body that is not a JSON object classifies
/// as nothing, as does one that fails to parse; a repeated key keeps its
/// last value.
#[derive(Default)]
struct Classified {
    message: Option<serde_json::Value>,
    documentation_url: Option<serde_json::Value>,
    errors: Option<serde_json::Value>,
}

impl Classified {
    fn parse(body: &[u8]) -> Self {
        serde_json::from_slice(body).unwrap_or_default()
    }
}

impl<'de> serde::Deserialize<'de> for Classified {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        struct Fields;
        impl<'de> serde::de::Visitor<'de> for Fields {
            type Value = Classified;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a JSON object")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Classified, A::Error> {
                let mut fields = Classified::default();
                while let Some(key) = map.next_key::<std::borrow::Cow<'de, str>>()? {
                    let slot = match key.as_ref() {
                        "message" => &mut fields.message,
                        "documentation_url" => &mut fields.documentation_url,
                        "errors" => &mut fields.errors,
                        _ => {
                            map.next_value::<serde::de::IgnoredAny>()?;
                            continue;
                        }
                    };
                    *slot = Some(map.next_value()?);
                }
                Ok(fields)
            }
        }
        deserializer.deserialize_map(Fields)
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

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::{
        StatusCode,
        header::{HeaderMap, HeaderName, HeaderValue},
    };

    fn response(status: u16, headers: &[(&'static str, String)]) -> Response {
        Response {
            status: StatusCode::from_u16(status).unwrap(),
            headers: headers
                .iter()
                .map(|(name, value)| {
                    (
                        HeaderName::from_static(name),
                        HeaderValue::from_str(value).unwrap(),
                    )
                })
                .collect::<HeaderMap>(),
            body: Vec::new(),
            stale: None,
        }
    }

    /// A 200's body classifies only on GraphQL: a REST answer that happens
    /// to hold a GraphQL-shaped error is content, not a rate limit.
    #[tokio::test]
    async fn only_a_graphql_answer_reports_a_limit_in_its_body() {
        let governor = Governor::new(4, Duration::from_secs(3600), 12);
        let mut answer = response(200, &[("retry-after", "1".into())]);
        answer.body = br#"{"errors":[{"type":"RATE_LIMITED","message":"limited"}]}"#.to_vec();
        assert!(!governor.rate_limited("core", &answer, 0).await.unwrap());
        assert!(governor.rate_limited("graphql", &answer, 0).await.unwrap());
    }

    /// A GraphQL `RATE_LIMITED` answer with a spent primary window is the
    /// primary limit: it waits for `x-ratelimit-reset` on the graphql bucket
    /// and leaves writes and the secondary cooldown alone.
    #[tokio::test]
    async fn a_spent_graphql_window_is_the_primary_limit() {
        let governor = Governor::new(4, Duration::from_secs(3600), 12);
        let reset = epoch_seconds() + 600;
        let mut answer = response(
            200,
            &[
                ("x-ratelimit-remaining", "0".into()),
                ("x-ratelimit-reset", reset.to_string()),
                ("x-ratelimit-resource", "graphql".into()),
            ],
        );
        answer.body = br#"{"errors":[{"type":"RATE_LIMITED","message":"API rate limit already exceeded for user ID 1."}]}"#.to_vec();
        assert!(governor.rate_limited("graphql", &answer, 0).await.unwrap());
        let state = governor.state.lock().await;
        assert!(state.secondary_until.is_none());
        assert!(state.content_throttled_until.is_none());
        let bucket = &state.primary["graphql"];
        assert_eq!(bucket.available, Some(0));
        let wait = bucket.reset.unwrap() - Instant::now();
        assert!(wait > Duration::from_secs(590) && wait <= Duration::from_secs(602));
    }

    /// Without a spent window, or with a message naming the secondary
    /// limit, a GraphQL `RATE_LIMITED` answer stays a secondary limit.
    #[tokio::test]
    async fn a_graphql_limit_without_a_spent_window_is_secondary() {
        let reset = (epoch_seconds() + 600).to_string();
        for (headers, message) in [
            (vec![("x-ratelimit-remaining", "12".to_owned())], "limited"),
            (vec![], "limited"),
            (
                vec![
                    ("x-ratelimit-remaining", "0".to_owned()),
                    ("x-ratelimit-reset", reset.clone()),
                ],
                "You have exceeded a secondary rate limit.",
            ),
        ] {
            let governor = Governor::new(4, Duration::from_secs(3600), 12);
            let mut answer = response(200, &headers);
            answer.body = serde_json::to_vec(&serde_json::json!({
                "errors": [{"type": "RATE_LIMITED", "message": message}]
            }))
            .unwrap();
            assert!(governor.rate_limited("graphql", &answer, 0).await.unwrap());
            let state = governor.state.lock().await;
            assert!(state.secondary_until.is_some(), "{headers:?} {message}");
            assert!(state.content_throttled_until.is_some());
            assert!(!state.primary.contains_key("graphql"));
        }
    }

    /// A GraphQL 200 without the bytes of an `errors` key is not parsed; any
    /// body that might spell one, escaped or not, is.
    #[test]
    fn only_a_body_that_may_hold_errors_is_parsed() {
        assert!(!may_name_errors(
            br#"{"data":{"b0":{"text":"fn main() {}","oid":"45b9"}}}"#
        ));
        assert!(may_name_errors(br#"{"errors":[]}"#));
        assert!(may_name_errors(br#"{"\u0065rrors":[]}"#));
        assert!(may_name_errors(br#"{"data":{"b0":{"text":"\u003c"}}}"#));
    }

    /// The classifying fields are read like a document's: a repeated key
    /// keeps its last value, an escaped key is the key it spells, and a
    /// body that is not an object classifies as nothing.
    #[test]
    fn classified_reads_the_top_level_fields_only() {
        let body = Classified::parse(
            br#"{"data":{"errors":[1]},"message":1,"\u0065rrors":[{"type":"X"}],"errors":[{"type":"RATE_LIMITED"}]}"#,
        );
        assert_eq!(
            body.errors,
            Some(serde_json::json!([{"type": "RATE_LIMITED"}]))
        );
        assert_eq!(body.message, Some(serde_json::json!(1)));
        assert!(body.documentation_url.is_none());
        for other in [&br#"[{"errors":[]}]"#[..], b"not json", b""] {
            let body = Classified::parse(other);
            assert!(body.errors.is_none() && body.message.is_none());
        }
    }

    /// An ordinary GraphQL 200 — a blob batch — is not a limit, and a
    /// `RATE_LIMITED` answer is one however its key is spelled, beside
    /// megabytes of data.
    #[tokio::test]
    async fn a_large_graphql_answer_classifies_by_its_errors_alone() {
        let text = "x".repeat(4 << 20);
        let governor = Governor::new(4, Duration::from_secs(3600), 12);
        let mut answer = response(200, &[]);
        answer.body = serde_json::to_vec(&serde_json::json!({
            "data": {"b0": {"text": text}}
        }))
        .unwrap();
        assert!(!governor.rate_limited("graphql", &answer, 0).await.unwrap());
        for errors in [r#""errors""#, r#""\u0065rrors""#] {
            let governor = Governor::new(4, Duration::from_secs(3600), 12);
            answer.body = format!(
                r#"{{"data":{{"b0":{{"text":"{text}"}}}},{errors}:[{{"type":"RATE_LIMITED","message":"slow down"}}]}}"#
            )
            .into_bytes();
            assert!(governor.rate_limited("graphql", &answer, 0).await.unwrap());
            assert!(governor.state.lock().await.secondary_until.is_some());
        }
    }

    /// A GraphQL answer that spent the last point but carries no limit
    /// error is an ordinary answer.
    #[tokio::test]
    async fn a_graphql_answer_spending_the_last_point_is_not_limited() {
        let governor = Governor::new(4, Duration::from_secs(3600), 12);
        let mut answer = response(200, &[("x-ratelimit-remaining", "0".into())]);
        answer.body = br#"{"data":{"viewer":{"login":"count"}}}"#.to_vec();
        assert!(!governor.rate_limited("graphql", &answer, 0).await.unwrap());
    }

    /// A response still in flight when the limit hit may carry an earlier
    /// reset; it must not shorten the wait the rate-limit answer set.
    #[tokio::test]
    async fn a_spent_bucket_reset_only_moves_later() {
        let governor = Governor::new(4, Duration::from_secs(3600), 12);
        let soon = (epoch_seconds() + 10).to_string();
        // No reset header: the governor backs off a minute.
        let limited = response(403, &[("x-ratelimit-remaining", "0".into())]);
        assert!(governor.rate_limited("core", &limited, 0).await.unwrap());
        let in_flight = response(
            200,
            &[
                ("x-ratelimit-remaining", "0".into()),
                ("x-ratelimit-reset", soon),
            ],
        );
        governor.observe("core", &in_flight).await;
        let state = governor.state.lock().await;
        let reset = state.primary["core"].reset.unwrap();
        assert!(reset >= Instant::now() + Duration::from_secs(50));
    }

    /// The first `burst` writes of a run start at once; each write after
    /// waits for a token, one a second from the last write's start.
    #[tokio::test(start_paused = true)]
    async fn writes_spend_the_budget_then_take_a_second_each() {
        let governor = Governor::new(4, Duration::from_secs(3600), 12);
        let started = Instant::now();
        for _ in 0..12 {
            drop(governor.content().await.unwrap());
        }
        assert_eq!(started.elapsed(), Duration::ZERO);
        drop(governor.content().await.unwrap());
        assert_eq!(started.elapsed(), Duration::from_secs(1));
        drop(governor.content().await.unwrap());
        assert_eq!(started.elapsed(), Duration::from_secs(2));
    }

    /// A spent budget earns a token a second and never more than `burst`:
    /// an idle hour cannot stockpile writes.
    #[tokio::test(start_paused = true)]
    async fn the_budget_refills_one_a_second_up_to_burst() {
        let governor = Governor::new(4, Duration::from_secs(3600), 12);
        for _ in 0..12 {
            drop(governor.content().await.unwrap());
        }
        tokio::time::advance(Duration::from_secs(5)).await;
        let started = Instant::now();
        for _ in 0..5 {
            drop(governor.content().await.unwrap());
        }
        assert_eq!(started.elapsed(), Duration::ZERO);
        drop(governor.content().await.unwrap());
        assert_eq!(started.elapsed(), Duration::from_secs(1));
        tokio::time::advance(Duration::from_secs(600)).await;
        let started = Instant::now();
        for _ in 0..12 {
            drop(governor.content().await.unwrap());
        }
        assert_eq!(started.elapsed(), Duration::ZERO);
        drop(governor.content().await.unwrap());
        assert_eq!(started.elapsed(), Duration::from_secs(1));
    }

    /// However writes arrive, a minute holds at most `burst + 60` of them —
    /// under the documented eighty content requests a minute.
    #[tokio::test(start_paused = true)]
    async fn the_budget_keeps_writes_under_eighty_a_minute() {
        const BURST: usize = 12;
        let governor = Governor::new(4, Duration::from_secs(3600), BURST);
        // A seeded generator, not a crate, so the arrivals are deterministic.
        let mut bits = 0x9e3779b97f4a7c15_u64;
        let mut next = move || {
            bits ^= bits << 13;
            bits ^= bits >> 7;
            bits ^= bits << 17;
            bits
        };
        let mut started = Vec::new();
        for _ in 0..2_000 {
            // Arrivals 0–1.999 s apart cluster writes against the refill.
            tokio::time::advance(Duration::from_millis(next() % 2000)).await;
            let permit = governor.content().await.unwrap();
            started.push(Instant::now());
            drop(permit);
        }
        for (i, at) in started.iter().enumerate() {
            let minute = started[i..]
                .iter()
                .take_while(|t| t.duration_since(*at) < Duration::from_secs(60))
                .count();
            assert!(
                minute <= BURST + 60,
                "{minute} writes in the minute from {at:?}"
            );
        }
    }

    /// Any secondary-rate-limit answer cuts the budget to one token for the
    /// next hour — the one write a second GitHub asks for.
    #[tokio::test(start_paused = true)]
    async fn a_secondary_limit_answer_throttles_writes_for_an_hour() {
        let governor = Governor::new(4, Duration::from_secs(3600), 12);
        assert!(
            governor
                .rate_limited("core", &response(429, &[]), 0)
                .await
                .unwrap()
        );
        let started = Instant::now();
        let mut at = Vec::new();
        for _ in 0..4 {
            let permit = governor.content().await.unwrap();
            at.push(started.elapsed());
            drop(permit);
        }
        assert_eq!(at, [0, 1, 2, 3].map(Duration::from_secs));
        // Still one a second at the hour's edge.
        tokio::time::advance(Duration::from_secs(3596)).await;
        let started = Instant::now();
        drop(governor.content().await.unwrap());
        drop(governor.content().await.unwrap());
        assert_eq!(started.elapsed(), Duration::from_secs(1));
        // The hour over, the budget refills at a token a second again.
        tokio::time::advance(Duration::from_secs(14)).await;
        let started = Instant::now();
        for _ in 0..12 {
            drop(governor.content().await.unwrap());
        }
        assert_eq!(started.elapsed(), Duration::ZERO);
    }
}
