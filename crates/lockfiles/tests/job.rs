//! `job()`'s dispatch names a manager on every arm and guard; these cases
//! pin each one's answer — the work it yields, the formats it refuses, and
//! the declarations no lockfile records. `Refresh::claims` applies the same
//! names to maintenance, so it is covered here too.
use patchula_lockfiles::{Ecosystem, Job, Scope, job, refresh};
use patchula_manager::{Dependency, EditForm, Location, Syntax};
use patchula_report::Update;
use patchula_words::{DependencyKind, Manager, Scheme, Source, Update as UpdateKind};
use std::collections::BTreeSet;

fn dependency(manager: Manager, file: &str) -> Dependency {
    Dependency {
        manager,
        file: file.into(),
        location: Location {
            start: 0,
            end: 1,
            line: 1,
            column: 1,
            pointer: String::new(),
        },
        form: EditForm::Scalar(Syntax::Bare),
        name: "pkg".into(),
        package: "pkg".into(),
        kind: DependencyKind::Production,
        current: "1.0.0".into(),
        resolved: Some("1.0.0".into()),
        digest: None,
        source: Source::Npm,
        scheme: Scheme::Semver,
        extract: None,
        included_by: None,
        registry: None,
        cooldown: None,
    }
}

fn update() -> Update {
    Update {
        new_value: Some("2.0.0".into()),
        new_version: Some("2.0.0".into()),
        new_digest: None,
        update_type: UpdateKind::Minor,
        group: None,
        policy: None,
        release_age: None,
        fixes: vec![],
        image: None,
        cooldown: None,
    }
}

fn paths(paths: &[&str]) -> BTreeSet<String> {
    paths.iter().map(|p| (*p).to_owned()).collect()
}

#[test]
fn a_mise_tool_is_locked_by_a_mise_lock_at_its_config_root() {
    let dep = dependency(Manager::Mise, "tools/mise.toml");
    assert_eq!(job(&dep, &update(), &paths(&[])).unwrap(), None);
    assert_eq!(
        job(&dep, &update(), &paths(&["tools/mise.lock"])).unwrap(),
        Some(Job {
            ecosystem: Ecosystem::Mise,
            directory: "tools".into(),
            scope: Scope::Package {
                package: "pkg".into(),
                current: Some("1.0.0".into()),
                target: Some("2.0.0".into()),
            },
            exact_version: false,
            publish_age_allow: false,
            wait_hours: 0,
        })
    );
}

#[test]
fn node_version_files_have_no_lock_work_but_manifests_do() {
    for file in [".nvmrc", "app/.node-version"] {
        for manager in [Manager::Npm, Manager::Bun] {
            let dep = dependency(manager, file);
            assert_eq!(
                job(&dep, &update(), &paths(&["app/package-lock.json"])).unwrap(),
                None,
                "{manager:?} {file}"
            );
        }
    }
    let dep = dependency(Manager::Npm, "app/package.json");
    let j = job(&dep, &update(), &paths(&["app/package-lock.json"]))
        .unwrap()
        .unwrap();
    assert_eq!(
        (j.ecosystem, j.directory.as_str()),
        (Ecosystem::Node, "app")
    );
    let dep = dependency(Manager::Npm, "app/package.json");
    assert_eq!(
        job(&dep, &update(), &paths(&["app/bun.lockb"])),
        Err("unsupported lockfile format".into())
    );
}

#[test]
fn a_helm_chart_lock_is_an_unsupported_format() {
    let dep = dependency(Manager::Helm, "charts/app/Chart.yaml");
    assert_eq!(job(&dep, &update(), &paths(&[])).unwrap(), None);
    assert_eq!(
        job(&dep, &update(), &paths(&["charts/app/Chart.lock"])),
        Err("unsupported lockfile format: Chart.lock".into())
    );
}

#[test]
fn an_apm_lock_sits_beside_its_manifest() {
    let dep = dependency(Manager::Apm, "tools/apm.yml");
    assert_eq!(job(&dep, &update(), &paths(&[])).unwrap(), None);
    let j = job(&dep, &update(), &paths(&["tools/apm.lock.yaml"]))
        .unwrap()
        .unwrap();
    assert_eq!(j.ecosystem, Ecosystem::Apm);
    assert_eq!(j.directory, "tools");
    let dep = dependency(Manager::Apm, "tools/not-a-manifest.txt");
    assert_eq!(
        job(&dep, &update(), &paths(&["tools/apm.lock.yaml"])).unwrap(),
        None
    );
}

#[test]
fn a_bundler_toolchain_pin_has_no_lock_work() {
    let mut dep = dependency(Manager::Bundler, ".ruby-version");
    dep.kind = DependencyKind::Toolchain;
    assert_eq!(
        job(&dep, &update(), &paths(&["Gemfile.lock"])).unwrap(),
        None
    );
    let dep = dependency(Manager::Bundler, "Gemfile");
    assert!(
        job(&dep, &update(), &paths(&["Gemfile.lock"]))
            .unwrap()
            .is_some()
    );
}

#[test]
fn composer_nuget_and_cargo_locks_claim_their_directories() {
    let dep = dependency(Manager::Composer, "app/composer.json");
    assert_eq!(
        job(&dep, &update(), &paths(&["app/composer.lock"]))
            .unwrap()
            .map(|j| (j.ecosystem, j.directory)),
        Some((Ecosystem::Php, "app".into()))
    );
    let dep = dependency(Manager::Nuget, "src/app/app.csproj");
    assert_eq!(
        job(&dep, &update(), &paths(&["src/app/packages.lock.json"]))
            .unwrap()
            .map(|j| (j.ecosystem, j.directory, j.scope)),
        Some((Ecosystem::Dotnet, "src/app".into(), Scope::Directory))
    );
    // A toolchain pin and a tool-manifest entry record nothing a lock moves.
    let mut dep = dependency(Manager::Nuget, "src/app/app.csproj");
    dep.kind = DependencyKind::Toolchain;
    assert_eq!(
        job(&dep, &update(), &paths(&["src/app/packages.lock.json"])).unwrap(),
        None
    );
    let dep = dependency(Manager::Nuget, "src/app/dotnet-tools.json");
    assert_eq!(
        job(&dep, &update(), &paths(&["src/app/packages.lock.json"])).unwrap(),
        None
    );
    let dep = dependency(Manager::Cargo, "rust-toolchain");
    assert_eq!(job(&dep, &update(), &paths(&["Cargo.lock"])).unwrap(), None);
    let dep = dependency(Manager::Cargo, "rust/uv/Cargo.toml");
    assert_eq!(
        job(&dep, &update(), &paths(&["rust/uv/Cargo.lock"]))
            .unwrap()
            .map(|j| (j.ecosystem, j.directory)),
        Some((Ecosystem::Cargo, "rust/uv".into()))
    );
}

#[test]
fn gradle_lockfiles_are_unsupported_but_the_wrapper_is_a_job() {
    let dep = dependency(Manager::Gradle, "app/build.gradle");
    assert_eq!(job(&dep, &update(), &paths(&[])).unwrap(), None);
    assert_eq!(
        job(&dep, &update(), &paths(&["app/gradle.lockfile"])),
        Err("unsupported lockfile format: gradle.lockfile".into())
    );
    let dep = dependency(Manager::Gradle, "gradle/wrapper/gradle-wrapper.properties");
    assert_eq!(
        job(&dep, &update(), &paths(&[]))
            .unwrap()
            .map(|j| j.ecosystem),
        Some(Ecosystem::GradleWrapper)
    );
}

#[test]
fn python_version_files_have_no_lock_work_and_unsupported_locks_hold() {
    let dep = dependency(Manager::Pyproject, ".python-version");
    assert_eq!(job(&dep, &update(), &paths(&["uv.lock"])).unwrap(), None);
    let dep = dependency(Manager::Pyproject, "app/pyproject.toml");
    assert_eq!(
        job(&dep, &update(), &paths(&["app/uv.lock"]))
            .unwrap()
            .map(|j| (j.ecosystem, j.directory)),
        Some((Ecosystem::Python, "app".into()))
    );
    assert_eq!(
        job(&dep, &update(), &paths(&["app/pdm.lock"])),
        Err("unsupported lockfile format: pdm.lock".into())
    );
    // An unsupported lock above the supported one does not hold the update.
    let dep = dependency(Manager::Pyproject, "app/sub/pyproject.toml");
    assert_eq!(
        job(
            &dep,
            &update(),
            &paths(&["app/pdm.lock", "app/sub/uv.lock"])
        )
        .unwrap()
        .map(|j| j.directory),
        Some("app/sub".into())
    );
}

#[test]
fn terraform_registry_providers_lock_but_release_sourced_tools_do_not() {
    let mut dep = dependency(Manager::Terraform, "main.tf");
    dep.source = Source::Terraform;
    assert!(job(&dep, &update(), &paths(&[])).unwrap().is_some());
    dep.source = Source::GithubReleases;
    assert_eq!(job(&dep, &update(), &paths(&[])).unwrap(), None);
}

#[test]
fn a_flake_input_is_moved_by_name() {
    let mut dep = dependency(Manager::Nix, "flake.nix");
    dep.name = "flake-parts".into();
    dep.source = Source::Nix;
    dep.digest = Some("a".repeat(64));
    let mut update = update();
    update.new_digest = Some("b".repeat(64));
    assert_eq!(
        job(&dep, &update, &paths(&[])).unwrap(),
        Some(Job {
            ecosystem: Ecosystem::Nix,
            directory: "".into(),
            scope: Scope::Package {
                package: "flake-parts".into(),
                current: Some("a".repeat(64)),
                target: Some("b".repeat(64)),
            },
            exact_version: false,
            publish_age_allow: false,
            wait_hours: 0,
        })
    );
}

#[test]
fn refresh_claims_locks_and_manifests_only_when_the_manager_is_enabled() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus");
    let mut config = patchula_config::load(&root).unwrap();
    config.managers = vec![Manager::Cargo];
    let paths = paths(&["app/flake.nix"]);
    // Locks: a supported lockfile claims only while its manager is enabled.
    let cargo = refresh(Ecosystem::Cargo);
    assert!(cargo.claims(&config, "Cargo.lock", "app", &paths));
    config.managers = vec![Manager::Go];
    assert!(!cargo.claims(&config, "Cargo.lock", "app", &paths));
    // Manifests: the manifest itself is the work.
    assert!(refresh(Ecosystem::Go).claims(&config, "go.mod", "app", &paths));
    assert!(!refresh(Ecosystem::Go).claims(&config, "go.sum", "app", &paths));
    // LocksBesideManifest: a lock claims only beside its manifest.
    config.managers = vec![Manager::Nix];
    let nix = refresh(Ecosystem::Nix);
    assert!(nix.claims(&config, "flake.lock", "app", &paths));
    assert!(!nix.claims(&config, "flake.lock", "other", &paths));
    assert!(!nix.claims(&config, "flake.nix", "app", &paths));
    assert!(!refresh(Ecosystem::GradleWrapper).claims(
        &config,
        "gradle-wrapper.properties",
        "app",
        &paths
    ));
    // Path: actions.lock claims maintenance at its one committed path,
    // while the Actions manager is enabled; the same leaf elsewhere is not
    // the lock.
    config.managers = vec![Manager::GithubActions];
    let actions = refresh(Ecosystem::Actions);
    assert!(actions.claims(&config, "actions.lock", ".github/workflows", &paths));
    assert!(!actions.claims(&config, "actions.lock", "app", &paths));
    assert!(!actions.claims(&config, "actions.lock", ".forgejo/workflows", &paths));
    let jobs = patchula_lockfiles::refresh_jobs(
        &config,
        &[patchula_lockfiles::ACTIONS_LOCK.to_owned()].into(),
    );
    assert_eq!(
        jobs.into_iter().collect::<Vec<_>>(),
        [Job {
            ecosystem: Ecosystem::Actions,
            directory: ".github/workflows".into(),
            scope: Scope::Directory,
            exact_version: false,
            publish_age_allow: false,
            wait_hours: 0,
        }]
    );
    // Without the lockfile there is no job, and without the manager none.
    assert!(
        patchula_lockfiles::refresh_jobs(&config, &[".github/workflows/ci.yml".to_owned()].into())
            .is_empty()
    );
    config.managers = vec![Manager::Cargo];
    assert!(!actions.claims(&config, "actions.lock", ".github/workflows", &paths));
}

/// `workflows:` is read as gh-actions-lock's parser reads it: a path key it
/// refuses or an entry that is no repository-scoped pin makes the whole
/// lock unreadable, and entries come back in the canonical key form the
/// tool writes.
#[test]
fn actions_lock_workflows_follow_the_tool_grammar() {
    use patchula_lockfiles::actions_lock_workflows as read;
    let lock =
        |workflows: &str| format!("version: 'v0.0.3'\nworkflows:\n{workflows}dependencies: {{}}\n");
    let sha = "ABCDEF0123456789ABCDEF0123456789ABCDEF01";
    let read_back = read(&lock(&format!(
        "    '.github/workflows/ci.yml':\n        - 'Azure/login@v2'\n        - 'actions/checkout@{sha}'\n        - 'octo/x@Release@2024'\n    '.github/workflows/empty.yml': []\n    '.github/workflows/null.yml':\n"
    )))
    .unwrap();
    assert_eq!(
        read_back[".github/workflows/ci.yml"],
        BTreeSet::from([
            "azure/login@v2".to_owned(),
            format!("actions/checkout@{}", sha.to_ascii_lowercase()),
            "octo/x@Release@2024".to_owned(),
        ])
    );
    assert!(read_back[".github/workflows/empty.yml"].is_empty());
    assert!(read_back[".github/workflows/null.yml"].is_empty());
    for bad in [
        // Path keys the tool refuses.
        "    '/etc/ci.yml': []\n",
        "    '.github/workflows/../../ci.yml': []\n",
        "    'C:\\ci.yml': []\n",
        "    '.github\\workflows\\ci.yml': []\n",
        "    '': []\n",
        // Entries that are no repository-scoped pin.
        "    '.github/workflows/ci.yml':\n        - 'actions/checkout'\n",
        "    '.github/workflows/ci.yml':\n        - 'actions/cache/save@v4'\n",
        "    '.github/workflows/ci.yml':\n        - 'actions@v4'\n",
        "    '.github/workflows/ci.yml':\n        - 'actions/checkout@'\n",
        "    '.github/workflows/ci.yml':\n        - 'actions/checkout@v4:x'\n",
        "    '.github/workflows/ci.yml':\n        - '/checkout@v4'\n",
        "    '.github/workflows/ci.yml':\n        - 3\n",
        "    '.github/workflows/ci.yml': {}\n",
    ] {
        assert_eq!(read(&lock(bad)), None, "{bad}");
    }
    assert_eq!(read("workflows: 3\n"), None);
    assert_eq!(read("version: 'v0.0.3'\n"), None);
}

/// `$/` callers follow gh-actions-lock's `ScanSelfRepositoryActions`: bare
/// step-level `$/<dir>` values, `parallel:` groups included, cleaned like
/// `filepath.Join`; `action.yml` before `action.yaml`; nested composites
/// followed. `./` actions, `$/…@ref`, job-level `$/` reusable workflows
/// and unlisted workflows call nothing. What the tool could not read
/// leaves the answer unknown.
#[test]
fn actions_lock_callers_follow_the_tool() {
    use patchula_lockfiles::actions_lock_callers;
    use std::collections::BTreeMap;
    let steps = |uses: &[&str]| {
        let mut text = "on: push\njobs:\n  t:\n    runs-on: ubuntu-latest\n    steps:\n".to_owned();
        for u in uses {
            text.push_str(&format!("      - uses: {u}\n"));
        }
        text
    };
    let composite =
        |uses: &str| format!("name: x\nruns:\n  using: composite\n  steps:\n    - uses: {uses}\n");
    let mut files: BTreeMap<String, String> = BTreeMap::from([
        (
            ".github/workflows/ci.yml".into(),
            steps(&["$/.github/actions/outer"]),
        ),
        (
            ".github/workflows/deploy.yml".into(),
            "on: push\njobs:\n  t:\n    runs-on: ubuntu-latest\n    steps:\n      - parallel:\n          - uses: $/./.github//actions/setup/\n".into(),
        ),
        (
            ".github/workflows/tagged.yml".into(),
            steps(&["$/.github/actions/setup@v1"]),
        ),
        (
            ".github/workflows/local.yml".into(),
            steps(&["./.github/actions/setup"]),
        ),
        (
            ".github/workflows/reusable.yml".into(),
            "on: push\njobs:\n  call:\n    uses: $/.github/actions/setup\n".into(),
        ),
        (
            ".github/workflows/unlisted.yml".into(),
            steps(&["$/.github/actions/setup"]),
        ),
        (
            ".github/workflows/both.yml".into(),
            steps(&["$/.github/actions/both"]),
        ),
        (
            ".github/actions/outer/action.yml".into(),
            composite("$/.github/actions/setup"),
        ),
        (
            ".github/actions/setup/action.yml".into(),
            composite("actions/setup-node@v6"),
        ),
        (
            ".github/actions/both/action.yml".into(),
            composite("actions/setup-node@v6"),
        ),
        (
            ".github/actions/both/action.yaml".into(),
            composite("actions/setup-node@v6"),
        ),
    ]);
    let mut listed: BTreeSet<String> = files
        .keys()
        .filter(|path| path.starts_with(".github/workflows/") && !path.contains("unlisted"))
        .cloned()
        .collect();
    let callers = |files: &BTreeMap<String, String>, listed: &BTreeSet<String>, file: &str| {
        actions_lock_callers(file, listed, &|path| Ok(files.get(path).cloned()))
    };
    assert_eq!(
        callers(&files, &listed, ".github/actions/setup/action.yml").unwrap(),
        BTreeSet::from([
            ".github/workflows/ci.yml".to_owned(),
            ".github/workflows/deploy.yml".to_owned(),
        ])
    );
    assert_eq!(
        callers(&files, &listed, ".github/actions/both/action.yml").unwrap(),
        BTreeSet::from([".github/workflows/both.yml".to_owned()])
    );
    assert!(
        callers(&files, &listed, ".github/actions/both/action.yaml")
            .unwrap()
            .is_empty()
    );
    // A workflow that does not parse, a `$/` path out of the repository, a
    // nested composite that does not parse, and a file whose text is not at
    // hand all leave the answer unknown.
    let file = ".github/actions/setup/action.yml";
    let mut broken = files.clone();
    broken.insert(".github/workflows/broken.yml".into(), "jobs: [\n".into());
    listed.insert(".github/workflows/broken.yml".into());
    assert!(
        callers(&broken, &listed, file)
            .unwrap_err()
            .contains("broken.yml does not parse")
    );
    listed.remove(".github/workflows/broken.yml");
    let mut escaping = files.clone();
    escaping.insert(
        ".github/workflows/ci.yml".into(),
        steps(&["$/../elsewhere"]),
    );
    assert!(
        callers(&escaping, &listed, file)
            .unwrap_err()
            .contains("outside the repository")
    );
    files.insert(
        ".github/actions/outer/action.yml".into(),
        "runs: [\n".into(),
    );
    assert!(
        callers(&files, &listed, file)
            .unwrap_err()
            .contains("outer/action.yml does not parse")
    );
    let unread = actions_lock_callers(file, &listed, &|path| {
        if path == ".github/workflows/ci.yml" {
            Err("not read".into())
        } else {
            Ok(None)
        }
    });
    assert!(unread.unwrap_err().contains("not read"));
}
