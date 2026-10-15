use rustovate_config::{DependencyKind, Manager};
use rustovate_manager::{Files, Scanner};
fn config() -> rustovate_config::Config {
    rustovate_config::load(std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/renovate-runner.pkl"
    )))
    .unwrap()
}
#[test]
fn manifests_workspaces_aliases_overrides_and_lock_resolution() {
    let files=Files::from([
        ("package.json".into(),r#"{"workspaces":["packages/*"],"dependencies":{"foo":"^1.0.0"},"devDependencies":{"alias":"npm:real@~2.0.0"},"peerDependencies":{"peer":"1.x"},"optionalDependencies":{"opt":"3.0.0"},"overrides":{"parent":{"foo":"4.0.0"}},"packageManager":"bun@1.2.3"}"#.into()),
        ("packages/a/package.json".into(),r#"{"dependencies":{"foo":"^1.0.0","local":"workspace:*"}}"#.into()),
        ("bun.lock".into(),r#"{// Bun JSONC
          "packages":{"foo":["foo@1.8.0", "", {}, "sha512-test"],"alias":["real@2.1.0", "", {}],},
        }"#.into()),
    ]);
    let scanner = Scanner::new(&config()).unwrap();
    assert!(scanner.needs("packages/a/package.json"));
    assert!(!scanner.needs("node_modules/foo/package.json"));
    let extraction = scanner.extract(&files).unwrap();
    assert_eq!(extraction.dependencies.len(), 7);
    let alias = extraction
        .dependencies
        .iter()
        .find(|d| d.name == "alias")
        .unwrap();
    assert_eq!(alias.package, "real");
    assert_eq!(alias.resolved.as_deref(), Some("2.1.0"));
    assert_eq!(alias.kind, DependencyKind::Development);
    for dep in &extraction.dependencies {
        let text = &files[&dep.file];
        assert!(dep.location.start < dep.location.end);
        assert!(text[dep.location.start..dep.location.end].contains(&dep.current));
    }
    let workspace = extraction
        .dependencies
        .iter()
        .find(|d| d.file.starts_with("packages/"))
        .unwrap();
    assert_eq!(workspace.resolved.as_deref(), Some("1.8.0"));
}
#[test]
fn workflows_composites_pins_containers_and_local_refs() {
    let files = Files::from([
        (
            ".github/workflows/test.yml".into(),
            r#"jobs:
  reusable:
    uses: org/repo/.github/workflows/build.yml@main
  build:
    container: node:22
    services:
      db:
        image: postgres:16
    steps:
      - uses: actions/checkout@aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa # v4.2.0
      - uses: docker://alpine:3.20
      - uses: ./local
      - run: |
          echo 'uses: wrong/repo@v99'
"#
            .into(),
        ),
        (
            ".github/actions/local/action.yml".into(),
            "runs:\n  using: composite\n  steps:\n    - uses: org/action@v1\n".into(),
        ),
    ]);
    let deps = Scanner::new(&config())
        .unwrap()
        .extract(&files)
        .unwrap()
        .dependencies;
    assert_eq!(deps.len(), 6);
    assert!(!deps.iter().any(|d| d.package == "wrong/repo"));
    let pin = deps.iter().find(|d| d.name == "actions/checkout").unwrap();
    assert_eq!(pin.resolved.as_deref(), Some("v4.2.0"));
    assert_eq!(pin.current, "v4.2.0");
    assert_eq!(pin.digest.as_ref().unwrap().len(), 40);
    assert_eq!(
        deps.iter().find(|d| d.current == "main").unwrap().kind,
        DependencyKind::ReusableWorkflow
    );
}

#[test]
fn action_comment_digest_only_and_ref_models_keep_edit_format() {
    let text = "jobs:\n  a:\n    steps:\n      - uses: 'org/action/path@aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa' #  v1.2.3 trailing note\n      - uses: org/plain@bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n      - uses: org/ref@main\n";
    let files = Files::from([(".github/workflows/test.yml".into(), text.into())]);
    let deps = Scanner::new(&config())
        .unwrap()
        .extract(&files)
        .unwrap()
        .dependencies;
    assert_eq!(deps[0].current, "v1.2.3");
    assert_eq!(
        deps[0].digest.as_deref(),
        Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
    );
    assert_eq!(
        rustovate_manager::action_replacement(&deps[0], text, "v2.0.0", Some(&"c".repeat(40)))
            .unwrap(),
        format!(
            "'org/action/path@{}' #  v2.0.0 trailing note",
            "c".repeat(40)
        )
    );
    assert_eq!(deps[1].current, "b".repeat(40));
    assert!(deps[1].resolved.is_none());
    assert_eq!(deps[2].current, "main");
    assert!(deps[2].digest.is_none());
}

#[test]
fn hosted_runner_literals_preserve_families_and_skip_self_hosted_and_expressions() {
    let text = "jobs:\n  a:\n    runs-on: ubuntu-latest\n  b:\n    runs-on: [macos-15, windows-2025, ubuntu-24.04-arm]\n  c:\n    runs-on:\n      - macos-26-intel\n  d:\n    runs-on: [self-hosted, ubuntu-22.04]\n  e:\n    runs-on: ${{ matrix.os }}\n  f:\n    runs-on: custom-linux\n";
    let deps = Scanner::new(&config())
        .unwrap()
        .extract(&Files::from([(
            ".github/workflows/test.yml".into(),
            text.into(),
        )]))
        .unwrap()
        .dependencies;
    assert_eq!(deps.len(), 5);
    assert!(deps.iter().all(|d| d.kind == DependencyKind::Runner));
    assert_eq!(deps[0].name, "ubuntu");
    assert_eq!(deps[0].current, "latest");
    assert!(deps[0].resolved.is_none());
    assert_eq!(deps[3].package, "ubuntu-arm");
    assert_eq!(deps[3].current, "24.04");
    assert_eq!(deps[4].package, "macos-intel");
    for dep in &deps {
        assert!(text[dep.location.start..dep.location.end].contains(&dep.current));
    }
}

#[test]
fn hosted_runner_locations_do_not_select_identical_self_hosted_labels_or_run_text() {
    let text = "'jobs': # workflows\n  own:\n    runs-on: [self-hosted, ubuntu-22.04]\n  hosted: # GitHub hosted job\n    steps:\n      - run: echo ubuntu-22.04\n    \"runs-on\": ubuntu-22.04\n";
    let deps = Scanner::new(&config())
        .unwrap()
        .extract(&Files::from([(
            ".github/workflows/test.yml".into(),
            text.into(),
        )]))
        .unwrap()
        .dependencies;
    assert_eq!(deps.len(), 1);
    assert_eq!(deps[0].location.start, text.rfind("ubuntu-22.04").unwrap());
    assert_eq!(
        rustovate_manager::runner_replacement(&deps[0], text, "24.04").unwrap(),
        "ubuntu-24.04"
    );
}
#[test]
fn mise_forms_and_custom_capture_locations() {
    let files=Files::from([("mise.toml".into(),"[tools]\nbun = \"1.2.3\"\nnode = [\"20\", \"22\"]\n\"github:org/tool\" = { version = \"v2.0.0\", os = [\"linux\"] }\n".into()),(".tool-versions".into(),"bun 1.2.3 1.3.0 # comment\n".into()),("infra/versions.ts".into(),"// renovate: datasource=npm depName=renovate\nexport const RENOVATE_VERSION = \"44.0.0\";\n".into())]);
    let deps = Scanner::new(&config())
        .unwrap()
        .extract(&files)
        .unwrap()
        .dependencies;
    assert_eq!(deps.len(), 7);
    assert!(deps.iter().any(|d| d.manager == Manager::Regex));
    for dep in &deps {
        assert!(files[&dep.file][dep.location.start..dep.location.end].contains(&dep.current));
    }
}

#[test]
fn npm_is_strict_json_and_bun_lock_is_jsonc() {
    let scanner = Scanner::new(&config()).unwrap();
    for text in [
        "{'dependencies':{'a':'1.0.0'}}",
        "{\"dependencies\":{\"a\":\"1.0.0\",}}",
        "{\"dependencies\":{\"a\":\"1.0.0\" \"b\":\"2.0.0\"}}",
    ] {
        assert!(
            scanner
                .extract(&Files::from([("package.json".into(), text.into())]))
                .is_err()
        );
    }
}
