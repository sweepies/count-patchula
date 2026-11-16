//! Canaries for the rule that no Node lockfile tool runs code a dependency
//! chose. A loopback server is both an npm registry and a dumb-HTTP Git
//! host. Its Git repository holds `canary-evil`, whose lifecycle and pack
//! scripts each touch a marker file; its registry serves `canary-mid-*`,
//! whose own install scripts touch markers too and which depend on
//! `canary-evil` through Git in the spellings npm and Yarn read. Every case
//! runs the real pinned tool through the runner and then checks that no
//! marker exists.
//!
//! npm and Yarn Berry pack a git dependency to prepare it, running its
//! scripts, so a direct git dependency is refused before they start, and a
//! transitive one fails because their runs have no Git transport. A control
//! run with Git reachable shows Yarn would otherwise have run the scripts,
//! so the canary has teeth. aube (for `aube-lock.yaml` and `pnpm-lock.yaml`)
//! and Bun run with Git reachable, directly and transitively, and still
//! leave no marker.
use super::*;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;

/// The lifecycle and pack scripts a package can carry, each of which the
/// canaries make touch a marker named after itself.
const SCRIPTS: &[&str] = &[
    "preinstall",
    "install",
    "postinstall",
    "prepare",
    "prepack",
    "postpack",
];

/// One registry package: its name and `package.json`.
struct Package {
    name: String,
    manifest: serde_json::Value,
}

/// A loopback npm registry and dumb-HTTP Git host.
pub(super) struct NodeRegistry {
    base: String,
    packages: Mutex<Vec<Package>>,
    /// The bare repositories served under `/git/`.
    git: PathBuf,
    /// Every path asked for, in order.
    requests: Mutex<Vec<String>>,
}

impl NodeRegistry {
    fn start(git: PathBuf) -> Arc<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let registry = Arc::new(Self {
            base: format!("http://{}", listener.local_addr().unwrap()),
            packages: Mutex::new(Vec::new()),
            git,
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
    fn publish(&self, name: &str, manifest: serde_json::Value) {
        self.packages.lock().unwrap().push(Package {
            name: name.into(),
            manifest,
        });
    }
    fn tarball(manifest: &serde_json::Value) -> Vec<u8> {
        real_sparse_registry::gzip(&real_sparse_registry::tar(&[(
            "package/package.json".into(),
            serde_json::to_vec_pretty(manifest).unwrap(),
        )]))
    }
    fn answer(&self, path: &str) -> Option<(&'static str, Vec<u8>)> {
        if let Some(rest) = path.strip_prefix("/git/") {
            if rest.split('/').any(|part| part == ".." || part.is_empty()) {
                return None;
            }
            return std::fs::read(self.git.join(rest))
                .ok()
                .map(|bytes| ("application/octet-stream", bytes));
        }
        let packages = self.packages.lock().unwrap();
        let name = path.trim_start_matches('/');
        if let Some((name, _)) = name.split_once("/-/") {
            let package = packages.iter().find(|p| p.name == name)?;
            return Some(("application/octet-stream", Self::tarball(&package.manifest)));
        }
        let name = name.replace("%2f", "/").replace("%2F", "/");
        let package = packages.iter().find(|p| p.name == name)?;
        let tarball = Self::tarball(&package.manifest);
        let integrity = {
            use base64::Engine;
            use sha2::Digest;
            format!(
                "sha512-{}",
                base64::engine::general_purpose::STANDARD.encode(sha2::Sha512::digest(&tarball))
            )
        };
        let mut version = package.manifest.clone();
        version["dist"] = serde_json::json!({
            "tarball": format!("{}/{}/-/{}-1.0.0.tgz", self.base, package.name, package.name),
            "integrity": integrity,
        });
        let packument = serde_json::json!({
            "name": package.name,
            "dist-tags": {"latest": "1.0.0"},
            "versions": {"1.0.0": version},
            "time": {"1.0.0": "2020-01-01T00:00:00.000Z", "created": "2020-01-01T00:00:00.000Z", "modified": "2020-01-01T00:00:00.000Z"},
        });
        Some(("application/json", serde_json::to_vec(&packument).unwrap()))
    }
    fn respond(&self, stream: TcpStream) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() {
            return;
        }
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header).is_err() || header.trim().is_empty() {
                break;
            }
        }
        let path = line.split_whitespace().nth(1).unwrap_or("/").to_owned();
        let path = path.split('?').next().unwrap_or(&path).to_owned();
        self.requests.lock().unwrap().push(path.clone());
        let mut stream = stream;
        let response = match self.answer(&path) {
            Some((kind, body)) => {
                let mut head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .into_bytes();
                head.extend(body);
                head
            }
            None => {
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
            }
        };
        let _ = stream.write_all(&response);
    }
}

/// Scripts that touch `markers/<tool>-<script>` for every script in
/// `SCRIPTS`.
fn marking_scripts(markers: &Path, who: &str) -> serde_json::Value {
    SCRIPTS
        .iter()
        .map(|script| {
            (
                (*script).to_owned(),
                serde_json::Value::String(format!(
                    "touch {}",
                    markers.join(format!("{who}-{script}")).display()
                )),
            )
        })
        .collect::<serde_json::Map<_, _>>()
        .into()
}

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "canary")
        .env("GIT_AUTHOR_EMAIL", "canary@example.com")
        .env("GIT_COMMITTER_NAME", "canary")
        .env("GIT_COMMITTER_EMAIL", "canary@example.com")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The canary world: the server, its packages, and the marker directory.
struct World {
    registry: Arc<NodeRegistry>,
    markers: PathBuf,
    _temp: tempfile::TempDir,
}

impl World {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let markers = temp.path().join("markers");
        std::fs::create_dir_all(&markers).unwrap();
        // canary-evil, in a Git repository served over dumb HTTP.
        let work = temp.path().join("evil");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(
            work.join("package.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "name": "canary-evil",
                "version": "1.0.0",
                "scripts": marking_scripts(&markers, "evil"),
            }))
            .unwrap(),
        )
        .unwrap();
        git(&work, &["init", "-q", "-b", "main"]);
        git(&work, &["add", "package.json"]);
        git(&work, &["commit", "-q", "-m", "canary"]);
        let served = temp.path().join("git");
        std::fs::create_dir_all(&served).unwrap();
        git(
            temp.path(),
            &["clone", "-q", "--bare", "evil", "git/evil.git"],
        );
        git(&served.join("evil.git"), &["update-server-info"]);
        let registry = NodeRegistry::start(served);
        let repo = format!("{}/git/evil.git", registry.base);
        for (name, spec) in [
            ("canary-mid-gitplus", format!("git+{repo}")),
            ("canary-mid-dotgit", repo.clone()),
            (
                "canary-mid-github",
                "github:patchula-canary/evil#0123456789abcdef0123456789abcdef01234567".into(),
            ),
        ] {
            registry.publish(
                name,
                serde_json::json!({
                    "name": name,
                    "version": "1.0.0",
                    "dependencies": {"canary-evil": spec},
                    "scripts": marking_scripts(&markers, name),
                }),
            );
        }
        Self {
            registry,
            markers,
            _temp: temp,
        }
    }
    /// The repository's `.npmrc`, `.yarnrc.yml` and `pnpm-workspace.yaml`,
    /// naming the registry and turning off each tool's own guard against
    /// git dependencies, as a hostile repository would: Yarn's
    /// `approvedGitRepositories` and aube's `blockExoticSubdeps`.
    fn settings(&self) -> [(String, String); 3] {
        [
            (
                ".npmrc".into(),
                format!("registry={}/\n", self.registry.base),
            ),
            (
                ".yarnrc.yml".into(),
                format!(
                    "nodeLinker: node-modules\nnpmRegistryServer: \"{}\"\nunsafeHttpWhitelist:\n  - 127.0.0.1\napprovedGitRepositories:\n  - \"**\"\n",
                    self.registry.base
                ),
            ),
            (
                "pnpm-workspace.yaml".into(),
                "blockExoticSubdeps: false\n".into(),
            ),
        ]
    }
    fn git_url(&self, plus: bool) -> String {
        format!(
            "{}{}/git/evil.git",
            if plus { "git+" } else { "" },
            self.registry.base
        )
    }
    fn markers(&self) -> Vec<String> {
        let mut found: Vec<_> = std::fs::read_dir(&self.markers)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        found.sort();
        found
    }
    fn clear(&self) {
        for entry in std::fs::read_dir(&self.markers).unwrap().flatten() {
            std::fs::remove_file(entry.path()).unwrap();
        }
    }
}

/// What one case ran and left behind.
struct Outcome {
    status: BranchStatus,
    reason: String,
    lock: Option<String>,
    markers: Vec<String>,
    /// Whether the tool for the case's lock was started at all.
    ran: bool,
}

/// Run the real tool for `leaf` over a project depending on `dependency`
/// at `spec`, with `config`, through the runner.
async fn run(
    world: &World,
    leaf: &str,
    seed: &str,
    dependency: &str,
    spec: &str,
    config: &Config,
    git_protocols: Option<&'static str>,
) -> Outcome {
    world.clear();
    let mut files: BTreeMap<String, String> = world.settings().into_iter().collect();
    let mut manifest = serde_json::json!({
        "name": "fixture",
        "version": "1.0.0",
        "dependencies": {dependency: spec},
    });
    if leaf == "yarn.lock" {
        manifest["packageManager"] = format!("yarn@{}", default_version("yarn")).into();
    }
    files.insert(
        "package.json".into(),
        serde_json::to_string(&manifest).unwrap() + "\n",
    );
    files.insert(leaf.into(), seed.into());
    let mut executor = fixture(files.clone(), false, true);
    {
        let fixture = Arc::get_mut(&mut executor).unwrap();
        fixture.node_registry = Some(world.registry.clone());
        fixture.git_protocols = git_protocols;
    }
    let temp = tempfile::tempdir().unwrap();
    let runner = test_runner(temp.path(), executor.clone());
    let mut snapshot = snapshot(&files);
    let mut p = maintenance(&repo(), config, &snapshot, &[], None, monday())
        .unwrap()
        .unwrap();
    p = Draft::from(p).update().build();
    prepare_one(&runner, &mut p, &mut snapshot, config).await;
    let calls = executor.calls.lock().unwrap();
    let bin = match leaf {
        "yarn.lock" => "yarn",
        "bun.lock" => "bun",
        "package-lock.json" if config.lockfiles.npm_engine == NpmEngine::Npm => "npm",
        _ => "aube",
    };
    let ran = recorded(&calls)
        .iter()
        .any(|(_, command)| command.first().map(String::as_str) == Some(bin));
    Outcome {
        status: p.action().status,
        reason: p.action().reason.clone().unwrap_or_default(),
        lock: p.contents().get(leaf).cloned(),
        markers: world.markers(),
        ran,
    }
}

const NPM_SEED: &str = "{\"name\":\"fixture\",\"version\":\"1.0.0\",\"lockfileVersion\":3,\"requires\":true,\"packages\":{\"\":{\"name\":\"fixture\",\"version\":\"1.0.0\"}}}\n";

/// npm: a direct git dependency is refused before npm starts; a git
/// dependency of a registry package fails for want of a Git transport.
/// Neither runs a script.
#[tokio::test]
async fn real_npm_runs_no_dependency_code_directly_or_transitively() {
    let world = World::new();
    let config = config_with("npmEngine = \"npm\"");
    let direct = run(
        &world,
        "package-lock.json",
        NPM_SEED,
        "canary-evil",
        &world.git_url(true),
        &config,
        None,
    )
    .await;
    assert_eq!(direct.status, BranchStatus::Skipped, "{}", direct.reason);
    assert!(
        direct
            .reason
            .contains("package.json names canary-evil from git+"),
        "{}",
        direct.reason
    );
    assert!(!direct.ran, "npm must not start for a refused lock");
    assert_eq!(direct.markers, Vec::<String>::new());

    let transitive = run(
        &world,
        "package-lock.json",
        NPM_SEED,
        "canary-mid-gitplus",
        "1.0.0",
        &config,
        None,
    )
    .await;
    assert!(transitive.ran);
    assert_ne!(
        transitive.status,
        BranchStatus::Planned,
        "{}",
        transitive.reason
    );
    assert_eq!(
        transitive.markers,
        Vec::<String>::new(),
        "{}",
        transitive.reason
    );
}

/// Yarn Berry: a direct git dependency is refused before Yarn starts; a git
/// dependency of a registry package fails for want of a Git transport, and a
/// GitHub one because the GitHub hosts are closed. With Git reachable, the
/// same transitive dependency runs its scripts, which is why both guards
/// exist.
#[tokio::test]
async fn real_yarn_runs_no_dependency_code_directly_or_transitively() {
    let world = World::new();
    let config = config();
    let direct = run(
        &world,
        "yarn.lock",
        BERRY_LOCK,
        "canary-evil",
        &world.git_url(false),
        &config,
        None,
    )
    .await;
    assert_eq!(direct.status, BranchStatus::Skipped, "{}", direct.reason);
    assert!(
        direct
            .reason
            .contains("package.json names canary-evil from http"),
        "{}",
        direct.reason
    );
    assert!(!direct.ran, "Yarn must not start for a refused lock");
    assert_eq!(direct.markers, Vec::<String>::new());

    for mid in ["canary-mid-dotgit", "canary-mid-github"] {
        let transitive = run(&world, "yarn.lock", BERRY_LOCK, mid, "1.0.0", &config, None).await;
        assert!(transitive.ran, "{mid}");
        assert_ne!(
            transitive.status,
            BranchStatus::Planned,
            "{mid}: {}",
            transitive.reason
        );
        assert_eq!(
            transitive.markers,
            Vec::<String>::new(),
            "{mid}: {}",
            transitive.reason
        );
        let expected = if mid == "canary-mid-github" {
            "blocked because of your configuration settings"
        } else {
            "not allowed"
        };
        assert!(
            transitive.reason.contains(expected),
            "{mid}: {}",
            transitive.reason
        );
    }

    // The control: the same transitive dependency with Git reachable is
    // packed, and packing runs its scripts despite enableScripts: false.
    let control = run(
        &world,
        "yarn.lock",
        BERRY_LOCK,
        "canary-mid-dotgit",
        "1.0.0",
        &config,
        Some("http"),
    )
    .await;
    assert!(
        control.markers.iter().any(|m| m.starts_with("evil-")),
        "the canary must catch Yarn packing a git dependency: {:?} {}",
        control.markers,
        control.reason
    );
    // And the lock it wrote is thrown away rather than published.
    assert_eq!(control.status, BranchStatus::Skipped, "{}", control.reason);
    assert!(
        control.reason.contains("the regenerated yarn.lock"),
        "{}",
        control.reason
    );
}

/// aube (writing `aube-lock.yaml` and `pnpm-lock.yaml`) and Bun lock a git
/// dependency, direct or through a registry package, without running a
/// script of it or of the registry package, even with Git reachable.
#[tokio::test]
async fn real_aube_and_bun_lock_git_dependencies_without_running_their_scripts() {
    let world = World::new();
    let config = config();
    for (leaf, seed) in [
        ("aube-lock.yaml", "lockfileVersion: '9.0'\n"),
        ("pnpm-lock.yaml", "lockfileVersion: '9.0'\n"),
        ("bun.lock", "{}\n"),
    ] {
        for (dependency, spec) in [
            ("canary-evil", world.git_url(true)),
            ("canary-mid-gitplus", "1.0.0".to_owned()),
        ] {
            let outcome = run(&world, leaf, seed, dependency, &spec, &config, Some("http")).await;
            assert!(outcome.ran, "{leaf} {dependency}");
            assert_eq!(
                outcome.markers,
                Vec::<String>::new(),
                "{leaf} {dependency}: {}",
                outcome.reason
            );
            // The tool fetched the git dependency, so the canary was reached.
            assert_eq!(
                outcome.status,
                BranchStatus::Planned,
                "{leaf} {dependency}: {}",
                outcome.reason
            );
            assert!(
                outcome
                    .lock
                    .as_deref()
                    .is_some_and(|lock| lock.contains("canary-evil")),
                "{leaf} {dependency}: {:?}",
                outcome.lock
            );
        }
    }
}

/// The guard every npm run loads stops a Node process started the way
/// pacote starts a git dependency's preparation, `_PACOTE_NO_PREPARE_` set,
/// before it runs anything, and lets every other Node process through. A
/// GitHub dependency pinned to a commit reaches that preparation over HTTPS,
/// which the missing Git transport does not stop.
#[test]
fn real_npm_prepare_guard_stops_git_dependency_preparation() {
    let temp = tempfile::tempdir().unwrap();
    let guard = temp.path().join("guard.cjs");
    std::fs::write(&guard, super::node::NPM_PREPARE_GUARD).unwrap();
    let marker = temp.path().join("ran");
    let script = format!(
        "require('fs').writeFileSync({:?}, 'x')",
        marker.to_string_lossy()
    );
    let node = |preparing: bool| {
        let mut command = Command::new("mise");
        command
            .args(["exec", &default_spec("node"), "--", "node", "-e", &script])
            .env("MISE_DATA_DIR", mise_store())
            .env("MISE_AUTO_INSTALL", "false")
            .env("NODE_OPTIONS", format!("--require={}", guard.display()));
        if preparing {
            command.env(
                "_PACOTE_NO_PREPARE_",
                "git+ssh://git@github.com/o/r.git#0123",
            );
        } else {
            command.env_remove("_PACOTE_NO_PREPARE_");
        }
        run_with_deadline(&mut command, std::time::Duration::from_secs(60))
    };
    let preparing = node(true);
    assert_eq!(preparing.status.code(), Some(97), "{preparing:?}");
    assert!(!marker.exists(), "the preparation ran");
    let plain = node(false);
    assert!(plain.status.success(), "{plain:?}");
    assert!(marker.exists(), "an ordinary npm process must run");
}
