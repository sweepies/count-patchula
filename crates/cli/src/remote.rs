//! `--server`: hand a lookup or run to a Count Patchula server. The user signs
//! in once with the server's GitHub App through GitHub's device flow; the
//! resulting user access token is stored per server and refreshed as it
//! expires.
use crate::Services;
use patchula_http::Transport;
use reqwest::{Method, Url, header};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf, time::Duration};

pub const SERVER_VAR: &str = "PATCHULA_SERVER";
const CONFIG_VAR: &str = "XDG_CONFIG_HOME";
const DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
const ACCESS_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
const POLL_INTERVAL: Duration = Duration::from_secs(2);

type Result<T> = std::result::Result<T, (u8, miette::Report)>;
fn fail(message: impl std::fmt::Display) -> (u8, miette::Report) {
    (1, miette::miette!("{message}"))
}
fn usage(message: impl std::fmt::Display) -> (u8, miette::Report) {
    (2, miette::miette!("{message}"))
}

/// The server's base URL from `--server`, then `PATCHULA_SERVER`.
pub fn server(flag: Option<&str>) -> Result<Option<String>> {
    let value = match flag {
        Some(value) => value.to_owned(),
        None => match std::env::var(SERVER_VAR) {
            Ok(value) if !value.is_empty() => value,
            _ => return Ok(None),
        },
    };
    let url = Url::parse(&value).map_err(|_| usage(format!("{value} is not a server URL")))?;
    let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if !(url.scheme() == "https" || (url.scheme() == "http" && local)) {
        return Err(usage(
            "the server URL must use https (http only for localhost)",
        ));
    }
    if url.query().is_some() || url.fragment().is_some() || !url.username().is_empty() {
        return Err(usage(
            "the server URL must not have a query, fragment or user",
        ));
    }
    Ok(Some(url.as_str().trim_end_matches('/').to_owned()))
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    Lookup,
    DryRun,
    Run,
}

/// Runs `repositories` on the server and returns its report.
pub async fn report(
    services: &Services,
    server: &str,
    repositories: Vec<String>,
    mode: Mode,
) -> Result<patchula_report::Report> {
    let transport = services.transport().map_err(fail)?;
    let body = json!({"mode": mode, "repositories": repositories});
    let mut token = token(services, &*transport, server).await?;
    let mut response = send(
        &*transport,
        Method::POST,
        &format!("{server}/api/v1/runs"),
        Some(&token),
        Some(&body),
    )
    .await?;
    if response.0 == 401 {
        // GitHub revoked the stored token; sign in again once.
        forget(services, server)?;
        token = login(services, &*transport, server).await?;
        response = send(
            &*transport,
            Method::POST,
            &format!("{server}/api/v1/runs"),
            Some(&token),
            Some(&body),
        )
        .await?;
    }
    let (status, value) = response;
    if status != 202 {
        return Err(rejected(server, status, &value));
    }
    let id = value["id"]
        .as_str()
        .ok_or_else(|| fail("the server did not return a run ID"))?
        .to_owned();
    eprintln!("patchula: the Count is on it at {server}");
    loop {
        tokio::time::sleep(POLL_INTERVAL).await;
        let (status, value) = send(
            &*transport,
            Method::GET,
            &format!("{server}/api/v1/runs/{id}"),
            Some(&token),
            None,
        )
        .await?;
        if status != 200 {
            return Err(rejected(server, status, &value));
        }
        match value["status"].as_str() {
            Some("running") => continue,
            Some("complete") => {
                return serde_json::from_value(value["report"].clone())
                    .map_err(|_| fail("the server returned a report this CLI cannot read"));
            }
            Some("failed") => {
                return Err(fail(format!(
                    "{server}: {}",
                    value["error"].as_str().unwrap_or("the run failed")
                )));
            }
            _ => return Err(fail("the server returned an unknown run status")),
        }
    }
}

fn rejected(server: &str, status: u16, value: &Value) -> (u8, miette::Report) {
    match value["error"].as_str() {
        Some(error) => fail(format!("{server}: {error}")),
        None => fail(format!(
            "{server} answered HTTP {status}; is it a Count Patchula server?"
        )),
    }
}

/// `patchula auth login`: sign in now, replacing any stored token.
pub async fn sign_in(services: &Services, server: &str) -> Result<String> {
    let transport = services.transport().map_err(fail)?;
    login(services, &*transport, server).await
}

/// `patchula auth logout`: forget the server's token. GitHub keeps the App
/// authorization; revoke it under Settings → Applications.
pub fn forget(services: &Services, server: &str) -> Result<bool> {
    let mut logins = load(services)?;
    let removed = logins.remove(server).is_some();
    if removed {
        save(services, &logins)?;
    }
    Ok(removed)
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Login {
    access_token: String,
    expires_at: Option<i64>,
    refresh_token: Option<String>,
    refresh_expires_at: Option<i64>,
}

fn path(services: &Services) -> Result<PathBuf> {
    #[cfg(test)]
    if let Some(dir) = &services.config_dir {
        return Ok(dir.join("servers.json"));
    }
    let _ = services;
    std::env::var_os(CONFIG_VAR)
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os(crate::HOME_VAR).map(|home| PathBuf::from(home).join(".config"))
        })
        .map(|dir| dir.join("patchula/servers.json"))
        .ok_or_else(|| fail("cannot find a config directory; set XDG_CONFIG_HOME or HOME"))
}
fn load(services: &Services) -> Result<BTreeMap<String, Login>> {
    let path = path(services)?;
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|_| fail(format!("{} is not a valid login store", path.display()))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(error) => Err(fail(format!("cannot read {}: {error}", path.display()))),
    }
}
fn save(services: &Services, logins: &BTreeMap<String, Login>) -> Result<()> {
    use std::io::Write;
    let path = path(services)?;
    let dir = path.parent().expect("store path has a parent");
    std::fs::create_dir_all(dir)
        .map_err(|e| fail(format!("cannot create {}: {e}", dir.display())))?;
    let temporary = path.with_extension("json.tmp");
    let mut options = std::fs::File::options();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let write = || -> std::io::Result<()> {
        let mut file = options.open(&temporary)?;
        file.write_all(&serde_json::to_vec_pretty(logins)?)?;
        file.sync_all()?;
        std::fs::rename(&temporary, &path)
    };
    write().map_err(|e| fail(format!("cannot write {}: {e}", path.display())))
}

/// A usable token for the server: the stored one, a refreshed one, or a new sign-in.
async fn token(services: &Services, transport: &dyn Transport, server: &str) -> Result<String> {
    let now = chrono::Utc::now().timestamp();
    let Some(stored) = load(services)?.remove(server) else {
        return login(services, transport, server).await;
    };
    if stored.expires_at.is_none_or(|at| at > now + 60) {
        return Ok(stored.access_token);
    }
    if let Some(refresh) = stored
        .refresh_token
        .filter(|_| stored.refresh_expires_at.is_none_or(|at| at > now + 60))
    {
        let client_id = client_id(transport, server).await?;
        let (_, value) = send_form(
            transport,
            ACCESS_TOKEN_URL,
            &[
                ("client_id", &client_id),
                ("grant_type", "refresh_token"),
                ("refresh_token", &refresh),
            ],
        )
        .await?;
        if let Some(login) = granted(&value, now) {
            store(services, server, login.clone())?;
            return Ok(login.access_token);
        }
    }
    login(services, transport, server).await
}

fn granted(value: &Value, now: i64) -> Option<Login> {
    let access_token = value["access_token"].as_str().filter(|t| !t.is_empty())?;
    Some(Login {
        access_token: access_token.to_owned(),
        expires_at: value["expires_in"].as_i64().map(|s| now + s),
        refresh_token: value["refresh_token"].as_str().map(str::to_owned),
        refresh_expires_at: value["refresh_token_expires_in"].as_i64().map(|s| now + s),
    })
}
fn store(services: &Services, server: &str, login: Login) -> Result<()> {
    let mut logins = load(services)?;
    logins.insert(server.to_owned(), login);
    save(services, &logins)
}

async fn client_id(transport: &dyn Transport, server: &str) -> Result<String> {
    let (status, value) = send(
        transport,
        Method::GET,
        &format!("{server}/api/v1/auth"),
        None,
        None,
    )
    .await?;
    if status != 200 {
        return Err(rejected(server, status, &value));
    }
    value["clientId"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| fail(format!("{server} did not name its GitHub App")))
}

/// GitHub's device flow for the server's App.
async fn login(services: &Services, transport: &dyn Transport, server: &str) -> Result<String> {
    let client_id = client_id(transport, server).await?;
    let (_, device) = send_form(transport, DEVICE_CODE_URL, &[("client_id", &client_id)]).await?;
    if device["error"] == "device_flow_disabled" {
        return Err(disabled(server));
    }
    let (Some(device_code), Some(user_code), Some(uri)) = (
        device["device_code"].as_str(),
        device["user_code"].as_str(),
        device["verification_uri"].as_str(),
    ) else {
        return Err(fail("GitHub did not start the sign-in"));
    };
    let mut interval = device["interval"].as_u64().unwrap_or(5);
    let deadline = tokio::time::Instant::now()
        + Duration::from_secs(device["expires_in"].as_u64().unwrap_or(900));
    eprintln!("patchula: to sign in to {server}, open {uri} and enter {user_code}");
    loop {
        tokio::time::sleep(Duration::from_secs(interval)).await;
        if tokio::time::Instant::now() >= deadline {
            return Err(fail("the sign-in code expired; run the command again"));
        }
        let (_, value) = send_form(
            transport,
            ACCESS_TOKEN_URL,
            &[
                ("client_id", &client_id),
                ("device_code", device_code),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ],
        )
        .await?;
        if let Some(login) = granted(&value, chrono::Utc::now().timestamp()) {
            store(services, server, login.clone())?;
            eprintln!("patchula: signed in to {server}");
            return Ok(login.access_token);
        }
        match value["error"].as_str() {
            Some("authorization_pending") => {}
            Some("slow_down") => interval = value["interval"].as_u64().unwrap_or(interval + 5),
            Some("expired_token" | "token_expired") => {
                return Err(fail("the sign-in code expired; run the command again"));
            }
            Some("access_denied") => return Err(fail("the sign-in was cancelled")),
            Some("device_flow_disabled") => return Err(disabled(server)),
            _ => return Err(fail("GitHub refused the sign-in")),
        }
    }
}
fn disabled(server: &str) -> (u8, miette::Report) {
    fail(format!(
        "{server}'s GitHub App does not allow device sign-in; its operator can turn on \"Enable Device Flow\" in the App settings"
    ))
}

async fn send(
    transport: &dyn Transport,
    method: Method,
    url: &str,
    token: Option<&str>,
    body: Option<&Value>,
) -> Result<(u16, Value)> {
    let url = Url::parse(url).map_err(|_| fail(format!("{url} is not a URL")))?;
    let mut request = reqwest::Request::new(method, url);
    let headers = request.headers_mut();
    headers.insert(
        header::ACCEPT,
        header::HeaderValue::from_static("application/json"),
    );
    if let Some(token) = token {
        let mut auth = header::HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| fail("the stored token is not valid"))?;
        auth.set_sensitive(true);
        headers.insert(header::AUTHORIZATION, auth);
    }
    if let Some(body) = body {
        headers.insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/json"),
        );
        *request.body_mut() = Some(body.to_string().into());
    }
    exchange(transport, request).await
}
async fn send_form(
    transport: &dyn Transport,
    url: &str,
    params: &[(&str, &str)],
) -> Result<(u16, Value)> {
    // A form body, so the refresh token never sits in a URL.
    let mut form = Url::parse("form:").expect("static URL");
    form.query_pairs_mut().extend_pairs(params);
    let url = Url::parse(url).expect("static GitHub URL");
    let mut request = reqwest::Request::new(Method::POST, url);
    let headers = request.headers_mut();
    headers.insert(
        header::ACCEPT,
        header::HeaderValue::from_static("application/json"),
    );
    headers.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/x-www-form-urlencoded"),
    );
    *request.body_mut() = Some(form.query().unwrap_or_default().to_owned().into());
    exchange(transport, request).await
}
async fn exchange(transport: &dyn Transport, request: reqwest::Request) -> Result<(u16, Value)> {
    let host = request.url().host_str().unwrap_or_default().to_owned();
    let response = transport
        .send(request)
        .await
        .map_err(|_| fail(format!("cannot reach {host}")))?;
    let value = serde_json::from_slice(&response.body).unwrap_or(Value::Null);
    Ok((response.status.as_u16(), value))
}

#[cfg(test)]
mod tests;
