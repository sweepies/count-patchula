//! A local sparse Cargo registry, on the model of `real_python_indexes.rs`:
//! a TCP server that answers `config.json` and index files whose `pubtime`
//! is set relative to now, and `.crate` files when Cargo asks for them. The
//! run's `CARGO_HOME` gets a `replace-with` source pointing crates.io at it,
//! and a dead proxy for everything else keeps it the only reachable index.
//!
//! Cargo 1.100 reads `pubtime` for the publish ages behind
//! `registry.global-min-publish-age`. These tests pin what the Count's
//! runner and the owner's own Cargo do with them: the committed cooldown
//! and each job's `waitHours` reach the run's Cargo by environment, so
//! direct updates, transitive crates and maintenance all wait the same
//! way — a `.cargo/config.toml` above the run's driver no longer leaks in,
//! since every invocation names its own age.
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
    /// The lookup catalog the index file for `name` implies at `now`:
    /// `pubtime` rendered as the server renders it, so the Count's lookup
    /// and the owner's Cargo judge the same releases on the same clock.
    pub(super) fn catalog(
        &self,
        name: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> patchula_datasource::Catalog {
        patchula_datasource::Catalog {
            versions: self
                .crates
                .lock()
                .unwrap()
                .get(name)
                .map(|releases| {
                    releases
                        .iter()
                        .map(|release| patchula_datasource::Version {
                            version: release.vers.clone(),
                            published_at: release.age.map(|age| {
                                (now - chrono::Duration::from_std(age).unwrap())
                                    .format("%Y-%m-%dT%H:%M:%SZ")
                                    .to_string()
                            }),
                            digest: None,
                            deprecated: false,
                            yanked: false,
                        })
                        .collect()
                })
                .unwrap_or_default(),
            dist_tags: Default::default(),
        }
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
    // A Cargo that cannot finish in two minutes is wedged, not slow: kill
    // it rather than hang the suite.
    let mut child = command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    let output = loop {
        if child.try_wait().unwrap().is_some() {
            break child.wait_with_output().unwrap();
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("owner cargo {args:?} ran past 120 seconds");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
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

/// The cooldowns extraction reads for `files`, what `Snapshot.cargo_cooldowns`
/// carries once the manager has scanned the tree — including the reason a
/// cooldown file it cannot read gives.
fn cooldowns(
    files: &BTreeMap<String, String>,
) -> BTreeMap<String, patchula_manager::CargoCooldown> {
    patchula_manager::Scanner::new(&config())
        .unwrap()
        .extract(&patchula_manager::all_read(files))
        .unwrap()
        .cargo_cooldowns
}
/// A snapshot over `files` carrying the cooldowns the Count's own
/// extraction would hand the run.
fn sparse_snapshot(files: &BTreeMap<String, String>) -> Snapshot {
    let mut snapshot = snapshot(files);
    snapshot.cargo_cooldowns = cooldowns(files);
    snapshot
}
/// A Cargo dependency as extraction reports one, for `maintenance`'s
/// `waitHours` derivation.
fn cargo_dependency(file: &str) -> patchula_manager::Dependency {
    patchula_manager::Dependency {
        manager: Manager::Cargo,
        file: file.into(),
        location: patchula_manager::Location {
            start: 0,
            end: 1,
            line: 1,
            column: 1,
            pointer: String::new(),
        },
        form: patchula_manager::EditForm::Scalar(patchula_manager::Syntax::Toml),
        name: "cool-b".into(),
        package: "cool-b".into(),
        kind: patchula_words::DependencyKind::Production,
        current: "1.0.0".into(),
        resolved: Some("1.0.0".into()),
        digest: None,
        source: Source::Crates,
        scheme: patchula_words::Scheme::Semver,
        extract: None,
        included_by: None,
        registry: None,
        cooldown: None,
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
    let mut snapshot = sparse_snapshot(files);
    let config = config();
    let mut p = maintenance(&repo(), &config, &snapshot, &[], None, monday())
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
    let mut p = maintenance(&repo(), &config, &shot, &[], None, monday())
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
    let mut p = maintenance(&repo(), &config, &shot, &[], None, monday())
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

/// What the Count concludes about `name` on `files` and the releases
/// `registry` serves at `now`: extraction marks the committed cooldown on
/// the dependency, and the evaluator — with no release-age wait of its own,
/// so the cooldown is the only gate — proposes and holds over the same
/// `pubtime`s the owner's Cargo reads.
fn count_verdict(
    files: &BTreeMap<String, String>,
    name: &str,
    registry: &SparseRegistry,
    now: chrono::DateTime<chrono::Utc>,
) -> (patchula_manager::Dependency, patchula_lookup::Evaluation) {
    let mut count = config();
    count.defaults.wait_hours = 0;
    let read = patchula_manager::all_read(files);
    let extraction = patchula_manager::Scanner::new(&count)
        .unwrap()
        .extract(&read)
        .unwrap();
    let dep = extraction
        .dependencies
        .iter()
        .find(|dep| dep.name == name)
        .unwrap_or_else(|| {
            panic!(
                "{name} was not extracted: {:?}",
                extraction
                    .dependencies
                    .iter()
                    .map(|dep| &dep.name)
                    .collect::<Vec<_>>()
            )
        })
        .clone();
    let verdict = patchula_lookup::Evaluator::new(&count, Default::default())
        .unwrap()
        .evaluate_at(&dep, &registry.catalog(name, now), now)
        .unwrap();
    (dep, verdict)
}

/// Invariant A, checked against Cargo itself rather than a model of it:
/// every version the Count proposes is one the owner's Cargo accepts with
/// `--precise`.
fn cargo_accepts(registry: &SparseRegistry, dir: &Path, name: &str, version: &str) -> bool {
    owner_cargo(
        dir,
        registry,
        "1.100.0",
        &["update", "-p", name, "--precise", version],
        &[],
    )
    .0
}

/// A repository that commits a `.cargo/config.toml` with
/// `[registry] global-min-publish-age`: extraction reads it, and lookup
/// holds the two-day-old release inside it — no update is proposed, so no
/// branch is planned — while the owner's own Cargo on the same files
/// refuses it. A branch built by hand that still asks for the release is
/// refused by the run's Cargo too, verbatim: the run carries the
/// repository's committed value to every invocation.
#[tokio::test]
async fn real_cargo_holds_at_lookup_inside_a_committed_cooldown_the_owner_enforces() {
    let registry = registry();
    let files = project(
        ("cool-b", "1.0"),
        "1.0.0",
        &[(".cargo/config.toml", COOLDOWN)],
    );
    let now = chrono::Utc::now();
    let (dep, verdict) = count_verdict(&files, "cool-b", &registry, now);
    assert_eq!(
        dep.cooldown,
        Some(patchula_manager::Cooldown {
            hours: 336,
            value: "14 days".into(),
            file: ".cargo/config.toml".into(),
        })
    );
    // Every newer release is inside the 14-day window: nothing is
    // proposed, so no branch exists to plan for it.
    assert!(verdict.updates.is_empty(), "{:?}", verdict.updates);
    assert!(!verdict.held_back.is_empty());
    for held in &verdict.held_back {
        assert_eq!(
            held.cooldown,
            Some(patchula_report::Cooldown {
                hours: 336,
                file: ".cargo/config.toml".into(),
            }),
            "{held:?}"
        );
    }

    // A plan forced by hand still asks for the held release: the run's
    // Cargo refuses it under `deny`, and the branch fails with Cargo's own
    // age line in the reason, as the owner's Cargo writes it below.
    let (p, _) = targeted(
        &registry,
        &files,
        ("cool-b", "1.0"),
        "1.0.0",
        "1.5.0",
        manifest(&[("cool-b", "1.5.0")]),
    )
    .await;
    assert_eq!(p.action().status, BranchStatus::Error, "{:?}", p.action());
    let reason = p.action().reason.as_deref().unwrap_or_default();
    for line in [
        "lockfile generation failed",
        "1.5.0 is too new",
        "minimum age 14 days",
    ] {
        assert!(reason.contains(line), "{line:?} not in:\n{reason}");
    }
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

/// A `.cargo/config.toml` in a directory above the driver or cache
/// directory is a parent of Cargo's working directory, so its config
/// would apply — but every invocation names its own publish age, and the
/// explicit `0` the repository's missing cooldown resolves to beats the
/// file. The too-new `--precise` locks in both directions.
#[tokio::test]
async fn real_cargo_ignores_cooldowns_above_the_driver_directory() {
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
        let mut p = maintenance(&repo(), &config, &snapshot, &[], None, monday())
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
        assert_eq!(
            p.action().status,
            BranchStatus::Planned,
            "config_above={config_above}: {:?}",
            p.action()
        );
        assert!(
            p.contents()["Cargo.lock"].contains("version = \"1.5.0\""),
            "config_above={config_above}: {}",
            p.contents()["Cargo.lock"]
        );
    }
}

/// Rust 1.99.1 with the publish-age variables set runs clean and ignores
/// them, as the repository's own pinned toolchain may be older than the
/// runner's default: a committed cooldown the run hands it by environment
/// is dropped with a warning, the too-new release is locked, and the run
/// reports that transitive crates there do not wait.
#[tokio::test]
async fn real_cargo_before_1_100_runs_clean_with_cooldown_keys_set() {
    let registry = registry();
    ensure_tool("rust@1.99.1");
    let files = project(
        ("cool-b", "1.0"),
        "1.0.0",
        &[
            (".tool-versions", "rust 1.99.1\n"),
            (".cargo/config.toml", COOLDOWN),
        ],
    );
    let executor = fixture_sparse(files.clone(), registry.clone());
    let temp = tempfile::tempdir().unwrap();
    let runner =
        Runner::new(temp.path().join("worktrees"), "offline-installation").with_executor(executor);
    let mut snapshot = sparse_snapshot(&files);
    let config = config();
    let mut p = maintenance(&repo(), &config, &snapshot, &[], None, monday())
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
    let warnings = patchula_update::lock_all(
        &runner,
        std::slice::from_mut(&mut p),
        &mut snapshot,
        &config,
        &|| {},
    )
    .await;
    assert_eq!(p.action().status, BranchStatus::Planned, "{:?}", p.action());
    assert!(
        p.contents()["Cargo.lock"].contains("version = \"1.5.0\""),
        "{}",
        p.contents()["Cargo.lock"]
    );
    // The run names the cooldown it could not enforce: the toolchain that
    // ignored it, and the transitive crates that do not wait.
    assert_eq!(
        warnings,
        [
            "Cargo.toml: rust 1.99.1 predates Cargo 1.100's publish-age support; the cooldown binds direct updates but transitive crates do not wait"
        ],
        "{warnings:?}"
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

/// The committed cooldown reaches maintenance: `cargo update` under it
/// keeps the day-old transitive release where the same run without one,
/// above, locks the newest. The lock does not change, so the run may not
/// write it at all.
#[tokio::test]
async fn real_cargo_maintenance_holds_inside_a_committed_cooldown() {
    let registry = registry();
    let files = BTreeMap::from([
        ("Cargo.toml".to_string(), manifest(&[("cool-wrapper", "1")])),
        ("src/lib.rs".to_string(), "// fixture\n".to_string()),
        (
            "Cargo.lock".to_string(),
            lock(&[("cool-wrapper", "1.0.0", &[])]),
        ),
        (".cargo/config.toml".to_string(), COOLDOWN.to_string()),
    ]);
    let executor = fixture_sparse(files.clone(), registry.clone());
    let temp = tempfile::tempdir().unwrap();
    let runner =
        Runner::new(temp.path().join("worktrees"), "offline-installation").with_executor(executor);
    let mut snapshot = sparse_snapshot(&files);
    let config = config();
    let mut p = maintenance(&repo(), &config, &snapshot, &[], None, monday())
        .unwrap()
        .unwrap();
    prepare_one(&runner, &mut p, &mut snapshot, &config).await;
    assert_eq!(p.action().status, BranchStatus::Planned, "{:?}", p.action());
    let lock = p.contents().get("Cargo.lock").map(String::as_str);
    assert!(
        lock.is_none_or(|text| text.contains("version = \"1.0.0\"")),
        "{lock:?}"
    );
    assert!(lock.is_none_or(|text| !text.contains("1.5.0")), "{lock:?}");
}

/// With no committed cooldown the job's own `waitHours` still reaches the
/// run's Cargo: the 336-hour wait maintenance derives from the update
/// policy holds the same day-old transitive release the committed value
/// does. The job records the wait it answered to.
#[tokio::test]
async fn real_cargo_maintenance_holds_inside_the_jobs_wait() {
    let registry = registry();
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
    let mut snapshot = sparse_snapshot(&files);
    let mut config = config();
    config.defaults.wait_hours = 336;
    let deps = [cargo_dependency("Cargo.toml")];
    let mut p = maintenance(&repo(), &config, &snapshot, &deps, None, monday())
        .unwrap()
        .unwrap();
    assert_eq!(p.lock_jobs()[0].wait_hours, 336);
    prepare_one(&runner, &mut p, &mut snapshot, &config).await;
    assert_eq!(p.action().status, BranchStatus::Planned, "{:?}", p.action());
    let lock = p.contents().get("Cargo.lock").map(String::as_str);
    assert!(
        lock.is_none_or(|text| text.contains("version = \"1.0.0\"")),
        "{lock:?}"
    );
    assert!(lock.is_none_or(|text| !text.contains("1.5.0")), "{lock:?}");
}

/// `allow` belongs to the one job whose security fix sits inside the
/// window: it locks cool-b 1.5.0 for the marked job, and the same
/// directory's ordinary job still runs `deny`, so its too-new target is
/// refused and the branch carries Cargo's age line.
#[tokio::test]
async fn real_cargo_allows_only_the_job_the_fix_marks() {
    let registry = registry();
    let files = BTreeMap::from([
        (
            "Cargo.toml".to_string(),
            manifest(&[("cool-b", "1.0"), ("cool-fresh", "1")]),
        ),
        ("src/lib.rs".to_string(), "// fixture\n".to_string()),
        (
            "Cargo.lock".to_string(),
            lock(&[("cool-b", "1.0.0", &[]), ("cool-fresh", "1.0.0", &[])]),
        ),
        (".cargo/config.toml".to_string(), COOLDOWN.to_string()),
    ]);
    let executor = fixture_sparse(files.clone(), registry.clone());
    let temp = tempfile::tempdir().unwrap();
    let runner =
        Runner::new(temp.path().join("worktrees"), "offline-installation").with_executor(executor);
    let mut snapshot = sparse_snapshot(&files);
    let config = config();
    let mut p = maintenance(&repo(), &config, &snapshot, &[], None, monday())
        .unwrap()
        .unwrap();
    let mut draft = Draft::from(p);
    // The marked security fix first, then an ordinary job on the same
    // directory the window still holds.
    {
        let jobs = draft.lock_jobs_mut();
        jobs[0].scope = Scope::Package {
            package: "cool-b".into(),
            current: Some("1.0.0".into()),
            target: Some("1.5.0".into()),
        };
        jobs[0].publish_age_allow = true;
        let mut denied = jobs[0].clone();
        denied.publish_age_allow = false;
        denied.scope = Scope::Package {
            package: "cool-fresh".into(),
            current: Some("1.0.0".into()),
            target: Some("1.1.0".into()),
        };
        jobs.push(denied);
    }
    let old = &files["Cargo.toml"];
    p = draft
        .update()
        .edit(
            "Cargo.toml",
            manifest(&[("cool-b", "1.5.0"), ("cool-fresh", "1")]),
            FileAction {
                path: "Cargo.toml".into(),
                blob_sha: blob_sha(old),
                diff: String::new(),
            },
        )
        .build();
    prepare_one(&runner, &mut p, &mut snapshot, &config).await;
    assert_eq!(p.action().status, BranchStatus::Error, "{:?}", p.action());
    let reason = p.action().reason.as_deref().unwrap_or_default();
    assert!(!reason.contains("1.5.0 is too new"), "{reason}");
    for line in ["1.1.0 is too new", "minimum age 14 days"] {
        assert!(reason.contains(line), "{line:?} not in:\n{reason}");
    }
}

/// A committed cooldown the Count cannot read fails the directory's jobs
/// rather than running with none: an ordinary update reports the file's
/// reason, and maintenance reports it too.
#[tokio::test]
async fn real_cargo_unreadable_cooldown_fails_the_directory() {
    let registry = registry();
    let files = project(
        ("cool-b", "1.0"),
        "1.0.0",
        &[(
            ".cargo/config.toml",
            "[registry]\nglobal-min-publish-age = \"fortnight\"\n",
        )],
    );
    let unread =
        ".cargo/config.toml's `registry.global-min-publish-age`, which the Count cannot read";
    let (p, _) = targeted(
        &registry,
        &files,
        ("cool-b", "1.0"),
        "1.0.0",
        "1.5.0",
        manifest(&[("cool-b", "1.5.0")]),
    )
    .await;
    assert_eq!(p.action().status, BranchStatus::Error, "{:?}", p.action());
    let reason = p.action().reason.as_deref().unwrap_or_default();
    assert!(reason.contains(unread), "{unread:?} not in:\n{reason}");

    let executor = fixture_sparse(files.clone(), registry.clone());
    let temp = tempfile::tempdir().unwrap();
    let runner =
        Runner::new(temp.path().join("worktrees"), "offline-installation").with_executor(executor);
    let mut snapshot = sparse_snapshot(&files);
    let config = config();
    let mut p = maintenance(&repo(), &config, &snapshot, &[], None, monday())
        .unwrap()
        .unwrap();
    prepare_one(&runner, &mut p, &mut snapshot, &config).await;
    assert_eq!(p.action().status, BranchStatus::Error, "{:?}", p.action());
    let reason = p.action().reason.as_deref().unwrap_or_default();
    assert!(reason.contains(unread), "{unread:?} not in:\n{reason}");
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

/// The floor against Cargo itself: generated cases over the committed
/// value's grammar, the file's placement and releases straddling the
/// window. For each, the Count's extraction and lookup run on the same
/// files the owner's `cargo update --precise` runs on, and every version
/// the Count proposes is one Cargo accepts — the Count may only be
/// stricter, and each place it is, the case says so.
#[test]
fn the_count_never_proposes_what_the_owners_cargo_holds_back() {
    // Cargo's `maybe_parse_time_span` month, pinned by the case below.
    const MONTH: u64 = 2_629_746;
    const HOUR: u64 = 3_600;
    const DAY: u64 = 86_400;
    let now = chrono::Utc::now();
    let temp = tempfile::tempdir().unwrap();
    let owner = temp.path().join("owner");
    let cooldown_toml = |value: &str| format!("[registry]\nglobal-min-publish-age = \"{value}\"\n");
    let floor = |value: &str, hours: u32| patchula_manager::Cooldown {
        hours,
        value: value.into(),
        file: ".cargo/config.toml".into(),
    };
    // One grammar cell: `held` is inside the cooldown, `allowed` outside
    // it, and `hours` is what the Count's rounded-up floor must be.
    let cell = |name: &str, value: &str, hours: u32, held: u64, allowed: u64| {
        let registry = SparseRegistry::start();
        registry
            .publish(name, Release::new("1.0.0").days(90))
            .publish(name, Release::new("1.5.0").age(held))
            .publish(name, Release::new("2.0.0").age(allowed));
        let files = project(
            (name, "*"),
            "1.0.0",
            &[(".cargo/config.toml", cooldown_toml(value).as_str())],
        );
        write_tree(&owner, &files);
        let (dep, verdict) = count_verdict(&files, name, &registry, now);
        assert_eq!(dep.cooldown, Some(floor(value, hours)), "{value}");
        // The Count holds what Cargo refuses and proposes what Cargo
        // accepts — each checked with `--precise`, not a model of it.
        let proposed: Vec<_> = verdict
            .updates
            .iter()
            .filter_map(|u| u.new_version.as_deref())
            .collect();
        for version in &proposed {
            assert!(
                cargo_accepts(&registry, &owner, name, version),
                "{value}: the Count proposed {version}, which the owner's Cargo refuses"
            );
        }
        for held in &verdict.held_back {
            for version in &held.versions {
                let (ok, output) = owner_cargo(
                    &owner,
                    &registry,
                    "1.100.0",
                    &["update", "-p", name, "--precise", version],
                    &[],
                );
                assert!(
                    !ok && output.contains("too new"),
                    "{value}: the Count held {version} at lookup but the owner's Cargo takes it:\n{output}"
                );
            }
        }
        assert_eq!(proposed, ["2.0.0"], "{value}: {:?}", verdict.updates);
        assert_eq!(
            verdict
                .held_back
                .iter()
                .flat_map(|h| h.versions.iter())
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["1.5.0"],
            "{value}"
        );
    };

    // Whole units both sides agree on: days, weeks, hours, months.
    cell("parity-days", "14 days", 336, 13 * DAY, 15 * DAY);
    cell("parity-weeks", "1 week", 168, 6 * DAY, 8 * DAY);
    cell("parity-hours", "48 hours", 48, 47 * HOUR, 49 * HOUR);
    cell("parity-months", "2 months", 1461, 60 * DAY, 62 * DAY);
    // `0` holds nothing, on both sides.
    {
        let registry = SparseRegistry::start();
        registry
            .publish("parity-zero", Release::new("1.0.0").days(90))
            .publish("parity-zero", Release::new("1.5.0").age(HOUR));
        let files = project(
            ("parity-zero", "*"),
            "1.0.0",
            &[(".cargo/config.toml", cooldown_toml("0").as_str())],
        );
        write_tree(&owner, &files);
        let (dep, verdict) = count_verdict(&files, "parity-zero", &registry, now);
        assert_eq!(dep.cooldown, Some(floor("0", 0)));
        assert_eq!(
            verdict
                .updates
                .iter()
                .filter_map(|u| u.new_version.as_deref())
                .collect::<Vec<_>>(),
            ["1.5.0"]
        );
        assert!(cargo_accepts(&registry, &owner, "parity-zero", "1.5.0"));
    }
    // Sub-hour values round the Count's floor up to one hour: a release
    // Cargo already allows stays held a while longer — the one direction
    // the Count may differ.
    {
        let registry = SparseRegistry::start();
        registry
            .publish("parity-minutes", Release::new("1.0.0").days(90))
            .publish("parity-minutes", Release::new("1.5.0").age(30 * 60))
            .publish("parity-minutes", Release::new("2.0.0").age(2 * HOUR));
        let files = project(
            ("parity-minutes", "*"),
            "1.0.0",
            &[(".cargo/config.toml", cooldown_toml("15 minutes").as_str())],
        );
        write_tree(&owner, &files);
        let (dep, verdict) = count_verdict(&files, "parity-minutes", &registry, now);
        assert_eq!(dep.cooldown, Some(floor("15 minutes", 1)));
        assert_eq!(
            verdict
                .held_back
                .iter()
                .flat_map(|h| h.versions.iter())
                .collect::<Vec<_>>(),
            [&"1.5.0".to_string()]
        );
        assert!(
            cargo_accepts(&registry, &owner, "parity-minutes", "1.5.0"),
            "Cargo allows a 30-minute-old release under a 15-minute cooldown"
        );
        assert!(cargo_accepts(&registry, &owner, "parity-minutes", "2.0.0"));
    }
    // A month is Cargo's 2,629,746 seconds — 30.436875 days, not 30 and not
    // 31: probes either side of it, one inside the Count's round-up sliver.
    {
        let registry = SparseRegistry::start();
        registry
            .publish("parity-month", Release::new("1.0.0").days(90))
            .publish("parity-month", Release::new("1.1.0").days(28))
            .publish("parity-month", Release::new("1.2.0").days(30))
            .publish("parity-month", Release::new("1.3.0").age(MONTH - 600))
            .publish("parity-month", Release::new("2.0.0").age(MONTH + 600))
            .publish(
                "parity-month",
                Release::new("3.0.0").age(MONTH + HOUR + 600),
            );
        let files = project(
            ("parity-month", "*"),
            "1.0.0",
            &[(".cargo/config.toml", cooldown_toml("1 month").as_str())],
        );
        write_tree(&owner, &files);
        let (dep, verdict) = count_verdict(&files, "parity-month", &registry, now);
        // floor(2_629_746 s) rounds up to 731 h.
        assert_eq!(dep.cooldown, Some(floor("1 month", 731)));
        // The minors are reported held; 2.0.0 is held too but the report
        // drops it, superseded by the eligible 3.0.0 of its class.
        assert_eq!(
            verdict
                .held_back
                .iter()
                .flat_map(|h| h.versions.iter())
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["1.1.0", "1.2.0", "1.3.0"]
        );
        // Cargo refuses the inside releases, verbatim as the Count does.
        for version in ["1.1.0", "1.2.0", "1.3.0"] {
            let (ok, output) = owner_cargo(
                &owner,
                &registry,
                "1.100.0",
                &["update", "-p", "parity-month", "--precise", version],
                &[],
            );
            assert!(!ok && output.contains("too new"), "{version}:\n{output}");
        }
        // 2.0.0 sits in the Count's rounded-up sliver: Cargo takes it, the
        // Count waits another hour at most. 3.0.0 is outside both.
        assert!(cargo_accepts(&registry, &owner, "parity-month", "2.0.0"));
        assert_eq!(
            verdict
                .updates
                .iter()
                .filter_map(|u| u.new_version.as_deref())
                .collect::<Vec<_>>(),
            ["3.0.0"]
        );
        assert!(cargo_accepts(&registry, &owner, "parity-month", "3.0.0"));
    }
    // `.cargo/config` at the root reads the same way by each.
    {
        let registry = SparseRegistry::start();
        registry
            .publish("parity-placed", Release::new("1.0.0").days(90))
            .publish("parity-placed", Release::new("1.5.0").days(5));
        let files = project(
            ("parity-placed", "*"),
            "1.0.0",
            &[(".cargo/config", COOLDOWN)],
        );
        write_tree(&owner, &files);
        let (dep, verdict) = count_verdict(&files, "parity-placed", &registry, now);
        assert_eq!(
            dep.cooldown,
            Some(patchula_manager::Cooldown {
                hours: 336,
                value: "14 days".into(),
                file: ".cargo/config".into(),
            })
        );
        assert!(verdict.updates.is_empty(), "{:?}", verdict.updates);
        let (ok, output) = owner_cargo(
            &owner,
            &registry,
            "1.100.0",
            &["update", "-p", "parity-placed", "--precise", "1.5.0"],
            &[],
        );
        assert!(!ok && output.contains("too new"), "{output}");
    }
    // `member/.cargo/config.toml` applies to the member's dependencies —
    // and to Cargo run inside the member.
    {
        let registry = SparseRegistry::start();
        registry
            .publish("parity-placed", Release::new("1.0.0").days(90))
            .publish("parity-placed", Release::new("1.5.0").days(5));
        let files = BTreeMap::from([
            (
                "Cargo.toml".to_string(),
                "[workspace]\nmembers = [\"member\"]\nresolver = \"2\"\n".to_string(),
            ),
            (
                "member/Cargo.toml".to_string(),
                manifest(&[("parity-placed", "*")]),
            ),
            ("member/src/lib.rs".to_string(), "// fixture\n".to_string()),
            (
                "Cargo.lock".to_string(),
                lock(&[("parity-placed", "1.0.0", &[])]),
            ),
            (
                "member/.cargo/config.toml".to_string(),
                COOLDOWN.to_string(),
            ),
        ]);
        write_tree(&owner, &files);
        let (dep, verdict) = count_verdict(&files, "parity-placed", &registry, now);
        assert_eq!(
            dep.cooldown,
            Some(patchula_manager::Cooldown {
                hours: 336,
                value: "14 days".into(),
                file: "member/.cargo/config.toml".into(),
            })
        );
        assert!(verdict.updates.is_empty(), "{:?}", verdict.updates);
        let (ok, output) = owner_cargo(
            &owner.join("member"),
            &registry,
            "1.100.0",
            &["update", "-p", "parity-placed", "--precise", "1.5.0"],
            &[],
        );
        assert!(!ok && output.contains("too new"), "{output}");
    }
    // `include` and an unparseable file are configs the Count cannot read:
    // its lookups fail unusable rather than run unprotected, while Cargo
    // follows the `include` and errors on bad TOML.
    for (extra, cargo_line) in [
        ("include = [\"more.toml\"]\n", "too new"),
        ("this is not [toml\n", "error"),
    ] {
        let registry = SparseRegistry::start();
        registry
            .publish("parity-unread", Release::new("1.0.0").days(90))
            .publish("parity-unread", Release::new("1.5.0").days(5));
        let mut files = project(
            ("parity-unread", "*"),
            "1.0.0",
            &[(".cargo/config.toml", extra)],
        );
        if extra.starts_with("include") {
            files.insert(".cargo/more.toml".into(), COOLDOWN.into());
        }
        write_tree(&owner, &files);
        let read = patchula_manager::all_read(&files);
        let mut count = config();
        count.defaults.wait_hours = 0;
        let extraction = patchula_manager::Scanner::new(&count)
            .unwrap()
            .extract(&read)
            .unwrap();
        let dep = extraction
            .dependencies
            .iter()
            .find(|dep| dep.name == "parity-unread")
            .unwrap();
        assert!(
            matches!(dep.registry, Some(patchula_config::Registry::Unusable(_))),
            "{extra:?}: {:?}",
            dep.registry
        );
        let (ok, output) = owner_cargo(
            &owner,
            &registry,
            "1.100.0",
            &["update", "-p", "parity-unread", "--precise", "1.5.0"],
            &[],
        );
        assert!(!ok && output.contains(cargo_line), "{extra:?}:\n{output}");
    }
    // A committed `resolver.incompatible-publish-age = "allow"` loosens
    // Cargo but never the Count: it holds the release anyway.
    {
        let registry = SparseRegistry::start();
        registry
            .publish("parity-allow", Release::new("1.0.0").days(90))
            .publish("parity-allow", Release::new("1.5.0").days(5));
        let files = project(
            ("parity-allow", "*"),
            "1.0.0",
            &[(
                ".cargo/config.toml",
                "[registry]\nglobal-min-publish-age = \"14 days\"\n[resolver]\nincompatible-publish-age = \"allow\"\n",
            )],
        );
        write_tree(&owner, &files);
        let (dep, verdict) = count_verdict(&files, "parity-allow", &registry, now);
        assert_eq!(dep.cooldown, Some(floor("14 days", 336)));
        assert!(verdict.updates.is_empty(), "{:?}", verdict.updates);
        assert!(
            cargo_accepts(&registry, &owner, "parity-allow", "1.5.0"),
            "a committed allow loosens Cargo, not the Count"
        );
    }
    // A key Cargo warns is unused is still read — the strictest of every
    // key wins, so `registry.min-publish-age` holds on the Count's side
    // alone today.
    {
        let registry = SparseRegistry::start();
        registry
            .publish("parity-key", Release::new("1.0.0").days(90))
            .publish("parity-key", Release::new("1.5.0").days(5));
        let files = project(
            ("parity-key", "*"),
            "1.0.0",
            &[(
                ".cargo/config.toml",
                "[registry]\nmin-publish-age = \"14 days\"\n",
            )],
        );
        write_tree(&owner, &files);
        let (dep, verdict) = count_verdict(&files, "parity-key", &registry, now);
        assert_eq!(dep.cooldown, Some(floor("14 days", 336)));
        assert!(verdict.updates.is_empty(), "{:?}", verdict.updates);
        assert!(
            cargo_accepts(&registry, &owner, "parity-key", "1.5.0"),
            "Cargo's unused key holds nothing today; the Count still reads it"
        );
    }
}
