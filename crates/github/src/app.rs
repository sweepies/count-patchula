//! GitHub App JWTs and per-installation access tokens. No credentials reach SQLite.
use crate::{
    ACCEPT, API_VERSION, Credentials, Error, Response, Result, Token, Transport, USER_AGENT,
    join_api, pagination::next_link, valid_api_url,
};
use chrono::{DateTime, Utc};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use reqwest::{
    Method, Request, Url,
    header::{self, HeaderValue},
};
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashSet},
    sync::Arc,
};
use tokio::sync::Mutex;

#[derive(Serialize)]
struct Claims {
    iat: i64,
    exp: i64,
    iss: String,
}
struct CachedToken {
    value: String,
    expires: i64,
}

/// Deliberately no Debug implementation: contains the signing key and access tokens.
pub struct App {
    id: u64,
    key: EncodingKey,
    api_url: Url,
    transport: Arc<dyn Transport>,
    tokens: Mutex<BTreeMap<u64, CachedToken>>,
}
impl App {
    pub fn new(
        id: u64,
        pem: &[u8],
        transport: Arc<dyn Transport>,
        api_url: Url,
    ) -> Result<Arc<Self>> {
        if id == 0 || !valid_api_url(&api_url) {
            return Err(Error::Configuration);
        }
        let key = EncodingKey::from_rsa_pem(pem).map_err(|_| Error::Configuration)?;
        Ok(Arc::new(Self {
            id,
            key,
            api_url: crate::api_root(&api_url),
            transport,
            tokens: Mutex::new(BTreeMap::new()),
        }))
    }
    pub fn installation(self: &Arc<Self>, id: u64) -> Arc<dyn Credentials> {
        Arc::new(Installation {
            app: self.clone(),
            id,
            scope: format!("github-app:{}/installation:{id}", self.id),
        })
    }
    pub fn id(&self) -> u64 {
        self.id
    }
    /// Resolve only this App's installation, authenticated with its JWT.
    pub async fn repository_installation(&self, repo: &str) -> Result<Option<u64>> {
        let parts: Vec<_> = repo.split('/').collect();
        if parts.len() != 2 || !parts.iter().all(|part| valid_segment(part)) {
            return Err(Error::Configuration);
        }
        self.lookup_installation(&format!("repos/{repo}/installation"))
            .await
    }
    pub async fn organization_installation(&self, org: &str) -> Result<Option<u64>> {
        if !valid_segment(org) {
            return Err(Error::Configuration);
        }
        self.lookup_installation(&format!("orgs/{org}/installation"))
            .await
    }
    async fn lookup_installation(&self, path: &str) -> Result<Option<u64>> {
        let value = match self
            .request_json(Method::GET, path, Utc::now().timestamp())
            .await
        {
            Ok(value) => value,
            Err(Error::Status(404)) => return Ok(None),
            Err(error) => return Err(error),
        };
        value["id"]
            .as_u64()
            .filter(|id| *id > 0 && *id <= i64::MAX as u64)
            .map(Some)
            .ok_or(Error::Json)
    }
    fn jwt(&self, now: i64) -> Result<String> {
        jsonwebtoken::encode(
            &Header::new(Algorithm::RS256),
            &Claims {
                iat: now - 60,
                exp: now + 540,
                iss: self.id.to_string(),
            },
            &self.key,
        )
        .map_err(|_| Error::Configuration)
    }
    fn url(&self, path: &str) -> Result<Url> {
        join_api(&self.api_url, path)
    }
    async fn request(&self, method: Method, url: Url, now: i64) -> Result<Response> {
        let mut request = Request::new(method, url);
        let mut auth = HeaderValue::from_str(&format!("Bearer {}", self.jwt(now)?))
            .map_err(|_| Error::Configuration)?;
        auth.set_sensitive(true);
        request.headers_mut().insert(header::AUTHORIZATION, auth);
        request
            .headers_mut()
            .insert(header::ACCEPT, HeaderValue::from_static(ACCEPT));
        request
            .headers_mut()
            .insert(header::USER_AGENT, HeaderValue::from_static(USER_AGENT));
        request.headers_mut().insert(
            "x-github-api-version",
            HeaderValue::from_static(API_VERSION),
        );
        let response = self
            .transport
            .send(request)
            .await
            .map_err(|_| Error::Transport)?;
        if !response.status.is_success() {
            return Err(Error::Status(response.status.as_u16()));
        }
        Ok(response)
    }
    async fn request_json(&self, method: Method, path: &str, now: i64) -> Result<Value> {
        let response = self.request(method, self.url(path)?, now).await?;
        serde_json::from_slice(&response.body).map_err(|_| Error::Json)
    }
    /// Authenticates the signing key with GitHub and returns the app's canonical bot login.
    pub async fn bot_login(&self) -> Result<String> {
        let value = self
            .request_json(Method::GET, "app", Utc::now().timestamp())
            .await?;
        value["slug"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|s| format!("{s}[bot]"))
            .ok_or(Error::Json)
    }
    /// The App's OAuth client ID, which the CLI's device flow needs. It is public.
    pub async fn client_id(&self) -> Result<String> {
        let value = self
            .request_json(Method::GET, "app", Utc::now().timestamp())
            .await?;
        value["client_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .ok_or(Error::Json)
    }
    /// The webhook URL in the App's settings, where GitHub delivers events.
    pub async fn webhook_url(&self) -> Result<String> {
        let value = self
            .request_json(Method::GET, "app/hook/config", Utc::now().timestamp())
            .await?;
        value["url"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .ok_or(Error::Json)
    }
    pub async fn installations(&self) -> Result<Vec<u64>> {
        let mut next = Some(self.url("app/installations?per_page=100")?);
        let mut visited = HashSet::new();
        let mut ids = Vec::new();
        for _ in 0..1000 {
            let Some(url) = next.take() else {
                return Ok(ids);
            };
            if !visited.insert(url.clone()) {
                return Err(Error::Pagination);
            }
            let response = self
                .request(Method::GET, url.clone(), Utc::now().timestamp())
                .await?;
            let value: Value = serde_json::from_slice(&response.body).map_err(|_| Error::Json)?;
            let entries = value.as_array().ok_or(Error::Json)?;
            for entry in entries {
                if !entry["suspended_at"].is_null() {
                    continue;
                }
                ids.push(
                    entry["id"]
                        .as_u64()
                        .filter(|id| *id > 0)
                        .ok_or(Error::Json)?,
                );
            }
            next = next_link(&response, &url)?
                .map(|target| self.url(target.as_str()))
                .transpose()?;
        }
        Err(Error::Pagination)
    }
    async fn token_at(&self, id: u64, now: i64) -> Result<String> {
        if id == 0 {
            return Err(Error::Configuration);
        }
        // Single flight: simultaneous readiness, Git and API requests cannot mint twice.
        let mut tokens = self.tokens.lock().await;
        if let Some(token) = tokens.get(&id).filter(|token| token.expires > now + 120) {
            return Ok(token.value.clone());
        }
        let value = self
            .request_json(
                Method::POST,
                &format!("app/installations/{id}/access_tokens"),
                now,
            )
            .await?;
        let token = value["token"]
            .as_str()
            .filter(|token| !token.is_empty() && !token.chars().any(char::is_whitespace))
            .ok_or(Error::Json)?;
        let expires =
            DateTime::parse_from_rfc3339(value["expires_at"].as_str().ok_or(Error::Json)?)
                .map_err(|_| Error::Json)?
                .timestamp();
        if expires <= now + 120 {
            return Err(Error::Configuration);
        }
        tokens.insert(
            id,
            CachedToken {
                value: token.into(),
                expires,
            },
        );
        Ok(token.into())
    }
}
fn valid_segment(value: &str) -> bool {
    !value.is_empty()
        && !matches!(value, "." | "..")
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}
struct Installation {
    app: Arc<App>,
    id: u64,
    scope: String,
}
impl Credentials for Installation {
    fn scope(&self) -> &str {
        &self.scope
    }
    fn token(&self) -> Token<'_> {
        Box::pin(self.app.token_at(self.id, Utc::now().timestamp()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use patchula_http::{Exchange, Response};
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Offline(AtomicUsize, String);
    impl Transport for Offline {
        fn send(&self, request: Request) -> Exchange<'_> {
            let mint = self.0.fetch_add(1, Ordering::SeqCst);
            assert_eq!(request.method(), Method::POST);
            let jwt = request.headers()[header::AUTHORIZATION]
                .to_str()
                .unwrap()
                .strip_prefix("Bearer ")
                .unwrap();
            let claims = jsonwebtoken::decode::<Value>(
                jwt,
                &jsonwebtoken::DecodingKey::from_rsa_pem(self.1.as_bytes()).unwrap(),
                &{
                    let mut validation = jsonwebtoken::Validation::new(Algorithm::RS256);
                    validation.validate_exp = false;
                    validation
                },
            )
            .unwrap()
            .claims;
            assert_eq!(claims["iss"], "1");
            assert_eq!(
                claims["exp"].as_i64().unwrap() - claims["iat"].as_i64().unwrap(),
                600
            );
            let expires = DateTime::from_timestamp(claims["iat"].as_i64().unwrap() + 60 + 3600, 0)
                .unwrap()
                .to_rfc3339();
            Box::pin(async move {
                Ok(Response { status: reqwest::StatusCode::CREATED, headers: Default::default(), body: serde_json::to_vec(&serde_json::json!({"token":format!("offline-installation-token-{mint}"), "expires_at":expires})).unwrap(), stale: None })
            })
        }
    }
    #[tokio::test]
    async fn signed_jwt_cached_per_installation_and_renewed_before_expiry() {
        use rsa::pkcs1::{EncodeRsaPrivateKey, EncodeRsaPublicKey, LineEnding};
        let key = rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap();
        let transport = Arc::new(Offline(
            AtomicUsize::new(0),
            key.to_public_key().to_pkcs1_pem(LineEnding::LF).unwrap(),
        ));
        let pem = key.to_pkcs1_pem(LineEnding::LF).unwrap();
        let app = App::new(
            1,
            pem.as_bytes(),
            transport.clone(),
            Url::parse("https://api.github.com/").unwrap(),
        )
        .unwrap();
        let now = DateTime::parse_from_rfc3339("2026-10-15T00:00:00Z")
            .unwrap()
            .timestamp();
        let first = app.token_at(10, now).await.unwrap();
        assert_eq!(app.token_at(10, now + 3479).await.unwrap(), first);
        assert_eq!(transport.0.load(Ordering::SeqCst), 1);
        assert_ne!(app.token_at(20, now).await.unwrap(), first);
        assert_ne!(app.token_at(10, now + 3480).await.unwrap(), first);
        assert_eq!(transport.0.load(Ordering::SeqCst), 3);
        assert_ne!(app.installation(10).scope(), app.installation(20).scope());
    }

    /// Answers the App metadata and installations endpoints; the second
    /// installations page arrives through the Link header.
    struct AppReads(AtomicUsize);
    impl Transport for AppReads {
        fn send(&self, request: Request) -> Exchange<'_> {
            self.0.fetch_add(1, Ordering::SeqCst);
            let path = request.url().path().to_owned();
            let page_two = request
                .url()
                .query()
                .is_some_and(|query| query.contains("page=2"));
            Box::pin(async move {
                let (body, link) = if path.ends_with("/app/hook/config") {
                    (
                        serde_json::json!({"url": "https://patchula.example.com/hook"}),
                        None,
                    )
                } else if path.ends_with("/app/installations") {
                    if page_two {
                        (serde_json::json!([{"id": 30}]), None)
                    } else {
                        (
                            serde_json::json!([
                                {"id": 10},
                                {"id": 20, "suspended_at": "2026-01-01T00:00:00Z"}
                            ]),
                            Some(
                                "<https://api.github.com/app/installations?per_page=100&page=2>; rel=\"next\"",
                            ),
                        )
                    }
                } else {
                    (
                        serde_json::json!({"slug": "count-patchula", "client_id": "Iv1.client"}),
                        None,
                    )
                };
                let mut headers = header::HeaderMap::new();
                if let Some(link) = link {
                    headers.insert(header::LINK, HeaderValue::from_static(link));
                }
                Ok(Response {
                    status: reqwest::StatusCode::OK,
                    headers,
                    body: serde_json::to_vec(&body).unwrap(),
                    stale: None,
                })
            })
        }
    }

    #[tokio::test]
    async fn app_metadata_and_installations_paginate() {
        use rsa::pkcs1::{EncodeRsaPrivateKey, LineEnding};
        let key = rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap();
        let pem = key.to_pkcs1_pem(LineEnding::LF).unwrap();
        let app = App::new(
            1,
            pem.as_bytes(),
            Arc::new(AppReads(AtomicUsize::new(0))),
            Url::parse("https://api.github.com").unwrap(),
        )
        .unwrap();
        assert_eq!(app.bot_login().await.unwrap(), "count-patchula[bot]");
        assert_eq!(app.client_id().await.unwrap(), "Iv1.client");
        assert_eq!(
            app.webhook_url().await.unwrap(),
            "https://patchula.example.com/hook"
        );
        // The suspended installation is skipped and the Link header's second
        // page is followed.
        assert_eq!(app.installations().await.unwrap(), vec![10, 30]);
    }

    /// Every installations page links back to itself: pagination must stop
    /// rather than loop forever.
    struct CyclingReads;
    impl Transport for CyclingReads {
        fn send(&self, request: Request) -> Exchange<'_> {
            let next = request.url().to_string();
            Box::pin(async move {
                let (body, link) = if request.url().path().ends_with("/app/installations") {
                    (
                        serde_json::json!([{"id": 10}]),
                        Some(format!("<{next}>; rel=\"next\"")),
                    )
                } else {
                    (serde_json::json!({}), None)
                };
                let mut headers = header::HeaderMap::new();
                if let Some(link) = link {
                    headers.insert(
                        header::LINK,
                        HeaderValue::from_str(&link).unwrap(),
                    );
                }
                Ok(Response {
                    status: reqwest::StatusCode::OK,
                    headers,
                    body: serde_json::to_vec(&body).unwrap(),
                    stale: None,
                })
            })
        }
    }

    #[tokio::test]
    async fn a_page_that_links_to_itself_is_a_pagination_error() {
        use rsa::pkcs1::{EncodeRsaPrivateKey, LineEnding};
        let key = rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap();
        let pem = key.to_pkcs1_pem(LineEnding::LF).unwrap();
        let app = App::new(
            1,
            pem.as_bytes(),
            Arc::new(CyclingReads),
            Url::parse("https://api.github.com").unwrap(),
        )
        .unwrap();
        assert!(matches!(
            app.installations().await,
            Err(Error::Pagination)
        ));
    }
}
