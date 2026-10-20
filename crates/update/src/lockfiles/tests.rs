use super::*;
use rustovate_http::{Exchange, Transport};
use std::{future::Future, pin::Pin, sync::Mutex};

struct Fixture {
    files: BTreeMap<String, String>,
    fail: bool,
    real: bool,
    leaf: Option<String>,
    calls: Mutex<Vec<Vec<String>>>,
}
impl Executor for Fixture {
    fn run(
        &self,
        mut spec: CommandSpec,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, String>> + Send + '_>> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(spec.args.clone());
            if spec.args.iter().any(|s| s == "git") {
                assert!(
                    spec.env
                        .contains_key(std::ffi::OsStr::new("GIT_CONFIG_VALUE_0"))
                );
                if let Some(i) = spec.args.iter().position(|s| s == "init") {
                    let path = Path::new(&spec.args[i + 2]);
                    std::fs::create_dir_all(path).unwrap();
                    std::fs::write(path.join("HEAD"), "ref: refs/heads/main\n").unwrap();
                }
                if spec.args.iter().any(|s| s == "ls-tree") {
                    return Ok(self
                        .files
                        .iter()
                        .map(|(p, c)| format!("100644 blob {}\t{p}\0", blob_sha(c)))
                        .collect::<String>()
                        .into_bytes());
                }
                if let Some(i) = spec.args.iter().position(|s| s == "checkout") {
                    let root = Path::new(
                        spec.args
                            .iter()
                            .find_map(|s| s.strip_prefix("--work-tree="))
                            .unwrap(),
                    );
                    for path in &spec.args[i + 3..] {
                        write(root, path, &self.files[path]).unwrap();
                    }
                }
                return Ok(Vec::new());
            }
            for (k, v) in &spec.env {
                let key = k.to_string_lossy().to_ascii_lowercase();
                assert!(
                    ![
                        "token",
                        "secret",
                        "password",
                        "authorization",
                        "github",
                        "gh_",
                        "rustovate"
                    ]
                    .iter()
                    .any(|part| key.contains(part)),
                    "unexpected env {key}"
                );
                assert!(!v.to_string_lossy().contains("offline-installation"));
            }
            assert_eq!(spec.env[std::ffi::OsStr::new("MISE_NO_CONFIG")], "1");
            assert_eq!(spec.env[std::ffi::OsStr::new("MISE_NO_ENV")], "1");
            assert_eq!(spec.env[std::ffi::OsStr::new("MISE_NO_HOOKS")], "1");
            if self.real {
                if spec.args.iter().any(|a| a == "cargo")
                    && self
                        .files
                        .get("Cargo.toml")
                        .is_some_and(|s| s.contains("offline-registry-fixture"))
                {
                    let vendor = spec.cwd.join("vendor");
                    for version in ["1.0.0", "2.0.0"] {
                        let package = vendor.join(format!("fixture-{version}"));
                        std::fs::create_dir_all(package.join("src")).unwrap();
                        std::fs::write(package.join("Cargo.toml"),format!("[package]\nname='offline-registry-fixture'\nversion='{version}'\nedition='2024'\n")).unwrap();
                        std::fs::write(package.join("src/lib.rs"), "// vendored fixture\n")
                            .unwrap();
                        std::fs::write(
                            package.join(".cargo-checksum.json"),
                            "{\"files\":{},\"package\":null}",
                        )
                        .unwrap();
                    }
                    let cargo = spec
                        .args
                        .iter()
                        .find_map(|a| a.strip_prefix("CARGO_HOME="))
                        .map(PathBuf::from)
                        .unwrap_or_else(|| {
                            PathBuf::from(&spec.env[std::ffi::OsStr::new("CARGO_HOME")])
                        });
                    std::fs::create_dir_all(&cargo).unwrap();
                    std::fs::write(cargo.join("config.toml"),format!("[source.crates-io]\nreplace-with='fixture'\n[source.fixture]\ndirectory='{}'\n",vendor.display())).unwrap();
                }
                if spec.args.iter().any(|a| a == "bun") {
                    spec.args.push("--offline".into());
                }
                for (key, value) in [
                    ("MISE_AUTO_INSTALL", "false"),
                    ("MISE_EXEC_AUTO_INSTALL", "false"),
                    ("CARGO_NET_OFFLINE", "true"),
                    ("NPM_CONFIG_OFFLINE", "true"),
                    ("GOPROXY", "off"),
                    ("GOSUMDB", "off"),
                ] {
                    spec.env.insert(key.into(), value.into());
                }
                return SystemExecutor.run(spec).await;
            }
            let root = spec.cwd.parent().unwrap().join("tree");
            if self.fail {
                write(&root, "bun.lock", "half-written").unwrap();
                return Err("fixture resolution failure".into());
            }
            let leaf = if let Some(leaf) = &self.leaf {
                leaf.clone()
            } else if spec.args.iter().any(|s| s == "aube") {
                // aube is told which committed lockfile it may write.
                spec.env
                    .get(std::ffi::OsStr::new("NPM_CONFIG_USERCONFIG"))
                    .and_then(|path| std::fs::read_to_string(PathBuf::from(path)).ok())
                    .and_then(|text| {
                        text.lines().find_map(|line| {
                            line.strip_prefix("default-lockfile=").map(str::to_owned)
                        })
                    })
                    .unwrap_or_else(|| "package-lock.json".into())
            } else if spec.args.iter().any(|s| s == "yarn") {
                "yarn.lock".into()
            } else if spec.args.iter().any(|s| s == "npm") {
                "package-lock.json".into()
            } else if spec.args.iter().any(|s| s == "cargo") {
                "Cargo.lock".into()
            } else if spec.args.iter().any(|s| s == "go") {
                "go.sum".into()
            } else if spec.args.iter().any(|s| s == "terraform") {
                ".terraform.lock.hcl".into()
            } else {
                "bun.lock".into()
            };
            write(&root, &leaf, "fixture deterministic lock\n").unwrap();
            Ok(Vec::new())
        })
    }
}
fn config() -> Config {
    config_with("")
}
fn config_with(lockfiles: &str) -> Config {
    rustovate_config::load_files(
        "/repo/rustovate.pkl",
        BTreeMap::from([(
            "/repo/rustovate.pkl".into(),
            format!(
                "amends \"Rustovate.pkl\"\nlockfiles {{ refresh = true; tidyGo = true; {lockfiles} }}\n"
            ),
        )]),
    )
    .unwrap()
}
fn fixture(files: BTreeMap<String, String>, fail: bool, real: bool) -> Arc<Fixture> {
    fixture_with(files, fail, real, None)
}
fn fixture_with(
    files: BTreeMap<String, String>,
    fail: bool,
    real: bool,
    leaf: Option<String>,
) -> Arc<Fixture> {
    Arc::new(Fixture {
        files,
        fail,
        real,
        leaf,
        calls: Mutex::new(Vec::new()),
    })
}
fn repo() -> Repository {
    Repository {
        repository: "demo/app".into(),
        commit: Some("a".repeat(40)),
        default_branch: Some("main".into()),
        status: rustovate_report::Status::Complete,
        reason: None,
        warnings: Vec::new(),
        package_files: Vec::new(),
    }
}
fn snapshot(files: &BTreeMap<String, String>) -> Snapshot {
    Snapshot {
        paths: files.keys().cloned().collect(),
        files: files
            .iter()
            .map(|(p, c)| {
                (
                    p.clone(),
                    Blob {
                        sha: blob_sha(c),
                        mode: "100644".into(),
                        content: c.clone(),
                    },
                )
            })
            .collect(),
    }
}
fn monday() -> chrono::DateTime<chrono::Utc> {
    "2026-10-19T01:00:00Z".parse().unwrap()
}
struct NoApi;
impl Transport for NoApi {
    fn send(&self, _: reqwest::Request) -> Exchange<'_> {
        panic!("failed tooling must never reach the API writer")
    }
}

#[tokio::test]
async fn tool_failure_is_reported_without_api_writes_or_partial_scratch_files() {
    let files = BTreeMap::from([
        ("package.json".into(), "{}\n".into()),
        ("bun.lock".into(), "{}\n".into()),
    ]);
    let executor = fixture(files.clone(), true, false);
    let temp = tempfile::tempdir().unwrap();
    let runner =
        Runner::new(temp.path().into(), "offline-installation").with_executor(executor.clone());
    let mut snapshot = snapshot(&files);
    let config = config();
    let mut p = maintenance(&repo(), &config, &snapshot, monday())
        .unwrap()
        .unwrap();
    runner.prepare(&mut p, &mut snapshot, &config).await;
    assert_eq!(p.action.status, ActionStatus::Error, "{:?}", p.action);
    assert!(
        p.action
            .reason
            .as_ref()
            .unwrap()
            .contains("fixture resolution failure")
    );
    assert!(p.contents.is_empty());
    assert!(!p.lockfiles_ready);
    assert!(
        !std::fs::read_dir(temp.path()).unwrap().any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("run-"))
    );
    let cache = rustovate_cache::Cache::open(":memory:", Default::default())
        .await
        .unwrap();
    let client = rustovate_github::Client::with_transport(
        "offline",
        cache,
        Default::default(),
        Arc::new(NoApi),
    )
    .unwrap();
    crate::apply_all(
        &client,
        std::slice::from_mut(&mut p),
        &snapshot,
        "main",
        rustovate_config::Rebase::Behind,
        false,
    )
    .await;
    assert!(p.action.commit.is_none());
}

#[tokio::test]
async fn all_tools_are_targeted_scrubbed_and_versions_are_read_without_loading_repository_env() {
    let files=BTreeMap::from([
        ("mise.toml".into(),"[tools]\nbun = '1.4.3'\nnode = { version = '24.21.0' }\n[env]\nSHOULD_NOT_RUN = '{{exec(command=\"false\")}}'\n".into()),
        ("package.json".into(),"{}\n".into()),("bun.lock".into(),"{}\n".into()),("package-lock.json".into(),"{}\n".into()),
        ("Cargo.toml".into(),"[package]\nname='fixture'\nversion='1.0.0'\n".into()),("Cargo.lock".into(),"version = 4\n".into()),
        ("go.mod".into(),"module example.com/fixture\ngo 1.22\n".into()),("go.sum".into(),"\n".into()),
        ("providers.tf".into(),"terraform {}\n".into()),(".terraform.lock.hcl".into(),"\n".into()),
    ]);
    let executor = fixture(files.clone(), false, false);
    let temp = tempfile::tempdir().unwrap();
    let runner =
        Runner::new(temp.path().into(), "offline-installation").with_executor(executor.clone());
    let mut snapshot = snapshot(&files);
    let config = config();
    let mut p = maintenance(&repo(), &config, &snapshot, monday())
        .unwrap()
        .unwrap();
    p.maintenance = false;
    let cargo = p
        .lock_jobs
        .iter_mut()
        .find(|j| j.ecosystem == Ecosystem::Cargo)
        .unwrap();
    cargo.package = Some("serde".into());
    cargo.current = Some("1.0.200".into());
    cargo.target = Some("1.0.229".into());
    let go = p
        .lock_jobs
        .iter_mut()
        .find(|j| j.ecosystem == Ecosystem::Go)
        .unwrap();
    go.package = Some("example.com/mod".into());
    go.current = Some("v1.0.0".into());
    go.target = Some("v1.1.0".into());
    runner.prepare(&mut p, &mut snapshot, &config).await;
    assert_eq!(p.action.status, ActionStatus::Planned, "{:?}", p.action);
    assert!(p.lockfiles_ready);
    assert_eq!(p.action.files.len(), 5);
    let calls = executor.calls.lock().unwrap();
    for sequence in [
        vec!["bun@1.4.3", "--", "bun"],
        vec!["aube@2.8.0", "--", "aube", "-C"],
        vec!["-p", "serde@1.0.200", "--precise", "1.0.229"],
        vec!["get", "example.com/mod@v1.1.0"],
        vec!["mod", "tidy"],
        vec!["providers", "lock"],
    ] {
        assert!(
            calls.iter().any(|args| args
                .windows(sequence.len())
                .any(|w| w.iter().map(String::as_str).eq(sequence.iter().copied()))),
            "missing command {sequence:?}"
        );
    }
    // npm locks are aube's by default; the npm binary never runs.
    assert!(
        !calls
            .iter()
            .any(|args| args.windows(2).any(|w| w == ["node@24.21.0", "--"])),
        "native npm must not run for an npm lock"
    );
}

const BERRY_LOCK: &str = "# This file is generated by running \"yarn install\" inside your project.\n\n__metadata:\n  version: 10\n  cacheKey: 10c0\n\n\"fixture@workspace:.\":\n  version: 0.0.0-use.local\n";
const CLASSIC_LOCK: &str =
    "# THIS IS AN AUTOGENERATED FILE. DO NOT EDIT THIS FILE DIRECTLY.\n# yarn lockfile v1\n\n";

/// The mise tool specifications and the command of one recorded invocation.
fn invocation(args: &[String]) -> Option<(Vec<String>, Vec<String>)> {
    let start = args.iter().position(|arg| arg == "exec")?;
    let rest = &args[start + 1..];
    let end = rest.iter().position(|arg| arg == "--")?;
    Some((rest[..end].to_vec(), rest[end + 1..].to_vec()))
}
fn recorded(calls: &[Vec<String>]) -> Vec<(Vec<String>, Vec<String>)> {
    calls.iter().filter_map(|args| invocation(args)).collect()
}
fn starts(command: &[String], sequence: &[&str]) -> bool {
    command.len() >= sequence.len()
        && command
            .iter()
            .zip(sequence)
            .all(|(actual, expected)| actual == expected)
}
fn contains(command: &[String], sequence: &[&str]) -> bool {
    let mut rest = command.iter();
    sequence
        .iter()
        .all(|expected| rest.any(|actual| actual == expected))
}

/// Node lockfiles are regenerated by the tool that owns their format.
#[tokio::test]
async fn node_lockfiles_use_aube_yarn_and_bun_by_lockfile_format() {
    let files = BTreeMap::from([
        ("package.json".into(), "{}\n".into()),
        ("package-lock.json".into(), "{}\n".into()),
        ("pnpm-lock.yaml".into(), "lockfileVersion: '9.0'\n".into()),
        ("yarn.lock".into(), BERRY_LOCK.into()),
        ("bun.lock".into(), "{}\n".into()),
    ]);
    let executor = fixture(files.clone(), false, false);
    let temp = tempfile::tempdir().unwrap();
    let runner =
        Runner::new(temp.path().into(), "offline-installation").with_executor(executor.clone());
    let mut snapshot = snapshot(&files);
    let config = config();
    let mut p = maintenance(&repo(), &config, &snapshot, monday())
        .unwrap()
        .unwrap();
    p.maintenance = false;
    runner.prepare(&mut p, &mut snapshot, &config).await;
    assert_eq!(p.action.status, ActionStatus::Planned, "{:?}", p.action);
    let calls = executor.calls.lock().unwrap();
    let commands = recorded(&calls);
    assert!(
        commands
            .iter()
            .any(|(specs, command)| specs.as_slice() == ["aube@2.8.0"]
                && starts(command, &["aube", "-C"])
                && contains(command, &["install", "--lockfile-only", "--ignore-scripts"])),
        "missing aube command {commands:?}"
    );
    assert!(
        commands.iter().any(
            |(specs, command)| specs.as_slice() == ["node@24.21.0", "yarn@4.18.1"]
                && starts(command, &["yarn", "install", "--mode=update-lockfile"])
        ),
        "missing yarn command {commands:?}"
    );
    assert!(
        commands
            .iter()
            .any(|(specs, command)| specs.as_slice() == ["bun@1.4.3"]
                && starts(command, &["bun", "install", "--cwd"])
                && contains(command, &["--lockfile-only", "--ignore-scripts"])),
        "missing bun command {commands:?}"
    );
    // aube is told which committed lockfile it may write.
    assert_eq!(
        commands
            .iter()
            .filter(|(specs, _)| specs.as_slice() == ["aube@2.8.0"])
            .count(),
        2,
        "one aube call per pnpm and npm lock"
    );
}

/// The repository setting keeps native npm for npm lockfiles only.
#[tokio::test]
async fn the_npm_engine_setting_restores_native_npm_for_npm_locks() {
    let files = BTreeMap::from([
        ("package.json".into(), "{}\n".into()),
        ("package-lock.json".into(), "{}\n".into()),
        ("pnpm-lock.yaml".into(), "lockfileVersion: '9.0'\n".into()),
    ]);
    let executor = fixture(files.clone(), false, false);
    let temp = tempfile::tempdir().unwrap();
    let runner =
        Runner::new(temp.path().into(), "offline-installation").with_executor(executor.clone());
    let mut snapshot = snapshot(&files);
    let config = config_with("npmEngine = \"npm\"");
    let mut p = maintenance(&repo(), &config, &snapshot, monday())
        .unwrap()
        .unwrap();
    p.maintenance = false;
    runner.prepare(&mut p, &mut snapshot, &config).await;
    assert_eq!(p.action.status, ActionStatus::Planned, "{:?}", p.action);
    let calls = executor.calls.lock().unwrap();
    let commands = recorded(&calls);
    assert!(
        commands
            .iter()
            .any(|(specs, command)| specs.as_slice() == ["node@24.21.0"]
                && starts(command, &["npm", "install", "--prefix"])
                && contains(
                    command,
                    &[
                        "--package-lock-only",
                        "--ignore-scripts",
                        "--no-audit",
                        "--no-fund"
                    ]
                )),
        "missing npm command {commands:?}"
    );
    // pnpm keeps aube whatever the npm engine says.
    assert!(
        commands
            .iter()
            .any(|(specs, command)| specs.as_slice() == ["aube@2.8.0"]
                && starts(command, &["aube", "-C"])),
        "pnpm must keep aube {commands:?}"
    );
}

/// Yarn's version follows `packageManager`, then the repository pin, then the
/// format of the committed lockfile.
#[test]
fn yarn_version_comes_from_the_package_manager_field() {
    let versions = BTreeMap::from([("yarn".to_string(), "1.22.22".to_string())]);
    let manifest = |value: &str| Some(value.to_string());
    assert_eq!(
        node::yarn_version(
            manifest("{\"packageManager\":\"yarn@4.18.1\"}").as_deref(),
            BERRY_LOCK,
            &versions,
            "1.22.22",
            "4.18.1"
        )
        .unwrap(),
        "4.18.1"
    );
    // Corepack hashes are not versions.
    assert_eq!(
        node::yarn_version(
            manifest("{\"packageManager\":\"yarn@4.18.1+sha512.abc\"}").as_deref(),
            BERRY_LOCK,
            &versions,
            "1.22.22",
            "4.18.1"
        )
        .unwrap(),
        "4.18.1"
    );
    // A range or dist-tag is not a version and falls back to the lock format.
    assert_eq!(
        node::yarn_version(
            manifest("{\"packageManager\":\"yarn@^4.0.0\"}").as_deref(),
            BERRY_LOCK,
            &versions,
            "1.22.22",
            "4.18.1"
        )
        .unwrap(),
        "4.18.1"
    );
    assert_eq!(
        node::yarn_version(
            manifest("{\"packageManager\":\"yarn@1.22.19\"}").as_deref(),
            CLASSIC_LOCK,
            &versions,
            "1.22.22",
            "4.18.1"
        )
        .unwrap(),
        "1.22.19"
    );
    // Without a field the lockfile format selects the pinned default.
    assert_eq!(
        node::yarn_version(None, CLASSIC_LOCK, &versions, "1.22.22", "4.18.1").unwrap(),
        "1.22.22"
    );
    assert_eq!(
        node::yarn_version(None, BERRY_LOCK, &versions, "1.22.22", "4.18.1").unwrap(),
        "4.18.1"
    );
    // A repository pin wins over the default when it matches the format.
    let pinned = BTreeMap::from([("yarn".to_string(), "4.10.3".to_string())]);
    assert_eq!(
        node::yarn_version(None, BERRY_LOCK, &pinned, "1.22.22", "4.18.1").unwrap(),
        "4.10.3"
    );
    assert!(node::yarn_version(None, "not a lockfile", &versions, "1.22.22", "4.18.1").is_err());
}

/// A manifest edit that changes recorded dependencies must change the lock.
#[tokio::test]
async fn an_unchanged_lockfile_after_a_manifest_edit_fails_the_branch() {
    let files = BTreeMap::from([
        (
            "package.json".into(),
            "{\"dependencies\":{\"fixture\":\"1.0.0\"}}\n".into(),
        ),
        (
            "package-lock.json".into(),
            "fixture deterministic lock\n".into(),
        ),
    ]);
    let executor = fixture(files.clone(), false, false);
    let temp = tempfile::tempdir().unwrap();
    let runner =
        Runner::new(temp.path().into(), "offline-installation").with_executor(executor.clone());
    let mut snapshot = snapshot(&files);
    let config = config();
    let mut p = maintenance(&repo(), &config, &snapshot, monday())
        .unwrap()
        .unwrap();
    p.maintenance = false;
    p.contents.insert(
        "package.json".into(),
        "{\"dependencies\":{\"fixture\":\"2.0.0\"}}\n".into(),
    );
    runner.prepare(&mut p, &mut snapshot, &config).await;
    assert_eq!(p.action.status, ActionStatus::Error, "{:?}", p.action);
    assert!(
        p.action
            .reason
            .as_ref()
            .unwrap()
            .contains("did not change after the manifest edit"),
        "{:?}",
        p.action.reason
    );
    assert!(!p.lockfiles_ready);
}

/// An edit that cannot affect the lock is not a failure.
#[tokio::test]
async fn a_manifest_edit_outside_the_dependencies_leaves_the_lock_alone() {
    let files = BTreeMap::from([
        (
            "package.json".into(),
            "{\"description\":\"first\",\"dependencies\":{\"fixture\":\"1.0.0\"}}\n".into(),
        ),
        (
            "package-lock.json".into(),
            "fixture deterministic lock\n".into(),
        ),
    ]);
    let executor = fixture(files.clone(), false, false);
    let temp = tempfile::tempdir().unwrap();
    let runner =
        Runner::new(temp.path().into(), "offline-installation").with_executor(executor.clone());
    let mut snapshot = snapshot(&files);
    let config = config();
    let mut p = maintenance(&repo(), &config, &snapshot, monday())
        .unwrap()
        .unwrap();
    p.maintenance = false;
    p.contents.insert(
        "package.json".into(),
        "{\"description\":\"second\",\"dependencies\":{\"fixture\":\"1.0.0\"}}\n".into(),
    );
    runner.prepare(&mut p, &mut snapshot, &config).await;
    assert_eq!(p.action.status, ActionStatus::Planned, "{:?}", p.action);
    assert!(p.lockfiles_ready);
}

/// aube must never introduce its own lockfile format.
#[tokio::test]
async fn aube_lock_yaml_creation_fails_the_branch() {
    let files = BTreeMap::from([
        ("package.json".into(), "{}\n".into()),
        ("package-lock.json".into(), "{}\n".into()),
    ]);
    let executor = fixture_with(files.clone(), false, false, Some("aube-lock.yaml".into()));
    let temp = tempfile::tempdir().unwrap();
    let runner =
        Runner::new(temp.path().into(), "offline-installation").with_executor(executor.clone());
    let mut snapshot = snapshot(&files);
    let config = config();
    let mut p = maintenance(&repo(), &config, &snapshot, monday())
        .unwrap()
        .unwrap();
    p.maintenance = false;
    runner.prepare(&mut p, &mut snapshot, &config).await;
    assert_eq!(p.action.status, ActionStatus::Error, "{:?}", p.action);
    assert!(
        p.action
            .reason
            .as_ref()
            .unwrap()
            .contains("aube wrote aube-lock.yaml"),
        "{:?}",
        p.action.reason
    );
}

/// Maintenance refreshes every Node lockfile within the manifest ranges.
#[tokio::test]
async fn maintenance_refreshes_node_lockfiles_within_the_manifest_ranges() {
    let files = BTreeMap::from([
        ("package.json".into(), "{}\n".into()),
        ("package-lock.json".into(), "{}\n".into()),
        ("pnpm-lock.yaml".into(), "lockfileVersion: '9.0'\n".into()),
        ("yarn.lock".into(), CLASSIC_LOCK.into()),
        ("bun.lock".into(), "{}\n".into()),
    ]);
    let executor = fixture(files.clone(), false, false);
    let temp = tempfile::tempdir().unwrap();
    let runner =
        Runner::new(temp.path().into(), "offline-installation").with_executor(executor.clone());
    let mut snapshot = snapshot(&files);
    let config = config_with("npmEngine = \"npm\"");
    let mut p = maintenance(&repo(), &config, &snapshot, monday())
        .unwrap()
        .unwrap();
    assert!(p.maintenance);
    runner.prepare(&mut p, &mut snapshot, &config).await;
    assert_eq!(p.action.status, ActionStatus::Planned, "{:?}", p.action);
    let calls = executor.calls.lock().unwrap();
    let commands = recorded(&calls);
    assert!(
        commands
            .iter()
            .any(|(specs, command)| specs.as_slice() == ["aube@2.8.0"]
                && starts(command, &["aube", "-C"])
                && contains(
                    command,
                    &["update", "--lockfile-only", "--no-save", "--ignore-scripts"]
                )),
        "missing aube maintenance command {commands:?}"
    );
    assert!(
        commands
            .iter()
            .any(|(specs, command)| specs.as_slice() == ["node@24.21.0"]
                && starts(command, &["npm", "update", "--prefix"])
                && contains(command, &["--package-lock-only", "--ignore-scripts"])),
        "missing npm maintenance command {commands:?}"
    );
    // Yarn Classic has no lock-only refresh; it reinstalls without scripts.
    assert!(
        commands.iter().any(|(specs, command)| specs.as_slice()
            == ["node@24.21.0", "yarn@1.22.22"]
            && starts(command, &["yarn", "install", "--ignore-scripts"])),
        "missing yarn command {commands:?}"
    );
    assert!(
        commands
            .iter()
            .any(|(specs, command)| specs.as_slice() == ["bun@1.4.3"]
                && starts(command, &["bun", "install", "--cwd"])
                && contains(command, &["--lockfile-only", "--ignore-scripts"])),
        "missing bun command {commands:?}"
    );
}

#[tokio::test]
async fn real_tools_produce_deterministic_lockfiles_from_local_offline_fixtures() {
    for (ecosystem,files,lock) in [
        (Ecosystem::Node,BTreeMap::from([
            ("package.json".into(),"{\"name\":\"fixture\",\"version\":\"1.0.0\",\"dependencies\":{\"local-fixture\":\"file:./local\"}}\n".into()),
            ("local/package.json".into(),"{\"name\":\"local-fixture\",\"version\":\"1.0.0\"}\n".into()),
            ("bun.lock".into(),"{}\n".into()),
            ("package-lock.json".into(),"{}\n".into()),
        ]),"bun.lock"),
        (Ecosystem::Cargo,BTreeMap::from([
            ("Cargo.toml".into(),"[package]\nname = 'fixture'\nversion = '1.0.0'\nedition = '2024'\n".into()),
            ("src/lib.rs".into(),"// offline fixture\n".into()),
            ("Cargo.lock".into(),"version = 4\n".into()),
        ]),"Cargo.lock"),
        (Ecosystem::Go,BTreeMap::from([
            ("go.mod".into(),"module example.com/fixture\n\ngo 1.22\n".into()),
            ("main.go".into(),"package main\nfunc main() {}\n".into()),
            ("go.sum".into(),"\n".into()),
        ]),"go.mod"),
    ] {
        let mut outputs=Vec::new();
        for _ in 0..2 {
            let executor=fixture(files.clone(),false,true);
            let temp=tempfile::tempdir().unwrap();
            let runner=Runner::new(temp.path().into(),"offline-installation").with_executor(executor);
            let mut snapshot=snapshot(&files);
            let config=config();
            let mut p=maintenance(&repo(),&config,&snapshot,monday()).unwrap().unwrap();
            assert!(p.lock_jobs.iter().all(|j|j.ecosystem==ecosystem));
            runner.prepare(&mut p,&mut snapshot,&config).await;
            assert!(matches!(p.action.status,ActionStatus::Planned|ActionStatus::Skipped),"{ecosystem:?}: {:?}",p.action);
            assert!(p.lockfiles_ready);
            assert!(p.contents.contains_key(lock) || ecosystem==Ecosystem::Go);
            outputs.push((p.contents,p.deleted));
        }
        assert_eq!(outputs[0],outputs[1],"{ecosystem:?}");
    }
}

/// aube regenerates pnpm and npm locks in place from local offline fixtures.
#[tokio::test]
async fn real_aube_rewrites_pnpm_and_npm_locks_in_place() {
    for (leaf, seed) in [
        ("pnpm-lock.yaml", "lockfileVersion: '9.0'\n"),
        (
            "package-lock.json",
            "{\"name\":\"fixture\",\"version\":\"1.0.0\",\"lockfileVersion\":3,\"requires\":true,\"packages\":{\"\":{\"name\":\"fixture\",\"version\":\"1.0.0\"}}}\n",
        ),
    ] {
        let files = BTreeMap::from([
            (
                "package.json".into(),
                "{\"name\":\"fixture\",\"version\":\"1.0.0\",\"dependencies\":{\"local-fixture\":\"file:./local\"}}\n".into(),
            ),
            (
                "local/package.json".into(),
                "{\"name\":\"local-fixture\",\"version\":\"1.0.0\"}\n".into(),
            ),
            (leaf.into(), seed.into()),
        ]);
        let executor = fixture(files.clone(), false, true);
        let temp = tempfile::tempdir().unwrap();
        let runner =
            Runner::new(temp.path().into(), "offline-installation").with_executor(executor.clone());
        let mut snapshot = snapshot(&files);
        let config = config();
        let mut p = maintenance(&repo(), &config, &snapshot, monday())
            .unwrap()
            .unwrap();
        p.maintenance = false;
        runner.prepare(&mut p, &mut snapshot, &config).await;
        assert_eq!(
            p.action.status,
            ActionStatus::Planned,
            "{leaf}: {:?}",
            p.action
        );
        assert!(p.contents[leaf].contains("local-fixture"), "{leaf}");
        assert!(
            !p.contents.contains_key("aube-lock.yaml"),
            "{leaf}: aube must not introduce its own lockfile"
        );
        let calls = executor.calls.lock().unwrap();
        let commands = recorded(&calls);
        assert!(
            commands
                .iter()
                .any(|(specs, _)| specs.as_slice() == ["aube@2.8.0"]),
            "{leaf}: missing aube command {commands:?}"
        );
    }
}

#[tokio::test]
async fn scheduled_refresh_uses_stable_name_windows_weekly_cadence_and_wait_hours() {
    let files = BTreeMap::from([("Cargo.lock".into(), "version = 4\n".into())]);
    let snapshot = snapshot(&files);
    let mut config = config();
    let repo = repo();
    let mut p = maintenance(&repo, &config, &snapshot, monday())
        .unwrap()
        .unwrap();
    assert_eq!(p.action.branch, "rustovate/lock-file-maintenance");
    assert_eq!(p.action.status, ActionStatus::Planned);
    let outside = maintenance(
        &repo,
        &config,
        &snapshot,
        monday() + chrono::Duration::days(1),
    )
    .unwrap()
    .unwrap();
    assert_eq!(outside.action.status, ActionStatus::Skipped);
    let cache = rustovate_cache::Cache::open(":memory:", Default::default())
        .await
        .unwrap();
    p.lockfiles_ready = true;
    p.action.status = ActionStatus::Skipped;
    p.action.reason = Some("lockfiles already current".into());
    record_refresh(&cache, &[p], &[], monday()).await.unwrap();
    for days in [0, 7] {
        config.lockfiles.wait_hours = 200;
        let now = monday() + chrono::Duration::days(days);
        let mut p = maintenance(&repo, &config, &snapshot, now)
            .unwrap()
            .unwrap();
        defer_completed_refresh(&cache, &mut p, &config, now)
            .await
            .unwrap();
        assert_eq!(p.action.status, ActionStatus::Skipped);
    }
    config.lockfiles.wait_hours = 0;
    let mut next = maintenance(
        &repo,
        &config,
        &snapshot,
        monday() + chrono::Duration::days(7),
    )
    .unwrap()
    .unwrap();
    defer_completed_refresh(
        &cache,
        &mut next,
        &config,
        monday() + chrono::Duration::days(7),
    )
    .await
    .unwrap();
    assert_eq!(next.action.status, ActionStatus::Planned);
}

#[tokio::test]
async fn process_output_is_bounded_and_tool_timeout_terminates_the_process() {
    let temp = tempfile::tempdir().unwrap();
    let runner = Runner::new(temp.path().into(), "offline-installation");
    let driver = temp.path().join("driver");
    std::fs::create_dir_all(&driver).unwrap();
    let mut spec = runner.command(&driver);
    spec.args
        .extend(["exec".into(), "--".into(), "sleep".into(), "10".into()]);
    spec.timeout = std::time::Duration::from_millis(100);
    let started = std::time::Instant::now();
    assert!(
        SystemExecutor
            .run(spec)
            .await
            .unwrap_err()
            .contains("time limit")
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(3));
    let mut spec = runner.command(&driver);
    spec.args.extend(["exec".into(),"bun@1.4.3".into(),"--".into(),"bun".into(),"-e".into(),"process.stdout.write('x'.repeat(100000)); process.stderr.write('y'.repeat(100000)); process.exit(2)".into()]);
    spec.env.insert("MISE_AUTO_INSTALL".into(), "false".into());
    spec.env
        .insert("MISE_EXEC_AUTO_INSTALL".into(), "false".into());
    let error = SystemExecutor.run(spec).await.unwrap_err();
    assert!(error.contains("exit"));
    assert!(error.len() <= 8300);
}

struct LocalCheckout {
    source: PathBuf,
    calls: Mutex<Vec<Vec<String>>>,
}
impl Executor for LocalCheckout {
    fn run(
        &self,
        mut spec: CommandSpec,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, String>> + Send + '_>> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(spec.args.clone());
            for arg in &mut spec.args {
                if arg == "https://github.com/demo/app.git" {
                    *arg = format!("file://{}", self.source.display());
                }
            }
            spec.env.insert("GIT_ALLOW_PROTOCOL".into(), "file".into());
            spec.env.insert("CARGO_NET_OFFLINE".into(), "true".into());
            spec.env.insert("MISE_AUTO_INSTALL".into(), "false".into());
            spec.env
                .insert("MISE_EXEC_AUTO_INSTALL".into(), "false".into());
            SystemExecutor.run(spec).await
        })
    }
}
#[tokio::test]
async fn real_git_cache_is_reused_and_fetches_new_bases_incrementally_offline() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let driver = temp.path().join("driver");
    std::fs::create_dir_all(&driver).unwrap();
    let runner = Runner::new(temp.path().join("cache"), "offline-installation");
    async fn git(runner: &Runner, driver: &Path, args: Vec<String>) -> String {
        let mut spec = runner.command(driver);
        spec.args.extend(["exec".into(), "--".into(), "git".into()]);
        spec.args.extend(args);
        String::from_utf8(SystemExecutor.run(spec).await.unwrap()).unwrap()
    }
    git(
        &runner,
        &driver,
        vec!["init".into(), source.to_string_lossy().into_owned()],
    )
    .await;
    std::fs::create_dir_all(source.join("src")).unwrap();
    std::fs::write(source.join("src/lib.rs"), "// fixture\n").unwrap();
    std::fs::write(source.join("README.md"), "unneeded blob\n").unwrap();
    std::fs::write(source.join("Cargo.lock"), "version = 4\n").unwrap();
    let executor = Arc::new(LocalCheckout {
        source: source.clone(),
        calls: Mutex::new(Vec::new()),
    });
    let runner = runner.with_executor(executor.clone());
    for version in ["1.0.0", "2.0.0"] {
        std::fs::write(
            source.join("Cargo.toml"),
            format!("[package]\nname='fixture'\nversion='{version}'\nedition='2024'\n"),
        )
        .unwrap();
        let prefix = vec!["-C".into(), source.to_string_lossy().into_owned()];
        let mut args = prefix.clone();
        args.extend(["add".into(), "--all".into()]);
        git(&runner, &driver, args).await;
        let mut args = prefix.clone();
        args.extend([
            "-c".into(),
            "user.name=Fixture".into(),
            "-c".into(),
            "user.email=fixture@example.invalid".into(),
            "commit".into(),
            "-m".into(),
            format!("fixture {version}"),
        ]);
        git(&runner, &driver, args).await;
        let mut args = prefix;
        args.extend(["rev-parse".into(), "HEAD".into()]);
        let base = git(&runner, &driver, args).await;
        let mut repo = repo();
        repo.commit = Some(base.trim().into());
        let files = BTreeMap::from([("Cargo.lock".into(), "version = 4\n".into())]);
        let mut snapshot = snapshot(&files);
        let config = config();
        let mut p = maintenance(&repo, &config, &snapshot, monday())
            .unwrap()
            .unwrap();
        runner.prepare(&mut p, &mut snapshot, &config).await;
        assert_eq!(p.action.status, ActionStatus::Planned, "{:?}", p.action);
        assert!(p.contents["Cargo.lock"].contains(version));
    }
    let calls = executor.calls.lock().unwrap();
    assert_eq!(
        calls
            .iter()
            .filter(|args| args.iter().any(|a| a == "init"))
            .count(),
        1
    );
    assert_eq!(
        calls
            .iter()
            .filter(|args| args.iter().any(|a| a == "fetch"))
            .count(),
        2
    );
    assert!(
        calls
            .iter()
            .filter(|args| args.iter().any(|a| a == "checkout"))
            .all(|args| !args.iter().any(|a| a == "README.md"))
    );
}

#[tokio::test]
async fn real_cargo_and_go_targeted_updates_retain_the_selected_manifest_and_lock_together() {
    for ecosystem in [Ecosystem::Cargo, Ecosystem::Go] {
        let (files, manifest, old, new, package, current, target, source) = if ecosystem
            == Ecosystem::Cargo
        {
            let old = "[package]\nname='fixture'\nversion='1.0.0'\nedition='2024'\n[dependencies]\noffline-registry-fixture='1.0.0'\n";
            (BTreeMap::from([
                ("Cargo.toml".into(),old.into()),("src/lib.rs".into(),"// fixture\n".into()),
                ("Cargo.lock".into(),"version = 4\n[[package]]\nname='fixture'\nversion='1.0.0'\ndependencies=['offline-registry-fixture']\n[[package]]\nname='offline-registry-fixture'\nversion='1.0.0'\nsource='registry+https://github.com/rust-lang/crates.io-index'\n".into()),
            ]),"Cargo.toml",old.to_owned(),old.replace("fixture='1.0.0'","fixture='2.0.0'"),"offline-registry-fixture","1.0.0","2.0.0","crates")
        } else {
            let old = "module example.com/fixture\n\ngo 1.22\n\nrequire example.com/local v1.0.0\n\nreplace example.com/local => ./local\n";
            (
                BTreeMap::from([
                    ("go.mod".into(), old.into()),
                    ("go.sum".into(), "\n".into()),
                    (
                        "main.go".into(),
                        "package main\nimport _ \"example.com/local\"\nfunc main() {}\n".into(),
                    ),
                    (
                        "local/go.mod".into(),
                        "module example.com/local\n\ngo 1.22\n".into(),
                    ),
                    ("local/local.go".into(), "package local\n".into()),
                ]),
                "go.mod",
                old.to_owned(),
                old.replace("v1.0.0", "v1.1.0"),
                "example.com/local",
                "v1.0.0",
                "v1.1.0",
                "go",
            )
        };
        let executor = fixture(files.clone(), false, true);
        let temp = tempfile::tempdir().unwrap();
        let runner =
            Runner::new(temp.path().into(), "offline-installation").with_executor(executor);
        let mut snapshot = snapshot(&files);
        let config = config();
        let mut p = maintenance(&repo(), &config, &snapshot, monday())
            .unwrap()
            .unwrap();
        p.maintenance = false;
        p.lock_jobs[0].package = Some(package.into());
        p.lock_jobs[0].current = Some(current.into());
        p.lock_jobs[0].target = Some(target.into());
        p.contents.insert(manifest.into(), new);
        p.action.files.push(FileAction {
            path: manifest.into(),
            blob_sha: blob_sha(&old),
            diff: String::new(),
        });
        p.changes.push(crate::Change {
            source: source.into(),
            package: package.into(),
            file: manifest.into(),
            current: current.into(),
            current_version: None,
            value: Some(target.into()),
            version: Some(target.into()),
            digest: None,
            kind: "major".into(),
            merge: false,
            labels: Vec::new(),
            release_notes: false,
            body_notes: Vec::new(),
        });
        runner.prepare(&mut p, &mut snapshot, &config).await;
        assert_eq!(
            p.action.status,
            ActionStatus::Planned,
            "{ecosystem:?}: {:?}",
            p.action
        );
        assert!(p.contents[manifest].contains(target));
        if ecosystem == Ecosystem::Cargo {
            assert!(p.contents["Cargo.lock"].contains("2.0.0"));
        }
    }
}

#[test]
fn provider_lock_drift_is_rejected_before_publication() {
    let mut files=BTreeMap::from([
        ("providers.tf".into(),"terraform {\n  required_providers {\n    aws = {\n      source = \"hashicorp/aws\"\n      version = \"1.0.0\"\n    }\n  }\n}\n".into()),
        (".terraform.lock.hcl".into(),"provider \"registry.terraform.io/hashicorp/aws\" {\n  version = \"2.0.0\"\n}\n".into()),
    ]);
    let config = config();
    let snapshot = snapshot(&files);
    let mut p = maintenance(&repo(), &config, &snapshot, monday())
        .unwrap()
        .unwrap();
    p.changes.push(crate::Change {
        source: "terraform".into(),
        package: "hashicorp/aws".into(),
        file: "providers.tf".into(),
        current: "0.9.0".into(),
        current_version: None,
        value: Some("1.0.0".into()),
        version: Some("1.0.0".into()),
        digest: None,
        kind: "major".into(),
        merge: false,
        labels: Vec::new(),
        release_notes: false,
        body_notes: Vec::new(),
    });
    assert!(
        validate_targets(&p, &config, &files)
            .unwrap_err()
            .contains("did not resolve")
    );
    files.insert(
        ".terraform.lock.hcl".into(),
        "provider \"registry.terraform.io/hashicorp/aws\" {\n  version = \"1.0.0\"\n}\n".into(),
    );
    validate_targets(&p, &config, &files).unwrap();
}

struct MiseFixture {
    git: Arc<Fixture>,
    fault: &'static str,
    calls: Mutex<Vec<Vec<String>>>,
}
impl Executor for MiseFixture {
    fn run(
        &self,
        spec: CommandSpec,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, String>> + Send + '_>> {
        if spec.args.iter().any(|arg| arg == "git") {
            return self.git.run(spec);
        }
        Box::pin(async move {
            self.calls.lock().unwrap().push(spec.args.clone());
            assert_eq!(spec.env[std::ffi::OsStr::new("MISE_NO_CONFIG")], "1");
            assert_eq!(spec.env[std::ffi::OsStr::new("MISE_NO_ENV")], "1");
            assert_eq!(spec.env[std::ffi::OsStr::new("MISE_NO_HOOKS")], "1");
            assert!(
                !spec
                    .env
                    .keys()
                    .any(|key| key.to_string_lossy().contains("TOKEN"))
            );
            assert!(spec.args.iter().any(|arg| arg == "mise@2026.10.11"));
            let work =
                Path::new(&spec.args[spec.args.iter().position(|arg| arg == "--cd").unwrap() + 1]);
            let text = std::fs::read_to_string(work.join("mise.toml")).unwrap();
            assert!(!text.contains("tasks") && !text.contains("env") && !text.contains("hooks"));
            let mut lock = std::fs::read_to_string(work.join("mise.lock")).unwrap();
            if self.fault == "failure" {
                return Err("offline fixture failure".into());
            }
            if self.fault != "stale" {
                lock = lock.replace("22.0.0", "24.0.0");
            }
            if self.fault == "unrelated" {
                lock = lock.replace("1.4.3", "1.4.4");
            }
            if self.fault == "platform" {
                lock = lock
                    .lines()
                    .filter(|line| !line.contains("macos"))
                    .collect::<Vec<_>>()
                    .join("\n");
            }
            std::fs::write(work.join("mise.lock"), lock).unwrap();
            Ok(Vec::new())
        })
    }
}

#[tokio::test]
async fn mise_lock_is_targeted_scrubbed_and_committed_with_each_supported_manifest() {
    for manifest in ["mise.toml", ".mise.toml", ".tool-versions"] {
        for fault in ["", "unrelated", "platform", "stale", "failure"] {
            let old = if manifest.ends_with(".tool-versions") {
                "node 22.0.0\nbun 1.4.3\n"
            } else {
                "[tools]\nnode='22.0.0'\nbun='1.4.3'\n[env]\nUNTRUSTED='no'\n[tasks.untrusted]\nrun='false'\n"
            };
            let files = BTreeMap::from([
                (manifest.into(), old.into()),
                (
                    "mise.lock".into(),
                    include_str!("fixtures/mise.lock").into(),
                ),
            ]);
            let executor = Arc::new(MiseFixture {
                git: fixture(files.clone(), false, false),
                fault,
                calls: Mutex::new(Vec::new()),
            });
            let temp = tempfile::tempdir().unwrap();
            let runner = Runner::new(temp.path().into(), "offline-installation")
                .with_executor(executor.clone());
            let mut snapshot = snapshot(&files);
            let config = config();
            let dep = rustovate_manager::Scanner::new(&config)
                .unwrap()
                .extract(&files)
                .unwrap()
                .dependencies
                .into_iter()
                .find(|d| d.name == "node")
                .unwrap();
            let update = Update {
                new_version: Some("24.0.0".into()),
                new_value: Some("24.0.0".into()),
                ..serde_json::from_value(serde_json::json!({"updateType":"major"})).unwrap()
            };
            let job = job(&dep, &update, &snapshot).unwrap().unwrap();
            assert_eq!(job.ecosystem, Ecosystem::Mise);
            let mut p = maintenance(&repo(), &config, &snapshot, monday())
                .unwrap()
                .unwrap();
            p.maintenance = false;
            p.lock_jobs = vec![job];
            p.contents
                .insert(manifest.into(), old.replace("22.0.0", "24.0.0"));
            p.action.files.push(FileAction {
                path: manifest.into(),
                blob_sha: blob_sha(old),
                diff: String::new(),
            });
            runner.prepare(&mut p, &mut snapshot, &config).await;
            if fault.is_empty() {
                assert_eq!(p.action.status, ActionStatus::Planned, "{:?}", p.action);
                assert!(p.lockfiles_ready);
                assert_eq!(p.action.files.len(), 2);
                assert!(p.contents["mise.lock"].contains("version = \"24.0.0\""));
                assert!(p.contents["mise.lock"].contains("version = \"1.4.3\""));
            } else {
                assert_eq!(
                    p.action.status,
                    ActionStatus::Error,
                    "{manifest} {fault}: {:?}",
                    p.action
                );
                assert!(!p.lockfiles_ready);
                assert!(!p.contents.contains_key("mise.lock"));
            }
            let calls = executor.calls.lock().unwrap();
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].last().unwrap(), "node");
            assert!(calls[0].iter().any(|arg| arg == "linux-x64,macos-arm64"));
        }
    }
}

#[test]
fn mise_lock_maintenance_respects_refresh_and_unlocked_manifests_need_no_job() {
    let files = BTreeMap::from([("mise.toml".into(), "[tools]\nnode='22.0.0'\n".into())]);
    let mut snapshot = snapshot(&files);
    let mut config = config();
    let dep = rustovate_manager::Scanner::new(&config)
        .unwrap()
        .extract(&files)
        .unwrap()
        .dependencies
        .remove(0);
    let update: Update =
        serde_json::from_value(serde_json::json!({"updateType":"major","newVersion":"24.0.0"}))
            .unwrap();
    assert!(job(&dep, &update, &snapshot).unwrap().is_none());
    snapshot.paths.insert("mise.lock".into());
    config.lockfiles.refresh = false;
    assert!(
        maintenance(&repo(), &config, &snapshot, monday())
            .unwrap()
            .is_none()
    );
    assert!(
        job(&dep, &update, &snapshot).unwrap().is_some(),
        "ordinary updates must still refresh a committed lock"
    );
    config.lockfiles.refresh = true;
    let plan = maintenance(&repo(), &config, &snapshot, monday())
        .unwrap()
        .unwrap();
    assert_eq!(plan.lock_jobs.len(), 1);
    assert_eq!(plan.lock_jobs[0].ecosystem, Ecosystem::Mise);
}
