//! Opt-in recording, fail-closed offline replay, and secret scanning for tests.
#![forbid(unsafe_code)]

mod mise;
mod redact;
pub use mise::{Call, MiseFixture, MiseRecorder, MiseReplay, Outcome, Run};
pub use redact::Redactor;

use patchula_http::{Exchange, Response, Transport};
use reqwest::{
    Request, StatusCode,
    header::{HeaderMap, HeaderName, HeaderValue},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use sha2::Digest as _;

type Headers = BTreeMap<String, Vec<String>>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "format", content = "value", rename_all = "snake_case")]
pub enum Body {
    Json(serde_json::Value),
    Text(String),
}

impl Body {
    fn read(bytes: &[u8]) -> Result<Self, Error> {
        match serde_json::from_slice(bytes) {
            Ok(json) => Ok(Self::Json(json)),
            Err(_) => String::from_utf8(bytes.to_vec())
                .map(Self::Text)
                .map_err(|_| failure("cassettes support UTF-8/JSON bodies, not binary streams")),
        }
    }

    fn bytes(&self) -> Vec<u8> {
        match self {
            Self::Json(json) => serde_json::to_vec(json).expect("JSON value is serializable"),
            Self::Text(text) => text.as_bytes().to_vec(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedRequest {
    pub method: String,
    pub url: String,
    pub headers: Headers,
    pub body: Body,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecordedResponse {
    pub status: u16,
    pub headers: Headers,
    pub body: Body,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Interaction {
    pub request: RecordedRequest,
    pub response: RecordedResponse,
}

/// Provenance is either a real endpoint recording or explicitly simulated.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Cassette {
    pub schema_version: u32,
    pub provenance: String,
    pub interactions: Vec<Interaction>,
}

#[derive(Clone)]
pub struct Options {
    pub match_headers: Vec<String>,
    pub redactor: Redactor,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            match_headers: [
                "accept",
                "content-type",
                "if-none-match",
                "if-modified-since",
                "x-github-api-version",
            ]
            .map(str::to_owned)
            .to_vec(),
            redactor: Redactor::default(),
        }
    }
}

/// A cassette or fixture failure: unreadable, invalid or secret-bearing
/// fixture data. Test setup APIs return it; an unmatched replay request
/// surfaces as an ordinary transport error instead.
#[derive(Debug)]
pub struct Error(String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Error {}

fn failure(message: impl Into<String>) -> Error {
    Error(message.into())
}

fn headers(headers: &HeaderMap, redactor: &Redactor) -> Headers {
    headers
        .keys()
        .map(|name| {
            let values = headers
                .get_all(name)
                .iter()
                .map(|value| redactor.field(name.as_str(), value.to_str().unwrap_or("[REDACTED]")))
                .collect();
            (name.to_string(), values)
        })
        .collect()
}

fn request_key(request: &Request, options: &Options) -> Result<RecordedRequest, Error> {
    let body = match request.body() {
        None => &[][..],
        Some(body) => body
            .as_bytes()
            .ok_or_else(|| failure("streaming request bodies cannot be matched"))?,
    };
    let mut body = Body::read(body)?;
    options.redactor.body(&mut body);
    let selected = headers(request.headers(), &options.redactor)
        .into_iter()
        .filter(|(name, _)| {
            options
                .match_headers
                .iter()
                .any(|selected| name.eq_ignore_ascii_case(selected))
        })
        .collect();
    Ok(RecordedRequest {
        method: request.method().to_string(),
        url: options.redactor.url(request.url()),
        headers: selected,
        body,
    })
}

/// Replay contains data and a consumption cursor only: no client, socket or fallback.
pub struct Replay {
    cassette: Cassette,
    used: Mutex<Vec<bool>>,
    options: Options,
}

impl Replay {
    pub fn open(path: &Path, options: Options) -> Result<Self, Error> {
        let bytes = std::fs::read(path).map_err(|_| {
            failure(format!(
                "cannot read cassette {} (record explicitly with mise run fixtures:record)",
                path.display()
            ))
        })?;
        options.redactor.check(&bytes)?;
        let cassette: Cassette =
            serde_json::from_slice(&bytes).map_err(|_| failure("invalid cassette JSON"))?;
        if cassette.schema_version != 1 {
            return Err(failure("unsupported cassette schema version"));
        }
        // A malformed recorded response fails here, not mid-replay.
        for interaction in &cassette.interactions {
            respond(&interaction.response)?;
        }
        Ok(Self {
            used: Mutex::new(vec![false; cassette.interactions.len()]),
            cassette,
            options,
        })
    }

    pub fn assert_finished(&self) -> Result<(), Error> {
        let count = self
            .used
            .lock()
            .map_err(|_| failure("replay lock poisoned"))?
            .iter()
            .filter(|used| !**used)
            .count();
        if count == 0 {
            Ok(())
        } else {
            Err(failure(format!("{count} unused cassette interactions")))
        }
    }
}

/// Rebuild the transport response a recorded one stands for.
fn respond(response: &RecordedResponse) -> Result<Response, Error> {
    let mut headers = HeaderMap::new();
    for (name, values) in &response.headers {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| failure("invalid cassette header name"))?;
        for value in values {
            headers.append(
                name.clone(),
                HeaderValue::from_str(value)
                    .map_err(|_| failure("invalid cassette header value"))?,
            );
        }
    }
    Ok(Response {
        status: StatusCode::from_u16(response.status)
            .map_err(|_| failure("invalid cassette status"))?,
        headers,
        body: response.body.bytes(),
        stale: None,
    })
}

/// The Git blob SHA of `bytes`, for checking a cassette's REST blob reads.
fn git_blob_sha(bytes: &[u8]) -> String {
    use sha1::Digest as _;
    let mut digest = sha1::Sha1::new();
    digest.update(format!("blob {}\0", bytes.len()));
    digest.update(bytes);
    digest
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The bytes of a recorded `GET /repos/{o}/{r}/git/blobs/{sha}` response.
fn blob_bytes(body: &Body) -> Option<Vec<u8>> {
    let parsed;
    let json = match body {
        Body::Json(json) => json,
        Body::Text(text) => {
            parsed = serde_json::from_str::<serde_json::Value>(text).ok()?;
            &parsed
        }
    };
    if json.get("encoding")?.as_str()? != "base64" {
        return None;
    }
    let content = json.get("content")?.as_str()?;
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(
            content
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect::<String>(),
        )
        .ok()
}

impl Replay {
    /// Answer a GraphQL blob batch from the REST blob reads the cassette
    /// holds, for cassettes recorded before batching existed. Returns the
    /// cassette indices the synthesized texts stand in for; binary or
    /// unverifiable blobs stay for their REST reads.
    fn blob_batch(
        &self,
        request: &Request,
        key: &RecordedRequest,
        used: &[bool],
    ) -> Option<(Vec<usize>, RecordedResponse)> {
        if key.method != "POST" {
            return None;
        }
        let url = reqwest::Url::parse(&key.url).ok()?;
        if !url.path().ends_with("/graphql") {
            return None;
        }
        let body = request.body()?.as_bytes()?;
        let json: serde_json::Value = serde_json::from_slice(body).ok()?;
        let query = json.get("query")?.as_str()?;
        let variables = json.get("variables")?.as_object()?;
        let owner = variables.get("owner")?.as_str()?;
        let name = variables.get("name")?.as_str()?;
        // The batch aliases each blob `b{i}` on `object(oid: $o{i})`.
        static ALIAS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
            regex::Regex::new(r"(\w+)\s*:\s*object\s*\(\s*oid\s*:\s*\$(o\d+)\s*\)").unwrap()
        });
        let mut aliases = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for captures in ALIAS.captures_iter(query) {
            let variable = captures[2].to_owned();
            let sha = variables.get(&variable)?.as_str()?.to_owned();
            seen.insert(variable);
            aliases.push((captures[1].to_owned(), sha));
        }
        if aliases.is_empty()
            || variables.keys().any(|key| {
                key.strip_prefix('o')
                    .is_some_and(|rest| rest.parse::<usize>().is_ok())
                    && !seen.contains(key)
            })
        {
            return None;
        }
        let mut repository = serde_json::Map::new();
        let mut consumed = Vec::new();
        for (alias, sha) in aliases {
            let path = format!("/repos/{owner}/{name}/git/blobs/{sha}");
            // Each recorded read answers once, as it would on its own.
            let index = self.cassette.interactions.iter().enumerate().position(
                |(index, interaction)| {
                    !used[index]
                        && !consumed.contains(&index)
                        && interaction.request.method == "GET"
                        && reqwest::Url::parse(&interaction.request.url)
                            .is_ok_and(|url| url.path() == path)
                },
            )?;
            let bytes = blob_bytes(&self.cassette.interactions[index].response.body)?;
            let blob = match std::str::from_utf8(&bytes) {
                Ok(text) if git_blob_sha(&bytes) == sha => {
                    consumed.push(index);
                    serde_json::json!({"text": text, "isBinary": false, "isTruncated": false})
                }
                _ => serde_json::json!({"text": null, "isBinary": true, "isTruncated": false}),
            };
            repository.insert(alias, blob);
        }
        Some((
            consumed,
            RecordedResponse {
                status: 200,
                headers: BTreeMap::from([(
                    "content-type".to_owned(),
                    vec!["application/json".to_owned()],
                )]),
                body: Body::Json(serde_json::json!({
                    "data": { "repository": repository }
                })),
            },
        ))
    }
}

impl Transport for Replay {
    fn send(&self, request: Request) -> Exchange<'_> {
        Box::pin(async move {
            let key =
                request_key(&request, &self.options).unwrap_or_else(|error| panic!("{error}"));
            let mut used = self
                .used
                .lock()
                .unwrap_or_else(|_| panic!("replay lock poisoned"));
            let index = match self
                .cassette
                .interactions
                .iter()
                .enumerate()
                .position(|(index, interaction)| !used[index] && interaction.request == key)
            {
                Some(index) => index,
                None => {
                    if let Some((consumed, response)) = self.blob_batch(&request, &key, &used) {
                        for index in consumed {
                            used[index] = true;
                        }
                        return Ok(respond(&response).expect("a synthesized batch is well formed"));
                    }
                    // An unmatched request is an ordinary transport failure:
                    // callers decide whether a missing interaction matters.
                    return Err(patchula_http::Error::Network);
                }
            };
            used[index] = true;
            Ok(respond(&self.cassette.interactions[index].response)
                .expect("responses are checked when the cassette opens"))
        })
    }
}

pub struct Recorder {
    path: PathBuf,
    network: Arc<dyn Transport>,
    cassette: Mutex<Cassette>,
    options: Options,
    addressed: Mutex<std::collections::BTreeSet<String>>,
}

impl Recorder {
    pub fn new(
        path: &Path,
        options: Options,
        network: Arc<dyn Transport>,
        provenance: String,
    ) -> Self {
        Self {
            path: path.to_owned(),
            network,
            cassette: Mutex::new(Cassette {
                schema_version: 1,
                provenance,
                interactions: Vec::new(),
            }),
            options,
            addressed: Mutex::default(),
        }
    }
}

impl Transport for Recorder {
    fn send(&self, request: Request) -> Exchange<'_> {
        Box::pin(async move {
            let key =
                request_key(&request, &self.options).unwrap_or_else(|error| panic!("{error}"));
            let response = self.network.send(request).await?;
            // A registry body is checked against the digest it was requested
            // by, even after a redirect, so it is kept byte for byte rather
            // than as reformatted JSON.
            let verbatim = {
                let mut addressed = self
                    .addressed
                    .lock()
                    .unwrap_or_else(|_| panic!("recorder lock poisoned"));
                if let Some((_, digest)) = key.url.split_once("/sha256:") {
                    addressed.insert(digest.chars().take(64).collect::<String>());
                }
                let hex: String = sha2::Sha256::digest(&response.body)
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect();
                addressed.contains(&hex)
            };
            let mut body = match String::from_utf8(response.body.to_vec()) {
                Ok(text) if verbatim => Body::Text(text),
                _ => Body::read(&response.body).unwrap_or_else(|error| panic!("{error}")),
            };
            self.options.redactor.body(&mut body);
            let interaction = Interaction {
                request: key,
                response: RecordedResponse {
                    status: response.status.as_u16(),
                    headers: headers(&response.headers, &self.options.redactor),
                    body,
                },
            };
            let mut cassette = self
                .cassette
                .lock()
                .unwrap_or_else(|_| panic!("recorder lock poisoned"));
            cassette.interactions.push(interaction);
            save(&self.path, &cassette, &self.options.redactor)
                .unwrap_or_else(|error| panic!("{error}"));
            Ok(response)
        })
    }
}

/// Set to `1` to record fixtures from the network instead of replaying them.
pub const RECORD_VAR: &str = "PATCHULA_RECORD";
/// The GitHub token variables a recording run reads.
pub const GH_TOKEN_VAR: &str = "GH_TOKEN";
pub const GITHUB_TOKEN_VAR: &str = "GITHUB_TOKEN";

/// Whether this run records fixtures from the network.
pub fn recording() -> bool {
    std::env::var(RECORD_VAR).as_deref() == Ok("1")
}

/// Lazy factory proves no network client is constructed in default replay mode.
pub fn from_env(
    path: &Path,
    options: Options,
    network: impl FnOnce() -> Result<Arc<dyn Transport>, patchula_http::Error>,
) -> Result<Arc<dyn Transport>, Error> {
    if recording() {
        let network = network().map_err(|error| failure(error.to_string()))?;
        Ok(Arc::new(Recorder::new(
            path,
            options,
            network,
            "Recorded by patchula from the request URLs; never imported from other bots".into(),
        )))
    } else {
        Ok(Arc::new(Replay::open(path, options)?))
    }
}

pub fn save(path: &Path, cassette: &Cassette, redactor: &Redactor) -> Result<(), Error> {
    let bytes =
        serde_json::to_vec_pretty(cassette).map_err(|_| failure("cannot serialize cassette"))?;
    redactor.check(&bytes)?;
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(|_| failure("cannot create cassette directory"))?;
    let mut file = tempfile::NamedTempFile::new_in(parent)
        .map_err(|_| failure("cannot create cassette temporary file"))?;
    file.write_all(&bytes)
        .map_err(|_| failure("cannot write cassette"))?;
    file.write_all(b"\n")
        .map_err(|_| failure("cannot write cassette newline"))?;
    file.persist(path)
        .map_err(|_| failure("cannot persist cassette"))?;
    Ok(())
}

/// Recursively check every JSON cassette; errors report paths, never secret values.
pub fn check_directory(path: &Path, redactor: &Redactor) -> Result<usize, Error> {
    let mut count = 0;
    for entry in
        std::fs::read_dir(path).map_err(|_| failure(format!("cannot scan {}", path.display())))?
    {
        let path = entry
            .map_err(|_| failure("cannot read fixture entry"))?
            .path();
        if path.is_dir() {
            count += check_directory(&path, redactor)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            let bytes = std::fs::read(&path).map_err(|_| failure("cannot read fixture"))?;
            redactor
                .check(&bytes)
                .map_err(|_| failure(format!("possible secret in cassette {}", path.display())))?;
            if path
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|dir| dir == "mise")
            {
                mise::parse(&bytes)
                    .map_err(|_| failure(format!("invalid mise fixture {}", path.display())))?;
            } else {
                let cassette: Cassette = serde_json::from_slice(&bytes)
                    .map_err(|_| failure(format!("invalid cassette {}", path.display())))?;
                if cassette.schema_version != 1 {
                    return Err(failure("unsupported cassette schema version"));
                }
            }
            count += 1;
        }
    }
    Ok(count)
}
