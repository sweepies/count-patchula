//! GitHub App JWTs and per-installation access tokens. No credentials reach SQLite.
use crate::{
    Credentials, Error, Response, Result, Token, Transport, base_headers, join_api,
    pagination::next_link, valid_api_url,
};
use chrono::{DateTime, Utc};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use reqwest::{
    Method, Request, Url,
    header::{self, HeaderValue},
};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    sync::Arc,
};
use tokio::sync::Mutex;

/// How far a JWT's issue time is set back, for clock drift with GitHub.
const JWT_BACKDATE: i64 = 60;
/// How long a JWT is valid after now; GitHub allows ten minutes.
const JWT_LIFETIME: i64 = 540;
/// A cached installation token is renewed this long before it expires.
const TOKEN_MARGIN: i64 = 120;

/// Whether `id` is a GitHub id the Count accepts: positive and within
/// `i64`, the bound configuration and the cache keep ids to.
pub(crate) fn valid_id(id: u64) -> bool {
    id > 0 && i64::try_from(id).is_ok()
}

/// Whether `repo` is an `owner/name` pair of plain path segments.
fn valid_repo(repo: &str) -> bool {
    repo.split_once('/')
        .is_some_and(|(owner, name)| valid_segment(owner) && valid_segment(name))
}

#[derive(Serialize)]
struct Claims {
    iat: i64,
    exp: i64,
    iss: String,
}
struct CachedToken {
    minted: Minted,
    expires: i64,
}
/// A minted token, and for a scoped mint the `full_name` of the repository
/// GitHub narrowed it to.
#[derive(Clone)]
struct Minted {
    value: String,
    repository: Option<String>,
}

/// What a cached installation token covers: the whole installation, or one
/// repository id with one permission set — a token never answers a mint
/// narrower or wider than its own.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Mint {
    installation: u64,
    /// `None` is the full installation token; `Some` is the `repository_ids`
    /// + `permissions` mint GitHub narrows the token to.
    scope: Option<MintScope>,
}
/// A scoped mint's `repository_ids` and `permissions`: the fields GitHub
/// binds the answer to, so both belong to the cache key.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct MintScope {
    repository: u64,
    permissions: Permissions,
}
/// One exact `permissions` object a scoped mint may request.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Permissions {
    /// `{"metadata": "read"}` — repository metadata reads only; the token
    /// reads public repositories as any authenticated caller does.
    MetadataRead,
}
impl MintScope {
    /// The request body this scope mints with.
    fn body(&self) -> Value {
        let permissions = match self.permissions {
            Permissions::MetadataRead => json!({"metadata": "read"}),
        };
        json!({"repository_ids": [self.repository], "permissions": permissions})
    }
}

/// One mint's cached token, locked while it is minted.
type MintSlot = Arc<Mutex<Option<CachedToken>>>;

/// Deliberately no Debug implementation: contains the signing key and access tokens.
pub struct App {
    id: u64,
    key: EncodingKey,
    api_url: Url,
    transport: Arc<dyn Transport>,
    /// One slot per mint, so a mint for one installation never waits on
    /// another's.
    tokens: std::sync::Mutex<BTreeMap<Mint, MintSlot>>,
    /// Repository ids by installation and lowercased `owner/name`, read
    /// once for scoped mints.
    repository_ids: std::sync::Mutex<BTreeMap<(u64, String), u64>>,
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
            tokens: Default::default(),
            repository_ids: Default::default(),
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
        if !valid_repo(repo) {
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
            .filter(|id| valid_id(*id))
            .map(Some)
            .ok_or(Error::Json)
    }
    fn jwt(&self, now: i64) -> Result<String> {
        jsonwebtoken::encode(
            &Header::new(Algorithm::RS256),
            &Claims {
                iat: now - JWT_BACKDATE,
                exp: now + JWT_LIFETIME,
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
        self.request_as(method, url, &self.jwt(now)?, None).await
    }
    /// The App surface's one request shape: a bearer — the JWT, or a minted
    /// installation token for the reads a scoped mint needs — plus an
    /// optional JSON body. `None` sends no body, as the full-installation
    /// mint requires.
    async fn request_as(
        &self,
        method: Method,
        url: Url,
        bearer: &str,
        body: Option<Value>,
    ) -> Result<Response> {
        let mut request = Request::new(method, url);
        let mut auth =
            HeaderValue::from_str(&format!("Bearer {bearer}")).map_err(|_| Error::Configuration)?;
        auth.set_sensitive(true);
        request.headers_mut().insert(header::AUTHORIZATION, auth);
        base_headers(request.headers_mut());
        if let Some(body) = body {
            request.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            *request.body_mut() = Some(serde_json::to_vec(&body).map_err(|_| Error::Json)?.into());
        }
        let response = self
            .transport
            .send(request)
            .await
            .map_err(Error::Transport)?;
        if !response.status.is_success() {
            return Err(Error::Status(response.status.as_u16()));
        }
        Ok(response)
    }
    async fn request_json(&self, method: Method, path: &str, now: i64) -> Result<Value> {
        self.request_json_as(method, path, &self.jwt(now)?, None)
            .await
    }
    /// A request authenticating as `bearer` — an installation token for the
    /// metadata reads a scoped mint needs — with an optional JSON body.
    async fn request_json_as(
        &self,
        method: Method,
        path: &str,
        bearer: &str,
        body: Option<Value>,
    ) -> Result<Value> {
        let response = self
            .request_as(method, self.url(path)?, bearer, body)
            .await?;
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
                        .filter(|id| valid_id(*id))
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
        if !valid_id(id) {
            return Err(Error::Configuration);
        }
        Ok(self
            .mint(
                Mint {
                    installation: id,
                    scope: None,
                },
                now,
            )
            .await?
            .value)
    }
    /// `repository`'s numeric id, read with `installation`'s token: the
    /// `repository_ids` a scoped mint must name.
    async fn repository_id(&self, installation: u64, repository: &str, now: i64) -> Result<u64> {
        if !valid_repo(repository) {
            return Err(Error::Configuration);
        }
        let key = (installation, repository.to_ascii_lowercase());
        if let Some(id) = lock(&self.repository_ids).get(&key) {
            return Ok(*id);
        }
        let token = self.token_at(installation, now).await?;
        let value = self
            .request_json_as(Method::GET, &format!("repos/{repository}"), &token, None)
            .await?;
        // A token mints only for the repository the caller named: after a
        // rename, GitHub may answer the old name with another repository.
        if !value["full_name"]
            .as_str()
            .is_some_and(|name| name.eq_ignore_ascii_case(repository))
        {
            return Err(Error::Json);
        }
        let id = value["id"]
            .as_u64()
            .filter(|id| valid_id(*id))
            .ok_or(Error::Json)?;
        lock(&self.repository_ids).insert(key, id);
        Ok(id)
    }
    /// Forget `repository`'s cached id, after a scoped mint for it failed.
    fn forget_repository(&self, installation: u64, repository: &str) {
        lock(&self.repository_ids).remove(&(installation, repository.to_ascii_lowercase()));
    }
    /// `installation`'s token narrowed to `repository`'s numeric id with
    /// `metadata: read` only: what a lockfile tool gets. The token reads
    /// the named repository's metadata and public repositories, as any
    /// authenticated caller does.
    async fn scoped_token_at(
        &self,
        installation: u64,
        repository: u64,
        now: i64,
    ) -> Result<Minted> {
        if !valid_id(installation) || !valid_id(repository) {
            return Err(Error::Configuration);
        }
        self.mint(
            Mint {
                installation,
                scope: Some(MintScope {
                    repository,
                    permissions: Permissions::MetadataRead,
                }),
            },
            now,
        )
        .await
    }
    /// `installation`'s token narrowed to the repository named `repository`.
    /// The mint must come back narrowed to that name: a cached id a rename
    /// and recreate left stale names another repository, so on a mismatch
    /// the id is read again and minted once more, and a second mismatch is
    /// an error rather than a token for the wrong repository.
    async fn scoped_token_for(
        &self,
        installation: u64,
        repository: &str,
        now: i64,
    ) -> Result<String> {
        for _ in 0..2 {
            let id = self.repository_id(installation, repository, now).await?;
            let minted = self.scoped_token_at(installation, id, now).await;
            let minted = match minted {
                Ok(minted) => minted,
                Err(error) => {
                    self.forget_repository(installation, repository);
                    return Err(error);
                }
            };
            if minted
                .repository
                .as_deref()
                .is_some_and(|name| name.eq_ignore_ascii_case(repository))
            {
                return Ok(minted.value);
            }
            self.forget_repository(installation, repository);
        }
        Err(Error::Json)
    }
    async fn mint(&self, key: Mint, now: i64) -> Result<Minted> {
        // Single flight per mint: simultaneous readiness, Git and API
        // requests cannot mint the same token twice.
        let path = format!("app/installations/{}/access_tokens", key.installation);
        let body = key.scope.as_ref().map(MintScope::body);
        let slot = lock(&self.tokens).entry(key).or_default().clone();
        let mut saved = slot.lock().await;
        if let Some(token) = saved
            .as_ref()
            .filter(|token| token.expires > now + TOKEN_MARGIN)
        {
            return Ok(token.minted.clone());
        }
        let jwt = self.jwt(now)?;
        let value = self
            .request_json_as(Method::POST, &path, &jwt, body)
            .await?;
        let token = value["token"]
            .as_str()
            .filter(|token| !token.is_empty() && !token.chars().any(char::is_whitespace))
            .ok_or(Error::Json)?;
        let expires =
            DateTime::parse_from_rfc3339(value["expires_at"].as_str().ok_or(Error::Json)?)
                .map_err(|_| Error::Json)?
                .timestamp();
        if expires <= now + TOKEN_MARGIN {
            return Err(Error::Configuration);
        }
        let minted = Minted {
            value: token.into(),
            repository: value["repositories"][0]["full_name"]
                .as_str()
                .map(str::to_owned),
        };
        *saved = Some(CachedToken {
            minted: minted.clone(),
            expires,
        });
        Ok(minted)
    }
}
/// A map lock; a panicking holder cannot leave a map entry half-written.
fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
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
    fn scoped_token(&self, repository: &str) -> Option<Token<'_>> {
        let app = self.app.clone();
        let installation = self.id;
        let repository = repository.to_owned();
        Some(Box::pin(async move {
            app.scoped_token_for(installation, &repository, Utc::now().timestamp())
                .await
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use patchula_http::{Exchange, Response};
    use patchula_testkit::app_key::AppKey;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Offline(AtomicUsize, Vec<u8>);
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
                &jsonwebtoken::DecodingKey::from_rsa_der(&self.1),
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
    /// The App's JWT is RS256 over the same header and claims, from a PKCS#1
    /// or PKCS#8 PEM key, and verifies with the public key independently of
    /// jsonwebtoken.
    #[test]
    fn jwt_signs_rs256_with_the_app_key_and_verifies_with_its_public_key() {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        let key = AppKey::generate();
        for pem in [key.pkcs1_pem(), key.pkcs8_pem()] {
            let app = App::new(
                1,
                pem.as_bytes(),
                Arc::new(CyclingReads),
                Url::parse("https://api.github.com").unwrap(),
            )
            .unwrap();
            let jwt = app.jwt(1_700_000_000).unwrap();
            let (signed, signature) = jwt.rsplit_once('.').unwrap();
            let (header, claims) = signed.split_once('.').unwrap();
            let json = |part: &str| -> Value {
                serde_json::from_slice(&URL_SAFE_NO_PAD.decode(part).unwrap()).unwrap()
            };
            assert_eq!(json(header), json!({"typ": "JWT", "alg": "RS256"}));
            assert_eq!(
                json(claims),
                json!({"iat": 1_699_999_940, "exp": 1_700_000_540, "iss": "1"})
            );
            aws_lc_rs::signature::UnparsedPublicKey::new(
                &aws_lc_rs::signature::RSA_PKCS1_2048_8192_SHA256,
                key.public_der(),
            )
            .verify(
                signed.as_bytes(),
                &URL_SAFE_NO_PAD.decode(signature).unwrap(),
            )
            .unwrap();
        }
    }

    /// A key that is not PEM is a configuration error. Which PEMs load is
    /// jsonwebtoken's backend-independent decoder: PKCS#1 and PKCS#8.
    #[test]
    fn a_key_that_is_not_pem_is_rejected() {
        assert!(matches!(
            App::new(
                1,
                b"not a key",
                Arc::new(CyclingReads),
                Url::parse("https://api.github.com").unwrap(),
            ),
            Err(Error::Configuration)
        ));
    }

    #[tokio::test]
    async fn signed_jwt_cached_per_installation_and_renewed_before_expiry() {
        let key = AppKey::generate();
        let transport = Arc::new(Offline(AtomicUsize::new(0), key.public_der().to_vec()));
        let pem = key.pkcs1_pem();
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

    /// A scoped mint is cached per (installation, repository, permission
    /// set): a different repository mints its own token, the full
    /// installation token stays a separate mint, and the window two
    /// minutes before expiry mints again.
    #[tokio::test]
    async fn scoped_mints_cache_per_repository_and_renew_before_expiry() {
        let key = AppKey::generate();
        let transport = Arc::new(Offline(AtomicUsize::new(0), key.public_der().to_vec()));
        let pem = key.pkcs1_pem();
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
        let first = app.scoped_token_at(10, 42, now).await.unwrap().value;
        assert_eq!(
            app.scoped_token_at(10, 42, now + 3479).await.unwrap().value,
            first
        );
        assert_eq!(transport.0.load(Ordering::SeqCst), 1);
        assert_ne!(app.scoped_token_at(10, 43, now).await.unwrap().value, first);
        assert_ne!(app.token_at(10, now).await.unwrap(), first);
        assert_ne!(
            app.scoped_token_at(10, 42, now + 3480).await.unwrap().value,
            first
        );
        assert_eq!(transport.0.load(Ordering::SeqCst), 4);
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
        let pem = AppKey::generate().pkcs1_pem();
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

    /// An installation whose `acme/app` id and per-id names a test steers:
    /// `GET repos/acme/app` reads the current id, and a scoped mint names
    /// the repository its id has now.
    struct Renames {
        current: std::sync::Mutex<u64>,
        names: std::sync::Mutex<BTreeMap<u64, String>>,
    }
    impl Transport for Renames {
        fn send(&self, request: Request) -> Exchange<'_> {
            let path = request.url().path().to_ascii_lowercase();
            let body: Value = request
                .body()
                .and_then(|body| body.as_bytes())
                .map_or(Value::Null, |body| serde_json::from_slice(body).unwrap());
            let answer = if path.ends_with("/repos/acme/app") {
                json!({"full_name": "acme/app", "id": *self.current.lock().unwrap()})
            } else {
                let expires = (Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
                match body["repository_ids"][0].as_u64() {
                    Some(id) => {
                        let name = self.names.lock().unwrap()[&id].clone();
                        json!({"token": format!("scoped-{id}"), "expires_at": expires,
                               "repositories": [{"id": id, "full_name": name}]})
                    }
                    None => json!({"token": "full", "expires_at": expires}),
                }
            };
            Box::pin(async move {
                Ok(Response {
                    status: reqwest::StatusCode::OK,
                    headers: header::HeaderMap::new(),
                    body: serde_json::to_vec(&answer).unwrap(),
                    stale: None,
                })
            })
        }
    }

    /// After a rename and recreate, the cached id names the renamed
    /// repository: the mint comes back narrowed to another name, so the id
    /// is read again and the token is minted for the repository asked for.
    /// A mint that still names another repository is an error.
    #[tokio::test]
    async fn a_stale_repository_id_is_read_again_and_a_mismatch_fails_closed() {
        let pem = AppKey::generate().pkcs1_pem();
        let transport = Arc::new(Renames {
            current: std::sync::Mutex::new(1),
            names: std::sync::Mutex::new(BTreeMap::from([(1, "acme/app".to_owned())])),
        });
        let app = App::new(
            1,
            pem.as_bytes(),
            transport.clone(),
            Url::parse("https://api.github.com").unwrap(),
        )
        .unwrap();
        let now = Utc::now().timestamp();
        assert_eq!(app.repository_id(10, "acme/app", now).await.unwrap(), 1);
        // `acme/app` is renamed to `acme/old` and a new `acme/app` created.
        *transport.current.lock().unwrap() = 2;
        transport
            .names
            .lock()
            .unwrap()
            .extend([(1, "acme/old".to_owned()), (2, "acme/app".to_owned())]);
        assert_eq!(
            app.scoped_token_for(10, "Acme/App", now).await.unwrap(),
            "scoped-2"
        );
        // A repository whose mint never names it is refused.
        transport
            .names
            .lock()
            .unwrap()
            .insert(2, "acme/elsewhere".to_owned());
        *transport.current.lock().unwrap() = 2;
        let fresh = App::new(
            1,
            pem.as_bytes(),
            transport.clone(),
            Url::parse("https://api.github.com").unwrap(),
        )
        .unwrap();
        assert!(matches!(
            fresh.scoped_token_for(10, "acme/app", now).await,
            Err(Error::Json)
        ));
    }

    /// Answers every request with one JSON body.
    struct Answer(Value);
    impl Transport for Answer {
        fn send(&self, _: Request) -> Exchange<'_> {
            let body = serde_json::to_vec(&self.0).unwrap();
            Box::pin(async move {
                Ok(Response {
                    status: reqwest::StatusCode::OK,
                    headers: header::HeaderMap::new(),
                    body,
                    stale: None,
                })
            })
        }
    }

    /// An installation id past `i64` is refused like every other GitHub id
    /// the App reads, not handed on to configuration and the cache.
    #[tokio::test]
    async fn an_installation_id_past_i64_is_invalid() {
        let pem = AppKey::generate().pkcs1_pem();
        let app = App::new(
            1,
            pem.as_bytes(),
            Arc::new(Answer(serde_json::json!([{"id": 1_u64 << 63}]))),
            Url::parse("https://api.github.com").unwrap(),
        )
        .unwrap();
        assert!(matches!(app.installations().await, Err(Error::Json)));
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
                    headers.insert(header::LINK, HeaderValue::from_str(&link).unwrap());
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
        let pem = AppKey::generate().pkcs1_pem();
        let app = App::new(
            1,
            pem.as_bytes(),
            Arc::new(CyclingReads),
            Url::parse("https://api.github.com").unwrap(),
        )
        .unwrap();
        assert!(matches!(app.installations().await, Err(Error::Pagination)));
    }
}
