use super::*;
use rustovate_http::{Exchange, Transport};
use std::{future::Future, pin::Pin, sync::Mutex};

struct Fixture {
    files: BTreeMap<String, String>,
    fail: bool,
    real: bool,
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
            let leaf = if spec.args.iter().any(|s| s == "npm") {
                "package-lock.json"
            } else if spec.args.iter().any(|s| s == "cargo") {
                "Cargo.lock"
            } else if spec.args.iter().any(|s| s == "go") {
                "go.sum"
            } else if spec.args.iter().any(|s| s == "terraform") {
                ".terraform.lock.hcl"
            } else {
                "bun.lock"
            };
            write(&root, leaf, "fixture deterministic lock\n").unwrap();
            Ok(Vec::new())
        })
    }
}
fn config() -> Config {
    rustovate_config::load_files(
        "/repo/rustovate.pkl",
        BTreeMap::from([(
            "/repo/rustovate.pkl".into(),
            "amends \"Rustovate.pkl\"\nlockfiles { refresh = true; tidyGo = true }\n".into(),
        )]),
    )
    .unwrap()
}
fn fixture(files: BTreeMap<String, String>, fail: bool, real: bool) -> Arc<Fixture> {
    Arc::new(Fixture {
        files,
        fail,
        real,
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
        vec!["node@24.21.0", "--", "npm"],
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
