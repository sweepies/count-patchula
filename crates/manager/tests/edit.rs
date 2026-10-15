use rustovate_config::{Config, Manager};
use rustovate_manager::{Edit, Files, Scanner, apply, edits};

fn config(manager: Manager) -> Config {
    static CONFIG: std::sync::OnceLock<Config> = std::sync::OnceLock::new();
    let mut config = CONFIG
        .get_or_init(|| {
            rustovate_config::load_files(
                "/repo/rustovate.pkl",
                std::collections::BTreeMap::from([(
                    "/repo/rustovate.pkl".into(),
                    "amends \"Rustovate.pkl\"".into(),
                )]),
            )
            .unwrap()
        })
        .clone();
    config.managers = vec![manager];
    config
}
fn replace(manager: Manager, path: &str, text: &str, value: &str, digest: Option<&str>) -> String {
    let dependencies = Scanner::new(&config(manager))
        .unwrap()
        .extract(&Files::from([(path.into(), text.into())]))
        .unwrap()
        .dependencies;
    assert!(!dependencies.is_empty());
    let changes = dependencies
        .iter()
        .flat_map(|dep| edits(dep, text, value, digest).unwrap())
        .collect();
    apply(text, changes).unwrap()
}

#[test]
fn json_order_indentation_aliases_workspaces_and_escaped_names_survive() {
    let old = "{\r\n\t\"name\": \"test\",\r\n\t\"dependencies\": {\"alias\": \"npm:@scope/real@^1.0\", \"a\\u0062\": \"~1.0\"},\r\n\t\"workspaces\": [\"packages/*\"]\r\n}\r\n";
    let new = replace(Manager::Npm, "package.json", old, "2.0.0", None);
    assert_eq!(new, old.replace("^1.0", "2.0.0").replace("~1.0", "2.0.0"));
}
#[test]
fn cargo_preserves_inline_dotted_target_workspace_and_table_forms() {
    let old = "# keep\n[workspace.dependencies]\na = '1.0' # a\nb = { version = \"1.0\", features = [\"x\"] }\n[dependencies]\na.workspace = true\n[target.'cfg(unix)'.dependencies.c]\nversion = '1.0'\npackage = 'real'\n";
    assert_eq!(
        replace(Manager::Cargo, "Cargo.toml", old, "=2.0.0", None),
        old.replace("'1.0'", "'=2.0.0'")
            .replace("\"1.0\"", "\"=2.0.0\"")
    );
}
#[test]
fn mise_preserves_arrays_inline_tables_subtables_and_tool_version_spacing() {
    let old = "# tools\n[tools]\nnode = ['20.0.0', \"tag:22.0.0\"] # keep\nbun = { version = '1.0.0', postinstall = 'echo hello' }\n[tools.jq]\nversion = \"1.6\"\n";
    assert_eq!(
        replace(Manager::Mise, "mise.toml", old, "2.0.0", None),
        old.replace("20.0.0", "2.0.0")
            .replace("22.0.0", "2.0.0")
            .replace("1.0.0", "2.0.0")
            .replace("1.6", "2.0.0")
    );
    let old = "node   20.0.0 22.0.0 # two\nbun\t1.0.0\n";
    assert_eq!(
        replace(Manager::Mise, ".tool-versions", old, "2.0.0", None),
        old.replace("20.0.0", "2.0.0")
            .replace("22.0.0", "2.0.0")
            .replace("1.0.0", "2.0.0")
    );
}
#[test]
fn action_pins_keep_subpaths_comments_quotes_and_crlf() {
    let sha = "a".repeat(40);
    let digest = "b".repeat(40);
    let old = format!(
        "jobs:\r\n  a:\r\n    steps:\r\n      - uses: 'org/action/path@{sha}' #  v1.0.0 note\r\n      - uses: org/plain@v1\r\n"
    );
    let new = replace(
        Manager::GithubActions,
        ".github/workflows/ci.yml",
        &old,
        "v2",
        Some(&digest),
    );
    assert_eq!(
        new,
        format!(
            "jobs:\r\n  a:\r\n    steps:\r\n      - uses: 'org/action/path@{digest}' #  v2 note\r\n      - uses: org/plain@{digest} # v2\r\n"
        )
    );
    let old = "jobs:\n  a:\n    steps:\n      - uses: org/plain@v1 # keep note\n";
    assert!(
        replace(
            Manager::GithubActions,
            ".github/workflows/ci.yml",
            old,
            "v2",
            Some(&digest)
        )
        .ends_with(&format!("@{digest} # v2 keep note\n"))
    );
    let old = "jobs:\n  a:\n    steps:\n      - uses: org/plain@v1 #  v1.2.3 keep note\n";
    assert!(
        replace(
            Manager::GithubActions,
            ".github/workflows/ci.yml",
            old,
            "v2",
            Some(&digest)
        )
        .ends_with(&format!("@{digest} #  v2 keep note\n"))
    );
}
#[test]
fn images_preserve_platform_stages_variants_ports_quotes_and_comments() {
    let old = "# stages\nFROM --platform=linux/amd64 registry.example:5000/base:1.0-alpine AS build\nFROM nginx:1.0-alpine\nCOPY --from=build /app /app\n";
    assert_eq!(
        replace(Manager::Dockerfile, "Dockerfile", old, "2.0-alpine", None),
        old.replace("1.0-alpine", "2.0-alpine")
    );
    let old = "# π\nservices:\n  a: {image: 'registry.example:5000/a:1.0-alpine'} # one\n  b:\n    image: a:1.0-alpine # two\n";
    assert_eq!(
        replace(Manager::Compose, "compose.yml", old, "2.0-alpine", None),
        old.replace("1.0-alpine", "2.0-alpine")
    );
    let old = "jobs:\n  a:\n    container: 'node:1.0-alpine' # keep\n";
    assert_eq!(
        replace(
            Manager::GithubActions,
            ".github/workflows/ci.yml",
            old,
            "2.0-alpine",
            None
        ),
        old.replace("1.0-alpine", "2.0-alpine")
    );
}
#[test]
fn go_preserves_require_blocks_indirect_comments_and_directives() {
    let old = "module example.org/app\n\ngo 1.22.0\ntoolchain go1.22.1\n\nrequire (\n\texample.org/a v1.0.0 // indirect\n)\nrequire example.org/b v1.0.0\n";
    assert_eq!(
        replace(Manager::Go, "go.mod", old, "1.23.0", None),
        old.replace("1.22.0", "1.23.0")
            .replace("1.22.1", "1.23.0")
            .replace("v1.0.0", "1.23.0")
    );
}
#[test]
fn overlapping_invalid_and_expanded_locations_refuse_edits() {
    let changes = vec![
        Edit {
            start: 0,
            end: 2,
            replacement: "x".into(),
        },
        Edit {
            start: 1,
            end: 3,
            replacement: "y".into(),
        },
    ];
    assert!(apply("abcd", changes).is_err());
    assert!(
        apply(
            "π",
            vec![Edit {
                start: 1,
                end: 2,
                replacement: "x".into()
            }]
        )
        .is_err()
    );
    let text = "ARG BASE=node:1.0-alpine\nFROM ${BASE}\n";
    let deps = Scanner::new(&config(Manager::Dockerfile))
        .unwrap()
        .extract(&Files::from([("Dockerfile".into(), text.into())]))
        .unwrap()
        .dependencies;
    assert!(edits(&deps[0], text, "2.0-alpine", None).is_err());
}
