//! The conditional-GET protocol the registry cache and the GitHub client
//! share: request keys, stored entries, validators, 304 merges, `Retry-After`
//! and bounded decoding. Each client keeps its own freshness policy, sender
//! and error mapping; only this protocol is common.
use crate::Response;
use patchula_cache::{HttpEntry, HttpKey};
use reqwest::{
    Request, StatusCode,
    header::{self, HeaderValue},
};
use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Semaphore;

/// Failures building requests or responses from cached metadata, and running
/// a bounded decode. Each client maps these into its own error enum.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid cached HTTP metadata")]
    Metadata,
    #[error("conditional decode worker failed")]
    Worker,
}

/// Unix seconds now, for `entry`'s fetch time and freshness decisions.
pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// The only `HttpKey` builder. `sent` is the representation-selecting headers
/// the client always sends; authorization and cookies never enter it. The
/// scope goes through `HttpKey::credential`, so responses are never shared
/// between credentials.
pub fn key(
    method: &str,
    url: &str,
    sent: &[(&str, &str)],
    representation: Option<&str>,
    scope: Option<&str>,
) -> patchula_cache::Result<HttpKey> {
    let mut key = HttpKey::new(
        method,
        url,
        sent.iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect::<BTreeMap<_, _>>(),
    )?;
    if let Some(format) = representation {
        key = key.representation(format);
    }
    if let Some(scope) = scope {
        key = key.credential(scope);
    }
    Ok(key)
}

/// One freshness rule — `max-age` less the response's `Age`, else `Expires` —
/// and the union of response headers an entry keeps.
pub fn entry(response: &Response, now: i64) -> HttpEntry {
    let cache_control = comma_header(response, "cache-control");
    let max_age = cache_control
        .as_deref()
        .into_iter()
        .flat_map(|value| value.split(','))
        .find_map(|part| {
            let (name, value) = part.split_once('=')?;
            name.trim()
                .eq_ignore_ascii_case("max-age")
                .then(|| value.trim().trim_matches('"').parse::<i64>().ok())?
        })
        .filter(|age| *age >= 0);
    let age = text(response, "age")
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0)
        .max(0);
    let expires_at = max_age
        .map(|max_age| now.saturating_add(max_age.saturating_sub(age).max(0)))
        .or_else(|| {
            httpdate::parse_http_date(text(response, "expires")?)
                .ok()?
                .duration_since(UNIX_EPOCH)
                .ok()
                .map(|value| value.as_secs() as i64)
        });
    // Only the headers needed to rebuild the response; never cookies or credentials.
    let headers = [
        "etag",
        "last-modified",
        "link",
        "cache-control",
        "expires",
        "vary",
        "content-type",
        "docker-content-digest",
    ]
    .into_iter()
    .filter_map(|name| comma_header(response, name).map(|value| (name.into(), value)))
    .collect();
    HttpEntry {
        status: response.status.as_u16(),
        etag: text(response, "etag").map(str::to_owned),
        last_modified: text(response, "last-modified").map(str::to_owned),
        headers,
        body: response.body.clone(),
        fetched_at: now,
        expires_at,
        cache_control,
    }
}

/// Whether the response may be stored under its key. A client sends a fixed
/// set of request headers, so a `Vary` on one the key holds, one it never
/// sends, or `Accept-Encoding` (bodies are stored decoded) selects the same
/// response every time. `no-store`, `Vary: *` and `Vary: User-Agent` (the
/// agent names this build) are uncacheable, and so is a `Vary` on
/// authorization unless `authorization_in_key`: the key's credential scope
/// names whoever authorized the request.
pub fn storable(response: &Response, authorization_in_key: bool) -> bool {
    let no_store = comma_header(response, "cache-control").is_some_and(|control| {
        control
            .split(',')
            .any(|part| part.trim().eq_ignore_ascii_case("no-store"))
    });
    let varies = comma_header(response, "vary").unwrap_or_default();
    let vary_ok = varies
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .all(|name| {
            match name.to_ascii_lowercase().as_str() {
                "*" => false,
                // The key carries who authorized only when the caller says so.
                "authorization" | "proxy-authorization" | "cookie" => authorization_in_key,
                // The agent names this build, which the key does not.
                "user-agent" => false,
                // In the key, decoded away (accept-encoding), or never sent.
                _ => true,
            }
        });
    !no_store && vary_ok
}

/// Send the entry's validators on a revalidating request.
pub fn add_validators(request: &mut Request, entry: &HttpEntry) -> Result<(), Error> {
    for (name, value) in [
        (header::IF_NONE_MATCH, &entry.etag),
        (header::IF_MODIFIED_SINCE, &entry.last_modified),
    ] {
        if let Some(value) = value {
            request.headers_mut().insert(
                name,
                HeaderValue::from_str(value).map_err(|_| Error::Metadata)?,
            );
        }
    }
    Ok(())
}

/// A 304 keeps the cached status and body; response headers win and cached
/// headers fill in what the response omitted.
pub fn not_modified(mut response: Response, entry: HttpEntry) -> Result<Response, Error> {
    response.status = StatusCode::from_u16(entry.status).map_err(|_| Error::Metadata)?;
    response.body = entry.body;
    for (name, value) in entry.headers {
        let name = header::HeaderName::from_bytes(name.as_bytes()).map_err(|_| Error::Metadata)?;
        if !response.headers.contains_key(&name) {
            response.headers.insert(
                name,
                HeaderValue::from_str(&value).map_err(|_| Error::Metadata)?,
            );
        }
    }
    Ok(response)
}

/// Rebuild the response a cached entry stands for.
pub fn response(entry: &HttpEntry) -> Result<Response, Error> {
    let mut headers = header::HeaderMap::new();
    for (name, value) in &entry.headers {
        headers.insert(
            header::HeaderName::from_bytes(name.as_bytes()).map_err(|_| Error::Metadata)?,
            HeaderValue::from_str(value).map_err(|_| Error::Metadata)?,
        );
    }
    Ok(Response {
        status: StatusCode::from_u16(entry.status).map_err(|_| Error::Metadata)?,
        headers,
        body: entry.body.clone(),
    })
}

/// `Retry-After` as delta-seconds or an HTTP date. Callers keep their own
/// defaults on top.
pub fn retry_after(response: &Response) -> Option<Duration> {
    let value = text(response, "retry-after")?;
    value
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
        .or_else(|| {
            httpdate::parse_http_date(value)
                .ok()
                .map(|date| date.duration_since(SystemTime::now()).unwrap_or_default())
        })
}

/// Run `derive` on the blocking pool, bounded by `decoding`. The inner
/// result is the derive function's own error, for the caller to map.
pub async fn derive<F, E>(
    body: Vec<u8>,
    derive: F,
    decoding: &Arc<Semaphore>,
) -> Result<std::result::Result<Vec<u8>, E>, Error>
where
    F: FnOnce(&[u8]) -> std::result::Result<Vec<u8>, E> + Send + 'static,
    E: Send + 'static,
{
    let permit = decoding
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| Error::Worker)?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        derive(&body)
    })
    .await
    .map_err(|_| Error::Worker)
}

fn text<'a>(response: &'a Response, name: &str) -> Option<&'a str> {
    response.headers.get(name)?.to_str().ok()
}

fn comma_header(response: &Response, name: &str) -> Option<String> {
    let values: Vec<_> = response
        .headers
        .get_all(name)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .collect();
    (!values.is_empty()).then(|| values.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(status: u16, headers: &[(&'static str, &'static str)]) -> Response {
        Response {
            status: StatusCode::from_u16(status).unwrap(),
            headers: headers
                .iter()
                .map(|&(name, value)| {
                    (
                        header::HeaderName::from_static(name),
                        HeaderValue::from_static(value),
                    )
                })
                .collect::<header::HeaderMap>(),
            body: b"body".to_vec(),
        }
    }

    #[test]
    fn entry_subtracts_age_from_max_age() {
        let entry = entry(
            &response(
                200,
                &[("cache-control", "public, max-age=300"), ("age", "120")],
            ),
            1000,
        );
        assert_eq!(entry.expires_at, Some(1000 + 180));
        assert_eq!(entry.fetched_at, 1000);
    }

    #[test]
    fn entry_falls_back_to_expires_without_max_age() {
        let entry = entry(
            &response(200, &[("expires", "Wed, 14 Oct 2026 00:00:00 GMT")]),
            1000,
        );
        assert_eq!(entry.expires_at, Some(1791936000));
    }

    #[test]
    fn entry_keeps_the_union_of_replay_headers() {
        let entry = entry(
            &response(
                200,
                &[
                    ("etag", "\"v\""),
                    ("last-modified", "Wed, 14 Oct 2026 00:00:00 GMT"),
                    ("link", "<https://a/>; rel=\"next\""),
                    ("cache-control", "public, max-age=300"),
                    ("expires", "Wed, 14 Oct 2026 00:00:00 GMT"),
                    ("vary", "Accept"),
                    ("content-type", "application/json"),
                    ("docker-content-digest", "sha256:abc"),
                    ("set-cookie", "session=secret"),
                    ("authorization", "Bearer hidden"),
                ],
            ),
            1000,
        );
        for name in [
            "etag",
            "last-modified",
            "link",
            "cache-control",
            "expires",
            "vary",
            "content-type",
            "docker-content-digest",
        ] {
            assert!(entry.headers.contains_key(name), "{name}");
        }
        assert!(!entry.headers.contains_key("set-cookie"));
        assert!(!entry.headers.contains_key("authorization"));
    }

    #[test]
    fn storable_follows_what_the_key_holds() {
        let stored = |headers: &[(&'static str, &'static str)], authorization_in_key| {
            storable(&response(200, headers), authorization_in_key)
        };
        assert!(!stored(&[("cache-control", "private, no-store")], true));
        assert!(!stored(&[("vary", "Accept, *")], true));
        assert!(!stored(&[("vary", "User-Agent")], true));
        assert!(stored(
            &[("vary", "Accept, Accept-Encoding, X-Requested-With")],
            false
        ));
        assert!(stored(&[("vary", "Accept, Authorization, Cookie")], true));
        assert!(!stored(&[("vary", "Authorization")], false));
        assert!(stored(&[], false));
    }

    #[test]
    fn max_age_is_read_whatever_its_case() {
        let entry = entry(
            &response(200, &[("cache-control", "public, Max-Age = 300")]),
            1000,
        );
        assert_eq!(entry.expires_at, Some(1300));
    }
}
