//! Unit tests of the crate's private helpers: the path subtree walk and
//! maintenance's `Refresh` claims.
use super::*;

fn paths(paths: &[&str]) -> BTreeSet<String> {
    paths.iter().map(|p| (*p).to_owned()).collect()
}

#[test]
fn subtree_lists_only_paths_under_the_directory() {
    let paths = paths(&["a/x", "a/b/y", "a-b/z", "ab/w", "c"]);
    let under = |dir| subtree(&paths, dir).cloned().collect::<Vec<_>>();
    assert_eq!(under("a"), ["a/b/y", "a/x"]);
    assert_eq!(under("a/b"), ["a/b/y"]);
    assert_eq!(under("d"), Vec::<String>::new());
    assert_eq!(under("").len(), 5);
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
    // The Gradle wrapper is never maintenance work, even at its own path.
    let wrapper = BTreeSet::from(["gradle/wrapper/gradle-wrapper.properties".to_owned()]);
    assert!(!refresh(Ecosystem::GradleWrapper).claims(
        &config,
        "gradle-wrapper.properties",
        "gradle/wrapper",
        &wrapper
    ));
    // Path: actions.lock claims maintenance at its one committed path,
    // while the Actions manager is enabled; the same leaf elsewhere is not
    // the lock.
    config.managers = vec![Manager::GithubActions];
    let actions = refresh(Ecosystem::Actions);
    assert!(actions.claims(&config, "actions.lock", ".github/workflows", &paths));
    assert!(!actions.claims(&config, "actions.lock", "app", &paths));
    assert!(!actions.claims(&config, "actions.lock", ".forgejo/workflows", &paths));
    let jobs = refresh_jobs(&config, &[ACTIONS_LOCK.to_owned()].into());
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
    assert!(refresh_jobs(&config, &[".github/workflows/ci.yml".to_owned()].into()).is_empty());
    config.managers = vec![Manager::Cargo];
    assert!(!actions.claims(&config, "actions.lock", ".github/workflows", &paths));
}
