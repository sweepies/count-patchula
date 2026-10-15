//! GitHub App JWTs and per-installation access tokens. No credentials reach SQLite.
use crate::{Credentials, Error, Result, Token, Transport};
use chrono::{DateTime, Utc};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use reqwest::{
    Method, Request,
    header::{self, HeaderValue},
};
use serde::Serialize;
use serde_json::Value;
use std::{collections::BTreeMap, sync::Arc};
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
    transport: Arc<dyn Transport>,
    tokens: Mutex<BTreeMap<u64, CachedToken>>,
}
impl App {
    pub fn new(id: u64, pem: &[u8], transport: Arc<dyn Transport>) -> Result<Arc<Self>> {
        if id == 0 {
            return Err(Error::Configuration);
        }
        let key = EncodingKey::from_rsa_pem(pem).map_err(|_| Error::Configuration)?;
        Ok(Arc::new(Self {
            id,
            key,
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
    async fn request(&self, method: Method, path: &str, now: i64) -> Result<Value> {
        let mut request = Request::new(
            method,
            format!("https://api.github.com/{path}")
                .parse()
                .map_err(|_| Error::Url)?,
        );
        let mut auth = HeaderValue::from_str(&format!("Bearer {}", self.jwt(now)?))
            .map_err(|_| Error::Configuration)?;
        auth.set_sensitive(true);
        request.headers_mut().insert(header::AUTHORIZATION, auth);
        request.headers_mut().insert(
            header::ACCEPT,
            HeaderValue::from_static("application/vnd.github+json"),
        );
        request
            .headers_mut()
            .insert(header::USER_AGENT, HeaderValue::from_static("rustovate"));
        request.headers_mut().insert(
            "x-github-api-version",
            HeaderValue::from_static("2022-11-28"),
        );
        let response = self
            .transport
            .send(request)
            .await
            .map_err(|_| Error::Transport)?;
        if !response.status.is_success() {
            return Err(Error::Status(response.status.as_u16()));
        }
        serde_json::from_slice(&response.body).map_err(|_| Error::Json)
    }
    /// Authenticates the signing key with GitHub and returns the app's canonical bot login.
    pub async fn bot_login(&self) -> Result<String> {
        let value = self
            .request(Method::GET, "app", Utc::now().timestamp())
            .await?;
        value["slug"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|s| format!("{s}[bot]"))
            .ok_or(Error::Json)
    }
    pub async fn installations(&self) -> Result<Vec<u64>> {
        let mut ids = Vec::new();
        for page in 1..=1000 {
            let value = self
                .request(
                    Method::GET,
                    &format!("app/installations?per_page=100&page={page}"),
                    Utc::now().timestamp(),
                )
                .await?;
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
            if entries.len() < 100 {
                return Ok(ids);
            }
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
            .request(
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
    use rustovate_http::{Exchange, Response};
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
                Ok(Response { status: reqwest::StatusCode::CREATED, headers: Default::default(), body: serde_json::to_vec(&serde_json::json!({"token":format!("offline-installation-token-{mint}"), "expires_at":expires})).unwrap() })
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
        let app = App::new(1, pem.as_bytes(), transport.clone()).unwrap();
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
}
