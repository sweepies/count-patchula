use super::*;
use patchula_http::{Exchange, Transport};
use patchula_process::CommandSpec;
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
            // Real tools come from the developer's store, which tests never
            // modify; the runner's own store starts empty.
            if spec.args.iter().any(|s| s == "uninstall") {
                return Ok(Vec::new());
            }
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
                // nix gets the token through its per-host access tokens alone.
                let v = if key == "nix_config" {
                    v.to_string_lossy()
                        .replace("access-tokens = github.com=offline-installation", "")
                        .into()
                } else {
                    v.clone()
                };
                assert!(
                    ![
                        "token",
                        "secret",
                        "password",
                        "authorization",
                        "github",
                        "gh_",
                        "patchula"
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
                let crates: Vec<_> = [
                    ("offline-registry-fixture", ["1.0.0", "2.0.0"]),
                    ("offline-patch-fixture", ["1.0.0", "1.0.1"]),
                ]
                .into_iter()
                .filter(|(name, _)| {
                    self.files
                        .get("Cargo.toml")
                        .is_some_and(|s| s.contains(name))
                })
                .flat_map(|(name, versions)| versions.map(|v| (name, v)))
                .collect();
                if spec.args.iter().any(|a| a == "cargo") && !crates.is_empty() {
                    let vendor = spec.cwd.join("vendor");
                    for (name, version) in crates {
                        let package = vendor.join(format!("{name}-{version}"));
                        std::fs::create_dir_all(package.join("src")).unwrap();
                        std::fs::write(
                            package.join("Cargo.toml"),
                            format!(
                                "[package]\nname='{name}'\nversion='{version}'\nedition='2024'\n"
                            ),
                        )
                        .unwrap();
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
                // Real tools come from the developer's mise store, where the
                // test task installs them, not the runner's empty tool store.
                let store = std::env::var_os("MISE_DATA_DIR")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| {
                        PathBuf::from(std::env::var_os("HOME").unwrap()).join(".local/share/mise")
                    });
                spec.env
                    .insert("MISE_DATA_DIR".into(), store.into_os_string());
                for (key, value) in [
                    ("MISE_AUTO_INSTALL", "false"),
                    ("MISE_EXEC_AUTO_INSTALL", "false"),
                    ("CARGO_NET_OFFLINE", "true"),
                    ("NPM_CONFIG_OFFLINE", "true"),
                    ("UV_OFFLINE", "1"),
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
            } else if spec.args.iter().any(|s| s == "uv") {
                "uv.lock".into()
            } else if spec.args.iter().any(|s| s == "poetry") {
                "poetry.lock".into()
            } else if spec.args.iter().any(|s| s == "cargo") {
                "Cargo.lock".into()
            } else if spec.args.iter().any(|s| s == "go") {
                "go.sum".into()
            } else if spec.args.iter().any(|s| s == "terraform") {
                ".terraform.lock.hcl".into()
            } else if spec.args.iter().any(|s| s == "apm") {
                // APM downloads packages beside the lock to hash them.
                write(&root, "apm_modules/owner/repo/SKILL.md", "skill\n").unwrap();
                "apm.lock.yaml".into()
            } else {
                "bun.lock".into()
            };
            write(&root, &leaf, "fixture deterministic lock\n").unwrap();
            Ok(Vec::new())
        })
    }
}
/// Real APM runs its lock-only command offline under the tool store's pin.
#[tokio::test]
async fn real_apm_relocks_offline() {
    let files = BTreeMap::from([
        (
            "apm.yml".to_string(),
            "name: demo\nversion: 1.0.0\ndependencies:\n  apm: []\n".to_string(),
        ),
        (
            "apm.lock.yaml".to_string(),
            "lockfile_version: '1'\n".to_string(),
        ),
    ]);
    let executor = fixture(files.clone(), false, true);
    let temp = tempfile::tempdir().unwrap();
    let runner =
        Runner::new(temp.path().into(), "offline-installation").with_executor(executor.clone());
    let mut snapshot = snapshot(&files);
    let config = config();
    let mut p = maintenance(&repo(), &config, &snapshot, None, monday())
        .unwrap()
        .unwrap();
    runner.prepare(&mut p, &mut snapshot, &config).await;
    assert!(
        matches!(
            p.action.status,
            ActionStatus::Planned | ActionStatus::Skipped
        ),
        "{:?}",
        p.action
    );
}

/// apm.lock.yaml is refreshed by APM's lock-only command, with APM from the
/// tool store: `--update` only for maintenance. The downloaded apm_modules
/// tree never enters the commit, and an edit that leaves the lock unchanged
/// fails the branch.
#[tokio::test]
async fn apm_locks_use_the_lock_only_command() {
    let manifest = "name: demo\nversion: 1.0.0\ndependencies:\n  apm:\n    - owner/repo#v1.0.0\n";
    for (lock, refresh, error) in [
        ("lockfile_version: '1'\n", true, false),
        ("lockfile_version: '1'\n", false, false),
        ("fixture deterministic lock\n", false, true),
    ] {
        let files = BTreeMap::from([
            ("apm.yml".to_string(), manifest.to_string()),
            ("apm.lock.yaml".to_string(), lock.to_string()),
        ]);
        let executor = fixture(files.clone(), false, false);
        let temp = tempfile::tempdir().unwrap();
        let runner =
            Runner::new(temp.path().into(), "offline-installation").with_executor(executor.clone());
        let mut snapshot = snapshot(&files);
        let config = config();
        let mut p = maintenance(&repo(), &config, &snapshot, None, monday())
            .unwrap()
            .unwrap();
        if !refresh {
            p.maintenance = false;
            p.contents
                .insert("apm.yml".into(), manifest.replace("v1.0.0", "v1.1.0"));
        }
        runner.prepare(&mut p, &mut snapshot, &config).await;
        let calls = executor.calls.lock().unwrap();
        let expected: &[&str] = if refresh {
            &["apm", "lock", "--update"]
        } else {
            &["apm", "lock"]
        };
        assert!(
            recorded(&calls)
                .iter()
                .any(|(specs, args)| specs.iter().any(|s| s == "apm@0.35.0") && args == expected),
            "{calls:?}"
        );
        if error {
            assert_eq!(p.action.status, ActionStatus::Error, "{:?}", p.action);
            assert!(
                p.action
                    .reason
                    .as_deref()
                    .unwrap_or("")
                    .contains("apm.lock.yaml did not change"),
                "{:?}",
                p.action
            );
            continue;
        }
        assert_eq!(p.action.status, ActionStatus::Planned, "{:?}", p.action);
        assert_eq!(p.contents["apm.lock.yaml"], "fixture deterministic lock\n");
        assert!(
            p.contents
                .keys()
                .all(|path| !path.starts_with("apm_modules/")),
            "{:?}",
            p.contents.keys()
        );
    }
}

fn config() -> Config {
    config_with("")
}
fn config_with(lockfiles: &str) -> Config {
    patchula_config::load_files(
        "/repo/patchula.pkl",
        BTreeMap::from([(
            "/repo/patchula.pkl".into(),
            format!(
                "amends \"PatchulaConfig.pkl\"\nlockfiles {{ refresh = true; tidyGo = true; {lockfiles} }}\n"
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
        status: patchula_report::Status::Complete,
        reason: None,
        warnings: Vec::new(),
        skipped: Vec::new(),
        notices: Vec::new(),
        lookup_failures: Vec::new(),
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
    let mut p = maintenance(&repo(), &config, &snapshot, None, monday())
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
    let cache = patchula_cache::Cache::open(":memory:", Default::default())
        .await
        .unwrap();
    let client = patchula_github::Client::with_transport(
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
        patchula_config::Rebase::Behind,
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
    let mut p = maintenance(&repo(), &config, &snapshot, None, monday())
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
        vec!["aube@2.8.1", "--", "aube", "-C"],
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

/// Generated by Yarn 4.18.1 for `{"name":"fixture","version":"1.0.0"}`.
const BERRY_LOCK: &str = "# This file is generated by running \"yarn install\" inside your project.\n# Manual changes might be lost - proceed with caution!\n\n__metadata:\n  version: 10\n  cacheKey: 10c0\n\n\"fixture@workspace:.\":\n  version: 0.0.0-use.local\n  resolution: \"fixture@workspace:.\"\n  languageName: unknown\n  linkType: soft\n";
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
    let mut p = maintenance(&repo(), &config, &snapshot, None, monday())
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
            .any(|(specs, command)| specs.as_slice() == ["aube@2.8.1"]
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
            .filter(|(specs, _)| specs.as_slice() == ["aube@2.8.1"])
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
    let mut p = maintenance(&repo(), &config, &snapshot, None, monday())
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
            .any(|(specs, command)| specs.as_slice() == ["aube@2.8.1"]
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
    let mut p = maintenance(&repo(), &config, &snapshot, None, monday())
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
    let mut p = maintenance(&repo(), &config, &snapshot, None, monday())
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
    let mut p = maintenance(&repo(), &config, &snapshot, None, monday())
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
    let mut p = maintenance(&repo(), &config, &snapshot, None, monday())
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
            .any(|(specs, command)| specs.as_slice() == ["aube@2.8.1"]
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
            let mut p=maintenance(&repo(),&config,&snapshot,None,monday()).unwrap().unwrap();
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

/// aube regenerates pnpm and npm locks in place from local offline fixtures,
/// including npm trees with local `file:` dependencies (fixed in aube 2.8.1,
/// aube#1796).
#[tokio::test]
async fn real_aube_rewrites_pnpm_and_npm_locks_in_place() {
    let npm_seed = "{\"name\":\"fixture\",\"version\":\"1.0.0\",\"lockfileVersion\":3,\"requires\":true,\"packages\":{\"\":{\"name\":\"fixture\",\"version\":\"1.0.0\"}}}\n";
    let local = "{\"name\":\"fixture\",\"version\":\"1.0.0\",\"dependencies\":{\"local-fixture\":\"file:./local\"}}\n";
    let plain = "{\"name\":\"fixture\",\"version\":\"1.0.0\"}\n";
    for (leaf, seed, manifest, tool, entry) in [
        (
            "pnpm-lock.yaml",
            "lockfileVersion: '9.0'\n",
            local,
            "aube@2.8.1",
            "local-fixture",
        ),
        (
            "package-lock.json",
            npm_seed,
            local,
            "aube@2.8.1",
            "local-fixture",
        ),
        (
            "package-lock.json",
            npm_seed,
            plain,
            "aube@2.8.1",
            "lockfileVersion",
        ),
    ] {
        let files = BTreeMap::from([
            ("package.json".into(), manifest.into()),
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
        let mut p = maintenance(&repo(), &config, &snapshot, None, monday())
            .unwrap()
            .unwrap();
        p.maintenance = false;
        runner.prepare(&mut p, &mut snapshot, &config).await;
        // An already-current lock is skipped; that still proves the engine choice.
        assert!(
            matches!(
                p.action.status,
                ActionStatus::Planned | ActionStatus::Skipped
            ),
            "{leaf} via {tool}: {:?}",
            p.action
        );
        let lock = p.contents.get(leaf).map_or(seed, String::as_str);
        assert!(lock.contains(entry), "{leaf} via {tool}");
        assert!(
            !p.contents.contains_key("aube-lock.yaml"),
            "{leaf}: aube must not introduce its own lockfile"
        );
        let calls = executor.calls.lock().unwrap();
        let commands = recorded(&calls);
        assert!(
            commands.iter().any(|(specs, _)| specs.as_slice() == [tool]),
            "{leaf}: expected {tool} in {commands:?}"
        );
    }
}

#[tokio::test]
async fn scheduled_refresh_uses_stable_name_windows_weekly_cadence_and_wait_hours() {
    let files = BTreeMap::from([("Cargo.lock".into(), "version = 4\n".into())]);
    let snapshot = snapshot(&files);
    let mut config = config();
    let repo = repo();
    let mut p = maintenance(&repo, &config, &snapshot, None, monday())
        .unwrap()
        .unwrap();
    assert_eq!(p.action.branch, "patchula/lock-file-maintenance");
    assert_eq!(p.action.status, ActionStatus::Planned);
    let outside = maintenance(
        &repo,
        &config,
        &snapshot,
        None,
        monday() + chrono::Duration::days(1),
    )
    .unwrap()
    .unwrap();
    assert_eq!(outside.action.status, ActionStatus::Skipped);
    let cache = patchula_cache::Cache::open(":memory:", Default::default())
        .await
        .unwrap();
    p.lockfiles_ready = true;
    p.action.status = ActionStatus::Skipped;
    p.action.reason = Some("lockfiles already current".into());
    record_refresh(&cache, &[p], &[], monday()).await.unwrap();
    for days in [0, 7] {
        config.lockfiles.wait_hours = 200;
        let now = monday() + chrono::Duration::days(days);
        let mut p = maintenance(&repo, &config, &snapshot, None, now)
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
        None,
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
        let mut p = maintenance(&repo, &config, &snapshot, None, monday())
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
        let mut p = maintenance(&repo(), &config, &snapshot, None, monday())
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
            advisories: Vec::new(),
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
    let mut p = maintenance(&repo(), &config, &snapshot, None, monday())
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
        advisories: Vec::new(),
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
            assert!(
                spec.args
                    .iter()
                    .any(|arg| arg == "aqua:jdx/mise@2026.10.11")
            );
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
            let dep = patchula_manager::Scanner::new(&config)
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
            let mut p = maintenance(&repo(), &config, &snapshot, None, monday())
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
    let dep = patchula_manager::Scanner::new(&config)
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
        maintenance(&repo(), &config, &snapshot, None, monday())
            .unwrap()
            .is_none()
    );
    assert!(
        job(&dep, &update, &snapshot).unwrap().is_some(),
        "ordinary updates must still refresh a committed lock"
    );
    config.lockfiles.refresh = true;
    let plan = maintenance(&repo(), &config, &snapshot, None, monday())
        .unwrap()
        .unwrap();
    assert_eq!(plan.lock_jobs.len(), 1);
    assert_eq!(plan.lock_jobs[0].ecosystem, Ecosystem::Mise);
}

fn npm_change(value: &str) -> crate::plan::Change {
    crate::plan::Change {
        source: "npm".into(),
        package: "fixture".into(),
        file: "package.json".into(),
        current: "1.0.0".into(),
        current_version: Some("1.0.0".into()),
        value: Some(value.into()),
        version: Some(value.into()),
        digest: None,
        kind: "major".into(),
        merge: false,
        labels: Vec::new(),
        release_notes: false,
        body_notes: Vec::new(),
        advisories: Vec::new(),
    }
}

/// A lock the runner reads back must prove the target; only pnpm and Yarn locks,
/// which it cannot read, defer to the unchanged-lockfile rule.
#[test]
fn readable_locks_must_resolve_the_requested_version() {
    let config = config();
    let seed = BTreeMap::from([("package-lock.json".to_string(), "{}\n".to_string())]);
    let mut p = maintenance(&repo(), &config, &snapshot(&seed), None, monday())
        .unwrap()
        .unwrap();
    p.changes = vec![npm_change("2.0.0")];
    let manifest = "{\"dependencies\":{\"fixture\":\"2.0.0\"}}\n";
    let lost = "{\"name\":\"t\",\"lockfileVersion\":3,\"requires\":true,\"packages\":{\"\":{\"dependencies\":{\"fixture\":\"2.0.0\"}}}}\n";
    let files = BTreeMap::from([
        ("package.json".to_string(), manifest.to_string()),
        ("package-lock.json".to_string(), lost.to_string()),
    ]);
    let error = validate_targets(&p, &config, &files).unwrap_err();
    assert!(error.contains("did not resolve fixture"), "{error}");
    for lock in ["pnpm-lock.yaml", "yarn.lock"] {
        let files = BTreeMap::from([
            ("package.json".to_string(), manifest.to_string()),
            (lock.to_string(), "unread\n".to_string()),
        ]);
        validate_targets(&p, &config, &files).unwrap_or_else(|e| panic!("{lock}: {e}"));
    }
}

/// aube cannot read npm v1 locks; every newer lock stays with aube, including
/// trees with local `file:` dependencies, which aube 2.8.1 keeps valid
/// (aube#1796).
#[test]
fn npm_locks_aube_cannot_write_stay_on_native_npm() {
    let case = |files: &[(&str, &str)]| {
        let temp = tempfile::tempdir().unwrap();
        for (path, content) in files {
            write(temp.path(), path, content).unwrap();
        }
        node::aube_cannot_write_npm(temp.path(), "package-lock.json").unwrap()
    };
    let v3 = "{\"lockfileVersion\":3}\n";
    assert!(!case(&[
        ("package.json", "{\"dependencies\":{\"a\":\"1.0.0\"}}"),
        ("package-lock.json", v3),
    ]));
    assert!(case(&[
        ("package.json", "{}"),
        ("package-lock.json", "{\"lockfileVersion\":1}\n"),
    ]));
    assert!(!case(&[
        ("package.json", "{\"dependencies\":{\"a\":\"file:./a\"}}"),
        ("package-lock.json", v3),
    ]));
}

/// The runner's tool pins match the task-scoped pins the real-tool tests use.
#[test]
fn lockfile_engine_pins_match_the_repository_mise_config() {
    let repo: toml_edit::Document<String> = include_str!("../../../../mise.toml").parse().unwrap();
    let defaults = tool_defaults().unwrap();
    for task in ["test", "test:stress"] {
        let tools = &repo["tasks"][task]["tools"];
        assert_eq!(
            tools["aube"].as_str(),
            defaults["tools"]["aube"].as_str(),
            "{task}"
        );
        assert_eq!(
            tools["yarn"].as_str(),
            defaults["yarn"]["berry"].as_str(),
            "{task}"
        );
        for tool in ["python", "uv", "poetry", "dotnet", "ruby", "apm"] {
            assert_eq!(
                tools[tool].as_str(),
                defaults["tools"][tool].as_str(),
                "{task} {tool}"
            );
        }
        let php = php::tools(
            patchula_config::PhpBuild::Static,
            defaults["tools"]["php"].as_str().unwrap(),
            defaults["tools"]["composer"].as_str().unwrap(),
        );
        for (tool, version) in php {
            assert_eq!(
                tools[&tool].as_str(),
                Some(version.as_str()),
                "{task} {tool}"
            );
        }
    }
}

/// Yarn never runs a committed release, plugins or credential interpolations.
#[test]
fn yarnrc_keeps_settings_but_drops_repository_code_and_credentials() {
    let text = "nodeLinker: node-modules\nyarnPath: .yarn/releases/yarn-4.5.0.cjs\nplugins:\n  - path: .yarn/plugins/x.cjs\nnpmScopes:\n  acme:\n    npmRegistryServer: https://npm.acme.test\n    npmAuthToken: \"${NPM_TOKEN}\"\ncatalog:\n  zod: 3.23.8\n";
    let clean: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(&node::yarnrc(text).unwrap()).unwrap();
    assert_eq!(clean["nodeLinker"].as_str(), Some("node-modules"));
    assert_eq!(clean["catalog"]["zod"].as_str(), Some("3.23.8"));
    assert_eq!(
        clean["npmScopes"]["acme"]["npmRegistryServer"].as_str(),
        Some("https://npm.acme.test")
    );
    assert!(clean.get("yarnPath").is_none() && clean.get("plugins").is_none());
    assert!(clean["npmScopes"]["acme"].get("npmAuthToken").is_none());
}

/// A catalog edit that leaves the lock untouched is a stale lock.
#[tokio::test]
async fn an_unchanged_lockfile_after_a_catalog_edit_fails_the_branch() {
    let files = BTreeMap::from([
        (
            "package.json".into(),
            "{\"dependencies\":{\"fixture\":\"catalog:\"}}\n".into(),
        ),
        (
            "pnpm-workspace.yaml".into(),
            "catalog:\n  fixture: 1.0.0\n".into(),
        ),
        (
            "pnpm-lock.yaml".into(),
            "fixture deterministic lock\n".into(),
        ),
    ]);
    let executor = fixture(files.clone(), false, false);
    let temp = tempfile::tempdir().unwrap();
    let runner =
        Runner::new(temp.path().into(), "offline-installation").with_executor(executor.clone());
    let mut snapshot = snapshot(&files);
    let config = config();
    let mut p = maintenance(&repo(), &config, &snapshot, None, monday())
        .unwrap()
        .unwrap();
    p.maintenance = false;
    p.contents.insert(
        "pnpm-workspace.yaml".into(),
        "catalog:\n  fixture: 2.0.0\n".into(),
    );
    runner.prepare(&mut p, &mut snapshot, &config).await;
    assert_eq!(p.action.status, ActionStatus::Error, "{:?}", p.action);
    assert!(
        p.action
            .reason
            .as_deref()
            .is_some_and(|r| r.contains("did not change after the manifest edit")),
        "{:?}",
        p.action.reason
    );
}

/// Real Yarn Berry reads catalogs from a sanitised `.yarnrc.yml` whose committed
/// release, plugins and credential references are absent, and the committed
/// file is restored.
#[tokio::test]
async fn real_yarn_berry_uses_a_sanitised_yarnrc_and_restores_it() {
    let yarnrc = "nodeLinker: node-modules\nyarnPath: .yarn/releases/yarn-4.5.0.cjs\nplugins:\n  - path: .yarn/plugins/missing.cjs\n    spec: missing\nnpmAuthToken: \"${NPM_TOKEN}\"\ncatalog:\n  local-fixture: \"file:./local\"\n";
    let files = BTreeMap::from([
        (
            "package.json".into(),
            "{\"name\":\"fixture\",\"version\":\"1.0.0\",\"packageManager\":\"yarn@4.18.1\",\"dependencies\":{\"local-fixture\":\"catalog:\"}}\n".into(),
        ),
        ("local/package.json".into(), "{\"name\":\"local-fixture\",\"version\":\"1.0.0\"}\n".into()),
        (".yarnrc.yml".into(), yarnrc.into()),
        ("yarn.lock".into(), BERRY_LOCK.into()),
    ]);
    let executor = fixture(files.clone(), false, true);
    let temp = tempfile::tempdir().unwrap();
    let runner =
        Runner::new(temp.path().into(), "offline-installation").with_executor(executor.clone());
    let mut snapshot = snapshot(&files);
    let config = config();
    let mut p = maintenance(&repo(), &config, &snapshot, None, monday())
        .unwrap()
        .unwrap();
    p.maintenance = false;
    runner.prepare(&mut p, &mut snapshot, &config).await;
    assert_eq!(p.action.status, ActionStatus::Planned, "{:?}", p.action);
    assert!(
        p.contents["yarn.lock"].contains("local-fixture"),
        "{}",
        p.contents["yarn.lock"]
    );
    assert!(
        !p.contents.contains_key(".yarnrc.yml"),
        "the committed .yarnrc.yml must be restored"
    );
}

/// uv and Poetry run targeted against the pinned or `.python-version` interpreter.
#[tokio::test]
async fn python_locks_use_uv_and_poetry_with_the_project_interpreter() {
    for (lock, tool, command, python) in [
        (
            "uv.lock",
            "uv@0.13.1",
            &["uv", "lock", "--directory"][..],
            "python@3.12.4",
        ),
        (
            "poetry.lock",
            "poetry@2.5.1",
            &[
                "poetry",
                "lock",
                "--no-interaction",
                "--no-plugins",
                "--directory",
            ][..],
            "python@3.14.8",
        ),
    ] {
        let mut files = BTreeMap::from([
            (
                "pyproject.toml".to_string(),
                "[project]\ndependencies = [\"ruff==0.6.0\"]\n".to_string(),
            ),
            (lock.to_string(), "seed\n".to_string()),
        ]);
        if lock == "uv.lock" {
            files.insert(".python-version".into(), "3.12.4\n".into());
        }
        let executor = fixture(files.clone(), false, false);
        let temp = tempfile::tempdir().unwrap();
        let runner =
            Runner::new(temp.path().into(), "offline-installation").with_executor(executor.clone());
        let mut snapshot = snapshot(&files);
        let config = config();
        let mut p = maintenance(&repo(), &config, &snapshot, None, monday())
            .unwrap()
            .unwrap();
        p.maintenance = false;
        p.lock_jobs = vec![Job {
            ecosystem: Ecosystem::Python,
            directory: String::new(),
            package: Some("ruff".into()),
            current: Some("0.6.0".into()),
            target: Some("0.6.9".into()),
        }];
        p.contents.insert(
            "pyproject.toml".into(),
            "[project]\ndependencies = [\"ruff==0.6.9\"]\n".into(),
        );
        runner.prepare(&mut p, &mut snapshot, &config).await;
        let calls = executor.calls.lock().unwrap();
        let commands = recorded(&calls);
        let (specs, args) = commands
            .iter()
            .find(|(specs, _)| specs.iter().any(|s| s == tool))
            .unwrap_or_else(|| panic!("{lock}: {commands:?}"));
        assert!(specs.iter().any(|s| s == python), "{lock}: {specs:?}");
        assert!(starts(args, command), "{lock}: {args:?}");
        if lock == "uv.lock" {
            assert!(contains(args, &["--upgrade-package", "ruff"]), "{args:?}");
        }
    }
}

/// A targeted Python upgrade that leaves the lock untouched is a stale lock.
#[tokio::test]
async fn an_unchanged_python_lock_after_a_targeted_upgrade_fails() {
    let files = BTreeMap::from([
        (
            "pyproject.toml".to_string(),
            "[project]\ndependencies = [\"ruff>=0.6\"]\n".to_string(),
        ),
        (
            "uv.lock".to_string(),
            "fixture deterministic lock\n".to_string(),
        ),
    ]);
    let executor = fixture(files.clone(), false, false);
    let temp = tempfile::tempdir().unwrap();
    let runner =
        Runner::new(temp.path().into(), "offline-installation").with_executor(executor.clone());
    let mut snapshot = snapshot(&files);
    let config = config();
    let mut p = maintenance(&repo(), &config, &snapshot, None, monday())
        .unwrap()
        .unwrap();
    p.maintenance = false;
    p.lock_jobs = vec![Job {
        ecosystem: Ecosystem::Python,
        directory: String::new(),
        package: Some("ruff".into()),
        current: Some("0.6.0".into()),
        target: Some("0.6.9".into()),
    }];
    runner.prepare(&mut p, &mut snapshot, &config).await;
    assert_eq!(p.action.status, ActionStatus::Error, "{:?}", p.action);
    assert!(
        p.action
            .reason
            .as_deref()
            .is_some_and(|r| r.contains("uv.lock did not change")),
        "{:?}",
        p.action.reason
    );
}

/// Real uv and Poetry regenerate stale locks offline for a dependency-free
/// project, against the runner's mise Python.
#[tokio::test]
async fn real_uv_and_poetry_regenerate_python_locks() {
    let project = "[project]\nname = \"fixture\"\nversion = \"1.0.0\"\nrequires-python = \">=3.12\"\ndependencies = []\n";
    for (lock, manifest, seed, tool, expected) in [
        (
            "uv.lock",
            project.to_string(),
            "version = 1\nrequires-python = \">=3.9\"\n",
            "uv@0.13.1",
            "name = \"fixture\"",
        ),
        (
            "poetry.lock",
            format!("{project}[tool.poetry]\npackage-mode = false\n"),
            "[metadata]\nlock-version = \"2.1\"\npython-versions = \">=3.9\"\ncontent-hash = \"0\"\n",
            "poetry@2.5.1",
            "python-versions = \">=3.12\"",
        ),
    ] {
        let files = BTreeMap::from([
            ("pyproject.toml".to_string(), manifest),
            (lock.to_string(), seed.to_string()),
            (".python-version".to_string(), "3.14.8\n".to_string()),
        ]);
        let executor = fixture(files.clone(), false, true);
        let temp = tempfile::tempdir().unwrap();
        let runner =
            Runner::new(temp.path().into(), "offline-installation").with_executor(executor.clone());
        let mut snapshot = snapshot(&files);
        let config = config();
        let mut p = maintenance(&repo(), &config, &snapshot, None, monday())
            .unwrap()
            .unwrap();
        runner.prepare(&mut p, &mut snapshot, &config).await;
        assert_eq!(
            p.action.status,
            ActionStatus::Planned,
            "{lock}: {:?}",
            p.action
        );
        assert!(
            p.contents[lock].contains(expected),
            "{lock}: {}",
            p.contents[lock]
        );
        let calls = executor.calls.lock().unwrap();
        assert!(
            recorded(&calls)
                .iter()
                .any(|(specs, _)| specs.iter().any(|s| s == tool)
                    && specs.iter().any(|s| s == "python@3.14.8")),
            "{lock}"
        );
    }
}

/// An update that raises the go or toolchain directive runs that Go.
#[test]
fn go_runs_at_the_version_the_edited_module_requires() {
    let temp = tempfile::tempdir().unwrap();
    for (module, expected) in [
        ("module m\n\ngo 1.22\n", "1.27.2"),
        ("module m\n\ngo 1.27.3\n", "1.27.3"),
        ("module m\n\ngo 1.27.3\n\ntoolchain go1.28.1\n", "1.28.1"),
        ("module m\n\ngo 1.29rc1\n", "1.27.2"),
    ] {
        std::fs::write(temp.path().join("go.mod"), module).unwrap();
        assert_eq!(
            go_version(temp.path(), "1.27.2").unwrap(),
            expected,
            "{module}"
        );
    }
}

/// A chart update beside a committed Chart.lock is held, not published stale.
#[test]
fn chart_updates_with_a_committed_chart_lock_are_pending() {
    let mut config = config();
    config.managers = vec![Manager::Helm];
    let chart = "dependencies:\n  - name: redis\n    version: 1.0.0\n    repository: https://charts.example.test\n";
    let deps = patchula_manager::Scanner::new(&config)
        .unwrap()
        .extract(&patchula_manager::Files::from([(
            "Chart.yaml".into(),
            chart.into(),
        )]))
        .unwrap()
        .dependencies;
    let update = patchula_report::Update {
        new_value: Some("2.0.0".into()),
        new_version: Some("2.0.0".into()),
        new_digest: None,
        update_type: "major".into(),
        group: None,
        policy: None,
        release_age: None,
        fixes: Vec::new(),
    };
    let unlocked = snapshot(&BTreeMap::from([("Chart.yaml".into(), chart.into())]));
    assert!(matches!(job(&deps[0], &update, &unlocked), Ok(None)));
    let locked = snapshot(&BTreeMap::from([
        ("Chart.yaml".into(), chart.into()),
        ("Chart.lock".into(), "dependencies: []\n".into()),
    ]));
    assert!(
        job(&deps[0], &update, &locked)
            .unwrap_err()
            .contains("Chart.lock")
    );
}

struct Checksums {
    requests: Mutex<Vec<String>>,
}
impl Transport for Checksums {
    fn send(&self, request: reqwest::Request) -> Exchange<'_> {
        Box::pin(async move {
            let url = request.url().to_string();
            self.requests.lock().unwrap().push(url.clone());
            let (status, headers, body) = if url.starts_with("https://services.gradle.org/") {
                let mut headers = reqwest::header::HeaderMap::new();
                headers.insert(
                    "location",
                    url.replace("services.gradle.org", "downloads.gradle.org")
                        .parse()
                        .unwrap(),
                );
                (301, headers, Vec::new())
            } else {
                (
                    200,
                    Default::default(),
                    format!("{}\n", "a".repeat(64)).into_bytes(),
                )
            };
            Ok(patchula_http::Response {
                status: reqwest::StatusCode::from_u16(status).unwrap(),
                headers,
                body,
            })
        })
    }
}

/// The wrapper checksum follows the edited distribution, from Gradle's own
/// hosts only; a wrapper without a checksum needs no request.
#[tokio::test]
async fn gradle_wrapper_checksums_follow_the_distribution() {
    let old = "distributionBase=GRADLE_USER_HOME\r\ndistributionSha256Sum=0000\r\ndistributionUrl=https\\://services.gradle.org/distributions/gradle-8.10-bin.zip\r\n";
    let new = old.replace("gradle-8.10-bin", "gradle-9.0-bin");
    let transport = Arc::new(Checksums {
        requests: Mutex::new(Vec::new()),
    });
    let temp = tempfile::tempdir().unwrap();
    let runner =
        Runner::new(temp.path().into(), "offline-installation").with_http(transport.clone());
    let root = temp.path().join("tree");
    write(&root, "gradle/wrapper/gradle-wrapper.properties", &new).unwrap();
    runner
        .gradle_wrapper(&root, "gradle/wrapper")
        .await
        .unwrap();
    let written =
        std::fs::read_to_string(root.join("gradle/wrapper/gradle-wrapper.properties")).unwrap();
    assert_eq!(
        written,
        new.replace("=0000", &format!("={}", "a".repeat(64)))
    );
    assert_eq!(
        *transport.requests.lock().unwrap(),
        [
            "https://services.gradle.org/distributions/gradle-9.0-bin.zip.sha256",
            "https://downloads.gradle.org/distributions/gradle-9.0-bin.zip.sha256",
        ]
    );
    write(
        &root,
        "gradle/wrapper/gradle-wrapper.properties",
        &new.replace("services.gradle.org", "mirror.example.test"),
    )
    .unwrap();
    assert!(
        runner
            .gradle_wrapper(&root, "gradle/wrapper")
            .await
            .unwrap_err()
            .contains("refusing")
    );
    transport.requests.lock().unwrap().clear();
    write(
        &root,
        "gradle/wrapper/gradle-wrapper.properties",
        "distributionUrl=https\\://services.gradle.org/distributions/gradle-9.0-bin.zip\n",
    )
    .unwrap();
    runner
        .gradle_wrapper(&root, "gradle/wrapper")
        .await
        .unwrap();
    assert!(transport.requests.lock().unwrap().is_empty());
}

struct Recorder(Mutex<Vec<Vec<String>>>);
impl Executor for Recorder {
    fn run(
        &self,
        spec: CommandSpec,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, String>> + Send + '_>> {
        Box::pin(async move {
            assert!(spec.env.contains_key(std::ffi::OsStr::new("MISE_DATA_DIR")));
            self.0.lock().unwrap().push(spec.args.clone());
            Ok(Vec::new())
        })
    }
}

/// Unused tool versions are pruned from the runner's own store; pinned
/// defaults and recently used versions stay, and prewarming installs defaults.
#[tokio::test]
async fn tool_store_prunes_unused_versions_and_prewarms_defaults() {
    let temp = tempfile::tempdir().unwrap();
    let recorder = Arc::new(Recorder(Mutex::new(Vec::new())));
    let runner =
        Runner::new(temp.path().join("cache.worktrees"), "x").with_executor(recorder.clone());
    assert_eq!(runner.tools(), temp.path().join("cache.tools"));
    runner.record_use(&[
        ("node".into(), "20.0.0".into()),
        ("uv".into(), "0.13.1".into()),
    ]);
    runner.record_use(&[("bun".into(), "1.3.0".into())]);
    // Age the node and uv records past the window.
    let path = runner.tools().join("patchula-usage.json");
    let mut usage: BTreeMap<String, i64> =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    for spec in ["node@20.0.0", "uv@0.13.1"] {
        usage.insert(spec.into(), 0);
    }
    std::fs::write(&path, serde_json::to_vec(&usage).unwrap()).unwrap();
    let pruned = runner.prune(30).await.unwrap();
    assert_eq!(pruned, ["node@20.0.0"], "uv 0.13.1 is a pinned default");
    let calls = recorder.0.lock().unwrap().clone();
    assert!(
        calls
            .iter()
            .any(|a| a.ends_with(&["uninstall".into(), "node@20.0.0".into()])),
        "{calls:?}"
    );
    let remaining: BTreeMap<String, i64> =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(remaining.contains_key("bun@1.3.0") && !remaining.contains_key("node@20.0.0"));
    let installed = runner.prewarm().await.unwrap();
    assert!(
        installed.contains(&"uv@0.13.1".to_string())
            && installed.contains(&"yarn@4.18.1".to_string())
    );
    assert!(!installed.iter().any(|s| s.starts_with("rust@")));
    assert!(installed.contains(&"aqua:jdx/mise@2026.10.11".to_string()));
}

/// A version the user already has is hard-linked into the store instead of
/// downloaded, once; tools whose installs embed their path are left to mise.
#[cfg(unix)]
#[test]
fn tool_store_seeds_relocatable_tools_from_the_users_mise_once() {
    use std::os::unix::fs::MetadataExt;
    let temp = tempfile::tempdir().unwrap();
    let user = temp.path().join("user-mise");
    for (tool, version) in [("node", "24.21.0"), ("python", "3.14.8")] {
        let dir = user.join("installs").join(tool).join(version).join("bin");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(tool), "binary").unwrap();
        std::os::unix::fs::symlink(tool, dir.join(format!("{tool}-alias"))).unwrap();
    }
    std::fs::write(user.join("installs/node/.mise.backend.toml"), "backend").unwrap();
    // mise's own bin links are absolute, into the version directory.
    let mise_bins = user.join("installs/node/24.21.0/.mise-bins");
    std::fs::create_dir_all(&mise_bins).unwrap();
    std::os::unix::fs::symlink(
        user.join("installs/node/24.21.0/bin/node")
            .canonicalize()
            .unwrap(),
        mise_bins.join("node"),
    )
    .unwrap();
    // uv's version points into a root shared with other versions.
    let shared = user.join("uv-root/bin");
    std::fs::create_dir_all(&shared).unwrap();
    std::fs::write(shared.join("uv"), "binary").unwrap();
    std::fs::create_dir_all(user.join("installs/uv")).unwrap();
    std::os::unix::fs::symlink(user.join("uv-root"), user.join("installs/uv/0.13.1")).unwrap();
    // bun's binary links outside its version directory, so it cannot move.
    let bun = user.join("installs/bun/1.4.3/bin");
    std::fs::create_dir_all(&bun).unwrap();
    std::fs::create_dir_all(user.join("installs/bun/shared")).unwrap();
    std::fs::write(user.join("installs/bun/shared/bun"), "binary").unwrap();
    std::os::unix::fs::symlink("../../shared/bun", bun.join("bun")).unwrap();
    // Another run already published go; this run leaves it as it was.
    let racing = temp.path().join("cache.tools/installs/go/1.27.2");
    std::fs::create_dir_all(user.join("installs/go/1.27.2/bin")).unwrap();
    std::fs::write(user.join("installs/go/1.27.2/bin/go"), "binary").unwrap();
    std::fs::create_dir_all(&racing).unwrap();
    std::fs::write(racing.join("other-run"), "complete").unwrap();
    let runner = Runner::new(temp.path().join("cache.worktrees"), "x").with_seed(user.clone());
    let tools = [
        ("node".to_string(), "24.21.0".to_string()),
        ("python".to_string(), "3.14.8".to_string()),
        ("go".to_string(), "1.27.2".to_string()),
        ("uv".to_string(), "0.13.1".to_string()),
        ("bun".to_string(), "1.4.3".to_string()),
    ];
    runner.seed(&tools);
    let store = runner.tools().join("installs");
    let linked = store.join("node/24.21.0/bin/node");
    assert_eq!(
        std::fs::metadata(&linked).unwrap().ino(),
        std::fs::metadata(user.join("installs/node/24.21.0/bin/node"))
            .unwrap()
            .ino(),
        "a hard link, not a copy"
    );
    assert_eq!(
        std::fs::read_link(store.join("node/24.21.0/bin/node-alias")).unwrap(),
        std::path::Path::new("node")
    );
    assert!(store.join("node/.mise.backend.toml").is_file());
    assert_eq!(
        std::fs::read_link(store.join("node/24.21.0/.mise-bins/node")).unwrap(),
        store.join("node/24.21.0/bin/node"),
        "absolute links point into the copy, not the user's install"
    );
    assert!(!store.join("python").exists(), "python is not relocatable");
    assert!(
        store.join("go/1.27.2/other-run").is_file() && !store.join("go/1.27.2/bin").exists(),
        "another run's install is left as it was"
    );
    assert!(
        !store.join("bun/1.4.3").exists(),
        "a link out of the version is not copied"
    );
    assert!(
        std::fs::read_dir(store.join("go")).unwrap().all(|e| !e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".patchula-seed")),
        "staging copies are cleaned up"
    );
    assert!(!store.join("uv").exists(), "a shared root is not copied");
    // Removing the seeded copy, as clearing an unproven install does, leaves
    // the user's install alone and is not undone by seeding again.
    std::fs::remove_dir_all(store.join("node/24.21.0")).unwrap();
    assert!(user.join("installs/node/24.21.0/bin/node").is_file());
    runner.seed(&tools);
    assert!(!store.join("node/24.21.0").exists());
}

/// Every prewarmed spec is one real mise can install: a dry run resolves
/// names and backends offline without downloading.
#[test]
fn real_mise_accepts_every_prewarm_spec() {
    let store = tempfile::tempdir().unwrap();
    let specs: Vec<String> = tools::defaults()
        .unwrap()
        .iter()
        .map(|(t, v)| format!("{t}@{v}"))
        .collect();
    let output = std::process::Command::new("mise")
        .args(["--no-config", "install", "--dry-run"])
        .args(&specs)
        .env("MISE_DATA_DIR", store.path())
        .env("MISE_OFFLINE", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Real dotnet restores a project's packages.lock.json offline and leaves no
/// obj/ intermediates behind.
#[tokio::test]
async fn real_dotnet_restore_regenerates_packages_lock_json() {
    let project = "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    <TargetFramework>net9.0</TargetFramework>\n    <RestorePackagesWithLockFile>true</RestorePackagesWithLockFile>\n  </PropertyGroup>\n</Project>\n";
    let files = BTreeMap::from([
        ("app/App.csproj".to_string(), project.to_string()),
        (
            "app/packages.lock.json".to_string(),
            "{\"version\": 1, \"dependencies\": {\"net8.0\": {}}}\n".to_string(),
        ),
    ]);
    let executor = fixture(files.clone(), false, true);
    let temp = tempfile::tempdir().unwrap();
    let runner =
        Runner::new(temp.path().into(), "offline-installation").with_executor(executor.clone());
    let mut snapshot = snapshot(&files);
    let config = config();
    let mut p = maintenance(&repo(), &config, &snapshot, None, monday())
        .unwrap()
        .unwrap();
    p.lock_jobs = vec![Job {
        ecosystem: Ecosystem::Dotnet,
        directory: "app".into(),
        package: None,
        current: None,
        target: None,
    }];
    runner.prepare(&mut p, &mut snapshot, &config).await;
    assert_eq!(p.action.status, ActionStatus::Planned, "{:?}", p.action);
    assert!(
        p.contents["app/packages.lock.json"].contains("net9.0"),
        "{}",
        p.contents["app/packages.lock.json"]
    );
    assert!(
        !p.contents.keys().any(|k| k.contains("/obj/")),
        "{:?}",
        p.contents.keys()
    );
}

/// Real Bundler relocks offline against the runner's mise Ruby, ignoring a
/// repository .bundle/config.
#[tokio::test]
async fn real_bundler_relocks_offline() {
    let files = BTreeMap::from([
        ("Gemfile".to_string(), "source \"https://rubygems.org\"\n".to_string()),
        ("Gemfile.lock".to_string(), "GEM\n  remote: https://rubygems.org/\n  specs:\n\nPLATFORMS\n  ruby\n\nDEPENDENCIES\n\nBUNDLED WITH\n   2.6.9\n".to_string()),
        (".ruby-version".to_string(), "3.4.11\n".to_string()),
    ]);
    let executor = fixture(files.clone(), false, true);
    let temp = tempfile::tempdir().unwrap();
    let runner =
        Runner::new(temp.path().into(), "offline-installation").with_executor(executor.clone());
    let mut snapshot = snapshot(&files);
    let config = config();
    let mut p = maintenance(&repo(), &config, &snapshot, None, monday())
        .unwrap()
        .unwrap();
    runner.prepare(&mut p, &mut snapshot, &config).await;
    assert!(
        matches!(
            p.action.status,
            ActionStatus::Planned | ActionStatus::Skipped
        ),
        "{:?}",
        p.action
    );
    let calls = executor.calls.lock().unwrap();
    assert!(
        recorded(&calls)
            .iter()
            .any(|(specs, args)| specs.iter().any(|s| s == "ruby@3.4.11")
                && starts(args, &["bundle", "lock", "--update"]))
    );
}

/// composer.lock runs Composer under static PHP by default, or mise's
/// compiled PHP when configured, ignoring extension requirements only.
#[tokio::test]
async fn composer_locks_use_static_or_compiled_php() {
    let files = BTreeMap::from([
        (
            "composer.json".to_string(),
            "{\"require\": {}}\n".to_string(),
        ),
        (
            "composer.lock".to_string(),
            "{\"packages\": []}\n".to_string(),
        ),
    ]);
    for (setting, php) in [
        (
            "",
            php::tools(patchula_config::PhpBuild::Static, "8.5.8", "2.10.3")[0]
                .0
                .clone(),
        ),
        ("php = \"compiled\"", "php".to_string()),
    ] {
        let executor = fixture(files.clone(), false, false);
        let temp = tempfile::tempdir().unwrap();
        let runner =
            Runner::new(temp.path().into(), "offline-installation").with_executor(executor.clone());
        let mut snapshot = snapshot(&files);
        let config = config_with(setting);
        let mut p = maintenance(&repo(), &config, &snapshot, None, monday())
            .unwrap()
            .unwrap();
        runner.prepare(&mut p, &mut snapshot, &config).await;
        let calls = executor.calls.lock().unwrap();
        assert!(
            recorded(&calls)
                .iter()
                .any(|(specs, args)| specs.contains(&format!("{php}@8.5.8"))
                    && starts(args, &["composer", "update", "--no-install"])
                    && contains(
                        args,
                        &["--ignore-platform-req=ext-*", "--ignore-platform-req=lib-*"]
                    )),
            "{setting}: {calls:?}"
        );
    }
    assert!(
        php::tools(patchula_config::PhpBuild::Static, "8.5.8", "2.10.3")[0]
            .0
            .starts_with("http:php[url=https://dl.static-php.dev/")
    );
}

/// Static PHP downloads exact releases, so a partial repository pin fails
/// clearly instead of resolving against another PHP.
#[tokio::test]
async fn static_php_requires_an_exact_version() {
    let files = BTreeMap::from([
        (
            "composer.json".to_string(),
            "{\"require\": {}}\n".to_string(),
        ),
        (
            "composer.lock".to_string(),
            "{\"packages\": []}\n".to_string(),
        ),
        (
            "mise.toml".to_string(),
            "[tools]\nphp = \"8.4\"\n".to_string(),
        ),
    ]);
    let executor = fixture(files.clone(), false, false);
    let temp = tempfile::tempdir().unwrap();
    let runner =
        Runner::new(temp.path().into(), "offline-installation").with_executor(executor.clone());
    let mut snapshot = snapshot(&files);
    let config = config();
    let mut p = maintenance(&repo(), &config, &snapshot, None, monday())
        .unwrap()
        .unwrap();
    runner.prepare(&mut p, &mut snapshot, &config).await;
    assert_eq!(p.action.status, ActionStatus::Error, "{:?}", p.action);
    assert!(
        p.action
            .reason
            .as_deref()
            .unwrap_or("")
            .contains("exact version"),
        "{:?}",
        p.action
    );
}

/// Real Composer relocks offline under the static PHP from the tool store.
#[tokio::test]
async fn real_composer_relocks_offline() {
    let manifest = "{\n    \"name\": \"demo/app\",\n    \"repositories\": [{\"packagist.org\": false}],\n    \"require\": {}\n}\n";
    let files = BTreeMap::from([
        ("composer.json".to_string(), manifest.to_string()),
        (
            "composer.lock".to_string(),
            "{\"packages\": [], \"packages-dev\": []}\n".to_string(),
        ),
    ]);
    let executor = fixture(files.clone(), false, true);
    let temp = tempfile::tempdir().unwrap();
    let runner =
        Runner::new(temp.path().into(), "offline-installation").with_executor(executor.clone());
    let mut snapshot = snapshot(&files);
    let config = config();
    let mut p = maintenance(&repo(), &config, &snapshot, None, monday())
        .unwrap()
        .unwrap();
    runner.prepare(&mut p, &mut snapshot, &config).await;
    assert_eq!(p.action.status, ActionStatus::Planned, "{:?}", p.action);
    assert!(
        p.contents["composer.lock"].contains("content-hash"),
        "{}",
        p.contents["composer.lock"]
    );
}

struct Timeouts(Mutex<Vec<std::time::Duration>>);
impl Executor for Timeouts {
    fn run(
        &self,
        spec: CommandSpec,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, String>> + Send + '_>> {
        Box::pin(async move {
            if spec.args.iter().any(|a| a == "exec") {
                self.0.lock().unwrap().push(spec.timeout);
            } else {
                assert!(
                    spec.args.iter().any(|a| a == "uninstall"),
                    "{:?}",
                    spec.args
                );
            }
            Ok(Vec::new())
        })
    }
}

/// A tool version the store has not run yet is reinstalled from scratch within
/// the call, so it gets the install allowance; known versions keep the normal
/// limit.
#[tokio::test]
async fn first_tool_use_allows_for_the_install() {
    let temp = tempfile::tempdir().unwrap();
    let timeouts = Arc::new(Timeouts(Mutex::new(Vec::new())));
    let runner = Runner::new(temp.path().join("cache"), "x").with_executor(timeouts.clone());
    let driver = temp.path().join("driver");
    std::fs::create_dir_all(&driver).unwrap();
    let call = || Invocation {
        tools: vec![("uv".into(), "0.13.1".into())],
        bin: "uv".into(),
        cwd: None,
        env: Vec::new(),
        args: vec!["lock".into()],
    };
    runner.tool(&driver, call()).await.unwrap();
    runner.tool(&driver, call()).await.unwrap();
    let normal = runner.command(&driver).timeout;
    assert_eq!(
        *timeouts.0.lock().unwrap(),
        [tools::INSTALL_TIMEOUT, normal]
    );
}

#[test]
fn a_vulnerable_transitive_crate_moves_through_its_lockfile_alone() {
    let lock = |version: &str| {
        format!(
            "version = 4\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\n\n[[package]]\nname = \"time\"\nversion = \"{version}\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n"
        )
    };
    let mut files = BTreeMap::from([
        (
            "Cargo.toml".into(),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n".into(),
        ),
        ("Cargo.lock".into(), lock("0.3.20")),
    ]);
    let config = config();
    let extraction = patchula_manager::Scanner::new(&config)
        .unwrap()
        .extract(&files)
        .unwrap();
    assert!(extraction.dependencies.is_empty());
    let dependencies: Vec<_> = extraction.locked.iter().map(|l| l.dependency()).collect();
    assert_eq!(dependencies.len(), 1);
    let mut repository = repo();
    repository.package_files = serde_json::from_value(serde_json::json!([{
        "path": "Cargo.lock",
        "dependencies": [{
            "manager": "cargo", "name": "time", "package": "time",
            "current": {"value": "0.3.20", "version": "0.3.20", "digest": null},
            "updates": [{"newValue": null, "newVersion": "0.3.23", "newDigest": null,
                "updateType": "patch", "group": null, "fixes": ["RUSTSEC-2023-0071"]}],
            "metadata": {"source": "crates", "kinds": ["transitive"], "locations": [],
                "advisories": [{"id": "RUSTSEC-2023-0071", "url": "https://osv.dev/vulnerability/RUSTSEC-2023-0071"}]}
        }]
    }]))
    .unwrap();
    let plans = crate::plan(
        &repository,
        &config,
        &dependencies,
        &snapshot(&files),
        false,
        None,
        monday(),
    )
    .unwrap();
    assert_eq!(plans.len(), 1);
    let plan = &plans[0];
    assert_eq!(
        plan.action.status,
        ActionStatus::Planned,
        "{:?}",
        plan.action
    );
    assert!(plan.contents.is_empty());
    assert!(
        plan.action
            .message
            .ends_with("update time to 0.3.23 [security]")
    );
    assert_eq!(
        plan.lock_jobs,
        [Job {
            ecosystem: Ecosystem::Cargo,
            directory: String::new(),
            package: Some("time".into()),
            current: Some("0.3.20".into()),
            target: Some("0.3.23".into()),
        }]
    );
    assert_eq!(plan.changes[0].advisories[0].id, "RUSTSEC-2023-0071");
    assert!(
        validate_targets(plan, &config, &files)
            .unwrap_err()
            .contains("did not resolve time")
    );
    files.insert("Cargo.lock".into(), lock("0.3.23"));
    validate_targets(plan, &config, &files).unwrap();
}

#[tokio::test]
async fn real_cargo_moves_a_locked_package_without_a_manifest_change() {
    let manifest = "[package]\nname='fixture'\nversion='1.0.0'\nedition='2024'\n[dependencies]\noffline-patch-fixture='1'\n";
    let files = BTreeMap::from([
        ("Cargo.toml".into(), manifest.into()),
        ("src/lib.rs".into(), "// fixture\n".into()),
        ("Cargo.lock".into(), "version = 4\n[[package]]\nname='fixture'\nversion='1.0.0'\ndependencies=['offline-patch-fixture']\n[[package]]\nname='offline-patch-fixture'\nversion='1.0.0'\nsource='registry+https://github.com/rust-lang/crates.io-index'\n".into()),
    ]);
    let executor = fixture(files.clone(), false, true);
    let temp = tempfile::tempdir().unwrap();
    let runner = Runner::new(temp.path().into(), "offline-installation").with_executor(executor);
    let mut snapshot = snapshot(&files);
    let config = config();
    let mut dep = patchula_manager::Locked {
        manager: Manager::Cargo,
        file: "Cargo.lock".into(),
        package: "offline-patch-fixture".into(),
        version: "1.0.0".into(),
        location: patchula_manager::Location {
            start: 0,
            end: 1,
            line: 1,
            column: 1,
            pointer: String::new(),
        },
    }
    .dependency();
    dep.location.start = files["Cargo.lock"].rfind("'1.0.0'").unwrap();
    dep.location.end = dep.location.start + 7;
    let mut repository = repo();
    repository.package_files = serde_json::from_value(serde_json::json!([{
        "path": "Cargo.lock",
        "dependencies": [{
            "manager": "cargo", "name": "offline-patch-fixture", "package": "offline-patch-fixture",
            "current": {"value": "1.0.0", "version": "1.0.0", "digest": null},
            "updates": [{"newValue": null, "newVersion": "1.0.1", "newDigest": null,
                "updateType": "patch", "group": null, "fixes": ["RUSTSEC-test"]}]
        }]
    }]))
    .unwrap();
    let mut plans = crate::plan(
        &repository,
        &config,
        &[dep],
        &snapshot,
        false,
        None,
        monday(),
    )
    .unwrap();
    let p = &mut plans[0];
    runner.prepare(p, &mut snapshot, &config).await;
    assert_eq!(p.action.status, ActionStatus::Planned, "{:?}", p.action);
    assert!(!p.contents.contains_key("Cargo.toml"));
    assert!(
        p.contents["Cargo.lock"].contains("version = \"1.0.1\""),
        "{}",
        p.contents["Cargo.lock"]
    );
}

/// One nix invocation: its arguments and environment.
type NixCall = (Vec<String>, BTreeMap<String, String>);
/// Stands in for nix: records each nix invocation with its environment and
/// writes the given lock where nix would. Everything else goes to the fixture.
struct NixFake {
    inner: Arc<Fixture>,
    lock: String,
    output: Vec<u8>,
    /// What restricted evaluation of flake.nix's inputs prints.
    declared: String,
    calls: Mutex<Vec<NixCall>>,
}
impl Executor for NixFake {
    fn run(
        &self,
        spec: CommandSpec,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, String>> + Send + '_>> {
        if !spec.args.iter().any(|a| a == "nix") {
            return self.inner.run(spec);
        }
        Box::pin(async move {
            let env = spec
                .env
                .iter()
                .map(|(k, v)| (k.to_string_lossy().into(), v.to_string_lossy().into()))
                .collect();
            self.calls.lock().unwrap().push((spec.args.clone(), env));
            if spec.args.iter().any(|a| a == "eval") {
                return Ok(self.declared.clone().into_bytes());
            }
            if spec.args.iter().any(|a| a == "update") {
                std::fs::write(spec.cwd.join("flake.lock"), &self.lock).unwrap();
            }
            Ok(self.output.clone())
        })
    }
}
const NIXPKGS: &str = "1111111111111111111111111111111111111111";
const MOVED: &str = "9999999999999999999999999999999999999999";
/// The inputs `flake.nix` declares for `flake_lock(true)`: `private` is the
/// ssh input the lock already refuses.
const DECLARED: &str = r#"{"nixpkgs":{"url":"github:NixOS/nixpkgs/nixos-unstable"},"private":{"url":"git+ssh://git@example.com/private.git"}}"#;
fn flake_lock(private: bool) -> String {
    let private_node = r#""private":{"locked":{"lastModified":5,"narHash":"sha256-g","rev":"5555555555555555555555555555555555555555","type":"git","url":"ssh://git@example.com/private.git"},"original":{"type":"git","url":"ssh://git@example.com/private.git"}},"#;
    format!(
        r#"{{"nodes":{{"nixpkgs":{{"locked":{{"lastModified":1,"narHash":"sha256-a","owner":"NixOS","repo":"nixpkgs","rev":"{NIXPKGS}","type":"github"}},"original":{{"owner":"NixOS","ref":"nixos-unstable","repo":"nixpkgs","type":"github"}}}},{}"root":{{"inputs":{{"nixpkgs":"nixpkgs"{}}}}}}},"root":"root","version":7}}
"#,
        if private { private_node } else { "" },
        if private {
            r#","private":"private""#
        } else {
            ""
        },
    )
}
fn flake(private: bool) -> BTreeMap<String, String> {
    BTreeMap::from([
        (
            "dev/flake.nix".to_string(),
            "{ outputs = _: { }; }\n".to_string(),
        ),
        ("dev/flake.lock".to_string(), flake_lock(private)),
    ])
}
fn nix_fake(files: &BTreeMap<String, String>, lock: String) -> Arc<NixFake> {
    Arc::new(NixFake {
        inner: fixture(files.clone(), false, false),
        lock,
        output: Vec::new(),
        declared: if files.values().any(|f| f.contains("private.git")) {
            DECLARED.into()
        } else {
            r#"{"nixpkgs":"github:NixOS/nixpkgs/nixos-unstable"}"#.into()
        },
        calls: Mutex::new(Vec::new()),
    })
}
/// One flake input updated by name: a dependency, its digest update, the plan.
async fn flake_update(
    executor: Arc<NixFake>,
    files: &BTreeMap<String, String>,
) -> (Plan, tempfile::TempDir) {
    let temp = tempfile::tempdir().unwrap();
    let runner = Runner::new(temp.path().into(), "offline-installation").with_executor(executor);
    let mut snapshot = snapshot(files);
    let config = config();
    let extraction = patchula_manager::Scanner::new(&config)
        .unwrap()
        .extract(files)
        .unwrap();
    let mut repository = repo();
    repository.package_files = serde_json::from_value(serde_json::json!([{
        "path": "dev/flake.lock",
        "dependencies": [{
            "manager": "nix", "name": "nixpkgs", "package": "https://github.com/NixOS/nixpkgs",
            "current": {"value": "github:NixOS/nixpkgs/nixos-unstable", "version": null, "digest": NIXPKGS},
            "updates": [{"newValue": null, "newVersion": null, "newDigest": MOVED,
                "updateType": "digest", "group": null}]
        }]
    }]))
    .unwrap();
    let mut plans = crate::plan(
        &repository,
        &config,
        &extraction.dependencies,
        &snapshot,
        false,
        None,
        monday(),
    )
    .unwrap();
    let mut p = plans.remove(0);
    runner.prepare(&mut p, &mut snapshot, &config).await;
    (p, temp)
}

/// A flake input moves through `nix flake update <input>` with the pinned static
/// nix, a private chroot store, its own configuration, and the token only in
/// nix's per-host access tokens. The lock is the only file written.
#[tokio::test]
async fn flake_inputs_move_through_nix_with_a_private_store() {
    let files = flake(true);
    let executor = nix_fake(&files, flake_lock(true).replace(NIXPKGS, MOVED));
    let (p, temp) = flake_update(executor.clone(), &files).await;
    assert_eq!(p.action.status, ActionStatus::Planned, "{:?}", p.action);
    assert!(p.contents["dev/flake.lock"].contains(MOVED));
    assert!(!p.contents.contains_key("dev/flake.nix"));
    assert_eq!(p.action.branch, "patchula/nix-dev--nixpkgs");
    let calls = executor.calls.lock().unwrap();
    let [(eval, _), (args, env)] = calls.as_slice() else {
        panic!("an eval and an update expected: {calls:?}");
    };
    let (_, eval) = invocation(eval).unwrap();
    assert!(contains(
        &eval,
        &[
            "eval",
            "--json",
            "--impure",
            "--option",
            "restrict-eval",
            "true"
        ]
    ));
    let (specs, command) = invocation(args).unwrap();
    assert_eq!(
        specs,
        nix::tools()
            .unwrap()
            .iter()
            .map(|(t, v)| format!("{t}@{v}"))
            .collect::<Vec<_>>()
    );
    assert!(specs[0].starts_with("http:nix[url=https://hydra.nixos.org/build/"));
    assert!(specs[0].contains(",checksum=sha256:"));
    let store = temp.path().with_extension("tools").join("nix/root");
    assert!(starts(
        &command,
        &[
            "nix",
            "--store",
            &format!("local?root={}", store.display()),
            "flake",
            "update",
            "--flake"
        ]
    ));
    assert!(command[6].starts_with("path:") && command[6].ends_with("/tree/dev"));
    assert_eq!(&command[7..], ["--", "nixpkgs"]);
    let config = &env["NIX_CONFIG"];
    for line in [
        "experimental-features = nix-command flakes",
        "accept-flake-config = false",
        "max-jobs = 0",
        "access-tokens = github.com=offline-installation",
    ] {
        assert!(config.lines().any(|l| l == line), "{line}");
    }
    for (key, value) in env {
        assert!(
            key == "NIX_CONFIG" || !value.contains("offline-installation"),
            "{key}"
        );
    }
    let home = Path::new(&env["HOME"]);
    for key in ["NIX_CONF_DIR", "NIX_USER_CONF_FILES", "XDG_CONFIG_DIRS"] {
        assert!(
            Path::new(&env[key]).starts_with(home.parent().unwrap()),
            "{key}"
        );
    }
    assert!(!env.contains_key("NIX_REMOTE"));
}

/// Maintenance runs a bare `nix flake update`, unless an input is refused:
/// then it names every other input, so nix never fetches the refused one.
#[tokio::test]
async fn flake_maintenance_skips_refused_inputs() {
    for (private, expected) in [(false, None), (true, Some(vec!["--", "nixpkgs"]))] {
        let files = flake(private);
        let executor = nix_fake(&files, flake_lock(private).replace(NIXPKGS, MOVED));
        let temp = tempfile::tempdir().unwrap();
        let runner =
            Runner::new(temp.path().into(), "offline-installation").with_executor(executor.clone());
        let mut snapshot = snapshot(&files);
        let config = config();
        let mut p = maintenance(&repo(), &config, &snapshot, None, monday())
            .unwrap()
            .unwrap();
        assert_eq!(
            p.lock_jobs
                .iter()
                .map(|j| (j.ecosystem, j.directory.as_str()))
                .collect::<Vec<_>>(),
            [(Ecosystem::Nix, "dev")]
        );
        runner.prepare(&mut p, &mut snapshot, &config).await;
        assert_eq!(p.action.status, ActionStatus::Planned, "{:?}", p.action);
        let calls = executor.calls.lock().unwrap();
        let (_, command) = invocation(&calls[1].0).unwrap();
        let tail: Vec<&str> = command[7..].iter().map(String::as_str).collect();
        assert_eq!(expected.unwrap_or_default(), tail);
    }
}

/// The new lock is checked before anything is published: a node nix fetched
/// from outside the repository fails the branch, and so does a requested
/// input that did not move.
#[tokio::test]
async fn flake_locks_that_escape_or_do_not_move_fail() {
    let files = flake(false);
    let escaped = flake_lock(false).replace(NIXPKGS, MOVED).replace(
        r#""root":{"inputs""#,
        r#""host":{"locked":{"lastModified":1,"narHash":"sha256-h","path":"/home","type":"path"},"original":{"path":"/home","type":"path"}},"root":{"inputs""#,
    );
    for (lock, reason) in [
        (escaped, "node host: it resolves outside the repository"),
        (flake_lock(false), "not the approved"),
        (
            flake_lock(false).replace(NIXPKGS, &"8".repeat(40)),
            "it moved after the lookup",
        ),
    ] {
        let (p, _temp) = flake_update(nix_fake(&files, lock), &files).await;
        assert_eq!(p.action.status, ActionStatus::Error, "{:?}", p.action);
        assert!(
            p.action.reason.as_deref().unwrap().contains(reason),
            "{:?}",
            p.action
        );
    }
}

/// Lookups prefetch a flake reference with the same nix and store; local,
/// ssh and flag-like references never reach it.
#[tokio::test]
async fn flake_prefetch_runs_nix_only_for_remote_references() {
    use patchula_datasource::Flakes;
    let files = flake(false);
    let executor = Arc::new(NixFake {
        inner: fixture(files.clone(), false, false),
        lock: String::new(),
        output: br#"{"hash":"sha256-x","locked":{"rev":"abc"}}"#.to_vec(),
        declared: String::new(),
        calls: Mutex::new(Vec::new()),
    });
    let temp = tempfile::tempdir().unwrap();
    let runner = Runner::new(temp.path().into(), "").with_executor(executor.clone());
    let output = runner
        .prefetch("github:NixOS/nixpkgs/nixos-unstable", Some("secret-token"))
        .await
        .unwrap();
    assert!(output.starts_with(b"{\"hash\""));
    for refused in [
        "path:/etc",
        "git+ssh://git@host/r",
        "git+file:///srv/r",
        "--impure",
        "github:a/b c",
    ] {
        assert!(runner.prefetch(refused, None).await.is_err(), "{refused}");
    }
    let calls = executor.calls.lock().unwrap();
    let [(args, env)] = calls.as_slice() else {
        panic!("one nix call expected: {calls:?}");
    };
    let (_, command) = invocation(args).unwrap();
    assert_eq!(
        &command[3..],
        [
            "flake",
            "prefetch",
            "--json",
            "--refresh",
            "--",
            "github:NixOS/nixpkgs/nixos-unstable"
        ]
    );
    assert!(env["NIX_CONFIG"].contains("access-tokens = github.com=secret-token"));
}

/// Real nix relocks a flake offline: its one input is a sub-flake in the
/// repository, which needs no network.
#[tokio::test]
async fn real_nix_relocks_a_flake_offline() {
    let files = BTreeMap::from([
        (
            "flake.nix".to_string(),
            "{\n  inputs.sub.url = \"path:./sub\";\n  inputs.parent.url = ./sub;\n  outputs = { self, sub, parent }: { };\n}\n"
                .to_string(),
        ),
        (
            "sub/flake.nix".to_string(),
            "{ outputs = { self }: { }; }\n".to_string(),
        ),
        (
            "flake.lock".to_string(),
            "{\"nodes\":{\"root\":{}},\"root\":\"root\",\"version\":7}\n".to_string(),
        ),
    ]);
    let executor = fixture(files.clone(), false, true);
    let temp = tempfile::tempdir().unwrap();
    let runner = Runner::new(temp.path().into(), "offline-installation").with_executor(executor);
    let mut snapshot = snapshot(&files);
    let config = config();
    let mut p = maintenance(&repo(), &config, &snapshot, None, monday())
        .unwrap()
        .unwrap();
    runner.prepare(&mut p, &mut snapshot, &config).await;
    assert_eq!(p.action.status, ActionStatus::Planned, "{:?}", p.action);
    let lock: serde_json::Value = serde_json::from_str(&p.contents["flake.lock"]).unwrap();
    assert_eq!(lock["nodes"]["root"]["inputs"]["sub"], "sub");
    assert_eq!(lock["nodes"]["sub"]["locked"]["path"], "./sub");
    assert_eq!(lock["nodes"]["parent"]["locked"]["path"], "sub");
    assert_eq!(p.action.files.len(), 1);
}

/// Pruning empties the private nix store with `nix store gc` and drops the
/// fetcher cache; nothing in the store is a root.
#[tokio::test]
async fn prune_empties_the_nix_store() {
    let files = flake(false);
    let executor = nix_fake(&files, String::new());
    let temp = tempfile::tempdir().unwrap();
    let runner = Runner::new(temp.path().into(), "").with_executor(executor.clone());
    assert!(
        !runner
            .prune(30)
            .await
            .unwrap()
            .contains(&"nix store".to_string())
    );
    let cache = runner.tools().join("nix/cache/nix");
    std::fs::create_dir_all(&cache).unwrap();
    runner.record_use(&nix::tools().unwrap());
    assert!(
        runner
            .prune(30)
            .await
            .unwrap()
            .contains(&"nix store".to_string())
    );
    assert!(!cache.exists());
    let calls = executor.calls.lock().unwrap();
    let [(args, _)] = calls.as_slice() else {
        panic!("one nix call expected: {calls:?}");
    };
    assert_eq!(&invocation(args).unwrap().1[3..], ["store", "gc"]);
}

/// nix fetches what flake.nix declares, so inputs flake.nix adds or repoints
/// to a refused place fail the branch before nix fetches anything; nested
/// input overrides count too.
#[tokio::test]
async fn flake_nix_inputs_are_checked_before_nix_fetches() {
    let files = flake(true);
    for (declared, reason) in [
        (
            r#"{"nixpkgs":"github:NixOS/nixpkgs/nixos-unstable","evil":{"url":"tarball+http://127.0.0.1:8765/x","flake":false}}"#,
            "input evil: only https URLs are fetched",
        ),
        (
            r#"{"nixpkgs":{"url":"github:NixOS/nixpkgs","inputs":{"systems":{"url":"path:/etc"}}}}"#,
            "input nixpkgs: systems: it resolves outside the repository",
        ),
        (
            r#"{"private":{"url":"git+ssh://git@example.com/other.git"}}"#,
            "input private: ssh is not allowed; only https",
        ),
    ] {
        let executor = Arc::new(NixFake {
            inner: fixture(files.clone(), false, false),
            lock: flake_lock(true).replace(NIXPKGS, MOVED),
            output: Vec::new(),
            declared: declared.into(),
            calls: Mutex::new(Vec::new()),
        });
        let (p, _temp) = flake_update(executor.clone(), &files).await;
        assert_eq!(p.action.status, ActionStatus::Error, "{:?}", p.action);
        assert!(
            p.action.reason.as_deref().unwrap().contains(reason),
            "{:?}",
            p.action
        );
        let calls = executor.calls.lock().unwrap();
        assert!(
            !calls
                .iter()
                .any(|(args, _)| args.iter().any(|a| a == "update"))
        );
    }
}

/// Real nix reads flake.nix's inputs under restricted evaluation, and a plain
/// http input is refused without a request.
#[tokio::test]
async fn real_nix_refuses_a_declared_http_input_before_fetching() {
    let files = BTreeMap::from([
        (
            "flake.nix".to_string(),
            "{\n  inputs.evil = { url = \"tarball+http://127.0.0.1:9/x\"; flake = false; };\n  outputs = _: { boom = builtins.fetchurl \"http://127.0.0.1:9/y\"; };\n}\n".to_string(),
        ),
        (
            "flake.lock".to_string(),
            "{\"nodes\":{\"root\":{}},\"root\":\"root\",\"version\":7}\n".to_string(),
        ),
    ]);
    let executor = fixture(files.clone(), false, true);
    let temp = tempfile::tempdir().unwrap();
    let runner = Runner::new(temp.path().into(), "offline-installation").with_executor(executor);
    let mut snapshot = snapshot(&files);
    let config = config();
    let mut p = maintenance(&repo(), &config, &snapshot, None, monday())
        .unwrap()
        .unwrap();
    runner.prepare(&mut p, &mut snapshot, &config).await;
    assert_eq!(p.action.status, ActionStatus::Error, "{:?}", p.action);
    assert!(
        p.action
            .reason
            .as_deref()
            .unwrap()
            .contains("flake.nix: input evil: only https URLs are fetched"),
        "{:?}",
        p.action
    );
}

/// Inputs of different flakes never share a branch name, even when the root
/// flake's input name spells out the other flake's directory.
#[test]
fn flake_branches_keep_the_directory_apart_from_the_input() {
    let lock = |name: &str| {
        flake_lock(false).replace(
            "\"nixpkgs\":\"nixpkgs\"",
            &format!("\"{name}\":\"nixpkgs\""),
        )
    };
    let files = BTreeMap::from([
        (
            "flake.nix".to_string(),
            "{ outputs = _: { }; }\n".to_string(),
        ),
        ("flake.lock".to_string(), lock("dev-nixpkgs")),
        (
            "dev/flake.nix".to_string(),
            "{ outputs = _: { }; }\n".to_string(),
        ),
        ("dev/flake.lock".to_string(), lock("nixpkgs")),
    ]);
    let config = config();
    let extraction = patchula_manager::Scanner::new(&config)
        .unwrap()
        .extract(&files)
        .unwrap();
    let entry = |path: &str, name: &str| {
        serde_json::json!({
            "path": path,
            "dependencies": [{
                "manager": "nix", "name": name, "package": "https://github.com/NixOS/nixpkgs",
                "current": {"value": "github:NixOS/nixpkgs/nixos-unstable", "version": null, "digest": NIXPKGS},
                "updates": [{"newValue": null, "newVersion": null, "newDigest": MOVED,
                    "updateType": "digest", "group": null}]
            }]
        })
    };
    let mut repository = repo();
    repository.package_files = serde_json::from_value(serde_json::json!([
        entry("flake.lock", "dev-nixpkgs"),
        entry("dev/flake.lock", "nixpkgs"),
    ]))
    .unwrap();
    let plans = crate::plan(
        &repository,
        &config,
        &extraction.dependencies,
        &snapshot(&files),
        false,
        None,
        monday(),
    )
    .unwrap();
    let branches: BTreeSet<_> = plans.iter().map(|p| p.action.branch.as_str()).collect();
    assert_eq!(
        branches,
        BTreeSet::from(["patchula/nix-dev--nixpkgs", "patchula/nix-dev-nixpkgs"])
    );
}
