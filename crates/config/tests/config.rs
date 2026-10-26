#![forbid(unsafe_code)]

use patchula_config::{DependencyKind, Manager, SCHEMA, Update, VersionPolicy, discover, load};
use std::path::Path;

fn workspace() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}

#[test]
fn versions_host_is_enabled_by_default_and_can_be_disabled() {
    for (body, expected) in [("", true), ("useVersionsHost = false", false)] {
        let (_directory, path) = fixture(body);
        assert_eq!(load(&path).unwrap().use_versions_host, expected);
    }
}

fn fixture(body: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("patchula.pkl");
    std::fs::write(&path, format!("amends \"PatchulaConfig.pkl\"\n{body}\n")).unwrap();
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
    assert_eq!(examples.len(), 14);
    for example in examples {
        load(&example).unwrap_or_else(|error| panic!("{}: {error:?}", example.display()));
    }
}

#[test]
fn the_npm_lockfile_engine_is_aube_by_default_and_can_stay_native() {
    use patchula_config::NpmEngine;
    let (_directory, path) = fixture("");
    assert_eq!(load(&path).unwrap().lockfiles.npm_engine, NpmEngine::Aube);
    let (_directory, path) = fixture("lockfiles { npmEngine = \"npm\" }");
    assert_eq!(load(&path).unwrap().lockfiles.npm_engine, NpmEngine::Npm);
    let (_directory, path) = fixture("lockfiles { npmEngine = \"pnpm\" }");
    let error = load(&path).unwrap_err().to_string();
    assert!(error.contains("npmEngine"), "{error}");
}

#[test]
fn the_starter_config_is_the_schema_defaults_on_disk_or_off() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("patchula.pkl");
    std::fs::write(&path, patchula_config::STARTER).unwrap();
    let on_disk = serde_json::to_value(load(&path).unwrap()).unwrap();
    let (_directory, path) = fixture("");
    let schema = serde_json::to_value(load(&path).unwrap()).unwrap();
    let built_in = serde_json::to_value(patchula_config::defaults().unwrap()).unwrap();
    assert_eq!(on_disk, schema);
    assert_eq!(built_in, schema);
}

#[test]
fn defaults_are_explicit_safe_and_unlimited() {
    let (_directory, path) = fixture("");
    let config = load(&path).unwrap();
    assert!(config.defaults.action_digests && config.defaults.image_digests);
    assert!(!config.defaults.merge);
    assert_eq!(config.defaults.versions, VersionPolicy::Exact);
    assert_eq!(
        config.defaults.channel,
        patchula_config::ReleaseChannel::Stable
    );
    assert_eq!(config.defaults.wait_hours, 24);
    assert_eq!(config.prs.concurrent, None);
    assert_eq!(config.prs.per_hour, None);
    assert!(config.managers.contains(&Manager::GithubActions));
}

#[test]
fn nested_amendments_keep_class_defaults_locally_and_through_the_api() {
    use std::collections::BTreeMap;
    let dir = tempfile::tempdir().unwrap();
    let source =
        "amends \"PatchulaConfig.pkl\"\nprs { dashboard = true }\ndefaults { waitHours = 0 }\n";
    std::fs::write(dir.path().join("patchula.pkl"), source).unwrap();
    let files = BTreeMap::from([("/repo/patchula.pkl".into(), source.into())]);
    let local = load(dir.path()).unwrap();
    let api = patchula_config::load_files("/repo/patchula.pkl", files).unwrap();
    assert_eq!(
        serde_json::to_value(&local).unwrap(),
        serde_json::to_value(api).unwrap()
    );
    assert_eq!(local.prs.branch, "patchula/");
    assert!(local.defaults.active);
    assert!(local.prs.dashboard);
    assert_eq!(local.defaults.wait_hours, 0);
}

#[test]
fn a_vendored_schema_overrides_the_built_in_one() {
    let (dir, path) = fixture("prs { dashboard = true }");
    std::fs::write(
        dir.path().join("PatchulaConfig.pkl"),
        SCHEMA.replace("patchula/", "custom/"),
    )
    .unwrap();
    assert_eq!(load(&path).unwrap().prs.branch, "custom/");
}

#[test]
fn node_lts_channel_is_typed_and_rule_selectable() {
    let (_dir, path) = fixture(
        "rules = new Listing { new Rule { select { packages = new Listing { \"node\"; \"actions/node-versions\" } } apply { channel = \"lts\" } } }",
    );
    assert_eq!(
        load(&path).unwrap().rules[0].apply.channel,
        Some(patchula_config::ReleaseChannel::Lts)
    );
    let (_dir, path) = fixture("defaults { channel = \"guess\" }");
    assert!(load(&path).is_err());
}

#[test]
fn unknown_field_path_contains_leaf_once() {
    for (body, field) in [
        ("surprise = true", "surprise"),
        ("prs { dashbord = true }", "prs.dashbord"),
        (
            "rules = new Listing { new Rule { apply { typo = true } } }",
            "rules[0].apply.typo",
        ),
    ] {
        let (_directory, path) = fixture(body);
        let message = load(&path).unwrap_err().to_string();
        assert!(
            message.contains(&format!("{field}: unknown field")),
            "{message}"
        );
        let leaf = field.rsplit('.').next().unwrap();
        assert!(!message.contains(&format!("{field}.{leaf}")), "{message}");
    }
}

#[test]
fn dependency_kinds_are_closed_and_typed() {
    let (_directory, path) = fixture(
        "rules = new Listing { new Rule { select { kinds = new Listing { \"development\"; \"reusable-workflow\"; \"go-directive\" } } } }",
    );
    assert_eq!(
        load(&path).unwrap().rules[0].select.kinds,
        [
            DependencyKind::Development,
            DependencyKind::ReusableWorkflow,
            DependencyKind::GoDirective
        ]
    );
    for kind in ["developmnt", "regex:.*", "*"] {
        let (_directory, path) = fixture(&format!(
            "rules = new Listing {{ new Rule {{ select {{ kinds = new Listing {{ \"{kind}\" }} }} }} }}"
        ));
        assert!(load(&path).is_err(), "accepted {kind}");
    }
    assert!(serde_json::from_str::<DependencyKind>("\"developmnt\"").is_err());
}

#[test]
fn migration_preserves_critical_intent() {
    let config = load(&workspace().join("examples/renovate-migration.pkl")).unwrap();
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
        ["acme/shared-workflows"]
    );
    assert_eq!(config.rules[1].apply.digest, Some(false));
    assert_eq!(
        config.rules[1].select.kinds,
        [DependencyKind::ReusableWorkflow]
    );
    assert_eq!(config.extractors.len(), 2);
    let config = load(&workspace().join("examples/kubernetes-infra.pkl")).unwrap();
    assert_eq!(config.rules[3].select.updates, [Update::Major]);
    assert_eq!(config.rules[3].apply.active, Some(false));
    assert_eq!(config.files.len(), 3);
    let config = load(&workspace().join("examples/preserve-ranges.pkl")).unwrap();
    assert_eq!(config.defaults.versions, VersionPolicy::Preserve);
    let (_directory, path) = fixture("");
    let mut defaults = serde_json::to_value(load(&path).unwrap()).unwrap();
    defaults["defaults"]["versions"] = serde_json::json!("preserve");
    assert_eq!(serde_json::to_value(config).unwrap(), defaults);
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
        assert!(message.contains("patchula.pkl"), "{message}");
    }
}

#[test]
fn syntax_errors_include_file_and_location() {
    let (_directory, path) = fixture("defaults { versions = ");
    let error = format!("{:?}", load(&path).unwrap_err());
    assert!(error.contains("patchula.pkl"), "{error}");
    assert!(error.contains('│') || error.contains("line"), "{error}");
}

#[test]
fn local_organisation_amends_and_imports_work() {
    let (directory, path) = fixture("");
    std::fs::write(
        directory.path().join("org.pkl"),
        "amends \"PatchulaConfig.pkl\"\nprs { concurrent = 8 }\ndefaults { releaseNotes = false }\n",
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
fn discovery_reads_only_the_root_and_reports_absence() {
    let (directory, root) = fixture("");
    assert_eq!(discover(directory.path()).unwrap(), Some(root));
    let github_only = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(github_only.path().join(".github")).unwrap();
    std::fs::write(
        github_only.path().join(".github/patchula.pkl"),
        "amends \"PatchulaConfig.pkl\"\nprs { concurrent = 3 }\n",
    )
    .unwrap();
    assert_eq!(discover(github_only.path()).unwrap(), None);
    let empty = tempfile::tempdir().unwrap();
    assert_eq!(discover(empty.path()).unwrap(), None);
    assert!(
        load(empty.path())
            .unwrap_err()
            .to_string()
            .contains("no patchula.pkl")
    );
}

#[test]
fn remote_imports_and_environment_reads_are_rejected_without_network() {
    for body in [
        "amends \"https://invalid.example/config.pkl\"",
        "amends \"PatchulaConfig.pkl\"\ndefaults { commitPrefix = read(\"env:PATH\") }",
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

#[test]
fn cron_weekdays_use_standard_numbering() {
    use chrono::Datelike;
    // 2026-10-17 is a Saturday. Days are numbered Monday 1 to Sunday 7.
    let start = chrono::DateTime::parse_from_rfc3339("2026-10-17T00:00:00Z")
        .unwrap()
        .to_utc();
    let days = |expr: &str, seconds: bool| -> Vec<u32> {
        patchula_config::schedule(expr, seconds)
            .unwrap()
            .after(&start)
            .take(7)
            .map(|t| t.weekday().number_from_monday())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
    };
    assert_eq!(days("0 12 * * 1", false), [1]);
    assert_eq!(days("0 12 * * 0", false), [7]);
    assert_eq!(days("0 12 * * 7", false), [7]);
    assert_eq!(days("0 12 * * 1-5", false), [1, 2, 3, 4, 5]);
    assert_eq!(days("0 12 * * 5-7", false), [5, 6, 7]);
    assert_eq!(days("0 12 * * MON,WED", false), [1, 3]);
    assert_eq!(days("0 0 12 * * 1", true), [1]);
    // Steps apply across the whole range, and 7 alone is Sunday.
    assert_eq!(days("0 12 * * 5-7/2", false), [5, 7]);
    assert_eq!(days("0 12 * * 7-7", false), [7]);
    assert_eq!(days("0 12 * * */2", false), [2, 4, 6, 7]);
    assert_eq!(days("0 12 * * 1/2", false), [1, 3, 5, 7]);
    assert_eq!(days("0 12 * * 7/2", false), [7]);
}

#[test]
fn gitlab_host_is_gitlab_com_by_default_and_must_name_a_gitlab_instance() {
    let (_directory, path) = fixture("");
    assert_eq!(load(&path).unwrap().gitlab.host, "gitlab.com");
    let (_directory, path) = fixture("gitlab { host = \"gitlab.example.com\" }");
    assert_eq!(load(&path).unwrap().gitlab.host, "gitlab.example.com");
    for host in [
        "git.example.com",
        "https://gitlab.example.com",
        "GitLab.example.com",
        "",
    ] {
        let (_directory, path) = fixture(&format!("gitlab {{ host = \"{host}\" }}"));
        let error = format!("{:?}", load(&path).unwrap_err());
        assert!(error.contains("gitlab.host"), "{host}: {error}");
    }
}
