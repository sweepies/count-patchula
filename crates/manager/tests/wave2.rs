use patchula_config::{DependencyKind, Manager, Source};
use patchula_manager::{Files, Scanner};
fn extract(files: &Files) -> Vec<patchula_manager::Dependency> {
    let config = patchula_config::load(std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/kubernetes-infra.pkl"
    )))
    .unwrap();
    let result = Scanner::new(&config).unwrap().extract(files).unwrap();
    assert!(result.warnings.is_empty());
    for dep in &result.dependencies {
        let text = &files[&dep.file];
        assert!(dep.location.start < dep.location.end);
        assert!(
            text.is_char_boundary(dep.location.start) && text.is_char_boundary(dep.location.end)
        );
        assert!(dep.location.line > 0 && dep.location.column > 0);
    }
    result.dependencies
}
#[test]
fn cargo_workspaces_targets_renames_and_lock_resolution() {
    let files = Files::from([
        (
            "Cargo.toml".into(),
            r#"[package]
name = "app"
version = "0.1.0"
[workspace.dependencies]
serde = "1.0"
[dependencies]
renamed = { package = "actual", version = "^0.3" }
local = { path = "local" }
[target.'cfg(unix)'.build-dependencies]
cc = "1"
[dev-dependencies]
test = "2.0"
"#
            .into(),
        ),
        (
            "Cargo.lock".into(),
            r#"version = 4
[[package]]
name = "app"
version = "0.1.0"
dependencies = ["actual 0.3.9"]
[[package]]
name = "actual"
version = "0.3.9"
source = "registry+https://github.com/rust-lang/crates.io-index"
[[package]]
name = "actual"
version = "0.4.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
[[package]]
name = "transitive"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
"#
            .into(),
        ),
        (
            "crates/member/Cargo.toml".into(),
            "[dependencies]\nserde.workspace = true\n".into(),
        ),
    ]);
    let deps = extract(&files);
    assert_eq!(deps.len(), 5);
    assert!(deps.iter().all(|d| d.manager == Manager::Cargo));
    let renamed = deps.iter().find(|d| d.name == "renamed").unwrap();
    assert_eq!(renamed.package, "actual");
    assert_eq!(renamed.resolved.as_deref(), Some("0.3.9"));
    assert_eq!(
        deps.iter().find(|d| d.name == "cc").unwrap().kind,
        DependencyKind::Build
    );
    assert!(
        deps.iter()
            .filter(|d| d.file == "Cargo.toml")
            .all(|d| files[&d.file][d.location.start..d.location.end].contains(&d.current))
    );
}
#[test]
fn dockerfile_args_multistage_platform_and_compose() {
    let files=Files::from([
        ("Dockerfile".into(),"ARG ALPINE_VERSION=3.20\nFROM --platform=$BUILDPLATFORM alpine:${ALPINE_VERSION} AS build\nFROM build AS copied\nFROM \\\n  nginx:1.26-bookworm\nFROM scratch\n".into()),
        ("compose.yaml".into(),"services:\n  db:\n    image: 'postgres:16.3-alpine@sha256:abc'\n  build:\n    build: .\n".into()),
    ]);
    let deps = extract(&files);
    assert_eq!(deps.len(), 3, "{deps:?}");
    assert!(
        deps.iter()
            .any(|d| d.package == "alpine" && d.current == "3.20")
    );
    assert!(
        deps.iter()
            .any(|d| d.package == "nginx" && d.current == "1.26-bookworm")
    );
    assert!(
        deps.iter()
            .any(|d| d.package == "postgres" && d.digest.as_deref() == Some("sha256:abc"))
    );
}
#[test]
fn go_directives_requires_and_python_markers_hashes() {
    let files=Files::from([
        ("go.mod".into(),"module example.org/m\ngo 1.24.0\ntoolchain go1.24.2\nrequire (\n example.org/lib v1.2.3 // indirect\n)\nrequire example.org/other/v2 v2.4.0\nreplace example.org/lib => ../local\n".into()),
        ("requirements.txt".into(),"# spec fixture\nrequests[security]==2.31.0 ; python_version >= '3.8'\nDjango>=4.2,<5\nsix==1.16.0 --hash=sha256:abc\n-r extra.txt\n-e ../local\n".into()),
    ]);
    let deps = extract(&files);
    assert_eq!(deps.len(), 7, "{deps:?}");
    assert!(
        deps.iter()
            .any(|d| d.kind == DependencyKind::GoDirective && d.current == "1.24.0")
    );
    assert!(
        deps.iter()
            .any(|d| d.kind == DependencyKind::Toolchain && d.current == "1.24.2")
    );
    assert!(
        deps.iter()
            .any(|d| d.package == "django" && d.current == ">=4.2,<5")
    );
}
#[test]
fn terraform_providers_modules_and_lockfile() {
    let files=Files::from([
        ("main.tf".into(),r#"terraform {
  required_providers {
    aws = {
      source = "hashicorp/aws"
      version = "~> 5.0"
    }
    random = ">= 3.0"
  }
}
module "vpc" {
  source = "terraform-aws-modules/vpc/aws"
  version = "5.4.0"
}
module "local" {
  source = "./local"
}
"#.into()),
        (".terraform.lock.hcl".into(),"provider \"registry.terraform.io/hashicorp/aws\" {\n version = \"5.70.0\"\n hashes = [\"h1:abc\"]\n}\n".into()),
    ]);
    let deps = extract(&files);
    assert_eq!(deps.len(), 3, "{deps:?}");
    assert_eq!(
        deps.iter()
            .find(|d| d.name == "aws")
            .unwrap()
            .resolved
            .as_deref(),
        Some("5.70.0")
    );
    assert!(
        deps.iter()
            .any(|d| d.package == "terraform-aws-modules/vpc/aws"
                && d.kind == DependencyKind::Module)
    );
    assert!(
        deps.iter()
            .all(|d| files[&d.file][d.location.start..d.location.end].contains(&d.current))
    );
}
#[test]
fn kubernetes_kustomize_flux_and_values_locations() {
    let files=Files::from([
        ("apps/demo/deploy.yaml".into(),"apiVersion: apps/v1\nkind: Deployment\nspec:\n  template:\n    spec:\n      initContainers:\n        - name: init\n          image: busybox:1.36\n      containers:\n        - name: app\n          image: ghcr.io/example/app:1.2.3\n---\napiVersion: v1\nkind: ConfigMap\ndata:\n  image: fake:1\n".into()),
        ("kustomization.yaml".into(),"apiVersion: kustomize.config.k8s.io/v1beta1\nkind: Kustomization\nimages:\n  - name: nginx\n    newName: library/nginx\n    newTag: 1.26-alpine\nhelmCharts:\n  - name: podinfo\n    repo: https://stefanprodan.github.io/podinfo\n    version: 6.7.1\n".into()),
        ("apps/demo/source.yaml".into(),"apiVersion: source.toolkit.fluxcd.io/v1\nkind: HelmRepository\nmetadata:\n  name: podinfo\n  namespace: demo\nspec:\n  url: https://stefanprodan.github.io/podinfo\n".into()),
        ("apps/demo/release.yaml".into(),"apiVersion: helm.toolkit.fluxcd.io/v2\nkind: HelmRelease\nmetadata:\n  name: app\n  namespace: demo\nspec:\n  chart:\n    spec:\n      chart: podinfo\n      version: 6.7.1\n      sourceRef:\n        kind: HelmRepository\n        name: podinfo\n  values:\n    image:\n      repository: ghcr.io/example/custom\n      tag: 1.2.0\n".into()),
        ("apps/demo/oci.yaml".into(),"apiVersion: source.toolkit.fluxcd.io/v1\nkind: OCIRepository\nmetadata:\n  name: chart\nspec:\n  url: oci://ghcr.io/stefanprodan/charts/podinfo\n  ref:\n    tag: 6.7.1\n".into()),
        ("values.yaml".into(),"image:\n  registry: ghcr.io\n  repository: example/app\n  tag: 1.2.3\nsidecar:\n  image: alpine:3.20\n".into()),
    ]);
    let deps = extract(&files);
    assert_eq!(deps.len(), 9, "{deps:?}");
    assert!(deps.iter().any(|d| d.source == Source::Helm
        && d.package == "https://stefanprodan.github.io/podinfo|podinfo"));
    assert!(
        deps.iter()
            .any(|d| d.source == Source::Oci && d.package == "ghcr.io/stefanprodan/charts/podinfo")
    );
    assert!(deps.iter().all(|d| d.package != "fake"));
}

#[test]
fn yaml_locations_follow_paths_with_identical_values_flow_maps_and_unicode() {
    let text = "# café 🦀\napiVersion: apps/v1\nkind: Deployment\nmetadata: {annotations: {image: 'nginx:1.26'}}\nspec: {template: {spec: {containers: [{name: app, image: 'nginx:1.26'}]}}}\n";
    let files = Files::from([("deploy.yaml".into(), text.into())]);
    let deps = extract(&files);
    // Kubernetes images only belong to workload specs.
    let spec = deps
        .iter()
        .find(|d| d.location.pointer.contains("/spec/"))
        .unwrap();
    assert_eq!(
        &text[spec.location.start..spec.location.end],
        "'nginx:1.26'"
    );
    assert!(spec.location.start > text.find("spec:").unwrap());
}

#[test]
fn flux_chart_refs_still_extract_embedded_image_pins() {
    let files=Files::from([("apps/demo/release.yaml".into(),"apiVersion: helm.toolkit.fluxcd.io/v2\nkind: HelmRelease\nspec:\n  chartRef: {kind: OCIRepository, name: app}\n  values:\n    image: {repository: ghcr.io/example/app, tag: 1.2.3}\n".into())]);
    let deps = extract(&files);
    assert_eq!(deps.len(), 1);
    assert_eq!(deps[0].manager, Manager::Flux);
    assert_eq!(deps[0].package, "ghcr.io/example/app");
}

#[test]
fn exact_requirements_and_go_pseudo_versions_retain_observed_identity() {
    let deps = extract(&Files::from([
        ("requirements.txt".into(), "widget==2.3.4\nrange>=2,<3\nwild==2.*\n".into()),
        ("go.mod".into(), "module example.org/demo\nrequire example.org/lib v0.0.0-20240102030405-abcdef123456\nrequire example.org/plain v1.2.3\n".into()),
    ]));
    assert_eq!(
        deps.iter()
            .find(|d| d.name == "widget")
            .unwrap()
            .resolved
            .as_deref(),
        Some("2.3.4")
    );
    assert!(
        deps.iter()
            .filter(|d| matches!(d.name.as_str(), "range" | "wild"))
            .all(|d| d.resolved.is_none())
    );
    assert_eq!(
        deps.iter()
            .find(|d| d.name == "example.org/lib")
            .unwrap()
            .digest
            .as_deref(),
        Some("abcdef123456")
    );
    assert!(
        deps.iter()
            .find(|d| d.name == "example.org/plain")
            .unwrap()
            .digest
            .is_none()
    );
}

#[test]
fn terraform_opentofu_locks_and_runtime_constraints_use_the_same_identity() {
    let files = Files::from([
        ("infra/versions.tf".into(), "terraform {\n required_version = \">= 1.8.0\"\n required_providers {\n widget = { source = \"example/widget\", version = \"~> 2.0\" }\n }\n}\n".into()),
        ("infra/.terraform.lock.hcl".into(), "provider \"registry.opentofu.org/example/widget\" {\n version = \"2.7.1\"\n}\n".into()),
    ]);
    let deps = extract(&files);
    let provider = deps.iter().find(|d| d.name == "widget").unwrap();
    assert_eq!(provider.resolved.as_deref(), Some("2.7.1"));
    let runtime = deps
        .iter()
        .find(|d| d.name == "hashicorp/terraform")
        .unwrap();
    assert_eq!(runtime.current, ">= 1.8.0");
    assert_eq!(runtime.source, Source::GithubReleases);
    assert_eq!(
        &files[&runtime.file][runtime.location.start..runtime.location.end],
        "\">= 1.8.0\""
    );
}

#[test]
fn mise_git_tag_pins_are_read_without_the_selector_prefix() {
    let deps = extract(&Files::from([(
        "mise.toml".into(),
        "[tools]\n\"cargo:https://github.com/example/tool\" = \"tag:v0.3.1\"\n".into(),
    )]));
    assert_eq!(deps.len(), 1);
    // mise lists only `HEAD` for a Git URL, so the lookup reports that rather
    // than reading the repository's tags itself.
    assert_eq!(deps[0].source, Source::Mise);
    assert_eq!(deps[0].package, "cargo:https://github.com/example/tool");
    assert_eq!(deps[0].current, "v0.3.1");
    assert_eq!(deps[0].resolved.as_deref(), Some("v0.3.1"));
}

#[test]
fn setup_inputs_have_their_own_editable_locations() {
    let text = "jobs:\n  check:\n    steps:\n      - uses: actions/setup-node@v4\n        with: {node-version: '20'}\n      - uses: golangci/golangci-lint-action@v6\n        with:\n          version: v2.3\n      - uses: actions/setup-go@v5\n        with:\n          go-version-file: go.mod\n      - uses: actions/setup-node@v4\n        with:\n          node-version: ${{ matrix.node }}\n";
    let deps = extract(&Files::from([(
        ".github/workflows/ci.yml".into(),
        text.into(),
    )]));
    let tools: Vec<_> = deps
        .iter()
        .filter(|d| d.kind == DependencyKind::Tool)
        .collect();
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0].package, "actions/node-versions");
    assert_eq!(
        &text[tools[0].location.start..tools[0].location.end],
        "'20'"
    );
    assert_eq!(tools[1].name, "golangci/golangci-lint");
    assert_eq!(
        &text[tools[1].location.start..tools[1].location.end],
        "v2.3"
    );
}

#[test]
fn setup_bun_inputs_are_npm_tools_with_concrete_locations() {
    let text = "jobs:\n  check:\n    steps:\n      - uses: oven-sh/setup-bun@v2\n        with:\n          bun-version: latest\n      - uses: oven-sh/setup-bun@v2\n        with:\n          bun-version: '1.2.3'\n      - uses: oven-sh/setup-bun@v2\n        with:\n          bun-version: ${{ matrix.bun }}\n";
    let deps = extract(&Files::from([(
        ".github/workflows/ci.yml".into(),
        text.into(),
    )]));
    let tools: Vec<_> = deps
        .iter()
        .filter(|d| d.kind == DependencyKind::Tool)
        .collect();
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0].name, "bun");
    assert_eq!(tools[0].source, Source::Npm);
    assert!(tools[0].resolved.is_none());
    assert_eq!(tools[1].resolved.as_deref(), Some("1.2.3"));
    assert_eq!(
        &text[tools[1].location.start..tools[1].location.end],
        "'1.2.3'"
    );
}

#[test]
fn generated_flux_bundle_and_configmap_image_variables_are_concrete_dependencies() {
    let text = "# This manifest was generated by flux. DO NOT EDIT.\n# Flux Version: v2.3.4\napiVersion: v1\nkind: ConfigMap\ndata:\n  CONTROLLER_IMAGE: example/controller:1.2.3\n  NOT_AN_IMAGE: ''\n  image: arbitrary:1\n";
    let deps = extract(&Files::from([(
        "clusters/demo/bundle.yaml".into(),
        text.into(),
    )]));
    assert_eq!(deps.len(), 2);
    assert!(deps.iter().any(|d| d.manager == Manager::Flux
        && d.package == "fluxcd/flux2"
        && d.current == "v2.3.4"));
    assert!(
        deps.iter()
            .any(|d| d.manager == Manager::HelmValues && d.package == "example/controller")
    );
    assert!(deps.iter().all(|d| d.package != "arbitrary"));
}

/// The same crate can be locked twice: for the manifest, and for a dependent
/// that needs another version a wide requirement also matches. Only the
/// manifest's own dependency edge says which one is its locked version.
#[test]
fn a_cargo_dependency_resolves_through_its_own_lock_edge_not_a_transitive_instance() {
    let lock = |edge: &str| {
        format!(
            r#"version = 4
[[package]]
name = "app"
version = "0.1.0"
dependencies = ["{edge}", "dependent"]
[[package]]
name = "dependent"
version = "2.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
dependencies = ["shared 0.22.1"]
[[package]]
name = "shared"
version = "0.21.7"
source = "registry+https://github.com/rust-lang/crates.io-index"
[[package]]
name = "shared"
version = "0.22.1"
source = "registry+https://github.com/rust-lang/crates.io-index"
"#
        )
    };
    let manifest = "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\nshared = \">=0.21\"\ndependent = \"2\"\n";
    for (edge, expected) in [("shared 0.21.7", "0.21.7"), ("shared 0.22.1", "0.22.1")] {
        let files = Files::from([
            ("Cargo.toml".into(), manifest.into()),
            ("Cargo.lock".into(), lock(edge)),
        ]);
        let deps = extract(&files);
        let shared = deps.iter().find(|d| d.name == "shared").unwrap();
        assert_eq!(shared.resolved.as_deref(), Some(expected), "{edge}");
        assert_eq!(shared.locked(), Some(expected));
    }
    // A manifest the lock does not know has no locked version at all.
    let files = Files::from([
        (
            "Cargo.toml".into(),
            manifest.replace("name = \"app\"", "name = \"other\""),
        ),
        ("Cargo.lock".into(), lock("shared 0.21.7")),
    ]);
    let deps = extract(&files);
    assert_eq!(
        deps.iter().find(|d| d.name == "shared").unwrap().resolved,
        None
    );
}

const REGISTRY: &str = "registry+https://github.com/rust-lang/crates.io-index";

/// A virtual workspace declares `shared` once for its members. A path
/// dependency outside the members is in the lock without a source too, and its
/// edge to another `shared` is not the workspace's.
#[test]
fn a_virtual_workspace_reads_the_edges_of_its_members_only() {
    let files = Files::from([
        (
            "Cargo.toml".into(),
            "[workspace]\nresolver = \"3\"\nmembers = [\"crates/*\"]\nexclude = [\"crates/old\"]\n\n[workspace.dependencies]\nshared = \">=0.21\"\n".into(),
        ),
        (
            "crates/app/Cargo.toml".into(),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nshared.workspace = true\n".into(),
        ),
        (
            "crates/old/Cargo.toml".into(),
            "[package]\nname = \"old\"\nversion = \"0.1.0\"\n\n[dependencies]\nshared = \"0.22\"\n".into(),
        ),
        (
            "third_party/vendored/Cargo.toml".into(),
            "[package]\nname = \"vendored\"\nversion = \"0.1.0\"\n\n[dependencies]\nshared = \"0.22\"\n".into(),
        ),
        (
            "Cargo.lock".into(),
            format!(
                r#"# This file is automatically @generated by Cargo.
# It is not intended for manual editing.
version = 4

[[package]]
name = "app"
version = "0.1.0"
dependencies = [
 "shared 0.21.7",
]

[[package]]
name = "old"
version = "0.1.0"
dependencies = [
 "shared 0.22.1",
]

[[package]]
name = "shared"
version = "0.21.7"
source = "{REGISTRY}"
checksum = "1111111111111111111111111111111111111111111111111111111111111111"

[[package]]
name = "shared"
version = "0.22.1"
source = "{REGISTRY}"
checksum = "2222222222222222222222222222222222222222222222222222222222222222"

[[package]]
name = "vendored"
version = "0.1.0"
dependencies = [
 "shared 0.22.1",
]
"#
            ),
        ),
    ]);
    let deps = extract(&files);
    let resolved = |file: &str| {
        deps.iter()
            .find(|d| d.name == "shared" && d.file == file)
            .unwrap_or_else(|| panic!("{file}: {deps:?}"))
            .resolved
            .clone()
    };
    // The root declares it for the members, `crates/app` among them. The
    // excluded `old` and the path dependency `vendored` are not members.
    assert_eq!(resolved("Cargo.toml").as_deref(), Some("0.21.7"));
    assert_eq!(resolved("crates/app/Cargo.toml").as_deref(), Some("0.21.7"));
    // A manifest that is not a member reads its own package's edge.
    assert_eq!(
        resolved("third_party/vendored/Cargo.toml").as_deref(),
        Some("0.22.1")
    );
}

/// Cargo's `**` matches the directory it follows as well, so `crates/**`
/// makes a package at `crates/Cargo.toml` a member.
#[test]
fn a_trailing_double_star_member_glob_includes_its_own_directory() {
    let files = Files::from([
        (
            "Cargo.toml".into(),
            "[workspace]\nmembers = [\"crates/**\"]\n\n[workspace.dependencies]\nshared = \">=0.21\"\n".into(),
        ),
        (
            "crates/Cargo.toml".into(),
            "[package]\nname = \"core\"\nversion = \"0.1.0\"\n\n[dependencies]\nshared.workspace = true\n".into(),
        ),
        (
            "Cargo.lock".into(),
            format!(
                r#"version = 4

[[package]]
name = "core"
version = "0.1.0"
dependencies = [
 "shared",
]

[[package]]
name = "shared"
version = "0.21.7"
source = "{REGISTRY}"
checksum = "1111111111111111111111111111111111111111111111111111111111111111"
"#
            ),
        ),
    ]);
    let deps = extract(&files);
    let root = deps
        .iter()
        .find(|d| d.name == "shared" && d.file == "Cargo.toml")
        .unwrap_or_else(|| panic!("{deps:?}"));
    assert_eq!(root.resolved.as_deref(), Some("0.21.7"));
}

/// The same crate at the same version can be locked from crates.io and from
/// git, and the edge names its source then. A git dependency under another
/// name is not the crates.io dependency's locked version.
#[test]
fn a_git_dependency_on_the_same_crate_is_not_the_registry_dependency() {
    let git =
        "git+https://github.com/acme/shared?rev=abc1234#abc1234abc1234abc1234abc1234abc1234abc1";
    let manifest = "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nshared = \">=0.21\"\nshared-git = { git = \"https://github.com/acme/shared\", rev = \"abc1234\", package = \"shared\" }\n";
    let lock = |edges: &str| {
        format!(
            r#"version = 4

[[package]]
name = "app"
version = "0.1.0"
dependencies = [
{edges}
 "dependent",
]

[[package]]
name = "dependent"
version = "2.0.0"
source = "{REGISTRY}"
checksum = "3333333333333333333333333333333333333333333333333333333333333333"
dependencies = [
 "shared 0.22.1 ({REGISTRY})",
]

[[package]]
name = "shared"
version = "0.21.7"
source = "{REGISTRY}"
checksum = "1111111111111111111111111111111111111111111111111111111111111111"

[[package]]
name = "shared"
version = "0.22.1"
source = "{REGISTRY}"
checksum = "2222222222222222222222222222222222222222222222222222222222222222"

[[package]]
name = "shared"
version = "0.22.1"
source = "{git}"
"#
        )
    };
    let resolved = |edges: &str, manifest: &str| {
        let files = Files::from([
            ("Cargo.toml".into(), manifest.into()),
            ("Cargo.lock".into(), lock(edges)),
        ]);
        extract(&files)
            .into_iter()
            .find(|d| d.name == "shared")
            .unwrap()
            .resolved
    };
    // The git edge carries its source, so the 0.22.1 in crates.io (locked for
    // `dependent`) does not make it the registry dependency's version.
    assert_eq!(
        resolved(
            &format!(" \"shared 0.21.7\",\n \"shared 0.22.1 ({git})\","),
            manifest
        )
        .as_deref(),
        Some("0.21.7")
    );
    // An edge that names the registry as its source is the registry's.
    assert_eq!(
        resolved(
            &format!(" \"shared 0.22.1 ({REGISTRY})\","),
            &manifest.replace("\">=0.21\"", "\">=0.22\"")
        )
        .as_deref(),
        Some("0.22.1")
    );
    // Only the git edge: nothing in crates.io is this manifest's.
    assert_eq!(
        resolved(&format!(" \"shared 0.22.1 ({git})\","), manifest),
        None
    );
}

/// `members` comes from the repository, so a pattern built to make a matcher
/// backtrack must not stall the scan. It runs under a timeout.
#[test]
fn a_hostile_members_glob_does_not_stall_extraction() {
    let hostile = "*a".repeat(60) + "*b";
    let directory = "a".repeat(200);
    let files = Files::from([
        (
            "Cargo.toml".into(),
            format!(
                "[workspace]\nmembers = [\"{hostile}\", \"crates/*\"]\n\n[workspace.dependencies]\nshared = \"0.21\"\n"
            ),
        ),
        (
            format!("{directory}/Cargo.toml"),
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\n\n[dependencies]\nshared = \"0.21\"\n".into(),
        ),
        (
            "crates/app/Cargo.toml".into(),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nshared.workspace = true\n".into(),
        ),
        (
            "Cargo.lock".into(),
            format!(
                "version = 4\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\"shared\"]\n\n[[package]]\nname = \"shared\"\nversion = \"0.21.7\"\nsource = \"{REGISTRY}\"\n"
            ),
        ),
    ]);
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(extract(&files));
    });
    let deps = receiver
        .recv_timeout(std::time::Duration::from_secs(20))
        .expect("extraction stalled on a hostile members glob");
    // The ordinary pattern still finds its member.
    let root = deps
        .iter()
        .find(|d| d.name == "shared" && d.file == "Cargo.toml")
        .unwrap();
    assert_eq!(root.resolved.as_deref(), Some("0.21.7"));
}

/// A workspace root with its own `[package]` is a declaring package even when no
/// member manifest is scanned; a virtual root with none has no locked version.
#[test]
fn a_workspace_root_with_a_package_counts_it_without_any_member_manifest() {
    let lock = format!(
        "version = 4\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\"shared\"]\n\n[[package]]\nname = \"shared\"\nversion = \"0.21.7\"\nsource = \"{REGISTRY}\"\n"
    );
    let resolved = |manifest: &str| {
        let files = Files::from([
            ("Cargo.toml".into(), manifest.into()),
            ("Cargo.lock".into(), lock.clone()),
        ]);
        extract(&files)
            .into_iter()
            .find(|d| d.name == "shared")
            .unwrap()
            .resolved
    };
    assert_eq!(
        resolved("[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[workspace]\nmembers = [\"crates/*\"]\n\n[dependencies]\nshared = \"0.21\"\n").as_deref(),
        Some("0.21.7")
    );
    assert_eq!(
        resolved(
            "[workspace]\nmembers = [\"crates/*\"]\n\n[workspace.dependencies]\nshared = \"0.21\"\n"
        ),
        None
    );
}
