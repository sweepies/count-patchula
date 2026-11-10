//! The registry rules the Count's lookups follow, supplied to the generic
//! HTTP client: OCI digest immutability, tag-page and Go proxy floors,
//! remembered 404s and Docker Hub's pull quota.
use patchula_cache::HttpEntry;
use patchula_http::{Options, RegistryRules, Response};
use reqwest::{Method, Url};
use std::{sync::Arc, time::Duration};

/// How long a digest-addressed registry document stays fresh: ten years, so
/// only the cache's size budget evicts it.
const IMMUTABLE: Duration = Duration::from_secs(10 * 365 * 24 * 3600);
/// How long a registry's 404 for a digest or a cosign tag is kept.
const MISSING: Duration = Duration::from_secs(24 * 3600);
const DOCKER_HUB: &str = "registry-1.docker.io";
const GO_PROXY: &str = "proxy.golang.org";

/// The rules for the operator freshness floors in `options`.
pub fn registry_rules(options: &Options) -> Arc<dyn RegistryRules> {
    Arc::new(Rules {
        registry_freshness: Duration::from_secs(options.registry_freshness_secs),
        go_freshness: Duration::from_secs(options.go_freshness_secs),
    })
}

/// `Options` for a `Cached` that serves registry reads: the datasource rules
/// installed over the defaults.
pub fn http_options() -> Options {
    let mut options = Options::default();
    options.rules = registry_rules(&options);
    options
}

/// A `Cached` for registry reads at `concurrency` per host, outside the
/// engine's shared client.
pub fn cached(
    cache: patchula_cache::Cache,
    transport: std::sync::Arc<dyn patchula_http::Transport>,
    concurrency: usize,
) -> std::result::Result<patchula_http::Cached, patchula_http::CacheError> {
    patchula_http::Cached::with_options(
        cache,
        transport,
        Options {
            default_host_concurrency: concurrency,
            ..http_options()
        },
    )
}

#[derive(Debug)]
struct Rules {
    registry_freshness: Duration,
    go_freshness: Duration,
}

/// A manifest or blob addressed by digest, which never changes.
fn immutable(url: &Url) -> bool {
    url.path().contains("/manifests/sha256:") || url.path().contains("/blobs/sha256:")
}

impl RegistryRules for Rules {
    fn immutable(&self, url: &Url) -> bool {
        immutable(url)
    }
    fn freshness_floor(&self, url: &Url, entry: &HttpEntry, registry: bool) -> Option<Duration> {
        if registry && immutable(url) && (200..300).contains(&entry.status) {
            return Some(IMMUTABLE);
        }
        if registry
            && url.path().ends_with("/tags/list")
            && entry.etag.is_none()
            && entry.last_modified.is_none()
        {
            return Some(self.registry_freshness);
        }
        if url.host_str() == Some(GO_PROXY)
            && (url.path().ends_with("/@v/list") || url.path().ends_with("/@latest"))
        {
            return Some(self.go_freshness);
        }
        None
    }
    fn remember_missing(&self, url: &Url) -> Option<Duration> {
        (immutable(url) || [".att", ".sbom"].iter().any(|s| url.path().ends_with(s)))
            .then_some(MISSING)
    }
    fn pull_quota(&self, method: &Method, url: &Url) -> bool {
        *method == Method::GET
            && url.host_str() == Some(DOCKER_HUB)
            && url.path().contains("/manifests/")
    }
    fn quota_window(&self, url: &Url, response: &Response) -> Option<Duration> {
        (url.host_str() == Some(DOCKER_HUB))
            .then(|| exhausted_quota(response))
            .flatten()
    }
    fn pull_limited(&self, response: &Response) -> bool {
        pull_limit(response)
    }
}

/// A manifest GET's 429 is Docker Hub's pull limit: its counter reads zero,
/// its body says so, or it gives no `Retry-After`, which the pull limit omits.
fn pull_limit(response: &Response) -> bool {
    response
        .headers
        .get("ratelimit-remaining")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(';')
                .next()
                .is_some_and(|v| v.trim().parse::<u64>() == Ok(0))
        })
        || response
            .body
            .windows(b"pull rate limit".len())
            .any(|v| v.eq_ignore_ascii_case(b"pull rate limit"))
        || !response.headers.contains_key("retry-after")
}

// Docker Hub uses `remaining;w=seconds`, not the unrelated Hub REST API's X- headers.
fn exhausted_quota(response: &Response) -> Option<Duration> {
    let remaining = response.headers.get("ratelimit-remaining")?.to_str().ok()?;
    if remaining.split(';').next()?.trim().parse::<u64>().ok()? != 0 {
        return None;
    }
    let window = remaining
        .split(';')
        .chain(
            response
                .headers
                .get("ratelimit-limit")
                .and_then(|v| v.to_str().ok())
                .into_iter()
                .flat_map(|v| v.split(';')),
        )
        .find_map(|v| v.trim().strip_prefix("w=")?.parse::<u64>().ok())
        .unwrap_or(21600);
    Some(Duration::from_secs(window.max(61)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::{StatusCode, header::HeaderMap};

    fn response(status: u16, headers: &[(&str, &str)], body: &str) -> Response {
        Response {
            status: StatusCode::from_u16(status).unwrap(),
            headers: headers
                .iter()
                .map(|(k, v)| {
                    (
                        reqwest::header::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                        v.parse().unwrap(),
                    )
                })
                .collect::<HeaderMap>(),
            body: body.as_bytes().to_vec(),
            stale: None,
        }
    }

    /// A manifest GET's 429 is Docker Hub's pull limit when its counter
    /// reads zero, its body says so, or it omits `Retry-After` — the pull
    /// limit is the one throttle that does not ask to be retried.
    #[test]
    fn a_manifest_429_is_a_pull_limit_only_on_the_evidence() {
        let rules = registry_rules(&Options::default());
        // The counter spent: `remaining` parses to zero.
        assert!(rules.pull_limited(&response(
            429,
            &[("ratelimit-remaining", "0;w=21600"), ("retry-after", "30")],
            "{}",
        )));
        // Docker Hub's own wording, in any case.
        assert!(rules.pull_limited(&response(
            429,
            &[("retry-after", "30")],
            "You have reached your pull rate limit.",
        )));
        // No `Retry-After` at all: the pull limit omits it.
        assert!(rules.pull_limited(&response(429, &[], "{}")));
        // An ordinary throttle names its wait and keeps its counter.
        assert!(!rules.pull_limited(&response(
            429,
            &[
                ("ratelimit-remaining", "137;w=21600"),
                ("retry-after", "30"),
            ],
            "{}",
        )));
        // A spent counter on a non-pull-limit body still reads as limited:
        // the header alone is the evidence.
        assert!(rules.pull_limited(&response(429, &[("ratelimit-remaining", "0")], "{}",)));
    }
}
