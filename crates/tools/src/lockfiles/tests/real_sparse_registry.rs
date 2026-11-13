//! A local sparse Cargo registry, on the model of `real_python_indexes.rs`:
//! a TCP server that answers `config.json` and index files whose `pubtime`
//! is set relative to now, and `.crate` files when Cargo asks for them. The
//! run's `CARGO_HOME` gets a `replace-with` source pointing crates.io at it,
//! and a dead proxy for everything else keeps it the only reachable index.
//!
//! Cargo 1.100 reads `pubtime` for the publish ages behind
//! `registry.global-min-publish-age`. These tests pin what the Count's
//! runner and the owner's own Cargo do with them today: the cooldown never
//! reaches the runner, so it locks what the owner would hold back, while a
//! `.cargo/config.toml` in a directory above the run's driver leaks in.
//! The registry's builder API is the case generator the reader (PR 2) and
//! the runner environment (PR 3) generate their matrices from.
use super::*;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;

/// One release of a crate: its version, how long ago the index says it was
/// published (`None` serves no `pubtime` at all, which Cargo never holds
/// back), and its dependencies.
pub(super) struct Release {
    vers: String,
    age: Option<Duration>,
    deps: Vec<(String, String)>,
}

impl Release {
    pub(super) fn new(vers: &str) -> Self {
        Self {
            vers: vers.into(),
            age: Some(Duration::ZERO),
            deps: Vec::new(),
        }
    }
    /// Published `secs` before the index file is served.
    pub(super) fn age(mut self, secs: u64) -> Self {
        self.age = Some(Duration::from_secs(secs));
        self
    }
    /// Published `days` before the index file is served.
    pub(super) fn days(mut self, days: u64) -> Self {
        self.age = Some(Duration::from_secs(days * 86_400));
        self
    }
    /// The index entry carries no `pubtime`.
    pub(super) fn undated(mut self) -> Self {
        self.age = None;
        self
    }
    /// This release requires `name` at `req`, in its index `deps` and in
    /// the manifest its `.crate` file serves.
    pub(super) fn dep(mut self, name: &str, req: &str) -> Self {
        self.deps.push((name.into(), req.into()));
        self
    }
}

/// A sparse index over loopback: crates and versions can be added cheaply,
/// so one server covers any number of generated cases.
pub(super) struct SparseRegistry {
    crates: Mutex<BTreeMap<String, Vec<Release>>>,
    base: String,
    /// Every path Cargo asked for, in order.
    requests: Mutex<Vec<String>>,
}

impl SparseRegistry {
    pub(super) fn start() -> Arc<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let registry = Arc::new(Self {
            crates: Mutex::new(BTreeMap::new()),
            base: format!("http://{}", listener.local_addr().unwrap()),
            requests: Mutex::new(Vec::new()),
        });
        let serving = registry.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let registry = serving.clone();
                std::thread::spawn(move || registry.respond(stream));
            }
        });
        registry
    }
    /// Record one release of `name`. `pubtime` is rendered when the index
    /// file is served, so a release keeps its age however long the test runs.
    pub(super) fn publish(&self, name: &str, release: Release) -> &Self {
        self.crates
            .lock()
            .unwrap()
            .entry(name.to_owned())
            .or_default()
            .push(release);
        self
    }
    /// The `sparse+` index URL a `[source.*]` table names.
    pub(super) fn index_url(&self) -> String {
        format!("sparse+{}/", self.base)
    }
    /// The paths Cargo has fetched so far.
    pub(super) fn requested(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
    /// The `CARGO_HOME/config.toml` that points crates.io at this registry.
    pub(super) fn write_cargo_home(&self, cargo_home: &Path) {
        std::fs::create_dir_all(cargo_home).unwrap();
        std::fs::write(
            cargo_home.join("config.toml"),
            format!(
                "[source.crates-io]\nreplace-with = \"fixture\"\n[source.fixture]\nregistry = \"{}\"\n",
                self.index_url()
            ),
        )
        .unwrap();
    }
    fn respond(&self, mut stream: TcpStream) {
        let Ok(clone) = stream.try_clone() else {
            return;
        };
        let mut reader = BufReader::new(clone);
        let mut request = String::new();
        // A connection that closes before its request line is not a request.
        if reader.read_line(&mut request).unwrap_or(0) == 0 {
            return;
        }
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header).unwrap_or(0) <= 2 {
                break;
            }
        }
        let mut words = request.split_whitespace();
        let method = words.next().unwrap_or_default();
        let path = words
            .next()
            .unwrap_or_default()
            .split(['?', '#'])
            .next()
            .unwrap_or_default()
            .to_owned();
        self.requests.lock().unwrap().push(path.clone());
        let (status, kind, body) = match self.route(&path) {
            Some((kind, body)) => ("200 OK", kind, body),
            None => ("404 Not Found", "text/plain", b"not found".to_vec()),
        };
        let head = format!(
            "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(head.as_bytes());
        if method != "HEAD" {
            let _ = stream.write_all(&body);
        }
    }
    /// `config.json`, a `.crate` download, or an index file: the file's last
    /// segment is the crate's name, as the sparse index format fixes it.
    fn route(&self, path: &str) -> Option<(&'static str, Vec<u8>)> {
        let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
        match parts.as_slice() {
            ["config.json"] => Some((
                "application/json",
                format!(
                    "{{\"dl\":\"{}/dl/{{crate}}/{{version}}/{{crate}}-{{version}}.crate\",\"api\":\"{}\"}}",
                    self.base, self.base
                )
                .into_bytes(),
            )),
            ["dl", name, vers, _] => self
                .crates
                .lock()
                .unwrap()
                .get(*name)
                .and_then(|releases| releases.iter().find(|r| r.vers == *vers))
                .map(|release| {
                    (
                        "application/octet-stream",
                        crate_file(name, vers, &release.deps),
                    )
                }),
            // Cargo lowercases index paths; the file keeps the published name.
            [.., name] => self
                .crates
                .lock()
                .unwrap()
                .iter()
                .find(|(published, _)| published.eq_ignore_ascii_case(name))
                .map(|(published, releases)| {
                    ("text/plain", index_file(published, releases).into_bytes())
                }),
            _ => None,
        }
    }
}

/// One release's line of the index file: `pubtime` as RFC 3339 UTC, the
/// same shape crates.io writes.
fn index_file(name: &str, releases: &[Release]) -> String {
    let mut out = String::new();
    for release in releases {
        let mut entry = serde_json::json!({
            "name": name,
            "vers": release.vers,
            "deps": release
                .deps
                .iter()
                .map(|(dep, req)| serde_json::json!({
                    "name": dep,
                    "req": req,
                    "features": [],
                    "optional": false,
                    "default_features": true,
                    "target": null,
                    "kind": "normal",
                }))
                .collect::<Vec<_>>(),
            "cksum": patchula_manager::hash(&crate_file(name, &release.vers, &release.deps)),
            "features": {},
            "yanked": false,
        });
        if let Some(age) = release.age {
            let published = chrono::Utc::now() - chrono::Duration::from_std(age).unwrap();
            entry["pubtime"] =
                serde_json::Value::String(published.format("%Y-%m-%dT%H:%M:%SZ").to_string());
        }
        out.push_str(&serde_json::to_string(&entry).unwrap());
        out.push('\n');
    }
    out
}

/// A `.crate` file: a gzipped tar with the manifest (carrying `deps`, as a
/// real crate would) and one source file, no compression library needed.
fn crate_file(name: &str, vers: &str, deps: &[(String, String)]) -> Vec<u8> {
    let manifest = format!(
        "[package]\nname = \"{name}\"\nversion = \"{vers}\"\nedition = \"2021\"\n[dependencies]\n{}",
        deps.iter()
            .map(|(dep, req)| format!("{dep} = \"{req}\"\n"))
            .collect::<String>()
    )
    .into_bytes();
    let tar = tar(&[
        (format!("{name}-{vers}/Cargo.toml"), manifest),
        (
            format!("{name}-{vers}/src/lib.rs"),
            b"// fixture\n".to_vec(),
        ),
    ]);
    gzip(&tar)
}

/// A ustar archive of `entries`.
fn tar(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
    fn field(header: &mut [u8], range: std::ops::Range<usize>, value: &[u8]) {
        header[range].copy_from_slice(value);
    }
    fn octal(header: &mut [u8], range: std::ops::Range<usize>, value: u64) {
        let text = format!("{:0>width$o}", value, width = range.len() - 1);
        header[range.start..range.start + text.len()].copy_from_slice(text.as_bytes());
    }
    let mut out = Vec::new();
    for (name, data) in entries {
        assert!(name.len() <= 100, "tar name too long: {name}");
        let mut header = [0u8; 512];
        field(&mut header, 0..name.len(), name.as_bytes());
        octal(&mut header, 100..108, 0o644);
        octal(&mut header, 108..116, 0);
        octal(&mut header, 116..124, 0);
        octal(&mut header, 124..136, data.len() as u64);
        octal(&mut header, 136..148, 0);
        header[148..156].fill(b' ');
        header[156] = b'0';
        field(&mut header, 257..263, b"ustar\0");
        field(&mut header, 263..265, b"00");
        field(&mut header, 265..269, b"user");
        let sum: u64 = header.iter().map(|b| u64::from(*b)).sum();
        let text = format!("{sum:06o}\0 ");
        header[148..156].copy_from_slice(text.as_bytes());
        out.extend_from_slice(&header);
        out.extend_from_slice(data);
        out.resize(out.len() + (512 - data.len() % 512) % 512, 0);
    }
    out.resize(out.len() + 1024, 0);
    out
}

/// A gzip member wrapping `data` in uncompressed deflate blocks.
fn gzip(data: &[u8]) -> Vec<u8> {
    let mut out = b"\x1f\x8b\x08\x00\x00\x00\x00\x00\x00\xff".to_vec();
    for (last, chunk) in data
        .chunks(65_535)
        .enumerate()
        .map(|(i, c)| (i == data.len().div_ceil(65_535).saturating_sub(1), c))
    {
        out.push(u8::from(last));
        let len = chunk.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(chunk);
    }
    if data.is_empty() {
        out.extend_from_slice(&[1, 0, 0, 0xff, 0xff]);
    }
    out.extend_from_slice(&crc32fast::hash(data).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out
}

/// The crates these tests share: `cool-b` spans a semver boundary with
/// recent releases, `cool-mid` and `cool-wrapper` pull transitive crates,
/// `cool-fresh` was published minutes ago and `cool-undated` has a release
/// the index never dates.
fn registry() -> Arc<SparseRegistry> {
    let registry = SparseRegistry::start();
    registry
        .publish("cool-b", Release::new("1.0.0").days(30))
        .publish("cool-b", Release::new("1.5.0").age(52 * 3600))
        .publish("cool-b", Release::new("2.0.0").age(52 * 3600))
        .publish("cool-b", Release::new("2.1.0").age(30 * 3600))
        .publish(
            "cool-mid",
            Release::new("1.0.0")
                .days(30)
                .dep("cool-b", "1")
                .dep("cool-trans", "1"),
        )
        .publish("cool-trans", Release::new("1.0.0").days(30))
        .publish("cool-trans", Release::new("1.5.0").days(1))
        .publish(
            "cool-wrapper",
            Release::new("1.0.0").days(30).dep("cool-trans", "1"),
        )
        .publish("cool-fresh", Release::new("1.0.0").days(30))
        .publish("cool-fresh", Release::new("1.1.0").age(300))
        .publish("cool-undated", Release::new("1.0.0").days(30))
        .publish("cool-undated", Release::new("1.2.0").undated());
    registry
}

fn manifest(deps: &[(&str, &str)]) -> String {
    format!(
        "[package]\nname = \"fixture\"\nversion = \"1.0.0\"\nedition = \"2024\"\n\n[dependencies]\n{}",
        deps.iter()
            .map(|(name, req)| format!("{name} = \"{req}\"\n"))
            .collect::<String>()
    )
}

/// A committed lock with `fixture` and each `(name, version, deps)` entry;
/// a dep entry is `"name version"`, as Cargo itself writes them.
fn lock(entries: &[(&str, &str, &[&str])]) -> String {
    let mut out =
        "version = 4\n\n[[package]]\nname = \"fixture\"\nversion = \"1.0.0\"\n".to_owned();
    if !entries.is_empty() {
        let roots: Vec<&str> = entries.iter().map(|(name, _, _)| *name).collect();
        out.push_str(&format!("dependencies = {roots:?}\n"));
    }
    for (name, version, deps) in entries {
        out.push_str(&format!(
            "\n[[package]]\nname = \"{name}\"\nversion = \"{version}\"\nsource = \"{CRATES_IO}\"\n"
        ));
        if !deps.is_empty() {
            let rendered: Vec<String> = deps
                .iter()
                .map(|dep| {
                    let dep_version = entries
                        .iter()
                        .find(|(n, _, _)| n == dep)
                        .map(|(_, v, _)| *v)
                        .unwrap();
                    format!("{dep} {dep_version}")
                })
                .collect();
            out.push_str(&format!("dependencies = {rendered:?}\n"));
        }
    }
    out
}

/// The developer's mise store, where the real tool tests take tools from.
fn mise_store() -> PathBuf {
    std::env::var_os("MISE_DATA_DIR").map_or_else(
        || PathBuf::from(std::env::var_os("HOME").unwrap()).join(".local/share/mise"),
        PathBuf::from,
    )
}

/// `mise exec` a real `tool@version` the test needs: install it into the
/// developer's store when it is not already there, as the task tools are
/// installed ahead of `test:tools` on CI.
fn ensure_tool(spec: &str) {
    let store = mise_store();
    let bin = match spec.split('@').next().unwrap() {
        "rust" => "rustc",
        tool => tool,
    };
    // A rustup proxy answers `--version` from whatever toolchain is the
    // default, so the probe must report the requested version itself.
    let version = spec.split('@').nth(1).unwrap();
    let probe = Command::new("mise")
        .arg("--no-config")
        .arg("exec")
        .arg(spec)
        .arg("--")
        .arg(bin)
        .arg("--version")
        .env("MISE_DATA_DIR", &store)
        .env("MISE_EXEC_AUTO_INSTALL", "false")
        .output()
        .unwrap();
    let installed = probe.status.success()
        && String::from_utf8_lossy(&probe.stdout)
            .split_whitespace()
            .any(|word| word == version);
    if installed {
        return;
    }
    let status = Command::new("mise")
        .args(["install", spec])
        .env("MISE_DATA_DIR", &store)
        .status()
        .unwrap();
    assert!(status.success(), "mise install {spec} failed");
}

/// Cargo the way a project owner runs it: `cargo <args>` inside `dir`, on
/// `rust`, with a fresh `CARGO_HOME` wired to `registry`, the ambient Cargo
/// and proxy environment stripped, and every host but this one behind a
/// dead proxy. Returns success and the combined output.
fn owner_cargo(
    dir: &Path,
    registry: &SparseRegistry,
    rust: &str,
    args: &[&str],
    env: &[(&str, &str)],
) -> (bool, String) {
    let temp = tempfile::tempdir().unwrap();
    let cargo_home = temp.path().join("cargo-home");
    registry.write_cargo_home(&cargo_home);
    let mut command = Command::new("mise");
    command
        .arg("--no-config")
        .arg("exec")
        .arg(format!("rust@{rust}"))
        .arg("--")
        .arg("cargo")
        .args(args)
        .current_dir(dir);
    command.env_clear();
    for (key, value) in std::env::vars() {
        let upper = key.to_uppercase();
        if !(upper.starts_with("CARGO")
            || upper == "RUSTUP_TOOLCHAIN"
            || upper.ends_with("_PROXY")
            || upper == "PROXY")
        {
            command.env(&key, &value);
        }
    }
    command
        .env("MISE_DATA_DIR", mise_store())
        .env("MISE_AUTO_INSTALL", "false")
        .env("MISE_EXEC_AUTO_INSTALL", "false")
        .env("CARGO_HOME", &cargo_home)
        .env("CI", "true")
        .env("NO_PROXY", "127.0.0.1,localhost")
        .env("no_proxy", "127.0.0.1,localhost");
    for key in [
        "HTTPS_PROXY",
        "HTTP_PROXY",
        "ALL_PROXY",
        "https_proxy",
        "http_proxy",
        "all_proxy",
    ] {
        command.env(key, "http://127.0.0.1:9");
    }
    for (key, value) in env {
        command.env(key, value);
    }
    let output = command.output().unwrap();
    (
        output.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

/// The files the Count's git checkout sees, written to `dir` for the
/// owner's own Cargo run. `dir` holds exactly `files` afterwards: a file an
/// earlier case wrote, a cooldown config or a lock, must not decide this one.
fn write_tree(dir: &Path, files: &BTreeMap<String, String>) {
    if dir.exists() {
        std::fs::remove_dir_all(dir).unwrap();
    }
    std::fs::create_dir_all(dir).unwrap();
    for (path, text) in files {
        let path = dir.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
}

/// `files` for a one-crate project on `dep` at `req`, locked at `version`,
/// with `extra` files merged in.
fn project(dep: (&str, &str), version: &str, extra: &[(&str, &str)]) -> BTreeMap<String, String> {
    let mut files = BTreeMap::from([
        ("Cargo.toml".to_string(), manifest(&[dep])),
        ("src/lib.rs".to_string(), "// fixture\n".to_string()),
        ("Cargo.lock".to_string(), lock(&[(dep.0, version, &[])])),
    ]);
    for (path, text) in extra {
        files.insert((*path).to_string(), (*text).to_string());
    }
    files
}

/// One targeted update of `dep`'s crate `current` to `target` through the
/// runner, exactly as `real_cargo_and_go_targeted_...` drives it.
async fn targeted(
    registry: &Arc<SparseRegistry>,
    files: &BTreeMap<String, String>,
    dep: (&str, &str),
    current: &str,
    target: &str,
    new_manifest: String,
) -> (patchula_update::Plan, Snapshot) {
    let executor = fixture_sparse(files.clone(), registry.clone());
    let temp = tempfile::tempdir().unwrap();
    let runner =
        Runner::new(temp.path().join("worktrees"), "offline-installation").with_executor(executor);
    let mut snapshot = snapshot(files);
    let config = config();
    let mut p = maintenance(&repo(), &config, &snapshot, None, monday())
        .unwrap()
        .unwrap();
    let mut draft = Draft::from(p);
    draft.lock_jobs_mut()[0].scope = Scope::Package {
        package: dep.0.into(),
        current: Some(current.into()),
        target: Some(target.into()),
    };
    let old = &files["Cargo.toml"];
    p = draft
        .update()
        .edit(
            "Cargo.toml",
            new_manifest,
            FileAction {
                path: "Cargo.toml".into(),
                blob_sha: blob_sha(old),
                diff: String::new(),
            },
        )
        .change(patchula_update::Change {
            registry: None,
            source: "crates".parse().unwrap(),
            package: dep.0.into(),
            file: "Cargo.toml".into(),
            current: current.into(),
            current_version: None,
            value: Some(target.into()),
            version: Some(target.into()),
            digest: None,
            kind: patchula_words::Update::Major,
            merge: false,
            labels: Vec::new(),
            release_notes: false,
            body_notes: Vec::new(),
            advisories: Vec::new(),
            image: None,
        })
        .build();
    prepare_one(&runner, &mut p, &mut snapshot, &config).await;
    (p, snapshot)
}

const COOLDOWN: &str = "[registry]\nglobal-min-publish-age = \"14 days\"\n";

/// With no cooldown anywhere, the runner locks every release regardless of
/// `pubtime`: the direct `--precise` update, the semver-boundary
/// `--workspace` then `--precise` path, a transitive crate newly locked and
/// lockfile maintenance all take the newest matching release.
#[tokio::test]
async fn real_cargo_locks_every_release_without_a_cooldown() {
    let registry = registry();

    // Direct `--precise`: 1.0.0 to 1.5.0, published two days ago.
    let files = project(("cool-b", "1.0"), "1.0.0", &[]);
    let (p, _) = targeted(
        &registry,
        &files,
        ("cool-b", "1.0"),
        "1.0.0",
        "1.5.0",
        manifest(&[("cool-b", "1.5.0")]),
    )
    .await;
    assert_eq!(p.action().status, BranchStatus::Planned, "{:?}", p.action());
    assert!(
        p.contents()["Cargo.lock"].contains("version = \"1.5.0\""),
        "{}",
        p.contents()["Cargo.lock"]
    );

    // The semver boundary: `cool-b = "1"` to `"2.0.0"`. The manifest's
    // package moves to the 2.0.0 target, and the entry cool-mid still
    // needs at `"1"` keeps the locked 1.0.0 — Cargo does not re-pick a
    // satisfying version.
    let mut files = project(("cool-b", "1"), "1.0.0", &[]);
    files.insert(
        "Cargo.toml".into(),
        manifest(&[("cool-b", "1"), ("cool-mid", "1")]),
    );
    files.insert(
        "Cargo.lock".into(),
        lock(&[
            ("cool-b", "1.0.0", &[]),
            ("cool-mid", "1.0.0", &["cool-b", "cool-trans"]),
            ("cool-trans", "1.0.0", &[]),
        ]),
    );
    let (p, _) = targeted(
        &registry,
        &files,
        ("cool-b", "1"),
        "1.0.0",
        "2.0.0",
        manifest(&[("cool-b", "2.0.0"), ("cool-mid", "1")]),
    )
    .await;
    assert_eq!(p.action().status, BranchStatus::Planned, "{:?}", p.action());
    let text = &p.contents()["Cargo.lock"];
    for version in ["1.0.0", "2.0.0"] {
        assert!(text.contains(&format!("version = \"{version}\"")), "{text}");
    }
    for version in ["1.5.0", "2.1.0"] {
        assert!(
            !text.contains(&format!("version = \"{version}\"")),
            "{text}"
        );
    }

    // A transitive crate newly locked gets the newest release: `cargo
    // update` for maintenance adds cool-trans 1.5.0, a day old.
    let files = BTreeMap::from([
        ("Cargo.toml".to_string(), manifest(&[("cool-wrapper", "1")])),
        ("src/lib.rs".to_string(), "// fixture\n".to_string()),
        (
            "Cargo.lock".to_string(),
            lock(&[("cool-wrapper", "1.0.0", &[])]),
        ),
    ]);
    let executor = fixture_sparse(files.clone(), registry.clone());
    let temp = tempfile::tempdir().unwrap();
    let runner =
        Runner::new(temp.path().join("worktrees"), "offline-installation").with_executor(executor);
    let mut shot = snapshot(&files);
    let config = config();
    let mut p = maintenance(&repo(), &config, &shot, None, monday())
        .unwrap()
        .unwrap();
    prepare_one(&runner, &mut p, &mut shot, &config).await;
    assert_eq!(p.action().status, BranchStatus::Planned, "{:?}", p.action());
    assert!(
        p.contents()["Cargo.lock"].contains(&"version = \"1.5.0\"".to_string()),
        "{}",
        p.contents()["Cargo.lock"]
    );

    // Lockfile maintenance takes the newest too: cool-fresh 1.1.0 is five
    // minutes old.
    let files = project(("cool-fresh", "1"), "1.0.0", &[]);
    let executor = fixture_sparse(files.clone(), registry.clone());
    let temp = tempfile::tempdir().unwrap();
    let runner =
        Runner::new(temp.path().join("worktrees"), "offline-installation").with_executor(executor);
    let mut shot = snapshot(&files);
    let mut p = maintenance(&repo(), &config, &shot, None, monday())
        .unwrap()
        .unwrap();
    prepare_one(&runner, &mut p, &mut shot, &config).await;
    assert_eq!(p.action().status, BranchStatus::Planned, "{:?}", p.action());
    assert!(
        p.contents()["Cargo.lock"].contains("version = \"1.1.0\""),
        "{}",
        p.contents()["Cargo.lock"]
    );
}

/// A repository that commits a `.cargo/config.toml` with
/// `[registry] global-min-publish-age`: the runner's Cargo never reads it —
/// the file is not materialized and Cargo's working directory is the
/// driver, not the repository — so the Count locks a release inside the
/// owner's cooldown. The owner's own Cargo on the same files refuses it.
#[tokio::test]
async fn real_cargo_locks_inside_a_committed_cooldown_the_owner_enforces() {
    let registry = registry();
    let files = project(
        ("cool-b", "1.0"),
        "1.0.0",
        &[(".cargo/config.toml", COOLDOWN)],
    );
    let (p, _) = targeted(
        &registry,
        &files,
        ("cool-b", "1.0"),
        "1.0.0",
        "1.5.0",
        manifest(&[("cool-b", "1.5.0")]),
    )
    .await;
    assert_eq!(p.action().status, BranchStatus::Planned, "{:?}", p.action());
    assert!(
        p.contents()["Cargo.lock"].contains("version = \"1.5.0\""),
        "{}",
        p.contents()["Cargo.lock"]
    );
    assert!(!p.contents().contains_key(".cargo/config.toml"));

    // The owner's Cargo on the same tree refuses the same target verbatim,
    // and a fresh resolve holds the release back, naming what it skipped.
    let temp = tempfile::tempdir().unwrap();
    let owner = temp.path().join("owner");
    write_tree(&owner, &files);
    let (ok, output) = owner_cargo(
        &owner,
        &registry,
        "1.100.0",
        &["update", "-p", "cool-b", "--precise", "1.5.0"],
        &[],
    );
    assert!(!ok);
    for line in [
        "error: failed to select a version for the requirement `cool-b = \"^1.0\"`",
        "version 1.5.0 is too new (published ",
        "minimum age 14 days)",
        "location searched: `fixture` index (which is replacing registry `crates-io`)",
        "required by package `fixture v1.0.0 (",
        "help: to use too-new packages anyways, re-resolve with `CARGO_RESOLVER_INCOMPATIBLE_PUBLISH_AGE=allow`",
    ] {
        assert!(output.contains(line), "{line:?} not in:\n{output}");
    }
    write_tree(
        &owner,
        &project(
            ("cool-b", "*"),
            "1.0.0",
            &[(".cargo/config.toml", COOLDOWN)],
        ),
    );
    std::fs::remove_file(owner.join("Cargo.lock")).unwrap();
    let (ok, output) = owner_cargo(&owner, &registry, "1.100.0", &["update"], &[]);
    assert!(ok, "{output}");
    assert!(output.contains("as of 14 days ago"), "{output}");
    assert!(
        output.contains("Adding cool-b v1.0.0 (available: v1.5.0, published "),
        "{output}"
    );
    let text = std::fs::read_to_string(owner.join("Cargo.lock")).unwrap();
    assert!(text.contains("version = \"1.0.0\""), "{text}");
    assert!(!text.contains("version = \"1.5.0\""), "{text}");
}

/// The one way a cooldown reaches the Count's Cargo today: a
/// `.cargo/config.toml` in a directory above the driver or cache directory
/// is a parent of Cargo's working directory, so its config applies. PR 3
/// ends the leak; today a too-new `--precise` fails the branch.
#[tokio::test]
async fn real_cargo_reads_cooldowns_above_the_driver_directory() {
    let registry = registry();
    let files = project(("cool-b", "1.0"), "1.0.0", &[]);
    for config_above in [false, true] {
        let executor = fixture_sparse(files.clone(), registry.clone());
        let temp = tempfile::tempdir().unwrap();
        if config_above {
            std::fs::create_dir_all(temp.path().join(".cargo")).unwrap();
            std::fs::write(temp.path().join(".cargo/config.toml"), COOLDOWN).unwrap();
        }
        let runner = Runner::new(temp.path().join("worktrees"), "offline-installation")
            .with_executor(executor);
        let mut snapshot = snapshot(&files);
        let config = config();
        let mut p = maintenance(&repo(), &config, &snapshot, None, monday())
            .unwrap()
            .unwrap();
        let mut draft = Draft::from(p);
        draft.lock_jobs_mut()[0].scope = Scope::Package {
            package: "cool-b".into(),
            current: Some("1.0.0".into()),
            target: Some("1.5.0".into()),
        };
        let old = &files["Cargo.toml"];
        p = draft
            .update()
            .edit(
                "Cargo.toml",
                manifest(&[("cool-b", "1.5.0")]),
                FileAction {
                    path: "Cargo.toml".into(),
                    blob_sha: blob_sha(old),
                    diff: String::new(),
                },
            )
            .change(patchula_update::Change {
                registry: None,
                source: "crates".parse().unwrap(),
                package: "cool-b".into(),
                file: "Cargo.toml".into(),
                current: "1.0.0".into(),
                current_version: None,
                value: Some("1.5.0".into()),
                version: Some("1.5.0".into()),
                digest: None,
                kind: patchula_words::Update::Major,
                merge: false,
                labels: Vec::new(),
                release_notes: false,
                body_notes: Vec::new(),
                advisories: Vec::new(),
                image: None,
            })
            .build();
        prepare_one(&runner, &mut p, &mut snapshot, &config).await;
        if !config_above {
            assert_eq!(p.action().status, BranchStatus::Planned, "{:?}", p.action());
            continue;
        }
        assert_eq!(p.action().status, BranchStatus::Error, "{:?}", p.action());
        let reason = p.action().reason.as_deref().unwrap_or_default();
        assert!(reason.contains("cargo"), "{reason}");
        assert!(reason.contains("1.5.0 is too new"), "{reason}");
        assert!(reason.contains("minimum age 14 days"), "{reason}");
    }
}

/// Rust 1.99.1 with the publish-age variables set runs clean and ignores
/// them, as the repository's own pinned toolchain may be older than the
/// runner's default: the too-new release is locked either way.
#[tokio::test]
async fn real_cargo_before_1_100_runs_clean_with_cooldown_keys_set() {
    let registry = registry();
    ensure_tool("rust@1.99.1");
    let files = project(
        ("cool-b", "1.0"),
        "1.0.0",
        &[(".tool-versions", "rust 1.99.1\n")],
    );
    // A parent-directory cooldown that reaches today's runner, ignored by
    // the 1.99.1 Cargo it drives.
    let executor = fixture_sparse(files.clone(), registry.clone());
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join(".cargo")).unwrap();
    std::fs::write(temp.path().join(".cargo/config.toml"), COOLDOWN).unwrap();
    let runner =
        Runner::new(temp.path().join("worktrees"), "offline-installation").with_executor(executor);
    let mut snapshot = snapshot(&files);
    let config = config();
    let mut p = maintenance(&repo(), &config, &snapshot, None, monday())
        .unwrap()
        .unwrap();
    let mut draft = Draft::from(p);
    draft.lock_jobs_mut()[0].scope = Scope::Package {
        package: "cool-b".into(),
        current: Some("1.0.0".into()),
        target: Some("1.5.0".into()),
    };
    let old = &files["Cargo.toml"];
    p = draft
        .update()
        .edit(
            "Cargo.toml",
            manifest(&[("cool-b", "1.5.0")]),
            FileAction {
                path: "Cargo.toml".into(),
                blob_sha: blob_sha(old),
                diff: String::new(),
            },
        )
        .change(patchula_update::Change {
            registry: None,
            source: "crates".parse().unwrap(),
            package: "cool-b".into(),
            file: "Cargo.toml".into(),
            current: "1.0.0".into(),
            current_version: None,
            value: Some("1.5.0".into()),
            version: Some("1.5.0".into()),
            digest: None,
            kind: patchula_words::Update::Major,
            merge: false,
            labels: Vec::new(),
            release_notes: false,
            body_notes: Vec::new(),
            advisories: Vec::new(),
            image: None,
        })
        .build();
    prepare_one(&runner, &mut p, &mut snapshot, &config).await;
    assert_eq!(p.action().status, BranchStatus::Planned, "{:?}", p.action());
    assert!(
        p.contents()["Cargo.lock"].contains("version = \"1.5.0\""),
        "{}",
        p.contents()["Cargo.lock"]
    );

    // The same keys by environment, verbatim: 1.99.1 warns and ignores.
    let temp = tempfile::tempdir().unwrap();
    let owner = temp.path().join("owner");
    write_tree(&owner, &project(("cool-b", "*"), "1.0.0", &[]));
    let (ok, output) = owner_cargo(
        &owner,
        &registry,
        "1.99.1",
        &["update"],
        &[
            ("CARGO_REGISTRY_GLOBAL_MIN_PUBLISH_AGE", "14 days"),
            ("CARGO_RESOLVER_INCOMPATIBLE_PUBLISH_AGE", "deny"),
        ],
    );
    assert!(ok, "{output}");
    for line in [
        "warning: ignoring `resolver.incompatible-publish-age` without `-Zmin-publish-age`",
        "warning: ignoring `registry.global-min-publish-age` without `-Zmin-publish-age`",
    ] {
        assert!(output.contains(line), "{line:?} not in:\n{output}");
    }
    let text = std::fs::read_to_string(owner.join("Cargo.lock")).unwrap();
    assert!(text.contains("version = \"2.1.0\""), "{text}");
}

/// Cargo 1.100's own texts, the ones its docs do not show, pinned verbatim
/// for the reader and the runner environment: the duration grammar, the
/// refusal under `deny`, the held-back note, the environment variable
/// against the config key, an undated release, and a too-new lock entry
/// that `cargo update` downgrades.
#[test]
fn real_cargo_publish_age_texts_are_pinned() {
    let registry = registry();
    let temp = tempfile::tempdir().unwrap();
    let owner = temp.path().join("owner");
    let files = project(("cool-b", "*"), "1.0.0", &[]);
    write_tree(&owner, &files);
    let env = |age: &'static str| [("CARGO_REGISTRY_GLOBAL_MIN_PUBLISH_AGE", age)];
    let deny = [
        ("CARGO_REGISTRY_GLOBAL_MIN_PUBLISH_AGE", "14 days"),
        ("CARGO_RESOLVER_INCOMPATIBLE_PUBLISH_AGE", "deny"),
    ];

    // The refusal, default `deny` and explicit: every too-new version is
    // listed, then the searched source and the allow hint.
    for resolver in [
        &[("CARGO_REGISTRY_GLOBAL_MIN_PUBLISH_AGE", "14 days")][..],
        &deny[..],
    ] {
        let (ok, output) = owner_cargo(
            &owner,
            &registry,
            "1.100.0",
            &["update", "-p", "cool-b", "--precise", "2.0.0"],
            resolver,
        );
        assert!(!ok);
        for line in [
            "error: failed to select a version for the requirement `cool-b = \"*\"`",
            "version 2.0.0 is too new (published ",
            "minimum age 14 days)",
            "location searched: `fixture` index (which is replacing registry `crates-io`)",
            "required by package `fixture v1.0.0 (",
            "help: to use too-new packages anyways, re-resolve with `CARGO_RESOLVER_INCOMPATIBLE_PUBLISH_AGE=allow`",
        ] {
            assert!(output.contains(line), "{line:?} not in:\n{output}");
        }
    }

    // `allow` resolves the same request, noting the release's age.
    let (ok, output) = owner_cargo(
        &owner,
        &registry,
        "1.100.0",
        &["update", "-p", "cool-b", "--precise", "2.0.0"],
        &[
            ("CARGO_REGISTRY_GLOBAL_MIN_PUBLISH_AGE", "14 days"),
            ("CARGO_RESOLVER_INCOMPATIBLE_PUBLISH_AGE", "allow"),
        ],
    );
    assert!(ok, "{output}");
    assert!(
        output.contains("Updating cool-b v1.0.0 -> v2.0.0 (published "),
        "{output}"
    );
    assert!(output.contains("minimum age 14 days)"), "{output}");

    // An invalid resolver value is a config error, not a resolve one.
    let (ok, output) = owner_cargo(
        &owner,
        &registry,
        "1.100.0",
        &["update"],
        &[
            ("CARGO_REGISTRY_GLOBAL_MIN_PUBLISH_AGE", "14 days"),
            ("CARGO_RESOLVER_INCOMPATIBLE_PUBLISH_AGE", "bogus"),
        ],
    );
    assert!(!ok);
    for line in [
        "error: error in environment variable `CARGO_RESOLVER_INCOMPATIBLE_PUBLISH_AGE`: could not load config key `resolver.incompatible-publish-age`",
        "unknown variant `bogus`, expected `allow` or `deny`",
    ] {
        assert!(output.contains(line), "{line:?} not in:\n{output}");
    }

    // The duration grammar: `N` digits, an optional single space, one of
    // the six lowercase units, singular or plural. Anything else is an
    // error, never ignored. Weeks report in days; a month is over 30 days.
    let gates = |cutoff: &str| format!("as of {cutoff}");
    let invalid = |value: &str| {
        format!(
            "error: invalid value for `registry.global-min-publish-age`: expected a value of the form \"N seconds/minutes/hours/days/weeks/months\", got: \"{value}\""
        )
    };
    for (value, expected) in [
        ("14 days", Ok(gates("14 days ago"))),
        ("14 day", Ok(gates("14 days ago"))),
        ("14days", Ok(gates("14 days ago"))),
        ("1 week", Ok(gates("7 days ago"))),
        ("1 weeks", Ok(gates("7 days ago"))),
        ("5 hours", Ok(gates("5 hours ago"))),
        ("5hours", Ok(gates("5 hours ago"))),
        ("15 minutes", Ok(gates("15 minutes ago"))),
        ("3 seconds", Ok(gates("3 seconds ago"))),
        ("0", Ok(gates("min-publish-age"))),
        ("0 days", Ok(gates("moments ago"))),
        ("0 months", Ok(gates("moments ago"))),
        ("14  days", Err(invalid("14  days"))),
        ("14 Days", Err(invalid("14 Days"))),
        ("14 DAYS", Err(invalid("14 DAYS"))),
        ("1 year", Err(invalid("1 year"))),
        ("2 years", Err(invalid("2 years"))),
        ("fortnight", Err(invalid("fortnight"))),
        ("abc", Err(invalid("abc"))),
        ("-5 days", Err(invalid("-5 days"))),
        ("+5 days", Err(invalid("+5 days"))),
        ("1.5 days", Err(invalid("1.5 days"))),
        ("1.0 days", Err(invalid("1.0 days"))),
        ("days", Err(invalid("days"))),
        (" 14 days", Err(invalid(" 14 days"))),
        ("  5 days", Err(invalid("  5 days"))),
        ("5 days ", Err(invalid("5 days "))),
        ("1 hour ", Err(invalid("1 hour "))),
    ] {
        let (ok, output) = owner_cargo(&owner, &registry, "1.100.0", &["update"], &env(value));
        match &expected {
            Ok(text) => assert!(ok && output.contains(text.as_str()), "{value:?}:\n{output}"),
            Err(text) => {
                assert!(
                    !ok && output.contains(text.as_str()),
                    "{value:?}:\n{output}"
                )
            }
        }
    }

    // A month is more than 30 days: the 30-day-old release is too new, and
    // every too-new version is listed. `1 months` resolves the same way,
    // verbatim.
    for value in ["1 month", "1 months", "2 months", "90 days"] {
        let (ok, output) = owner_cargo(&owner, &registry, "1.100.0", &["update"], &env(value));
        assert!(!ok, "{value}");
        for line in [
            "version 1.0.0 is too new (published ",
            "version 2.0.0 is too new (published ",
            "version 2.1.0 is too new (published ",
            &format!("minimum age {value})"),
        ] {
            assert!(output.contains(line), "{value}: {line:?} not in:\n{output}");
        }
    }

    // A release the index never dates is never held back.
    let files = project(("cool-undated", "1"), "1.0.0", &[]);
    write_tree(&owner, &files);
    std::fs::remove_file(owner.join("Cargo.lock")).unwrap();
    let (ok, output) = owner_cargo(&owner, &registry, "1.100.0", &["update"], &env("14 days"));
    assert!(ok, "{output}");
    let text = std::fs::read_to_string(owner.join("Cargo.lock")).unwrap();
    assert!(text.contains("version = \"1.2.0\""), "{text}");

    // The environment beats the file: `0` over the committed 14 days locks
    // the newest; the file alone holds.
    let files = project(
        ("cool-b", "*"),
        "1.0.0",
        &[(".cargo/config.toml", COOLDOWN)],
    );
    write_tree(&owner, &files);
    std::fs::remove_file(owner.join("Cargo.lock")).unwrap();
    let (ok, output) = owner_cargo(&owner, &registry, "1.100.0", &["update"], &env("0"));
    assert!(ok, "{output}");
    let text = std::fs::read_to_string(owner.join("Cargo.lock")).unwrap();
    assert!(text.contains("version = \"2.1.0\""), "{text}");
    std::fs::remove_file(owner.join("Cargo.lock")).unwrap();
    let (ok, output) = owner_cargo(&owner, &registry, "1.100.0", &["update"], &[]);
    assert!(ok, "{output}");
    assert!(output.contains("as of 14 days ago"), "{output}");
    let text = std::fs::read_to_string(owner.join("Cargo.lock")).unwrap();
    assert!(text.contains("version = \"1.0.0\""), "{text}");

    // The RFC's crates.io-only spelling is no config key at all; the
    // per-registry table works, with a degenerate cutoff line.
    let files = project(
        ("cool-b", "*"),
        "1.0.0",
        &[(
            ".cargo/config.toml",
            "[registry]\nmin-publish-age = \"14 days\"\n",
        )],
    );
    write_tree(&owner, &files);
    std::fs::remove_file(owner.join("Cargo.lock")).unwrap();
    let (ok, output) = owner_cargo(&owner, &registry, "1.100.0", &["update"], &[]);
    assert!(ok, "{output}");
    assert!(
        output.contains("warning: unused config key `registry.min-publish-age`"),
        "{output}"
    );
    let text = std::fs::read_to_string(owner.join("Cargo.lock")).unwrap();
    assert!(text.contains("version = \"2.1.0\""), "{text}");
    let files = project(
        ("cool-b", "*"),
        "1.0.0",
        &[(
            ".cargo/config.toml",
            "[registries.crates-io]\nmin-publish-age = \"14 days\"\n",
        )],
    );
    write_tree(&owner, &files);
    std::fs::remove_file(owner.join("Cargo.lock")).unwrap();
    let (ok, output) = owner_cargo(&owner, &registry, "1.100.0", &["update"], &[]);
    assert!(ok, "{output}");
    assert!(output.contains("as of min-publish-age"), "{output}");
    let text = std::fs::read_to_string(owner.join("Cargo.lock")).unwrap();
    assert!(text.contains("version = \"1.0.0\""), "{text}");

    // A too-new version already in the lock does not stay: `cargo update`
    // re-resolves and downgrades it.
    let mut files = project(("cool-b", "*"), "2.1.0", &[]);
    files.insert(".cargo/config.toml".to_string(), COOLDOWN.to_string());
    write_tree(&owner, &files);
    let (ok, output) = owner_cargo(&owner, &registry, "1.100.0", &["update"], &[]);
    assert!(ok, "{output}");
    assert!(
        output.contains("Downgrading cool-b v2.1.0 -> v1.0.0 (available: v1.5.0, published "),
        "{output}"
    );
    let text = std::fs::read_to_string(owner.join("Cargo.lock")).unwrap();
    assert!(text.contains("version = \"1.0.0\""), "{text}");

    // Nothing but this registry was reachable.
    let requested = registry.requested();
    assert!(
        requested.iter().any(|p| p == "/config.json"),
        "{requested:?}"
    );
    assert!(requested.iter().all(|p| p.starts_with('/')));
}
