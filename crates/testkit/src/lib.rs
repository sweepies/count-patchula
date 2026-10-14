//! Opt-in recording, fail-closed offline replay, and secret scanning for tests.
#![forbid(unsafe_code)]

mod redact;
pub use redact::Redactor;

use reqwest::{
    Request, StatusCode,
    header::{HeaderMap, HeaderName, HeaderValue},
};
use rustovate_http::{Error, Exchange, Response, Transport};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

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

fn failure(message: impl Into<String>) -> Error {
    Error::Fixture(message.into())
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

impl Transport for Replay {
    fn send(&self, request: Request) -> Exchange<'_> {
        Box::pin(async move {
            let key = request_key(&request, &self.options)?;
            let mut used = self
                .used
                .lock()
                .map_err(|_| failure("replay lock poisoned"))?;
            let index = self.cassette.interactions.iter().enumerate().position(|(index, interaction)| !used[index] && interaction.request == key)
                .ok_or_else(|| failure(format!("unmatched {} {}; check method, URL, selected headers and body; replay never accesses the network", key.method, key.url)))?;
            used[index] = true;
            let response = &self.cassette.interactions[index].response;
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
            })
        })
    }
}

pub struct Recorder {
    path: PathBuf,
    network: Arc<dyn Transport>,
    cassette: Mutex<Cassette>,
    options: Options,
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
        }
    }
}

impl Transport for Recorder {
    fn send(&self, request: Request) -> Exchange<'_> {
        Box::pin(async move {
            let key = request_key(&request, &self.options)?;
            let response = self.network.send(request).await?;
            let mut body = Body::read(&response.body)?;
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
                .map_err(|_| failure("recorder lock poisoned"))?;
            cassette.interactions.push(interaction);
            save(&self.path, &cassette, &self.options.redactor)?;
            Ok(response)
        })
    }
}

/// Lazy factory proves no network client is constructed in default replay mode.
pub fn from_env(
    path: &Path,
    options: Options,
    network: impl FnOnce() -> Result<Arc<dyn Transport>, Error>,
) -> Result<Arc<dyn Transport>, Error> {
    if std::env::var("RUSTOVATE_RECORD").as_deref() == Ok("1") {
        Ok(Arc::new(Recorder::new(
            path,
            options,
            network()?,
            "Recorded by rustovate from the request URLs; never imported from other bots".into(),
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
        .unwrap_or(Path::new("."));
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
            let cassette: Cassette = serde_json::from_slice(&bytes)
                .map_err(|_| failure(format!("invalid cassette {}", path.display())))?;
            if cassette.schema_version != 1 {
                return Err(failure("unsupported cassette schema version"));
            }
            count += 1;
        }
    }
    Ok(count)
}
