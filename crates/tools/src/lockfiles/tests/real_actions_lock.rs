//! `gh-actions-lock` behind a loopback CONNECT proxy, on the model of
//! `real_sparse_registry.rs`: the run's `HTTPS_PROXY` points at
//! `GitHubProxy` and `SSL_CERT_FILE` at a certificate generated per test,
//! since a committed key would trip the fixtures scan. `CONNECT
//! api.github.com:443` is terminated in TLS and every tunneled request is
//! answered through the testkit's cassette `Replay`, so the cassette
//! format, redaction, `PATCHULA_RECORD=1` recording and `assert_finished`
//! all apply. Any other `CONNECT` is refused, which proves the tool
//! contacts nothing else.
//!
//! The tests pin what the plain tool does today, so moving the
//! `defaults.toml` pin notices a change: a major-only ref is narrowed, a
//! SHA pin is unpinned, and a sub-path action locks under its repository
//! root.
use super::*;
use patchula_testkit::{Options as FixtureOptions, Recorder, Redactor, Replay};
use reqwest::header::{HeaderName, HeaderValue};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;

/// The only host the tool may reach; any other `CONNECT` is refused and
/// recorded, which fails the test.
const GITHUB: &str = "api.github.com:443";

/// `HTTPS_PROXY=http://{url}` sends a run's GitHub requests through this
/// proxy: `CONNECT api.github.com:443` is answered `200` and terminated in
/// TLS with a certificate made per test, then each request is converted to
/// a `reqwest::Request` and fed to the cassette transport — `Replay` by
/// default, `Recorder` against real GitHub when `PATCHULA_RECORD=1`.
pub(super) struct GitHubProxy {
    url: String,
    config: Arc<rustls::ServerConfig>,
    /// Holds `ca.pem`, which `SSL_CERT_FILE` points at, until the proxy drops.
    certs: tempfile::TempDir,
    token: String,
    transport: Arc<dyn Transport>,
    replay: Option<Arc<Replay>>,
    /// `CONNECT` targets refused so far.
    refused: Mutex<Vec<String>>,
    /// Tunneled requests the cassette could not answer.
    unmatched: Mutex<Vec<String>>,
}

impl GitHubProxy {
    /// Bind the proxy and its cassette, `fixtures/tools/{cassette}.json`, and
    /// start serving on a std thread.
    pub(super) fn start(cassette: &str) -> Arc<Self> {
        let token = if patchula_testkit::recording() {
            std::env::var(patchula_testkit::GH_TOKEN_VAR)
                .or_else(|_| std::env::var(patchula_testkit::GITHUB_TOKEN_VAR))
                .expect("fixture recording requires an existing GITHUB_TOKEN or GH_TOKEN")
        } else {
            "offline-test-credential".to_string()
        };
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../../fixtures/tools/{cassette}.json"));
        let options = FixtureOptions {
            redactor: Redactor::default().with_literals([token.clone()]),
            ..FixtureOptions::default()
        };
        let (transport, replay): (Arc<dyn Transport>, Option<Arc<Replay>>) =
            if patchula_testkit::recording() {
                (
                    Arc::new(Recorder::new(
                        &path,
                        options,
                        Arc::new(patchula_http::Network::new().unwrap()),
                        "Recorded by patchula from the request URLs; never imported from other bots"
                            .into(),
                    )),
                    None,
                )
            } else {
                let replay = Arc::new(Replay::open(&path, options).unwrap());
                (replay.clone(), Some(replay))
            };
        // A CA the run trusts through SSL_CERT_FILE, and a leaf for
        // api.github.com it issues; both are generated per test.
        let ca_key = rcgen::KeyPair::generate().unwrap();
        let mut ca_params = rcgen::CertificateParams::default();
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let ca = ca_params.self_signed(&ca_key).unwrap();
        let leaf_key = rcgen::KeyPair::generate().unwrap();
        let mut leaf =
            rcgen::CertificateParams::new(vec![GITHUB.split(':').next().unwrap().into()]).unwrap();
        leaf.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
        leaf.use_authority_key_identifier_extension = true;
        let issuer = rcgen::Issuer::from_params(&ca_params, &ca_key);
        let cert = leaf.signed_by(&leaf_key, &issuer).unwrap();
        let mut config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert.der().clone()],
                rustls::pki_types::PrivateKeyDer::Pkcs8(leaf_key.serialize_der().into()),
            )
            .unwrap();
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        let certs = tempfile::tempdir().unwrap();
        std::fs::write(certs.path().join("ca.pem"), ca.pem()).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy = Arc::new(Self {
            url: format!("http://{}", listener.local_addr().unwrap()),
            config: Arc::new(config),
            certs,
            token,
            transport,
            replay,
            refused: Mutex::new(Vec::new()),
            unmatched: Mutex::new(Vec::new()),
        });
        let serving = proxy.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let proxy = serving.clone();
                std::thread::spawn(move || proxy.serve(stream));
            }
        });
        proxy
    }
    /// The CA certificate file `SSL_CERT_FILE` points at.
    pub(super) fn cert_file(&self) -> PathBuf {
        self.certs.path().join("ca.pem")
    }
    /// Every recorded interaction was consumed and no host was refused or
    /// missed: the run reached only the GitHub its cassette describes.
    pub(super) fn assert_finished(&self) {
        if let Some(replay) = &self.replay {
            replay.assert_finished().unwrap();
        }
        assert!(
            self.refused.lock().unwrap().is_empty(),
            "refused CONNECTs: {:?}",
            self.refused.lock().unwrap()
        );
        assert!(
            self.unmatched.lock().unwrap().is_empty(),
            "unmatched requests: {:?}",
            self.unmatched.lock().unwrap()
        );
    }
    /// One accepted connection: the `CONNECT` handshake, then the tunnel.
    fn serve(&self, mut stream: TcpStream) {
        let Some(head) = read_head(&mut stream) else {
            return;
        };
        let head = String::from_utf8_lossy(&head);
        let mut words = head.split_whitespace();
        let method = words.next().unwrap_or_default();
        let target = words.next().unwrap_or_default();
        if method != "CONNECT" || target != GITHUB {
            self.refused
                .lock()
                .unwrap()
                .push(format!("{method} {target}"));
            let _ = stream.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n");
            return;
        }
        if stream
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .is_err()
        {
            return;
        }
        self.tunnel(stream);
    }
    /// TLS over `stream`, then one cassette exchange per request until the
    /// client disconnects or a request cannot be served.
    fn tunnel(&self, mut stream: TcpStream) {
        let Ok(mut conn) = rustls::ServerConnection::new(self.config.clone()) else {
            return;
        };
        let mut tls = rustls::Stream::new(&mut conn, &mut stream);
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return;
        };
        while let Some(request) = Request::read(&mut tls) {
            let Some(reqwest) = request.reqwest() else {
                break;
            };
            match runtime.block_on(self.transport.send(reqwest)) {
                Ok(response) => {
                    if response.write(&mut tls).is_err() {
                        break;
                    }
                }
                Err(_) => {
                    self.unmatched.lock().unwrap().push(request.line.clone());
                    let _ = tls.write_all(
                        b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                    break;
                }
            }
        }
    }
}

/// A plain HTTP request read out of the tunnel.
struct Request {
    line: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    /// `METHOD target HTTP/1.1`, the headers, and the `Content-Length` body.
    /// Reads a byte at a time so nothing buffered ahead of the head is lost.
    fn read(tls: &mut impl Read) -> Option<Self> {
        let head = read_head(tls)?;
        let head = String::from_utf8_lossy(&head);
        let mut lines = head.split("\r\n");
        let line = lines.next()?.to_string();
        let headers: Vec<(String, String)> = lines
            .take_while(|line| !line.is_empty())
            .filter_map(|line| {
                line.split_once(':')
                    .map(|(name, value)| (name.trim().to_string(), value.trim().to_string()))
            })
            .collect();
        let length: usize = headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, value)| value.parse().ok())
            .unwrap_or(0);
        let mut body = vec![0; length];
        tls.read_exact(&mut body).ok()?;
        Some(Self {
            line,
            headers,
            body,
        })
    }
    /// The same request as a `reqwest::Request` under `api.github.com`, so
    /// the cassette matches it on method, URL, selected headers and body.
    fn reqwest(&self) -> Option<reqwest::Request> {
        let mut words = self.line.split_whitespace();
        let method = reqwest::Method::from_bytes(words.next()?.as_bytes()).ok()?;
        let target = words.next()?;
        let url = if target.starts_with("http") {
            target.to_string()
        } else {
            format!("https://api.github.com{target}")
        };
        let mut request = reqwest::Request::new(method, reqwest::Url::parse(&url).ok()?);
        for (name, value) in &self.headers {
            if HOP_BY_HOP.iter().any(|hop| name.eq_ignore_ascii_case(hop)) {
                continue;
            }
            request.headers_mut().append(
                HeaderName::from_bytes(name.as_bytes()).ok()?,
                HeaderValue::from_str(value).ok()?,
            );
        }
        if !self.body.is_empty() {
            *request.body_mut() = Some(self.body.clone().into());
        }
        Some(request)
    }
}

/// Headers that belong to this hop, never to the cassette or the request
/// passed upstream.
const HOP_BY_HOP: &[&str] = &[
    "host",
    "content-length",
    "connection",
    "proxy-connection",
    "keep-alive",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// A recorded response's headers that must not go back down the tunnel:
/// the cassette stores the decoded body under its own `Content-Length`.
const RESPONSE_HOP_BY_HOP: &[&str] = &[
    "content-length",
    "content-encoding",
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

trait WriteResponse {
    fn write(&self, tls: &mut impl Write) -> std::io::Result<()>;
}

impl WriteResponse for patchula_http::Response {
    fn write(&self, tls: &mut impl Write) -> std::io::Result<()> {
        let mut head = format!(
            "HTTP/1.1 {} {}\r\n",
            self.status.as_u16(),
            self.status.canonical_reason().unwrap_or("")
        );
        for (name, value) in &self.headers {
            let name = name.as_str();
            if RESPONSE_HOP_BY_HOP
                .iter()
                .any(|hop| name.eq_ignore_ascii_case(hop))
            {
                continue;
            }
            if let Ok(value) = value.to_str() {
                head.push_str(&format!("{name}: {value}\r\n"));
            }
        }
        head.push_str(&format!("Content-Length: {}\r\n\r\n", self.body.len()));
        tls.write_all(head.as_bytes())?;
        tls.write_all(&self.body)
    }
}

/// The head of a request or `CONNECT`: bytes up to and including `\r\n\r\n`.
/// One byte at a time, so bytes of the following TLS handshake or request
/// body are never consumed early.
fn read_head(stream: &mut impl Read) -> Option<Vec<u8>> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while head.len() < 16 * 1024 {
        match stream.read(&mut byte) {
            Ok(0) | Err(_) => return None,
            Ok(_) => {
                head.push(byte[0]);
                if head.ends_with(b"\r\n\r\n") {
                    return Some(head);
                }
            }
        }
    }
    None
}

/// The developer's mise store, where the real tool tests take tools from.
fn mise_store() -> PathBuf {
    std::env::var_os("MISE_DATA_DIR").map_or_else(
        || PathBuf::from(std::env::var_os("HOME").unwrap()).join(".local/share/mise"),
        PathBuf::from,
    )
}

/// The `tool@version` spec the task tools install, installed into the
/// developer's store first when a local run lacks it.
fn ensure_tool() -> String {
    let spec = format!(
        "{}@{}",
        tool::ACTIONS_LOCK_SPEC,
        default_version(tool::ACTIONS_LOCK_SPEC)
    );
    let probe = Command::new("mise")
        .arg("--no-config")
        .arg("exec")
        .arg(&spec)
        .arg("--")
        .arg("gh-actions-lock")
        .arg("--version")
        .env("MISE_DATA_DIR", mise_store())
        .env("MISE_EXEC_AUTO_INSTALL", "false")
        .output()
        .unwrap();
    if !probe.status.success() {
        let status = Command::new("mise")
            .args(["install", &spec])
            .env("MISE_DATA_DIR", mise_store())
            .status()
            .unwrap();
        assert!(status.success(), "mise install {spec} failed");
    }
    spec
}

/// `gh-actions-lock` the way the lockfile job will call it: the pinned spec
/// through `mise exec`, a scratch `HOME`, `CI=false` for the tool's quick
/// retry policy, and every GitHub read through `proxy`. Ambient tokens and
/// proxy variables are stripped, so replay never sees a real credential and
/// nothing reaches outside the proxy.
fn actions_lock(tree: &Path, proxy: &GitHubProxy, args: &[&str]) -> (bool, String, String) {
    let home = tempfile::tempdir().unwrap();
    let mut command = Command::new("mise");
    command
        .arg("--no-config")
        .arg("exec")
        .arg(ensure_tool())
        .arg("--")
        .arg("gh-actions-lock")
        .args(args)
        .current_dir(tree);
    command.env_clear();
    for (key, value) in std::env::vars() {
        let upper = key.to_uppercase();
        if !(upper.starts_with("GH_")
            || upper.contains("TOKEN")
            || upper.ends_with("_PROXY")
            || upper == "PROXY"
            || upper == "CI"
            || upper == "HOME"
            || upper.starts_with("XDG_"))
        {
            command.env(&key, &value);
        }
    }
    command
        .env("MISE_DATA_DIR", mise_store())
        .env("MISE_AUTO_INSTALL", "false")
        .env("MISE_EXEC_AUTO_INSTALL", "false")
        .env("MISE_OFFLINE", "1")
        .env("HTTPS_PROXY", &proxy.url)
        .env("https_proxy", &proxy.url)
        .env("SSL_CERT_FILE", proxy.cert_file())
        .env("GH_TOKEN", &proxy.token)
        .env("CI", "false")
        .env("GH_HOST", "github.com")
        .env("GH_REPO", "octo/fixture")
        .env("GH_ACTIONS_LOCK_REPO_ROOT", tree)
        .env("HOME", home.path())
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .env("NO_COLOR", "1")
        .env("TERM", "dumb");
    let output = command.output().unwrap();
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// The files `gh-actions-lock` sees, written under `.github/workflows/` in a
/// fresh tree.
fn write_tree(dir: &Path, files: &BTreeMap<String, String>) {
    for (path, text) in files {
        let path = dir.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
}

fn workflow(step: &str) -> String {
    format!(
        "name: ci\non: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: {step}\n"
    )
}

fn read(tree: &Path, path: &str) -> String {
    std::fs::read_to_string(tree.join(path)).unwrap()
}

/// Without `--no-narrow` the plain run narrows a major-only ref:
/// `actions/checkout@v7` is rewritten to the newest point tag in the
/// workflow itself. `--no-narrow` exists because of this, so the default
/// is pinned here.
#[tokio::test]
async fn real_actions_lock_narrows_a_major_tag() {
    let proxy = GitHubProxy::start("actions-lock-narrow");
    let tree = tempfile::tempdir().unwrap();
    write_tree(
        tree.path(),
        &BTreeMap::from([(
            ".github/workflows/ci.yml".into(),
            workflow("actions/checkout@v7"),
        )]),
    );
    let (ok, stdout, stderr) = actions_lock(tree.path(), &proxy, &[]);
    assert!(ok, "{stdout}\n{stderr}");
    let ci = read(tree.path(), ".github/workflows/ci.yml");
    assert!(ci.contains("actions/checkout@v7.0.1"), "{ci}");
    let lock = read(tree.path(), ".github/workflows/actions.lock");
    assert!(lock.contains("'actions/checkout@v7.0.1'"), "{lock}");
    proxy.assert_finished();
}

/// And it unpins a SHA the workflow pins: the pin and its version comment
/// are rewritten to the tag they resolved to.
#[tokio::test]
async fn real_actions_lock_unpins_a_sha() {
    const V700: &str = "9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0";
    let proxy = GitHubProxy::start("actions-lock-unpin");
    let tree = tempfile::tempdir().unwrap();
    write_tree(
        tree.path(),
        &BTreeMap::from([(
            ".github/workflows/ci.yml".into(),
            workflow(&format!("actions/checkout@{V700} # v7.0.0")),
        )]),
    );
    let (ok, stdout, stderr) = actions_lock(tree.path(), &proxy, &[]);
    assert!(ok, "{stdout}\n{stderr}");
    let ci = read(tree.path(), ".github/workflows/ci.yml");
    assert!(ci.contains("actions/checkout@v7.0.0"), "{ci}");
    assert!(!ci.contains(V700), "{ci}");
    assert!(!ci.contains('#'), "{ci}");
    proxy.assert_finished();
}

/// A sub-path action's lock key drops the sub-path: `actions/cache/restore`
/// records an `actions/cache` dependency, so the lock names the repository.
#[tokio::test]
async fn real_actions_lock_sub_path_key() {
    let proxy = GitHubProxy::start("actions-lock-subpath");
    let tree = tempfile::tempdir().unwrap();
    write_tree(
        tree.path(),
        &BTreeMap::from([(
            ".github/workflows/ci.yml".into(),
            "name: ci\non: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/cache/restore@v4\n        with:\n          path: ~/.cache\n          key: x\n".into(),
        )]),
    );
    let (ok, stdout, stderr) = actions_lock(tree.path(), &proxy, &[]);
    assert!(ok, "{stdout}\n{stderr}");
    let lock = read(tree.path(), ".github/workflows/actions.lock");
    assert!(lock.contains("'actions/cache@v4.3.0'"), "{lock}");
    assert!(!lock.contains("restore"), "{lock}");
    proxy.assert_finished();
}

/// The flags the actions.lock job will use exit 0 with `valid: true` on a
/// workflow the lock already lists: the fixture works end to end.
#[tokio::test]
async fn real_actions_lock_flags_exit_valid() {
    let proxy = GitHubProxy::start("actions-lock-flags");
    let tree = tempfile::tempdir().unwrap();
    write_tree(
        tree.path(),
        &BTreeMap::from([
            (
                ".github/workflows/ci.yml".into(),
                workflow("actions/checkout@v7.0.1"),
            ),
            (
                ".github/workflows/actions.lock".into(),
                "# This file is machine-generated by `gh actions-lock`.\n# Do not edit by hand; run `gh actions-lock` to update.\n# Docs: https://gh.io/actions-lockfile\nversion: 'v0.0.3'\nworkflows:\n    '.github/workflows/ci.yml':\n        - 'actions/checkout@v7.0.1'\ndependencies:\n    'actions/checkout@v7.0.1':\n        ref: 'v7.0.1'\n        commit: 'sha1-3d3c42e5aac5ba805825da76410c181273ba90b1'\n        owner_id: 44036562\n        repo_id: 197814629\n".into(),
            ),
        ]),
    );
    let (ok, stdout, stderr) = actions_lock(
        tree.path(),
        &proxy,
        &[
            "--no-interactive",
            "--no-onboard",
            "--no-narrow",
            "--ignore-local-actions",
            "--json=valid,findings",
        ],
    );
    assert!(ok, "{stdout}\n{stderr}");
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(json["valid"], true, "{stdout}");
    assert_eq!(json["findings"], serde_json::json!([]), "{stdout}");
    // A frozen lock and workflow stay untouched.
    let ci = read(tree.path(), ".github/workflows/ci.yml");
    assert!(ci.contains("actions/checkout@v7.0.1"), "{ci}");
    proxy.assert_finished();
}
