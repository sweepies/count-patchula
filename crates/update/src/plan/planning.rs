//! The plan-side tests of `lockfiles/tests.rs`: every test that exercises
//! planning and batching — `plan()`, `batch_plan()`, `maintenance()` and
//! `job()` — rather than running a tool, plus the goldens pinning the bytes
//! the lockfile contract freezes: `Job`'s wire shape and set order, the
//! built and maintenance fingerprints, the maintenance `update_key` and the
//! report reasons `job()` errors and generation failures produce. The
//! tool-run tests stay in patchula-tools' `lockfiles::tests`.
use super::*;
use crate::maintenance::{defer_completed_refresh, maintenance, record_refresh};
use patchula_lockfiles::{self as contract, Ecosystem, Job, Scope};
use patchula_words::Manager;

/// `job()` over a snapshot's path set, as callers hold it.
fn job(dep: &Dependency, update: &Update, snapshot: &Snapshot) -> Result<Option<Job>, String> {
    contract::job(dep, update, &snapshot.paths)
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
        tooling: Vec::new(),
        package_files: Vec::new(),
    }
}
fn snapshot(files: &BTreeMap<String, String>) -> Snapshot {
    Snapshot {
        cargo_cooldowns: Default::default(),
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
        public_only: Default::default(),
    }
}
fn monday() -> chrono::DateTime<chrono::Utc> {
    "2026-10-19T01:00:00Z".parse().unwrap()
}

#[tokio::test]
async fn scheduled_refresh_uses_stable_name_windows_weekly_cadence_and_wait_hours() {
    let files = BTreeMap::from([("Cargo.lock".into(), "version = 4\n".into())]);
    let snapshot = snapshot(&files);
    let mut config = config();
    let repo = repo();
    let p = maintenance(&repo, &config, &snapshot, &[], None, monday())
        .unwrap()
        .unwrap();
    assert_eq!(p.action().branch, "patchula/lock-file-maintenance");
    assert_eq!(p.action().status, BranchStatus::Planned);
    let outside = maintenance(
        &repo,
        &config,
        &snapshot,
        &[],
        None,
        monday() + chrono::Duration::days(1),
    )
    .unwrap()
    .unwrap();
    assert_eq!(outside.action().status, BranchStatus::Skipped);
    let cache = patchula_cache::Cache::open(":memory:", Default::default())
        .await
        .unwrap();
    let mut p = crate::plan::testing::Draft::from(p)
        .lockfiles_ready(true)
        .build();
    p.hold(crate::plan::Hold::LockfilesCurrent);
    record_refresh(&cache, &[p], &[], monday()).await.unwrap();
    for days in [0, 7] {
        config.lockfiles.wait_hours = 200;
        let now = monday() + chrono::Duration::days(days);
        let mut p = maintenance(&repo, &config, &snapshot, &[], None, now)
            .unwrap()
            .unwrap();
        defer_completed_refresh(&cache, &mut p, &config, now)
            .await
            .unwrap();
        assert_eq!(p.action().status, BranchStatus::Skipped);
    }
    config.lockfiles.wait_hours = 0;
    let mut next = maintenance(
        &repo,
        &config,
        &snapshot,
        &[],
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
    assert_eq!(next.action().status, BranchStatus::Planned);
}

#[test]
fn mise_lock_maintenance_respects_refresh_and_unlocked_manifests_need_no_job() {
    let files = BTreeMap::from([("mise.toml".into(), "[tools]\nnode='22.0.0'\n".into())]);
    let mut snapshot = snapshot(&files);
    let mut config = config();
    let dep = patchula_manager::Scanner::new(&config)
        .unwrap()
        .extract(&patchula_manager::all_read(&files))
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
        maintenance(&repo(), &config, &snapshot, &[], None, monday())
            .unwrap()
            .is_none()
    );
    assert!(
        job(&dep, &update, &snapshot).unwrap().is_some(),
        "ordinary updates must still refresh a committed lock"
    );
    config.lockfiles.refresh = true;
    let plan = maintenance(&repo(), &config, &snapshot, &[], None, monday())
        .unwrap()
        .unwrap();
    assert_eq!(plan.lock_jobs().len(), 1);
    assert_eq!(plan.lock_jobs()[0].ecosystem, Ecosystem::Mise);
}

/// Maintenance plans an Actions job for `.github/workflows/actions.lock`
/// when the file exists and the Actions manager is enabled, beside the
/// repository's other refresh jobs; workflows without the lockfile plan
/// none.
#[test]
fn actions_lock_maintenance_needs_the_lockfile() {
    let workflow = "name: ci\non: push\njobs:\n  t:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v7\n";
    let mut files = BTreeMap::from([
        (".github/workflows/ci.yml".to_owned(), workflow.to_owned()),
        ("Cargo.lock".to_owned(), "version = 4\n".to_owned()),
    ]);
    let config = config();
    let jobs = |files: &BTreeMap<String, String>| {
        maintenance(&repo(), &config, &snapshot(files), &[], None, monday())
            .unwrap()
            .unwrap()
            .lock_jobs()
            .iter()
            .map(|job| (job.ecosystem, job.directory.clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(jobs(&files), [(Ecosystem::Cargo, String::new())]);
    files.insert(
        contract::ACTIONS_LOCK.to_owned(),
        "version: 'v0.0.3'\nworkflows: {}\ndependencies: {}\n".to_owned(),
    );
    assert_eq!(
        jobs(&files),
        [
            (Ecosystem::Cargo, String::new()),
            (Ecosystem::Actions, ".github/workflows".to_owned()),
        ]
    );
    // The same leaf outside `.github/workflows/` is not the lockfile.
    let elsewhere = BTreeMap::from([(
        ".github/actions.lock".to_owned(),
        "version: 'v0.0.3'\n".to_owned(),
    )]);
    assert!(
        maintenance(&repo(), &config, &snapshot(&elsewhere), &[], None, monday())
            .unwrap()
            .is_none()
    );
}

/// A job's scope is an enum in memory but its wire shape is the flat field
/// layout it always was: job identities feed branch and plan hashes, so the
/// bytes cannot drift.
#[test]
fn lock_jobs_keep_their_serialized_field_layout() {
    let targeted = Job {
        ecosystem: Ecosystem::Cargo,
        directory: "crates/api".into(),
        scope: Scope::Package {
            package: "serde".into(),
            current: Some("1.0.0".into()),
            target: Some("1.0.1".into()),
        },
        exact_version: true,
        publish_age_allow: false,
        wait_hours: 0,
    };
    assert_eq!(
        serde_json::to_string(&targeted).unwrap(),
        r#"{"ecosystem":"Cargo","directory":"crates/api","package":"serde","current":"1.0.0","target":"1.0.1","lock_only":true,"publish_age_allow":false,"wait_hours":0}"#
    );
    let whole = Job {
        ecosystem: Ecosystem::Node,
        directory: String::new(),
        scope: Scope::Directory,
        exact_version: false,
        publish_age_allow: false,
        wait_hours: 0,
    };
    assert_eq!(
        serde_json::to_string(&whole).unwrap(),
        r#"{"ecosystem":"Node","directory":"","package":null,"current":null,"target":null,"lock_only":false,"publish_age_allow":false,"wait_hours":0}"#
    );
    let back: Job = serde_json::from_str(
        r#"{"ecosystem":"Cargo","directory":"crates/api","package":"serde","current":"1.0.0","target":"1.0.1","lock_only":true,"publish_age_allow":false,"wait_hours":0}"#,
    )
    .unwrap();
    assert_eq!(back, targeted);
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
        cooldown: None,
        new_value: Some("2.0.0".into()),
        new_version: Some("2.0.0".into()),
        new_digest: None,
        update_type: patchula_words::Update::Major,
        group: None,
        policy: None,
        release_age: None,
        fixes: Vec::new(),
        image: None,
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

/// A lockfile of a tool nothing here runs holds the update rather than
/// leaving the lock stale behind a manifest-only change.
#[test]
fn a_python_lockfile_nothing_regenerates_holds_the_update() {
    let manifest =
        "[project]\nname = 'app'\nversion = '1.0.0'\ndependencies = ['requests==2.0.0']\n";
    let update: Update = serde_json::from_value(
        serde_json::json!({"updateType":"major","newVersion":"3.0.0","newValue":"==3.0.0"}),
    )
    .unwrap();
    // The locks beside or above `packages/app/pyproject.toml`; the closest
    // governs it, and an unsupported one beside a supported one still holds.
    for (locks, held) in [
        (vec!["packages/app/pdm.lock"], Some("pdm.lock")),
        (vec!["packages/app/pylock.toml"], Some("pylock.toml")),
        (vec!["packages/app/Pipfile.lock"], Some("Pipfile.lock")),
        (vec!["packages/app/uv.lock"], None),
        (
            vec!["packages/app/uv.lock", "packages/app/pdm.lock"],
            Some("pdm.lock"),
        ),
        (vec!["Pipfile.lock", "packages/app/uv.lock"], None),
        (vec!["pdm.lock", "packages/app/poetry.lock"], None),
        (vec!["uv.lock", "packages/app/pdm.lock"], Some("pdm.lock")),
        (vec!["pdm.lock"], Some("pdm.lock")),
    ] {
        let mut files = BTreeMap::from([(
            "packages/app/pyproject.toml".to_string(),
            manifest.to_string(),
        )]);
        for lock in &locks {
            files.insert((*lock).into(), String::new());
        }
        let dep = patchula_manager::Scanner::new(&config())
            .unwrap()
            .extract(&patchula_manager::all_read(&files))
            .unwrap()
            .dependencies
            .remove(0);
        let result = job(&dep, &update, &snapshot(&files));
        if let Some(lock) = held {
            assert_eq!(
                result.as_ref().unwrap_err(),
                &format!("unsupported lockfile format: {lock}"),
                "{locks:?}"
            );
        } else {
            assert!(result.unwrap().is_some(), "{locks:?}");
        }
    }
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
        .extract(&patchula_manager::all_read(&files))
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
    let branches: BTreeSet<_> = plans.iter().map(|p| p.action().branch.as_str()).collect();
    assert_eq!(
        branches,
        BTreeSet::from(["patchula/nix-dev--nixpkgs", "patchula/nix-dev-nixpkgs"])
    );
}

/// Bundler, Composer and Poetry lock-only updates, with the lock they start from.
/// Manager, source, files, manifest, package, locked version and ecosystem.
type LockOnlyCase = (
    &'static str,
    &'static str,
    BTreeMap<String, String>,
    &'static str,
    &'static str,
    &'static str,
    Ecosystem,
);
fn lock_only_cases() -> Vec<LockOnlyCase> {
    vec![
        (
            "bundler",
            "rubygems",
            BTreeMap::from([
                (
                    "Gemfile".to_string(),
                    "source \"https://rubygems.org\"\ngem \"rake\", \"~> 13.0\"\n".to_string(),
                ),
                (
                    "Gemfile.lock".to_string(),
                    "GEM\n  remote: https://rubygems.org/\n  specs:\n    rake (13.0.6)\n\nPLATFORMS\n  ruby\n\nDEPENDENCIES\n  rake (~> 13.0)\n".to_string(),
                ),
            ]),
            "Gemfile",
            "rake",
            "13.0.6",
            Ecosystem::Ruby,
        ),
        (
            "composer",
            "packagist",
            BTreeMap::from([
                (
                    "composer.json".to_string(),
                    "{\"require\": {\"psr/log\": \"^3.0\"}}\n".to_string(),
                ),
                (
                    "composer.lock".to_string(),
                    "{\"packages\": [{\"name\": \"psr/log\", \"version\": \"3.0.0\", \"notification-url\": \"https://packagist.org/downloads/\"}]}\n".to_string(),
                ),
            ]),
            "composer.json",
            "psr/log",
            "3.0.0",
            Ecosystem::Php,
        ),
        (
            "pyproject",
            "pypi",
            BTreeMap::from([
                (
                    "pyproject.toml".to_string(),
                    "[project]\nname = \"app\"\ndependencies = [\"six>=1.15\"]\n".to_string(),
                ),
                (
                    "uv.lock".to_string(),
                    "version = 1\n\n[[package]]\nname = \"six\"\nversion = \"1.15.0\"\nsource = { registry = \"https://pypi.org/simple\" }\n".to_string(),
                ),
            ]),
            "pyproject.toml",
            "six",
            "1.15.0",
            Ecosystem::Python,
        ),
        (
            "pyproject",
            "pypi",
            BTreeMap::from([
                (
                    "pyproject.toml".to_string(),
                    "[project]\nname = \"app\"\ndependencies = [\"six>=1.15\"]\n".to_string(),
                ),
                (
                    "poetry.lock".to_string(),
                    "[[package]]\nname = \"six\"\nversion = \"1.15.0\"\n".to_string(),
                ),
            ]),
            "pyproject.toml",
            "six",
            "1.15.0",
            Ecosystem::Python,
        ),
    ]
}

/// One in-range update of a direct dependency, as the report carries it: no
/// `newValue`, because the declaration stays as it is.
fn lock_only_plan(
    manager: &str,
    source: &str,
    files: &BTreeMap<String, String>,
    manifest: &str,
    name: &str,
    current: &str,
    target: &str,
) -> (Plan, Vec<patchula_manager::Dependency>) {
    let config = config();
    let extraction = patchula_manager::Scanner::new(&config)
        .unwrap()
        .extract(&patchula_manager::all_read(files))
        .unwrap();
    let dep = extraction
        .dependencies
        .iter()
        .find(|d| d.package == name)
        .unwrap_or_else(|| panic!("{name} not extracted: {:?}", extraction.dependencies));
    let mut repository = repo();
    repository.package_files = serde_json::from_value(serde_json::json!([{
        "path": manifest,
        "dependencies": [{
            "manager": manager, "name": dep.name, "package": dep.package,
            "current": {"value": dep.current, "version": dep.resolved, "digest": null},
            "updates": [{"newValue": null, "newVersion": target, "newDigest": null,
                "updateType": "minor", "group": null}],
            "metadata": {"source": source, "kinds": ["production"], "locations": []}
        }]
    }]))
    .unwrap();
    let mut plans = crate::plan(
        &repository,
        &config,
        &extraction.dependencies,
        &snapshot(files),
        false,
        None,
        monday(),
    )
    .unwrap();
    assert_eq!(plans.len(), 1);
    assert_eq!(current, dep.resolved.as_deref().unwrap());
    (plans.remove(0), extraction.dependencies)
}

#[test]
fn an_in_range_release_is_a_lock_only_plan_for_bundler_composer_and_pyproject() {
    for (manager, source, files, manifest, package, locked, ecosystem) in lock_only_cases() {
        // A release inside each declared range: `~> 13.0`, `^3.0` and `>=1.15`.
        let target = match ecosystem {
            Ecosystem::Ruby => "13.2.1",
            Ecosystem::Php => "3.0.2",
            _ => "1.17.0",
        };
        let (plan, dependencies) =
            lock_only_plan(manager, source, &files, manifest, package, locked, target);
        {
            use patchula_versioning::Versioning;
            let dep = &dependencies[0];
            let scheme = patchula_versioning::Ecosystem::of(dep.scheme, dep.source);
            assert!(
                scheme
                    .matches(&dep.current, &scheme.parse(target).unwrap())
                    .unwrap(),
                "{manager}: {target} is outside {}",
                dep.current
            );
        }
        assert_eq!(
            plan.action().status,
            BranchStatus::Planned,
            "{manager}: {:?}",
            plan.action()
        );
        assert!(plan.contents().is_empty(), "{manager}");
        assert_eq!(
            plan.action().message,
            format!("chore(deps): update {package} to {target}"),
            "{manager}"
        );
        assert_eq!(
            plan.lock_jobs(),
            [Job {
                ecosystem,
                directory: String::new(),
                scope: Scope::Package {
                    package: dependencies[0].package.clone(),
                    current: Some(locked.into()),
                    target: Some(target.into()),
                },
                exact_version: true,
                publish_age_allow: false,
                wait_hours: 0,
            }],
            "{manager}"
        );
    }
}

/// With no lockfile committed, the same update has no job and so no branch.
#[test]
fn a_lock_only_update_without_a_committed_lockfile_plans_nothing_to_do() {
    let files = BTreeMap::from([(
        "Gemfile".to_string(),
        "source \"https://rubygems.org\"\ngem \"rake\", \"~> 13.0\"\n".to_string(),
    )]);
    let config = config();
    let extraction = patchula_manager::Scanner::new(&config)
        .unwrap()
        .extract(&patchula_manager::all_read(&files))
        .unwrap();
    assert_eq!(
        extraction.dependencies[0]
            .proposing()
            .lock
            .locked(extraction.dependencies[0].resolved.as_deref()),
        None
    );
    let update = patchula_report::Update {
        cooldown: None,
        new_value: None,
        new_version: Some("13.2.1".into()),
        new_digest: None,
        update_type: patchula_words::Update::Minor,
        group: None,
        policy: None,
        release_age: None,
        fixes: Vec::new(),
        image: None,
    };
    assert_eq!(
        job(&extraction.dependencies[0], &update, &snapshot(&files)),
        Ok(None)
    );
}

/// A branch that carries a security fix takes that fix's commit prefix, even
/// when another occurrence of the package moves to the same version without
/// needing the fix.
#[test]
fn a_branch_with_a_security_fix_takes_its_commit_prefix() {
    let manifest = |name: &str| {
        format!(
            "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n\n[dependencies]\ntokio = \"1.0\"\n"
        )
    };
    let files = BTreeMap::from([
        ("a/Cargo.toml".to_string(), manifest("a")),
        ("b/Cargo.toml".to_string(), manifest("b")),
    ]);
    let config = config();
    let extraction = patchula_manager::Scanner::new(&config)
        .unwrap()
        .extract(&patchula_manager::all_read(&files))
        .unwrap();
    let update = |prefix: &str, fixes: serde_json::Value| {
        serde_json::json!({"newValue": "=1.40.0", "newVersion": "1.40.0", "newDigest": null,
            "updateType": "minor", "group": null, "fixes": fixes,
            "policy": {"waitHours": 0, "windows": [], "merge": false,
                "labels": ["dependencies"], "commitPrefix": prefix,
                "releaseNotes": true, "bodyNotes": []}})
    };
    let file = |path: &str, update: serde_json::Value| {
        serde_json::json!({"path": path, "dependencies": [{
            "manager": "cargo", "name": "tokio", "package": "tokio",
            "current": {"value": "1.0", "version": null, "digest": null},
            "updates": [update],
            "metadata": {"source": "crates", "kinds": ["production"], "locations": []}
        }]})
    };
    for (vulnerable, plain, expected) in [
        (
            "fix(deps):",
            "chore(deps):",
            "fix(deps): update tokio to 1.40.0 [security]",
        ),
        (
            "fix(deps):",
            "fix(deps):",
            "fix(deps): update tokio to 1.40.0 [security]",
        ),
    ] {
        let mut repository = repo();
        repository.package_files = serde_json::from_value(serde_json::json!([
            file(
                "a/Cargo.toml",
                update(vulnerable, serde_json::json!(["GHSA-test"]))
            ),
            file("b/Cargo.toml", update(plain, serde_json::json!([]))),
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
        assert_eq!(plans.len(), 1);
        assert_eq!(
            plans[0].action().status,
            BranchStatus::Planned,
            "{:?}",
            plans[0].action()
        );
        assert_eq!(plans[0].action().message, expected);
    }
}

fn cargo_change(package: &str, current: &str, version: &str) -> crate::plan::Change {
    crate::plan::Change {
        registry: None,
        source: patchula_words::Source::Crates,
        package: package.into(),
        file: "Cargo.toml".into(),
        current: current.into(),
        current_version: None,
        value: None,
        version: Some(version.into()),
        digest: None,
        kind: patchula_words::Update::Patch,
        merge: false,
        labels: Vec::new(),
        release_notes: false,
        body_notes: Vec::new(),
        advisories: Vec::new(),
        image: None,
    }
}

/// One batch item carrying `targets`, as a ticked PR's marker held them.
fn batched(targets: Vec<crate::plan::Change>) -> Vec<crate::batch::Item> {
    vec![crate::batch::Item {
        origin: "o".repeat(64),
        number: None,
        title: "batched".into(),
        targets,
    }]
}

/// A lock that already records the target, whatever build metadata its release
/// carries, has nothing left to do: `zstd-sys` 2.1.2 is locked as
/// `2.1.2+zstd.1.5.7`.
#[test]
fn a_batched_lock_only_target_is_complete_once_the_lock_holds_the_release() {
    let files = BTreeMap::from([
        (
            "Cargo.toml".to_string(),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nzstd-sys = \"2\"\n"
                .to_string(),
        ),
        (
            "Cargo.lock".to_string(),
            "version = 4\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\"zstd-sys\"]\n\n[[package]]\nname = \"zstd-sys\"\nversion = \"2.1.2+zstd.1.5.7\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n"
                .to_string(),
        ),
    ]);
    let config = config();
    let dependencies = patchula_manager::Scanner::new(&config)
        .unwrap()
        .extract(&patchula_manager::all_read(&files))
        .unwrap()
        .dependencies;
    assert_eq!(
        dependencies[0].resolved.as_deref(),
        Some("2.1.2+zstd.1.5.7")
    );
    let batch = |version: &str| {
        crate::plan::batch_plan(
            &repo(),
            &config,
            &dependencies,
            &snapshot(&files),
            batched(vec![cargo_change("zstd-sys", "2", version)]),
            false,
            None,
            monday(),
        )
        .unwrap()
    };
    assert!(batch("2.1.2").is_none());
    assert!(batch("2.1.2+zstd.1.5.7").is_none());
    assert!(batch("2.1.3").is_some());
}

fn cargo_lock(version: &str) -> String {
    format!(
        "version = 4\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\"tokio\"]\n\n[[package]]\nname = \"tokio\"\nversion = \"{version}\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n"
    )
}

/// With no Cargo.lock committed, a release the range already allows changes
/// nothing; cargo would otherwise generate a lock and publish it.
#[test]
fn a_lock_only_cargo_update_without_a_committed_lock_has_no_job() {
    let files = BTreeMap::from([(
        "Cargo.toml".to_string(),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\ntokio = \"1\"\n"
            .to_string(),
    )]);
    let config = config();
    let dependencies = patchula_manager::Scanner::new(&config)
        .unwrap()
        .extract(&patchula_manager::all_read(&files))
        .unwrap()
        .dependencies;
    let update = |value: Option<&str>| patchula_report::Update {
        cooldown: None,
        new_value: value.map(Into::into),
        new_version: Some("1.40.0".into()),
        new_digest: None,
        update_type: patchula_words::Update::Minor,
        group: None,
        policy: None,
        release_age: None,
        fixes: Vec::new(),
        image: None,
    };
    assert_eq!(
        job(&dependencies[0], &update(None), &snapshot(&files)),
        Ok(None)
    );
    // A manifest edit still regenerates the lock beside it.
    assert!(
        job(
            &dependencies[0],
            &update(Some("=1.40.0")),
            &snapshot(&files)
        )
        .unwrap()
        .is_some()
    );
    let mut with_lock = files.clone();
    with_lock.insert("Cargo.lock".into(), cargo_lock("1.39.0"));
    assert!(
        job(&dependencies[0], &update(None), &snapshot(&with_lock))
            .unwrap()
            .is_some()
    );
}

/// A PR records each file once, but a workflow can use the same action in
/// several jobs; the batch moves every one of them, as the PR did.
#[test]
fn a_batch_moves_every_declaration_of_a_dependency_in_a_file() {
    let workflow = "on: push\njobs:\n  a:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v4.1.0\n  b:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v4.1.0\n  c:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v4.1.0\n";
    let files = BTreeMap::from([(".github/workflows/ci.yml".to_string(), workflow.to_string())]);
    let config = config();
    let dependencies = patchula_manager::Scanner::new(&config)
        .unwrap()
        .extract(&patchula_manager::all_read(&files))
        .unwrap()
        .dependencies
        .into_iter()
        .filter(|d| d.package == "actions/checkout")
        .collect::<Vec<_>>();
    assert_eq!(dependencies.len(), 3, "{dependencies:?}");
    let target = crate::plan::Change {
        registry: None,
        source: dependencies[0].source,
        package: dependencies[0].package.clone(),
        file: ".github/workflows/ci.yml".into(),
        current: dependencies[0].current.clone(),
        current_version: None,
        value: Some("v4.2.0".into()),
        version: Some("4.2.0".into()),
        digest: None,
        kind: patchula_words::Update::Minor,
        merge: false,
        labels: Vec::new(),
        release_notes: false,
        body_notes: Vec::new(),
        advisories: Vec::new(),
        image: None,
    };
    let plan = crate::plan::batch_plan(
        &repo(),
        &config,
        &dependencies,
        &snapshot(&files),
        batched(vec![target]),
        false,
        None,
        monday(),
    )
    .unwrap()
    .expect("a batch plan");
    let written = &plan.contents()[".github/workflows/ci.yml"];
    assert_eq!(
        written.matches("actions/checkout@v4.2.0").count(),
        3,
        "{written}"
    );
    assert!(!written.contains("@v4.1.0"), "{written}");
}

/// A batch marker keeps the target but not the cooldown: the reported
/// update that proposed a security fix inside the committed window hands
/// its marker over, so the rebuilt Cargo job keeps `publish_age_allow`
/// where a marker of its own would have dropped it.
#[test]
fn a_batch_keeps_a_security_fixs_cooldown_on_its_job() {
    let files = BTreeMap::from([
        (
            "Cargo.toml".to_string(),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\ntokio = \"1.39.0\"\n"
                .to_string(),
        ),
        ("Cargo.lock".to_string(), cargo_lock("1.39.0")),
    ]);
    let config = config();
    let dependencies = patchula_manager::Scanner::new(&config)
        .unwrap()
        .extract(&patchula_manager::all_read(&files))
        .unwrap()
        .dependencies;
    let mut repo = repo();
    repo.package_files.push(patchula_report::PackageFile {
        path: "Cargo.toml".into(),
        dependencies: vec![patchula_report::Dependency {
            manager: patchula_words::Manager::Cargo,
            name: "tokio".into(),
            occurrence: None,
            package: "tokio".into(),
            registry: None,
            current: patchula_report::Current {
                value: Some("1.39.0".into()),
                version: Some("1.39.0".into()),
                digest: None,
            },
            updates: vec![patchula_report::Update {
                new_value: None,
                new_version: Some("1.40.0".into()),
                new_digest: None,
                update_type: patchula_words::Update::Patch,
                group: None,
                policy: None,
                release_age: None,
                fixes: vec!["GHSA-1".into()],
                image: None,
                cooldown: Some(patchula_report::Cooldown {
                    hours: 336,
                    file: ".cargo/config.toml".into(),
                }),
            }],
            metadata: None,
        }],
    });
    let plan = crate::plan::batch_plan(
        &repo,
        &config,
        &dependencies,
        &snapshot(&files),
        batched(vec![cargo_change("tokio", "1.39.0", "1.40.0")]),
        false,
        None,
        monday(),
    )
    .unwrap()
    .expect("a batch plan");
    let job = plan
        .lock_jobs()
        .iter()
        .find(|job| job.ecosystem == Ecosystem::Cargo)
        .expect("a Cargo job");
    assert!(job.publish_age_allow, "{job:?}");
}

/// With no committed cooldown, a fix moved into a batch runs Cargo under
/// `security.apply`'s wait (none by default), not the routine 24 hours,
/// so Cargo does not refuse a fix lookup proposed fresh.
#[test]
fn a_batched_fix_waits_only_what_security_apply_says() {
    let files = BTreeMap::from([
        (
            "Cargo.toml".to_string(),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\ntokio = \"1.39.0\"\n"
                .to_string(),
        ),
        ("Cargo.lock".to_string(), cargo_lock("1.39.0")),
    ]);
    let config = config();
    let dependencies = patchula_manager::Scanner::new(&config)
        .unwrap()
        .extract(&patchula_manager::all_read(&files))
        .unwrap()
        .dependencies;
    let mut repo = repo();
    repo.package_files.push(patchula_report::PackageFile {
        path: "Cargo.toml".into(),
        dependencies: vec![patchula_report::Dependency {
            manager: patchula_words::Manager::Cargo,
            name: "tokio".into(),
            occurrence: None,
            package: "tokio".into(),
            registry: None,
            current: patchula_report::Current {
                value: Some("1.39.0".into()),
                version: Some("1.39.0".into()),
                digest: None,
            },
            updates: vec![patchula_report::Update {
                new_value: None,
                new_version: Some("1.40.0".into()),
                new_digest: None,
                update_type: patchula_words::Update::Patch,
                group: None,
                policy: None,
                release_age: None,
                fixes: vec!["GHSA-1".into()],
                image: None,
                cooldown: None,
            }],
            metadata: None,
        }],
    });
    let plan = crate::plan::batch_plan(
        &repo,
        &config,
        &dependencies,
        &snapshot(&files),
        batched(vec![cargo_change("tokio", "1.39.0", "1.40.0")]),
        false,
        None,
        monday(),
    )
    .unwrap()
    .expect("a batch plan");
    let job = plan
        .lock_jobs()
        .iter()
        .find(|job| job.ecosystem == Ecosystem::Cargo)
        .expect("a Cargo job");
    assert!(!job.publish_age_allow, "{job:?}");
    assert_eq!(job.wait_hours, 0, "{job:?}");
}

/// A routine update moved into a batch waits as its own kind.
#[test]
fn a_batched_routine_update_keeps_the_routine_wait() {
    let files = BTreeMap::from([
        (
            "Cargo.toml".to_string(),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\ntokio = \"1.39.0\"\n"
                .to_string(),
        ),
        ("Cargo.lock".to_string(), cargo_lock("1.39.0")),
    ]);
    let config = config();
    let dependencies = patchula_manager::Scanner::new(&config)
        .unwrap()
        .extract(&patchula_manager::all_read(&files))
        .unwrap()
        .dependencies;
    let mut repo = repo();
    repo.package_files.push(patchula_report::PackageFile {
        path: "Cargo.toml".into(),
        dependencies: vec![patchula_report::Dependency {
            manager: patchula_words::Manager::Cargo,
            name: "tokio".into(),
            occurrence: None,
            package: "tokio".into(),
            registry: None,
            current: patchula_report::Current {
                value: Some("1.39.0".into()),
                version: Some("1.39.0".into()),
                digest: None,
            },
            updates: vec![patchula_report::Update {
                new_value: None,
                new_version: Some("1.40.0".into()),
                new_digest: None,
                update_type: patchula_words::Update::Patch,
                group: None,
                policy: None,
                release_age: None,
                fixes: vec![],
                image: None,
                cooldown: None,
            }],
            metadata: None,
        }],
    });
    let plan = crate::plan::batch_plan(
        &repo,
        &config,
        &dependencies,
        &snapshot(&files),
        batched(vec![cargo_change("tokio", "1.39.0", "1.40.0")]),
        false,
        None,
        monday(),
    )
    .unwrap()
    .expect("a batch plan");
    let job = plan
        .lock_jobs()
        .iter()
        .find(|job| job.ecosystem == Ecosystem::Cargo)
        .expect("a Cargo job");
    assert!(!job.publish_age_allow, "{job:?}");
    assert_eq!(job.wait_hours, 24, "{job:?}");
}

/// A routine update moved into a batch waits as its own kind, not the
/// strictest wait any kind carries: a batched minor under a 720-hour major
/// rule still waits its 24.
#[test]
fn a_batched_minor_waits_as_a_minor_not_the_strictest_kind() {
    let files = BTreeMap::from([
        (
            "Cargo.toml".to_string(),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\ntokio = \"1.39.0\"\n"
                .to_string(),
        ),
        ("Cargo.lock".to_string(), cargo_lock("1.39.0")),
    ]);
    let config = patchula_config::load_files(
        "/repo/patchula.pkl",
        BTreeMap::from([(
            "/repo/patchula.pkl".into(),
            "amends \"PatchulaConfig.pkl\"\nlockfiles { refresh = true; tidyGo = true }\nrules { new Rule { select { updates { \"major\" } } apply { waitHours = 720 } } }\n"
                .into(),
        )]),
    )
    .unwrap();
    let dependencies = patchula_manager::Scanner::new(&config)
        .unwrap()
        .extract(&patchula_manager::all_read(&files))
        .unwrap()
        .dependencies;
    let mut repo = repo();
    repo.package_files.push(patchula_report::PackageFile {
        path: "Cargo.toml".into(),
        dependencies: vec![patchula_report::Dependency {
            manager: patchula_words::Manager::Cargo,
            name: "tokio".into(),
            occurrence: None,
            package: "tokio".into(),
            registry: None,
            current: patchula_report::Current {
                value: Some("1.39.0".into()),
                version: Some("1.39.0".into()),
                digest: None,
            },
            updates: vec![patchula_report::Update {
                new_value: None,
                new_version: Some("1.40.0".into()),
                new_digest: None,
                update_type: patchula_words::Update::Minor,
                group: None,
                policy: None,
                release_age: None,
                fixes: vec![],
                image: None,
                cooldown: None,
            }],
            metadata: None,
        }],
    });
    let mut change = cargo_change("tokio", "1.39.0", "1.40.0");
    change.kind = patchula_words::Update::Minor;
    let plan = crate::plan::batch_plan(
        &repo,
        &config,
        &dependencies,
        &snapshot(&files),
        batched(vec![change]),
        false,
        None,
        monday(),
    )
    .unwrap()
    .expect("a batch plan");
    let job = plan
        .lock_jobs()
        .iter()
        .find(|job| job.ecosystem == Ecosystem::Cargo)
        .expect("a Cargo job");
    assert!(!job.publish_age_allow, "{job:?}");
    assert_eq!(job.wait_hours, 24, "{job:?}");
}

/// A fix moved into a batch keeps its advisory records: the rebuilt change
/// lists them, so the batch's body shows what it fixes and
/// `SecurityMergeWithoutDelivery` sees a fix it holds.
#[test]
fn a_batched_fix_keeps_its_advisories() {
    let files = BTreeMap::from([
        (
            "Cargo.toml".to_string(),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\ntokio = \"1.39.0\"\n"
                .to_string(),
        ),
        ("Cargo.lock".to_string(), cargo_lock("1.39.0")),
    ]);
    let config = config();
    let dependencies = patchula_manager::Scanner::new(&config)
        .unwrap()
        .extract(&patchula_manager::all_read(&files))
        .unwrap()
        .dependencies;
    let mut repo = repo();
    repo.package_files.push(patchula_report::PackageFile {
        path: "Cargo.toml".into(),
        dependencies: vec![patchula_report::Dependency {
            manager: patchula_words::Manager::Cargo,
            name: "tokio".into(),
            occurrence: None,
            package: "tokio".into(),
            registry: None,
            current: patchula_report::Current {
                value: Some("1.39.0".into()),
                version: Some("1.39.0".into()),
                digest: None,
            },
            updates: vec![patchula_report::Update {
                new_value: None,
                new_version: Some("1.40.0".into()),
                new_digest: None,
                update_type: patchula_words::Update::Patch,
                group: None,
                policy: None,
                release_age: None,
                fixes: vec!["GHSA-1".into()],
                image: None,
                cooldown: None,
            }],
            metadata: Some(patchula_report::DependencyMetadata {
                tag_digests: Default::default(),
                source: patchula_words::Source::Crates,
                kinds: vec![patchula_words::DependencyKind::Production],
                locations: Vec::new(),
                held_back: Vec::new(),
                stale: Default::default(),
                lookup: None,
                resolution: None,
                advisories: vec![
                    patchula_report::Advisory {
                        id: "GHSA-1".into(),
                        aliases: vec!["CVE-2026-0001".into()],
                        severity: Some(patchula_words::Severity::High),
                        summary: Some("a flaw".into()),
                        url: "https://osv.dev/GHSA-1".into(),
                        fixed_in: Some("1.40.0".into()),
                    },
                    // An advisory the update does not fix stays off it.
                    patchula_report::Advisory {
                        id: "GHSA-2".into(),
                        aliases: Vec::new(),
                        severity: None,
                        summary: None,
                        url: "https://osv.dev/GHSA-2".into(),
                        fixed_in: None,
                    },
                ],
                image: None,
                credentialed: false,
            }),
        }],
    });
    let plan = crate::plan::batch_plan(
        &repo,
        &config,
        &dependencies,
        &snapshot(&files),
        batched(vec![cargo_change("tokio", "1.39.0", "1.40.0")]),
        false,
        None,
        monday(),
    )
    .unwrap()
    .expect("a batch plan");
    let change = plan
        .changes()
        .iter()
        .find(|change| change.package == "tokio")
        .expect("a tokio change");
    assert_eq!(
        change
            .advisories
            .iter()
            .map(|a| a.id.as_str())
            .collect::<Vec<_>>(),
        ["GHSA-1"]
    );
}

/// A lockfile-only package's group holds its fix alone: routine evaluation
/// never sees the advisory's catalog, so no Major joins the branch whose
/// job could not lock it and fail the fix with it.
#[test]
fn a_grouped_branch_with_a_transitive_fix_plans_only_the_fix() {
    let files = BTreeMap::from([
        (
            "Cargo.toml".to_string(),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nserde = \"1.0.0\"\n"
                .to_string(),
        ),
        (
            "Cargo.lock".to_string(),
            "version = 4\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\"serde\", \"time\"]\n\n[[package]]\nname = \"serde\"\nversion = \"1.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n\n[[package]]\nname = \"time\"\nversion = \"0.3.20\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n"
                .to_string(),
        ),
    ]);
    let config = config();
    let extraction = patchula_manager::Scanner::new(&config)
        .unwrap()
        .extract(&patchula_manager::all_read(&files))
        .unwrap();
    let mut dependencies = extraction.dependencies;
    // The engine pushes a vulnerable lockfile-only package in as a
    // transitive dependency, so its fix can plan a lockfile job.
    dependencies.push(
        extraction
            .locked
            .iter()
            .find(|locked| locked.package == "time")
            .expect("time locked")
            .dependency(),
    );
    let update = |version: &str, fixes: Vec<String>| patchula_report::Update {
        new_value: None,
        new_version: Some(version.into()),
        new_digest: None,
        update_type: patchula_words::Update::Patch,
        group: Some("sec".into()),
        policy: None,
        release_age: None,
        fixes,
        image: None,
        cooldown: None,
    };
    let mut repo = repo();
    repo.package_files.push(patchula_report::PackageFile {
        path: "Cargo.toml".into(),
        dependencies: vec![patchula_report::Dependency {
            manager: patchula_words::Manager::Cargo,
            name: "serde".into(),
            occurrence: None,
            package: "serde".into(),
            registry: None,
            current: patchula_report::Current {
                value: Some("1.0.0".into()),
                version: Some("1.0.0".into()),
                digest: None,
            },
            updates: vec![update("1.0.1", Vec::new())],
            metadata: Some(patchula_report::DependencyMetadata {
                tag_digests: Default::default(),
                source: patchula_words::Source::Crates,
                kinds: vec![patchula_words::DependencyKind::Production],
                locations: Vec::new(),
                held_back: Vec::new(),
                stale: Default::default(),
                lookup: None,
                resolution: None,
                advisories: Vec::new(),
                image: None,
                credentialed: false,
            }),
        }],
    });
    repo.package_files.push(patchula_report::PackageFile {
        path: "Cargo.lock".into(),
        dependencies: vec![patchula_report::Dependency {
            manager: patchula_words::Manager::Cargo,
            name: "time".into(),
            occurrence: None,
            package: "time".into(),
            registry: None,
            current: patchula_report::Current {
                value: Some("0.3.20".into()),
                version: Some("0.3.20".into()),
                digest: None,
            },
            updates: vec![update("0.3.23", vec!["RUSTSEC-1".into()])],
            metadata: Some(patchula_report::DependencyMetadata {
                tag_digests: Default::default(),
                source: patchula_words::Source::Crates,
                kinds: vec![patchula_words::DependencyKind::Transitive],
                locations: Vec::new(),
                held_back: Vec::new(),
                stale: Default::default(),
                lookup: None,
                resolution: None,
                advisories: vec![patchula_report::Advisory {
                    id: "RUSTSEC-1".into(),
                    aliases: Vec::new(),
                    severity: None,
                    summary: None,
                    url: "https://osv.dev/RUSTSEC-1".into(),
                    fixed_in: Some("0.3.23".into()),
                }],
                image: None,
                credentialed: false,
            }),
        }],
    });
    let plans = crate::plan::plan(
        &repo,
        &config,
        &dependencies,
        &snapshot(&files),
        false,
        None,
        monday(),
    )
    .unwrap();
    assert_eq!(plans.len(), 1, "{plans:?}");
    let changes = plans[0].changes();
    assert_eq!(changes.len(), 2, "{changes:?}");
    assert!(
        changes
            .iter()
            .all(|change| change.kind != patchula_words::Update::Major),
        "{changes:?}"
    );
    let fix = changes
        .iter()
        .find(|change| change.package == "time")
        .expect("a time change");
    assert_eq!(fix.version.as_deref(), Some("0.3.23"));
    assert!(fix.lock_only(), "{fix:?}");
    assert_eq!(
        fix.advisories
            .iter()
            .map(|a| a.id.as_str())
            .collect::<Vec<_>>(),
        ["RUSTSEC-1"]
    );
}

/// A target the dependency has already moved past is dropped, never written
/// backwards: node 24.22.2 ticked into the batch after a batch took node to
/// 26.13.0 must not take it back down.
#[test]
fn a_batch_drops_a_target_older_than_the_current_version() {
    let files = BTreeMap::from([(
        "mise.toml".to_string(),
        "[tools]\nnode = \"26.13.0\"\n".to_string(),
    )]);
    let config = config();
    let dependencies = patchula_manager::Scanner::new(&config)
        .unwrap()
        .extract(&patchula_manager::all_read(&files))
        .unwrap()
        .dependencies;
    let target = |version: &str| crate::plan::Change {
        registry: None,
        source: dependencies[0].source,
        package: dependencies[0].package.clone(),
        file: "mise.toml".into(),
        current: "24.21.0".into(),
        current_version: None,
        value: Some(version.into()),
        version: Some(version.into()),
        digest: None,
        kind: patchula_words::Update::Minor,
        merge: false,
        labels: Vec::new(),
        release_notes: false,
        body_notes: Vec::new(),
        advisories: Vec::new(),
        image: None,
    };
    let batch = |version: &str| {
        crate::plan::batch_plan(
            &repo(),
            &config,
            &dependencies,
            &snapshot(&files),
            batched(vec![target(version)]),
            false,
            None,
            monday(),
        )
        .unwrap()
    };
    assert!(batch("24.22.2").is_none());
    assert!(batch("26.13.0").is_none());
    let newer = batch("26.14.0").expect("a newer target is still batched");
    assert!(newer.contents()["mise.toml"].contains("26.14.0"));
}

/// Supersession reads the lock: a lockfile-only target older than the locked
/// release, and a manifest target behind a ranged declaration's locked release,
/// are both dropped.
#[test]
fn a_batch_measures_targets_against_the_locked_release() {
    let lock = |version: &str| {
        format!(
            "version = 4\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\"tokio\"]\n\n[[package]]\nname = \"tokio\"\nversion = \"{version}\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n"
        )
    };
    let files = BTreeMap::from([
        (
            "Cargo.toml".to_string(),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\ntokio = \"^1.40\"\n"
                .to_string(),
        ),
        ("Cargo.lock".to_string(), lock("1.40.0")),
    ]);
    let config = config();
    let dependencies = patchula_manager::Scanner::new(&config)
        .unwrap()
        .extract(&patchula_manager::all_read(&files))
        .unwrap()
        .dependencies;
    assert_eq!(dependencies[0].resolved.as_deref(), Some("1.40.0"));
    let batch = |change: crate::plan::Change| {
        crate::plan::batch_plan(
            &repo(),
            &config,
            &dependencies,
            &snapshot(&files),
            batched(vec![change]),
            false,
            None,
            monday(),
        )
        .unwrap()
    };
    // A lockfile-only target behind the lock.
    assert!(batch(cargo_change("tokio", "^1.40", "1.39.0")).is_none());
    // A ranged declaration names no version; its locked release still counts.
    let mut ranged = cargo_change("tokio", "^1.39", "1.39.0");
    ranged.value = Some("^1.39".into());
    assert!(batch(ranged).is_none());
    // A newer lockfile-only target is still batched.
    assert!(batch(cargo_change("tokio", "^1.40", "1.41.0")).is_some());
}

const NIXPKGS: &str = "1111111111111111111111111111111111111111";
const MOVED: &str = "9999999999999999999999999999999999999999";
/// The inputs `flake.nix` declares for `flake_lock(true)`: `private` is the
/// ssh input the lock already refuses.
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

/// A dependency as extraction would report it, for `job()` dispatch cases.
fn mkdep(manager: Manager, file: &str, kind: patchula_words::DependencyKind) -> Dependency {
    Dependency {
        cooldown: None,
        manager,
        file: file.into(),
        location: patchula_manager::Location {
            start: 0,
            end: 1,
            line: 1,
            column: 1,
            pointer: String::new(),
        },
        form: patchula_manager::EditForm::Scalar(patchula_manager::Syntax::Json),
        name: "dep".into(),
        package: "dep".into(),
        kind,
        current: "1.0.0".into(),
        resolved: Some("1.0.0".into()),
        digest: None,
        source: Source::Npm,
        scheme: patchula_words::Scheme::Semver,
        extract: None,
        included_by: None,
        registry: None,
    }
}
fn update() -> Update {
    Update {
        new_value: Some("1.2.0".into()),
        new_version: Some("1.2.0".into()),
        ..serde_json::from_value(serde_json::json!({"updateType":"minor"})).unwrap()
    }
}

/// `job()` places each manager's lock work on the lockfile it owns — NuGet's
/// beside the project, Gradle's wrapper at its properties file, Go's and
/// Terraform's at the manifest — and refuses or skips what it cannot do.
#[test]
fn jobs_find_each_managers_lockfile_root() {
    let mkfiles = |paths: &[&str]| {
        snapshot(
            &paths
                .iter()
                .map(|p| (p.to_string(), "x".to_string()))
                .collect(),
        )
    };
    // APM's lock sits beside apm.yml; without it there is no work.
    let d = mkdep(
        Manager::Apm,
        "apm.yml",
        patchula_words::DependencyKind::Production,
    );
    assert_eq!(
        job(&d, &update(), &mkfiles(&["apm.yml", "apm.lock.yaml"]))
            .unwrap()
            .map(|j| (j.ecosystem, j.directory)),
        Some((Ecosystem::Apm, String::new()))
    );
    assert!(
        job(&d, &update(), &mkfiles(&["apm.yml"]))
            .unwrap()
            .is_none()
    );
    // NuGet restores the directory holding locks below the manifest.
    let d = mkdep(
        Manager::Nuget,
        "src/app.csproj",
        patchula_words::DependencyKind::Production,
    );
    let locked = job(
        &d,
        &update(),
        &mkfiles(&["src/app.csproj", "src/packages.lock.json"]),
    )
    .unwrap()
    .unwrap();
    assert_eq!(locked.ecosystem, Ecosystem::Dotnet);
    assert_eq!(locked.directory, "src");
    assert_eq!(locked.package(), None, "a directory job moves no package");
    for snap in [
        mkfiles(&["src/app.csproj"]),
        mkfiles(&["src/app.csproj", "obj/project.assets.json"]),
    ] {
        assert!(job(&d, &update(), &snap).unwrap().is_none(), "{snap:?}");
    }
    // Tool manifests and dotnet's local tool manifest never produce a job.
    let toolchain = mkdep(
        Manager::Nuget,
        "src/app.csproj",
        patchula_words::DependencyKind::Toolchain,
    );
    let snap = mkfiles(&["src/app.csproj", "src/packages.lock.json"]);
    assert!(job(&toolchain, &update(), &snap).unwrap().is_none());
    let tools = mkdep(
        Manager::Nuget,
        "src/dotnet-tools.json",
        patchula_words::DependencyKind::Production,
    );
    assert!(job(&tools, &update(), &snap).unwrap().is_none());
    // The wrapper properties file is Gradle's only regenerable work.
    let d = mkdep(
        Manager::Gradle,
        "gradle/wrapper/gradle-wrapper.properties",
        patchula_words::DependencyKind::Production,
    );
    let wrapper = job(
        &d,
        &update(),
        &mkfiles(&["gradle/wrapper/gradle-wrapper.properties"]),
    )
    .unwrap()
    .unwrap();
    assert_eq!(wrapper.ecosystem, Ecosystem::GradleWrapper);
    assert_eq!(wrapper.directory, "gradle/wrapper");
    // Dependency locking needs the build itself; a committed gradle.lockfile
    // holds the update instead of planning a job.
    let d = mkdep(
        Manager::Gradle,
        "app/build.gradle.kts",
        patchula_words::DependencyKind::Production,
    );
    let error = job(
        &d,
        &update(),
        &mkfiles(&["app/build.gradle.kts", "gradle.lockfile"]),
    )
    .unwrap_err();
    assert!(error.contains("unsupported lockfile format"), "{error}");
    assert!(
        job(&d, &update(), &mkfiles(&["app/build.gradle.kts"]))
            .unwrap()
            .is_none()
    );
    // Go and Terraform modules lock beside their manifests.
    let d = mkdep(
        Manager::Go,
        "go.mod",
        patchula_words::DependencyKind::Production,
    );
    let jobbed = job(&d, &update(), &mkfiles(&["go.mod"])).unwrap().unwrap();
    assert_eq!(
        (jobbed.ecosystem, jobbed.directory.as_str()),
        (Ecosystem::Go, "")
    );
    let mut d = mkdep(
        Manager::Terraform,
        "main.tf",
        patchula_words::DependencyKind::Production,
    );
    d.source = Source::Terraform;
    let jobbed = job(&d, &update(), &mkfiles(&["main.tf"])).unwrap().unwrap();
    assert_eq!(
        (jobbed.ecosystem, jobbed.directory.as_str()),
        (Ecosystem::Terraform, "")
    );
    d.source = Source::Npm;
    assert!(
        job(&d, &update(), &mkfiles(&["main.tf"]))
            .unwrap()
            .is_none()
    );
}

/// A job serialized without a package is directory-scoped, as the wire shape
/// reads it.
#[test]
fn a_job_without_a_package_deserializes_as_directory_scoped() {
    let job: Job = serde_json::from_value(serde_json::json!({
        "ecosystem": "Node", "directory": "web",
        "package": null, "current": null, "target": null, "lock_only": false,
        "publish_age_allow": false, "wait_hours": 0,
    }))
    .unwrap();
    assert_eq!(job.scope, Scope::Directory);
    assert_eq!(job.package(), None);
    assert_eq!(job.current(), None);
}

mod goldens {
    //! Frozen bytes the lockfile split must not move: `Job`'s serde shape and
    //! `BTreeSet` order, the built-plan fingerprint recipe, the maintenance
    //! fingerprint and `update_key` that reach the PR ownership marker, and
    //! the report reasons `job()` errors and generation failures produce.
    use super::*;

    /// `Job` serializes every ecosystem and both scope shapes as the flat
    /// wire layout it always had, and reads back the same.
    #[test]
    fn job_serde_is_frozen_for_every_ecosystem_and_scope() {
        const ECOSYSTEMS: &[(Ecosystem, &str)] = &[
            (Ecosystem::Node, "Node"),
            (Ecosystem::Cargo, "Cargo"),
            (Ecosystem::Go, "Go"),
            (Ecosystem::Terraform, "Terraform"),
            (Ecosystem::Mise, "Mise"),
            (Ecosystem::Python, "Python"),
            (Ecosystem::GradleWrapper, "GradleWrapper"),
            (Ecosystem::Dotnet, "Dotnet"),
            (Ecosystem::Ruby, "Ruby"),
            (Ecosystem::Php, "Php"),
            (Ecosystem::Nix, "Nix"),
            (Ecosystem::Apm, "Apm"),
        ];
        for &(ecosystem, name) in ECOSYSTEMS {
            let directory = Job {
                ecosystem,
                directory: "web".into(),
                scope: Scope::Directory,
                exact_version: false,
                publish_age_allow: false,
                wait_hours: 0,
            };
            let json = serde_json::to_string(&directory).unwrap();
            assert_eq!(
                json,
                format!(
                    r#"{{"ecosystem":"{name}","directory":"web","package":null,"current":null,"target":null,"lock_only":false,"publish_age_allow":false,"wait_hours":0}}"#
                ),
                "{name} directory job"
            );
            assert_eq!(
                serde_json::from_str::<Job>(&json).unwrap(),
                directory,
                "{name} round-trip"
            );
            let targeted = Job {
                ecosystem,
                directory: "web".into(),
                scope: Scope::Package {
                    package: "left-pad".into(),
                    current: Some("1.0.0".into()),
                    target: Some("1.2.0".into()),
                },
                exact_version: true,
                publish_age_allow: false,
                wait_hours: 0,
            };
            let json = serde_json::to_string(&targeted).unwrap();
            assert_eq!(
                json,
                format!(
                    r#"{{"ecosystem":"{name}","directory":"web","package":"left-pad","current":"1.0.0","target":"1.2.0","lock_only":true,"publish_age_allow":false,"wait_hours":0}}"#
                ),
                "{name} package job"
            );
            assert_eq!(serde_json::from_str::<Job>(&json).unwrap(), targeted);
        }
        // The package scope's optional fields serialize as null, and a
        // directory job honors `exact_version`.
        for (job, expected) in [
            (
                Job {
                    ecosystem: Ecosystem::Node,
                    directory: String::new(),
                    scope: Scope::Package {
                        package: "p".into(),
                        current: None,
                        target: None,
                    },
                    exact_version: false,
                    publish_age_allow: false,
                    wait_hours: 0,
                },
                r#"{"ecosystem":"Node","directory":"","package":"p","current":null,"target":null,"lock_only":false,"publish_age_allow":false,"wait_hours":0}"#,
            ),
            (
                Job {
                    ecosystem: Ecosystem::Node,
                    directory: String::new(),
                    scope: Scope::Package {
                        package: "p".into(),
                        current: Some("1.0.0".into()),
                        target: None,
                    },
                    exact_version: false,
                    publish_age_allow: false,
                    wait_hours: 0,
                },
                r#"{"ecosystem":"Node","directory":"","package":"p","current":"1.0.0","target":null,"lock_only":false,"publish_age_allow":false,"wait_hours":0}"#,
            ),
            (
                Job {
                    ecosystem: Ecosystem::Node,
                    directory: String::new(),
                    scope: Scope::Package {
                        package: "p".into(),
                        current: None,
                        target: Some("2.0.0".into()),
                    },
                    exact_version: false,
                    publish_age_allow: false,
                    wait_hours: 0,
                },
                r#"{"ecosystem":"Node","directory":"","package":"p","current":null,"target":"2.0.0","lock_only":false,"publish_age_allow":false,"wait_hours":0}"#,
            ),
            (
                Job {
                    ecosystem: Ecosystem::Node,
                    directory: String::new(),
                    scope: Scope::Directory,
                    exact_version: true,
                    publish_age_allow: false,
                    wait_hours: 0,
                },
                r#"{"ecosystem":"Node","directory":"","package":null,"current":null,"target":null,"lock_only":true,"publish_age_allow":false,"wait_hours":0}"#,
            ),
        ] {
            assert_eq!(serde_json::to_string(&job).unwrap(), expected);
            assert_eq!(serde_json::from_str::<Job>(expected).unwrap(), job);
        }
    }

    /// The derived `Ord` orders a mixed job set by ecosystem, directory,
    /// scope then `exact_version`; the serialized set keeps that order.
    #[test]
    fn a_job_set_serializes_in_ord_order() {
        let jobs = [
            Job {
                ecosystem: Ecosystem::Apm,
                directory: "b".into(),
                scope: Scope::Directory,
                exact_version: false,
                publish_age_allow: false,
                wait_hours: 0,
            },
            Job {
                ecosystem: Ecosystem::Cargo,
                directory: String::new(),
                scope: Scope::Package {
                    package: "z".into(),
                    current: None,
                    target: Some("2.0.0".into()),
                },
                exact_version: true,
                publish_age_allow: false,
                wait_hours: 0,
            },
            Job {
                ecosystem: Ecosystem::Node,
                directory: String::new(),
                scope: Scope::Directory,
                exact_version: false,
                publish_age_allow: false,
                wait_hours: 0,
            },
            Job {
                ecosystem: Ecosystem::Cargo,
                directory: String::new(),
                scope: Scope::Package {
                    package: "a".into(),
                    current: Some("1.0.0".into()),
                    target: Some("2.0.0".into()),
                },
                exact_version: false,
                publish_age_allow: false,
                wait_hours: 0,
            },
            Job {
                ecosystem: Ecosystem::Cargo,
                directory: "sub".into(),
                scope: Scope::Directory,
                exact_version: false,
                publish_age_allow: false,
                wait_hours: 0,
            },
            Job {
                ecosystem: Ecosystem::Nix,
                directory: "f".into(),
                scope: Scope::Package {
                    package: "nixpkgs".into(),
                    current: Some("old".into()),
                    target: Some("new".into()),
                },
                exact_version: false,
                publish_age_allow: false,
                wait_hours: 0,
            },
            Job {
                ecosystem: Ecosystem::Php,
                directory: String::new(),
                scope: Scope::Directory,
                exact_version: true,
                publish_age_allow: false,
                wait_hours: 0,
            },
        ];
        let set: BTreeSet<Job> = jobs.into_iter().collect();
        assert_eq!(
            serde_json::to_string(&set).unwrap(),
            r#"[{"ecosystem":"Node","directory":"","package":null,"current":null,"target":null,"lock_only":false,"publish_age_allow":false,"wait_hours":0},{"ecosystem":"Cargo","directory":"","package":"a","current":"1.0.0","target":"2.0.0","lock_only":false,"publish_age_allow":false,"wait_hours":0},{"ecosystem":"Cargo","directory":"","package":"z","current":null,"target":"2.0.0","lock_only":true,"publish_age_allow":false,"wait_hours":0},{"ecosystem":"Cargo","directory":"sub","package":null,"current":null,"target":null,"lock_only":false,"publish_age_allow":false,"wait_hours":0},{"ecosystem":"Php","directory":"","package":null,"current":null,"target":null,"lock_only":true,"publish_age_allow":false,"wait_hours":0},{"ecosystem":"Nix","directory":"f","package":"nixpkgs","current":"old","target":"new","lock_only":false,"publish_age_allow":false,"wait_hours":0},{"ecosystem":"Apm","directory":"b","package":null,"current":null,"target":null,"lock_only":false,"publish_age_allow":false,"wait_hours":0}]"#
        );
    }

    /// The built-stage fingerprint hashes contents, message, the pending
    /// flag and the serialized jobs; the jobs inside it are `Job` serde.
    #[test]
    fn the_built_plan_fingerprint_is_frozen() {
        let contents = BTreeMap::from([("Cargo.toml".to_owned(), "new".to_owned())]);
        let jobs = [
            Job {
                ecosystem: Ecosystem::Cargo,
                directory: String::new(),
                scope: Scope::Package {
                    package: "tokio".into(),
                    current: Some("1.39.0".into()),
                    target: Some("1.40.0".into()),
                },
                exact_version: true,
                publish_age_allow: false,
                wait_hours: 0,
            },
            Job {
                ecosystem: Ecosystem::Node,
                directory: String::new(),
                scope: Scope::Directory,
                exact_version: false,
                publish_age_allow: false,
                wait_hours: 0,
            },
        ];
        let built = fingerprint(FingerprintStage::Built {
            contents: &contents,
            message: "chore(deps): update tokio to 1.40.0",
            lockfile_pending: false,
            lock_jobs: &jobs,
        });
        assert_eq!(
            built,
            "4205caa57f71901465d28ea1747d5eb379e3095bed2b2e66db0e9848c119cc9b"
        );
    }

    /// The maintenance plan's fingerprint covers the ISO week, the claimed
    /// job set and the lockfiles config; `update_key` hashes it into the PR
    /// ownership marker. Both are pinned for fixed inputs.
    #[test]
    fn the_maintenance_fingerprint_and_update_key_are_frozen() {
        let files = BTreeMap::from([
            (
                "package.json".to_string(),
                "{\"name\":\"app\"}\n".to_string(),
            ),
            ("package-lock.json".to_string(), "{}\n".to_string()),
            ("Cargo.lock".to_string(), "version = 4\n".to_string()),
            (
                "Gemfile".to_string(),
                "source \"https://rubygems.org\"\n".to_string(),
            ),
            ("Gemfile.lock".to_string(), "GEM\n".to_string()),
            ("composer.json".to_string(), "{}\n".to_string()),
            ("composer.lock".to_string(), "{}\n".to_string()),
            (
                "pyproject.toml".to_string(),
                "[project]\nname = \"app\"\n".to_string(),
            ),
            ("poetry.lock".to_string(), "[[package]]\n".to_string()),
            ("go.mod".to_string(), "module example.com/app\n".to_string()),
            ("go.sum".to_string(), "\n".to_string()),
            ("flake.nix".to_string(), "{ }\n".to_string()),
            ("flake.lock".to_string(), "{}\n".to_string()),
            ("mise.toml".to_string(), "[tools]\n".to_string()),
            ("mise.lock".to_string(), "{}\n".to_string()),
            ("apm.yml".to_string(), "name: app\n".to_string()),
            (
                "apm.lock.yaml".to_string(),
                "lockfile_version: '1'\n".to_string(),
            ),
            ("app.csproj".to_string(), "<Project/>\n".to_string()),
            ("packages.lock.json".to_string(), "{}\n".to_string()),
            ("main.tf".to_string(), "terraform {}\n".to_string()),
            (".terraform.lock.hcl".to_string(), "# lock\n".to_string()),
        ]);
        let plan = maintenance(&repo(), &config(), &snapshot(&files), &[], None, monday())
            .unwrap()
            .unwrap();
        assert!(plan.is_maintenance());
        assert_eq!(
            plan.fingerprint(),
            "d837ffb0e7ca52d4f497dd5a5d658c20a590c5530360cabfbc9083ec109589ff"
        );
        assert_eq!(plan.input_fingerprint(), plan.fingerprint());
        assert_eq!(
            plan.identity(),
            "066510027b40697776d9e4eddb977f9302a2e648de2f0d4c8371579d1b1f94f4"
        );
        assert_eq!(
            plan.update_key(),
            "d7687789d9073a6db85865a1a4ad7d4c4e4344745ae08b40f7d39cf4cb88d8fc"
        );
    }

    /// An update plan's `update_key` — the hash the PR ownership marker
    /// carries for the branch and its targets — is pinned for fixed inputs.
    #[test]
    fn an_update_plans_update_key_is_frozen() {
        let config = config();
        let evaluator = patchula_lookup::Evaluator::new(&config, Default::default()).unwrap();
        let files = BTreeMap::from([(
            "package.json".to_string(),
            "{\"dependencies\":{\"dep\":\"1.0.0\"}}\n".to_string(),
        )]);
        let dep = mkdep(
            Manager::Npm,
            "package.json",
            patchula_words::DependencyKind::Production,
        );
        let update = update();
        let plan = build(
            &repo(),
            &config,
            &evaluator,
            &snapshot(&files),
            false,
            None,
            monday(),
            "golden",
            "patchula/golden".into(),
            &[Item {
                dep: &dep,
                update: &update,
                current_version: None,
                advisories: &[],
            }],
            Kind::Update,
        )
        .unwrap();
        assert_eq!(
            plan.update_key(),
            "15b8ee4e4771104763996d169c7b732b4a162bf1fa7fc90f5c7737972d3872f3"
        );
    }

    /// The batch plan's fingerprint — the built-stage hash the trailer and
    /// the write receipts carry — and its `update_key` are pinned for fixed
    /// inputs.
    #[test]
    fn a_batch_plans_fingerprint_and_update_key_are_frozen() {
        let workflow = "on: push\njobs:\n  a:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v4.1.0\n";
        let files =
            BTreeMap::from([(".github/workflows/ci.yml".to_string(), workflow.to_string())]);
        let config = config();
        let dependencies = patchula_manager::Scanner::new(&config)
            .unwrap()
            .extract(&patchula_manager::all_read(&files))
            .unwrap()
            .dependencies
            .into_iter()
            .filter(|d| d.package == "actions/checkout")
            .collect::<Vec<_>>();
        let target = crate::plan::Change {
            registry: None,
            source: dependencies[0].source,
            package: dependencies[0].package.clone(),
            file: ".github/workflows/ci.yml".into(),
            current: dependencies[0].current.clone(),
            current_version: None,
            value: Some("v4.2.0".into()),
            version: Some("4.2.0".into()),
            digest: None,
            kind: patchula_words::Update::Minor,
            merge: false,
            labels: Vec::new(),
            release_notes: false,
            body_notes: Vec::new(),
            advisories: Vec::new(),
            image: None,
        };
        let plan = crate::plan::batch_plan(
            &repo(),
            &config,
            &dependencies,
            &snapshot(&files),
            batched(vec![target]),
            false,
            None,
            monday(),
        )
        .unwrap()
        .expect("a batch plan");
        assert_eq!(
            plan.fingerprint(),
            "7b4481e59fd80abacc94f9b8e9d0b1b9ebd8c3bf8a67730a9565d72d36389f34"
        );
        assert_eq!(
            plan.update_key(),
            "e6336e6849029ee6bc95f8f5ce7751b009895228b45b809be41efab34a8cb9f8"
        );
    }

    /// Every `job()` error reaches the report as the pending reason the
    /// branch is held with, naming the lockfile it could not handle.
    #[test]
    fn lockfile_job_errors_reach_the_report_pending() {
        let config = config();
        let evaluator = patchula_lookup::Evaluator::new(&config, Default::default()).unwrap();
        let update = Update {
            cooldown: None,
            new_value: None,
            new_version: Some("2.0.0".into()),
            new_digest: None,
            update_type: UpdateKind::Minor,
            group: None,
            policy: None,
            release_age: None,
            fixes: vec![],
            image: None,
        };
        let plan_for = |files: &BTreeMap<String, String>, dep: &Dependency| {
            build(
                &repo(),
                &config,
                &evaluator,
                &snapshot(files),
                false,
                None,
                monday(),
                "golden",
                "patchula/golden".into(),
                &[Item {
                    dep,
                    update: &update,
                    current_version: None,
                    advisories: &[],
                }],
                Kind::Update,
            )
            .unwrap()
        };
        for (files, dep, error) in [
            (
                BTreeMap::from([
                    ("package.json".to_string(), "{}\n".to_string()),
                    ("bun.lockb".to_string(), "x".to_string()),
                ]),
                mkdep(
                    Manager::Npm,
                    "package.json",
                    patchula_words::DependencyKind::Production,
                ),
                "unsupported lockfile format",
            ),
            (
                BTreeMap::from([
                    ("Chart.yaml".to_string(), "dependencies: []\n".to_string()),
                    ("Chart.lock".to_string(), "dependencies: []\n".to_string()),
                ]),
                mkdep(
                    Manager::Helm,
                    "Chart.yaml",
                    patchula_words::DependencyKind::Production,
                ),
                "unsupported lockfile format: Chart.lock",
            ),
            (
                BTreeMap::from([
                    (
                        "app/build.gradle.kts".to_string(),
                        "plugins {}\n".to_string(),
                    ),
                    ("gradle.lockfile".to_string(), "# lock\n".to_string()),
                ]),
                mkdep(
                    Manager::Gradle,
                    "app/build.gradle.kts",
                    patchula_words::DependencyKind::Production,
                ),
                "unsupported lockfile format: gradle.lockfile",
            ),
            (
                BTreeMap::from([
                    (
                        "pyproject.toml".to_string(),
                        "[project]\nname = 'app'\n".to_string(),
                    ),
                    ("pdm.lock".to_string(), "# lock\n".to_string()),
                ]),
                mkdep(
                    Manager::Pyproject,
                    "pyproject.toml",
                    patchula_words::DependencyKind::Production,
                ),
                "unsupported lockfile format: pdm.lock",
            ),
        ] {
            let plan = plan_for(&files, &dep);
            let expected = format!(
                "lockfile pending: {error}; --allow-lockfile-pending permits manifest-only testing"
            );
            assert_eq!(plan.action().status, BranchStatus::Skipped, "{error}");
            assert_eq!(
                plan.action().reason.as_deref(),
                Some(expected.as_str()),
                "{error}"
            );
            assert!(plan.action().lockfile_pending, "{error}");
        }
    }

    /// An in-repository composite is planned against actions.lock from the
    /// contents: an Actions job when a listed workflow calls it through
    /// `$/`, none for a `./` composite, and a hold naming the cause when a
    /// listed workflow cannot be read to tell.
    #[test]
    fn a_composite_bump_plans_the_actions_job_only_for_self_repository_callers() {
        let config = config();
        let evaluator = patchula_lookup::Evaluator::new(&config, Default::default()).unwrap();
        let update = Update {
            cooldown: None,
            new_value: Some("v6.1.0".into()),
            new_version: Some("v6.1.0".into()),
            new_digest: Some("b".repeat(40)),
            update_type: UpdateKind::Minor,
            group: None,
            policy: None,
            release_age: None,
            fixes: vec![],
            image: None,
        };
        let plan_for = |workflow: &str, extra: Option<(&str, &str)>| {
            let mut files = BTreeMap::from([
                (
                    ".github/workflows/ci.yml".to_string(),
                    format!("on: push\njobs:\n  t:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: {workflow}\n"),
                ),
                (
                    ".github/actions/setup/action.yml".to_string(),
                    format!("name: setup\nruns:\n  using: composite\n  steps:\n    - uses: actions/setup-node@{} # v6.0.0\n", "a".repeat(40)),
                ),
                (
                    ".github/workflows/actions.lock".to_string(),
                    "version: 'v0.0.3'\nworkflows:\n    '.github/workflows/ci.yml': []\n    '.github/workflows/other.yml': []\ndependencies: {}\n".to_string(),
                ),
            ]);
            if let Some((path, text)) = extra {
                files.insert(path.into(), text.into());
            }
            let dep = patchula_manager::Scanner::new(&config)
                .unwrap()
                .extract(&patchula_manager::all_read(&files))
                .unwrap()
                .dependencies
                .into_iter()
                .find(|dep| dep.package == "actions/setup-node")
                .unwrap();
            build(
                &repo(),
                &config,
                &evaluator,
                &snapshot(&files),
                false,
                None,
                monday(),
                "golden",
                "patchula/golden".into(),
                &[Item {
                    dep: &dep,
                    update: &update,
                    current_version: None,
                    advisories: &[],
                }],
                Kind::Update,
            )
            .unwrap()
        };
        let called = plan_for("$/.github/actions/setup", None);
        assert_eq!(
            called.action().status,
            BranchStatus::Planned,
            "{:?}",
            called.action()
        );
        assert_eq!(
            called
                .lock_jobs()
                .iter()
                .map(|job| job.ecosystem)
                .collect::<Vec<_>>(),
            [Ecosystem::Actions]
        );
        let local = plan_for("./.github/actions/setup", None);
        assert_eq!(
            local.action().status,
            BranchStatus::Planned,
            "{:?}",
            local.action()
        );
        assert!(local.lock_jobs().is_empty());
        let unknown = plan_for(
            "./.github/actions/setup",
            Some((".github/workflows/other.yml", "jobs: [\n")),
        );
        assert_ne!(unknown.action().status, BranchStatus::Planned);
        assert!(
            unknown
                .action()
                .reason
                .as_deref()
                .is_some_and(|r| r.contains("other.yml does not parse")),
            "{:?}",
            unknown.action()
        );
    }

    /// A failed generation reports `lockfile generation failed: …` with the
    /// tool's context or the plain reason, whichever `Error` carries.
    #[test]
    fn a_lockfile_generation_failure_reason_is_frozen() {
        let action = || BranchAction {
            repository: "demo/app".into(),
            branch: "patchula/x".into(),
            base_commit: "a".repeat(40),
            status: BranchStatus::Planned,
            reason: None,
            message: "m".into(),
            previous_commit: None,
            commit: None,
            verified: None,
            lockfile_pending: false,
            files: Vec::new(),
        };
        let mut plan = testing::Draft::new(action()).build();
        plan.lockfiles_failed(patchula_lockfiles::Failure::Text(
            "the fixture failed".into(),
        ));
        assert_eq!(plan.action().status, BranchStatus::Error);
        assert_eq!(
            plan.action().reason.as_deref(),
            Some("lockfile generation failed: the fixture failed")
        );
        let mut plan = testing::Draft::new(action()).build();
        plan.lockfiles_failed(patchula_lockfiles::Failure::Tool {
            context: "cargo".into(),
            failure: patchula_process::Failure::Io("disk full".into()),
        });
        assert_eq!(
            plan.action().reason.as_deref(),
            Some("lockfile generation failed: cargo: disk full")
        );
    }
}
