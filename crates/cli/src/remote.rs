//! `--remote`: hand a lookup or run to a Count Patchula server. The user signs
//! in once with the server's GitHub App through GitHub's device flow; the
//! resulting user access token is stored per server and refreshed as it
//! expires. A checkout's repository can also find its server, hidden by the
//! Count in the PRs and dashboard he writes; the user chooses once per
//! repository whether to run there.
use crate::Services;
use patchula_http::Transport;
use reqwest::{Method, Url, header};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf, time::Duration};

pub const SERVER_VAR: &str = "PATCHULA_SERVER";
const CONFIG_VAR: &str = "XDG_CONFIG_HOME";
const DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
const ACCESS_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
const POLL_INTERVAL: Duration = Duration::from_secs(2);
const LOGINS: &str = "servers.json";
const CHOICES: &str = "repositories.json";
/// Open issues and PRs read while looking for the server, 100 a page.
const DISCOVERY_PAGES: u32 = 5;

type Result<T> = std::result::Result<T, (u8, miette::Report)>;
fn fail(message: impl std::fmt::Display) -> (u8, miette::Report) {
    (1, miette::miette!("{message}"))
}
fn usage(message: impl std::fmt::Display) -> (u8, miette::Report) {
    (2, miette::miette!("{message}"))
}

/// The server named by `PATCHULA_SERVER`, if it is set.
pub fn configured(services: &Services) -> Result<Option<String>> {
    #[cfg(test)]
    if let Some(server) = &services.server {
        return parse(server).map(Some);
    }
    let _ = services;
    match std::env::var(SERVER_VAR) {
        Ok(value) if !value.is_empty() => parse(&value).map(Some),
        _ => Ok(None),
    }
}

/// A server's base URL: https, or http only on loopback.
pub fn parse(value: &str) -> Result<String> {
    let url = Url::parse(value).map_err(|_| usage(format!("{value} is not a server URL")))?;
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
    Ok(url.as_str().trim_end_matches('/').to_owned())
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
    let transport = services.remote_transport().map_err(fail)?;
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
        forget_login(services, server)?;
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
    let transport = services.remote_transport().map_err(fail)?;
    login(services, &*transport, server).await
}

/// `patchula auth logout`: forget the server's token, and which checkouts
/// chose it, so they ask again. GitHub keeps the App authorization; revoke it
/// under Settings → Applications.
pub fn forget(services: &Services, server: &str) -> Result<bool> {
    let chosen = update(
        services,
        CHOICES,
        |choices: &mut BTreeMap<String, Choice>| {
            let before = choices.len();
            choices.retain(|_, choice| choice.server != server);
            choices.len() != before
        },
    )?;
    Ok(forget_login(services, server)? || chosen)
}
fn forget_login(services: &Services, server: &str) -> Result<bool> {
    update(services, LOGINS, |logins: &mut BTreeMap<String, Login>| {
        logins.remove(server).is_some()
    })
}

/// The user's answer for a repository's advertised server. A refusal keeps
/// the URL too, so signing out of that server asks again.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Choice {
    server: String,
    remote: bool,
}

/// The server for the current checkout's `repository`: the one the user chose
/// before, or one the Count advertises, if the user wants it. None runs here.
pub async fn choose(services: &Services, repository: &str) -> Result<Option<String>> {
    if let Some(choice) = load::<Choice>(services, CHOICES)?.remove(repository) {
        if !choice.remote {
            return Ok(None);
        }
        eprintln!(
            "patchula: running {repository} on {}, as you chose; --no-remote runs it here",
            choice.server
        );
        return Ok(Some(choice.server));
    }
    if !services.interactive() {
        return Ok(None);
    }
    let Some(server) = discover(services, repository).await else {
        return Ok(None);
    };
    let remote = services
        .ask(&format!(
            "patchula: the Count runs {repository} on {server}. Run there instead of on this machine? [Y/n] "
        ))
        .map_err(|e| fail(format!("cannot read the answer: {e}")))?;
    update(
        services,
        CHOICES,
        |choices: &mut BTreeMap<String, Choice>| {
            choices.insert(
                repository.to_owned(),
                Choice {
                    server: server.clone(),
                    remote,
                },
            );
            true
        },
    )?;
    eprintln!(
        "patchula: remembered for {repository}. --remote or --no-remote overrides it once; patchula auth logout --server {server} asks again"
    );
    Ok(remote.then_some(server))
}

/// The server for `--remote` in the current checkout's `repository`: the one
/// remembered for it, whatever the answer was, or one the Count advertises.
pub async fn find(services: &Services, repository: &str) -> Result<Option<String>> {
    if let Some(choice) = load::<Choice>(services, CHOICES)?.remove(repository) {
        return Ok(Some(choice.server));
    }
    Ok(discover(services, repository).await)
}

/// The server URL in the newest open issue or PR that the server's own bot
/// wrote. Anyone can paste the marker, and any installed App can write as a
/// bot, so the author must be the bot the server names.
async fn discover(services: &Services, repository: &str) -> Option<String> {
    let transport = services.transport().ok()?;
    // Public repositories answer without a token.
    let token = services.token().await;
    let mut checked = std::collections::BTreeSet::new();
    for page in 1..=DISCOVERY_PAGES {
        let url = format!(
            "https://api.github.com/repos/{repository}/issues?state=open&sort=updated&direction=desc&per_page=100&page={page}"
        );
        let (status, value) = send(&*transport, Method::GET, &url, token.as_deref(), None)
            .await
            .ok()?;
        let items = value.as_array().filter(|_| status == 200)?;
        for item in items.iter().filter(|item| item["user"]["type"] == "Bot") {
            let (Some(author), Some(body)) =
                (item["user"]["login"].as_str(), item["body"].as_str())
            else {
                continue;
            };
            let Some(server) = patchula_update::advertise::find(body)
                .filter(|url| url.starts_with("https://"))
                .and_then(|url| parse(url).ok())
            else {
                continue;
            };
            if !checked.insert((server.clone(), author.to_owned())) {
                continue;
            }
            if names_bot(services, &server).await.as_deref() == Some(author) {
                return Some(server);
            }
        }
        if items.len() < 100 {
            break;
        }
    }
    None
}
/// The bot login the server says its App writes as.
async fn names_bot(services: &Services, server: &str) -> Option<String> {
    let transport = services.remote_transport().ok()?;
    let (status, value) = send(
        &*transport,
        Method::GET,
        &format!("{server}/api/v1/auth"),
        None,
        None,
    )
    .await
    .ok()?;
    (status == 200)
        .then(|| value["bot"].as_str().map(str::to_owned))
        .flatten()
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Login {
    access_token: String,
    expires_at: Option<i64>,
    refresh_token: Option<String>,
    refresh_expires_at: Option<i64>,
}

fn path(services: &Services, name: &str) -> Result<PathBuf> {
    #[cfg(test)]
    if let Some(dir) = &services.config_dir {
        return Ok(dir.join(name));
    }
    let _ = services;
    std::env::var_os(CONFIG_VAR)
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os(crate::HOME_VAR).map(|home| PathBuf::from(home).join(".config"))
        })
        .map(|dir| dir.join("patchula").join(name))
        .ok_or_else(|| fail("cannot find a config directory; set XDG_CONFIG_HOME or HOME"))
}
fn load<T: DeserializeOwned>(services: &Services, name: &str) -> Result<BTreeMap<String, T>> {
    let path = path(services, name)?;
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| {
            fail(format!(
                "{} is not valid; delete it to start over",
                path.display()
            ))
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(error) => Err(fail(format!("cannot read {}: {error}", path.display()))),
    }
}
/// Changes a store under a lock, so two CLIs signing in at once keep both
/// tokens. `change` says whether it changed anything.
fn update<T: DeserializeOwned + Serialize>(
    services: &Services,
    name: &str,
    change: impl FnOnce(&mut BTreeMap<String, T>) -> bool,
) -> Result<bool> {
    let path = path(services, name)?;
    let dir = path.parent().expect("store path has a parent");
    std::fs::create_dir_all(dir)
        .map_err(|e| fail(format!("cannot create {}: {e}", dir.display())))?;
    // Held until this function returns; the OS releases it with the file.
    let _lock = std::fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path.with_extension("json.lock"))
        .and_then(|file| file.lock().map(|()| file))
        .map_err(|e| fail(format!("cannot lock {}: {e}", path.display())))?;
    let mut entries = load(services, name)?;
    let changed = change(&mut entries);
    if changed {
        save(&path, &entries)?;
    }
    Ok(changed)
}
fn save<T: Serialize>(path: &std::path::Path, entries: &BTreeMap<String, T>) -> Result<()> {
    use std::io::Write;
    let temporary = path.with_extension(format!("json.{}.tmp", std::process::id()));
    let mut options = std::fs::File::options();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let write = || -> std::io::Result<()> {
        let mut file = options.open(&temporary)?;
        file.write_all(&serde_json::to_vec_pretty(entries)?)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)
    };
    write().map_err(|e| fail(format!("cannot write {}: {e}", path.display())))
}

/// A usable token for the server: the stored one, a refreshed one, or a new sign-in.
async fn token(services: &Services, transport: &dyn Transport, server: &str) -> Result<String> {
    let now = chrono::Utc::now().timestamp();
    let Some(stored) = load::<Login>(services, LOGINS)?.remove(server) else {
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
    update(services, LOGINS, |logins: &mut BTreeMap<String, Login>| {
        logins.insert(server.to_owned(), login);
        true
    })
    .map(|_| ())
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
