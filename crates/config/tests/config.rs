#![forbid(unsafe_code)]

use rustovate_config::{Manager, SCHEMA, Update, VersionPolicy, discover, load};
use std::path::Path;

fn workspace() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}

fn fixture(body: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("Rustovate.pkl"), SCHEMA).unwrap();
    let path = directory.path().join("rustovate.pkl");
    std::fs::write(&path, format!("amends \"Rustovate.pkl\"\n{body}\n")).unwrap();
    (directory, path)
}

#[test]
fn every_repository_example_validates() {
    let mut examples: Vec<_> = std::fs::read_dir(workspace().join("examples"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "pkl"))
        .collect();
    examples.sort();
    assert_eq!(examples.len(), 12);
    for example in examples {
        load(&example).unwrap_or_else(|error| panic!("{}: {error:?}", example.display()));
    }
}

#[test]
fn defaults_are_explicit_safe_and_unlimited() {
    let (_directory, path) = fixture("");
    let config = load(&path).unwrap();
    assert!(config.defaults.action_digests && config.defaults.image_digests);
    assert!(!config.defaults.merge);
    assert_eq!(config.defaults.versions, VersionPolicy::Exact);
    assert_eq!(config.prs.concurrent, None);
    assert_eq!(config.prs.per_hour, None);
    assert!(config.managers.contains(&Manager::GithubActions));
}

#[test]
fn migration_preserves_critical_intent() {
    let config = load(&workspace().join("examples/renovate-runner.pkl")).unwrap();
    assert_eq!(
        config.managers,
        [
            Manager::Npm,
            Manager::GithubActions,
            Manager::Mise,
            Manager::Regex
        ]
    );
    assert_eq!(
        config.rules[0].apply.group.as_deref(),
        Some("alchemy, effect and sveltekit")
    );
    assert_eq!(
        config.rules[1].select.dependencies,
        ["sweepies/ops-workflows"]
    );
    assert_eq!(config.rules[1].apply.digest, Some(false));
    assert_eq!(config.extractors.len(), 2);
    let config = load(&workspace().join("examples/maccrae-infra.pkl")).unwrap();
    assert_eq!(config.rules[3].select.updates, [Update::Major]);
    assert_eq!(config.rules[3].apply.active, Some(false));
    assert_eq!(config.files.len(), 3);
    let config = load(&workspace().join("examples/fnox.pkl")).unwrap();
    assert!(
        config.defaults.merge
            && config.submodules
            && config.lockfiles.refresh
            && config.lockfiles.tidy_go
    );
    assert_eq!(config.defaults.wait_hours, 168);
    assert_eq!(config.prs.per_hour, Some(10));
    assert_eq!(config.defaults.windows[0].zone, "America/Chicago");
    assert!(
        config
            .rules
            .iter()
            .any(|rule| rule.apply.wait_hours == Some(0)
                && rule.select.packages.contains(&"pklr".into()))
    );
}

#[test]
fn invalid_values_have_actionable_diagnostics() {
    for (body, expected) in [
        ("surprise = true", "surprise"),
        ("prs { dashboard = \"yes\" }", "dashboard"),
        (
            "rules = new Listing { new Rule { select { packages = new Listing { \"regex:[\" } } } }",
            "invalid regex",
        ),
        (
            "rules = new Listing { new Rule { select { updates = new Listing { \"enormous\" } } } }",
            "enormous",
        ),
        (
            "managers = new Listing { \"unknown-manager\" }",
            "unknown-manager",
        ),
        ("prs { concurrent = 0 }", "unlimited"),
        ("defaults { waitHours = -1 }", "waitHours"),
        (
            "rules = new Listing { new Rule { apply { typo = true } } }",
            "typo",
        ),
        (
            "rules = new Listing { new Rule { apply { permitted = \"semver:>??\" } } }",
            "semver",
        ),
        (
            "files = new Listing { new FileSet { manager = \"flux\"; paths = new Listing {} } }",
            "at least one",
        ),
        (
            "extractors = new Listing { new Extractor { paths = new Listing { \"*.txt\" }; patterns = new Listing { \"(?<value>.+)\" } } }",
            "current",
        ),
        (
            "extractors = new Listing { new Extractor { paths = new Listing { \"*.txt\" }; patterns = new Listing { \"(?<current>.+)\" }; source = \"github-releases\"; dependency = \"{missing}\" } }",
            "missing capture",
        ),
        (
            "defaults { windows = new Listing { new Window { cron = new Listing { \"nonsense\" } } } }",
            "five-field cron",
        ),
        (
            "defaults { windows = new Listing { new Window { cron = new Listing { \"* * * * *\" }; zone = \"Mars/Olympus\" } } }",
            "timezone",
        ),
    ] {
        let (_directory, path) = fixture(body);
        let error = load(&path).unwrap_err();
        let message = format!("{error:?}");
        assert!(
            message.contains(expected),
            "expected {expected:?} in {message}"
        );
        assert!(message.contains("rustovate.pkl"), "{message}");
    }
}

#[test]
fn syntax_errors_include_file_and_location() {
    let (_directory, path) = fixture("defaults { versions = ");
    let error = format!("{:?}", load(&path).unwrap_err());
    assert!(error.contains("rustovate.pkl"), "{error}");
    assert!(error.contains('│') || error.contains("line"), "{error}");
}

#[test]
fn local_organisation_amends_and_imports_work() {
    let (directory, path) = fixture("");
    std::fs::write(
        directory.path().join("org.pkl"),
        "amends \"Rustovate.pkl\"\nprs { concurrent = 8 }\ndefaults { releaseNotes = false }\n",
    )
    .unwrap();
    std::fs::write(
        directory.path().join("labels.pkl"),
        "value = \"team-infra\"\n",
    )
    .unwrap();
    std::fs::write(&path, "amends \"org.pkl\"\nimport \"labels.pkl\" as Labels\ndefaults { labels = new Listing { Labels.value } }\n").unwrap();
    let config = load(&path).unwrap();
    assert_eq!(config.prs.concurrent, Some(8));
    assert!(!config.defaults.release_notes);
    assert_eq!(config.defaults.labels, ["team-infra"]);
}

#[test]
fn discovery_prefers_github_and_reports_absence() {
    let (directory, root) = fixture("");
    assert_eq!(discover(directory.path()).unwrap(), Some(root.clone()));
    std::fs::create_dir(directory.path().join(".github")).unwrap();
    let github = directory.path().join(".github/rustovate.pkl");
    std::fs::write(
        &github,
        "amends \"../Rustovate.pkl\"\nprs { concurrent = 3 }\n",
    )
    .unwrap();
    assert_eq!(discover(directory.path()).unwrap(), Some(github));
    assert_eq!(load(directory.path()).unwrap().prs.concurrent, Some(3));
    let empty = tempfile::tempdir().unwrap();
    assert_eq!(discover(empty.path()).unwrap(), None);
    assert!(
        load(empty.path())
            .unwrap_err()
            .to_string()
            .contains("no rustovate config")
    );
}

#[test]
fn remote_imports_and_environment_reads_are_rejected_without_network() {
    for body in [
        "amends \"https://invalid.example/config.pkl\"",
        "amends \"Rustovate.pkl\"\ndefaults { commitPrefix = read(\"env:PATH\") }",
    ] {
        let (directory, path) = fixture("");
        std::fs::write(&path, body).unwrap();
        let error = format!("{:?}", load(&path).unwrap_err());
        assert!(
            error.contains("offline")
                || error.contains("denied")
                || error.contains("not allowed")
                || error.contains("disabled")
                || error.contains("allowlist"),
            "{error}"
        );
        drop(directory);
    }
}

#[test]
fn extractor_templates_support_named_version_schemes() {
    let (_directory, path) = fixture(
        r##"
extractors = new Listing {
  new Extractor {
    paths = new Listing { "versions.txt" }
    patterns = new Listing { #"(?<source>npm) (?<dependency>[^ ]+) (?<scheme>semver) (?<current>.+)"# }
    scheme = "{scheme}"
  }
}
"##,
    );
    let config = load(&path).unwrap();
    assert_eq!(config.extractors[0].scheme, "{scheme}");
}
